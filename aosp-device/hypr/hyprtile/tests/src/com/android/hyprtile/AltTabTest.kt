package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AltTabTest {
    @Test
    fun modeParses() {
        assertEquals(AltTabMode.SWITCHER, AltTabMode.parse("switcher"))
        assertEquals(AltTabMode.CYCLE, AltTabMode.parse(" Cycle "))
        assertEquals(AltTabMode.BIND, AltTabMode.parse("bind"))
        assertEquals(AltTabMode.OVERVIEW, AltTabMode.parse("overview"))
        assertEquals(AltTabMode.OFF, AltTabMode.parse("off"))
        assertEquals("switcher", AltTabMode.SWITCHER.wireName)
        assertTrue(runCatching { AltTabMode.parse("windows") }.exceptionOrNull() is IllegalArgumentException)
    }

    @Test
    fun orderIsHistoryThenUnknownWindows() {
        // 5 closed (still in a stale history), 8 and 9 never focused.
        assertEquals(listOf(3, 1, 2, 8, 9), altTabOrder(listOf(3, 5, 1, 2), listOf(1, 2, 3, 8, 9)))
        assertEquals(listOf(4), altTabOrder(emptyList(), listOf(4)))
        assertEquals(emptyList<Int>(), altTabOrder(listOf(1), emptyList()))
    }

    @Test
    fun firstTabIsTheWindowFocusedBefore() {
        // As focuscurrentorlast: the history's second entry.
        val s = AltTabSession(listOf(3, 1, 2), listOf(1, 2, 3), focused = 3)
        assertEquals(1, s.selectedTask)
    }

    @Test
    fun nothingFocusedSelectsTheMostRecent() {
        val s = AltTabSession(listOf(3, 1, 2), listOf(1, 2, 3), focused = -1)
        assertEquals(3, s.selectedTask)
    }

    @Test
    fun oneOrNoWindow() {
        assertEquals(7, AltTabSession(listOf(7), listOf(7), focused = 7).selectedTask)
        val empty = AltTabSession(emptyList(), emptyList(), focused = -1)
        assertNull(empty.selectedTask)
        empty.step(true)
        assertNull(empty.selectedTask)
    }

    @Test
    fun tabGoesDeeperShiftTabBackBothWrap() {
        val s = AltTabSession(listOf(3, 1, 2), listOf(1, 2, 3), focused = 3)
        s.step(true)
        assertEquals(2, s.selectedTask)
        s.step(true)
        assertEquals(3, s.selectedTask)
        s.step(false)
        assertEquals(2, s.selectedTask)
        s.step(false)
        s.step(false)
        assertEquals(3, s.selectedTask)
    }

    @Test
    fun selectByClick() {
        val s = AltTabSession(listOf(3, 1, 2), listOf(1, 2, 3), focused = 3)
        assertTrue(s.select(2))
        assertEquals(2, s.selectedTask)
        assertFalse(s.select(42))
        assertEquals(2, s.selectedTask)
    }

    @Test
    fun updateKeepsOrderAndSelection() {
        val s = AltTabSession(listOf(3, 1, 2), listOf(1, 2, 3), focused = 3)
        s.step(true) // 2
        s.update(listOf(2, 3, 9)) // 1 closed, 9 opened
        assertEquals(listOf(3, 2, 9), s.order)
        assertEquals(2, s.selectedTask)
        s.update(listOf(3, 9)) // the selected one closed: the place is kept
        assertEquals(listOf(3, 9), s.order)
        assertEquals(9, s.selectedTask)
        s.update(emptyList())
        assertNull(s.selectedTask)
    }
}
