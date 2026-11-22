package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MouseBindsTest {
    private val alt = setOf(Mod.ALT)

    @Test
    fun parsesMouseKeys() {
        assertEquals(MouseInput.LEFT, MouseInput.parse("mouse:272"))
        assertEquals(MouseInput.FORWARD, MouseInput.parse("MOUSE:276"))
        assertEquals(MouseInput.WHEEL_DOWN, MouseInput.parse("mouse_down"))
        assertEquals(MouseInput.WHEEL_RIGHT, MouseInput.parse("MOUSE_RIGHT"))
        assertNull(MouseInput.parse("mouse:277"))
        assertNull(MouseInput.parse("Q"))
        assertTrue(MouseInput.WHEEL_UP.isWheel)
        assertFalse(MouseInput.BACK.isPointerButton)
    }

    @Test
    fun wheelDirectionFromAxes() {
        assertEquals(MouseInput.WHEEL_UP, MouseInput.wheel(1f, 0f))
        assertEquals(MouseInput.WHEEL_DOWN, MouseInput.wheel(-1f, 0.5f))
        assertEquals(MouseInput.WHEEL_LEFT, MouseInput.wheel(0f, -1f))
        assertEquals(MouseInput.WHEEL_RIGHT, MouseInput.wheel(0f, 1f))
        assertNull(MouseInput.wheel(0f, 0f))
    }

    @Test
    fun bindLineRoundTripKeepsFlags() {
        val b = MouseBind(alt + Mod.SHIFT, MouseInput.WHEEL_DOWN, "workspace", "e+1", consuming = false, repeating = true)
        val line = b.toBind().toString()
        assertEquals("SHIFT ALT, MOUSE_DOWN/NE, workspace, e+1", line)
        assertEquals(b, MouseBind.from(parseBind(line)))
        assertEquals(MouseBind(alt, MouseInput.LEFT, MouseBind.DRAG), MouseBind.from(parseBind("ALT, MOUSE:272, __drag")))
        assertNull(MouseBind.from(parseBind("ALT, Q, killactive")))
        assertTrue(MouseBind.isMouseKey("MOUSE_UP/E"))
    }

    @Test
    fun matchesExactModifiers() {
        val binds = MouseBinds(listOf(MouseBind(alt, MouseInput.WHEEL_DOWN, "workspace", "e+1")))
        assertEquals("e+1", binds.match(alt, MouseInput.WHEEL_DOWN)?.arg)
        assertNull(binds.match(alt + Mod.SHIFT, MouseInput.WHEEL_DOWN))
        assertNull(binds.match(emptySet(), MouseInput.WHEEL_DOWN))
        assertNull(binds.match(alt, MouseInput.WHEEL_UP))
    }

    @Test
    fun scrollDelayLimitsWheelBindsUnlessRepeating() {
        val down = MouseBind(alt, MouseInput.WHEEL_DOWN, "workspace", "e+1")
        val up = MouseBind(alt, MouseInput.WHEEL_UP, "workspace", "e-1", repeating = true)
        val click = MouseBind(alt, MouseInput.MIDDLE, "togglefloating")
        val binds = MouseBinds(listOf(down, up, click), scrollDelayMs = 300)
        assertTrue(binds.fires(down, 1000))
        assertFalse(binds.fires(down, 1100))
        assertFalse(binds.fires(down, 1299))
        assertTrue(binds.fires(down, 1300))
        assertTrue(binds.fires(up, 1000))
        assertTrue(binds.fires(up, 1001))
        assertTrue(binds.fires(click, 1000))
        assertTrue(binds.fires(click, 1001))
        assertTrue(MouseBinds(listOf(down), scrollDelayMs = 0).let { it.fires(down, 1) && it.fires(down, 2) })
    }

    @Test
    fun wheelFilterListsConsumedWheelBinds() {
        val binds = MouseBinds(listOf(
            MouseBind(alt, MouseInput.WHEEL_DOWN, "workspace", "e+1"),
            MouseBind(alt, MouseInput.WHEEL_UP, "workspace", "e-1"),
            MouseBind(setOf(Mod.SUPER, Mod.SHIFT, Mod.CTRL), MouseInput.WHEEL_LEFT, "movefocus", "l"),
            MouseBind(emptySet(), MouseInput.WHEEL_RIGHT, "movefocus", "r", consuming = false),
            MouseBind(alt, MouseInput.LEFT, MouseBind.DRAG),
        ))
        assertEquals("A:d,A:u,SCM:l", binds.wheelFilter())
        assertEquals("", MouseBinds(binds.binds.toList(), passMouseWhenBound = true).wheelFilter())
        assertEquals("-:d", MouseBinds(listOf(MouseBind(emptySet(), MouseInput.WHEEL_DOWN, "workspace", "e+1"))).wheelFilter())
    }

    @Test
    fun passingFollowsNonConsumingAndPassMouseWhenBound() {
        val b = MouseBind(alt, MouseInput.RIGHT, "killactive")
        assertFalse(MouseBinds(listOf(b)).passes(b))
        assertTrue(MouseBinds(listOf(b), passMouseWhenBound = true).passes(b))
        assertTrue(MouseBinds(emptyList()).passes(b.copy(consuming = false)))
    }

    @Test
    fun defaultsWithoutABindList() {
        val d = MouseBinds.fromBindList(null, alt)
        assertEquals(MouseBind.DRAG, d.match(alt, MouseInput.LEFT)?.dispatcher)
        assertEquals(MouseBind.RESIZE, d.match(alt, MouseInput.RIGHT)?.dispatcher)
        assertEquals("togglefloating", d.match(alt, MouseInput.MIDDLE)?.dispatcher)
        assertEquals("", d.wheelFilter())
        val lua = MouseBinds.fromBindList(listOf(parseBind("ALT, MOUSE_DOWN, workspace, e+1"), parseBind("ALT, Q, killactive")), alt)
        assertEquals(1, lua.binds.size)
        assertNull(lua.match(alt, MouseInput.LEFT))
    }
}
