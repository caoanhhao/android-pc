package com.android.hyprtile

/** An integer rectangle; right and bottom are exclusive, as in android.graphics.Rect. */
data class Box(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    val width: Int get() = right - left
    val height: Int get() = bottom - top

    fun inset(i: Insets): Box = Box(left + i.left, top + i.top, right - i.right, bottom - i.bottom)

    override fun toString() = "[$left,$top][$right,$bottom]"
}

/** Space taken from each side of a box: a bar, a gap. */
data class Insets(val left: Int = 0, val top: Int = 0, val right: Int = 0, val bottom: Int = 0) {
    companion object {
        val NONE = Insets()
    }
}

/**
 * Shrinks a tile [node] of the work [area] into the window's box, as Hyprland does: a side
 * that lies on the work area's edge gets [TileConfig.gapsOut], an inner side gets
 * [TileConfig.gapsIn] (so two neighbours are 2 * gapsIn apart).
 */
internal fun applyGaps(node: Box, area: Box, cfg: TileConfig): Box {
    fun gap(onEdge: Boolean) = if (onEdge) cfg.gapsOut else cfg.gapsIn
    return Box(
        node.left + gap(node.left <= area.left),
        node.top + gap(node.top <= area.top),
        node.right - gap(node.right >= area.right),
        node.bottom - gap(node.bottom >= area.bottom),
    )
}
