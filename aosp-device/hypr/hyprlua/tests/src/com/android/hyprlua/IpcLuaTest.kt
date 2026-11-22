package com.android.hyprlua

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E12: what hyprctl asks the Lua side (eval, repl, keyword, getoption). */
class IpcLuaTest {
    private val host = FakeHost()
    private val lua = HyprLua(host) { it in setOf("Q", "1") }

    private fun load(src: String) {
        val r = lua.load(src)
        if (r is HyprLua.Result.Failed) throw AssertionError("load failed: ${r.message}")
    }

    @Test
    fun evalAndRepl() {
        assertEquals("error: no configuration loaded", lua.eval("x = 1", repl = false))
        load("hl.config({ general = { gaps_in = 7 } })")
        assertEquals("ok", lua.eval("x = 40 + 2", repl = false))
        assertEquals("42", lua.eval("x", repl = true))
        assertEquals("a\t1\n2", lua.eval("print('a', 1) return 2", repl = true))
        assertEquals("hi", lua.eval("print('hi')", repl = false))
        assertEquals("7", lua.eval("hl.get_config('general.gaps_in')", repl = true))
        assertTrue(lua.eval("error('boom')", repl = false).startsWith("error: "))
        assertTrue(lua.eval("this is not lua", repl = false).startsWith("error: "))
        // print is back to its own afterwards.
        assertEquals("ok", lua.eval("assert(print ~= nil)", repl = false))
    }

    @Test
    fun evalDispatches() {
        load("")
        assertEquals("ok", lua.eval("hl.dispatch(hl.dsp.focus({ workspace = 3 }))", repl = false))
        assertEquals("workspace" to "3", host.dispatched.last())
    }

    @Test
    fun keywordAndOption() {
        load("hl.config({ general = { gaps_in = 7 } })")
        assertEquals("7", lua.option("general:gaps_in"))
        assertTrue(lua.optionSet("general:gaps_in"))
        assertFalse(lua.optionSet("general:gaps_out"))
        assertNull(lua.keyword("general:gaps_out", "12"))
        assertEquals("12", lua.option("general.gaps_out"))
        assertEquals("12", host.changed.last().wire.values["gaps_out"])
        assertNull(lua.keyword("decoration:shadow:enabled", "false"))
        assertEquals("false", lua.option("decoration:shadow:enabled"))
        assertTrue(lua.keyword("general:gaps_in", "lots") != null)
        assertTrue(lua.keyword("no:such_option", "1") != null)
        assertNull(lua.option("no:such_option"))
        assertTrue("general.gaps_in" in lua.options())
    }

    @Test
    fun execRawIsExecr() {
        load("""hl.bind("ALT + Q", hl.dsp.exec_raw("hyprctl dispatch workspace 1"))""")
        assertEquals("ok", lua.eval("hl.dispatch(hl.dsp.exec_raw('hyprctl notify 1 1000 0 hi'))", repl = false))
        assertEquals("execr" to "hyprctl notify 1 1000 0 hi", host.dispatched.last())
    }
}
