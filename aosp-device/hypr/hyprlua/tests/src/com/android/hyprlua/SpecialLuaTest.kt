package com.android.hyprlua

import com.android.hyprtile.WindowRule
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E5: special workspaces (scratchpads) in the Lua configuration. */
class SpecialLuaTest {
    private val host = FakeHost()
    private val lua = HyprLua(host) { it in setOf("S", "T") }

    private fun load(src: String): HyprConfig = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    private fun binds(c: HyprConfig) = c.wire.lists.getValue("binds").map { parseBind(it) }
        .associate { "${it.mods.sorted().joinToString(" ")} ${it.key}" to "${it.dispatcher} ${it.arg}".trim() }

    @Test
    fun dispatchers() {
        val c = load("""
            hl.config({ shell = { default_binds = false } })
            hl.bind("ALT + S", hl.dsp.workspace.toggle_special())
            hl.bind("ALT + T", hl.dsp.workspace.toggle_special("special:term"))
            hl.bind("ALT + CTRL + S", hl.dsp.window.move({ workspace = "special" }))
            hl.bind("ALT + SHIFT + S", hl.dsp.window.move({ workspace = "special:term", follow = false }))
            hl.bind("ALT + CTRL + T", hl.dsp.focus({ workspace = "special:term" }))
        """.trimIndent())
        val b = binds(c)
        assertEquals("togglespecialworkspace", b["ALT S"])
        assertEquals("togglespecialworkspace term", b["ALT T"])
        assertEquals("movetoworkspace special", b["CTRL ALT S"])
        assertEquals("movetoworkspacesilent special:term", b["SHIFT ALT S"])
        assertEquals("workspace special:term", b["CTRL ALT T"])
    }

    @Test
    fun rules() {
        val c = load("""
            hl.window_rule({ match = { class = "termux" }, workspace = "special:term silent" })
            hl.window_rule({ match = { class = "calc" }, workspace = "special" })
            hl.workspace_rule({ workspace = "special:term", gaps_out = 60, layout = "master" })
            hl.workspace_rule({ workspace = "special:term", gaps_in = 2 })
        """.trimIndent())
        val rules = c.wire.lists.getValue("windowrule").map { WindowRule.decode(it) }
        assertEquals("term", rules[0].special)
        assertTrue(rules[0].silent)
        assertEquals("special", rules[1].special)
        val ws = c.workspaceRules.single()
        assertEquals("term", ws.special)
        assertEquals(60, ws.gapsOut)
        assertEquals(2, ws.gapsIn)
        assertEquals(ws, WorkspaceRule.decode(c.wire.lists.getValue("workspace").single()))
    }

    @Test
    fun options() {
        val d = load("")
        assertEquals("0.2", d.wire.values["dim_special"])
        assertEquals("1.0", d.wire.values["special_scale_factor"])
        assertEquals("false", d.wire.values["hide_special_on_workspace_change"])
        val c = load("""
            hl.config({ decoration = { dim_special = 0.5 }, dwindle = { special_scale_factor = 0.8 },
              master = { special_scale_factor = 0.6 }, binds = { hide_special_on_workspace_change = true } })
        """.trimIndent())
        assertEquals("0.5", c.wire.values["dim_special"])
        assertEquals("0.8", c.wire.values["special_scale_factor"])
        assertEquals("true", c.wire.values["hide_special_on_workspace_change"])
        val m = load("""hl.config({ general = { layout = "master" }, master = { special_scale_factor = 0.6 } })""")
        assertEquals("0.6", m.wire.values["special_scale_factor"])
    }

    @Test
    fun specialWorkspacesInTheState() {
        host.snap = Snapshot(1, 1, 7, listOf(
            SnapWindow(7, 1, "com.android.settings", "Settings", false, false),
            SnapWindow(9, -99, "com.termux", "Termux", false, false),
        ), specials = mapOf(-99 to "special", -98 to "term"), specialShown = "special")
        load("""
            local ws = hl.get_workspaces()
            local s = ws[#ws]
            hl.notify(#ws .. " " .. s.name .. " " .. tostring(s.special) .. " " .. tostring(s.visible) .. " " .. s.windows)
            local w = hl.get_window("taskid:9")
            hl.notify(w.workspace.name .. " " .. w.workspace.id)
        """.trimIndent())
        assertEquals(listOf("2 special:special true true 1", "special:special -99"), host.notes)
    }
}
