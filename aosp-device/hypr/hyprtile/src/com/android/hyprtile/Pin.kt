package com.android.hyprtile

/** What `pin`, `setfloating`-style dispatchers do with a window's state. */
enum class Switch {
    TOGGLE, ON, OFF;

    /** The new state of something that is [current] now. */
    fun apply(current: Boolean) = when (this) {
        TOGGLE -> !current
        ON -> true
        OFF -> false
    }

    companion object {
        fun parse(word: String): Switch? = when (word.lowercase()) {
            "toggle", "" -> TOGGLE
            "on", "enable", "1", "true" -> ON
            "off", "disable", "0", "false" -> OFF
            else -> null
        }
    }
}

/** A window a dispatcher names: `taskid:N` (Lua's `address`), `address:taskid:N`; null is the active one. */
fun parseWindow(word: String): Result<Int?> {
    val w = word.trim().lowercase()
    if (w.isEmpty() || w == "activewindow") return Result.success(null)
    val id = w.removePrefix("address:").removePrefix("taskid:").toIntOrNull()
    return if (id != null && w.removePrefix("address:").startsWith("taskid:")) {
        Result.success(id)
    } else {
        Result.failure(IllegalArgumentException("bad window '$word' (taskid:N)"))
    }
}

/**
 * The argument of Hyprland's `pin`: an optional action (`toggle`, `on`, `off`; toggle when
 * left out) and an optional window (`taskid:N`; the active window when left out), separated by
 * spaces or a comma, in either order. Null when it does not parse.
 */
fun parsePin(arg: String): Pair<Switch, Int?>? {
    var action: Switch? = null
    var window: Int? = null
    for (word in arg.split(' ', ',').map { it.trim() }.filter { it.isNotEmpty() }) {
        val s = Switch.parse(word)
        if (s != null && action == null) {
            action = s
            continue
        }
        val w = parseWindow(word)
        if (w.isFailure || window != null) return null
        window = w.getOrNull()
    }
    return (action ?: Switch.TOGGLE) to window
}

/**
 * One coordinate of `moveactive`/`resizeactive exact`: pixels, or a percentage of the work
 * area (its size, or for a position its origin plus that share of its size).
 */
data class Coord(val value: Int, val percent: Boolean = false) {
    /** The pixel value against a work area side starting at [origin] and [size] long. */
    fun resolve(size: Int, origin: Int = 0): Int =
        if (percent) origin + size * value / 100 else value
}

/**
 * The argument of `moveactive` and `resizeactive`: `DX DY` (relative pixels) or
 * `exact X Y` (display pixels, or `N%` of the work area). Returns whether it is exact and
 * the two coordinates; null when it does not parse (a percentage only goes with `exact`).
 */
fun parseXY(arg: String): Triple<Boolean, Coord, Coord>? {
    val words = arg.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
    val exact = words.firstOrNull()?.lowercase() == "exact"
    val rest = if (exact) words.drop(1) else words
    if (rest.size != 2) return null
    val coords = rest.map { w ->
        if (w.endsWith("%")) {
            if (!exact) return null
            Coord(w.dropLast(1).toIntOrNull()?.takeIf { it >= 0 } ?: return null, percent = true)
        } else {
            Coord(w.toDoubleOrNull()?.toInt() ?: return null)
        }
    }
    return Triple(exact, coords[0], coords[1])
}
