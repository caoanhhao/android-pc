package com.android.hyprtile

/** The most workspaces a display can have (Phase E2). */
const val MAX_WORKSPACES = 1000

/** Workspaces available when the configuration does not say (`shell.workspaces`). */
const val DEFAULT_WORKSPACES = 10

/**
 * Resolves a workspace argument as Hyprland does: `N`; `+N`/`-N` (and `r+N`/`r-N`) numeric, from
 * the [current] workspace; `e+N`/`e-N` stepping through the workspaces that exist ([existing],
 * with [current] always counted), in number order and wrapping around. Returns null for anything
 * else or a number outside 1..[max].
 */
fun resolveWorkspace(
    arg: String,
    current: Int,
    existing: Collection<Int> = emptyList(),
    max: Int = MAX_WORKSPACES,
): Int? {
    val a = arg.trim().lowercase()
    val n = when {
        a.startsWith("e+") || a.startsWith("e-") -> {
            val step = a.substring(1).toIntOrNull() ?: return null
            val list = (existing + current).filter { it in 1..max }.distinct().sorted()
            if (list.isEmpty()) return null
            val i = list.indexOf(current).coerceAtLeast(0)
            list[Math.floorMod(i + step, list.size)]
        }
        a.startsWith("+") || a.startsWith("-") -> a.toIntOrNull()?.let { current + it }
        a.startsWith("r+") || a.startsWith("r-") -> a.substring(1).toIntOrNull()?.let { current + it }
        else -> a.toIntOrNull()
    }
    return n?.takeIf { it in 1..max }
}

/** Whether [arg] is a workspace argument [resolveWorkspace] can take (any number up to 1000). */
fun isWorkspaceArg(arg: String): Boolean {
    val a = arg.trim().lowercase()
    return if (a.matches(Regex("[er]?[+-]\\d+"))) true else a.toIntOrNull()?.let { it in 1..MAX_WORKSPACES } == true
}

/**
 * The workspaces a bar (waybar's `hyprland/workspaces`) or the overview shows: the [active] one
 * (0: none) and every occupied one, in number order.
 */
fun shownWorkspaces(active: Int, occupied: Collection<Int>): List<Int> =
    (occupied + active).filter { it >= 1 }.distinct().sorted()

/**
 * Workspace numbers of one display's desks (Shell's multidesks). Hyprland names workspaces by
 * number and creates them on demand, so workspace 7 is a desk of its own, not the 7th desk:
 * desks get a number when they are made for one, or the lowest free number when the shell finds
 * one it did not make (Shell's default desk, desks restored at boot).
 */
class WorkspaceMap {
    private val byDesk = HashMap<Int, Int>()
    private val byNumber = java.util.TreeMap<Int, Int>()

    fun numberOf(deskId: Int): Int? = byDesk[deskId]
    fun deskOf(number: Int): Int? = byNumber[number]
    /** Numbers in use, in order. */
    val numbers: Set<Int> get() = byNumber.keys
    val size get() = byDesk.size

    /** Gives [deskId] workspace [number], taking it from any desk that had it. */
    fun put(deskId: Int, number: Int) {
        require(number in 1..MAX_WORKSPACES) { "workspace $number out of 1..$MAX_WORKSPACES" }
        byDesk.remove(deskId)?.let { byNumber.remove(it) }
        byNumber.put(number, deskId)?.let { byDesk.remove(it) }
        byDesk[deskId] = number
    }

    fun remove(deskId: Int) {
        byDesk.remove(deskId)?.let { byNumber.remove(it) }
    }

    /**
     * Brings the map in line with the desks that exist ([desks], in Shell's order): gone desks
     * lose their number, new ones get the lowest free one. Returns true when anything changed.
     */
    fun sync(desks: List<Int>): Boolean {
        var changed = false
        for (d in byDesk.keys.toList()) if (d !in desks) { remove(d); changed = true }
        for (d in desks) {
            if (d in byDesk) continue
            var n = 1
            while (n in byNumber) n++
            if (n > MAX_WORKSPACES) break
            put(d, n)
            changed = true
        }
        return changed
    }

    /** `desk:number` pairs, comma-separated, for keeping the numbers over a restart. */
    fun encode(): String = byDesk.entries.sortedBy { it.value }.joinToString(",") { "${it.key}:${it.value}" }

    companion object {
        /** Reads [encode]'s form; bad pairs are skipped. */
        fun decode(s: String): WorkspaceMap {
            val m = WorkspaceMap()
            for (p in s.split(',')) {
                val (d, n) = p.split(':').takeIf { it.size == 2 } ?: continue
                val desk = d.trim().toIntOrNull() ?: continue
                val number = n.trim().toIntOrNull()?.takeIf { it in 1..MAX_WORKSPACES } ?: continue
                m.put(desk, number)
            }
            return m
        }
    }
}
