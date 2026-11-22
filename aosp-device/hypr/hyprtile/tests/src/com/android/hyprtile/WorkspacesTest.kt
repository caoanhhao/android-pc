package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class WorkspacesTest {
    @Test
    fun resolvesNumbersAndNumericSteps() {
        assertEquals(3, resolveWorkspace("3", 1))
        assertEquals(1000, resolveWorkspace("1000", 1))
        assertNull(resolveWorkspace("1001", 1))
        assertNull(resolveWorkspace("11", 1, max = 10))
        assertEquals(1, resolveWorkspace("-1", 2))
        assertEquals(6, resolveWorkspace("+1", 5))
        assertEquals(7, resolveWorkspace("r+2", 5))
        assertNull(resolveWorkspace("-1", 1))
        assertNull(resolveWorkspace("+1", 10, max = 10))
        assertNull(resolveWorkspace("special", 1))
        assertNull(resolveWorkspace("0", 1))
    }

    @Test
    fun existingStepsWalkTheWorkspacesThatExistAndWrap() {
        val existing = listOf(1, 4, 12)
        assertEquals(12, resolveWorkspace("e+1", 4, existing))
        assertEquals(1, resolveWorkspace("e+1", 12, existing))
        assertEquals(12, resolveWorkspace("e-1", 1, existing))
        assertEquals(4, resolveWorkspace("e+2", 12, existing))
        // The current workspace counts even when empty.
        assertEquals(12, resolveWorkspace("e+1", 7, existing))
        assertEquals(4, resolveWorkspace("e-1", 7, existing))
        assertEquals(1, resolveWorkspace("e-1", 1))
        // Past the configured limit: left out.
        assertEquals(1, resolveWorkspace("e+1", 4, existing, max = 10))
    }

    @Test
    fun workspaceArgumentsAreChecked() {
        assertTrue(isWorkspaceArg("20"))
        assertTrue(isWorkspaceArg("e+1"))
        assertTrue(isWorkspaceArg("-1"))
        assertTrue(isWorkspaceArg("r-3"))
        assertFalse(isWorkspaceArg("1001"))
        assertFalse(isWorkspaceArg("0"))
        assertFalse(isWorkspaceArg("special:x"))
    }

    @Test
    fun barShowsActiveAndOccupied() {
        assertEquals(listOf(1, 3, 15), shownWorkspaces(3, listOf(15, 1, 1)))
        assertEquals(listOf(2, 7), shownWorkspaces(0, listOf(7, 2)))
        assertEquals(listOf(4), shownWorkspaces(4, emptyList()))
    }

    @Test
    fun mapNumbersDesksAndKeepsThemOverASync() {
        val m = WorkspaceMap()
        m.put(100, 7)
        // Shell's default desk and a restored one get the lowest free numbers.
        assertTrue(m.sync(listOf(50, 100, 60)))
        assertEquals(1, m.numberOf(50))
        assertEquals(2, m.numberOf(60))
        assertEquals(7, m.numberOf(100))
        assertEquals(100, m.deskOf(7))
        assertFalse(m.sync(listOf(50, 100, 60)))
        // A removed desk frees its number.
        assertTrue(m.sync(listOf(50, 100)))
        assertNull(m.deskOf(2))
        assertEquals(setOf(1, 7), m.numbers)
        // Taking a number from another desk.
        m.put(50, 7)
        assertNull(m.numberOf(100))
        assertEquals(50, m.deskOf(7))
    }

    @Test
    fun mapSurvivesEncoding() {
        val m = WorkspaceMap()
        m.put(3, 1); m.put(9, 20); m.put(11, 1000)
        val back = WorkspaceMap.decode(m.encode())
        assertEquals(m.encode(), back.encode())
        assertEquals(9, back.deskOf(20))
        assertEquals(0, WorkspaceMap.decode("x,4:0,5:abc,").size)
    }

    @Test
    fun keypadKeysMapToAndroidNumpad() {
        assertEquals("NUMPAD_1", androidKeyName("KP_End"))
        assertEquals("NUMPAD_0", androidKeyName("KP_Insert"))
        assertEquals("NUMPAD_9", androidKeyName("KP_Prior"))
        assertEquals("NUMPAD_5", androidKeyName("KP_Begin"))
        assertEquals("NUMPAD_7", androidKeyName("kp_7"))
    }
}
