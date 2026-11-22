package com.android.hyprtile

/**
 * What a mouse bind is bound to: Hyprland's `mouse:272` (left) to `mouse:276` (forward side
 * button, BTN_EXTRA) and the wheel, `mouse_up`/`mouse_down` and its tilt `mouse_left`/
 * `mouse_right`. [key] is the name in a bind, upper case.
 */
enum class MouseInput(val key: String) {
    LEFT("MOUSE:272"),
    RIGHT("MOUSE:273"),
    MIDDLE("MOUSE:274"),
    BACK("MOUSE:275"),
    FORWARD("MOUSE:276"),
    WHEEL_UP("MOUSE_UP"),
    WHEEL_DOWN("MOUSE_DOWN"),
    WHEEL_LEFT("MOUSE_LEFT"),
    WHEEL_RIGHT("MOUSE_RIGHT");

    val isWheel get() = this == WHEEL_UP || this == WHEEL_DOWN || this == WHEEL_LEFT || this == WHEEL_RIGHT

    /** Buttons Android delivers as motion events (the side buttons come as BACK/FORWARD keys). */
    val isPointerButton get() = this == LEFT || this == RIGHT || this == MIDDLE

    /** One letter for the input dispatcher's list of bound wheel directions. */
    val wheelLetter: Char? get() = when (this) {
        WHEEL_UP -> 'u'
        WHEEL_DOWN -> 'd'
        WHEEL_LEFT -> 'l'
        WHEEL_RIGHT -> 'r'
        else -> null
    }

    companion object {
        /** The input a key names (`MOUSE:272`, `mouse_down`, ...), or null for other keys. */
        fun parse(key: String): MouseInput? {
            val k = key.uppercase()
            return entries.firstOrNull { it.key == k }
        }

        /**
         * The wheel direction of a scroll with these axis values (Android's AXIS_VSCROLL, positive
         * away from the user, and AXIS_HSCROLL, positive to the right); vertical wins.
         */
        fun wheel(vscroll: Float, hscroll: Float): MouseInput? = when {
            vscroll > 0f -> WHEEL_UP
            vscroll < 0f -> WHEEL_DOWN
            hscroll < 0f -> WHEEL_LEFT
            hscroll > 0f -> WHEEL_RIGHT
            else -> null
        }
    }
}

/**
 * A mouse bind. [dispatcher] is a Hyprland dispatcher, `lua`, or [DRAG]/[RESIZE] for Hyprland's
 * `bindm` (`{ mouse = true }`: the window follows the pointer while the button is held).
 * [consuming] false is `non_consuming` (the app gets the event too); [repeating] on a wheel bind
 * fires at every wheel step, without `binds.scroll_event_delay`.
 */
data class MouseBind(
    val mods: Set<Mod>,
    val input: MouseInput,
    val dispatcher: String,
    val arg: String = "",
    val consuming: Boolean = true,
    val repeating: Boolean = false,
) {
    val isDragOrResize get() = dispatcher == DRAG || dispatcher == RESIZE

    /**
     * As a line of the bind list the shell gets (a [Bind] whose key carries the flags after a
     * `/`: `n` non-consuming, `e` repeating).
     */
    fun toBind(): Bind {
        val flags = (if (consuming) "" else "N") + (if (repeating) "E" else "")
        return Bind(mods, if (flags.isEmpty()) input.key else "${input.key}/$flags", dispatcher, arg)
    }

    companion object {
        const val DRAG = "__drag"
        const val RESIZE = "__resize"

        /** The mouse bind a [Bind] of the bind list stands for, or null for a key or gesture bind. */
        fun from(bind: Bind): MouseBind? {
            val slash = bind.key.indexOf('/')
            val input = MouseInput.parse(if (slash < 0) bind.key else bind.key.substring(0, slash)) ?: return null
            val flags = if (slash < 0) "" else bind.key.substring(slash + 1).uppercase()
            return MouseBind(bind.mods, input, bind.dispatcher, bind.arg,
                consuming = 'N' !in flags, repeating = 'E' in flags)
        }

        /** Whether a key of the bind list is a mouse key (with or without flags). */
        fun isMouseKey(key: String) = MouseInput.parse(key.substringBefore('/')) != null
    }
}

/**
 * Hyprland's example mouse binds on [mod]: `bindm` left drags (movewindow), right resizes
 * (resizewindow); middle toggles floating of the window under the pointer (Phase D3).
 */
fun defaultMouseBinds(mod: Set<Mod>): List<MouseBind> = listOf(
    MouseBind(mod, MouseInput.LEFT, MouseBind.DRAG),
    MouseBind(mod, MouseInput.RIGHT, MouseBind.RESIZE),
    MouseBind(mod, MouseInput.MIDDLE, "togglefloating"),
)

/**
 * The mouse binds in force, matched as Hyprland does: the modifiers must be exactly the bind's.
 * [passMouseWhenBound] is `binds.pass_mouse_when_bound` (the app gets bound events too);
 * [scrollDelayMs] is `binds.scroll_event_delay` (a wheel bind fires at most once per this time,
 * unless it is repeating).
 */
class MouseBinds(
    binds: List<MouseBind>,
    val passMouseWhenBound: Boolean = false,
    val scrollDelayMs: Long = 300,
) {
    private val byInput: Map<Pair<Set<Mod>, MouseInput>, MouseBind> =
        binds.associateBy { it.mods to it.input }
    private val lastFired = HashMap<Pair<Set<Mod>, MouseInput>, Long>()

    val binds: Collection<MouseBind> get() = byInput.values

    fun match(mods: Set<Mod>, input: MouseInput): MouseBind? = byInput[mods to input]

    /** Whether the bound event goes to the app as well. */
    fun passes(bind: MouseBind) = passMouseWhenBound || !bind.consuming

    /**
     * Whether a wheel step at [nowMs] fires [bind] (a step inside `scroll_event_delay` of the last
     * one that fired is still consumed, but does nothing). Records the step when it fires.
     */
    fun fires(bind: MouseBind, nowMs: Long): Boolean {
        if (!bind.input.isWheel || bind.repeating || scrollDelayMs <= 0) return true
        val k = bind.mods to bind.input
        val last = lastFired[k]
        if (last != null && nowMs - last in 0 until scrollDelayMs) return false
        lastFired[k] = nowMs
        return true
    }

    /**
     * The bound wheel directions the input dispatcher keeps from the apps
     * (`sys.hypr.wheel_binds`): `MODS:dir` entries joined by `,`, MODS the letters `S` `C` `A` `M`
     * (shift, ctrl, alt, meta) in that order (`-` for none), dir `u`/`d`/`l`/`r`. Passed binds are
     * left out.
     */
    fun wheelFilter(): String = byInput.values.filter { it.input.isWheel && !passes(it) }
        .map { "${modLetters(it.mods)}:${it.input.wheelLetter}" }.sorted().joinToString(",")

    companion object {
        /** The bind list's mouse binds, or the defaults on [defaultMod] when it has none. */
        fun fromBindList(binds: List<Bind>?, defaultMod: Set<Mod>, pass: Boolean = false, delayMs: Long = 300) =
            MouseBinds(binds?.mapNotNull { MouseBind.from(it) } ?: defaultMouseBinds(defaultMod), pass, delayMs)

        fun modLetters(mods: Set<Mod>): String = buildString {
            if (Mod.SHIFT in mods) append('S')
            if (Mod.CTRL in mods) append('C')
            if (Mod.ALT in mods) append('A')
            if (Mod.SUPER in mods) append('M')
        }.ifEmpty { "-" }
    }
}
