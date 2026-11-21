package com.android.systemui.wmshell

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.graphics.Rect
import android.graphics.RectF
import android.util.Log
import android.view.Choreographer
import android.view.SurfaceControl
import android.view.animation.LinearInterpolator
import android.window.TransitionInfo
import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.Bezier
import com.android.hyprtile.WindowStyle
import com.android.hyprtile.WorkspaceStyle
import kotlin.math.min

/**
 * Plays Hyprland-style animations on Shell transition surfaces (Phase D3,
 * docs/hypr-desktop.md). Every animated surface is a [Track] with its own duration and bezier
 * curve from [AnimationConfig]; one [ValueAnimator] drives all tracks of a transition and the
 * transition finishes when the longest one ends.
 *
 * - Windows opening (`windowsIn` + `fadeIn`): popin (grow from N % around the centre) or slide.
 * - Windows closing (`windowsOut` + `fadeOut`): the reverse.
 * - Windows moving or resizing (`windowsMove`): position and size interpolated; the content
 *   (already drawn at its new size) is scaled to the current size, as Hyprland stretches it.
 * - Workspace switch (`workspacesIn`/`workspacesOut`): slide, slidevert, fade, slidefade.
 *
 * Runs on Shell's main thread, where transitions are started and finished.
 */
class HyprAnimator {
    var config: AnimationConfig = HyprStyleProps.animations(android.os.Bundle())

    /** One animated surface: [apply] sets its state at [elapsedMs] into the transaction. */
    private class Track(val durationMs: Long, val apply: (SurfaceControl.Transaction, Long) -> Unit)

    /** A window of a transition and what happens to it. */
    class WindowChange(
        val leash: SurfaceControl,
        val kind: Kind,
        /** Bounds before and after, in the leash's parent's coordinates. */
        val from: Rect,
        val to: Rect,
        /** Opacity the window ends with (inactive windows are translucent). */
        val alpha: Float,
        /** Display bounds in the same coordinates, for slide styles. */
        val display: Rect,
    ) {
        enum class Kind { OPEN, CLOSE, MOVE }
    }

    /**
     * Animates [changes]; [startT] is applied here, [onFinished] runs on the main thread when
     * all are done (the caller sets the final state in the finish transaction).
     */
    fun animateWindows(
        changes: List<WindowChange>,
        startT: SurfaceControl.Transaction,
        onFinished: () -> Unit,
    ) {
        val tracks = changes.mapNotNull { windowTrack(it) }
        for (t in tracks) t.apply(startT, 0)
        play(tracks, startT, onFinished)
    }

