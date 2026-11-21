package com.android.hyprtile

import kotlin.math.roundToInt

/**
 * Hyprland's master layout, orientation left: one master on the left takes mfact of the
 * width, the other windows stack on the right in equal rows. With only masters they share the
 * whole area. Closing the master promotes the top of the stack.
 */
class MasterLayout(private val masterCount: Int = 1) : TileLayout {
    private val order = mutableListOf<Int>()
    private var newIsMaster = false
    private var newOnTop = false

    override val ids: List<Int> get() = order.toList()

    override fun contains(id: Int) = id in order

    override fun add(id: Int, target: Int?) {
        require(id !in order) { "window $id is already tiled" }
        when {
            newIsMaster -> order.add(0, id)
            newOnTop -> order.add(minOf(masterCount, order.size), id)
            else -> order.add(id)
        }
    }

    override fun remove(id: Int) = order.remove(id)

    override fun swap(a: Int, b: Int) {
        val ia = order.indexOf(a)
        val ib = order.indexOf(b)
        require(ia >= 0 && ib >= 0) { "window $a or $b is not tiled" }
        order[ia] = b
        order[ib] = a
    }

    override fun configure(cfg: TileConfig) {
        newIsMaster = cfg.newIsMaster
        newOnTop = cfg.newOnTop
    }

    override fun arrange(area: Box, cfg: TileConfig): Map<Int, Box> {
        configure(cfg)
        val out = LinkedHashMap<Int, Box>()
        if (order.isEmpty()) return out
        val masters = order.take(masterCount)
        val stack = order.drop(masterCount)
        val split = if (stack.isEmpty()) area.right
                    else area.left + (area.width * cfg.masterFactor).roundToInt()
        column(masters, Box(area.left, area.top, split, area.bottom), area, cfg, out)
        column(stack, Box(split, area.top, area.right, area.bottom), area, cfg, out)
        return out
    }

    private fun column(ids: List<Int>, box: Box, area: Box, cfg: TileConfig, out: MutableMap<Int, Box>) {
        ids.forEachIndexed { i, id ->
            val top = box.top + (box.height.toLong() * i / ids.size).toInt()
            val bottom = box.top + (box.height.toLong() * (i + 1) / ids.size).toInt()
            out[id] = applyGaps(Box(box.left, top, box.right, bottom), area, cfg)
        }
    }
}
