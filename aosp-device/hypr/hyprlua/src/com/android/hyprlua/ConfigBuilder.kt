package com.android.hyprlua

import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.Bind
import com.android.hyprtile.MouseBind
import com.android.hyprtile.MouseInput
import com.android.hyprtile.defaultMouseBinds
import com.android.hyprtile.CaptionMode
import com.android.hyprtile.AltTabMode
import com.android.hyprtile.DISPATCHERS
import com.android.hyprtile.parsePin
import com.android.hyprtile.clampZoom
import com.android.hyprtile.parseZoom
import com.android.hyprtile.parseWindow
import com.android.hyprtile.parseXY
import com.android.hyprtile.DecorationConfig
import com.android.hyprtile.Direction
import com.android.hyprtile.Gesture
import com.android.hyprtile.TouchGesture
import com.android.hyprtile.defaultTouchBinds
import com.android.hyprtile.gestureBinds
import com.android.hyprtile.Gradient
import com.android.hyprtile.Mod
import com.android.hyprtile.TileConfig
import com.android.hyprtile.WindowRule
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.androidKeyName
import com.android.hyprtile.defaultBinds
import com.android.hyprtile.parseColor
import com.android.hyprtile.parseKeys
import com.android.hyprtile.parseLayout
import com.android.hyprtile.parseMods
import com.android.hyprtile.DEFAULT_WORKSPACES
import com.android.hyprtile.MAX_WORKSPACES
import com.android.hyprtile.isWorkspaceArg
import com.android.hyprtile.CycleOptions
import com.android.hyprtile.WindowSelector
import com.android.hyprtile.previousWorkspaceArg
import com.android.hyprtile.specialName
import com.android.hyprtile.DEFAULT_DIM_SPECIAL
import java.text.SimpleDateFormat

/**
 * The configuration as IHyprShell.setConfig carries it: [values] under the `persist.hypr.*`
 * names without the prefix, [lists] of lines (`bezier`, `animation`, `binds`, `windowrule`,
 * `workspace`, `gesture`). Complete: every value is present, defaults filled in, so no
 * property set earlier leaks into a Lua configuration.
 */
data class WireConfig(val values: Map<String, String>, val lists: Map<String, List<String>>)

/** A module of the bar defined in Lua (`hl.bar_module`). */
data class BarModule(
    val name: String,
    /** Fixed text, or null when [textFn] gives it. */
    val text: String? = null,
    /** Id of the Lua function that returns the text (`__hl_call`). */
    val textFn: String? = null,
    /** How often [textFn] runs, ms. */
    val intervalMs: Long = 1000,
    /** Dispatcher on click: name and argument (`lua` + id for a function). */
    val click: Pair<String, String>? = null,
    val tooltip: String? = null,
)

/** The bar's layout and look (`hl.bar`). Colours are ARGB. */
data class BarConfig(
    val left: List<String> = listOf("apps", "workspaces"),
    val center: List<String> = listOf("title"),
    val right: List<String> = listOf("windows", "tray", "clock"),
    val clockFormat: String = "EEE d MMM  HH:mm",
    val heightDp: Int = 30,
    val background: Int = 0xE61E1E2E.toInt(),
    val foreground: Int = 0xFFCDD6F4.toInt(),
    val accent: Int = 0xFF33CCFF.toInt(),
    val modules: Map<String, BarModule> = emptyMap(),
)

/** What a configuration produces: the shell's part, the bar's part, and the warnings. */
data class HyprConfig(
    val wire: WireConfig,
    val bar: BarConfig,
    val workspaceRules: List<WorkspaceRule>,
    val warnings: List<String>,
)

/**
 * Turns the calls of the `hl` API (made through `__host` by the prelude) into a [HyprConfig].
 * Each call is checked when it is made, so its error carries the line of init.lua; options
 * follow Hyprland's names (`general.gaps_in`, `decoration.rounding`, ...), with this shell's
 * own under `shell.*`. Hyprland options it has no use for are accepted with a warning, so a
 * Hyprland configuration loads; unknown names are errors. Not thread-safe.
 *
 * [isKey] says whether Android knows a key name (KeyEvent on the device; tests pass a list).
 */
class ConfigBuilder(private val isKey: (androidName: String) -> Boolean) {
    /** Option values by Hyprland name, normalized. */
    private val values = LinkedHashMap<String, String>()
    private val beziers = LinkedHashMap<String, String>()
    private val animations = LinkedHashMap<String, String>()

    private class BindOp(val id: Int, val keys: Pair<Set<Mod>, String>?, val bind: Bind?) {
        var enabled = true
        var removed = false
    }

