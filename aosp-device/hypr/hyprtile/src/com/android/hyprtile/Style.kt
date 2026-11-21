package com.android.hyprtile

import kotlin.math.PI
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * Parses one colour as Hyprland writes it: `rgba(33ccffee)`, `rgb(33ccff)`, `0xee33ccff` (ARGB),
 * or `#33ccff` / `#33ccffee` (RGB, RGBA). Returns ARGB. Throws on anything else.
 */
fun parseColor(text: String): Int {
    val t = text.trim().lowercase()
    fun hex(s: String) = s.toLong(16).toInt()
    fun rgba(s: String): Int = when (s.length) {
        6 -> (0xff shl 24) or hex(s)
        8 -> (hex(s.substring(6)) shl 24) or hex(s.substring(0, 6))
        else -> throw IllegalArgumentException("bad colour $text")
    }
    return try {
        when {
            t.startsWith("rgba(") && t.endsWith(")") ->
                rgba(t.substring(5, t.length - 1).trim().also { require(it.length == 8) })
            t.startsWith("rgb(") && t.endsWith(")") ->
                rgba(t.substring(4, t.length - 1).trim().also { require(it.length == 6) })
            t.startsWith("0x") && t.length == 10 -> hex(t.substring(2))
            t.startsWith("#") -> rgba(t.substring(1))
            else -> throw IllegalArgumentException("bad colour $text")
        }
    } catch (e: NumberFormatException) {
        throw IllegalArgumentException("bad colour $text", e)
    }
}

/**
 * A border gradient, Hyprland's `col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg`: one
 * or more colours spread evenly, rotated by [angle] degrees (0 runs left to right, 90 top to
 * bottom).
 */
data class Gradient(val colors: List<Int>, val angle: Int = 0) {
    init {
        require(colors.isNotEmpty()) { "a gradient needs a colour" }
    }

    /**
     * Where a point of a box falls on the gradient, 0 (first colour) to 1 (last colour), for
     * normalized box coordinates [nx], [ny] (0..1, y down). Hyprland's border shader, so borders
     * look the same: the angle is folded into 0..90 degrees by mirroring the box, and the
     * progress mixes x and y with sin(angle) and 1 - sin(angle).
     */
    fun progress(nx: Float, ny: Float): Float {
        if (colors.size < 2) return 0f
        var x = nx
        var y = ny
        val a = ((angle % 360 + 360) % 360) * PI / 180.0
        val folded = when {
            a > 3 * PI / 2 -> { y = 1 - y; 2 * PI - a }
            a > PI -> { x = 1 - x; y = 1 - y; a - PI }
            a > PI / 2 -> { x = 1 - x; PI - a }
            else -> a
        }
        val s = sin(folded).toFloat()
        return (y * s + x * (1 - s)).coerceIn(0f, 1f)
    }

    /**
     * The gradient line for a [width] x [height] box: progress 0 at the first point, 1 at the
     * second, as a linear gradient shader takes it. [progress] is linear in x and y, so a
     * linear gradient along its gradient vector draws it exactly.
     */
    fun line(width: Int, height: Int): FloatArray {
        val w = width.coerceAtLeast(1).toFloat()
        val h = height.coerceAtLeast(1).toFloat()
        val c0 = progress(0f, 0f)
        val gx = (progress(1f, 0f) - c0) / w
        val gy = (progress(0f, 1f) - c0) / h
        val len2 = gx * gx + gy * gy
        if (len2 == 0f) return floatArrayOf(0f, 0f, w, 0f)
        val sx = -c0 * gx / len2
        val sy = -c0 * gy / len2
        return floatArrayOf(sx, sy, sx + gx / len2, sy + gy / len2)
    }

    override fun toString() =
        colors.joinToString(" ") { "rgba(%08x)".format((it shl 8) or (it ushr 24)) } + " ${angle}deg"

    companion object {
        /** Parses Hyprland's gradient syntax: colours separated by spaces, an optional `Ndeg`. */
        fun parse(text: String): Gradient {
            val parts = text.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
            var angle = 0
            val colors = ArrayList<Int>()
            for (p in parts) {
                if (p.endsWith("deg")) {
                    angle = p.removeSuffix("deg").toFloat().roundToInt()
                } else {
                    colors += parseColor(p)
                }
            }
            return Gradient(colors, angle)
        }
    }
}

/** Which windows get Shell's caption (title bar with close, maximize and minimize buttons). */
enum class CaptionMode {
    /** No caption anywhere, as in Hyprland; windows are moved and resized with MOD + mouse. */
    NONE,
    /** Only floating windows keep the caption; tiled windows have none. */
    FLOATING,
    /** Every desktop window keeps Shell's caption. */
    ALL;

    companion object {
        fun parse(text: String): CaptionMode = when (text.trim().lowercase()) {
            "none", "off", "0", "false" -> NONE
            "floating", "float" -> FLOATING
            "all", "on", "1", "true" -> ALL
            else -> throw IllegalArgumentException("bad caption mode $text")
        }
    }
}

/**
 * Window decoration, Hyprland's `general:border_size`, `general:col.*` and `decoration:*`.
 * Pixels are physical pixels of the display.
 */
data class DecorationConfig(
    /** decoration:rounding, the corner radius of windows. */
    val rounding: Int = 10,
    /** general:border_size; 0 draws no border. */
    val borderSize: Int = 2,
    /** general:col.active_border, the focused window's border. */
    val activeBorder: Gradient = Gradient(listOf(0xee33ccff.toInt(), 0xee00ff99.toInt()), 45),
    /** general:col.inactive_border, every other window's border. */
    val inactiveBorder: Gradient = Gradient(listOf(0xaa595959.toInt())),
    /** decoration:active_opacity. */
    val activeOpacity: Float = 1f,
    /** decoration:inactive_opacity. */
    val inactiveOpacity: Float = 1f,
    /** Which windows keep Shell's caption. */
    val caption: CaptionMode = CaptionMode.FLOATING,
    /** Shell's drop shadow under floating windows (Hyprland's decoration:shadow:enabled). */
    val shadow: Boolean = true,
) {
    init {
        require(rounding >= 0 && borderSize >= 0) { "rounding and border_size must not be negative" }
        require(activeOpacity in 0f..1f && inactiveOpacity in 0f..1f) { "opacity must be in 0..1" }
    }

    /** Whether a window gets Shell's caption. */
    fun hasCaption(floating: Boolean) = when (caption) {
        CaptionMode.NONE -> false
        CaptionMode.FLOATING -> floating
        CaptionMode.ALL -> true
    }
}
