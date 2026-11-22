package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Test

/** Dragging a tiled window's edge moves the split line on that side (its neighbours follow). */
class ResizeEdgesTest {
    private val cfg = TileConfig(gapsIn = 5, gapsOut = 12)
    private val wide = Box(0, 0, 2560, 1080)

    /** 1 | 2 | (3 / 4) on 2560x1080. */
    private fun four() = DwindleLayout().apply {
        add(1); add(2); add(3); add(4)
        arrange(wide, cfg)
    }

    @Test
    fun rightEdgeMovesTheSplitOnItsRight() {
        val l = four()
        l.resizeEdges(1, 0, 0, 100, 0)
        val b = l.arrange(wide, cfg)
        assertEquals(Box(12, 12, 1375, 1068), b[1])
        assertEquals(1385, b.getValue(2).left)
        // The right part is 100 px narrower; its own split keeps its ratio (as in Hyprland).
        assertEquals(Box(1975, 12, 2548, 535), b[3])
        DwindleLayoutTest.checkTiling(b.values.toList(), wide, cfg)
    }

    @Test
    fun leftEdgeMovesTheSplitOnItsLeftNotTheNearestOne() {
        val l = four()
        // Window 3's nearest side-by-side split (2 | 3/4) has it on the right: that line moves.
        l.resizeEdges(3, -100, 0, 0, 0)
        val b = l.arrange(wide, cfg)
        assertEquals(1815, b.getValue(2).right)
        assertEquals(1825, b.getValue(3).left)
        assertEquals(1825, b.getValue(4).left)
        assertEquals(Box(12, 12, 1275, 1068), b[1])
    }

    @Test
    fun bottomEdgeMovesTheTopBottomSplit() {
        val l = four()
        l.resizeEdges(3, 0, 0, 0, 50)
        val b = l.arrange(wide, cfg)
        assertEquals(585, b.getValue(3).bottom)
        assertEquals(595, b.getValue(4).top)
    }

    @Test
    fun edgesOnTheBorderStay() {
        val l = four()
        val before = l.arrange(wide, cfg)
        l.resizeEdges(3, 0, -30, 40, 0) // top and right edges lie on the work area's border
        assertEquals(before, l.arrange(wide, cfg))
    }

    @Test
    fun twoEdgesAtOnce() {
        val l = four()
        l.resizeEdges(4, -100, -50, 0, 0) // corner drag: left and top edges of the bottom-right
        val b = l.arrange(wide, cfg)
        assertEquals(1825, b.getValue(4).left)
        assertEquals(495, b.getValue(4).top)
        assertEquals(485, b.getValue(3).bottom)
        DwindleLayoutTest.checkTiling(b.values.toList(), wide, cfg)
    }

    @Test
    fun masterFallsBackToResize() {
        val l = MasterLayout().apply { add(1); add(2); add(3) }
        l.arrange(wide, cfg)
        l.resizeEdges(2, -256, 0, 0, 0) // the stack's left edge moves left: the master shrinks
        assertEquals(1147, l.arrange(wide, cfg).getValue(1).right) // mfact 0.55 -> 0.45
    }

    @Test
    fun hyprTileRoutesToTheWindowsLayout() {
        val t = HyprTile(cfg)
        t.open(1, 0); t.open(2, 0)
        t.arrange(0, wide)
        t.resizeEdges(1, 0, 0, 100, 0)
        assertEquals(1375, t.arrange(0, wide).getValue(1).right)
    }
}