    private val bindOps = ArrayList<BindOp>()
    private var mouseMods: Set<Mod>? = null
    private val windowRules = LinkedHashMap<Int, Pair<WindowRule, Boolean>>()
    private val workspaceRules = LinkedHashMap<Int, Pair<WorkspaceRule, Boolean>>()
    private val gestures = ArrayList<Gesture>()
    private var bar = BarConfig()
    private val barModules = LinkedHashMap<String, BarModule>()
    private val warned = LinkedHashSet<String>()
    private var nextId = 1

    /** Set when something changed since the last [build] (a hook changing the configuration). */
    var dirty = true
        private set

    fun warn(message: String) {
        warned += message
    }

    /** `hl.config` leaf: [key] is the dotted Hyprland name, [luaType] the Lua type of the value. */
    fun set(key: String, value: String, luaType: String) {
        val opt = OPTIONS[key]
        if (opt == null) {
            if (key in HYPRLAND_OPTIONS) {
                warn("$key is not used by hypr on Android")
                return
            }
            fail("unknown option $key")
        }
        values[key] = try {
            opt(value, luaType)
        } catch (e: IllegalArgumentException) {
            fail("$key: ${e.message}")
        }
        dirty = true
    }

    /** Whether the configuration sets [key] itself (hyprctl getoption's `set`, E12). */
    fun isSet(key: String): Boolean = key in values

    /** The value of [key] as set or by default, for `hl.get_config`. */
    fun get(key: String): String? {
        values[key]?.let { return it }
        return if (key in OPTIONS) DEFAULTS[key] else null
    }

    fun curve(name: String, x0: String, y0: String, x1: String, y1: String) {
        require(name.isNotBlank() && !name.contains(',')) { "bad curve name '$name'" }
        val line = "$name, $x0, $y0, $x1, $y1"
        checked("curve $name") { AnimationConfig().withBezier(line) }
        beziers[name] = line
        dirty = true
    }

    fun spring(name: String) {
        warn("curve $name: spring curves are not supported, using easeOutQuint")
        curve(name, "0.23", "1", "0.32", "1")
    }

    fun animation(leaf: String, enabled: Boolean, speed: String?, curve: String?, style: String?) {
        if (leaf !in AnimationConfig.ANIMATIONS) {
            if (leaf in HYPRLAND_ANIMATIONS) {
                warn("animation $leaf is not played by hypr on Android")
                return
            }
            fail("unknown animation $leaf (${AnimationConfig.ANIMATIONS.sorted().joinToString()})")
        }
        if (enabled) {
            if (speed == null) fail("animation $leaf needs speed")
            if (curve != null && curve != "default" && curve !in beziers &&
                curve !in AnimationConfig.DEFAULT_BEZIERS) {
                fail("animation $leaf: unknown curve $curve (define it with hl.curve first)")
            }
        }
        val line = listOfNotNull(leaf, if (enabled) "1" else "0", speed ?: "1", curve ?: "default",
            style?.takeIf { it.isNotBlank() }).joinToString(", ")
        checked("animation $leaf") { AnimationConfig().withAnimation(line) }
        animations[leaf] = line
        dirty = true
    }

    /**
     * `hl.bind`. Returns the bind's id. Mouse binds (E6): `mouse:272`..`276` and the wheel
     * (`mouse_up`, `mouse_down`, `mouse_left`, `mouse_right`) with any modifiers and dispatcher;
     * `{ mouse = true }` with `hl.dsp.window.drag()`/`resize()` is Hyprland's `bindm`.
     */
    fun bind(keys: String, dispatcher: String, arg: String, mouse: Boolean, flags: String, description: String?): Int {
        val parsed = checked("bind '$keys'") { parseKeys(keys) }
        val (mods, key) = parsed
        MouseInput.parse(key)?.let { return mouseBind(keys, mods, it, dispatcher, arg, mouse, flags) }
        if (flags.isNotEmpty()) warn("bind '$keys': ${flags.replace(",", ", ")} not supported, ignored")
        if (TouchGesture.isGestureKey(key)) {
            if (mods.isNotEmpty()) fail("bind '$keys': gestures take no modifiers")
            val name = checked("bind '$keys'") { TouchGesture.nameOf(key) }
            if (dispatcher == "__drag" || dispatcher == "__resize") fail("bind '$keys': drag and resize are mouse binds")
            checkDispatcher(dispatcher, arg)?.let { fail("bind '$keys': $it") }
            val k = TouchGesture.key(name)
            return addOp(mods to k, Bind(mods, k, dispatcher, arg))
        }
        if (key.startsWith("MOUSE")) fail("bind '$keys': mouse keys are mouse:272..mouse:276, mouse_up, mouse_down, mouse_left, mouse_right")
        if (mouse) fail("bind '$keys': { mouse = true } binds a mouse button (mouse:272, mouse:273, mouse:274)")
        if (!isKey(androidKeyName(key))) fail("bind '$keys': unknown key $key")
        if (dispatcher == "__drag" || dispatcher == "__resize") {
            fail("bind '$keys': drag and resize are mouse binds (mouse:272, mouse:273)")
        }
        checkDispatcher(dispatcher, arg)?.let { fail("bind '$keys': $it") }
        return addOp(mods to key, Bind(mods, key, dispatcher, arg))
    }

