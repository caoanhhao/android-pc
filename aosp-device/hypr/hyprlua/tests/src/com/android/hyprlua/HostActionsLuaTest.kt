package com.android.hyprlua

import com.android.hyprtile.androidKeyName
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Phase E11: the dispatchers for what the host's scripts do. */
class HostActionsLuaTest {
    private val keys = setOf("F2", "F3", "F11", "F12", "SPACE", "W", "R", "Q", "P", "NUMPAD_ENTER",
        "VOLUME_UP", "MEDIA_NEXT", "BRIGHTNESS_UP")
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

    private fun binds(c: HyprConfig) =
        c.wire.lists.getValue("binds").map { parseBind(it) }.associateBy { it.mods.sorted().joinToString("+") + "+" + androidKeyName(it.key) }

    @Test
    fun hostBindsLoad() {
        val b = binds(load("""
            hl.bind("ALT + F12", hl.dsp.volume("up"))
            hl.bind("ALT + F11", hl.dsp.volume({ action = "down" }))
            hl.bind("ALT + F3", hl.dsp.brightness("up"))
            hl.bind("ALT + F2", hl.dsp.brightness("down"))
            hl.bind("ALT + SHIFT + Space", hl.dsp.input_method())
            hl.bind("ALT + CTRL + SHIFT + P", hl.dsp.display("extend"))
            hl.bind("ALT + SHIFT + R", hl.dsp.reload())
            hl.bind("ALT + CTRL + Q", hl.dsp.window.kill())
            hl.bind("ALT + SHIFT + W", hl.dsp.wallpaper({ workspace = "current" }))
            hl.bind("ALT + CTRL + SHIFT + W", hl.dsp.wallpaper({ workspace = "all", dir = "/sdcard/Pictures/W" }))
        """.trimIndent()))
        assertEquals("volume" to "up", b.getValue("ALT+F12").let { it.dispatcher to it.arg })
        assertEquals("volume" to "down", b.getValue("ALT+F11").let { it.dispatcher to it.arg })
        assertEquals("brightness" to "up", b.getValue("ALT+F3").let { it.dispatcher to it.arg })
        assertEquals("brightness" to "down", b.getValue("ALT+F2").let { it.dispatcher to it.arg })
        assertEquals("inputmethod" to "next", b.getValue("SHIFT+ALT+SPACE").let { it.dispatcher to it.arg })
        assertEquals("display" to "extend", b.getValue("SHIFT+CTRL+ALT+P").let { it.dispatcher to it.arg })
        assertEquals("reload", b.getValue("SHIFT+ALT+R").dispatcher)
        assertEquals("forcekillactive", b.getValue("CTRL+ALT+Q").dispatcher)
        assertEquals("random current", b.getValue("SHIFT+ALT+W").arg)
        assertEquals("random all /sdcard/Pictures/W", b.getValue("SHIFT+CTRL+ALT+W").arg)
    }

    @Test
    fun floatingExec() {
        val b = binds(load("""
            hl.bind("ALT + KP_Enter", hl.dsp.exec_cmd("com.termux"))
            hl.bind("ALT + CTRL + KP_Enter", hl.dsp.exec_cmd("com.termux", { float = true }))
        """.trimIndent()))
        assertEquals("com.termux", b.getValue("ALT+NUMPAD_ENTER").arg)
        assertEquals("[float] com.termux", b.getValue("CTRL+ALT+NUMPAD_ENTER").arg)
    }

    @Test
    fun hostKeyNames() {
        assertEquals("VOLUME_UP", androidKeyName("XF86AudioRaiseVolume"))
        assertEquals("MEDIA_NEXT", androidKeyName("XF86AudioNext"))
        assertEquals("BRIGHTNESS_UP", androidKeyName("XF86MonBrightnessUp"))
        assertEquals("NUMPAD_ENTER", androidKeyName("KP_Enter"))
    }

    @Test
    fun dispatchFromLua() {
        load("""
            hl.dispatch(hl.dsp.media("next"))
            hl.dispatch(hl.dsp.input_method("previous"))
            hl.dispatch(hl.dsp.display("mirror"))
        """.trimIndent())
        assertEquals(listOf("media" to "next", "inputmethod" to "previous", "display" to "mirror"),
            host.dispatched.takeLast(3))
    }

    @Test
    fun badArgumentsFail() {
        assertTrue(error("hl.dsp.volume('loud')").contains("hl.dsp.volume"))
        assertTrue(error("hl.dsp.display('left')").contains("extend, mirror, primary, external"))
        assertTrue(error("hl.dsp.wallpaper({ workspace = 3 })").contains("'current' or 'all'"))
    }
}
