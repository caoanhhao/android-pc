package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.Shader
import android.gui.BorderSettings
import android.gui.BoxShadowSettings
import android.hardware.HardwareBuffer
import android.util.Log
import android.view.SurfaceControl
import android.window.DesktopExperienceFlags
import com.android.hyprtile.DecorationConfig
import com.android.hyprtile.Gradient
import com.android.wm.shell.windowdecor.WindowDecorStyle
import kotlin.math.max
import kotlin.math.min

/**
 * Hyprland-style window decoration (Phase D3, docs/hypr-desktop.md), plugged into Shell's
 * decorations through [WindowDecorStyle] (tools/patches/aosp17-shell-decor-style.patch):
 *
 * - Tiled windows get no caption ([DecorationConfig.caption]); floating ones keep Shell's.
 * - Every relayout of a desktop window sets the corner radius ([DecorationConfig.rounding]) and
 *   the opacity (active/inactive) on the task surface in Shell's own transactions, after
 *   Shell's values, so they win and move in step with Shell's changes.
 * - The border is drawn by four buffer layers above the window content (top and bottom strips
 *   that hold the rounded corners, left and right edges), clipped by the task's rounded corners.
 *   Each strip is drawn with the gradient of the whole window ([Gradient.line]) and comes from a
 *   small software bitmap, so a resize redraws a few kilobytes instead of a window-sized
 *   buffer. Buffers are kept per focus state until the window's size changes, so a focus
 *   change only swaps buffers. The buffers are set in the relayout's start transaction, so they
 *   change in the same frame as the window.
 *
 * Shell's own SurfaceFlinger border (BorderSettings, a solid stroke) is turned off where Shell
 * sets it; its drop shadow stays unless [DecorationConfig.shadow] is false.
 * Runs on Shell's main thread.
 */
