package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class FuzzyTest {
    @Test
    fun matchesInOrderIgnoringCase() {
        assertNotNull(fuzzyScore("stg", "Settings"))
        assertNotNull(fuzzyScore("FIRE", "Firefox"))
        assertNull(fuzzyScore("gts", "Settings"))
        assertNull(fuzzyScore("xyz", "Settings"))
        assertEquals(0, fuzzyScore("  ", "Settings"))
    }

    @Test
    fun prefixAndWordStartsWin() {
        val apps = listOf("Calendar", "Clock", "Contacts", "Calculator", "Camera", "Files", "Chrome Canary")
        assertEquals("Clock", fuzzyFilter("clo", apps) { it }.first())
        assertEquals("Calculator", fuzzyFilter("calc", apps) { it }.first())
        assertEquals("Chrome Canary", fuzzyFilter("cc", apps) { it }.first())
        assertTrue(fuzzyFilter("fl", apps) { it }.first() == "Files")
    }

    @Test
    fun consecutiveBeatsScattered() {
        val a = fuzzyScore("set", "Settings")!!
        val b = fuzzyScore("set", "Sound Effects Tuner")!!
        assertTrue("$a > $b", a > b)
    }

    @Test
    fun emptyQueryKeepsOrder() {
        val apps = listOf("b", "a", "c")
        assertEquals(apps, fuzzyFilter("", apps) { it })
    }
}
