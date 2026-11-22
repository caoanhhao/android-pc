package com.android.hyprtile

/**
 * Separates the `key=value` fields of a rule on the wire (IHyprShell.setConfig). A control
 * character, so no regular expression or name a user writes can contain it.
 */
const val RULE_FIELD_SEPARATOR = '\u001f'

private fun encodeFields(fields: List<Pair<String, String?>>): String =
    fields.filter { it.second != null }
        .joinToString(RULE_FIELD_SEPARATOR.toString()) { "${it.first}=${it.second}" }

private fun decodeFields(line: String): Map<String, String> =
    line.split(RULE_FIELD_SEPARATOR).filter { it.isNotEmpty() }.associate {
        val eq = it.indexOf('=')
        require(eq > 0) { "bad rule field $it" }
        it.substring(0, eq) to it.substring(eq + 1)
    }

private fun parseBoolField(s: String): Boolean = when (s.lowercase()) {
    "1", "true", "yes", "on" -> true
    "0", "false", "no", "off" -> false
    else -> throw IllegalArgumentException("not a boolean: $s")
}

/**
 * Hyprland's `windowrule`: windows whose package (Hyprland's `class`) and title match the
 * regular expressions (searched, so `firefox` matches `org.mozilla.firefox`; anchor with `^$`)
 * float or tile, open on a workspace, or get their own opacity. Fields left null do not apply.
 */
data class WindowRule(
    val name: String = "",
    /** Regular expression for the package name. */
    val matchClass: String? = null,
    /** Regular expression for the window title (the task label). */
    val matchTitle: String? = null,
    /** true: the window floats; false: it is tiled even if the float rules would float it. */
    val float: Boolean? = null,
    /** Workspace number the window opens on. */
    val workspace: Int? = null,
    /** Special workspace the window opens on (`workspace = "special:name"`), instead of [workspace]. */
    val special: String? = null,
    /** With [workspace] or [special]: move it there without following it (not shown). */
    val silent: Boolean = false,
    /** Opacity of the window when focused and when not. */
    val opacityActive: Float? = null,
    val opacityInactive: Float? = null,
) {
    private val classRegex by lazy { matchClass?.let { Regex(it) } }
    private val titleRegex by lazy { matchTitle?.let { Regex(it) } }

    init {
        matchClass?.let { Regex(it) }
        matchTitle?.let { Regex(it) }
        require(workspace == null || workspace >= 1) { "workspace must be 1 or more" }
        require(opacityActive == null || opacityActive in 0f..1f) { "opacity must be in 0..1" }
        require(opacityInactive == null || opacityInactive in 0f..1f) { "opacity must be in 0..1" }
    }

    fun matches(packageName: String?, title: String?): Boolean {
        classRegex?.let { if (packageName == null || !it.containsMatchIn(packageName)) return false }
        titleRegex?.let { if (title == null || !it.containsMatchIn(title)) return false }
        return true
    }

    fun encode(): String = encodeFields(listOf(
        "name" to name.ifEmpty { null },
        "class" to matchClass,
        "title" to matchTitle,
        "float" to float?.toString(),
        "workspace" to (special?.let { "special:$it" } ?: workspace?.toString())
            ?.let { if (silent) "$it silent" else it },
        "opacity" to opacityActive?.let { "$it ${opacityInactive ?: it}" },
    ))

    companion object {
        /** Reads a rule written by [encode]; throws on a bad field. */
        fun decode(line: String): WindowRule {
            val f = decodeFields(line)
            val ws = f["workspace"]?.trim()?.split(Regex("\\s+"))
            val opacity = f["opacity"]?.trim()?.split(Regex("\\s+"))?.map { it.toFloat() }
            return WindowRule(
                name = f["name"] ?: "",
                matchClass = f["class"],
                matchTitle = f["title"],
                float = f["float"]?.let(::parseBoolField),
                workspace = ws?.get(0)?.takeIf { specialName(it) == null }?.toInt(),
                special = ws?.get(0)?.let { specialName(it) },
                silent = ws?.getOrNull(1) == "silent",
                opacityActive = opacity?.getOrNull(0),
                opacityInactive = opacity?.let { it.getOrNull(1) ?: it[0] },
            )
        }
    }
}

