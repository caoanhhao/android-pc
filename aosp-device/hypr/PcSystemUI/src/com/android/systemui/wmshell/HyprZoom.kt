package com.android.systemui.wmshell

import android.app.ActivityManager
import android.content.ComponentName
import android.content.Context
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.input.InputManager
import android.os.Looper
import android.provider.Settings
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.InputEvent
import android.view.InputEventReceiver
import android.view.InputMonitor
import android.view.MotionEvent
import com.android.hyprtile.ZoomOp
import com.android.hyprtile.clampZoom
import com.android.hyprtile.parseZoom
import com.android.hyprtile.zoomCentre
import com.android.wm.shell.common.ShellExecutor

/**
 * Hyprland's cursor zoom (Phase E10, docs/hypr-desktop.md, "Zoom (E10)"): the `zoom`
 * dispatcher (`hl.dsp.zoom({ delta = 0.5 })`, `{ factor = 1 }`) and Lua's
 * `cursor.zoom_factor` magnify the whole display around the pointer, as Hyprland's
 * `cursor:zoom_factor` does: the content under the pointer stays under it, and moving the
 * pointer moves the view with it ([zoomCentre]).
 *
 * The scale is Android's full-screen magnification, set through [HyprZoomService], an
 * accessibility service of this process that has only the magnification capability. It is
 * written into `enabled_accessibility_services` (keeping the user's own services) when the
 * zoom goes above 1 and taken out again a moment after it is back at 1, so no accessibility
 * settings screen or magnification shortcut is involved and apps see accessibility off while
 * nothing is zoomed. While zoomed a gesture monitor follows the pointer (hover and drag
 * events), and the view follows it without animation; factor changes animate.
 *
 * Created and called on Shell's main thread.
 */
class HyprZoom(
    private val context: Context,
    private val inputManager: InputManager,
    private val mainExecutor: ShellExecutor,
) {
    /** The zoom factor in force (1 is none). */
    var factor = 1f
        private set
    private var displayId = DEFAULT_DISPLAY
    private var pointerX = Float.NaN
    private var pointerY = Float.NaN
    private var monitor: InputMonitor? = null
    private var receiver: InputEventReceiver? = null
    /** The `cursor.zoom_factor` of the last configuration, so only a change applies. */
    private var configFactor: Float? = null
    private val disableRunnable = Runnable { if (factor <= 1f) setServiceEnabled(false) }
    private val component = ComponentName(context, HyprZoomService::class.java)

    fun start() {
        HyprZoomService.onConnected = { mainExecutor.execute { apply(animate = true) } }
        // A service entry left by an earlier process (a crash while zoomed) goes away.
        if (HyprZoomService.instance == null) setServiceEnabled(false)
    }

    /** The `zoom` dispatcher; false when [arg] does not parse. */
    fun dispatch(arg: String): Boolean {
        val op = parseZoom(arg) ?: return false
        zoomTo(op.apply(factor))
        return true
    }

    /** Lua's `cursor.zoom_factor` (the host's way to zoom): applied when it changes. */
    fun onConfigChanged(zoomFactor: String?) {
        val f = zoomFactor?.toFloatOrNull()?.let(::clampZoom) ?: 1f
        if (f == configFactor) return
        val first = configFactor == null
        configFactor = f
        // The first configuration after a start only zooms when it asks for a zoom.
        if (!first || f > 1f) zoomTo(ZoomOp.To(f).apply(factor))
    }

    private fun zoomTo(f: Float) {
        if (f == factor) return
        Log.d(TAG, "zoom $factor -> $f")
        // The pointer is only followed while zoomed: where it is now, if Android knows.
        if (factor <= 1f) readPointer()
        factor = f
        mainExecutor.removeCallbacks(disableRunnable)
        if (f <= 1f) {
            stopFollowing()
            HyprZoomService.instance?.reset(displayId, true)
            // After the reset animation; the service's unbinding would cut it short.
            mainExecutor.executeDelayed(disableRunnable, DISABLE_DELAY_MS)
            return
        }
        startFollowing()
        if (HyprZoomService.instance == null) setServiceEnabled(true) else apply(animate = true)
    }

    /** Sets the magnification for [factor] and the pointer; nothing until the service is connected. */
    private fun apply(animate: Boolean) {
        val service = HyprZoomService.instance ?: return
        if (factor <= 1f) return
        val size = displaySize() ?: return
        val px = if (pointerX.isNaN()) size.x / 2f else pointerX
        val py = if (pointerY.isNaN()) size.y / 2f else pointerY
        val cx = zoomCentre(px, size.x.toFloat(), factor)
        val cy = zoomCentre(py, size.y.toFloat(), factor)
        if (!service.magnify(displayId, factor, cx, cy, animate)) {
            Log.w(TAG, "magnification refused (display $displayId, $factor at $cx,$cy)")
        }
    }

    private fun readPointer() {
        val p = runCatching { inputManager.getCursorPosition(displayId) }.getOrNull()
        Log.d(TAG, "pointer at ${p?.x},${p?.y} (last ${pointerX},${pointerY})")
        if (p == null) return
        pointerX = p.x
        pointerY = p.y
    }

    private fun startFollowing() {
        if (monitor != null) return
        val m = inputManager.monitorGestureInput("hypr-zoom", displayId)
        monitor = m
        receiver = object : InputEventReceiver(m.inputChannel, Looper.myLooper()) {
            override fun onInputEvent(event: InputEvent) {
                try {
                    if (event is MotionEvent) onMotion(event)
                } finally {
                    finishInputEvent(event, false)
                }
            }
        }
    }

    private fun stopFollowing() {
        receiver?.dispose()
        receiver = null
        monitor?.dispose()
        monitor = null
    }

    private fun onMotion(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_HOVER_MOVE, MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_MOVE,
            MotionEvent.ACTION_DOWN -> {
                if (e.rawX == pointerX && e.rawY == pointerY) return
                pointerX = e.rawX
                pointerY = e.rawY
                apply(animate = false)
            }
        }
    }

    private fun displaySize(): Point? {
        val display = context.getSystemService(DisplayManager::class.java)?.getDisplay(displayId)
            ?: return null
        return Point().also { display.getRealSize(it) }
    }

    /** Adds or removes [HyprZoomService] in the current user's enabled accessibility services. */
    private fun setServiceEnabled(on: Boolean) {
        val resolver = context.contentResolver
        val user = ActivityManager.getCurrentUser()
        val key = Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES
        val current = Settings.Secure.getStringForUser(resolver, key, user).orEmpty()
        val others = current.split(':').filter {
            it.isNotEmpty() && ComponentName.unflattenFromString(it) != component
        }
        val wanted = (if (on) others + component.flattenToString() else others).joinToString(":")
        if (wanted == current) return
        Log.i(TAG, "accessibility service ${if (on) "on" else "off"} for user $user")
        Settings.Secure.putStringForUser(resolver, key, wanted, user)
        if (on) Settings.Secure.putIntForUser(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 1, user)
    }

    private companion object {
        const val TAG = "HyprZoom"
        const val DISABLE_DELAY_MS = 1000L
    }
}
