package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class StyleTest {
    @Test
    fun parsesHyprlandColours() {
        assertEquals(0xee33ccff.toInt(), parseColor("rgba(33ccffee)"))
        assertEquals(0xff33ccff.toInt(), parseColor("rgb(33CCFF)"))
        assertEquals(0xee33ccff.toInt(), parseColor("0xee33ccff"))
        assertEquals(0xff33ccff.toInt(), parseColor("#33ccff"))
        assertEquals(0x8033ccff.toInt(), parseColor("#33ccff80"))
        for (bad in listOf("red", "rgba(33ccff)", "rgb(33ccffee)", "0x33ccff", "#12345", "rgba(zz00zz00)")) {
            assertThrows(bad) { parseColor(bad) }
        }
    }

    @Test
    fun parsesGradients() {
        val g = Gradient.parse("rgba(33ccffee) rgba(00ff99ee) 45deg")
        assertEquals(listOf(0xee33ccff.toInt(), 0xee00ff99.toInt()), g.colors)
        assertEquals(45, g.angle)
        assertEquals(Gradient(listOf(0xaa595959.toInt()), 0), Gradient.parse("rgba(595959aa)"))
        assertEquals(g, Gradient.parse(g.toString()))
        assertThrows("empty") { Gradient.parse("45deg") }
    }

    @Test
    fun gradientProgressFollowsHyprlandsShader() {
        val two = listOf(0, 1)
        val h = Gradient(two, 0)
        assertEquals(0f, h.progress(0f, 0.7f), 1e-6f)
        assertEquals(1f, h.progress(1f, 0.2f), 1e-6f)
        val v = Gradient(two, 90)
        assertEquals(0f, v.progress(0.3f, 0f), 1e-6f)
        assertEquals(1f, v.progress(0.3f, 1f), 1e-6f)
        // 180 runs right to left, 270 bottom to top.
        assertEquals(1f, Gradient(two, 180).progress(0f, 0.5f), 1e-6f)
        assertEquals(1f, Gradient(two, 270).progress(0.5f, 0f), 1e-6f)
        // 45: sin(45) weights y, 1 - sin(45) weights x.
        val d = Gradient(two, 45)
        assertEquals(0f, d.progress(0f, 0f), 1e-6f)
        assertEquals(0.70710677f, d.progress(0f, 1f), 1e-5f)
        assertEquals(1f, d.progress(1f, 1f), 1e-5f)
        assertEquals(0f, Gradient(listOf(5)).progress(1f, 1f), 0f)
    }

    @Test
    fun gradientLineDrawsTheSameProgress() {
        for (angle in listOf(0, 30, 45, 90, 135, 200, 300, 359)) {
            val g = Gradient(listOf(0, 1), angle)
            val (w, h) = 1280 to 700
            val l = g.line(w, h)
            val dx = l[2] - l[0]
            val dy = l[3] - l[1]
            val len2 = dx * dx + dy * dy
            for ((x, y) in listOf(0 to 0, w to 0, 0 to h, w to h, 400 to 300)) {
                val along = ((x - l[0]) * dx + (y - l[1]) * dy) / len2
                assertEquals("angle $angle at $x,$y", g.progress(x / w.toFloat(), y / h.toFloat()),
                    along.coerceIn(0f, 1f), 1e-3f)
            }
        }
    }

    @Test
    fun captionModes() {
        val d = DecorationConfig()
        assertFalse(d.hasCaption(floating = true))
        assertFalse(d.hasCaption(floating = false))
        assertTrue(d.copy(caption = CaptionMode.FLOATING).hasCaption(true))
        assertFalse(d.copy(caption = CaptionMode.FLOATING).hasCaption(false))
        assertTrue(d.copy(caption = CaptionMode.ALL).hasCaption(false))
        assertEquals(CaptionMode.NONE, CaptionMode.parse("off"))
        assertEquals(CaptionMode.FLOATING, CaptionMode.parse("Floating"))
        assertThrows("mode") { CaptionMode.parse("some") }
        assertThrows("opacity") { DecorationConfig(inactiveOpacity = 1.5f) }
    }
}

internal fun assertThrows(what: String, block: () -> Unit) {
    try {
        block()
    } catch (e: IllegalArgumentException) {
        return
    }
    throw AssertionError("expected IllegalArgumentException for $what")
}