    private fun mouseBind(keys: String, mods: Set<Mod>, input: MouseInput, dispatcher: String, arg: String,
                          mouse: Boolean, flags: String): Int {
        val flagSet = flags.split(',').filter { it.isNotEmpty() }.toSet()
        (flagSet - setOf("non_consuming", "repeating")).takeIf { it.isNotEmpty() }?.let {
            warn("bind '$keys': ${it.joinToString(", ")} not supported on mouse binds, ignored")
        }
        val drag = dispatcher == MouseBind.DRAG || dispatcher == MouseBind.RESIZE
        if (drag && !input.isPointerButton) {
            fail("bind '$keys': drag and resize take mouse:272, mouse:273 or mouse:274")
        }
        if (mouse && !drag) fail("bind '$keys': { mouse = true } takes hl.dsp.window.drag() or resize()")
        if (!drag) checkDispatcher(dispatcher, arg)?.let { fail("bind '$keys': $it") }
        if (!input.isPointerButton && !input.isWheel && "non_consuming" in flagSet) {
            warn("bind '$keys': side buttons arrive as BACK/FORWARD keys and are always consumed")
        }
        if (drag) mouseMods = mods
        val b = MouseBind(mods, input, dispatcher, arg,
            consuming = "non_consuming" !in flagSet, repeating = "repeating" in flagSet)
        return addOp(mods to input.key, b.toBind())
    }

    private fun addOp(keys: Pair<Set<Mod>, String>?, bind: Bind?): Int {
        val id = nextId++
        bindOps += BindOp(id, keys, bind)
        dirty = true
        return id
    }

    /** `hl.unbind`: removes the bind on these keys, a default one or one bound before. */
    fun unbind(keys: String) {
        addOp(checked("unbind '$keys'") { parseKeys(keys) }, null)
    }

    fun setBindEnabled(id: Int, enabled: Boolean) {
        bindOps.find { it.id == id }?.let { it.enabled = enabled; dirty = true }
    }

    fun isBindEnabled(id: Int) = bindOps.find { it.id == id }?.let { it.enabled && !it.removed } ?: false

    fun removeBind(id: Int) {
        bindOps.find { it.id == id }?.let { it.removed = true; dirty = true }
    }

    /** `hl.window_rule` from flattened key/value pairs; returns the rule's id. */
    fun windowRule(fields: Map<String, String>): Int {
        var rule = WindowRule()
        var enabled = true
        var effects = 0
        for ((k, v) in fields) {
            rule = when (k) {
                "name" -> rule.copy(name = v)
                "enabled" -> { enabled = bool(k, v); rule }
                "match.class", "match.initial_class" -> rule.copy(matchClass = regex(k, v))
                "match.title", "match.initial_title" -> rule.copy(matchTitle = regex(k, v))
                "float" -> { effects++; rule.copy(float = bool(k, v)) }
                "tile" -> { effects++; rule.copy(float = !bool(k, v)) }
                "workspace" -> {
                    effects++
                    val p = v.trim().split(Regex("\\s+"))
                    val special = specialName(p[0])
                    val n = if (special != null) null else p[0].toIntOrNull()?.takeIf { it in 1..MAX_WORKSPACES }
                        ?: fail("window rule: workspace must be a number 1..$MAX_WORKSPACES or special[:name], not '$v'")
                    if (p.size > 2 || (p.size == 2 && p[1] != "silent")) fail("window rule: workspace is 'N' or 'N silent'")
                    rule.copy(workspace = n, special = special, silent = p.size == 2)
                }
                "opacity" -> {
                    effects++
                    val nums = v.trim().split(Regex("\\s+")).filter { it != "override" }
                        .map { it.toFloatOrNull()?.takeIf { f -> f in 0f..1f } ?: fail("window rule: bad opacity '$v'") }
                    if (nums.isEmpty() || nums.size > 2) fail("window rule: opacity is 'ACTIVE [INACTIVE]'")
                    rule.copy(opacityActive = nums[0], opacityInactive = nums.getOrElse(1) { nums[0] })
                }
                else -> {
                    if (k.startsWith("match.")) fail("window rule: matching on ${k.removePrefix("match.")} is not supported (class, title)")
                    warn("window rule${if (rule.name.isEmpty()) "" else " ${rule.name}"}: $k is not supported, ignored")
                    rule
                }
            }
        }
        if (effects == 0) warn("window rule${if (rule.name.isEmpty()) "" else " ${rule.name}"} has no effect hypr supports")
        val id = nextId++
        windowRules[id] = rule to enabled
        dirty = true
        return id
    }

