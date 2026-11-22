package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PinTest {
    @Test
    fun parsesPinArguments() {
        assertEquals(Switch.TOGGLE to null, parsePin(""))
        assertEquals(Switch.ON to null, parsePin("on"))
        assertEquals(Switch.OFF to null, parsePin(" disable "))
        assertEquals(Switch.TOGGLE to 42, parsePin("taskid:42"))
        assertEquals(Switch.ON to 42, parsePin("on taskid:42"))
        assertEquals(Switch.OFF to 7, parsePin("address:taskid:7, off"))
        assertEquals(Switch.TOGGLE to null, parsePin("activewindow"))
        assertNull(parsePin("sideways"))
        assertNull(parsePin("42"))
        assertNull(parsePin("taskid:1 taskid:2"))
        assertNull(parsePin("on off"))
    }

    @Test
    fun switchesApply() {
        assertTrue(Switch.TOGGLE.apply(false))
        assertFalse(Switch.TOGGLE.apply(true))
        assertTrue(Switch.ON.apply(true))
        assertFalse(Switch.OFF.apply(true))
        assertEquals(Switch.ON, Switch.parse("1"))
        assertNull(Switch.parse("maybe"))
    }

    @Test
    fun parsesWindows() {
        assertEquals(5, parseWindow("taskid:5").getOrNull())
        assertNull(parseWindow("").getOrThrow())
        assertTrue(parseWindow("class:firefox").isFailure)
        assertTrue(parseWindow("5").isFailure)
    }

    @Test
    fun parsesMoveAndResizeArguments() {
        assertEquals(Triple(false, Coord(40), Coord(-10)), parseXY("40 -10"))
        assertEquals(Triple(true, Coord(100, true), Coord(100, true)), parseXY("exact 100% 100%"))
        assertEquals(Triple(true, Coord(12), Coord(39)), parseXY("exact 12 39.0"))
        assertNull(parseXY("50% 10"))
        assertNull(parseXY("exact 10"))
        assertNull(parseXY("exact -5% 10"))
        assertEquals(2514, Coord(100, true).resolve(2514))
        assertEquals(12 + 1257, Coord(50, true).resolve(2514, 12))
        assertEquals(700, Coord(700).resolve(2514, 12))
    }

    @Test
    fun pinningFloatsATiledWindow() {
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0)
        assertEquals(0, t.setPinned(2, true))
        assertTrue(t.isPinned(2))
        assertTrue(t.isFloating(2))
        assertEquals(listOf(1), t.tiled(0))
        assertEquals(setOf(2), t.pinned())
        // Unpinned, it stays floating.
        assertNull(t.setPinned(2, false))
        assertFalse(t.isPinned(2))
        assertTrue(t.isFloating(2))
    }

    @Test
    fun pinnedWindowsStayPinnedAcrossWorkspacesUntilTiledOrClosed() {
        val t = HyprTile()
        t.open(1, 0)
        t.open(2, 0, float = true)
        t.setPinned(2, true)
        // Moved to another workspace as a floating window: still pinned.
        t.open(2, 1, float = true)
        assertTrue(t.isPinned(2))
        // Tiled again: no longer pinned.
        assertFalse(t.toggleFloating(2, 0, focused = 1))
        assertFalse(t.isPinned(2))
        assertEquals(listOf(1, 2), t.tiled(0))
        t.setPinned(1, true)
        t.close(1)
        assertTrue(t.pinned().isEmpty())
    }
}