    private fun windowTrack(c: WindowChange): Track? {
        val cfg = config
        return when (c.kind) {
            WindowChange.Kind.MOVE -> {
                val spec = cfg.resolve("windowsMove")
                if (!spec.enabled || c.from == c.to) return null
                val curve = cfg.curveNamed(spec.curve)
                val d = spec.durationMs
                // The buffer has the new size: scale it to the current one.
                val bw = c.to.width().coerceAtLeast(1).toFloat()
                val bh = c.to.height().coerceAtLeast(1).toFloat()
                Track(d) { t, ms ->
                    val p = progress(curve, ms, d)
                    val r = lerp(c.from, c.to, p)
                    t.setMatrix(c.leash, r.width() / bw, 0f, 0f, r.height() / bh)
                    t.setPosition(c.leash, r.left, r.top)
                }
            }
            WindowChange.Kind.OPEN, WindowChange.Kind.CLOSE -> {
                val open = c.kind == WindowChange.Kind.OPEN
                val node = if (open) "windowsIn" else "windowsOut"
                val fadeNode = if (open) "fadeIn" else "fadeOut"
                val spec = cfg.resolve(node)
                val fade = cfg.resolve(fadeNode)
                val style = runCatching { cfg.windowStyle(node) }.getOrDefault(WindowStyle.Popin())
                val curve = cfg.curveNamed(spec.curve)
                val fadeCurve = cfg.curveNamed(fade.curve)
                val d = spec.durationMs
                val fd = fade.durationMs
                val rect = if (open) c.to else c.from
                val bw = rect.width().coerceAtLeast(1).toFloat()
                val bh = rect.height().coerceAtLeast(1).toFloat()
                val startAlpha = if (open) 0f else c.alpha
                val endAlpha = if (open) c.alpha else 0f
                val offscreen = offscreenRect(style, rect, c.display)
                Track(maxOf(d, fd)) { t, ms ->
                    // Progress from "away" (0) to "in place" (1).
                    val p = if (d == 0L) 1f else progress(curve, ms, d).let { if (open) it else 1 - it }
                    val r: RectF = when (style) {
                        is WindowStyle.Popin -> {
                            val s0 = style.percent / 100f
                            val s = s0 + (1 - s0) * p
                            val cx = rect.exactCenterX()
                            val cy = rect.exactCenterY()
                            RectF(cx - bw * s / 2, cy - bh * s / 2, cx + bw * s / 2, cy + bh * s / 2)
                        }
                        is WindowStyle.Slide -> lerp(offscreen ?: rect, rect, p)
                        WindowStyle.Fade -> RectF(rect)
                    }
                    t.setMatrix(c.leash, r.width() / bw, 0f, 0f, r.height() / bh)
                    t.setPosition(c.leash, r.left, r.top)
                    val a = if (fd == 0L) endAlpha else {
                        startAlpha + (endAlpha - startAlpha) * progress(fadeCurve, ms, fd)
                    }
                    t.setAlpha(c.leash, a.coerceIn(0f, 1f))
                }
            }
        }
    }

    /** Where a sliding window comes from: just past the display edge it is nearest to. */
    private fun offscreenRect(style: WindowStyle, r: Rect, display: Rect): Rect? {
        if (style !is WindowStyle.Slide) return null
        val edge = style.edge ?: run {
            val dists = mapOf(
                WindowStyle.Edge.LEFT to r.left - display.left,
                WindowStyle.Edge.RIGHT to display.right - r.right,
                WindowStyle.Edge.TOP to r.top - display.top,
                WindowStyle.Edge.BOTTOM to display.bottom - r.bottom,
            )
            dists.minBy { it.value }.key
        }
        return Rect(r).apply {
            when (edge) {
                WindowStyle.Edge.LEFT -> offset(display.left - r.right, 0)
                WindowStyle.Edge.RIGHT -> offset(display.right - r.left, 0)
                WindowStyle.Edge.TOP -> offset(0, display.top - r.bottom)
                WindowStyle.Edge.BOTTOM -> offset(0, display.bottom - r.top)
            }
        }
    }

    /**
     * A switch between two desks of a display (`workspace N`). [forward] is true when the new
     * desk comes after the old one (it comes in from the right, or from below). Desk leashes
     * move; with a fade style their tasks fade. [toAlpha] gives each incoming task its opacity.
     */
    fun animateDeskSwitch(
        display: Rect,
        fromDesk: SurfaceControl?,
        toDesk: SurfaceControl?,
        fromTasks: List<SurfaceControl>,
        toTasks: List<Pair<SurfaceControl, Float>>,
        forward: Boolean,
        startT: SurfaceControl.Transaction,
        onFinished: () -> Unit,
    ) {
        val cfg = config
        val inSpec = cfg.resolve("workspacesIn")
        val outSpec = cfg.resolve("workspacesOut")
        val inStyle = runCatching { cfg.workspaceStyle("workspacesIn") }.getOrDefault(WorkspaceStyle())
        val outStyle = runCatching { cfg.workspaceStyle("workspacesOut") }.getOrDefault(WorkspaceStyle())
        val sign = if (forward) 1 else -1
        fun shift(style: WorkspaceStyle): Pair<Float, Float> {
            if (!style.slide) return 0f to 0f
            val dx = if (style.vertical) 0f else display.width() * style.slidePercent / 100f
            val dy = if (style.vertical) display.height() * style.slidePercent / 100f else 0f
            return dx to dy
        }
        val tracks = ArrayList<Track>()
        val (ix, iy) = shift(inStyle)
        val (ox, oy) = shift(outStyle)
        val inCurve = cfg.curveNamed(inSpec.curve)
        val outCurve = cfg.curveNamed(outSpec.curve)
        val inD = inSpec.durationMs
        val outD = outSpec.durationMs
        // The new desk comes from +shift (forward) and the old one leaves to -shift.
        toDesk?.let { leash ->
            startT.setAlpha(leash, 1f)
            tracks += Track(inD) { t, ms ->
                val q = 1 - progress(inCurve, ms, inD)
                t.setPosition(leash, display.left + sign * ix * q, display.top + sign * iy * q)
            }
        }
        fromDesk?.let { leash ->
            tracks += Track(outD) { t, ms ->
                val p = progress(outCurve, ms, outD)
                t.setPosition(leash, display.left - sign * ox * p, display.top - sign * oy * p)
            }
        }
        for ((leash, alpha) in toTasks) {
            tracks += Track(inD) { t, ms ->
                val a = if (inStyle.fade) alpha * progress(inCurve, ms, inD) else alpha
                t.setAlpha(leash, a)
            }
        }
        for (leash in fromTasks) {
            tracks += Track(outD) { t, ms ->
                val a = if (outStyle.fade) 1 - progress(outCurve, ms, outD) else 1f
                t.setAlpha(leash, a)
            }
        }
        for (t in tracks) t.apply(startT, 0)
        play(tracks, startT, onFinished)
    }

