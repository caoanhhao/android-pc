package com.android.hyprtile

/**
 * What ALT + Tab does (Phase E4, `shell.alt_tab`, docs/hypr-desktop.md "ALT + Tab switcher").
 * InputManager keeps ALT + Tab for its recents gesture, so HyprLauncher, its handler, owns the
 * key on the real PC; under Waydroid the host's compositor takes it first.
 */
enum class AltTabMode {
    /** Hold ALT and press Tab to walk the focus history in an overlay; releasing ALT commits. */
    SWITCHER,
    /** Every Tab while ALT is held runs `cyclenext`, which also raises (the host's `alterzorder top`). */
    CYCLE,
    /** Every Tab while ALT is held runs the configuration's bind on `ALT + Tab` (Phase E3). */
    BIND,
    /** Phase D4: the overview on the next window, focused when ALT is released. */
    OVERVIEW,
    /** ALT + Tab does nothing. */
    OFF;

    val wireName get() = name.lowercase()

    companion object {
        fun parse(text: String): AltTabMode =
            entries.firstOrNull { it.wireName == text.trim().lowercase() }
                ?: throw IllegalArgumentException(
                    "alt_tab is one of ${entries.joinToString { "\"${it.wireName}\"" }}, not '$text'")
    }
}

/**
 * One ALT press of the switcher: the windows in the order the switcher lists them and the one
 * selected. The order is the focus history (most recent first, Phase E3) followed by windows
 * the history does not know yet. The first Tab selects the window focused before the current
 * one, exactly what `focuscurrentorlast` focuses, so a quick ALT + Tab is that dispatcher; with
 * nothing focused (the desktop in front) it selects the most recent window. Further Tabs go
 * deeper, SHIFT + Tab goes back, both wrapping.
 */
class AltTabSession(history: List<Int>, windows: List<Int>, focused: Int) {
    var order: List<Int> = altTabOrder(history, windows)
        private set
    var selected: Int = when {
        order.isEmpty() -> -1
        order.size > 1 && order[0] == focused -> 1
        else -> 0
    }
        private set

    val selectedTask: Int? get() = order.getOrNull(selected)

    fun step(forward: Boolean) {
        if (order.isEmpty()) return
        selected = Math.floorMod(selected + if (forward) 1 else -1, order.size)
    }

    /** Selects [taskId] (a click on it); false when it is not listed. */
    fun select(taskId: Int): Boolean {
        val i = order.indexOf(taskId)
        if (i >= 0) selected = i
        return i >= 0
    }

    /**
     * The windows changed while the switcher is up: the order is kept for the windows still
     * there (a list that reorders under the user would be hard to follow), new ones join at the
     * end, and the selection stays on its window, or on the place it had when that one closed.
     */
    fun update(windows: List<Int>) {
        val keep = selectedTask
        val live = windows.toSet()
        val next = order.filter { it in live } + windows.filter { it !in order }
        selected = when {
            next.isEmpty() -> -1
            keep != null && keep in live -> next.indexOf(keep)
            else -> selected.coerceIn(0, next.size - 1)
        }
        order = next
    }
}

/** The switcher's order: [history] (most recent first) for the open [windows], then the rest. */
fun altTabOrder(history: List<Int>, windows: List<Int>): List<Int> {
    val open = windows.toSet()
    val known = history.filter { it in open }.distinct()
    return known + windows.filter { it !in known }.distinct()
}
