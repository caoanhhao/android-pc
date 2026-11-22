package com.android.hyprlua

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E4: `shell.alt_tab`, what ALT + Tab does. */
class AltTabLuaTest {
    private val lua = HyprLua(FakeHost()) { it in setOf("TAB") }

    private fun load(src: String): HyprConfig = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    @Test
    fun switcherByDefault() {
        assertEquals("switcher", load("").wire.values["alt_tab"])
    }

    @Test
    fun modes() {
        for (m in listOf("switcher", "cycle", "bind", "overview", "off")) {
            assertEquals(m, load("hl.config({ shell = { alt_tab = \"$m\" } })").wire.values["alt_tab"])
        }
        assertEquals("cycle", load("hl.config({ shell = { alt_tab = \"Cycle\" } })").wire.values["alt_tab"])
    }

    @Test
    fun unknownModeIsAnError() {
        val r = lua.load("hl.config({ shell = { alt_tab = \"windows\" } })")
        assertTrue(r is HyprLua.Result.Failed)
        assertTrue((r as HyprLua.Result.Failed).message, r.message.contains("\"switcher\""))
    }
}
