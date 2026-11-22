package com.android.hyprtile

/**
 * Hyprland's window focus history (Phase E3, docs/hypr-desktop.md), one per display: window
 * ids, most recently focused first, across all workspaces of the display. A window moved to
 * another workspace keeps its place; one moved to another display goes with its place to that
 * display's history ([moveToDisplay]). Windows that are gone are dropped by [retain].
 */
class FocusHistory {
    private val byDisplay = HashMap<Int, ArrayList<Int>>()

    /** [window] got the focus on [display]: it goes to the front. */
    fun focused(display: Int, window: Int) {
        remove(window)
        byDisplay.getOrPut(display) { ArrayList() }.add(0, window)
    }

    /** A window that opened without the focus: last, as Hyprland's `openEarly` track. */
    fun opened(display: Int, window: Int) {
        if (byDisplay.values.any { window in it }) return
        byDisplay.getOrPut(display) { ArrayList() }.add(window)
    }

    fun remove(window: Int) {
        for (list in byDisplay.values) list.remove(window)
    }

    /** [window] now lives on [display]: first there if it was first where it came from, else last. */
    fun moveToDisplay(window: Int, display: Int) {
        val from = byDisplay.entries.find { window in it.value }?.key ?: return
        if (from == display) return
        val wasFirst = byDisplay.getValue(from).firstOrNull() == window
        remove(window)
        val list = byDisplay.getOrPut(display) { ArrayList() }
        list.add(if (wasFirst) 0 else list.size, window)
    }

    /** Keeps only the windows [alive] says exist. */
    fun retain(alive: (Int) -> Boolean) {
        for (list in byDisplay.values) list.retainAll(alive)
    }

    /** The history of [display], most recent first. */
    fun of(display: Int): List<Int> = byDisplay[display]?.toList() ?: emptyList()

    /** Every display's history. */
    fun displays(): Set<Int> = byDisplay.keys.toSet()

    /**
     * Hyprland's `focuscurrentorlast`: the window before [focused] when [focused] is the most
     * recent one, else the most recent one (nothing focused, or the desktop). [usable] filters
     * windows that cannot take the focus now.
     */
    fun currentOrLast(display: Int, focused: Int?, usable: (Int) -> Boolean = { true }): Int? {
        val list = of(display).filter(usable)
        return if (focused != null && list.firstOrNull() == focused) list.getOrNull(1) else list.firstOrNull()
    }
}

/**
 * Hyprland 0.57's workspace history: one most-recently-used list of every display's
 * workspaces (`workspace previous` is the entry after the current one). A workspace is
 * (display, number).
 */
class WorkspaceHistory {
    data class Entry(val display: Int, val workspace: Int)

    private val list = ArrayList<Entry>()

    /**
     * [workspace] became active on [display]. With [allowCycles] false Hyprland ignores an
     * activation of the workspace already in front; with the list being most-recently-used
     * the two are the same here, as they are in Hyprland 0.57.
     */
    fun track(display: Int, workspace: Int, allowCycles: Boolean = false) {
        val e = Entry(display, workspace)
        if (list.firstOrNull() == e && !allowCycles) return
        list.remove(e)
        list.add(0, e)
    }

    /** `previous`: the workspace before [current] (or the latest one when [current] is unknown). */
    fun previous(current: Entry): Entry? {
        val i = list.indexOf(current)
        return if (i < 0) list.firstOrNull() else list.getOrNull(i + 1)
    }

    /** `previous_per_monitor`: the workspace before [current] that was on [current]'s display. */
    fun previousOnDisplay(current: Entry): Entry? {
        val i = list.indexOf(current)
        return list.drop(i + 1).firstOrNull { it.display == current.display }
    }

    /** Forgets workspaces of displays that are gone, or numbers past [count] of a display. */
    fun retain(keep: (Entry) -> Boolean) {
        list.retainAll(keep)
    }

    fun entries(): List<Entry> = list.toList()
}

/**
 * Options of `cyclenext` (Hyprland's old syntax, also `hl.dsp.window.cycle_next`): `prev`,
 * `tiled`, `floating`, `hist` (in focus history order), `visible` (all visible workspaces, here
 * the active one of every display).
 */
data class CycleOptions(
    val next: Boolean = true,
    val tiled: Boolean = false,
    val floating: Boolean = false,
    val hist: Boolean = false,
    val visible: Boolean = false,
) {
    /** The tiled/floating filter: null for both, true for floating only, false for tiled only. */
    val floatingOnly: Boolean? get() = if (tiled == floating) null else floating

    override fun toString() = listOfNotNull(
        "prev".takeIf { !next }, "tiled".takeIf { tiled }, "floating".takeIf { floating },
        "hist".takeIf { hist }, "visible".takeIf { visible },
    ).joinToString(" ")

    companion object {
        /** Words separated by spaces or commas; `next`, `b`/`prev`/`p` as Hyprland. Null if unknown. */
        fun parse(arg: String): CycleOptions? {
            var o = CycleOptions()
            for (w in arg.lowercase().split(' ', ',').map { it.trim() }.filter { it.isNotEmpty() }) {
                o = when (w) {
                    "next" -> o.copy(next = true)
                    "prev", "p", "b" -> o.copy(next = false)
                    "tiled" -> o.copy(tiled = true)
                    "floating" -> o.copy(floating = true)
                    "hist" -> o.copy(hist = true)
                    "visible" -> o.copy(visible = true)
                    else -> return null
                }
            }
            return o
        }
    }
}

