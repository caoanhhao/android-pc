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
    /** mfact changed by [resize]; null follows the configuration. */
    private var factor: Float? = null
    private var lastWidth = 0

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
        if (factor == null) factor0 = cfg.masterFactor
        val out = LinkedHashMap<Int, Box>()
        if (order.isEmpty()) return out
        val masters = order.take(masterCount)
        val stack = order.drop(masterCount)
        lastWidth = area.width
        val split = if (stack.isEmpty()) area.right
                    else area.left + (area.width * (factor ?: cfg.masterFactor)).roundToInt()
        column(masters, Box(area.left, area.top, split, area.bottom), area, cfg, out)
        column(stack, Box(split, area.top, area.right, area.bottom), area, cfg, out)
        return out
    }

    /** Masters grow to the right, stack windows to the left; rows keep equal heights. */
    override fun resize(id: Int, dx: Int, dy: Int) {
        val i = order.indexOf(id)
        if (i < 0 || dx == 0 || lastWidth <= 0 || order.size <= masterCount) return
        val d = dx.toFloat() / lastWidth
        val f = factor ?: factor0
        factor = (if (i < masterCount) f + d else f - d).coerceIn(0.05f, 0.95f)
    }

    private var factor0 = 0.55f

    private fun column(ids: List<Int>, box: Box, area: Box, cfg: TileConfig, out: MutableMap<Int, Box>) {
        ids.forEachIndexed { i, id ->
            val top = box.top + (box.height.toLong() * i / ids.size).toInt()
            val bottom = box.top + (box.height.toLong() * (i + 1) / ids.size).toInt()
            out[id] = applyGaps(Box(box.left, top, box.right, bottom), area, cfg)
        }
    }
}
