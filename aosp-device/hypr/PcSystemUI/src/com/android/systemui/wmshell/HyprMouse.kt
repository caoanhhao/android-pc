package com.android.systemui.wmshell

import android.hardware.input.InputManager
import android.os.Looper
import android.os.SystemClock
import android.os.SystemProperties
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.InputDevice
import android.view.InputEvent
import android.view.InputEventReceiver
import android.view.InputMonitor
import android.view.KeyEvent
import android.view.MotionEvent
import com.android.hyprtile.Mod
import com.android.hyprtile.MouseBind
import com.android.hyprtile.MouseBinds
import com.android.hyprtile.MouseInput
import com.android.hyprtile.parseBind
import com.android.hyprtile.parseMods

/**
 * Hyprland's mouse binds (D3, E6; docs/hypr-desktop.md, "E6"): the buttons `mouse:272`..`274`
 * and the wheel (`mouse_up`, `mouse_down`, `mouse_left`, `mouse_right`) with any modifiers and
 * any dispatcher or Lua function, and `bindm` drags: with its modifiers held, the left button
 * drags a window (a tiled one swaps with the tiled window it is dropped on, a floating one
 * moves) and the right button resizes it from the nearest corner (a tiled one moves its split
 * lines). The side buttons (`mouse:275`/`276`) reach Android as BACK/FORWARD keys and are key
 * binds ([HyprKeybinds]).
 *
 * A gesture monitor (a spy window) sees every pointer event of the display. A bound button
 * press on the monitor pilfers the pointer, so the app gets a cancel and the rest of the
 * gesture comes here. A spy cannot take a scroll from the window under the pointer (it is one
 * event, dispatched to both at once), so the input dispatcher drops bound scrolls for every
 * window but spies: this class publishes the bound wheel directions in [PROP_WHEEL_BINDS]
 * (tools/patches/aosp17-input-hypr-wheel-binds.patch). Unbound events, and bound ones with
 * `non_consuming` or `binds.pass_mouse_when_bound`, go to the app as before.
 *
 * Touch (no button) counts as the left button, for `bindm` drags only. Created and fed on
 * Shell's main thread.
 */