    /** `hl.workspace_rule`; returns the rule's id. */
    fun workspaceRule(fields: Map<String, String>): Int {
        val ws = fields["workspace"] ?: fail("workspace rule needs workspace =")
        val special = specialName(ws)
        val n = if (special != null) 0 else ws.trim().toIntOrNull()?.takeIf { it in 1..MAX_WORKSPACES }
            ?: fail("workspace rule: only numbered workspaces 1..$MAX_WORKSPACES and special[:name] are supported, not '$ws'")
        var rule = WorkspaceRule(n, special = special)
        var enabled = true
        for ((k, v) in fields) {
            rule = when (k) {
                "workspace" -> rule
                "enabled" -> { enabled = bool(k, v); rule }
                "default_name", "name" -> rule.copy(name = v)
                "layout" -> rule.copy(layout = checked("workspace rule layout") { parseLayout(v) })
                "gaps_in" -> rule.copy(gapsIn = pixels(k, v))
                "gaps_out" -> rule.copy(gapsOut = pixels(k, v))
                "on_created_empty" -> rule.copy(onCreatedEmpty = v.trim().ifEmpty { null })
                else -> { warn("workspace rule $n: $k is not supported, ignored"); rule }
            }
        }
        val id = nextId++
        workspaceRules[id] = rule to enabled
        dirty = true
        return id
    }

    fun setRuleEnabled(id: Int, enabled: Boolean) {
        windowRules[id]?.let { windowRules[id] = it.first to enabled; dirty = true }
        workspaceRules[id]?.let { workspaceRules[id] = it.first to enabled; dirty = true }
    }

    fun isRuleEnabled(id: Int) = windowRules[id]?.second ?: workspaceRules[id]?.second ?: false

    fun gesture(fields: Map<String, String>) {
        val fingers = fields["fingers"]?.toIntOrNull() ?: fail("gesture needs fingers =")
        val direction = fields["direction"] ?: fail("gesture needs direction =")
        val action = fields["action"] ?: fail("gesture needs action =")
        for (k in fields.keys - setOf("fingers", "direction", "action", "mods")) {
            warn("gesture: $k is not supported, ignored")
        }
        val mods = fields["mods"] ?: ""
        checked("gesture") { parseMods(mods) }
        val g = checked("gesture") { Gesture(fingers, direction, action, mods) }
        gestures += g
        // Played as gesture binds; a bind on the same gesture made later replaces it.
        for (b in checked("gesture") { gestureBinds(g) }) {
            checkDispatcher(b.dispatcher, b.arg)?.let { fail("gesture: $it") }
            addOp(b.mods to b.key, b)
        }
        dirty = true
    }

    fun monitor(fields: Map<String, String>) {
        for ((k, v) in fields) {
            when (k) {
                "output" -> {}
                "reserved", "reserved_area" -> {
                    val px = pixels(k, v)
                    values["monitor.reserved_top"] = "$px"
                    values["monitor.reserved_bottom"] = "$px"
                }
                "reserved.top", "reserved_area.top" -> values["monitor.reserved_top"] = "${pixels(k, v)}"
                "reserved.bottom", "reserved_area.bottom" -> values["monitor.reserved_bottom"] = "${pixels(k, v)}"
                else -> warn("monitor: $k is set by Android, ignored")
            }
        }
        dirty = true
    }

    fun bar(fields: Map<String, String>) {
        var b = bar
        for ((k, v) in fields) {
            b = when (k) {
                "modules_left" -> b.copy(left = modules(v))
                "modules_center" -> b.copy(center = modules(v))
                "modules_right" -> b.copy(right = modules(v))
                "clock_format" -> {
                    checked("bar clock_format") { SimpleDateFormat(v) }
                    b.copy(clockFormat = v)
                }
                "height" -> b.copy(heightDp = v.toIntOrNull()?.takeIf { it in 16..64 }
                    ?: fail("bar height must be 16..64 (dp)"))
                "background" -> b.copy(background = color(k, v))
                "foreground" -> b.copy(foreground = color(k, v))
                "accent" -> b.copy(accent = color(k, v))
                else -> fail("unknown bar option $k")
            }
        }
        bar = b
        dirty = true
    }

    fun barModule(
        name: String, text: String?, textFn: String?, interval: String?, clickName: String?,
        clickArg: String?, tooltip: String?,
    ) {
        if (!name.matches(Regex("[A-Za-z_][A-Za-z0-9_-]*"))) fail("bad bar module name '$name'")
        if (name in BUILTIN_MODULES) fail("bar module $name is built in")
        if (text == null && textFn == null) fail("bar module $name needs text =")
        val ms = interval?.let {
            it.toLongOrNull()?.takeIf { v -> v >= 250 } ?: fail("bar module $name: interval is 250 ms or more")
        } ?: 1000
        val click = clickName?.let {
            checkDispatcher(it, clickArg ?: "")?.let { e -> fail("bar module $name: $e") }
            it to (clickArg ?: "")
        }
        barModules[name] = BarModule(name, text, textFn, ms, click, tooltip)
        dirty = true
    }

