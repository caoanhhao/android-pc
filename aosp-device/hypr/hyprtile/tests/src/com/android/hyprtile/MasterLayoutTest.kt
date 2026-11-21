package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Test

class MasterLayoutTest {
    private val wide = Box(0, 0, 2560, 1080)
    private val cfg = TileConfig(layout = LayoutKind.MASTER, gapsIn = 5, gapsOut = 12)

    private fun layout(vararg ids: Int, c: TileConfig = cfg) =
        MasterLayout().apply { configure(c); ids.forEach { add(it) } }

    @Test
    fun lonelyMasterTakesEverything() {
        assertEquals(mapOf(1 to Box(12, 12, 2548, 1068)), layout(1).arrange(wide, cfg))
    }

    @Test
    fun masterOnTheLeftStackOnTheRight() {
        val boxes = layout(1, 2, 3).arrange(wide, cfg)
        assertEquals(Box(12, 12, 1403, 1068), boxes[1]) // mfact 0.55 -> x = 1408
        assertEquals(Box(1413, 12, 2548, 535), boxes[2])
        assertEquals(Box(1413, 545, 2548, 1068), boxes[3])
    }

    @Test
    fun closingTheMasterPromotesTheStack() {
        val l = layout(1, 2, 3)
        l.remove(1)
        assertEquals(listOf(2, 3), l.ids)
        assertEquals(Box(12, 12, 1403, 1068), l.arrange(wide, cfg)[2])
    }

    @Test
    fun newStatusMasterAndNewOnTop() {
        assertEquals(listOf(3, 2, 1), layout(1, 2, 3, c = cfg.copy(newIsMaster = true)).ids)
        assertEquals(listOf(1, 3, 2), layout(1, 2, 3, c = cfg.copy(newOnTop = true)).ids)
    }

    @Test
    fun swapAndRandomOps() {
        val l = layout(1, 2, 3)
        l.swap(1, 3)
        assertEquals(listOf(3, 2, 1), l.ids)
        val rnd = java.util.Random(7)
        val open = mutableListOf(1, 2, 3)
        var next = 4
        repeat(500) {
            if (open.isEmpty() || (open.size < 10 && rnd.nextBoolean())) { l.add(next); open += next++ }
            else l.remove(open.removeAt(rnd.nextInt(open.size)))
            DwindleLayoutTest.checkTiling(l.arrange(wide, cfg).values.toList(), wide, cfg)
        }
    }
}
