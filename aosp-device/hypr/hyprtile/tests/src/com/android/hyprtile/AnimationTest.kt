package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AnimationTest {
    @Test
    fun bezierEndsAndLinear() {
        val c = Bezier(0.23f, 1f, 0.32f, 1f)
        assertEquals(0f, c.value(0f), 0f)
        assertEquals(1f, c.value(1f), 0f)
        assertEquals(0f, c.value(-1f), 0f)
        for (i in 0..10) assertEquals(i / 10f, Bezier.LINEAR.value(i / 10f), 1e-4f)
    }

    @Test
    fun bezierMatchesCubicBezierReference() {
        // CSS ease (0.25, 0.1, 0.25, 1) at x = 0.5 is about 0.8024.
        assertEquals(0.8024f, Bezier(0.25f, 0.1f, 0.25f, 1f).value(0.5f), 2e-3f)
        // easeOutQuint rises fast: well past half way at a quarter of the time.
        assertTrue(Bezier(0.23f, 1f, 0.32f, 1f).value(0.25f) > 0.7f)
        // Overshoot: y control points above 1 go past the end before settling.
        val snappy = Bezier(0.05f, 0.9f, 0.1f, 1.05f)
        assertTrue((1..99).any { snappy.value(it / 100f) > 1f })
        // Monotonic in time for a curve without overshoot.
        val q = Bezier(0.15f, 0f, 0.1f, 1f)
        var last = 0f
        for (i in 1..200) q.value(i / 200f).also { assertTrue(it >= last - 1e-5f); last = it }
    }

    @Test
    fun defaultsFollowHyprlandsExample() {
        val a = AnimationConfig()
        assertEquals(410L, a.resolve("windowsIn").durationMs)
        assertEquals(WindowStyle.Popin(87), a.windowStyle("windowsIn"))
        assertEquals(WindowStyle.Popin(87), a.windowStyle("windowsOut"))
        assertEquals(149L, a.resolve("windowsOut").durationMs)
        // windowsMove is not set: it takes windows' settings.
        assertEquals(AnimSpec(true, 4.79f, "easeOutQuint"), a.resolve("windowsMove"))
        assertEquals(Bezier(0.23f, 1f, 0.32f, 1f), a.curve("windowsMove"))
        assertEquals(WorkspaceStyle(), a.workspaceStyle("workspacesIn"))
        assertEquals(250L, a.resolve("workspacesOut").durationMs)
        assertEquals(1000L, a.resolve("border").copy(speed = 10f).durationMs)
    }

    @Test
    fun parsesHyprlandLines() {
        val a = AnimationConfig()
            .withBezier("snappy, 0.05, 0.9, 0.1, 1.05")
            .withAnimation("windows, 1, 4, snappy, popin 80%")
            .withAnimation("windowsIn, 0")
            .withAnimation("workspaces, 1, 5, snappy, slidefadevert 30%")
        assertEquals(Bezier(0.05f, 0.9f, 0.1f, 1.05f), a.curve("windowsMove"))
        assertFalse(a.resolve("windowsIn").enabled)
        assertEquals(0L, a.resolve("windowsIn").durationMs)
        assertEquals(WorkspaceStyle(vertical = true, slidePercent = 30, fade = true),
            a.workspaceStyle("workspacesIn"))
        assertEquals(WorkspaceStyle(slide = false, fade = true), WorkspaceStyle.parse("fade"))
        assertEquals(WorkspaceStyle(vertical = true), WorkspaceStyle.parse("slidevert"))
        assertEquals(WindowStyle.Slide(WindowStyle.Edge.TOP), WindowStyle.parse("slide top"))
        assertEquals(WindowStyle.Fade, WindowStyle.parse("fade"))
        // An unknown curve name falls back to Hyprland's default.
        assertEquals(Bezier.DEFAULT, a.withAnimation("fade, 1, 3, nosuchcurve").curve("fadeSwitch"))
        assertThrows("name") { a.withAnimation("windowz, 1, 4, snappy") }
        assertThrows("speed") { a.withAnimation("windows, 1, 0, snappy") }
        assertThrows("style") { a.withAnimation("workspaces, 1, 4, snappy, spin") }
        assertThrows("bezier") { a.withBezier("bad, 1.5, 0, 0, 1") }
        assertThrows("bezier args") { a.withBezier("bad, 0, 0") }
    }

    @Test
    fun configsCompareByValue() {
        assertEquals(AnimationConfig(), AnimationConfig().withBezier("linear, 0, 0, 1, 1"))
        assertTrue(AnimationConfig() != AnimationConfig().withAnimation("windows, 0"))
    }
}
