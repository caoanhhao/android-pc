package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FollowMouseTest {
    // Two tiles side by side, a floating window over the left one, a second floating one
    // partly under the first.
    private val left = HoverWindow(1, Box(0, 0, 500, 1000), 0)
    private val right = HoverWindow(2, Box(510, 0, 1000, 1000), 0)
    private val float = HoverWindow(3, Box(100, 100, 300, 300), 1)
    private val under = HoverWindow(4, Box(250, 250, 450, 450), 1)
    private val windows = listOf(float, under, left, right)

    private fun follow(c: FollowMouseConfig = FollowMouseConfig()) = FollowMouse(c)

    @Test
    fun readsTheConfiguration() {
        val wire = mapOf(
            "follow_mouse" to "2",
            "follow_mouse_threshold" to "12.5",
            "follow_mouse_shrink" to "8",
            "float_switch_override_focus" to "0",
        )
        assertEquals(FollowMouseConfig(2, 12.5f, 8, 0), FollowMouseConfig.read { wire[it] })
        assertEquals(FollowMouseConfig(), FollowMouseConfig.read { null })
        val bad = mapOf("follow_mouse" to "7", "follow_mouse_threshold" to "-1", "follow_mouse_shrink" to "x")
        assertEquals(FollowMouseConfig(), FollowMouseConfig.read { bad[it] })
    }

    @Test
    fun hoverFocusesTheWindowUnderThePointer() {
        val f = follow()
        assertEquals(2, f.target(windows, 700f, 500f, focused = 1, now = 0))
        // Over the floating window, not the tile under it.
        assertEquals(3, f.target(windows, 150f, 150f, focused = 2, now = 0))
        // Float to float, and onto a tile.
        assertEquals(1, f.target(windows, 50f, 900f, focused = 3, now = 0))
    }

    @Test
    fun nothingForTheFocusedWindowOrAGap() {
        val f = follow()
        assertNull(f.target(windows, 700f, 500f, focused = 2, now = 0))
        assertNull(f.target(windows, 505f, 500f, focused = 1, now = 0))
        assertNull(f.target(emptyList(), 505f, 500f, focused = null, now = 0))
    }

    @Test
    fun aWindowPartlyCoveredInItsLayerWaitsForAClick() {
        // The visible part of the lower floating window: focusing would raise it over the other.
        assertNull(follow().target(windows, 400f, 400f, focused = 1, now = 0))
        // A tile under a floating window is another layer: it does not move.
        assertEquals(1, follow().target(windows, 50f, 50f, focused = 2, now = 0))
    }

    @Test
    fun askedOnceUntilTheFocusArrivesOrTimesOut() {
        val f = follow()
        assertEquals(2, f.target(windows, 700f, 500f, focused = 1, now = 1000))
        assertNull(f.target(windows, 720f, 500f, focused = 1, now = 1200))
        assertEquals(2, f.target(windows, 720f, 500f, focused = 1, now = 1000 + FollowMouse.RETRY_MS))
        // The focus arrived and moved away again (a key): asked at once.
        f.focusChanged()
        assertEquals(2, f.target(windows, 720f, 500f, focused = 1, now = 1600))
    }

    @Test
    fun clickModesOnlySwitchBetweenTiledAndFloating() {
        val f = follow(FollowMouseConfig(mode = 0))
        assertNull(f.target(windows, 700f, 500f, focused = 1, now = 0))
        assertEquals(3, f.target(windows, 150f, 150f, focused = 2, now = 0))
        assertEquals(1, f.target(windows, 50f, 900f, focused = 3, now = 0))
        // Float to float only with float_switch_override_focus = 2.
        val other = HoverWindow(5, Box(600, 600, 800, 800), 1)
        val ws = windows + other
        assertNull(follow(FollowMouseConfig(mode = 2)).target(listOf(other) + ws, 700f, 700f, focused = 3, now = 0))
        assertEquals(5, follow(FollowMouseConfig(mode = 2, floatSwitch = 2))
            .target(listOf(other) + ws, 700f, 700f, focused = 3, now = 0))
        // Switching off.
        assertNull(follow(FollowMouseConfig(mode = 0, floatSwitch = 0)).target(windows, 150f, 150f, focused = 2, now = 0))
        // Nothing focused on the workspace: a click decides.
        assertNull(f.target(windows, 150f, 150f, focused = null, now = 0))
        // 3 never follows.
        assertNull(follow(FollowMouseConfig(mode = 3, floatSwitch = 2)).target(windows, 150f, 150f, focused = 2, now = 0))
    }

    @Test
    fun thresholdCountsFromTheFirstMoveAfterAFocusChange() {
        val f = follow(FollowMouseConfig(threshold = 50f))
        f.focusChanged()
        f.moved(495f, 500f)
        f.moved(530f, 500f)
        assertNull(f.target(windows, 530f, 500f, focused = 1, now = 0))
        f.moved(560f, 500f)
        assertEquals(2, f.target(windows, 560f, 500f, focused = 1, now = 0))
    }

    @Test
    fun shrinkKeepsTheEdgesOfAWindowOut() {
        val f = follow(FollowMouseConfig(shrink = 20))
        assertNull(f.target(windows, 520f, 500f, focused = 1, now = 0))
        assertEquals(2, f.target(windows, 540f, 500f, focused = 1, now = 0))
    }
}
