package com.android.systemui.wmshell

import android.os.Bundle
import android.os.SystemProperties
import android.util.Log
import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.CaptionMode
import com.android.hyprtile.DecorationConfig
import com.android.hyprtile.Gradient

/**
 * Decoration and animation settings: the launcher's Lua configuration (IHyprShell.setConfig,
 * a Bundle with the same names, lists for `bezier` and `animation`) over the `persist.hypr.*`
 * properties. A bad value is logged and replaced by its default.
 *
 * - `persist.hypr.rounding`, `persist.hypr.border_size` (pixels)
 * - `persist.hypr.col.active_border`, `persist.hypr.col.inactive_border`
 *   (`rgba(33ccffee) rgba(00ff99ee) 45deg`)
 * - `persist.hypr.active_opacity`, `persist.hypr.inactive_opacity` (0..1)
 * - `persist.hypr.caption` (`none`, `floating`, `all`), `persist.hypr.shadow` (bool)
 * - `persist.hypr.animations` (bool, Hyprland's `animations:enabled`)
 * - `persist.hypr.bezier.1` .. `.16` (`name, x1, y1, x2, y2`)
 * - `persist.hypr.animation.1` .. `.32` (`NAME, ONOFF, SPEED, CURVE[, STYLE]`)
 */
object HyprStyleProps {
    private const val TAG = "HyprStyle"
    private const val P = "persist.hypr."

    fun decoration(o: Bundle): DecorationConfig {
        val d = DecorationConfig()
        fun <T> read(name: String, def: T, parse: (String) -> T): T {
            val raw = (o.getString(name) ?: SystemProperties.get(P + name)).trim()
            if (raw.isEmpty()) return def
            return runCatching { parse(raw) }
                .onFailure { Log.w(TAG, "bad $P$name '$raw': ${it.message}") }.getOrDefault(def)
        }
        val cfg = runCatching {
            d.copy(
                rounding = read("rounding", d.rounding) { it.toInt().coerceAtLeast(0) },
                borderSize = read("border_size", d.borderSize) { it.toInt().coerceAtLeast(0) },
                activeBorder = read("col.active_border", d.activeBorder, Gradient::parse),
                inactiveBorder = read("col.inactive_border", d.inactiveBorder, Gradient::parse),
                activeOpacity = read("active_opacity", d.activeOpacity) { it.toFloat().coerceIn(0f, 1f) },
                inactiveOpacity = read("inactive_opacity", d.inactiveOpacity) {
                    it.toFloat().coerceIn(0f, 1f)
                },
                caption = read("caption", d.caption, CaptionMode::parse),
                shadow = read("shadow", d.shadow) { parseBool(it) },
            )
        }
        return cfg.onFailure { Log.w(TAG, "decoration: ${it.message}") }.getOrDefault(d)
    }

    /** Hyprland's `animations:enabled`; with it off, Shell's own animations play. */
    fun animationsEnabled(o: Bundle): Boolean =
        o.getString("animations")?.toBooleanStrictOrNull() ?: SystemProperties.getBoolean(P + "animations", true)

    fun animations(o: Bundle): AnimationConfig {
        var cfg = AnimationConfig()
        val beziers = o.getStringArray("bezier")
        val animations = o.getStringArray("animation")
        if (beziers != null || animations != null) {
            for (line in beziers.orEmpty()) {
                cfg = runCatching { cfg.withBezier(line) }
                    .onFailure { Log.w(TAG, "bad bezier '$line': ${it.message}") }.getOrDefault(cfg)
            }
            for (line in animations.orEmpty()) {
                cfg = runCatching { cfg.withAnimation(line) }
                    .onFailure { Log.w(TAG, "bad animation '$line': ${it.message}") }.getOrDefault(cfg)
            }
            return cfg
        }
        for (i in 1..16) {
            val line = SystemProperties.get("${P}bezier.$i").trim()
            if (line.isEmpty()) continue
            cfg = runCatching { cfg.withBezier(line) }
                .onFailure { Log.w(TAG, "bad ${P}bezier.$i '$line': ${it.message}") }.getOrDefault(cfg)
        }
        for (i in 1..32) {
            val line = SystemProperties.get("${P}animation.$i").trim()
            if (line.isEmpty()) continue
            cfg = runCatching { cfg.withAnimation(line) }
                .onFailure { Log.w(TAG, "bad ${P}animation.$i '$line': ${it.message}") }
                .getOrDefault(cfg)
        }
        return cfg
    }

    private fun parseBool(s: String): Boolean = when (s.lowercase()) {
        "1", "true", "yes", "on" -> true
        "0", "false", "no", "off" -> false
        else -> throw IllegalArgumentException("not a boolean")
    }
}
