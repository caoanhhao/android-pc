package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BarVisibilityTest {
    @Test
    fun plainBarTogglesItsStrip() {
        val b = BarVisibility()
        assertTrue(b.visible)
        assertTrue(b.reserves)
        assertFalse(b.edgeStrip)
        assertNull(b.dispatch("toggle"))
        assertFalse(b.visible)
        assertFalse(b.reserves)
        assertFalse(b.edgeStrip)
        // No auto-hide: the pointer at the edge brings nothing back.
        b.pointerEntered()
        assertFalse(b.visible)
        assertNull(b.touched())
        b.dispatch("show")
        assertTrue(b.visible && b.reserves)
        b.dispatch("hide")
        assertFalse(b.visible)
    }

    @Test
    fun autohideRevealsOnPointerAndHidesAfterLeaving() {
        val b = BarVisibility(autohide = true, hideDelayMs = 400)
        assertFalse(b.visible)
        assertFalse(b.reserves)
        assertTrue(b.edgeStrip)
        assertNull(b.pointerEntered())
        assertTrue(b.visible)
        assertFalse("a revealed bar floats over the windows", b.reserves)
        assertFalse(b.edgeStrip)
        assertEquals(400L, b.pointerLeft())
        val gen = b.generation
        // Back in before the timer: the stale timer does nothing.
        b.pointerEntered()
        assertFalse(b.expire(gen))
        assertTrue(b.visible)
        assertEquals(400L, b.pointerLeft())
        assertTrue(b.expire(b.generation))
        assertFalse(b.visible)
        assertTrue(b.edgeStrip)
    }

    @Test
    fun timerWhilePointerInsideKeepsTheBar() {
        val b = BarVisibility(autohide = true)
        b.pointerEntered()
        b.pointerLeft()
        val gen = b.generation
        b.pointerEntered()
        assertFalse(b.expire(gen))
        assertFalse(b.expire(b.generation))
        assertTrue(b.visible)
    }

    @Test
    fun pinningAnAutoHidingBarReservesItsStrip() {
        val b = BarVisibility(autohide = true)
        b.dispatch("toggle")
        assertTrue(b.visible && b.reserves)
        assertNull(b.pointerLeft())
        b.pointerEntered()
        assertTrue(b.reserves)
        b.dispatch("toggle")
        assertFalse(b.visible)
        assertTrue(b.edgeStrip)
    }

    @Test
    fun touchAndRevealKeepTheBarOutLonger() {
        val b = BarVisibility(autohide = true, hideDelayMs = 500)
        assertEquals(BarVisibility.TOUCH_HIDE_DELAY_MS, b.touched())
        assertTrue(b.visible)
        assertTrue(b.expire(b.generation))
        assertEquals(BarVisibility.TOUCH_HIDE_DELAY_MS, b.dispatch("reveal"))
        assertTrue(b.visible)
        val slow = BarVisibility(autohide = true, hideDelayMs = 5000)
        assertEquals(5000L, slow.touched())
        // A pinned bar or one without auto-hide is not revealed.
        assertNull(BarVisibility().dispatch("reveal"))
    }

    @Test
    fun newConfigurationResetsOnlyWhenAutohideChanges() {
        val b = BarVisibility()
        b.dispatch("hide")
        b.configure(autohide = false, hideDelayMs = 800)
        assertFalse("same mode keeps the user's choice", b.visible)
        assertEquals(800L, b.hideDelayMs)
        b.configure(autohide = true, hideDelayMs = 800)
        assertFalse(b.pinned)
        assertTrue(b.edgeStrip)
        b.dispatch("show")
        b.configure(autohide = false, hideDelayMs = 800)
        assertTrue(b.pinned)
    }

    @Test
    fun resetGoesBackToTheConfiguredState() {
        val b = BarVisibility(autohide = true)
        b.dispatch("show")
        b.reset()
        assertFalse(b.pinned)
        assertTrue(b.edgeStrip)
        val plain = BarVisibility()
        plain.dispatch("hide")
        plain.reset()
        assertTrue(plain.visible && plain.reserves)
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsUnknownActions() {
        BarVisibility().dispatch("sideways")
    }
}
