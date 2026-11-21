package com.android.systemui.wmshell

import android.hardware.input.InputManager
import android.os.Looper
import android.os.SystemProperties
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.InputEvent
import android.view.InputEventReceiver
import android.view.InputMonitor
import android.view.KeyEvent
import android.view.MotionEvent
import com.android.hyprtile.Mod
import com.android.hyprtile.parseMods

/**
 * Hyprland's mouse binds (`bindm = $mod, mouse:272, movewindow` and `mouse:273, resizewindow`),
 * Phase D3: with the main modifier held (`persist.hypr.mod`, ALT by default), the left button
 * drags a window (a tiled one swaps with the tiled window it is dropped on, a floating one
 * moves), the right button resizes it from the nearest corner (a tiled one moves its split
 * lines), the middle button toggles floating. This keeps windows manageable with the mouse
 * when they have no caption.
 *
 * A gesture monitor sees every pointer event of the display; when a press with the modifier
 * hits a desktop window, the monitor pilfers the pointer so the app gets a cancel and the rest
 * of the gesture comes here. Touch (no button) counts as the left button. Created and fed on
 * Shell's main thread.
 */
class HyprMouse(
    private val inputManager: InputManager,
    private val controller: HyprTileController,
) {
    private var monitor: InputMonitor? = null
    private var receiver: InputEventReceiver? = null
    private var active = false

    fun start(displayId: Int = DEFAULT_DISPLAY) {
        if (monitor != null) return
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

    private fun onMotion(e: MotionEvent, displayId: Int): Boolean {
        if (active && Log.isLoggable(TAG, Log.DEBUG)) {
            Log.d(TAG, "${MotionEvent.actionToString(e.action)} buttons=${e.buttonState} " +
                "at ${e.rawX},${e.rawY} history=${e.historySize}")
        }
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                active = false
                // Test hook: `input` cannot inject a drag with a modifier held or with the right
                // button, so this property stands for both (move, resize, float).
                val test = SystemProperties.get(PROP_TEST)
                val mask = modMask()
                if (test.isEmpty() && e.metaState and mask != mask) return false
                val button = when {
                    test == "move" -> MotionEvent.BUTTON_PRIMARY
                    test == "resize" -> MotionEvent.BUTTON_SECONDARY
                    test == "float" -> MotionEvent.BUTTON_TERTIARY
                    e.isButtonPressed(MotionEvent.BUTTON_SECONDARY) -> MotionEvent.BUTTON_SECONDARY
                    e.isButtonPressed(MotionEvent.BUTTON_TERTIARY) -> MotionEvent.BUTTON_TERTIARY
                    else -> MotionEvent.BUTTON_PRIMARY
                }
                if (!controller.mouseDown(displayId, e.rawX, e.rawY, button)) return false
                monitor?.pilferPointers()
                active = button != MotionEvent.BUTTON_TERTIARY
                return true
            }
            MotionEvent.ACTION_MOVE -> if (active) controller.mouseMove(e.rawX, e.rawY)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> if (active) {
                active = false
                controller.mouseUp(e.rawX, e.rawY, cancelled = e.actionMasked == MotionEvent.ACTION_CANCEL)
            }
            else -> return false
        }
        return active
    }

    /** Meta state bits of the main modifier, read at each press (it can change at runtime). */
    private fun modMask(): Int {
        val mods = runCatching { parseMods(SystemProperties.get("persist.hypr.mod", "ALT")) }
            .getOrNull()?.takeIf { it.isNotEmpty() } ?: setOf(Mod.ALT)
        var meta = 0
        if (Mod.SHIFT in mods) meta = meta or KeyEvent.META_SHIFT_ON
        if (Mod.CTRL in mods) meta = meta or KeyEvent.META_CTRL_ON
        if (Mod.ALT in mods) meta = meta or KeyEvent.META_ALT_ON
        if (Mod.SUPER in mods) meta = meta or KeyEvent.META_META_ON
        return meta
    }

    private companion object {
        const val TAG = "HyprMouse"
        /** debug.hypr.bindm_test = move | resize | float: every press acts so, no modifier. */
        const val PROP_TEST = "debug.hypr.bindm_test"
    }
}
