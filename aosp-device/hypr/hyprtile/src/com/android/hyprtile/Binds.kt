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
 * workspaces 1-10 (0 is 10), movefocus/movewindow on the arrows, C killactive, V togglefloating,
 * J togglesplit, F maximize (`fullscreen 1`), SHIFT F fullscreen, E the file manager and,
 * when [terminal] is set, Q the terminal. D and SPACE open HyprLauncher's app launcher (rofi's
 * usual binds), GRAVE its overview (hyprexpo's). TAB goes back to the window focused before
 * (`focuscurrentorlast`, Phase E3; ALT + Tab reaches HyprLauncher as a recents key gesture).
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
    binds += Bind(mainMod, "D", "launcher")
    binds += Bind(mainMod, "SPACE", "launcher")
    binds += Bind(mainMod, "GRAVE", "overview")
    binds += Bind(mainMod, "TAB", "focuscurrentorlast")
    binds += Bind(mainMod, "C", "killactive")
    binds += Bind(mainMod, "V", "togglefloating")
    binds += Bind(mainMod, "J", "togglesplit")
    binds += Bind(mainMod, "F", "fullscreen", "1")
    binds += Bind(shift, "F", "fullscreen", "0")
    for ((key, dir) in listOf("LEFT" to "l", "RIGHT" to "r", "UP" to "u", "DOWN" to "d")) {
        binds += Bind(mainMod, key, "movefocus", dir)
        binds += Bind(shift, key, "movewindow", dir)
    }
    for (n in 1..10) {
        val key = "${n % 10}"
        binds += Bind(mainMod, key, "workspace", "$n")
        binds += Bind(shift, key, "movetoworkspace", "$n")
    }
    return binds
}

/**
 * Parses the keys of a bind as Hyprland's Lua configuration writes them, `SUPER + SHIFT + Q`
 * (modifiers and the key joined by `+`), or as its old configuration did, `SUPER SHIFT, Q`.
 * Returns the modifiers and the key name, upper case. Throws on an unknown modifier or no key.
 */
fun parseKeys(text: String): Pair<Set<Mod>, String> {
    val comma = text.indexOf(',')
    val (mods, key) = if (comma >= 0) {
        text.substring(0, comma) to text.substring(comma + 1).trim()
    } else {
        val parts = text.split('+').map { it.trim() }.filter { it.isNotEmpty() }
        require(parts.isNotEmpty()) { "no key in '$text'" }
        parts.dropLast(1).joinToString(" ") to parts.last()
    }
    require(key.isNotEmpty() && !key.contains(' ')) { "bad key '$key' in '$text'" }
    return parseMods(mods) to key.uppercase()
}

/**
 * Android's key code name (without `KEYCODE_`) for a key as Hyprland configurations name it
 * (xkb names, any case): `LEFT` is `DPAD_LEFT`, `RETURN` is `ENTER`, `HOME` is `MOVE_HOME`, ...;
 * names Android shares (`Q`, `1`, `F5`, `TAB`, `SPACE`, `GRAVE`) and `KEYCODE_` names pass through.
 */
fun androidKeyName(key: String): String {
    val k = key.uppercase()
    return KEY_ALIASES[k] ?: k.removePrefix("KEYCODE_")
}

private val KEY_ALIASES = mapOf(
    "LEFT" to "DPAD_LEFT", "RIGHT" to "DPAD_RIGHT", "UP" to "DPAD_UP", "DOWN" to "DPAD_DOWN",
    "RETURN" to "ENTER", "KP_ENTER" to "NUMPAD_ENTER", "ESC" to "ESCAPE",
    "PRIOR" to "PAGE_UP", "NEXT" to "PAGE_DOWN", "PAGEUP" to "PAGE_UP", "PAGEDOWN" to "PAGE_DOWN",
    "DELETE" to "FORWARD_DEL", "BACKSPACE" to "DEL", "PRINT" to "SYSRQ",
    "HOME" to "MOVE_HOME", "END" to "MOVE_END",
    "EQUAL" to "EQUALS", "BRACKETLEFT" to "LEFT_BRACKET", "BRACKETRIGHT" to "RIGHT_BRACKET",
    // The keypad as xkb names it with Num Lock off (the host's KP_End..KP_Insert binds) and on.
    "KP_END" to "NUMPAD_1", "KP_DOWN" to "NUMPAD_2", "KP_NEXT" to "NUMPAD_3", "KP_PAGE_DOWN" to "NUMPAD_3",
    "KP_LEFT" to "NUMPAD_4", "KP_BEGIN" to "NUMPAD_5", "KP_RIGHT" to "NUMPAD_6", "KP_HOME" to "NUMPAD_7",
    "KP_UP" to "NUMPAD_8", "KP_PRIOR" to "NUMPAD_9", "KP_PAGE_UP" to "NUMPAD_9", "KP_INSERT" to "NUMPAD_0",
    "KP_DELETE" to "NUMPAD_DOT", "KP_DECIMAL" to "NUMPAD_DOT", "KP_ADD" to "NUMPAD_ADD",
    "KP_SUBTRACT" to "NUMPAD_SUBTRACT", "KP_MULTIPLY" to "NUMPAD_MULTIPLY", "KP_DIVIDE" to "NUMPAD_DIVIDE",
    "KP_0" to "NUMPAD_0", "KP_1" to "NUMPAD_1", "KP_2" to "NUMPAD_2", "KP_3" to "NUMPAD_3", "KP_4" to "NUMPAD_4",
    "KP_5" to "NUMPAD_5", "KP_6" to "NUMPAD_6", "KP_7" to "NUMPAD_7", "KP_8" to "NUMPAD_8", "KP_9" to "NUMPAD_9",
    "SUPER_L" to "META_LEFT", "SUPER_R" to "META_RIGHT", "CAPS_LOCK" to "CAPS_LOCK",
)

/** The dispatchers the shell and HyprLauncher run (Hyprland's names, `lua` for Lua functions). */
val DISPATCHERS = setOf(
    "focuscurrentorlast", "focusurgentorlast", "cyclenext", "cycleprev", "swapnext", "swapwindow",
    "alterzorder",
    "workspace", "movetoworkspace", "movetoworkspacesilent", "movefocus", "movewindow",
    "togglefloating", "togglesplit", "fullscreen", "killactive", "exec", "focuswindow",
    "resizeactive", "launcher", "overview", "hyprexpo:expo", "lua",
    "notifications", "quicksettings",
    "pin", "setfloating", "settiled", "moveactive",
)
