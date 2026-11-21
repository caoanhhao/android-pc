package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HyprTileTest {
    private val wide = Box(0, 0, 2560, 1080)

    @Test
    fun workspacesHaveTheirOwnLayouts() {
        val t = HyprTile()
        assertTrue(t.open(1, 0))
        assertTrue(t.open(2, 0))
        assertTrue(t.open(3, 1))
        assertEquals(listOf(1, 2), t.tiled(0))
        assertEquals(listOf(3), t.tiled(1))
        assertEquals(Box(12, 12, 2548, 1068), t.arrange(1, wide)[3])
        assertEquals(1, t.workspaceOf(3))
    }

    @Test
    fun reopeningOnTheSameWorkspaceIsANoOp() {
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0)
        assertFalse(t.open(1, 0))
        assertEquals(listOf(1, 2), t.tiled(0))
    }

    @Test
    fun movingToAnotherWorkspaceLeavesTheOldOne() {
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0)
        assertTrue(t.open(2, 1))
        assertEquals(listOf(1), t.tiled(0))
        assertEquals(listOf(2), t.tiled(1))
    }

    @Test
    fun focusedWindowOnAnotherWorkspaceIsNotATarget() {
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0); t.open(9, 1)
        t.open(3, 0, focused = 9)
        assertEquals(listOf(1, 2, 3), t.tiled(0))
        t.open(4, 0, focused = 1)
        assertEquals(listOf(1, 4, 2, 3), t.tiled(0))
    }

    @Test
    fun closeReturnsTheWorkspace() {
        val t = HyprTile()
        t.open(1, 3)
        assertEquals(3, t.close(1))
        assertNull(t.close(1))
        assertTrue(t.arrange(3, wide).isEmpty())
    }

    @Test
    fun floatingWindowsAreNotTiled() {
        val t = HyprTile()
        t.open(1, 0)
        assertFalse(t.open(2, 0, float = true))
        assertTrue(t.isFloating(2))
        assertEquals(setOf(1), t.arrange(0, wide).keys)
        assertTrue(t.toggleFloating(1, 0))
        assertTrue(t.arrange(0, wide).isEmpty())
        assertFalse(t.toggleFloating(2, 0))
        assertEquals(setOf(2), t.arrange(0, wide).keys)
        assertFalse(t.isFloating(2))
    }

    @Test
    fun switchingLayoutKeepsTheWindows() {
        val t = HyprTile()
        listOf(1, 2, 3).forEach { t.open(it, 0) }
        t.config = t.config.copy(layout = LayoutKind.MASTER, gapsIn = 0, gapsOut = 0)
        val boxes = t.arrange(0, wide)
        assertEquals(Box(0, 0, 1408, 1080), boxes[1])
        assertEquals(Box(1408, 0, 2560, 540), boxes[2])
        t.config = t.config.copy(layout = LayoutKind.DWINDLE)
        assertEquals(listOf(1, 2, 3), t.tiled(0))
    }

    @Test
    fun swapNeedsOneWorkspace() {
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0)
        t.swap(1, 2)
        assertEquals(listOf(2, 1), t.tiled(0))
    }

    @Test
    fun floatRules() {
        val cfg = TileConfig(floatPackages = setOf("com.example.calc"))
        assertFalse(shouldFloat(WindowTraits("com.android.settings"), cfg))
        assertTrue(shouldFloat(WindowTraits(resizeable = false), cfg))
        assertTrue(shouldFloat(WindowTraits(translucent = true), cfg))
        assertTrue(shouldFloat(WindowTraits(dialog = true), cfg))
        assertTrue(shouldFloat(WindowTraits("com.example.calc"), cfg))
        assertTrue(shouldFloat(WindowTraits(minWidth = 3000), cfg, wide))
        assertFalse(shouldFloat(WindowTraits(minWidth = 800), cfg, wide))
    }

    @Test(expected = IllegalArgumentException::class)
    fun badConfigIsRejected() {
        TileConfig(defaultSplitRatio = 3f)
    }
}