/** The last rule matching the window that sets [pick], Hyprland's "later rules win". */
fun <T> List<WindowRule>.effect(packageName: String?, title: String?, pick: (WindowRule) -> T?): T? =
    lastOrNull { pick(it) != null && it.matches(packageName, title) }?.let(pick)

/**
 * Hyprland's `workspace` rule for one numbered (or special) workspace: its own layout and gaps,
 * a name the bar shows, and an app started when it is opened empty (`on_created_empty`).
 */
data class WorkspaceRule(
    /** The workspace number; 0 when the rule is for the special workspace [special]. */
    val workspace: Int,
    val name: String? = null,
    val layout: LayoutKind? = null,
    val gapsIn: Int? = null,
    val gapsOut: Int? = null,
    /** Package started when the workspace becomes active with no window on it. */
    val onCreatedEmpty: String? = null,
    /** The special workspace (`workspace = "special:name"`) the rule is for (E5). */
    val special: String? = null,
) {
    init {
        require(workspace >= 1 || (workspace == 0 && special != null)) { "workspace must be 1 or more" }
        require((gapsIn ?: 0) >= 0 && (gapsOut ?: 0) >= 0) { "gaps must not be negative" }
    }

    /** [base] with this rule's layout and gaps. */
    fun apply(base: TileConfig): TileConfig = base.copy(
        layout = layout ?: base.layout,
        gapsIn = gapsIn ?: base.gapsIn,
        gapsOut = gapsOut ?: base.gapsOut,
    )

    fun encode(): String = encodeFields(listOf(
        "workspace" to (special?.let { "special:$it" } ?: "$workspace"),
        "name" to name,
        "layout" to layout?.name?.lowercase(),
        "gaps_in" to gapsIn?.toString(),
        "gaps_out" to gapsOut?.toString(),
        "on_created_empty" to onCreatedEmpty,
    ))

    companion object {
        fun decode(line: String): WorkspaceRule {
            val f = decodeFields(line)
            val ws = requireNotNull(f["workspace"]) { "workspace rule without workspace" }
            val special = specialName(ws)
            return WorkspaceRule(
                workspace = if (special != null) 0 else ws.toInt(),
                special = special,
                name = f["name"],
                layout = f["layout"]?.let { parseLayout(it) },
                gapsIn = f["gaps_in"]?.toInt(),
                gapsOut = f["gaps_out"]?.toInt(),
                onCreatedEmpty = f["on_created_empty"],
            )
        }
    }
}

/** Hyprland's `general:layout` names. */
fun parseLayout(text: String): LayoutKind = when (text.trim().lowercase()) {
    "dwindle" -> LayoutKind.DWINDLE
    "master" -> LayoutKind.MASTER
    else -> throw IllegalArgumentException("unknown layout $text (dwindle, master)")
}

/**
 * A touch gesture, Hyprland's `gesture`: [fingers] fingers swiping in [direction] run [action]
 * (a dispatcher line, `workspace` for Hyprland's workspace swipe). Played as touch gesture binds
 * ([gestureBinds], Touch.kt).
 */
data class Gesture(val fingers: Int, val direction: String, val action: String, val mods: String = "") {
    init {
        require(fingers in 2..5) { "fingers must be 2..5" }
        require(direction in DIRECTIONS) { "unknown direction $direction (${DIRECTIONS.joinToString()})" }
        require(action.isNotBlank()) { "a gesture needs an action" }
    }

    fun encode(): String = encodeFields(listOf(
        "fingers" to "$fingers", "direction" to direction, "action" to action,
        "mods" to mods.ifEmpty { null },
    ))

    companion object {
        val DIRECTIONS = setOf(
            "left", "right", "up", "down", "horizontal", "vertical", "swipe",
            "pinch", "pinchin", "pinchout",
        )

        fun decode(line: String): Gesture {
            val f = decodeFields(line)
            return Gesture(
                fingers = requireNotNull(f["fingers"]).toInt(),
                direction = requireNotNull(f["direction"]),
                action = requireNotNull(f["action"]),
                mods = f["mods"] ?: "",
            )
        }
    }
}
