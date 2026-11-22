package com.android.hyprlua

import com.android.hyprtile.Mod
import com.android.hyprtile.androidKeyName
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E10: `hl.dsp.zoom` and Hyprland's `cursor.zoom_factor`. */
class ZoomLuaTest {
    private val keys = setOf("EQUALS", "MINUS", "0")
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

    @Test
    fun zoomDispatcherBinds() {
        val c = load("""
            hl.bind("ALT + SHIFT + equal", hl.dsp.zoom({ delta = 0.5 }), { repeating = true })
            hl.bind("ALT + SHIFT + minus", hl.dsp.zoom({ delta = -0.5 }), { repeating = true })
            hl.bind("ALT + SHIFT + 0", hl.dsp.zoom({ factor = 1 }))
        """.trimIndent())
        // `repeating` works on key binds (E13): no warning, the flag rides on the key.
        assertTrue(c.warnings.isEmpty())
        val binds = c.wire.lists.getValue("binds").map { parseBind(it) }
        val zoom = binds.filter { it.dispatcher == "zoom" }.associate { androidKeyName(it.baseKey) to it.arg }
        assertEquals("+0.5", zoom["EQUALS"])
        assertEquals("-0.5", zoom["MINUS"])
        assertEquals("1", zoom["0"])
        val plus = binds.first { androidKeyName(it.baseKey) == "EQUALS" }
        assertTrue(plus.mods == setOf(Mod.ALT, Mod.SHIFT))
        assertTrue(plus.repeating)
        assertFalse(binds.first { it.baseKey == "0" && it.dispatcher == "zoom" }.repeating)
    }

    @Test
    fun zoomDispatchesFromLua() {
        load("hl.dispatch(hl.dsp.zoom({ delta = 0.2 }))")
        assertEquals("zoom" to "+0.2", host.dispatched.last())
    }

    @Test
    fun badZoomArgumentsFail() {
        assertTrue(error("hl.dsp.zoom(2)").contains("hl.dsp.zoom"))
        assertTrue(error("hl.dsp.zoom({ factor = 0.5 })").contains("1 or more"))
        assertTrue(error("hl.bind('ALT + 0', hl.dsp.raw('zoom', 'in'))").contains("zoom takes"))
    }

    @Test
    fun zoomFactorOptionGoesOnTheWire() {
        assertEquals("1.0", load("").wire.values["zoom_factor"])
        assertEquals("2.5", load("hl.config({ cursor = { zoom_factor = 2.5 } })").wire.values["zoom_factor"])
        // The host's set_zoom keeps adding; the shell stops at Android's maximum.
        assertEquals("8.0", load("hl.config({ cursor = { zoom_factor = 12 } })").wire.values["zoom_factor"])
        assertTrue(error("hl.config({ cursor = { zoom_factor = 0.5 } })").contains("zoom_factor"))
    }
}