/**
 * The window after [current] in [order] (before it with [next] false) that [accept] takes,
 * wrapping around; the way Hyprland's `cyclenext` walks its window list. Never [current]
 * itself. Without [current] in [order] the first accepted window from the start (the end).
 */
fun <K> cycle(order: List<K>, current: K?, next: Boolean, accept: (K) -> Boolean): K? {
    val list = if (next) order else order.asReversed()
    val i = current?.let { list.indexOf(it) } ?: -1
    val after = list.drop(i + 1) + list.take(maxOf(i, 0))
    return after.firstOrNull { it != current && accept(it) }
}

/** What a [WindowSelector] looks at. */
data class WindowFacts(
    val id: Int,
    /** The app's package (Hyprland's class). */
    val cls: String,
    val title: String,
    val pid: Int = -1,
    val floating: Boolean = false,
    val focused: Boolean = false,
    /** On the focused display's active workspace. */
    val onActiveWorkspace: Boolean = false,
)

/**
 * Hyprland's window selectors (`focuswindow`, `hl.dsp.focus({ window = ... })`, the `window`
 * field of window dispatchers): `class:`, `initialclass:` (the package), `title:`,
 * `initialtitle:` (regular expressions, searched as window rules are; `negative:` inverts),
 * `pid:`, `address:0x<hex task id>`, `stableid:`/`taskid:` (the task id, also a bare number),
 * `activewindow`, `floating` and `tiled` (the first such window on the active workspace).
 */
sealed class WindowSelector {
    abstract fun matches(w: WindowFacts): Boolean

    data class Id(val id: Int) : WindowSelector() {
        override fun matches(w: WindowFacts) = w.id == id
    }

    data class Pid(val pid: Int) : WindowSelector() {
        override fun matches(w: WindowFacts) = w.pid == pid
    }

    class Pattern(val field: String, val regex: Regex, val negative: Boolean) : WindowSelector() {
        override fun matches(w: WindowFacts): Boolean {
            val text = if (field == "class") w.cls else w.title
            return regex.containsMatchIn(text) != negative
        }
        override fun equals(other: Any?) = other is Pattern && other.field == field &&
            other.regex.pattern == regex.pattern && other.negative == negative
        override fun hashCode() = regex.pattern.hashCode()
        override fun toString() = "$field:${if (negative) "negative:" else ""}${regex.pattern}"
    }

    object Active : WindowSelector() {
        override fun matches(w: WindowFacts) = w.focused
    }

    data class Kind(val floating: Boolean) : WindowSelector() {
        override fun matches(w: WindowFacts) = w.onActiveWorkspace && w.floating == floating
    }

    /** The first window of [windows] (in their order) this selector picks. */
    fun pick(windows: List<WindowFacts>): WindowFacts? = windows.firstOrNull { matches(it) }

    companion object {
        /** Parses a selector; throws IllegalArgumentException with the reason. */
        fun parse(text: String): WindowSelector {
            val t = text.trim()
            require(t.isNotEmpty()) { "empty window selector" }
            t.toIntOrNull()?.let { return Id(it) }
            when (t.lowercase()) {
                "activewindow" -> return Active
                "floating" -> return Kind(true)
                "tiled" -> return Kind(false)
            }
            val colon = t.indexOf(':')
            require(colon > 0) { "bad window selector '$t'" }
            val kind = t.substring(0, colon).lowercase()
            val value = t.substring(colon + 1)
            return when (kind) {
                "taskid", "stableid" -> Id(value.trim().toIntOrNull() ?: throw IllegalArgumentException("bad id in '$t'"))
                "address" -> Id(value.trim().removePrefix("taskid:").let { v ->
                    if (v.startsWith("0x") || v.startsWith("0X")) v.substring(2).toIntOrNull(16) else v.toIntOrNull()
                } ?: throw IllegalArgumentException("bad address in '$t'"))
                "pid" -> Pid(value.trim().toIntOrNull() ?: throw IllegalArgumentException("bad pid in '$t'"))
                "class", "initialclass", "title", "initialtitle" -> {
                    val negative = value.startsWith("negative:")
                    val pattern = value.removePrefix("negative:")
                    val regex = runCatching { Regex(pattern) }.getOrElse {
                        throw IllegalArgumentException("bad regex in '$t': ${it.message}")
                    }
                    Pattern(if (kind.endsWith("class")) "class" else "title", regex, negative)
                }
                "tag" -> throw IllegalArgumentException("window tags are not supported ('$t')")
                else -> throw IllegalArgumentException("unknown window selector '$kind:'")
            }
        }
    }
}

/**
 * A workspace argument that names the previous workspace: `previous` (also `prev`), or
 * `previous_per_monitor`. Returns null for anything else, true for the per-monitor one.
 */
fun previousWorkspaceArg(arg: String): Boolean? = when (arg.trim().lowercase()) {
    "previous", "prev" -> false
    "previous_per_monitor" -> true
    else -> null
}
