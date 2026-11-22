package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ZoomTest {
    @Test
    fun parsesZoomArguments() {
        assertEquals(ZoomOp.By(0.5f), parseZoom("+0.5"))
        assertEquals(ZoomOp.By(-0.2f), parseZoom(" -0.2 "))
        assertEquals(ZoomOp.To(1f), parseZoom("1"))
        assertEquals(ZoomOp.To(2.5f), parseZoom("2.5"))
        assertEquals(ZoomOp.To(1f), parseZoom("reset"))
        assertNull(parseZoom(""))
        assertNull(parseZoom("in"))
        assertNull(parseZoom("NaN"))
    }

    @Test
    fun appliesAndClamps() {
        assertEquals(1.5f, ZoomOp.By(0.5f).apply(1f), 1e-6f)
        assertEquals(1f, ZoomOp.By(-0.5f).apply(1.2f), 1e-6f)
        assertEquals(MAX_ZOOM, ZoomOp.By(0.5f).apply(MAX_ZOOM), 1e-6f)
        assertEquals(MAX_ZOOM, ZoomOp.To(20f).apply(1f), 1e-6f)
        assertEquals(1f, ZoomOp.To(0f).apply(3f), 1e-6f)
        // Steps do not drift.
        assertEquals(1.4f, ZoomOp.By(0.2f).apply(ZoomOp.By(0.2f).apply(1f)))
        assertEquals(1f, ZoomOp.By(-0.2f).apply(ZoomOp.By(-0.2f).apply(1.4f)))
    }

    @Test
    fun argsRoundTrip() {
        assertEquals(ZoomOp.By(0.5f), parseZoom(zoomArg(delta = 0.5f)))
        assertEquals(ZoomOp.By(-0.2f), parseZoom(zoomArg(delta = -0.2f)))
        assertEquals(ZoomOp.To(1f), parseZoom(zoomArg(factor = 1f)))
    }

    @Test
    fun centreKeepsThePointedContentUnderThePointer() {
        val size = 2000f
        for (scale in listOf(1f, 1.5f, 2f, 4f)) {
            for (p in listOf(0f, 300f, 1000f, 1999f, 2000f)) {
                val c = zoomCentre(p, size, scale)
                // Android shows content q at (q - c) * scale + size / 2; q = p must stay at p.
                assertEquals(p, (p - c) * scale + size / 2, 1e-2f)
                // The view never leaves the screen.
                val half = size / (2 * scale)
                assertEquals(true, c >= half - 1e-2f && c <= size - half + 1e-2f)
            }
        }
        assertEquals(1000f, zoomCentre(1000f, size, 3f), 1e-3f)
        assertEquals(0f + size / 4, zoomCentre(-50f, size, 2f), 1e-3f)
    }
}
