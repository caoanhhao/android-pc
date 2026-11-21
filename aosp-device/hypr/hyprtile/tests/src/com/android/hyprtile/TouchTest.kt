package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TouchTest {
    private val config = TouchConfig(width = 2560, height = 1080, edge = 24, distance = 80, slop = 16)

    /** One finger from (x0, y0) to (x1, y1) in [steps] moves over [ms]; the first result. */
    private fun swipe(x0: Float, y0: Float, x1: Float, y1: Float, excluded: Boolean = false,
                      steps: Int = 10, ms: Long = 200): List<String>? {
        val r = TouchRecognizer(config)
        r.down(0, x0, y0, 0, excluded = excluded, onWindow = true)
        for (i in 1..steps) {
            val f = i / steps.toFloat()
            r.move(mapOf(0 to (x0 + (x1 - x0) * f to y0 + (y1 - y0) * f)), ms * i / steps)?.let { return it }
        }
        return null
    }

    @Test
    fun edgeSwipesFromEachEdge() {
        assertEquals(listOf("EDGE_BOTTOM"), swipe(1200f, 1075f, 1200f, 900f))
        assertEquals(listOf("EDGE_LEFT"), swipe(3f, 500f, 200f, 510f))
        assertEquals(listOf("EDGE_RIGHT"), swipe(2555f, 500f, 2400f, 490f))
        assertEquals(listOf("EDGE_TOP"), swipe(600f, 2f, 600f, 200f))
        assertEquals(listOf("EDGE_TOP_RIGHT", "EDGE_TOP"), swipe(2300f, 2f, 2300f, 200f))
    }

    @Test
    fun noEdgeSwipeInTheMiddleSidewaysOrExcluded() {
        assertNull(swipe(1200f, 500f, 1200f, 300f))
        // Along the edge, not inward.
        assertNull(swipe(1200f, 1075f, 1500f, 1070f))
        // Outward first.
        assertNull(swipe(3f, 500f, 0f, 500f))
        // Apps asked for the edge.
        assertNull(swipe(1200f, 1075f, 1200f, 900f, excluded = true))
        // Too slow.
        assertNull(swipe(1200f, 1075f, 1200f, 900f, steps = 10, ms = 3000))
    }

    @Test
    fun threeFingerSwipeUp() {
        val r = TouchRecognizer(config)
        r.down(0, 1000f, 600f, 0, excluded = false, onWindow = true)
        r.pointerDown(1, 1100f, 600f, 10, onWindow = true)
        r.pointerDown(2, 1200f, 610f, 20, onWindow = true)
        var out: List<String>? = null
        for (i in 1..10) {
            val dy = -15f * i
            out = r.move(mapOf(0 to (1000f to 600f + dy), 1 to (1100f to 600f + dy), 2 to (1200f to 610f + dy)), 20L + 20 * i)
            if (out != null) break
        }
        assertEquals(listOf("SWIPE_3_UP"), out)
        assertTrue(r.recognized)
    }

    @Test
    fun threeFingersApartAreNoSwipe() {
        val r = TouchRecognizer(config)
        r.down(0, 1000f, 600f, 0, excluded = false, onWindow = true)
        r.pointerDown(1, 1100f, 600f, 10, onWindow = true)
        r.pointerDown(2, 1200f, 600f, 20, onWindow = true)
        // Two fingers up, one down: a spread, not a swipe.
        for (i in 1..10) {
            assertNull(r.move(mapOf(0 to (1000f to 600f - 20f * i), 1 to (1100f to 600f - 20f * i),
                2 to (1200f to 600f + 20f * i)), 20L + 20 * i))
        }
    }

    @Test
    fun pinchOnlyOffWindows() {
        fun pinch(onWindow: Boolean, from: Float, to: Float): List<String>? {
            val r = TouchRecognizer(config)
            r.down(0, 1280f - from, 500f, 0, excluded = false, onWindow = onWindow)
            r.pointerDown(1, 1280f + from, 500f, 5, onWindow = false)
            for (i in 1..10) {
                val d = from + (to - from) * i / 10
                r.move(mapOf(0 to (1280f - d to 500f), 1 to (1280f + d to 500f)), 5L + 20 * i)?.let { return it }
            }
            return null
        }
        assertEquals(listOf("PINCH_IN"), pinch(false, 300f, 80f))
        assertEquals(listOf("PINCH_OUT"), pinch(false, 80f, 300f))
        // On an app window a pinch is the app's zoom.
        assertNull(pinch(true, 300f, 80f))
    }

    @Test
    fun aSecondFingerEndsTheEdgeSwipe() {
        val r = TouchRecognizer(config)
        r.down(0, 1200f, 1075f, 0, excluded = false, onWindow = false)
        r.pointerDown(1, 1300f, 1000f, 10, onWindow = false)
        r.pointerUp(1)
        assertNull(r.move(mapOf(0 to (1200f to 800f)), 100))
    }

    @Test
    fun gestureKeysAndDefaults() {
        assertEquals("EDGE_BOTTOM", TouchGesture.nameOf("gesture:edge_bottom"))
        assertEquals("SWIPE_4_LEFT", TouchGesture.nameOf("GESTURE:SWIPE_4_LEFT"))
        assertTrue(runCatching { TouchGesture.nameOf("gesture:swipe_2_up") }.isFailure)
        val defaults = defaultTouchBinds().associate { TouchGesture.nameOf(it.key) to it.dispatcher }
        assertEquals("launcher", defaults["EDGE_BOTTOM"])
        assertEquals("overview", defaults["PINCH_IN"])
        assertEquals("overview", defaults["SWIPE_3_UP"])
        // A gesture bind survives the wire format.
        val b = defaultTouchBinds().first { it.arg == "e+1" }
        assertEquals(b, parseBind(b.toString()))
    }

    @Test
    fun hyprlandGesturesBecomeBinds() {
        val ws = gestureBinds(Gesture(3, "horizontal", "workspace"))
        assertEquals(listOf("GESTURE:SWIPE_3_LEFT" to "e+1", "GESTURE:SWIPE_3_RIGHT" to "e-1"),
            ws.map { it.key to it.arg })
        assertEquals("overview", gestureBinds(Gesture(4, "up", "overview")).single().dispatcher)
        assertEquals("GESTURE:PINCH_OUT", gestureBinds(Gesture(2, "pinchout", "launcher")).single().key)
        assertEquals("killactive", gestureBinds(Gesture(3, "down", "close")).single().dispatcher)
        assertTrue(runCatching { gestureBinds(Gesture(2, "up", "overview")) }.isFailure)
        assertTrue(runCatching { gestureBinds(Gesture(3, "up", "special")) }.isFailure)
    }
}
