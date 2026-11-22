package com.android.hyprtile

/**
 * The argument of the `zoom` dispatcher (Phase E10): a signed number changes the zoom factor by
 * that much (`+0.5`, `-0.2`), an unsigned one sets it (`1` is no zoom). Hyprland itself zooms with
 * the option `cursor:zoom_factor`; this dispatcher is ours (Lua: `hl.dsp.zoom({ delta = 0.5 })`,
 * `hl.dsp.zoom({ factor = 1 })`).
 */
sealed interface ZoomOp {
    data class By(val delta: Float) : ZoomOp
    data class To(val factor: Float) : ZoomOp

    /**
     * The factor after this change of [current], kept between 1 and [MAX_ZOOM] and rounded to
     * hundredths, so repeated steps do not drift (1.2 + 0.2 is 1.4, not 1.4000001).
     */
    fun apply(current: Float): Float = clampZoom(
        Math.round(
            when (this) {
                is By -> current + delta
                is To -> factor
            } * 100f
        ) / 100f
    )
}

/** Android's full-screen magnification goes up to 8 (MagnificationScaleProvider.MAX_SCALE). */
const val MAX_ZOOM = 8f

fun clampZoom(factor: Float): Float = if (factor.isNaN()) 1f else factor.coerceIn(1f, MAX_ZOOM)

/** Parses the `zoom` argument; null when it is not a number (or `reset`, which is `1`). */
fun parseZoom(arg: String): ZoomOp? {
    val a = arg.trim().lowercase()
    if (a == "reset") return ZoomOp.To(1f)
    val v = a.toFloatOrNull()?.takeIf { it.isFinite() } ?: return null
    return if (a.startsWith("+") || a.startsWith("-")) ZoomOp.By(v) else ZoomOp.To(v)
}

/** The `zoom` argument for a Lua `delta` (always signed) or `factor`. */
fun zoomArg(delta: Float? = null, factor: Float? = null): String = when {
    delta != null -> (if (delta >= 0) "+" else "") + delta
    factor != null -> factor.toString()
    else -> throw IllegalArgumentException("zoom needs delta or factor")
}

/**
 * The magnification centre on one axis that zooms around the pointer like Hyprland: the content
 * under the pointer stays under it, so content point q shows at `pointer + (q - pointer) * scale`.
 * Android magnifies around a centre that lands in the middle of the screen; solving
 * `pointer = (pointer - c) * scale + size / 2` gives c. At the screen's edges it shows the
 * edges, so the result never leaves `[size / 2scale, size - size / 2scale]`.
 */
fun zoomCentre(pointer: Float, size: Float, scale: Float): Float {
    val p = pointer.coerceIn(0f, size)
    return p - (p - size / 2) / scale
}
