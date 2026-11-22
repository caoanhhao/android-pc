package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ClipHistoryTest {
    @Test
    fun newestFirstWithoutDuplicates() {
        val h = ClipHistory()
        h.add("one")
        h.add("two")
        h.add("one")
        assertEquals(listOf("one", "two"), h.items)
    }

    @Test
    fun keepsOnlyCapacity() {
        val h = ClipHistory(capacity = 3)
        for (i in 1..5) h.add("clip $i")
        assertEquals(listOf("clip 5", "clip 4", "clip 3"), h.items)
    }

    @Test
    fun skipsSensitiveBlankAndOversized() {
        val h = ClipHistory(maxChars = 10)
        assertFalse(h.add("hunter2", sensitive = true))
        assertFalse(h.add("   "))
        assertFalse(h.add(null))
        assertFalse(h.add("x".repeat(11)))
        assertTrue(h.add("x".repeat(10)))
        assertEquals(1, h.items.size)
    }

    @Test
    fun removeAndClear() {
        val h = ClipHistory()
        h.add("a")
        h.add("b")
        h.remove("a")
        assertEquals(listOf("b"), h.items)
        h.clear()
        assertTrue(h.items.isEmpty())
    }

    @Test
    fun filterMatchesAllWordsInAnyOrder() {
        val h = ClipHistory()
        h.add("Hello World")
        h.add("world peace")
        h.add("hello there")
        assertEquals(listOf("hello there", "world peace", "Hello World"), h.filter(" "))
        assertEquals(listOf("hello there", "Hello World"), h.filter("HELLO"))
        assertEquals(listOf("Hello World"), h.filter("world hello"))
    }

    @Test
    fun previewIsOneShortLine() {
        assertEquals("a b c", ClipHistory.preview("  a\n\tb   c \n"))
        assertEquals("abcd…", ClipHistory.preview("abcdefgh", max = 5))
    }

    @Test
    fun screenshotModes() {
        assertEquals(ScreenshotMode.AREA, ScreenshotMode.parse(""))
        assertEquals(ScreenshotMode.AREA, ScreenshotMode.parse("Region"))
        assertEquals(ScreenshotMode.FULL, ScreenshotMode.parse("full"))
        assertEquals(ScreenshotMode.FULL, ScreenshotMode.parse("now"))
        assertNull(ScreenshotMode.parse("window"))
    }
}
