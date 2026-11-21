package com.android.hyprtile

/** A modifier key of a bind. SUPER is the Meta (Windows) key. */
enum class Mod { SHIFT, CTRL, ALT, SUPER }

/**
 * One keybind, Hyprland's `bind = MODS, key, dispatcher, arg`. [key] is a key name without the
 * modifiers (`Q`, `1`, `LEFT`, `RETURN`); the Shell side turns it into an Android key code.
 */
data class Bind(val mods: Set<Mod>, val key: String, val dispatcher: String, val arg: String = "") {
    override fun toString() =
        "${mods.sorted().joinToString(" ")}, $key, $dispatcher${if (arg.isEmpty()) "" else ", $arg"}"
}

/**
 * Parses modifier names as Hyprland writes them: separated by spaces, `_` or `+`, case-insensitive,
 * `SUPER`/`META`/`WIN`/`MOD4`, `ALT`/`MOD1`, `CTRL`/`CONTROL`, `SHIFT`. Unknown names throw.
 */
fun parseMods(text: String): Set<Mod> =
    text.split(' ', '_', '+').map { it.trim().uppercase() }.filter { it.isNotEmpty() }.map {
        when (it) {
            "SUPER", "META", "WIN", "MOD4" -> Mod.SUPER
            "ALT", "MOD1" -> Mod.ALT
            "CTRL", "CONTROL" -> Mod.CTRL
            "SHIFT" -> Mod.SHIFT
            else -> throw IllegalArgumentException("unknown modifier $it")
        }
    }.toSet()

/** Parses one `MODS, key, dispatcher[, arg]` line (the right-hand side of Hyprland's `bind =`). */
fun parseBind(line: String): Bind {
    val parts = line.split(',', limit = 4).map { it.trim() }
    require(parts.size >= 3) { "bind needs MODS, key, dispatcher: $line" }
    require(parts[1].isNotEmpty() && parts[2].isNotEmpty()) { "empty key or dispatcher: $line" }
    return Bind(parseMods(parts[0]), parts[1].uppercase(), parts[2].lowercase(), parts.getOrElse(3) { "" })
}

/**
 * The default binds, after Hyprland's example configuration with [mainMod] as `$mainMod`:
 * workspaces 1-9, movefocus/movewindow on the arrows, C killactive, V togglefloating,
 * J togglesplit, F maximize (`fullscreen 1`), SHIFT F fullscreen, E the file manager and,
 * when [terminal] is set, Q the terminal.
 */
fun defaultBinds(
    mainMod: Set<Mod>,
    terminal: String? = null,
    fileManager: String? = "com.android.documentsui",
): List<Bind> {
    val shift = mainMod + Mod.SHIFT
    val binds = ArrayList<Bind>()
    if (!terminal.isNullOrEmpty()) binds += Bind(mainMod, "Q", "exec", terminal)
    if (!fileManager.isNullOrEmpty()) binds += Bind(mainMod, "E", "exec", fileManager)
    binds += Bind(mainMod, "C", "killactive")
    binds += Bind(mainMod, "V", "togglefloating")
    binds += Bind(mainMod, "J", "togglesplit")
    binds += Bind(mainMod, "F", "fullscreen", "1")
    binds += Bind(shift, "F", "fullscreen", "0")
    for ((key, dir) in listOf("LEFT" to "l", "RIGHT" to "r", "UP" to "u", "DOWN" to "d")) {
        binds += Bind(mainMod, key, "movefocus", dir)
        binds += Bind(shift, key, "movewindow", dir)
    }
    for (n in 1..9) {
        binds += Bind(mainMod, "$n", "workspace", "$n")
        binds += Bind(shift, "$n", "movetoworkspace", "$n")
    }
    return binds
}

/**
 * Resolves a workspace argument (`3`, `e+1`, `e-1`, `+1`, `-1`) against the [current] workspace
 * number (1-based). Returns null for anything else or a result below 1.
 */
fun resolveWorkspace(arg: String, current: Int): Int? {
    val a = arg.trim().lowercase().removePrefix("e").removePrefix("r")
    val n = when {
        a.startsWith("+") || a.startsWith("-") -> a.toIntOrNull()?.let { current + it }
        else -> a.toIntOrNull()
    }
    return n?.takeIf { it >= 1 }
}
