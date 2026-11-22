package com.android.hyprlua

import com.android.hyprtile.Mod
import com.android.hyprtile.MouseBind
import com.android.hyprtile.MouseInput
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** E6: mouse button and wheel binds through `hl.bind`. */
class MouseBindsLuaTest {
    private val lua = HyprLua(FakeHost()) { it.length == 1 || it.startsWith("DPAD_") }

    private fun load(src: String): HyprConfig = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    private fun error(src: String): String = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> throw AssertionError("expected an error")
        is HyprLua.Result.Failed -> r.message
    }

    private fun mouseBinds(c: HyprConfig) =
        c.wire.lists.getValue("binds").mapNotNull { MouseBind.from(parseBind(it)) }

    private fun find(c: HyprConfig, mods: Set<Mod>, input: MouseInput) =
        mouseBinds(c).firstOrNull { it.mods == mods && it.input == input }

    @Test
    fun defaultMouseBindsOnTheMainModifier() {
        val c = load("hl.config({ shell = { main_mod = 'SUPER' } })")
        assertEquals(MouseBind.DRAG, find(c, setOf(Mod.SUPER), MouseInput.LEFT)?.dispatcher)
        assertEquals(MouseBind.RESIZE, find(c, setOf(Mod.SUPER), MouseInput.RIGHT)?.dispatcher)
        assertEquals("togglefloating", find(c, setOf(Mod.SUPER), MouseInput.MIDDLE)?.dispatcher)
        assertEquals("false", c.wire.values["pass_mouse_when_bound"])
        assertEquals("300", c.wire.values["scroll_event_delay"])
    }

    @Test
    fun wheelAndButtonBindsTakeAnyDispatcher() {
        val c = load("""
            hl.bind("ALT + mouse_down", hl.dsp.focus({ workspace = "e+1" }))
            hl.bind("ALT + mouse_up", hl.dsp.focus({ workspace = "e-1" }), { repeating = true })
            hl.bind("CTRL + SHIFT + mouse_left", hl.dsp.focus({ direction = "l" }))
            hl.bind("ALT + mouse:274", function() end, { non_consuming = true })
            hl.bind("ALT + mouse:276", hl.dsp.window.close())
            hl.bind("SUPER + mouse:274", hl.dsp.window.drag(), { mouse = true })
            hl.config({ binds = { pass_mouse_when_bound = true, scroll_event_delay = 100 } })
        """)
        val alt = setOf(Mod.ALT)
        assertEquals(MouseBind(alt, MouseInput.WHEEL_DOWN, "workspace", "e+1"), find(c, alt, MouseInput.WHEEL_DOWN))
        assertTrue(find(c, alt, MouseInput.WHEEL_UP)!!.repeating)
        assertEquals("movefocus", find(c, setOf(Mod.CTRL, Mod.SHIFT), MouseInput.WHEEL_LEFT)?.dispatcher)
        val middle = find(c, alt, MouseInput.MIDDLE)!!
        assertEquals("lua", middle.dispatcher)
        assertEquals(false, middle.consuming)
        assertEquals("killactive", find(c, alt, MouseInput.FORWARD)?.dispatcher)
        assertEquals(MouseBind.DRAG, find(c, setOf(Mod.SUPER), MouseInput.MIDDLE)?.dispatcher)
        assertEquals("SUPER", c.wire.values["mouse_mod"])
        assertEquals("true", c.wire.values["pass_mouse_when_bound"])
        assertEquals("100", c.wire.values["scroll_event_delay"])
        assertTrue(c.warnings.none { "wheel" in it })
    }

    @Test
    fun unbindAndReplaceMouseBinds() {
        val c = load("""
            hl.unbind("ALT + mouse:272")
            hl.bind("ALT + mouse:273", hl.dsp.window.float())
        """)
        val alt = setOf(Mod.ALT)
        assertNull(find(c, alt, MouseInput.LEFT))
        assertEquals("togglefloating", find(c, alt, MouseInput.RIGHT)?.dispatcher)
        assertEquals(1, mouseBinds(c).count { it.input == MouseInput.RIGHT })
    }

    @Test
    fun badMouseBinds() {
        assertTrue(error("hl.bind('ALT + mouse:277', hl.dsp.window.close())").contains("mouse:272..mouse:276"))
        assertTrue(error("hl.bind('ALT + mouse_down', hl.dsp.window.drag())").contains("drag and resize take"))
        assertTrue(error("hl.bind('ALT + mouse:275', hl.dsp.window.drag(), { mouse = true })").contains("drag and resize take"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.window.close(), { mouse = true })").contains("mouse = true"))
        assertTrue(error("hl.bind('ALT + mouse_up', hl.dsp.raw('frob'))").contains("unknown dispatcher frob"))
        val c = load("hl.bind('ALT + mouse_up', hl.dsp.window.close(), { locked = true })")
        assertTrue(c.warnings.any { "locked not supported on mouse binds" in it })
    }
}
