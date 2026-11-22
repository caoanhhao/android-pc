package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HistoryTest {
    @Test
    fun focusHistoryIsMostRecentFirstPerDisplay() {
        val h = FocusHistory()
        h.focused(0, 1)
        h.focused(0, 2)
        h.focused(1, 7)
        h.focused(0, 3)
        h.focused(0, 1)
        assertEquals(listOf(1, 3, 2), h.of(0))
        assertEquals(listOf(7), h.of(1))
        h.opened(0, 9)
        h.opened(0, 3)
        assertEquals(listOf(1, 3, 2, 9), h.of(0))
        h.remove(3)
        h.retain { it != 9 }
        assertEquals(listOf(1, 2), h.of(0))
    }

    @Test
    fun focusCurrentOrLast() {
        val h = FocusHistory()
        assertNull(h.currentOrLast(0, null))
        h.focused(0, 1)
        assertNull(h.currentOrLast(0, 1))
        h.focused(0, 2)
        // ALT + Tab toggles between the two most recent windows.
        assertEquals(1, h.currentOrLast(0, 2))
        h.focused(0, 1)
        assertEquals(2, h.currentOrLast(0, 1))
        // Nothing focused (the desktop): back to the current one.
        assertEquals(1, h.currentOrLast(0, null))
        // A window that cannot be focused is skipped.
        h.focused(0, 3)
        assertEquals(2, h.currentOrLast(0, 3) { it != 1 })
    }

    @Test
    fun windowMovedToAnotherDisplayKeepsItsPlace() {
        val h = FocusHistory()
        h.focused(0, 1)
        h.focused(0, 2)
        h.focused(1, 5)
        h.moveToDisplay(2, 1)
        assertEquals(listOf(1), h.of(0))
        assertEquals(listOf(2, 5), h.of(1))
        h.focused(0, 3)
        h.moveToDisplay(1, 1)
        assertEquals(listOf(2, 5, 1), h.of(1))
        h.moveToDisplay(3, 1)
        assertEquals(listOf(3, 2, 5, 1), h.of(1))
    }

    @Test
    fun workspacePreviousGoesBackAndForth() {
        val w = WorkspaceHistory()
        val one = WorkspaceHistory.Entry(0, 1)
        val two = WorkspaceHistory.Entry(0, 2)
        val three = WorkspaceHistory.Entry(0, 3)
        w.track(0, 1)
        w.track(0, 2)
        w.track(0, 3)
        assertEquals(two, w.previous(three))
        // Going there puts it in front: previous toggles between the two last ones.
        w.track(0, 2)
        assertEquals(three, w.previous(two))
        w.track(0, 3)
        assertEquals(two, w.previous(three))
        assertNull(w.previous(one))
        // An unknown current workspace: the latest one.
        assertEquals(three, w.previous(WorkspaceHistory.Entry(0, 9)))
    }

    @Test
    fun workspacePreviousPerMonitor() {
        val w = WorkspaceHistory()
        w.track(0, 1)
        w.track(1, 1)
        w.track(0, 2)
        val cur = WorkspaceHistory.Entry(0, 2)
        assertEquals(WorkspaceHistory.Entry(1, 1), w.previous(cur))
        assertEquals(WorkspaceHistory.Entry(0, 1), w.previousOnDisplay(cur))
        w.retain { it.display == 0 }
        assertEquals(WorkspaceHistory.Entry(0, 1), w.previous(cur))
    }

    @Test
    fun cycleWrapsAroundAndFilters() {
        val order = listOf(1, 2, 3, 4)
        assertEquals(3, cycle(order, 2, next = true) { true })
        assertEquals(1, cycle(order, 4, next = true) { true })
        assertEquals(4, cycle(order, 1, next = false) { true })
        assertEquals(4, cycle(order, 2, next = true) { it % 2 == 0 })
        assertEquals(2, cycle(order, 4, next = true) { it % 2 == 0 })
        assertNull(cycle(listOf(1), 1, next = true) { true })
        assertEquals(1, cycle(order, null, next = true) { true })
        assertEquals(4, cycle(order, null, next = false) { true })
    }

    @Test
    fun parsesCycleOptions() {
        assertEquals(CycleOptions(), CycleOptions.parse(""))
        assertEquals(CycleOptions(next = false, tiled = true), CycleOptions.parse("prev tiled"))
        assertEquals(CycleOptions(floating = true, hist = true), CycleOptions.parse("floating,hist"))
        assertEquals(CycleOptions(next = false), CycleOptions.parse("b"))
        assertNull(CycleOptions.parse("sideways"))
        assertEquals(false, CycleOptions(tiled = true).floatingOnly)
        assertEquals(true, CycleOptions(floating = true).floatingOnly)
        assertNull(CycleOptions(tiled = true, floating = true).floatingOnly)
        assertEquals("prev tiled hist", CycleOptions(next = false, tiled = true, hist = true).toString())
    }

    @Test
    fun windowSelectors() {
        val term = WindowFacts(11, "com.termux", "Termux", pid = 400, floating = false, onActiveWorkspace = true)
        val web = WindowFacts(12, "org.chromium.chrome", "New tab - Chromium", pid = 500, floating = true,
            focused = true, onActiveWorkspace = true)
        val far = WindowFacts(20, "com.android.settings", "Settings", floating = true)
        val all = listOf(term, web, far)
        assertEquals(term, WindowSelector.parse("class:termux").pick(all))
        assertEquals(term, WindowSelector.parse("initialclass:^com\\.termux$").pick(all))
        assertEquals(web, WindowSelector.parse("title:Chromium").pick(all))
        assertEquals(web, WindowSelector.parse("class:negative:termux").pick(all))
        assertEquals(far, WindowSelector.parse("taskid:20").pick(all))
        assertEquals(far, WindowSelector.parse("20").pick(all))
        assertEquals(term, WindowSelector.parse("address:0xb").pick(all))
        assertEquals(term, WindowSelector.parse("address:taskid:11").pick(all))
        assertEquals(term, WindowSelector.parse("stableid:11").pick(all))
        assertEquals(web, WindowSelector.parse("pid:500").pick(all))
        assertEquals(web, WindowSelector.parse("activewindow").pick(all))
        assertEquals(web, WindowSelector.parse("floating").pick(all))
        assertEquals(term, WindowSelector.parse("tiled").pick(all))
        assertNull(WindowSelector.parse("class:firefox").pick(all))
        for (bad in listOf("", "tag:x", "class:(", "bogus:1", "pid:x", "address:0xzz", "nonsense")) {
            assertTrue(bad, runCatching { WindowSelector.parse(bad) }.isFailure)
        }
    }

    @Test
    fun previousWorkspaceArguments() {
        assertEquals(false, previousWorkspaceArg("previous"))
        assertEquals(false, previousWorkspaceArg("prev"))
        assertEquals(true, previousWorkspaceArg("previous_per_monitor"))
        assertNull(previousWorkspaceArg("3"))
        assertFalse(previousWorkspaceArg("PREVIOUS")!!)
    }
}
