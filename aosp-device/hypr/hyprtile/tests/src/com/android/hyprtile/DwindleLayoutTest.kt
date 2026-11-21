package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DwindleLayoutTest {
    private val wide = Box(0, 0, 2560, 1080)
    private val fhd = Box(0, 0, 1920, 1080)
    private val cfg = TileConfig(gapsIn = 5, gapsOut = 12)

    private fun layout(vararg ids: Int) = DwindleLayout().apply { ids.forEach { add(it) } }

    @Test
    fun oneWindowFillsTheAreaMinusOuterGaps() {
        assertEquals(mapOf(1 to Box(12, 12, 2548, 1068)), layout(1).arrange(wide, cfg))
    }

    @Test
    fun emptyLayoutArrangesNothing() {
        assertTrue(DwindleLayout().arrange(wide, cfg).isEmpty())
    }

    @Test
    fun twoWindowsSplitSideBySideWithInnerGaps() {
        val boxes = layout(1, 2).arrange(wide, cfg)
        assertEquals(Box(12, 12, 1275, 1068), boxes[1])
        assertEquals(Box(1285, 12, 2548, 1068), boxes[2])
        assertEquals(2 * cfg.gapsIn, boxes.getValue(2).left - boxes.getValue(1).right)
    }

    @Test
    fun splitDirectionFollowsTheBoxShape() {
        // 2560 wide: the right half (1280x1080) is still wider than tall, so it splits again
        // side by side; on 1920x1080 the right half (960x1080) splits top/bottom.
        val ultra = layout(1, 2, 3).arrange(wide, cfg)
        assertEquals(Box(1285, 12, 1915, 1068), ultra[2])
        assertEquals(Box(1925, 12, 2548, 1068), ultra[3])

        val boxes = layout(1, 2, 3).arrange(fhd, cfg)
        assertEquals(Box(12, 12, 955, 1068), boxes[1])
        assertEquals(Box(965, 12, 1908, 535), boxes[2])
        assertEquals(Box(965, 545, 1908, 1068), boxes[3])
    }

    @Test
    fun fourthWindowOnUltrawideStacksTopBottom() {
        val boxes = layout(1, 2, 3, 4).arrange(wide, cfg)
        assertEquals(Box(1925, 12, 2548, 535), boxes[3])
        assertEquals(Box(1925, 545, 2548, 1068), boxes[4])
    }

    @Test
    fun splitWidthMultiplierBiasesTowardsTopBottom() {
        val boxes = layout(1, 2).arrange(wide, cfg.copy(splitWidthMultiplier = 2.5f))
        assertEquals(Box(12, 12, 2548, 535), boxes[1])
        assertEquals(Box(12, 545, 2548, 1068), boxes[2])
    }

    @Test
    fun newWindowSplitsTheTarget() {
        val l = layout(1, 2)
        l.add(3, target = 1)
        val boxes = l.arrange(wide, cfg)
        assertEquals(listOf(1, 3, 2), l.ids)
        assertEquals(Box(12, 12, 635, 1068), boxes[1])
        assertEquals(Box(645, 12, 1275, 1068), boxes[3])
        assertEquals(Box(1285, 12, 2548, 1068), boxes[2])
    }

    @Test
    fun unknownTargetFallsBackToNewestWindow() {
        val l = layout(1, 2)
        l.add(3, target = 99)
        assertEquals(listOf(1, 2, 3), l.ids)
    }

    @Test
    fun forceSplitOnePutsTheNewWindowFirst() {
        val l = DwindleLayout()
        l.configure(cfg.copy(forceSplit = 1))
        l.add(1); l.add(2)
        assertEquals(listOf(2, 1), l.ids)
        assertEquals(Box(12, 12, 1275, 1068), l.arrange(wide, cfg.copy(forceSplit = 1))[2])
    }

    @Test
    fun closingGivesTheBoxToTheSibling() {
        val l = layout(1, 2, 3)
        assertTrue(l.remove(2))
        val boxes = l.arrange(wide, cfg)
        assertEquals(Box(12, 12, 1275, 1068), boxes[1])
        assertEquals(Box(1285, 12, 2548, 1068), boxes[3])
        assertFalse(l.remove(2))
    }

    @Test
    fun closingTheFirstWindowPromotesTheSubtree() {
        val l = layout(1, 2, 3)
        l.remove(1)
        val boxes = l.arrange(wide, cfg)
        assertEquals(Box(12, 12, 1275, 1068), boxes[2])
        assertEquals(Box(1285, 12, 2548, 1068), boxes[3])
        l.remove(2); l.remove(3)
        assertTrue(l.arrange(wide, cfg).isEmpty())
        l.add(4)
        assertEquals(mapOf(4 to Box(12, 12, 2548, 1068)), l.arrange(wide, cfg))
    }

    @Test
    fun reservedBarSpaceIsLeftFree() {
        val tile = HyprTile(cfg.copy(reserved = Insets(top = 40)))
        tile.open(1, 0)
        assertEquals(Box(12, 52, 2548, 1068), tile.arrange(0, wide)[1])
    }

    @Test
    fun splitRatio() {
        val l = DwindleLayout()
        l.configure(cfg.copy(defaultSplitRatio = 1.5f))
        l.add(1); l.add(2)
        val boxes = l.arrange(wide, cfg)
        assertEquals(1920 - 5, boxes.getValue(1).right)
        l.setRatio(1, 0.5f)
        assertEquals(640 - 5, l.arrange(wide, cfg).getValue(1).right)
    }

    @Test
    fun preserveSplitKeepsDirectionWhenTheBoxChangesShape() {
        val keep = cfg.copy(preserveSplit = true)
        val l = layout(1, 2, 3, 4)
        l.arrange(wide, keep) // 3|4 is the top/bottom split of a 640x1080 box
        l.remove(1)           // now 2 | (3/4): the 3/4 box is 1280x1080 but keeps top/bottom
        val boxes = l.arrange(wide, keep)
        assertEquals(Box(1285, 12, 2548, 535), boxes[3])
        val reshaped = layout(1, 2, 3, 4).apply { arrange(wide, cfg); remove(1) }.arrange(wide, cfg)
        assertEquals(Box(1285, 12, 1915, 1068), reshaped[3])
    }

    @Test
    fun toggleSplitFlipsTheDirection() {
        val l = layout(1, 2)
        l.arrange(wide, cfg)
        l.toggleSplit(2)
        val boxes = l.arrange(wide, cfg)
        assertEquals(Box(12, 12, 2548, 535), boxes[1])
        assertEquals(Box(12, 545, 2548, 1068), boxes[2])
    }

    @Test
    fun swapExchangesPlaces() {
        val l = layout(1, 2, 3)
        val before = l.arrange(wide, cfg)
        l.swap(1, 3)
        val after = l.arrange(wide, cfg)
        assertEquals(before[1], after[3])
        assertEquals(before[3], after[1])
        assertEquals(before[2], after[2])
        l.remove(1)
        assertEquals(listOf(3, 2), l.ids)
    }

    @Test
    fun randomOpsNeverOverlapAndStayInside() {
        val rnd = java.util.Random(42)
        val l = DwindleLayout()
        val open = mutableListOf<Int>()
        var next = 1
        repeat(2000) {
            if (open.isEmpty() || (open.size < 12 && rnd.nextInt(3) != 0)) {
                val target = if (open.isEmpty()) null else open[rnd.nextInt(open.size)]
                l.add(next, target); open += next++
            } else {
                l.remove(open.removeAt(rnd.nextInt(open.size)))
            }
            val boxes = l.arrange(wide, cfg)
            assertEquals(open.toSet(), boxes.keys)
            checkTiling(boxes.values.toList(), wide, cfg)
        }
    }

    companion object {
        /** Boxes lie inside the area minus gaps_out, do not overlap, and cover it except gaps. */
        fun checkTiling(boxes: List<Box>, area: Box, cfg: TileConfig) {
            for (b in boxes) {
                assertTrue("$b inside $area", b.left >= area.left + cfg.gapsOut &&
                    b.top >= area.top + cfg.gapsOut && b.right <= area.right - cfg.gapsOut &&
                    b.bottom <= area.bottom - cfg.gapsOut)
                assertTrue("$b not empty", b.width > 0 && b.height > 0)
            }
            for (i in boxes.indices) for (j in i + 1 until boxes.size) {
                val a = boxes[i]; val b = boxes[j]
                val overlap = a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
                assertFalse("$a overlaps $b", overlap)
            }
        }
    }
}