    /** The whole configuration as of now; throws [LuaConfigException] on what only shows here. */
    fun build(): HyprConfig {
        dirty = false
        val v = values
        fun opt(key: String) = v[key] ?: DEFAULTS.getValue(key)
        val mainMod = parseMods(opt("shell.main_mod"))
        val terminal = opt("shell.terminal").ifEmpty { null }
        val fileManager = opt("shell.file_manager").ifEmpty { null }

        // Binds in the order they were made, over the defaults; a bind replaces one on the same
        // keys, an unbind removes it, and so does a disabled bind (its keys do nothing).
        val binds = LinkedHashMap<Pair<Set<Mod>, String>, Bind>()
        if (opt("shell.default_binds") == "true") {
            for (b in defaultBinds(mainMod, terminal, fileManager)) binds[b.mods to b.key] = b
            for (b in defaultTouchBinds()) binds[b.mods to b.key] = b
            for (b in defaultMouseBinds(mainMod)) binds[b.mods to b.input.key] = b.toBind()
        }
        for (op in bindOps) {
            if (op.removed || op.keys == null) continue
            // The same bind again (init.lua repeating a default) keeps its place.
            if (op.bind != null && op.enabled && binds[op.keys] == op.bind) continue
            binds.remove(op.keys)
            if (op.bind != null && op.enabled) binds[op.keys] = op.bind
        }
        if (binds.size > MAX_BINDS) warn("${binds.size} binds, only the first $MAX_BINDS are installed")
        // Workspaces past shell.workspaces do not exist: say which binds and rules go nowhere.
        val count = opt("shell.workspaces").toInt()
        for (b in binds.values) {
            val n = b.arg.trim().toIntOrNull() ?: continue
            if (b.dispatcher in WORKSPACE_DISPATCHERS && n > count) {
                warn("bind ${b.mods.sorted().joinToString(" ")} ${b.key}: workspace $n is past shell.workspaces = $count")
            }
        }
        for ((r, on) in windowRules.values) {
            if (on && (r.workspace ?: 0) > count) warn("window rule ${r.name}: workspace ${r.workspace} is past shell.workspaces = $count")
        }

        for (m in bar.left + bar.center + bar.right) {
            if (m !in BUILTIN_MODULES && m !in barModules) {
                throw LuaConfigException("bar: unknown module $m (built in: ${BUILTIN_MODULES.joinToString()}; or define it with hl.bar_module)")
            }
        }

        val wsRules = workspaceRules.values.filter { it.second }.map { it.first }
            .groupBy { it.workspace to it.special }.map { (_, rules) -> rules.reduce(::mergeWorkspaceRules) }

        val wire = LinkedHashMap<String, String>()
        for ((hypr, name) in WIRE_NAMES) wire[name] = opt(hypr)
        wire["mod"] = mainMod.sorted().joinToString(" ")
        wire["mouse_mod"] = (mouseMods ?: mainMod).sorted().joinToString(" ")
        wire["reserved_top"] = v["monitor.reserved_top"] ?: "0"
        wire["reserved_bottom"] = v["monitor.reserved_bottom"] ?: "0"
        wire["float_packages"] = ""
        // One scale for special workspaces: the active layout's section has it (E5).
        wire["special_scale_factor"] = opt(
            if (opt("general.layout") == "master") "master.special_scale_factor" else "dwindle.special_scale_factor")
        wire["source"] = "lua"

        val anim = AnimationConfig.DEFAULT_ANIMATIONS.mapValues { (k, s) ->
            listOfNotNull(k, if (s.enabled) "1" else "0", s.speed.toString(), s.curve,
                s.style.ifEmpty { null }).joinToString(", ")
        } + animations
        val curves = AnimationConfig.DEFAULT_BEZIERS.mapValues { (k, b) ->
            "$k, ${b.x1}, ${b.y1}, ${b.x2}, ${b.y2}"
        } + beziers
        val lists = mapOf(
            "bezier" to curves.values.toList(),
            "animation" to anim.values.toList(),
            "binds" to binds.values.take(MAX_BINDS).map { it.toString() },
            "windowrule" to windowRules.values.filter { it.second }.map { it.first.encode() },
            "workspace" to wsRules.map { it.encode() },
            "gesture" to gestures.map { it.encode() },
        )
        // Everything the shell will parse must parse.
        checked("configuration") {
            var a = AnimationConfig()
            lists.getValue("bezier").forEach { a = a.withBezier(it) }
            lists.getValue("animation").forEach { a = a.withAnimation(it) }
        }
        return HyprConfig(
            wire = WireConfig(wire, lists),
            bar = bar.copy(modules = LinkedHashMap(barModules)),
            workspaceRules = wsRules,
            warnings = warned.toList(),
        )
    }

