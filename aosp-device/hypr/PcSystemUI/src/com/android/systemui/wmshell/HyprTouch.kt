package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.app.KeyguardManager
import android.content.Context
import android.graphics.Region
import android.hardware.input.InputManager
import android.os.Looper
import android.os.RemoteException
import android.os.SystemProperties
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.ISystemGestureExclusionListener
import android.view.InputDevice
import android.view.InputEvent
import android.view.InputEventReceiver
import android.view.InputMonitor
import android.view.MotionEvent
import android.view.WindowManager
import android.view.WindowManagerGlobal
import com.android.hyprtile.Bind
import com.android.hyprtile.TouchConfig
import com.android.hyprtile.TouchGesture
import com.android.hyprtile.TouchRecognizer
import com.android.hyprtile.defaultTouchBinds
import com.android.hyprtile.parseBind
import com.android.wm.shell.common.ShellExecutor
import kotlin.math.hypot

/**
 * Touch gestures in the style of HarmonyOS's PC mode (Phase D6, docs/hypr-desktop.md):
 *
 * - edge swipes (bottom: app launcher, left/right: workspace, top: notifications, its right
 *   third: quick settings), skipped where apps asked for the edge (system gesture exclusion);
 * - three-finger swipes and, on the wallpaper or the bar, pinches (overview);
 * - a long-press on a window's frame (its border, the gap next to it, a floating window's
 *   caption) picks the window up: dragging swaps it with the tile under the finger (a floating
 *   one lands there, or joins the layout at the left or right screen edge), releasing without
 *   moving opens [HyprWindowMenu].
 *
 * A gesture monitor sees every touch of the display next to the apps; only a recognized
 * gesture pilfers the pointers (the app gets a cancel), so taps, scrolls and long-presses
 * inside apps keep working. Edge swipes and long-presses are the one-finger gestures, so they
 * never fire in the middle of a window. Gestures are binds (`GESTURE:<NAME>`, Lua
 * `hl.bind("gesture:edge_bottom", ...)`); without a Lua configuration [defaultTouchBinds].
 * Created and fed on Shell's main thread.
 */
