package com.android.hyprlua

import com.android.hyprtile.Bind
import com.android.hyprtile.Mod
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E3: window history and Hyprland's window switching dispatchers in Lua. */
class WindowHistoryLuaTest {
    private val keys = setOf("TAB", "GRAVE", "N", "P", "S", "U", "Z", "B", "1", "F")
    private val host = FakeHost()
    private val lua = HyprLua(host) { it in keys }

    private fun load(src: String): HyprConfig = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    private fun error(src: String): String = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> throw AssertionError("expected an error")
        is HyprLua.Result.Failed -> r.message
    }

    private fun binds(c: HyprConfig): Map<Pair<Set<Mod>, String>, Bind> =
        c.wire.lists.getValue("binds").map { parseBind(it) }.associateBy { it.mods to it.key }

    @Test
    fun defaultAltTabIsFocusCurrentOrLast() {
        val b = binds(load(""))
        assertEquals(Bind(setOf(Mod.ALT), "TAB", "focuscurrentorlast"), b[setOf(Mod.ALT) to "TAB"])
        assertEquals("overview", b[setOf(Mod.ALT) to "GRAVE"]?.dispatcher)
    }

    @Test
    fun focusDispatchers() {
        val b = binds(load("""
            local mainMod = "ALT"
            hl.bind(mainMod .. " + Tab", hl.dsp.focus({ last = "" }))
            hl.bind(mainMod .. " + U", hl.dsp.focus({ urgent_or_last = true }))
            hl.bind(mainMod .. " + grave", hl.dsp.focus({ workspace = "previous_per_monitor" }))
            hl.bind(mainMod .. " + B", hl.dsp.focus({ workspace = "previous", on_current_monitor = true }))
            hl.bind(mainMod .. " + F", hl.dsp.focus({ window = "class:^org\\.chromium" }))
            hl.bind(mainMod .. " + 1", hl.dsp.focus({ window = 42 }))
        """.trimIndent()))
        val alt = setOf(Mod.ALT)
        assertEquals("focuscurrentorlast", b[alt to "TAB"]?.dispatcher)
        assertEquals("focusurgentorlast", b[alt to "U"]?.dispatcher)
        assertEquals(Bind(alt, "GRAVE", "workspace", "previous_per_monitor"), b[alt to "GRAVE"])
        assertEquals(Bind(alt, "B", "workspace", "previous"), b[alt to "B"])
        assertEquals(Bind(alt, "F", "focuswindow", "class:^org\\.chromium"), b[alt to "F"])
        assertEquals(Bind(alt, "1", "focuswindow", "taskid:42"), b[alt to "1"])
    }

    @Test
    fun windowDispatchers() {
        val b = binds(load("""
            hl.bind("ALT + N", hl.dsp.window.cycle_next())
            hl.bind("ALT + SHIFT + N", hl.dsp.window.cycle_next({ next = false, tiled = true }))
            hl.bind("ALT + CTRL + N", hl.dsp.window.cycle_next({ floating = true, hist = true }))
            hl.bind("ALT + S", hl.dsp.window.swap({ next = true }))
            hl.bind("ALT + SHIFT + S", hl.dsp.window.swap({ prev = true }))
            hl.bind("ALT + CTRL + S", hl.dsp.window.swap({ target = "title:Settings" }))
            hl.bind("ALT + Z", hl.dsp.window.alter_zorder({ mode = "top" }))
            hl.bind("ALT + SHIFT + Z", hl.dsp.window.alter_zorder({ mode = "bottom", window = "class:termux" }))
            hl.bind("ALT + P", hl.dsp.raw("cycleprev", "tiled"))
        """.trimIndent()))
        val alt = setOf(Mod.ALT)
        val shift = setOf(Mod.ALT, Mod.SHIFT)
        val ctrl = setOf(Mod.ALT, Mod.CTRL)
        assertEquals(Bind(alt, "N", "cyclenext"), b[alt to "N"])
        assertEquals(Bind(shift, "N", "cyclenext", "prev tiled"), b[shift to "N"])
        assertEquals(Bind(ctrl, "N", "cyclenext", "floating hist"), b[ctrl to "N"])
        assertEquals(Bind(alt, "S", "swapnext"), b[alt to "S"])
        assertEquals(Bind(shift, "S", "swapnext", "prev"), b[shift to "S"])
        assertEquals(Bind(ctrl, "S", "swapwindow", "title:Settings"), b[ctrl to "S"])
        assertEquals(Bind(alt, "Z", "alterzorder", "top"), b[alt to "Z"])
        assertEquals(Bind(shift, "Z", "alterzorder", "bottom,class:termux"), b[shift to "Z"])
        assertEquals(Bind(alt, "P", "cycleprev", "tiled"), b[alt to "P"])
    }

    @Test
    fun badArgumentsAreErrors() {
        assertTrue(error("""hl.bind("ALT + Z", hl.dsp.window.alter_zorder({ mode = "middle" }))""")
            .contains("top' or 'bottom"))
        assertTrue(error("""hl.bind("ALT + F", hl.dsp.focus({ window = "tag:x" }))""").contains("tags"))
        assertTrue(error("""hl.bind("ALT + F", hl.dsp.focus({ window = "class:(" }))""").contains("regex"))
        assertTrue(error("""hl.bind("ALT + F", hl.dsp.raw("cyclenext", "sideways"))""").contains("cyclenext"))
        assertTrue(error("""hl.bind("ALT + F", hl.dsp.focus({ }))""").contains("urgent_or_last"))
        assertTrue(error("""hl.bind("ALT + F", hl.dsp.window.swap({ }))""").contains("next, prev"))
    }

    @Test
    fun historyQueries() {
        host.snap = host.snap.copy(history = listOf(7, 9), urgent = listOf(9), lastWorkspace = 2)
        load("""
            local last = hl.get_last_window()
            local urgent = hl.get_urgent_window()
            local ws = hl.get_last_workspace()
            hl.notify(last.class .. " " .. urgent.id .. " " .. ws.id)
        """.trimIndent())
        assertEquals("org.mozilla.firefox 9 2", host.notes.last())
        host.snap = host.snap.copy(history = listOf(7), urgent = emptyList(), lastWorkspace = 0)
        load("""hl.notify(tostring(hl.get_last_window()) .. tostring(hl.get_urgent_window()) .. tostring(hl.get_last_workspace()))""")
        assertEquals("nilnilnil", host.notes.last())
    }

    @Test
    fun bindsOptions() {
        val c = load("""
            hl.config({ binds = { workspace_back_and_forth = true, allow_workspace_cycles = true } })
        """.trimIndent())
        assertEquals("1", c.wire.values["workspace_back_and_forth"])
        assertEquals("true", c.wire.values["allow_workspace_cycles"])
        val d = load("""hl.config({ binds = { workspace_back_and_forth = 2 } })""")
        assertEquals("2", d.wire.values["workspace_back_and_forth"])
        assertEquals("false", d.wire.values["allow_workspace_cycles"])
        assertEquals("0", load("").wire.values["workspace_back_and_forth"])
    }
}
