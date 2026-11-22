package com.android.systemui.wmshell

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.MagnificationConfig
import android.util.Log
import android.view.accessibility.AccessibilityEvent

/**
 * The accessibility service behind [HyprZoom] (Phase E10): it only holds the
 * `canControlMagnification` capability (res/xml/hypr_zoom_service.xml) and asks for no events,
 * no gestures and no key events. AOSP has no other way for an app to set the full-screen
 * magnification's scale and centre: system_server's magnification controller is reached
 * through an accessibility service connection, IAccessibilityManager only has
 * `enableMagnificationAndZoomIn` (one step, no position). [HyprZoom] enables this service while
 * the screen is zoomed and disables it again at factor 1, so apps see accessibility off
 * otherwise. Runs in PcSystemUI's process, so [HyprZoom] calls it directly.
 */
class HyprZoomService : AccessibilityService() {

    override fun onServiceConnected() {
        Log.i(TAG, "connected")
        instance = this
        onConnected?.invoke()
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        if (instance === this) instance = null
        Log.i(TAG, "disconnected")
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    /** Full-screen magnification of [displayId] at [scale], centred on ([cx], [cy]) in screen pixels. */
    fun magnify(displayId: Int, scale: Float, cx: Float, cy: Float, animate: Boolean): Boolean {
        val config = MagnificationConfig.Builder()
            .setMode(MagnificationConfig.MAGNIFICATION_MODE_FULLSCREEN)
            .setScale(scale)
            .setCenterX(cx)
            .setCenterY(cy)
            .build()
        return controllerFor(displayId).setMagnificationConfig(config, animate)
    }

    fun reset(displayId: Int, animate: Boolean): Boolean = controllerFor(displayId).resetCurrentMagnification(animate)

    private fun controllerFor(displayId: Int): AccessibilityService.MagnificationController =
        if (displayId == android.view.Display.DEFAULT_DISPLAY) {
            magnificationController
        } else {
            getMagnificationController(displayId)
        }

    companion object {
        private const val TAG = "HyprZoomService"

        /** The connected service, or null. */
        @Volatile
        var instance: HyprZoomService? = null
            private set

        /** Called on the service's thread once it is connected. */
        @Volatile
        var onConnected: (() -> Unit)? = null
    }
}
