package com.android.hyprtile

import kotlin.math.roundToInt

/**
 * Hyprland's dwindle layout: a binary tree whose leaves are windows. A new window splits the
 * box of the target window in two; the split runs along the box's longer side (top/bottom when
 * height * split_width_multiplier > width), and is recomputed from the box's shape on every
 * arrange unless preserve_split is set. Closing a window gives its box to its sibling.
 */
class DwindleLayout : TileLayout {
    private sealed class Node {
        var parent: Split? = null
    }

    private class Leaf(var id: Int) : Node()

    private class Split(var first: Node, var second: Node, var ratio: Float) : Node() {
        /** Direction from the last arrange (or fixed, with preserve_split); null before. */
        var splitTop: Boolean? = null
        /** Set by [toggleSplit]: the direction no longer follows the box's shape. */
        var pinned = false
        /** The box from the last arrange, for [resize]. */
        var box: Box? = null

        fun replace(old: Node, new: Node) {
            if (first === old) first = new else second = new
            new.parent = this
        }
    }

    private var root: Node? = null
    private val leaves = LinkedHashMap<Int, Leaf>()
    private var pendingRatio = 1f
    private var pendingForceSplit = 2

    override val ids: List<Int>
        get() = mutableListOf<Int>().also { collect(root, it) }

    private fun collect(n: Node?, out: MutableList<Int>) {
        when (n) {
            is Leaf -> out += n.id
            is Split -> { collect(n.first, out); collect(n.second, out) }
            null -> {}
        }
    }

    override fun contains(id: Int) = id in leaves

    override fun add(id: Int, target: Int?) {
        require(id !in leaves) { "window $id is already tiled" }
        val leaf = Leaf(id)
        val on = (target?.let { leaves[it] }) ?: leaves.values.lastOrNull()
        leaves[id] = leaf
        if (on == null) {
            root = leaf
            return
        }
        val parent = on.parent
        val split = if (pendingForceSplit == 1) Split(leaf, on, pendingRatio)
                    else Split(on, leaf, pendingRatio)
        if (parent == null) root = split else parent.replace(on, split)
        split.parent = parent
        leaf.parent = split
        on.parent = split
    }

    override fun remove(id: Int): Boolean {
        val leaf = leaves.remove(id) ?: return false
        val parent = leaf.parent
        if (parent == null) {
            root = null
            return true
        }
        val sibling = if (parent.first === leaf) parent.second else parent.first
        val grand = parent.parent
        if (grand == null) {
            root = sibling
            sibling.parent = null
        } else {
            grand.replace(parent, sibling)
        }
        return true
    }

    override fun swap(a: Int, b: Int) {
        val la = requireNotNull(leaves[a]) { "window $a is not tiled" }
        val lb = requireNotNull(leaves[b]) { "window $b is not tiled" }
        la.id = b
        lb.id = a
        leaves[a] = lb
        leaves[b] = la
    }

    /** Flips the direction of the split that holds [id] (Hyprland's `togglesplit`). */
    fun toggleSplit(id: Int) {
        val split = leaves[id]?.parent ?: return
        split.splitTop = !(split.splitTop ?: false)
        split.pinned = true
    }

    /** Sets the ratio of the split that holds [id] (0.1..1.9, Hyprland's `splitratio exact`). */
    fun setRatio(id: Int, ratio: Float) {
        leaves[id]?.parent?.ratio = ratio.coerceIn(0.1f, 1.9f)
    }

    override fun resize(id: Int, dx: Int, dy: Int) {
        val leaf = leaves[id] ?: return
        if (dx != 0) grow(leaf, dx, top = false)
        if (dy != 0) grow(leaf, dy, top = true)
    }

    override fun resizeEdges(id: Int, left: Int, top: Int, right: Int, bottom: Int) {
        val leaf = leaves[id] ?: return
        if (left != 0) moveLine(leaf, left, top = false, after = false)
        if (right != 0) moveLine(leaf, right, top = false, after = true)
        if (top != 0) moveLine(leaf, top, top = true, after = false)
        if (bottom != 0) moveLine(leaf, bottom, top = true, after = true)
    }

    /**
     * Moves by [by] pixels the split line on one side of [leaf]: the nearest split of that
     * direction above it with the leaf's subtree before the line ([after], the right or bottom
     * edge) or after it (the left or top edge).
     */
    private fun moveLine(leaf: Leaf, by: Int, top: Boolean, after: Boolean) {
        var child: Node = leaf
        var split = leaf.parent
        while (split != null &&
            (split.splitTop != top || (split.first === child) != after)) {
            child = split
            split = split.parent
        }
        split ?: return
        val box = split.box ?: return
        val size = if (top) box.height else box.width
        if (size <= 0) return
        split.ratio = (split.ratio + 2f * by / size).coerceIn(0.1f, 1.9f)
    }

    /** Moves the nearest split of that direction above [leaf] so that it grows by [by]. */
    private fun grow(leaf: Leaf, by: Int, top: Boolean) {
        var child: Node = leaf
        var split = leaf.parent
        while (split != null && split.splitTop != top) {
            child = split
            split = split.parent
        }
        split ?: return
        val box = split.box ?: return
        val size = if (top) box.height else box.width
        if (size <= 0) return
        val delta = 2f * by / size
        split.ratio = (if (split.first === child) split.ratio + delta else split.ratio - delta)
            .coerceIn(0.1f, 1.9f)
    }

    override fun configure(cfg: TileConfig) {
        pendingRatio = cfg.defaultSplitRatio
        pendingForceSplit = cfg.forceSplit
    }

    override fun arrange(area: Box, cfg: TileConfig): Map<Int, Box> {
        configure(cfg)
        val out = LinkedHashMap<Int, Box>()
        root?.let { place(it, area, area, cfg, out) }
        return out
    }

    private fun place(n: Node, box: Box, area: Box, cfg: TileConfig, out: MutableMap<Int, Box>) {
        when (n) {
            is Leaf -> out[n.id] = applyGaps(box, area, cfg)
            is Split -> {
                val shapeTop = box.height * cfg.splitWidthMultiplier > box.width
                val top = when {
                    n.pinned -> n.splitTop ?: shapeTop
                    cfg.preserveSplit && n.splitTop != null -> n.splitTop!!
                    else -> shapeTop
                }
                n.splitTop = top
                n.box = box
                if (top) {
                    val y = box.top + (box.height * n.ratio / 2f).roundToInt()
                    place(n.first, Box(box.left, box.top, box.right, y), area, cfg, out)
                    place(n.second, Box(box.left, y, box.right, box.bottom), area, cfg, out)
                } else {
                    val x = box.left + (box.width * n.ratio / 2f).roundToInt()
                    place(n.first, Box(box.left, box.top, x, box.bottom), area, cfg, out)
                    place(n.second, Box(x, box.top, box.right, box.bottom), area, cfg, out)
                }
            }
        }
    }
}
