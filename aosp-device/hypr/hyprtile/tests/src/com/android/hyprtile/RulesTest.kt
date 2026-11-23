package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RulesTest {
    @Test
    fun windowRulesRoundTripAndMatch() {
        val r = WindowRule("fox", "firefox", "Private", float = true, workspace = 2, silent = true,
            opacityActive = 0.9f, opacityInactive = 0.7f)
        assertEquals(r, WindowRule.decode(r.encode()))
        assertTrue(r.matches("org.mozilla.firefox", "Private Browsing"))
        assertFalse(r.matches("org.mozilla.firefox", "Home"))
        assertFalse(r.matches(null, "Private"))
        assertEquals(WindowRule(), WindowRule.decode(WindowRule().encode()))
    }

    @Test
    fun laterRulesWin() {
        val rules = listOf(
            WindowRule(matchClass = "calc", float = true),
            WindowRule(matchClass = "calc", float = false),
            WindowRule(matchClass = "other", float = true),
        )
        assertEquals(false, rules.effect("com.android.calculator2", null) { it.float })
        assertEquals(null, rules.effect("com.android.settings", null) { it.float })
    }

    @Test
    fun windowRulesDecideFloating() {
        val cfg = TileConfig(windowRules = listOf(
            WindowRule(matchClass = "^com\\.example\\.tool$", float = true),
            WindowRule(matchTitle = "Permission", float = false),
        ))
        assertTrue(shouldFloat(WindowTraits("com.example.tool"), cfg))
        assertFalse(shouldFloat(WindowTraits("com.example.toolbox"), cfg))
        // A rule tiles a dialog-themed window; a rule floats one that cannot be resized.
        assertFalse(shouldFloat(WindowTraits("x", "Permission", dialog = true), cfg))
        assertTrue(shouldFloat(WindowTraits("com.example.tool", resizeable = false), cfg))
    }

    @Test
    fun workspaceRulesChangeLayoutAndGapsPerWorkspace() {
        val rule = WorkspaceRule(3, "code", LayoutKind.MASTER, 0, 0, "com.termux")
        assertEquals(rule, WorkspaceRule.decode(rule.encode()))
        val tile = HyprTile(TileConfig(gapsIn = 5, gapsOut = 12))
        val area = Box(0, 0, 2000, 1000)
        for (ws in listOf(30, 40)) {
            tile.open(1 + ws, ws)
            tile.open(2 + ws, ws, focused = 1 + ws)
        }
        tile.workspaceRules = mapOf(40 to rule)
        assertEquals(LayoutKind.MASTER, tile.configOf(40).layout)
        val ws40 = tile.arrange(40, area)
        assertEquals(Box(0, 0, 1100, 1000), ws40[41])
        val ws30 = tile.arrange(30, area)
        assertEquals(Box(12, 12, 995, 988), ws30[31])
        tile.workspaceRules = emptyMap()
        assertEquals(Box(12, 12, 995, 988), tile.arrange(40, area)[41])
    }

    @Test
    fun gesturesRoundTrip() {
        val g = Gesture(3, "horizontal", "workspace")
        assertEquals(g, Gesture.decode(g.encode()))
    }

    @Test
    fun parsesLuaStyleKeys() {
        assertEquals(setOf(Mod.SUPER, Mod.SHIFT) to "Q", parseKeys("SUPER + SHIFT + q"))
        assertEquals(setOf(Mod.ALT) to "RETURN", parseKeys("ALT + Return"))
        assertEquals(setOf(Mod.CTRL, Mod.ALT) to "T", parseKeys("CTRL ALT, T"))
        assertEquals(emptySet<Mod>() to "F5", parseKeys("F5"))
        assertEquals("DPAD_LEFT", androidKeyName("left"))
        assertEquals("ENTER", androidKeyName("RETURN"))
        assertEquals("MOVE_HOME", androidKeyName("Home"))
        assertEquals("Q", androidKeyName("q"))
    }
}
