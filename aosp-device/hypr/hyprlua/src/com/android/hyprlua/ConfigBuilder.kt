package com.android.hyprlua

import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.Bind
import com.android.hyprtile.CaptionMode
import com.android.hyprtile.DISPATCHERS
import com.android.hyprtile.DecorationConfig
import com.android.hyprtile.Direction
import com.android.hyprtile.Gesture
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
import com.android.hyprtile.resolveWorkspace
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
     * `hl.bind`. Returns the bind's id. Mouse binds (`mouse:272` left, `273` right, `274`
     * middle) set the modifier of the mouse binds, whose actions are fixed (drag, resize, float).
     */
    fun bind(keys: String, dispatcher: String, arg: String, mouse: Boolean, flags: String, description: String?): Int {
        val parsed = checked("bind '$keys'") { parseKeys(keys) }
        val (mods, key) = parsed
        if (flags.isNotEmpty()) warn("bind '$keys': ${flags.replace(",", ", ")} not supported, ignored")
        if (key.startsWith("MOUSE:") || mouse) {
            val expected = when (key) {
                "MOUSE:272" -> "__drag"
                "MOUSE:273" -> "__resize"
                "MOUSE:274" -> "togglefloating"
                else -> fail("bind '$keys': mouse binds are mouse:272, mouse:273 or mouse:274")
            }
            if (dispatcher != expected) {
                fail("bind '$keys': this mouse button only takes " + when (expected) {
                    "__drag" -> "hl.dsp.window.drag()"
                    "__resize" -> "hl.dsp.window.resize()"
                    else -> "hl.dsp.window.float()"
                })
            }
            if (mods.isEmpty()) fail("bind '$keys': mouse binds need a modifier")
            if (mouseMods != null && mouseMods != mods) {
                warn("mouse binds use one modifier; ${mods.joinToString(" ")} replaces ${mouseMods!!.joinToString(" ")}")
            }
            mouseMods = mods
            dirty = true
            return addOp(null, null)
        }
        if (key == "MOUSE_DOWN" || key == "MOUSE_UP" || key == "MOUSE_LEFT" || key == "MOUSE_RIGHT") {
            warn("bind '$keys': wheel binds are not supported (scroll over the bar's workspaces)")
            return addOp(null, null)
        }
        if (!isKey(androidKeyName(key))) fail("bind '$keys': unknown key $key")
        if (dispatcher == "__drag" || dispatcher == "__resize") {
            fail("bind '$keys': drag and resize are mouse binds (mouse:272, mouse:273)")
        }
        checkDispatcher(dispatcher, arg)?.let { fail("bind '$keys': $it") }
        return addOp(mods to key, Bind(mods, key, dispatcher, arg))
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
                    val n = p[0].toIntOrNull()?.takeIf { it in 1..MAX_WORKSPACE }
                        ?: fail("window rule: workspace must be a number 1..$MAX_WORKSPACE, not '$v'")
                    if (p.size > 2 || (p.size == 2 && p[1] != "silent")) fail("window rule: workspace is 'N' or 'N silent'")
                    rule.copy(workspace = n, silent = p.size == 2)
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
        val n = ws.trim().toIntOrNull()?.takeIf { it in 1..MAX_WORKSPACE }
            ?: fail("workspace rule: only numbered workspaces 1..$MAX_WORKSPACE are supported, not '$ws'")
        var rule = WorkspaceRule(n)
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
        gestures += checked("gesture") { Gesture(fingers, direction, action, mods) }
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
        }
        for (op in bindOps) {
            if (op.removed || op.keys == null) continue
            binds.remove(op.keys)
            if (op.bind != null && op.enabled) binds[op.keys] = op.bind
        }
        if (binds.size > MAX_BINDS) warn("${binds.size} binds, only the first $MAX_BINDS are installed")

        for (m in bar.left + bar.center + bar.right) {
            if (m !in BUILTIN_MODULES && m !in barModules) {
                throw LuaConfigException("bar: unknown module $m (built in: ${BUILTIN_MODULES.joinToString()}; or define it with hl.bar_module)")
            }
        }

        val wsRules = workspaceRules.values.filter { it.second }.map { it.first }
            .groupBy { it.workspace }.map { (_, rules) -> rules.reduce(::mergeWorkspaceRules) }

        val wire = LinkedHashMap<String, String>()
        for ((hypr, name) in WIRE_NAMES) wire[name] = opt(hypr)
        wire["mod"] = mainMod.sorted().joinToString(" ")
        wire["mouse_mod"] = (mouseMods ?: mainMod).sorted().joinToString(" ")
        wire["reserved_top"] = v["monitor.reserved_top"] ?: "0"
        wire["reserved_bottom"] = v["monitor.reserved_bottom"] ?: "0"
        wire["float_packages"] = ""
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
    )

    private fun modules(v: String): List<String> =
        v.split(',').map { it.trim() }.filter { it.isNotEmpty() }

    companion object {
        const val MAX_WORKSPACE = 9
        /** HyprKeybinds' gesture slots. */
        const val MAX_BINDS = 128

        val BUILTIN_MODULES = setOf(
            "apps", "workspaces", "title", "windows", "tray", "notifications", "quicksettings", "clock",
        )

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
            "shell.tiling" to "tiling",
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
            "shell.file_manager" to "com.android.documentsui",
            "shell.default_binds" to "true",
            "shell.caption" to "floating",
            "shell.tiling" to "true",
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
            "shell.file_manager" to { v, _ -> v.trim() },
            "shell.default_binds" to boolOpt,
            "shell.caption" to { v, _ -> CaptionMode.parse(v).name.lowercase() },
            "shell.tiling" to boolOpt,
        )

        /** Why [arg] does not suit [dispatcher], or null when it does. */
        fun checkDispatcher(dispatcher: String, arg: String): String? {
            if (dispatcher !in DISPATCHERS) return "unknown dispatcher $dispatcher"
            val a = arg.trim()
            return when (dispatcher) {
                "workspace", "movetoworkspace", "movetoworkspacesilent" ->
                    if (resolveWorkspace(a, 1) == null && !a.matches(Regex("[er]?[+-]\\d+"))) {
                        "bad workspace '$a'"
                    } else if ((a.toIntOrNull() ?: 1) > MAX_WORKSPACE) {
                        "workspaces go up to $MAX_WORKSPACE"
                    } else {
                        null
                    }
                "movefocus", "movewindow" -> if (Direction.parse(a) == null) "bad direction '$a'" else null
                "fullscreen" -> if (a !in setOf("", "0", "1")) "fullscreen takes 0 or 1" else null
                "exec" -> if (a.isEmpty()) "exec needs a package name" else null
                "focuswindow" -> if (a.removePrefix("taskid:").toIntOrNull() == null) "bad window '$a'" else null
                "resizeactive" -> if (a.split(Regex("\\s+")).size != 2 ||
                    a.split(Regex("\\s+")).any { it.toIntOrNull() == null }) "resizeactive takes 'DX DY'" else null
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
