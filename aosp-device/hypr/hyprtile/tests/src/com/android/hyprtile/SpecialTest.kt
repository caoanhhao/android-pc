package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SpecialTest {
    @Test
    fun names() {
        assertEquals("special", specialName("special"))
        assertEquals("special", specialName("special:"))
        assertEquals("term", specialName("special:term"))
        assertEquals("term", specialName(" SPECIAL:term "))
        assertNull(specialName("3"))
        assertNull(specialName("e+1"))
        assertNull(specialName("specials"))
    }

    @Test
    fun keysAreNegativeAndStable() {
        val s = SpecialWorkspaces()
        assertEquals(-99, s.keyOf("special"))
        assertEquals(-100, s.keyOf("term"))
        assertEquals(-99, s.keyOf("special"))
        assertEquals("term", s.nameOf(-100))
        assertTrue(s.isSpecialKey(-99))
        assertFalse(s.isSpecialKey(5))
    }

    @Test
    fun toggleShowsAndHidesOnePerDisplay() {
        val s = SpecialWorkspaces()
        assertEquals("special", s.toggle(0, "special"))
        assertEquals("special", s.shownOn(0))
        // Another name replaces the one shown.
        assertEquals("term", s.toggle(0, "term"))
        assertEquals("term", s.shownOn(0))
        assertNull(s.toggle(0, "term"))
        assertNull(s.shownOn(0))
        // A special workspace shows on one display at a time.
        s.show(0, "special")
        s.show(1, "special")
        assertNull(s.shownOn(0))
        assertEquals("special", s.shownOn(1))
        assertEquals(setOf(1), s.shownDisplays())
    }

    @Test
    fun membership() {
        val s = SpecialWorkspaces()
        assertNull(s.add(7, "special"))
        s.add(8, "special")
        s.add(9, "term")
        assertEquals(listOf(7, 8), s.windows("special"))
        assertEquals(listOf("special", "term"), s.occupied())
        s.show(0, "special")
        assertTrue(s.isShown(7, 0))
        assertFalse(s.isShown(9, 0))
        assertEquals("term", s.add(9, "special"))
        assertEquals(listOf("special"), s.occupied())
        s.retain { it != 8 }
        assertEquals(listOf(7, 9), s.windows("special"))
        assertEquals("special", s.remove(7))
        assertFalse(s.isMember(7))
    }

    @Test
    fun moveTargetTogglesBack() {
        val s = SpecialWorkspaces()
        assertEquals("special", s.moveTarget(1, "special"))
        s.add(1, "special")
        assertNull(s.moveTarget(1, "special"))
        assertEquals("term", s.moveTarget(1, "term"))
    }

    @Test
    fun encodeRoundTrip() {
        val s = SpecialWorkspaces()
        s.add(3, "special")
        s.add(4, "term")
        s.add(5, "term")
        s.keyOf("empty")
        val d = SpecialWorkspaces.decode(s.encode())
        assertEquals(s.encode(), d.encode())
        assertEquals(listOf(4, 5), d.windows("term"))
        assertEquals(-100, d.keyOf("term"))
        assertEquals(-102, d.keyOf("new"))
        assertEquals(0, SpecialWorkspaces.decode("garbage;x=1:2").allWindows().size)
    }

    @Test
    fun specialWorkspaceTilesInItsOwnLayoutAndArea() {
        val t = HyprTile(TileConfig(gapsIn = 0, gapsOut = 0, specialScaleFactor = 0.5f))
        val key = SpecialWorkspaces().keyOf("special")
        t.open(1, 10)
        t.open(2, key)
        val display = Box(0, 0, 1000, 800)
        assertEquals(mapOf(1 to display), t.arrange(10, display))
        assertEquals(mapOf(2 to Box(250, 200, 750, 600)), t.arrange(key, display))
        assertEquals(key, t.workspaceOf(2))
    }

    @Test
    fun specialRules() {
        val w = WindowRule(matchClass = "termux", special = "term", silent = true)
        val back = WindowRule.decode(w.encode())
        assertEquals("term", back.special)
        assertNull(back.workspace)
        assertTrue(back.silent)
        assertEquals("special", WindowRule.decode(WindowRule(special = "special").encode()).special)
        val r = WorkspaceRule(0, gapsOut = 40, special = "term")
        assertEquals(r, WorkspaceRule.decode(r.encode()))
        assertEquals(3, WorkspaceRule.decode(WorkspaceRule(3).encode()).workspace)
    }
}
