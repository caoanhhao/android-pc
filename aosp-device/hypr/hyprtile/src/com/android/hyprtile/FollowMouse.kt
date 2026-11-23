package com.android.hyprtile

import kotlin.math.hypot

/**
 * Hyprland's `input:follow_mouse` and its companions (docs/hypr-desktop.md, "Focus follows
 * the mouse"), as the wire names of Lua's `input.*` options.
 *
 * - [mode] 1 (the default): the window under the pointer takes the focus as the pointer moves.
 * - 0 and 2: a click focuses (Android sends pointer events to the window under the pointer
 *   whatever has the key focus, so 2 is 0 here); crossing between a tiled and a floating window
 *   still focuses with [floatSwitch] 1 or 2, onto any floating window with 2.
 * - 3: the pointer never moves the focus. A click still focuses: WindowManager focuses a
 *   tapped window, which the shell cannot stop.
 *
 * [threshold] is `follow_mouse_threshold`: how far (logical pixels) the pointer has to move
 * after a focus change before hovering focuses another window. [shrink] is
 * `follow_mouse_shrink`: the pointer has to be that many pixels inside a window.
 */
data class FollowMouseConfig(
    val mode: Int = 1,
    val threshold: Float = 0f,
    val shrink: Int = 0,
    val floatSwitch: Int = 1,
) {
    companion object {
        /** From the shell's configuration ([value] gives a wire value or null). */
        fun read(value: (String) -> String?): FollowMouseConfig {
            fun int(name: String, def: Int, range: IntRange) =
                value(name)?.trim()?.toIntOrNull()?.takeIf { it in range } ?: def
            return FollowMouseConfig(
                mode = int("follow_mouse", 1, 0..3),
                threshold = value("follow_mouse_threshold")?.trim()?.toFloatOrNull()
                    ?.takeIf { it.isFinite() && it >= 0f } ?: 0f,
                shrink = int("follow_mouse_shrink", 0, 0..10_000),
                floatSwitch = int("float_switch_override_focus", 1, 0..2),
            )
        }
    }
}

/**
 * A window the pointer may be over: its box and its stacking layer (0 tiled, above that
 * floating, special and pinned windows, as the shell stacks them).
 */
data class HoverWindow(val id: Int, val box: Box, val layer: Int) {
    val floating get() = layer > 0
}

/**
 * Which window hovering focuses, given the windows of the active workspace top first.
 * Pure state: the shell feeds it pointer positions and focus changes and asks [target] after
 * the pointer rested a moment (it debounces).
 *
 * Hovering never changes what shows: focusing a window brings it to the top of its layer, so a
 * window that another window of its layer partly covers is left to a click.
 */
class FollowMouse(var config: FollowMouseConfig = FollowMouseConfig()) {
    private var anchorX = Float.NaN
    private var anchorY = Float.NaN
    private var requested: Int? = null
    private var requestedAt = 0L

    /** The pointer moved to ([x], [y]); the first move after a focus change sets the anchor. */
    fun moved(x: Float, y: Float) {
        if (anchorX.isNaN()) {
            anchorX = x
            anchorY = y
        }
    }

    /** The focus changed (by anything): [config]'s threshold counts from the next move. */
    fun focusChanged() {
        anchorX = Float.NaN
        anchorY = Float.NaN
        requested = null
    }

    /**
     * The window to focus with the pointer at ([x], [y]), or null. [windows] are top first,
     * [focused] is the focused window (null for none); [now] in milliseconds. A window returned
     * is not returned again for [RETRY_MS] while the focus has not reached it.
     */
    fun target(windows: List<HoverWindow>, x: Float, y: Float, focused: Int?, now: Long): Int? {
        val c = config
        val i = windows.indexOfFirst { it.box.contains(x, y) }
        if (i < 0) return null
        val hit = windows[i]
        if (hit.id == focused) {
            requested = null
            return null
        }
        if (c.shrink > 0 && !hit.box.inset(Insets(c.shrink, c.shrink, c.shrink, c.shrink)).contains(x, y)) {
            return null
        }
        val current = windows.firstOrNull { it.id == focused }
        val allowed = when (c.mode) {
            1 -> true
            0, 2 -> current != null && ((hit.floating && c.floatSwitch == 2) ||
                (hit.floating != current.floating && c.floatSwitch != 0))
            else -> false
        }
        if (!allowed) return null
        if (windows.subList(0, i).any { it.layer == hit.layer && it.box.overlaps(hit.box) }) return null
        if (c.threshold > 0f && !anchorX.isNaN() && hypot(x - anchorX, y - anchorY) < c.threshold) {
            return null
        }
        if (hit.id == requested && now - requestedAt < RETRY_MS) return null
        requested = hit.id
        requestedAt = now
        return hit.id
    }

    companion object {
        /** How long a focus request may take before hovering asks again. */
        const val RETRY_MS = 500L
    }
}

private fun Box.contains(x: Float, y: Float) = x >= left && x < right && y >= top && y < bottom

private fun Box.overlaps(o: Box) = left < o.right && o.left < right && top < o.bottom && o.top < bottom
