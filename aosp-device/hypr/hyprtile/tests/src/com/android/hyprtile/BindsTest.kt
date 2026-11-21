package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BindsTest {
    @Test
    fun parsesHyprlandBindLines() {
        assertEquals(
            Bind(setOf(Mod.SUPER, Mod.SHIFT), "1", "movetoworkspace", "1"),
            parseBind("SUPER SHIFT, 1, movetoworkspace, 1"),
        )
        assertEquals(Bind(setOf(Mod.ALT), "Q", "killactive"), parseBind("alt, q, killactive"))
        assertEquals(setOf(Mod.CTRL, Mod.ALT), parseMods("CTRL_ALT"))
        assertEquals(setOf(Mod.SUPER), parseMods("META"))
        assertEquals(emptySet<Mod>(), parseMods(""))
        assertEquals("exec", parseBind("ALT, RETURN, exec, com.termux, extra").dispatcher)
        assertEquals("com.termux, extra", parseBind("ALT, RETURN, exec, com.termux, extra").arg)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsUnknownModifiers() {
        parseMods("HYPER")
    }

    @Test
    fun defaultBindsAreUniqueAndUseTheMainModifier() {
        val binds = defaultBinds(setOf(Mod.ALT), terminal = "com.termux")
        assertEquals(binds.size, binds.map { it.mods to it.key }.toSet().size)
        assertTrue(binds.all { Mod.ALT in it.mods && Mod.SUPER !in it.mods })
        assertTrue(Bind(setOf(Mod.ALT), "5", "workspace", "5") in binds)
        assertTrue(Bind(setOf(Mod.ALT, Mod.SHIFT), "9", "movetoworkspace", "9") in binds)
        assertTrue(Bind(setOf(Mod.ALT), "Q", "exec", "com.termux") in binds)
        assertTrue(defaultBinds(setOf(Mod.SUPER)).none { it.key == "Q" })
    }

    @Test
    fun resolvesWorkspaceArguments() {
        assertEquals(3, resolveWorkspace("3", 1))
        assertEquals(3, resolveWorkspace("e+1", 2))
        assertEquals(1, resolveWorkspace("-1", 2))
        assertNull(resolveWorkspace("e-1", 1))
        assertNull(resolveWorkspace("special", 1))
    }

    @Test
    fun neighborsFollowTheDwindleLayout() {
        // On a square: 1 | 2 / (3 | 4), 1 the left half, 2 top right, 3 and 4 bottom right.
        val square = Box(0, 0, 1600, 1600)
        val t = HyprTile()
        t.open(1, 0); t.open(2, 0, focused = 1)
        val b2 = t.arrange(0, square)
        assertEquals(2, neighbor(b2, 1, Direction.RIGHT, slack = 10))
        assertEquals(1, neighbor(b2, 2, Direction.LEFT, slack = 10))
        assertNull(neighbor(b2, 1, Direction.LEFT, slack = 10))
        assertNull(neighbor(b2, 1, Direction.UP, slack = 10))

        t.open(3, 0, focused = 2)
        t.open(4, 0, focused = 3)
        val boxes = t.arrange(0, square)
        assertEquals(2, neighbor(boxes, 3, Direction.UP, slack = 10))
        assertEquals(3, neighbor(boxes, 4, Direction.LEFT, slack = 10))
        assertEquals(1, neighbor(boxes, 3, Direction.LEFT, slack = 10))
        // From 2 down: 3 and 4 both touch it; 3 is first in reading order with the same gap and
        // overlap, either is fine, but it must be one of them.
        assertTrue(neighbor(boxes, 2, Direction.DOWN, slack = 10) in setOf(3, 4))
        assertNull(neighbor(boxes, 4, Direction.RIGHT, slack = 10))
        // Nothing lies below the left half: no diagonal jump to 3 or 4.
        assertNull(neighbor(boxes, 1, Direction.DOWN, slack = 10))
    }

    @Test
    fun neighborFallsBackToTheNearestCentre() {
        val boxes = mapOf(1 to Box(0, 0, 100, 100), 2 to Box(500, 400, 600, 500))
        assertEquals(2, neighbor(boxes, 1, Direction.RIGHT))
        assertEquals(2, neighbor(boxes, 1, Direction.DOWN))
        assertNull(neighbor(boxes, 1, Direction.LEFT))
        assertEquals(Direction.UP, Direction.parse("t"))
        assertNull(Direction.parse("x"))
    }
}