class HyprTouch(
    private val context: Context,
    private val inputManager: InputManager,
    private val controller: HyprTileController,
    private val mainExecutor: ShellExecutor,
) {
    private var monitor: InputMonitor? = null
    private var receiver: InputEventReceiver? = null
    private var displayId = DEFAULT_DISPLAY
    private val recognizer = TouchRecognizer(TouchConfig(1, 1))
    /** Where apps asked for priority over system gestures, as WindowManager restricts it. */
    private val excluded = Region()
    /** The same without WindowManager's limit on the side edges. */
    private val excludedAll = Region()
    private val menu = HyprWindowMenu(context, controller)

    /** The window the first finger may pick up (on its frame), until it moves or lifts. */
    private var holdCandidate: RunningTaskInfo? = null
    /** The window picked up by a long-press, following the finger. */
    private var holding: RunningTaskInfo? = null
    private var holdMoved = false
    private var downX = 0f
    private var downY = 0f
    /** The gesture was taken (pilfered); the rest of it is ignored. */
    private var taken = false
    private val holdRunnable = Runnable { onHold() }

    private val exclusionListener = object : ISystemGestureExclusionListener.Stub() {
        override fun onSystemGestureExclusionChanged(
            displayId: Int,
            systemGestureExclusion: Region?,
            unrestricted: Region?,
        ) {
            val r = Region(systemGestureExclusion ?: Region())
            val all = Region(unrestricted ?: r)
            mainExecutor.execute {
                excluded.set(r)
                excludedAll.set(all)
            }
        }
    }

    fun start(displayId: Int = DEFAULT_DISPLAY) {
        if (monitor != null) return
        this.displayId = displayId
        val m = inputManager.monitorGestureInput("hypr-touch", displayId)
        monitor = m
        receiver = object : InputEventReceiver(m.inputChannel, Looper.myLooper()) {
            override fun onInputEvent(event: InputEvent) {
                try {
                    if (event is MotionEvent && event.isFromSource(InputDevice.SOURCE_TOUCHSCREEN)) {
                        onTouch(event)
                    }
                } catch (e: RuntimeException) {
                    Log.e(TAG, "touch gesture failed", e)
                } finally {
                    finishInputEvent(event, false)
                }
            }
        }
        try {
            WindowManagerGlobal.getWindowManagerService()
                ?.registerSystemGestureExclusionListener(exclusionListener, displayId)
        } catch (e: RemoteException) {
            Log.w(TAG, "no gesture exclusion listener: $e")
        }
        Log.i(TAG, "touch gestures on display $displayId")
    }

    /** Touch gestures are on (Lua `shell.touch`, `persist.hypr.touch`). */
    private val enabled get() = controller.enabled &&
        (controller.configOverrides.getString("touch")?.toBooleanStrictOrNull()
            ?: SystemProperties.getBoolean(PROP_ENABLED, true))

    private fun onTouch(e: MotionEvent) {
        val now = e.eventTime
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                reset()
                // The menu closes itself on a touch outside it (a touch inside is its own).
                taken = !enabled
                if (taken) return
                val x = e.rawX
                val y = e.rawY
                // On the menu, or where a window asked for priority over system gestures (an
                // app's own swipe area, HyprLauncher's overview and launcher panels): only
                // edge swipes outside the app's allowance still count.
                if (menu.contains(x, y)) {
                    taken = true
                    return
                }
                val appArea = excludedAll.contains(x.toInt(), y.toInt())
                downX = x
                downY = y
                recognizer.config = touchConfig()
                val onWindow = controller.windowAt(displayId, x, y) != null
                recognizer.down(e.getPointerId(0), x, y, now,
                    excluded = excluded.contains(x.toInt(), y.toInt()), onWindow = onWindow)
                if (appArea) return
                holdCandidate = controller.frameAt(displayId, x, y, dp(FRAME_DP), dp(CAPTION_DP))
                if (holdCandidate != null) mainExecutor.executeDelayed(holdRunnable, holdTime())
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                cancelHold()
                if (taken || holding != null) return
                val i = e.actionIndex
                val (x, y) = raw(e, i)
                recognizer.pointerDown(e.getPointerId(i), x, y, now,
                    onWindow = controller.windowAt(displayId, x, y) != null)
            }
            MotionEvent.ACTION_POINTER_UP -> recognizer.pointerUp(e.getPointerId(e.actionIndex))
            MotionEvent.ACTION_MOVE -> {
                if (holding != null) {
                    if (hypot(e.rawX - downX, e.rawY - downY) > dp(SLOP_DP)) holdMoved = true
                    controller.mouseMove(e.rawX, e.rawY)
                    return
                }
                if (taken) return
                if (holdCandidate != null && hypot(e.rawX - downX, e.rawY - downY) > dp(SLOP_DP)) {
                    cancelHold()
                }
                val points = HashMap<Int, Pair<Float, Float>>()
                for (i in 0 until e.pointerCount) points[e.getPointerId(i)] = raw(e, i)
                val names = recognizer.move(points, now) ?: return
                fire(names)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                val task = holding
                if (task != null) {
                    holding = null
                    val cancelled = e.actionMasked == MotionEvent.ACTION_CANCEL
                    if (holdMoved && !cancelled) {
                        controller.touchDrop(e.rawX, e.rawY, dp(EDGE_DP))
                    } else {
                        controller.mouseUp(e.rawX, e.rawY, cancelled = true)
                        if (!cancelled) menu.show(task, e.rawX, e.rawY)
                    }
                }
                reset()
            }
        }
    }

    /** Runs the first of [names] that has a bind. */
    private fun fire(names: List<String>) {
        val binds = gestureBinds()
        val (name, bind) = names.firstNotNullOfOrNull { n -> binds[n]?.let { n to it } } ?: run {
            Log.d(TAG, "gesture ${names.first()}: not bound")
            return
        }
        taken = true
        cancelHold()
        // The lock screen keeps its own touch handling; only the shade opens over it.
        if (name != "EDGE_TOP" && name != "EDGE_TOP_RIGHT" &&
            context.getSystemService(KeyguardManager::class.java)?.isKeyguardLocked == true) {
            Log.d(TAG, "gesture $name: keyguard showing")
            return
        }
        monitor?.pilferPointers()
        Log.i(TAG, "gesture $name -> ${bind.dispatcher} ${bind.arg}")
        if (!controller.dispatch(bind.dispatcher, bind.arg)) {
            Log.w(TAG, "gesture $name: dispatch ${bind.dispatcher} ${bind.arg} refused")
        }
    }

    private fun onHold() {
        val task = holdCandidate ?: return
        holdCandidate = null
        if (taken) return
        taken = true
        monitor?.pilferPointers()
        holding = task
        holdMoved = false
        Log.i(TAG, "long-press on window ${task.taskId}")
        controller.touchHold(task, downX, downY)
    }

    private fun cancelHold() {
        if (holdCandidate != null) {
            holdCandidate = null
            mainExecutor.removeCallbacks(holdRunnable)
        }
    }

    private fun reset() {
        cancelHold()
        holding = null
        holdMoved = false
    }

    /** Gesture name -> bind: Lua's list (`binds`, GESTURE keys), else the defaults. */
    private fun gestureBinds(): Map<String, Bind> {
        val lines = controller.configOverrides.getStringArray("binds")
        val binds = if (lines != null) {
            lines.mapNotNull { line -> runCatching { parseBind(line) }.getOrNull() }
        } else {
            defaultTouchBinds()
        }
        return binds.filter { TouchGesture.isGestureKey(it.key) }
            .mapNotNull { b -> runCatching { TouchGesture.nameOf(b.key) to b }.getOrNull() }
            .toMap()
    }

    private fun touchConfig(): TouchConfig {
        val bounds = context.getSystemService(WindowManager::class.java).maximumWindowMetrics.bounds
        return TouchConfig(
            width = bounds.width(),
            height = bounds.height(),
            edge = dp(EDGE_DP),
            distance = dp(DISTANCE_DP),
            slop = dp(SLOP_DP),
        )
    }

    private fun holdTime(): Long =
        SystemProperties.getLong(PROP_HOLD, HOLD_MS).coerceIn(150, 2000)

    private fun dp(v: Int) = (v * context.resources.displayMetrics.density + 0.5f).toInt()

    private fun raw(e: MotionEvent, i: Int): Pair<Float, Float> = e.getRawX(i) to e.getRawY(i)

    private companion object {
        const val TAG = "HyprTouch"
        const val PROP_ENABLED = "persist.hypr.touch"
        /** persist.hypr.touch_hold: ms before a still finger on a frame picks the window up. */
        const val PROP_HOLD = "persist.hypr.touch_hold"
        /** Under the 400 ms of apps' long-press, so the app gets a cancel before it acts. */
        const val HOLD_MS = 350L
        const val EDGE_DP = 20
        const val DISTANCE_DP = 72
        const val SLOP_DP = 12
        /** How far inside a window's edge its frame reaches for a finger. */
        const val FRAME_DP = 14
        /** Shell's caption on floating windows. */
        const val CAPTION_DP = 40
    }
}
