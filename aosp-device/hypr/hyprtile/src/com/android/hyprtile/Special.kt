package com.android.hyprtile

/** Layout key of Hyprland's first special workspace; later names count up from it (-98, ...). */
const val SPECIAL_KEY_BASE = -99

/** Hyprland's `decoration:dim_special` default: how dark the workspace behind a special one gets. */
const val DEFAULT_DIM_SPECIAL = 0.2f

/**
 * The special workspace a workspace argument names: `special` is `special:special` (Hyprland's
 * unnamed scratchpad), `special:name` is `name`. Null when [arg] is not a special workspace.
 */
fun specialName(arg: String): String? {
    val a = arg.trim()
    if (a.equals("special", ignoreCase = true)) return "special"
    if (!a.startsWith("special:", ignoreCase = true)) return null
    return a.substring(8).trim().ifEmpty { "special" }
}

/**
 * Hyprland's special workspaces (scratchpads) of all displays (Phase E5): named workspaces shown
 * as an overlay above a display's active workspace and toggled on and off without leaving it.
 * Each special workspace has its own tiling layout under a negative [keyOf] key in [HyprTile]
 * (normal workspaces are keyed by desk id, never negative). At most one special workspace is
 * shown per display, as in Hyprland. Pure bookkeeping: the Shell side moves the windows.
 */
class SpecialWorkspaces {
    private val members = LinkedHashMap<Int, String>()
    private val shown = HashMap<Int, String>()
    private val keys = LinkedHashMap<String, Int>()

    /** The layout key of special workspace [name], given on first use and kept. */
    fun keyOf(name: String): Int = keys.getOrPut(name) {
        (keys.values.minOrNull()?.minus(1) ?: SPECIAL_KEY_BASE)
    }

    /** The name behind a layout key, or null when [key] is no special workspace's. */
    fun nameOf(key: Int): String? = keys.entries.firstOrNull { it.value == key }?.key

    fun isSpecialKey(key: Int) = nameOf(key) != null

    /** The special workspace window [id] belongs to, if any. */
    fun of(id: Int): String? = members[id]

    fun isMember(id: Int) = id in members

    /** Windows of special workspace [name], oldest first. */
    fun windows(name: String): List<Int> = members.filterValues { it == name }.keys.toList()

    /** Special workspaces with windows, in the order they got their keys. */
    fun occupied(): List<String> = keys.keys.filter { n -> members.containsValue(n) }

    /** Every window of a special workspace. */
    fun allWindows(): Set<Int> = members.keys

    /** The special workspace shown on [displayId], or null. */
    fun shownOn(displayId: Int): String? = shown[displayId]

    /** Displays showing a special workspace. */
    fun shownDisplays(): Set<Int> = shown.keys

    /** Whether window [id] is in the special workspace shown on [displayId]. */
    fun isShown(id: Int, displayId: Int) = members[id]?.let { it == shown[displayId] } == true

    /**
     * Hyprland's `togglespecialworkspace [name]` on [displayId]: hides [name] when it is the
     * one shown, else shows it (in place of another one shown there). Returns the special
     * workspace shown afterwards, or null.
     */
    fun toggle(displayId: Int, name: String): String? {
        if (shown[displayId] == name) {
            shown -= displayId
            return null
        }
        show(displayId, name)
        return name
    }

    fun show(displayId: Int, name: String) {
        keyOf(name)
        // A special workspace is shown on one display at a time.
        shown.entries.removeAll { it.value == name }
        shown[displayId] = name
    }

    fun hide(displayId: Int) {
        shown -= displayId
    }

    /** Window [id] moves to special workspace [name]; returns the one it was on before. */
    fun add(id: Int, name: String): String? {
        keyOf(name)
        return members.put(id, name)
    }

    /** Window [id] left its special workspace (moved out or closed); returns that workspace. */
    fun remove(id: Int): String? = members.remove(id)

    /** Keeps only the windows for which [alive] holds. */
    fun retain(alive: (Int) -> Boolean) {
        members.keys.retainAll(alive)
    }

    /**
     * Where `movetoworkspace special:name` takes window [id] on [displayId]: Hyprland moves a
     * window that is already on that special workspace back to the active workspace (null);
     * otherwise to [name].
     */
    fun moveTarget(id: Int, name: String): String? = if (members[id] == name) null else name

    /** Memberships and keys for SharedPreferences: `name=key:id,id;...`. */
    fun encode(): String = keys.entries.joinToString(";") { (name, key) ->
        "${name.replace(";", "").replace("=", "")}=$key:" + windows(name).joinToString(",")
    }

    companion object {
        /** Reads [encode]'s text; bad parts are skipped. */
        fun decode(text: String): SpecialWorkspaces {
            val s = SpecialWorkspaces()
            for (part in text.split(';')) {
                val eq = part.indexOf('=')
                val colon = part.indexOf(':', eq + 1)
                if (eq <= 0 || colon < 0) continue
                val name = part.substring(0, eq)
                val key = part.substring(eq + 1, colon).toIntOrNull() ?: continue
                if (key > SPECIAL_KEY_BASE || key in s.keys.values) continue
                s.keys[name] = key
                part.substring(colon + 1).split(',').mapNotNull { it.trim().toIntOrNull() }
                    .forEach { s.members[it] = name }
            }
            return s
        }
    }
}

/**
 * The work area of a special workspace: Hyprland's `special_scale_factor` shrinks it around its
 * centre (1 keeps it whole).
 */
fun specialArea(area: Box, scale: Float): Box {
    if (scale >= 1f) return area
    val w = (area.width * scale).toInt()
    val h = (area.height * scale).toInt()
    val left = area.left + (area.width - w) / 2
    val top = area.top + (area.height - h) / 2
    return Box(left, top, left + w, top + h)
}
