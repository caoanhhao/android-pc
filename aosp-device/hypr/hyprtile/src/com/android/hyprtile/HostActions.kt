package com.android.hyprtile

/**
 * A key the `volume`, `brightness`, `media` and `inputmethod` dispatchers (Phase E11) send in
 * place of the host's wpctl, brightnessctl, playerctl and fcitx5-remote: Android's own key
 * code (KeyEvent's value, so this file needs no Android classes), with SHIFT for the previous
 * keyboard layout (LANGUAGE_SWITCH with SHIFT goes backwards).
 */
data class HostKey(val keyCode: Int, val shift: Boolean = false) {
    companion object {
        const val KEYCODE_VOLUME_UP = 24
        const val KEYCODE_VOLUME_DOWN = 25
        const val KEYCODE_VOLUME_MUTE = 164
        const val KEYCODE_BRIGHTNESS_DOWN = 220
        const val KEYCODE_BRIGHTNESS_UP = 221
        const val KEYCODE_MEDIA_PLAY_PAUSE = 85
        const val KEYCODE_MEDIA_STOP = 86
        const val KEYCODE_MEDIA_NEXT = 87
        const val KEYCODE_MEDIA_PREVIOUS = 88
        const val KEYCODE_LANGUAGE_SWITCH = 204

        private val KEYS = mapOf(
            "volume" to mapOf(
                "up" to HostKey(KEYCODE_VOLUME_UP), "+" to HostKey(KEYCODE_VOLUME_UP),
                "down" to HostKey(KEYCODE_VOLUME_DOWN), "-" to HostKey(KEYCODE_VOLUME_DOWN),
                "mute" to HostKey(KEYCODE_VOLUME_MUTE),
            ),
            "brightness" to mapOf(
                "up" to HostKey(KEYCODE_BRIGHTNESS_UP), "+" to HostKey(KEYCODE_BRIGHTNESS_UP),
                "down" to HostKey(KEYCODE_BRIGHTNESS_DOWN), "-" to HostKey(KEYCODE_BRIGHTNESS_DOWN),
            ),
            "media" to mapOf(
                "play" to HostKey(KEYCODE_MEDIA_PLAY_PAUSE), "play_pause" to HostKey(KEYCODE_MEDIA_PLAY_PAUSE),
                "playpause" to HostKey(KEYCODE_MEDIA_PLAY_PAUSE), "pause" to HostKey(KEYCODE_MEDIA_PLAY_PAUSE),
                "next" to HostKey(KEYCODE_MEDIA_NEXT),
                "previous" to HostKey(KEYCODE_MEDIA_PREVIOUS), "prev" to HostKey(KEYCODE_MEDIA_PREVIOUS),
                "stop" to HostKey(KEYCODE_MEDIA_STOP),
            ),
            "inputmethod" to mapOf(
                "next" to HostKey(KEYCODE_LANGUAGE_SWITCH), "" to HostKey(KEYCODE_LANGUAGE_SWITCH),
                "toggle" to HostKey(KEYCODE_LANGUAGE_SWITCH),
                "previous" to HostKey(KEYCODE_LANGUAGE_SWITCH, shift = true),
                "prev" to HostKey(KEYCODE_LANGUAGE_SWITCH, shift = true),
            ),
        )

        /** The key of dispatcher [name] with [arg] (any case); null when either is unknown. */
        fun parse(name: String, arg: String): HostKey? = KEYS[name]?.get(arg.trim().lowercase())

        /** The arguments dispatcher [name] takes (for error messages). */
        fun args(name: String): Set<String> = KEYS[name]?.keys.orEmpty()
    }
}

/**
 * The `display` dispatcher's modes (Phase E11), the host's dual-screen script: `extend`
 * (dualscreen: every display on, each its own desktop), `mirror` (the default display shown on
 * the others), `primary` (the external displays off) and `external` (only the external
 * displays: the default display powered off).
 */
enum class DisplayMode(val arg: String) {
    EXTEND("extend"), MIRROR("mirror"), PRIMARY("primary"), EXTERNAL("external");

    companion object {
        private val ALIASES = mapOf("dualscreen" to EXTEND, "dual" to EXTEND, "single" to PRIMARY)

        fun parse(arg: String): DisplayMode? {
            val a = arg.trim().lowercase()
            return entries.find { it.arg == a } ?: ALIASES[a]
        }
    }
}
