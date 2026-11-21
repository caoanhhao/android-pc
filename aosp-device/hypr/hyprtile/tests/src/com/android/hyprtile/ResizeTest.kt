package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Test

class ResizeTest {
    private val cfg = TileConfig()
    private val wide = Box(0, 0, 2560, 1080)

    @Test
    fun dwindleMovesTheSplitNextToTheWindow() {
        val l = DwindleLayout().apply { add(1); add(2) }
        l.arrange(wide, cfg)
        // Split at x = 1280; window 1 (left) grows 256 px to the right.
        l.resize(1, 256, 0)
        val b = l.arrange(wide, cfg)
        assertEquals(Box(12, 12, 1531, 1068), b[1])
        assertEquals(Box(1541, 12, 2548, 1068), b[2])
        // Window 2 (right child) grows by moving the same split left.
        l.resize(2, 512, 0)
        assertEquals(Box(1029, 12, 2548, 1068), l.arrange(wide, cfg)[2])
    }

    @Test
    fun dwindleUsesTheNearestSplitOfThatDirection() {
        // On 2560x1080 the fourth window stacks under the third (DwindleLayoutTest).
        val l = DwindleLayout().apply { add(1); add(2); add(3); add(4) }
        val before = l.arrange(wide, cfg)
        // Growing 4 taller moves the top/bottom split it shares with 3.
        l.resize(4, 0, 108)
        val after = l.arrange(wide, cfg)
        assertEquals(before[4]!!.top - 108, after[4]!!.top)
        assertEquals(before[3]!!.bottom - 108, after[3]!!.bottom)
        assertEquals(before[1], after[1])
        // Growing it wider climbs to the side-by-side split between 2 and the 3/4 column.
        l.resize(4, 128, 0)
        val wider = l.arrange(wide, cfg)
        assertEquals(before[2]!!.right - 128, wider[2]!!.right)
        assertEquals(before[1], wider[1])
    }

    @Test
    fun dwindleClampsRatio() {
        val l = DwindleLayout().apply { add(1); add(2) }
        l.arrange(wide, cfg)
        l.resize(1, 100_000, 0)
        // ratio 1.9: the split sits at 95 % of the width.
        assertEquals(2432 - 5, l.arrange(wide, cfg)[1]!!.right)
        l.resize(1, 0, 50) // no top/bottom split: nothing happens
        assertEquals(2432 - 5, l.arrange(wide, cfg)[1]!!.right)
    }

    @Test
    fun masterChangesMfact() {
        val l = MasterLayout().apply { add(1); add(2); add(3) }
        l.arrange(wide, cfg)
        l.resize(1, 256, 0) // mfact 0.55 -> 0.65
        assertEquals(1664 - 5, l.arrange(wide, cfg)[1]!!.right)
        l.resize(2, 512, 0) // a stack window grows left: mfact 0.45
        assertEquals(1152 - 5, l.arrange(wide, cfg)[1]!!.right)
        val tile = HyprTile().apply { open(7, 1); open(8, 1) }
        tile.arrange(1, wide)
        tile.resize(7, 256, 0)
        assertEquals(1536 - 5, tile.arrange(1, wide)[7]!!.right)
    }
}