class HyprMouse(
    private val inputManager: InputManager,
    private val controller: HyprTileController,
) {
    private var monitor: InputMonitor? = null
    private var receiver: InputEventReceiver? = null
    /** A `bindm` drag or resize is in progress. */
    private var active = false
    /** The rest of a pilfered gesture is swallowed (a click bind fired on its press). */
    private var swallowing = false
    private var binds = MouseBinds(emptyList())

    fun start(displayId: Int = DEFAULT_DISPLAY) {
        if (monitor != null) return
        onConfigChanged()
        val m = inputManager.monitorGestureInput("hypr-bindm", displayId)
        monitor = m
        receiver = object : InputEventReceiver(m.inputChannel, Looper.myLooper()) {
            override fun onInputEvent(event: InputEvent) {
                var handled = false
                try {
                    if (event is MotionEvent) handled = onMotion(event, displayId)
                } catch (e: RuntimeException) {
                    Log.e(TAG, "mouse bind failed", e)
                } finally {
                    finishInputEvent(event, handled)
                }
            }
        }
        Log.i(TAG, "mouse binds on display $displayId")
    }

    /** Reads the binds again and tells the input dispatcher which scrolls are bound. */
    fun onConfigChanged() {
        val o = controller.configOverrides
        val list = o.getStringArray("binds")?.mapNotNull { runCatching { parseBind(it) }.getOrNull() }
        binds = MouseBinds.fromBindList(
            list,
            defaultMod(),
            pass = o.getString("pass_mouse_when_bound") == "true",
            delayMs = o.getString("scroll_event_delay")?.toLongOrNull() ?: 300,
        )
        val filter = binds.wheelFilter()
        if (SystemProperties.get(PROP_WHEEL_BINDS) != filter) {
            try {
                SystemProperties.set(PROP_WHEEL_BINDS, filter)
            } catch (e: RuntimeException) {
                Log.w(TAG, "cannot set $PROP_WHEEL_BINDS: bound scrolls reach the apps too", e)
            }
        }
        Log.i(TAG, "${binds.binds.size} mouse binds, bound wheel '$filter'")
    }

    private fun onMotion(e: MotionEvent, displayId: Int): Boolean {
        if ((active || swallowing) && Log.isLoggable(TAG, Log.DEBUG)) {
            Log.d(TAG, "${MotionEvent.actionToString(e.action)} buttons=${e.buttonState} " +
                "at ${e.rawX},${e.rawY} history=${e.historySize}")
        }
        when (e.actionMasked) {
            MotionEvent.ACTION_SCROLL -> return onScroll(e)
            MotionEvent.ACTION_DOWN -> {
                active = false
                swallowing = false
                return onPress(e, displayId)
            }
            MotionEvent.ACTION_MOVE -> if (active) controller.mouseMove(e.rawX, e.rawY)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (active) {
                    active = false
                    controller.mouseUp(e.rawX, e.rawY, cancelled = e.actionMasked == MotionEvent.ACTION_CANCEL)
                    return true
                }
                val was = swallowing
                swallowing = false
                return was
            }
            else -> return false
        }
        return active || swallowing
    }

    private fun onScroll(e: MotionEvent): Boolean {
        val input = MouseInput.wheel(e.getAxisValue(MotionEvent.AXIS_VSCROLL),
            e.getAxisValue(MotionEvent.AXIS_HSCROLL)) ?: return false
        val bind = binds.match(mods(e.metaState), input) ?: return false
        if (binds.fires(bind, SystemClock.uptimeMillis())) run(bind)
        return true
    }

    private fun onPress(e: MotionEvent, displayId: Int): Boolean {
        // Test hook: `input` cannot inject a press with a modifier held or with the right
        // button, so this property stands for both (move, resize, float).
        val test = SystemProperties.get(PROP_TEST)
        val input = when {
            test == "move" || test == "resize" || test == "float" -> null
            e.isButtonPressed(MotionEvent.BUTTON_SECONDARY) -> MouseInput.RIGHT
            e.isButtonPressed(MotionEvent.BUTTON_TERTIARY) -> MouseInput.MIDDLE
            e.isButtonPressed(MotionEvent.BUTTON_PRIMARY) -> MouseInput.LEFT
            e.buttonState == 0 && !e.isFromSource(InputDevice.SOURCE_MOUSE) -> MouseInput.LEFT
            else -> return false
        }
        val bind = if (input == null) {
            when (test) {
                "move" -> MouseBind(emptySet(), MouseInput.LEFT, MouseBind.DRAG)
                "resize" -> MouseBind(emptySet(), MouseInput.RIGHT, MouseBind.RESIZE)
                else -> MouseBind(emptySet(), MouseInput.MIDDLE, "togglefloating")
            }
        } else {
            binds.match(mods(e.metaState), input) ?: return false
        }
        // A touch with no button only drags.
        if (e.buttonState == 0 && input != null && !bind.isDragOrResize) return false
        val pass = input != null && binds.passes(bind) && !bind.isDragOrResize
        // Pilfer before acting: acting can take longer than a quick click, and once the button
        // is up the app has had the whole click and there is nothing left to pilfer.
        if (!pass) monitor?.pilferPointers()
        if (bind.isDragOrResize || (bind.dispatcher == "togglefloating" && bind.arg.isEmpty())) {
            // These act on the window under the pointer, not the focused one.
            val button = when (bind.dispatcher) {
                MouseBind.DRAG -> MotionEvent.BUTTON_PRIMARY
                MouseBind.RESIZE -> MotionEvent.BUTTON_SECONDARY
                else -> MotionEvent.BUTTON_TERTIARY
            }
            active = controller.mouseDown(displayId, e.rawX, e.rawY, button) && bind.isDragOrResize
        } else {
            run(bind)
        }
        swallowing = !pass && !active
        return !pass
    }

    private fun run(bind: MouseBind) {
        if (!controller.dispatch(bind.dispatcher, bind.arg)) {
            Log.w(TAG, "mouse bind ${bind.toBind()}: unknown dispatcher or argument")
        }
    }

    /** The modifiers held in [meta], as binds name them (lock keys do not count). */
    private fun mods(meta: Int): Set<Mod> {
        val m = HashSet<Mod>(4)
        if (meta and KeyEvent.META_SHIFT_ON != 0) m += Mod.SHIFT
        if (meta and KeyEvent.META_CTRL_ON != 0) m += Mod.CTRL
        if (meta and KeyEvent.META_ALT_ON != 0) m += Mod.ALT
        if (meta and KeyEvent.META_META_ON != 0) m += Mod.SUPER
        return m
    }

    /** The modifier of the default mouse binds without a Lua bind list. */
    private fun defaultMod(): Set<Mod> {
        val o = controller.configOverrides
        return runCatching { parseMods(o.getString("mouse_mod") ?: SystemProperties.get("persist.hypr.mod", "ALT")) }
            .getOrNull()?.takeIf { it.isNotEmpty() } ?: setOf(Mod.ALT)
    }

    private companion object {
        const val TAG = "HyprMouse"
        /** debug.hypr.bindm_test = move | resize | float: every press acts so, no modifier. */
        const val PROP_TEST = "debug.hypr.bindm_test"
        /** Read by the input dispatcher (MouseBinds.wheelFilter's format). */
        const val PROP_WHEEL_BINDS = "sys.hypr.wheel_binds"
    }
}
