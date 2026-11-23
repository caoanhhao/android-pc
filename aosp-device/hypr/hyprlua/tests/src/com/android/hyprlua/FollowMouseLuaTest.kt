package com.android.hyprlua

import com.android.hyprtile.FollowMouseConfig
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Hyprland's `input.follow_mouse` and its companions on the wire. */
class FollowMouseLuaTest {
    private val lua = HyprLua(FakeHost()) { true }

    private fun load(src: String): HyprConfig = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    private fun error(src: String): String = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> throw AssertionError("expected an error")
        is HyprLua.Result.Failed -> r.message
    }

    @Test
    fun defaultsFollowTheMouse() {
        val c = load("")
        assertEquals(FollowMouseConfig(), FollowMouseConfig.read { c.wire.values[it] })
        assertTrue(c.warnings.isEmpty())
    }

    @Test
    fun optionsGoOnTheWire() {
        val c = load("""
            hl.config({ input = {
                follow_mouse = 0,
                follow_mouse_threshold = 25,
                follow_mouse_shrink = 10,
                float_switch_override_focus = 2,
            } })
        """.trimIndent())
        assertTrue(c.warnings.toString(), c.warnings.isEmpty())
        assertEquals(FollowMouseConfig(0, 25f, 10, 2), FollowMouseConfig.read { c.wire.values[it] })
        assertEquals("0", lua.eval("hl.get_config('input.follow_mouse')", repl = true))
    }

    @Test
    fun modeThreeWarnsAndBadValuesFail() {
        val c = load("hl.config({ input = { follow_mouse = 3 } })")
        assertEquals("3", c.wire.values["follow_mouse"])
        assertTrue(c.warnings.any { it.contains("follow_mouse = 3") })
        assertTrue(error("hl.config({ input = { follow_mouse = 4 } })").contains("follow_mouse"))
        assertTrue(error("hl.config({ input = { follow_mouse_threshold = -1 } })").contains("threshold"))
        assertTrue(error("hl.config({ input = { float_switch_override_focus = 3 } })").contains("float_switch"))
    }
}