class HyprDecorations(
    private val enabled: () -> Boolean,
    private val isTiled: (Int) -> Boolean,
    private val isVisible: (Int) -> Boolean,
    /** A window rule's opacity for a task (active, inactive), or null for the configuration's. */
    private val opacityRule: (Int) -> Pair<Float, Float>? = { null },
) : WindowDecorStyle {

    var config: DecorationConfig = HyprStyleProps.decoration(android.os.Bundle())
        private set

    private val windows = HashMap<Int, Window>()

    /**
     * Opacity set with `hyprctl setprop` (Phase E12): active and inactive, each null when unset;
     * over a window rule's. Entries of closed tasks are harmless and dropped with them.
     */
    val propOpacity = HashMap<Int, Pair<Float?, Float?>>()
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    /** Opaque base under the border, so a translucent border colour does not show the app. */
    private val basePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        color = Color.BLACK
    }

    /** Starts styling Shell's decorations. */
    fun install() {
        WindowDecorStyle.instance = this
        Log.i(TAG, "decorations: $config")
    }

    /** Takes a new configuration and restyles every window at once. */
    fun setConfig(cfg: DecorationConfig) {
        if (cfg == config) return
        config = cfg
        Log.i(TAG, "decorations: $config")
        val t = SurfaceControl.Transaction()
        for (w in windows.values) {
            w.invalidate()
            w.restyle(t, t)
        }
        t.apply()
    }

    /** The opacity window [taskId] should have: setprop's, its window rule's, else the configuration's. */
    fun opacity(taskId: Int, focused: Boolean): Float {
        val rule = opacityRule(taskId)
        val prop = propOpacity[taskId]
        return if (focused) prop?.first ?: rule?.first ?: config.activeOpacity
        else prop?.second ?: rule?.second ?: config.inactiveOpacity
    }

    /** The opacity of [taskId] as last decorated (1 when unknown). */
    fun opacityOf(taskId: Int): Float =
        windows[taskId]?.takeIf { it.freeform && enabled() }?.let { opacity(taskId, it.focused) } ?: 1f

    /**
     * Sets the opacity of every visible desktop window again. Shell's animations end with
     * alpha 1 in their finish transaction, which comes after the relayout's.
     */
    fun reapplyOpacity() {
        if (!enabled()) return
        val t = SurfaceControl.Transaction()
        var any = false
        for (w in windows.values) {
            val s = w.surface ?: continue
            if (!w.freeform || !isVisible(w.taskId)) continue
            t.setAlpha(s, opacity(w.taskId, w.focused))
            any = true
        }
        if (any) t.apply()
    }

    /** Desktop windows whose caption does not match what [hidesCaption] says now. */
    fun staleCaptions(): List<Int> =
        if (!enabled()) emptyList()
        else windows.values.filter {
            it.freeform && it.hasCaption != config.hasCaption(floating = !isTiled(it.taskId))
        }.map { it.taskId }

    /** Our reference to the surface of [taskId] while it is decorated. */
    fun surfaceOf(taskId: Int): SurfaceControl? = windows[taskId]?.surface

    // ---- WindowDecorStyle ----

    override fun hidesCaption(taskInfo: RunningTaskInfo): Boolean =
        enabled() && !config.hasCaption(floating = !isTiled(taskInfo.taskId))

    /**
     * A fullscreen window (`fullscreen` dispatcher) has no app handle: HyprBar replaces the
     * status bar the handle sits in, so it is never shown, yet its spy window covered the
     * whole task and stole every press (the fingerprint enroll screen could not be touched).
     */
    override fun hidesAppHandle(taskInfo: RunningTaskInfo): Boolean = enabled()

    override fun onRelayout(
        taskInfo: RunningTaskInfo,
        taskSurface: SurfaceControl,
        hasGlobalFocus: Boolean,
        inSyncWithTransition: Boolean,
        startT: SurfaceControl.Transaction,
        finishT: SurfaceControl.Transaction,
    ) {
        if (!enabled()) {
            windows.remove(taskInfo.taskId)?.release(startT)
            return
        }
        val w = windows.getOrPut(taskInfo.taskId) { Window(taskInfo.taskId) }
        w.update(taskInfo, taskSurface, hasGlobalFocus, startT, finishT)
    }

    override fun onClosed(taskId: Int) {
        val w = windows.remove(taskId) ?: return
        val t = SurfaceControl.Transaction()
        w.release(t)
        t.apply()
    }

    // ---- one window ----

    private inner class Window(val taskId: Int) {
        /** Our own reference to the task surface (Shell releases its copy on close). */
        var surface: SurfaceControl? = null
        var freeform = false
        var focused = false
        var hasCaption = false
        private var width = 0
        private var height = 0
        private var styled = false
        private val layers = arrayOfNulls<SurfaceControl>(4)
        private val strips = arrayOfNulls<Rect>(4)
        /** Strip buffers for the current size, per focus state. */
        private val buffers = HashMap<Boolean, Array<HardwareBuffer?>>()
        private var shown: Boolean? = null

        fun update(
            taskInfo: RunningTaskInfo,
            taskSurface: SurfaceControl,
            hasGlobalFocus: Boolean,
            startT: SurfaceControl.Transaction,
            finishT: SurfaceControl.Transaction,
        ) {
            val old = surface
            if (old == null || !old.isSameSurface(taskSurface)) {
                old?.release()
                surface = SurfaceControl().apply { copyFrom(taskSurface, TAG) }
                for (l in layers) l?.let { startT.reparent(it, taskSurface) }
            }
            freeform = taskInfo.isFreeform
            focused = hasGlobalFocus
            hasCaption = !hidesCaption(taskInfo)
            val b = taskInfo.configuration.windowConfiguration.bounds
            if (b.width() != width || b.height() != height) {
                width = b.width()
                height = b.height()
                invalidate()
            }
            restyle(startT, finishT)
        }

        /** Sets radius, opacity, Shell's outline and the border for the current state. */
        fun restyle(startT: SurfaceControl.Transaction, finishT: SurfaceControl.Transaction) {
            val s = surface ?: return
            val cfg = config
            if (!freeform) {
                // Maximized out of the desk or fullscreen: square, opaque, no border.
                if (styled) {
                    for (t in arrayOf(startT, finishT)) t.setCornerRadius(s, 0f).setAlpha(s, 1f)
                    styled = false
                }
                hideLayers(startT)
                return
            }
            styled = true
            val alpha = opacity(taskId, focused)
            for (t in arrayOf(startT, finishT)) {
                t.setCornerRadius(s, cfg.rounding.toFloat())
                t.setAlpha(s, alpha)
            }
            if (hasCaption && DesktopExperienceFlags.ENABLE_FREEFORM_BOX_SHADOWS_V2.isTrue) {
                // Shell's solid SurfaceFlinger stroke; ours replaces it.
                val noBorder = BorderSettings().apply { strokeWidth = 0f }
                startT.setBorderSettings(s, noBorder)
                finishT.setBorderSettings(s, noBorder)
                if (!cfg.shadow) {
                    val noShadow = BoxShadowSettings()
                    startT.setBoxShadowSettings(s, noShadow)
                    finishT.setBoxShadowSettings(s, noShadow)
                }
            }
            showBorder(startT, s)
        }

        private fun showBorder(t: SurfaceControl.Transaction, parent: SurfaceControl) {
            val cfg = config
            if (cfg.borderSize <= 0 || width <= 0 || height <= 0) {
                hideLayers(t)
                return
            }
            if (shown == focused && layers[0] != null) return
            val bufs = buffers.getOrPut(focused) {
                drawStrips(if (focused) cfg.activeBorder else cfg.inactiveBorder, cfg)
            }
            for (i in 0 until 4) {
                val r = strips[i]
                val buf = bufs[i]
                if (r == null || buf == null) {
                    layers[i]?.let { t.hide(it) }
                    continue
                }
                val l = layers[i] ?: SurfaceControl.Builder()
                    .setName("HyprBorder#$taskId.$i")
                    .setParent(parent)
                    .setCallsite(TAG)
                    .build()
                    .also { layers[i] = it }
                t.setBuffer(l, buf)
                    .setPosition(l, r.left.toFloat(), r.top.toFloat())
                    .setLayer(l, Int.MAX_VALUE)
                    .show(l)
            }
            shown = focused
        }

        /**
         * Draws the four strips of a border of [g] for the current size: the top and bottom
         * strips are as tall as the corner (so they hold the rounded corners), the side strips
         * fill the edges between them.
         */
        private fun drawStrips(g: Gradient, cfg: DecorationConfig): Array<HardwareBuffer?> {
            val w = width
            val h = height
            val bw = min(cfg.borderSize, min(w, h) / 2).coerceAtLeast(1)
            val cap = min(max(cfg.rounding, bw), min(w, h) / 2)
            strips[0] = Rect(0, 0, w, cap)
            strips[1] = Rect(0, h - cap, w, h)
            strips[2] = Rect(0, cap, bw, h - cap).takeIf { !it.isEmpty }
            strips[3] = Rect(w - bw, cap, w, h - cap).takeIf { !it.isEmpty }
            val half = bw / 2f
            val box = RectF(half, half, w - half, h - half)
            val radius = max(cfg.rounding - half, 0f)
            // The base covers the stroke and one pixel past the edge, so it also hides the anti-aliased rim
            // the task's corner clip leaves outside the stroke.
            val baseBox = RectF(box).apply { inset(-0.5f, -0.5f) }
            paint.strokeWidth = bw.toFloat()
            basePaint.strokeWidth = bw + 1f
            if (g.colors.size == 1) {
                paint.shader = null
                paint.color = g.colors[0]
            } else {
                val line = g.line(w, h)
                paint.shader = LinearGradient(line[0], line[1], line[2], line[3],
                    g.colors.toIntArray(), null, Shader.TileMode.CLAMP)
                paint.color = 0xff000000.toInt()
            }
            return Array(4) { i ->
                val r = strips[i] ?: return@Array null
                val bmp = Bitmap.createBitmap(r.width(), r.height(), Bitmap.Config.ARGB_8888)
                Canvas(bmp).apply {
                    translate(-r.left.toFloat(), -r.top.toFloat())
                    // The border lies over the window's outer pixels: without an opaque base,
                    // a translucent colour (the inactive default is rgba(595959aa)) lets the
                    // app's edge show through and the line looks broken wherever it changes.
                    drawRoundRect(baseBox, radius + 0.5f, radius + 0.5f, basePaint)
                    drawRoundRect(box, radius, radius, paint)
                }
                val hw = bmp.copy(Bitmap.Config.HARDWARE, false)
                bmp.recycle()
                hw.hardwareBuffer.also { hw.recycle() }
            }
        }

        private fun hideLayers(t: SurfaceControl.Transaction) {
            for (l in layers) l?.let { t.hide(it) }
            shown = null
        }

        /** Drops the drawn buffers (size or configuration changed). */
        fun invalidate() {
            for (bufs in buffers.values) for (b in bufs) b?.close()
            buffers.clear()
            shown = null
        }

        fun release(t: SurfaceControl.Transaction) {
            for (i in layers.indices) {
                layers[i]?.let { t.remove(it) }
                layers[i] = null
            }
            invalidate()
            surface?.release()
            surface = null
        }
    }

    private companion object {
        const val TAG = "HyprDecorations"
    }
}