    /**
     * Animates one surface from [from] to [to] outside a transition (a mouse-dragged window
     * going back to its tile), with `windowsMove`.
     */
    fun animateLeash(leash: SurfaceControl, from: Rect, to: Rect) {
        val c = WindowChange(leash, WindowChange.Kind.MOVE, from, to, 1f, Rect())
        val track = windowTrack(c) ?: return
        val t = SurfaceControl.Transaction()
        track.apply(t, 0)
        play(listOf(track), t) {}
    }

    private fun play(tracks: List<Track>, startT: SurfaceControl.Transaction, onFinished: () -> Unit) {
        startT.apply()
        val total = tracks.maxOfOrNull { it.durationMs } ?: 0L
        if (total <= 0L) {
            onFinished()
            return
        }
        val t = SurfaceControl.Transaction()
        val anim = ValueAnimator.ofFloat(0f, 1f).setDuration(total)
        anim.interpolator = LinearInterpolator()
        anim.addUpdateListener {
            val ms = (it.animatedFraction * total).toLong()
            for (tr in tracks) tr.apply(t, ms)
            t.setFrameTimeline(Choreographer.getInstance().vsyncId)
            t.apply()
        }
        anim.addListener(object : AnimatorListenerAdapter() {
            override fun onAnimationEnd(animation: Animator) {
                for (tr in tracks) tr.apply(t, tr.durationMs)
                t.apply()
                t.close()
                onFinished()
            }
        })
        anim.start()
        if (DEBUG) Log.d(TAG, "playing ${tracks.size} tracks for $total ms")
    }

    companion object {
        private const val TAG = "HyprAnimator"
        private const val DEBUG = false

        fun progress(curve: Bezier, ms: Long, durationMs: Long): Float =
            if (durationMs <= 0) 1f else curve.value(min(1f, ms.toFloat() / durationMs))

        fun lerp(a: Rect, b: Rect, p: Float) = RectF(
            a.left + (b.left - a.left) * p,
            a.top + (b.top - a.top) * p,
            a.right + (b.right - a.right) * p,
            a.bottom + (b.bottom - a.bottom) * p,
        )

        /**
         * A task change of [info] in its parent's coordinates: [TransitionInfo.Change]'s
         * bounds are absolute, its end offset is relative to the parent.
         */
        fun relBounds(c: TransitionInfo.Change): Pair<Rect, Rect> {
            val end = Rect(c.endAbsBounds)
            val dx = c.endRelOffset.x - end.left
            val dy = c.endRelOffset.y - end.top
            val start = Rect(c.startAbsBounds).apply { offset(dx, dy) }
            end.offset(dx, dy)
            return start to end
        }
    }
}
