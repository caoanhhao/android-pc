package com.android.hyprtile

/** A direction argument of `movefocus` and `movewindow` (`l`, `r`, `u`, `d`). */
enum class Direction {
    LEFT, RIGHT, UP, DOWN;

    companion object {
        /** Hyprland accepts `l`/`left`, `r`/`right`, `u`/`t`/`up`/`top`, `d`/`b`/`down`/`bottom`. */
        fun parse(arg: String): Direction? = when (arg.trim().lowercase()) {
            "l", "left" -> LEFT
            "r", "right" -> RIGHT
            "u", "t", "up", "top" -> UP
            "d", "b", "down", "bottom" -> DOWN
            else -> null
        }
    }
}

/**
 * The box among [boxes] next to [from] in [dir], the way Hyprland picks the target of
 * `movefocus` for tiled windows: the closest box whose facing edge lies beyond [from]'s edge
 * (within [slack] pixels, which covers the gaps) and that overlaps it on the other axis; ties
 * go to the larger overlap. Without an overlapping box it falls back to the nearest centre
 * among the boxes wholly beyond that edge (floating windows). Returns null at the edge of the
 * layout.
 */
fun <K> neighbor(boxes: Map<K, Box>, from: K, dir: Direction, slack: Int = 0): K? {
    val cur = boxes[from] ?: return null
    val others = boxes.filterKeys { it != from }

    fun gap(b: Box) = when (dir) {
        Direction.LEFT -> cur.left - b.right
        Direction.RIGHT -> b.left - cur.right
        Direction.UP -> cur.top - b.bottom
        Direction.DOWN -> b.top - cur.bottom
    }
    fun overlap(b: Box) = when (dir) {
        Direction.LEFT, Direction.RIGHT -> minOf(cur.bottom, b.bottom) - maxOf(cur.top, b.top)
        Direction.UP, Direction.DOWN -> minOf(cur.right, b.right) - maxOf(cur.left, b.left)
    }

    others.filter { (_, b) -> gap(b) >= -slack && overlap(b) > 0 }
        .minWithOrNull(compareBy<Map.Entry<K, Box>> { gap(it.value) }.thenByDescending { overlap(it.value) })
        ?.let { return it.key }

    // Centres, doubled to stay in integers; only boxes that lie wholly beyond the edge.
    val cx = cur.left + cur.right
    val cy = cur.top + cur.bottom
    return others.filter { (_, b) -> gap(b) >= -slack }.minByOrNull { (_, b) ->
        val dx = (b.left + b.right - cx).toLong()
        val dy = (b.top + b.bottom - cy).toLong()
        dx * dx + dy * dy
    }?.key
}