    private fun mergeWorkspaceRules(a: WorkspaceRule, b: WorkspaceRule) = WorkspaceRule(
        workspace = a.workspace,
        name = b.name ?: a.name,
        layout = b.layout ?: a.layout,
        gapsIn = b.gapsIn ?: a.gapsIn,
        gapsOut = b.gapsOut ?: a.gapsOut,
        onCreatedEmpty = b.onCreatedEmpty ?: a.onCreatedEmpty,
        special = a.special,
    )

    private fun modules(v: String): List<String> =
        v.split(',').map { it.trim() }.filter { it.isNotEmpty() }

    companion object {
        private val WORKSPACE_DISPATCHERS = setOf("workspace", "movetoworkspace", "movetoworkspacesilent")
        /** HyprKeybinds' gesture slots. */
        const val MAX_BINDS = 256

        val BUILTIN_MODULES = setOf(
            "apps", "workspaces", "title", "windows", "tray", "notifications", "quicksettings", "clock",
        )

        /** The options this shell uses (`hyprctl descriptions`, E12). */
        val optionNames: Set<String> get() = OPTIONS.keys

        /** Hyprland's option names, accepted with a warning when unused. */
        val HYPRLAND_OPTIONS: Set<String> by lazy {
            ConfigBuilder::class.java.getResourceAsStream("hyprland-options.txt")!!
                .bufferedReader().readLines().map { it.trim() }
                .filter { it.isNotEmpty() && !it.startsWith("#") }.toSet()
        }

        /** Hyprland's animation leaves this shell does not play. */
        val HYPRLAND_ANIMATIONS = setOf(
            "layers", "layersIn", "layersOut", "fadeLayers", "fadeLayersIn", "fadeLayersOut",
            "fadePopups", "fadePopupsIn", "fadePopupsOut", "fadeDpms", "fadeShadow", "fadeDim",
            "borderangle", "specialWorkspace", "specialWorkspaceIn", "specialWorkspaceOut",
            "zoomFactor", "monitorAdded",
        )

        /** Hyprland name -> name on the wire (`persist.hypr.` + it is the property). */
        private val WIRE_NAMES = linkedMapOf(
            "general.layout" to "layout",
            "general.gaps_in" to "gaps_in",
            "general.gaps_out" to "gaps_out",
            "general.border_size" to "border_size",
            "general.col.active_border" to "col.active_border",
            "general.col.inactive_border" to "col.inactive_border",
            "decoration.rounding" to "rounding",
            "decoration.active_opacity" to "active_opacity",
            "decoration.inactive_opacity" to "inactive_opacity",
            "decoration.shadow.enabled" to "shadow",
            "animations.enabled" to "animations",
            "dwindle.split_width_multiplier" to "split_width_multiplier",
            "dwindle.default_split_ratio" to "default_split_ratio",
            "dwindle.preserve_split" to "preserve_split",
            "dwindle.force_split" to "force_split",
            "master.mfact" to "mfact",
            "master.new_status" to "new_status",
            "master.new_on_top" to "new_on_top",
            "shell.caption" to "caption",
            "ipc.allow" to "ipc_allow",
            "shell.tiling" to "tiling",
            "shell.touch" to "touch",
            "binds.workspace_back_and_forth" to "workspace_back_and_forth",
            "binds.allow_workspace_cycles" to "allow_workspace_cycles",
            "binds.pass_mouse_when_bound" to "pass_mouse_when_bound",
            "binds.scroll_event_delay" to "scroll_event_delay",
            "shell.workspaces" to "workspaces",
            "shell.alt_tab" to "alt_tab",
            "cursor.zoom_factor" to "zoom_factor",
            "decoration.dim_special" to "dim_special",
            "binds.hide_special_on_workspace_change" to "hide_special_on_workspace_change",
        )

        private val tile = TileConfig()
        private val deco = DecorationConfig()

        private val DEFAULTS: Map<String, String> = mapOf(
            "general.layout" to "dwindle",
            "general.gaps_in" to "${tile.gapsIn}",
            "general.gaps_out" to "${tile.gapsOut}",
            "general.border_size" to "${deco.borderSize}",
            "general.col.active_border" to deco.activeBorder.toString(),
            "general.col.inactive_border" to deco.inactiveBorder.toString(),
            "decoration.rounding" to "${deco.rounding}",
            "decoration.active_opacity" to "${deco.activeOpacity}",
            "decoration.inactive_opacity" to "${deco.inactiveOpacity}",
            "decoration.shadow.enabled" to "${deco.shadow}",
            "animations.enabled" to "true",
            "dwindle.split_width_multiplier" to "${tile.splitWidthMultiplier}",
            "dwindle.default_split_ratio" to "${tile.defaultSplitRatio}",
            "dwindle.preserve_split" to "${tile.preserveSplit}",
            "dwindle.force_split" to "${tile.forceSplit}",
            "master.mfact" to "${tile.masterFactor}",
            "master.new_status" to "slave",
            "master.new_on_top" to "${tile.newOnTop}",
            "shell.main_mod" to "ALT",
            "shell.terminal" to "",
            "ipc.allow" to "com.termux,com.android.hyprlauncher",
            "shell.file_manager" to "com.android.documentsui",
            "shell.default_binds" to "true",
            "shell.caption" to "floating",
            "shell.tiling" to "true",
            "shell.touch" to "true",
            "binds.workspace_back_and_forth" to "0",
            "binds.allow_workspace_cycles" to "false",
            "binds.pass_mouse_when_bound" to "false",
            "binds.scroll_event_delay" to "300",
            "shell.workspaces" to "$DEFAULT_WORKSPACES",
            "shell.alt_tab" to "switcher",
            "cursor.zoom_factor" to "1.0",
            "decoration.dim_special" to "$DEFAULT_DIM_SPECIAL",
            "binds.hide_special_on_workspace_change" to "false",
            "dwindle.special_scale_factor" to "1.0",
            "master.special_scale_factor" to "1.0",
        )

        private fun intIn(range: IntRange): (String, String) -> String = { v, t ->
            val n = v.toDoubleOrNull()?.takeIf { t == "number" && it == Math.floor(it) }?.toInt()
                ?: throw IllegalArgumentException("expected a whole number, got '$v'")
            require(n in range) { "must be in ${range.first}..${range.last}" }
            "$n"
        }

        private fun floatIn(lo: Float, hi: Float): (String, String) -> String = { v, t ->
            val f = v.toFloatOrNull()?.takeIf { t == "number" }
                ?: throw IllegalArgumentException("expected a number, got '$v'")
            require(f in lo..hi) { "must be in $lo..$hi" }
            "$f"
        }

        private val boolOpt: (String, String) -> String = { v, t ->
            when {
                t == "boolean" -> v
                t == "number" && (v == "0" || v == "1") -> if (v == "1") "true" else "false"
                else -> throw IllegalArgumentException("expected true or false, got '$v'")
            }
        }

        private val gradientOpt: (String, String) -> String = { v, _ -> Gradient.parse(v).toString() }

        private val OPTIONS: Map<String, (String, String) -> String> = mapOf(
            "general.layout" to { v, _ -> parseLayout(v).name.lowercase() },
            "general.gaps_in" to intIn(0..500),
            "general.gaps_out" to intIn(0..500),
            "general.border_size" to intIn(0..50),
            "general.col.active_border" to gradientOpt,
            "general.col.inactive_border" to gradientOpt,
            "decoration.rounding" to intIn(0..200),
            "decoration.active_opacity" to floatIn(0f, 1f),
            "decoration.inactive_opacity" to floatIn(0f, 1f),
            "decoration.shadow.enabled" to boolOpt,
            "animations.enabled" to boolOpt,
            "dwindle.split_width_multiplier" to floatIn(0.1f, 10f),
            "dwindle.default_split_ratio" to floatIn(0.1f, 1.9f),
            "dwindle.preserve_split" to boolOpt,
            // 0 (follow the mouse) splits as 2 here: new windows open after the focused one.
            "dwindle.force_split" to { v, t -> intIn(0..2)(v, t).let { if (it == "0") "2" else it } },
            "master.mfact" to floatIn(0.05f, 0.95f),
            "master.new_status" to { v, _ ->
                require(v in setOf("master", "slave", "inherit")) { "master, slave or inherit" }
                v
            },
            "master.new_on_top" to boolOpt,
            "shell.main_mod" to { v, _ ->
                val m = parseMods(v)
                require(m.isNotEmpty()) { "needs a modifier (ALT, SUPER, CTRL, SHIFT)" }
                m.sorted().joinToString(" ")
            },
            "shell.terminal" to { v, _ -> v.trim() },
            // hyprctl's sockets (E12, docs/hypr-ipc.md): app packages besides root, shell and system.
            "ipc.allow" to { v, _ -> v.split(',', ' ').map { it.trim() }.filter { it.isNotEmpty() }.joinToString(",") },
            "shell.file_manager" to { v, _ -> v.trim() },
            "shell.default_binds" to boolOpt,
            "shell.caption" to { v, _ -> CaptionMode.parse(v).name.lowercase() },
            "shell.tiling" to boolOpt,
            "shell.touch" to boolOpt,
            // Hyprland's own zoom (E10): 1 or more; the shell stops at MAX_ZOOM.
            "cursor.zoom_factor" to { v, t ->
                val f = v.toFloatOrNull()?.takeIf { t == "number" && it >= 1f }
                    ?: throw IllegalArgumentException("expected a number of 1 or more, got '$v'")
                "${clampZoom(f)}"
            },
            // 0 off, 1 (or true) back to the previous workspace, 2 the previous one on this display.
            "binds.workspace_back_and_forth" to { v, t ->
                if (t == "boolean") (if (v == "true") "1" else "0") else intIn(0..2)(v, t)
            },
            "binds.allow_workspace_cycles" to boolOpt,
            "binds.pass_mouse_when_bound" to boolOpt,
            "binds.scroll_event_delay" to intIn(0..2000),
            "shell.workspaces" to intIn(1..MAX_WORKSPACES),
            // What ALT + Tab does (E4): switcher, cycle, bind, overview or off.
            "shell.alt_tab" to { v, _ -> AltTabMode.parse(v).wireName },
            // Special workspaces (E5).
            "decoration.dim_special" to floatIn(0f, 1f),
            "binds.hide_special_on_workspace_change" to boolOpt,
            "dwindle.special_scale_factor" to floatIn(0.1f, 1f),
            "master.special_scale_factor" to floatIn(0.1f, 1f),
        )

        /** Why [arg] does not suit [dispatcher], or null when it does. */
        fun checkDispatcher(dispatcher: String, arg: String): String? {
            if (dispatcher !in DISPATCHERS) return "unknown dispatcher $dispatcher"
            val a = arg.trim()
            return when (dispatcher) {
                "workspace", "movetoworkspace", "movetoworkspacesilent" ->
                    if (previousWorkspaceArg(a) != null || specialName(a) != null) {
                        null
                    } else if ((a.toIntOrNull() ?: 1) > MAX_WORKSPACES) {
                        "workspaces go up to $MAX_WORKSPACES"
                    } else if (!isWorkspaceArg(a)) {
                        "bad workspace '$a'"
                    } else {
                        null
                    }
                "movefocus", "movewindow" -> if (Direction.parse(a) == null) "bad direction '$a'" else null
                "fullscreen" -> if (a !in setOf("", "0", "1")) "fullscreen takes 0 or 1" else null
                "exec" -> if (a.isEmpty()) "exec needs a package name" else null
                "execr" -> if (a.isEmpty()) "execr needs a shell command" else null
                "togglespecialworkspace" -> if (a.contains(' ')) "bad special workspace name '$a'" else null
                "focuswindow", "swapwindow" -> runCatching { WindowSelector.parse(a) }.exceptionOrNull()?.message
                "cyclenext", "cycleprev" ->
                    if (CycleOptions.parse(a) == null) "$dispatcher takes prev, tiled, floating, hist, visible" else null
                "swapnext" -> if (a.lowercase() !in setOf("", "next", "prev", "p", "b")) "swapnext takes prev or nothing" else null
                "alterzorder" -> {
                    val mode = a.substringBefore(',').trim()
                    val window = a.substringAfter(',', "").trim()
                    if (mode != "top" && mode != "bottom") {
                        "alterzorder takes top or bottom[,window]"
                    } else if (window.isNotEmpty()) {
                        runCatching { WindowSelector.parse(window) }.exceptionOrNull()?.message
                    } else {
                        null
                    }
                }
                "resizeactive", "moveactive" ->
                    if (parseXY(a) == null) "$dispatcher takes 'DX DY' or 'exact X Y'" else null
                "pin" -> if (parsePin(a) == null) "pin takes [toggle|on|off] [taskid:N]" else null
                "togglefloating", "setfloating", "settiled" ->
                    parseWindow(a).exceptionOrNull()?.message
                "zoom" -> if (parseZoom(a) == null) "zoom takes +D, -D (a change) or F (a factor)" else null
                else -> null
            }
        }
    }

    private fun fail(message: String): Nothing = throw LuaConfigException(message)

    private inline fun <T> checked(what: String, block: () -> T): T = try {
        block()
    } catch (e: LuaConfigException) {
        throw e
    } catch (e: RuntimeException) {
        fail("$what: ${e.message}")
    }

    private fun bool(key: String, v: String): Boolean = when (v) {
        "true", "1" -> true
        "false", "0" -> false
        else -> fail("$key: expected true or false, got '$v'")
    }

    private fun regex(key: String, v: String): String {
        checked(key) { Regex(v) }
        return v
    }

    private fun pixels(key: String, v: String): Int =
        v.toDoubleOrNull()?.takeIf { it == Math.floor(it) && it in 0.0..2000.0 }?.toInt()
            ?: fail("$key: expected pixels (a whole number), got '$v'")

    private fun color(key: String, v: String): Int = checked("bar $key") { parseColor(v) }
}

/** A configuration error found while checking a call of the `hl` API. */
class LuaConfigException(message: String) : RuntimeException(message)
