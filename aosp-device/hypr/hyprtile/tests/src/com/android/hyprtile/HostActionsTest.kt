package com.android.hyprtile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HostActionsTest {
    @Test
    fun volumeBrightnessAndMediaKeys() {
        assertEquals(HostKey(HostKey.KEYCODE_VOLUME_UP), HostKey.parse("volume", "up"))
        assertEquals(HostKey(HostKey.KEYCODE_VOLUME_DOWN), HostKey.parse("volume", " Down "))
        assertEquals(HostKey(HostKey.KEYCODE_VOLUME_MUTE), HostKey.parse("volume", "mute"))
        assertEquals(HostKey(HostKey.KEYCODE_BRIGHTNESS_UP), HostKey.parse("brightness", "+"))
        assertEquals(HostKey(HostKey.KEYCODE_BRIGHTNESS_DOWN), HostKey.parse("brightness", "down"))
        assertEquals(HostKey(HostKey.KEYCODE_MEDIA_PLAY_PAUSE), HostKey.parse("media", "play_pause"))
        assertEquals(HostKey(HostKey.KEYCODE_MEDIA_PREVIOUS), HostKey.parse("media", "prev"))
        assertNull(HostKey.parse("brightness", "mute"))
        assertNull(HostKey.parse("volume", "loud"))
        assertNull(HostKey.parse("workspace", "up"))
    }

    @Test
    fun inputMethodSwitchGoesBackWithShift() {
        assertEquals(HostKey(HostKey.KEYCODE_LANGUAGE_SWITCH), HostKey.parse("inputmethod", ""))
        assertEquals(HostKey(HostKey.KEYCODE_LANGUAGE_SWITCH), HostKey.parse("inputmethod", "next"))
        assertEquals(HostKey(HostKey.KEYCODE_LANGUAGE_SWITCH, shift = true),
            HostKey.parse("inputmethod", "previous"))
        assertTrue("toggle" in HostKey.args("inputmethod"))
    }

    @Test
    fun displayModes() {
        assertEquals(DisplayMode.EXTEND, DisplayMode.parse("extend"))
        assertEquals(DisplayMode.EXTEND, DisplayMode.parse("dualscreen"))
        assertEquals(DisplayMode.MIRROR, DisplayMode.parse("Mirror"))
        assertEquals(DisplayMode.PRIMARY, DisplayMode.parse("primary"))
        assertEquals(DisplayMode.EXTERNAL, DisplayMode.parse("external"))
        assertNull(DisplayMode.parse("left"))
    }

    @Test
    fun hostDispatchersAreKnown() {
        for (name in listOf("volume", "brightness", "media", "inputmethod", "display",
                "forcekillactive", "reload", "wallpaper")) {
            assertTrue(name, name in DISPATCHERS)
        }
    }
}
