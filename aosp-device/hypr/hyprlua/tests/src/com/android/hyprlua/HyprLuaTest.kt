package com.android.hyprlua

import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.Bind
import com.android.hyprtile.Gradient
import com.android.hyprtile.LayoutKind
import com.android.hyprtile.Mod
import com.android.hyprtile.WindowRule
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.parseBind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** A [LuaHost] that records what the runtime asks for; timers run when [runTimers] is called. */
class FakeHost(val files: Map<String, String> = emptyMap()) : LuaHost {
    val dispatched = ArrayList<Pair<String, String>>()
    val notes = ArrayList<String>()
    val logs = ArrayList<String>()
    val changed = ArrayList<HyprConfig>()
    val timers = LinkedHashMap<Any, Pair<Long, Runnable>>()
    var snap = Snapshot(1, 2, 7, listOf(
        SnapWindow(7, 1, "com.android.settings", "Settings", false, false, 12, 45, 1000, 900),
        SnapWindow(9, 2, "org.mozilla.firefox", "Mozilla Firefox", true, false),
    ), mapOf(2 to "web"))

    override fun readModule(path: String) = files[path]
    override fun dispatch(name: String, arg: String) { dispatched += name to arg }
    override fun notify(text: String, timeoutMs: Long, color: Int?) { notes += text }
    override fun snapshot() = snap
    override fun log(message: String) { logs += message }
    override fun schedule(delayMs: Long, task: Runnable): Any = Any().also { timers[it] = delayMs to task }
    override fun cancel(token: Any) { timers.remove(token) }
    override fun configChanged(config: HyprConfig) { changed += config }

    fun runTimers() {
        val due = timers.toList()
        timers.clear()
        due.forEach { it.second.second.run() }
    }
}

class HyprLuaTest {
    private val keys = setOf("Q", "C", "V", "J", "F", "E", "D", "SPACE", "GRAVE", "ENTER", "B",
        "DPAD_LEFT", "DPAD_RIGHT", "DPAD_UP", "DPAD_DOWN", "TAB", "SLASH", "T") + (0..9).map { "$it" }
    /** KeyEvent's names (without KEYCODE_) that the default binds.lua uses. */
    private val androidKeys = keys + ('A'..'Z').map { "$it" } + (1..12).map { "F$it" } +
        (0..9).map { "NUMPAD_$it" } + listOf("NUMPAD_ENTER", "ESCAPE", "EQUALS", "MINUS")
    private val host = FakeHost()
    private val lua = HyprLua(host) { it in keys }

    private fun load(src: String, h: HyprLua = lua): HyprConfig = when (val r = h.load(src)) {
        is HyprLua.Result.Loaded -> r.config
        is HyprLua.Result.Failed -> throw AssertionError("load failed: ${r.message}")
    }

    private fun error(src: String): String = when (val r = lua.load(src)) {
        is HyprLua.Result.Loaded -> throw AssertionError("expected an error")
        is HyprLua.Result.Failed -> r.message
    }

    private fun binds(c: HyprConfig) = c.wire.lists.getValue("binds").map { parseBind(it) }

    @Test
    fun emptyConfigGivesTheDefaults() {
        val c = load("")
        val v = c.wire.values
        assertEquals("dwindle", v["layout"])
        assertEquals("5", v["gaps_in"])
        assertEquals("12", v["gaps_out"])
        assertEquals("2", v["border_size"])
        assertEquals(Gradient.parse("rgba(33ccffee) rgba(00ff99ee) 45deg"), Gradient.parse(v.getValue("col.active_border")))
        assertEquals("10", v["rounding"])
        assertEquals("ALT", v["mod"])
        assertEquals("ALT", v["mouse_mod"])
        assertEquals("none", v["caption"])
        assertEquals("lua", v["source"])
        assertTrue(Bind(setOf(Mod.ALT), "1", "workspace", "1") in binds(c))
        assertTrue(Bind(setOf(Mod.ALT), "E", "exec", "com.android.documentsui") in binds(c))
        var a = AnimationConfig()
        c.wire.lists.getValue("bezier").forEach { a = a.withBezier(it) }
        c.wire.lists.getValue("animation").forEach { a = a.withAnimation(it) }
        assertEquals(AnimationConfig().resolve("windowsIn"), a.resolve("windowsIn"))
        assertEquals(AnimationConfig().curve("windows"), a.curve("windows"))
    }

    @Test
    fun theDefaultInitLuaLoadsWithoutWarnings() {
        val c = load(HyprLua.DEFAULT_INIT, HyprLua(FakeHost(HyprLua.DEFAULT_MODULES)) { it in androidKeys })
        assertEquals(emptyList<String>(), c.warnings)
        val v = c.wire.values
        assertEquals("ALT", v["mod"])
        assertEquals("20", v["workspaces"])
        assertEquals("switcher", v["alt_tab"])
        assertTrue(Bind(setOf(Mod.ALT), "Q", "killactive") in binds(c))
        assertTrue(Bind(setOf(Mod.ALT, Mod.CTRL), "5", "movetoworkspace", "5") in binds(c))
    }

    @Test
    fun theDefaultInitLuaNeedsItsModules() {
        assertTrue(error(HyprLua.DEFAULT_INIT).contains("module 'binds' not found"))
    }

    @Test
    fun generalAndDecorationMapToTheShellNames() {
        val c = load("""
            hl.config({
              general = { gaps_in = 0, gaps_out = 30, border_size = 4, layout = "master",
                col = { active_border = { colors = { "rgba(ff0000ff)", "#00ff00" }, angle = 90 },
                        inactive_border = 0xff112233 } },
              decoration = { rounding = 0, active_opacity = 0.9, inactive_opacity = 0.75,
                shadow = { enabled = false } },
              animations = { enabled = false },
              dwindle = { preserve_split = true, default_split_ratio = 1.2, force_split = 1 },
              master = { mfact = 0.6, new_status = "master", new_on_top = true },
              shell = { main_mod = "SUPER", caption = "floating", default_binds = false },
            })
        """)
        val v = c.wire.values
        assertEquals("master", v["layout"])
        assertEquals("0", v["gaps_in"])
        assertEquals("30", v["gaps_out"])
        assertEquals("4", v["border_size"])
        assertEquals(Gradient(listOf(0xffff0000.toInt(), 0xff00ff00.toInt()), 90), Gradient.parse(v.getValue("col.active_border")))
        assertEquals(Gradient(listOf(0xff112233.toInt())), Gradient.parse(v.getValue("col.inactive_border")))
        assertEquals("0", v["rounding"])
        assertEquals("0.9", v["active_opacity"])
        assertEquals("0.75", v["inactive_opacity"])
        assertEquals("false", v["shadow"])
        assertEquals("false", v["animations"])
        assertEquals("true", v["preserve_split"])
        assertEquals("1.2", v["default_split_ratio"])
        assertEquals("1", v["force_split"])
        assertEquals("0.6", v["mfact"])
        assertEquals("master", v["new_status"])
        assertEquals("true", v["new_on_top"])
        assertEquals("SUPER", v["mod"])
        assertEquals("floating", v["caption"])
        assertEquals(emptyList<String>(), c.wire.lists["binds"])
    }

    @Test
    fun unusedHyprlandOptionsWarnUnknownOnesFail() {
        val c = load("hl.config({ decoration = { blur = { enabled = true, size = 3 } }, input = { kb_layout = 'us' } })")
        assertTrue(c.warnings.toString(), c.warnings.any { "decoration.blur.size" in it })
        assertTrue(c.warnings.any { "input.kb_layout" in it })
        assertEquals("init.lua:2: unknown option general.gapz_in", error("\nhl.config({ general = { gapz_in = 3 } })"))
    }

    @Test
    fun badValuesReportTheLine() {
        assertEquals("init.lua:3: general.gaps_in: expected a whole number, got 'wide'",
            error("local x = 1\n\nhl.config({ general = { gaps_in = 'wide' } })"))
        assertTrue(error("hl.config({ decoration = { active_opacity = 2 } })").startsWith("init.lua:1: decoration.active_opacity: must be in"))
        assertTrue(error("hl.config({ general = { col = { active_border = 'blue' } } })").contains("bad colour"))
        assertTrue(error("hl.config({ general = { layout = 'scrolling' } })").contains("unknown layout"))
        assertTrue(error("hl.config({ shell = { main_mod = 'HYPER' } })").contains("unknown modifier"))
    }

    @Test
    fun syntaxAndRuntimeErrorsCarryTheLine() {
        assertTrue(error("hl.config({\n  general = {\n").startsWith("init.lua:"))
        assertEquals("init.lua:2: attempt to call a nil value (global 'nope')", error("\nnope()"))
        assertTrue(error("hl.frobnicate()").startsWith("init.lua:1: hl.frobnicate does not exist"))
        assertTrue(error("hl.bind('ALT + P', hl.dsp.window.pseudo())").startsWith("init.lua:1: hl.dsp.window.pseudo is not supported"))
    }

    @Test
    fun curvesAndAnimations() {
        val c = load("""
            hl.curve("snappy", { type = "bezier", points = { {0.05, 0.9}, {0.1, 1.05} } })
            hl.bezier("old", 0.1, 0.2, 0.3, 0.4)
            hl.animation({ leaf = "windowsIn", speed = 3, bezier = "snappy", style = "popin 60%" })
            hl.animation("workspaces", { speed = 5, curve = "old", style = "slidevert" })
            hl.animation({ leaf = "fadeOut", enabled = false })
            hl.animation({ leaf = "layers", speed = 3, bezier = "snappy" })
            hl.animation({ leaf = "fadeLayers", speed = 3, bezier = "snappy" })
        """)
        var a = AnimationConfig()
        c.wire.lists.getValue("bezier").forEach { a = a.withBezier(it) }
        c.wire.lists.getValue("animation").forEach { a = a.withAnimation(it) }
        assertEquals(0.9f, a.beziers.getValue("snappy").y1, 1e-6f)
        assertEquals(3f, a.resolve("windowsIn").speed)
        assertEquals("snappy", a.resolve("windowsIn").curve)
        assertEquals("popin 60%", a.resolve("windowsIn").style)
        assertEquals("slidevert", a.resolve("workspacesIn").style)
        assertEquals("old", a.resolve("workspacesOut").curve)
        assertFalse(a.resolve("fadeOut").enabled)
        assertEquals(3f, a.resolve("layers").speed)
        assertTrue(c.warnings.any { "fadeLayers" in it })
        assertTrue(error("hl.animation({ leaf = 'windows', speed = 3, bezier = 'nope' })").contains("unknown curve nope"))
        assertTrue(error("hl.animation({ leaf = 'wobble', speed = 3 })").contains("unknown animation wobble"))
        assertTrue(error("hl.animation({ leaf = 'windowsIn', speed = 3, style = 'spin' })").contains("windowsIn"))
    }

    @Test
    fun bindsOverTheDefaults() {
        val c = load("""
            local mod = "SUPER"
            hl.config({ shell = { main_mod = mod, terminal = "com.termux" } })
            hl.bind(mod .. " + Q", hl.dsp.window.close())
            hl.bind(mod .. " + left", hl.dsp.focus({ direction = "left" }))
            hl.bind(mod .. " + SHIFT + 3", hl.dsp.window.move({ workspace = 3, follow = false }))
            hl.bind(mod .. " + Return", hl.dsp.exec_cmd("com.termux"))
            hl.bind("CTRL ALT, T", hl.dsp.raw("exec", "org.example.term"))
            hl.bind(mod .. " + F", hl.dsp.window.fullscreen({ mode = "maximized" }))
            hl.bind(mod .. " + TAB", hl.dsp.overview())
            hl.unbind(mod .. " + J")
            local b = hl.bind(mod .. " + V", hl.dsp.layout("togglesplit"))
            b:set_enabled(false)
            hl.bind(mod .. " + B", function() hl.notify("hi") end)
        """)
        val b = binds(c)
        val sup = setOf(Mod.SUPER)
        assertTrue(Bind(sup, "Q", "killactive") in b)
        assertFalse(b.any { it.mods == sup && it.key == "Q" && it.dispatcher == "exec" })
        assertTrue(Bind(sup, "LEFT", "movefocus", "l") in b)
        assertTrue(Bind(sup + Mod.SHIFT, "3", "movetoworkspacesilent", "3") in b)
        assertTrue(Bind(sup, "RETURN", "exec", "com.termux") in b)
        assertTrue(Bind(setOf(Mod.CTRL, Mod.ALT), "T", "exec", "org.example.term") in b)
        assertTrue(Bind(sup, "F", "fullscreen", "1") in b)
        assertTrue(Bind(sup, "TAB", "overview") in b)
        assertFalse(b.any { it.mods == sup && it.key == "J" })
        // The disabled bind is gone, and with it the default it replaced.
        assertFalse(b.any { it.mods == sup && it.key == "V" })
        val lua = b.single { it.key == "B" }
        assertEquals("lua", lua.dispatcher)
        assertEquals(b.size, b.map { it.mods to it.key }.toSet().size)
        assertTrue(b.all { Mod.ALT !in it.mods || Mod.CTRL in it.mods })
    }

    @Test
    fun luaBindsRunInTheState() {
        val c = load("hl.bind('ALT + B', function() hl.notify('hi'); hl.dispatch(hl.dsp.focus({ workspace = 2 })) end)")
        val id = binds(c).single { it.key == "B" }.arg
        lua.call(id)
        assertEquals(listOf("hi"), host.notes)
        assertEquals(listOf("workspace" to "2"), host.dispatched)
    }

    @Test
    fun pinFloatMoveResizeAndTheMonitor() {
        host.snap = host.snap.copy(
            windows = host.snap.windows.map { if (it.id == 9) it.copy(pinned = true) else it },
            monitor = SnapMonitor(0, 2538, 1058, 12, 39, 2526, 983),
        )
        val c = load("""
            hl.bind("ALT + CTRL + Space", hl.dsp.window.pin())
            hl.bind("ALT + CTRL + SHIFT + F", function()
              local m = hl.get_active_monitor()
              local a = m.work_area
              hl.dispatch(hl.dsp.window.float({ action = "on" }))
              hl.dispatch(hl.dsp.window.resize({ x = a.width, y = a.height, relative = false }))
              hl.dispatch(hl.dsp.window.move({ x = a.x, y = a.y, relative = false }))
              hl.dispatch(hl.dsp.window.pin({ action = "toggle" }))
              hl.notify(m.width .. "x" .. m.height .. " " .. m.reserved.top .. " " .. m.reserved[3] ..
                " " .. tostring(hl.get_window(9).pinned) .. " " .. tostring(hl.get_window(7).pinned) ..
                " " .. #hl.get_monitors() .. " " .. m.active_workspace.id)
            end)
            hl.bind("ALT + Q", hl.dsp.window.pin({ action = "off", window = "taskid:7" }))
            hl.bind("ALT + T", hl.dsp.window.float({ action = "off", window = hl.get_window(9) }))
            hl.bind("ALT + B", hl.dsp.window.resize({ x = "50%", y = "100%", relative = false }))
            hl.bind("ALT + C", hl.dsp.window.move({ x = -20, y = 10 }))
        """)
        val b = binds(c)
        val alt = setOf(Mod.ALT)
        assertTrue(Bind(alt + Mod.CTRL, "SPACE", "pin", "toggle") in b)
        assertTrue(Bind(alt, "Q", "pin", "off taskid:7") in b)
        assertTrue(Bind(alt, "T", "settiled", "taskid:9") in b)
        assertTrue(Bind(alt, "B", "resizeactive", "exact 50% 100%") in b)
        assertTrue(Bind(alt, "C", "moveactive", "-20 10") in b)
        lua.call(b.single { it.key == "F" && Mod.CTRL in it.mods }.arg)
        assertEquals(listOf("setfloating" to "", "resizeactive" to "exact 2514 944",
            "moveactive" to "exact 12 39", "pin" to "toggle"), host.dispatched)
        assertEquals(listOf("2538x1058 39 12 true false 1 1"), host.notes)
        assertTrue(error("hl.dsp.window.pin({ action = 'sideways' })").contains("action is"))
        assertTrue(error("hl.dsp.window.move({ x = '10%', y = 0 })").contains("bad coordinate"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.raw('pin', 'frob'))").contains("pin takes"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.raw('setfloating', 'class:x'))").contains("bad window"))
        host.snap = host.snap.copy(monitor = null)
        load("hl.notify(tostring(hl.get_active_monitor()) .. ' ' .. #hl.get_monitors())")
        assertEquals("nil 0", host.notes.last())
    }

    @Test
    fun badBinds() {
        assertTrue(error("hl.bind('ALT + NOSUCHKEY', hl.dsp.window.close())").contains("unknown key NOSUCHKEY"))
        assertTrue(error("hl.bind('ALT + Q', 'killactive')").contains("expected an hl.dsp dispatcher or a function"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.raw('frob'))").contains("unknown dispatcher frob"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.focus({ workspace = 1001 }))").contains("workspaces go up to 1000"))
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.focus({ direction = 'sideways' }))").contains("bad direction"))
        assertTrue(error("hl.bind('ALT + mouse:272', hl.dsp.window.close(), { mouse = true })").contains("drag()"))
        val c = load("hl.bind('ALT + Q', hl.dsp.window.close(), { repeating = true, locked = true })")
        assertTrue(c.warnings.any { "bind 'ALT + Q': locked not supported" in it })
        assertTrue(c.warnings.none { "repeating" in it })
        val g = load("hl.bind('gesture:edge_bottom', hl.dsp.overview(), { repeating = true })")
        assertTrue(g.warnings.any { "repeating not supported" in it })
    }

    @Test
    fun gestureBinds() {
        val c = load("""
            hl.bind("gesture:edge_bottom", hl.dsp.overview())
            hl.unbind("gesture:edge_top_right")
            hl.bind("gesture:swipe_4_down", function() hl.notify("down") end)
            hl.gesture({ fingers = 3, direction = "horizontal", action = "workspace" })
            hl.gesture({ fingers = 2, direction = "pinchout", action = hl.dsp.launcher() })
            hl.config({ shell = { touch = false } })
        """)
        val g = binds(c).filter { it.key.startsWith("GESTURE:") }.associate { it.key.removePrefix("GESTURE:") to it }
        assertEquals("overview", g.getValue("EDGE_BOTTOM").dispatcher)
        assertFalse("EDGE_TOP_RIGHT" in g)
        assertEquals("notifications", g.getValue("EDGE_TOP").dispatcher)
        assertEquals("lua", g.getValue("SWIPE_4_DOWN").dispatcher)
        assertEquals("e+1", g.getValue("SWIPE_3_LEFT").arg)
        assertEquals("e-1", g.getValue("SWIPE_3_RIGHT").arg)
        assertEquals("launcher", g.getValue("PINCH_OUT").dispatcher)
        assertEquals("overview", g.getValue("PINCH_IN").dispatcher)
        assertEquals("back", g.getValue("EDGE_LEFT").dispatcher)
        val bk = load("hl.bind('gesture:edge_bottom', hl.dsp.back())")
        assertEquals("back", binds(bk).single { it.key == "GESTURE:EDGE_BOTTOM" }.dispatcher)
        assertEquals("false", c.wire.values["touch"])
        assertTrue(error("hl.bind('gesture:wiggle', hl.dsp.overview())").contains("unknown gesture WIGGLE"))
        assertTrue(error("hl.bind('ALT + gesture:edge_left', hl.dsp.overview())").contains("no modifiers"))
        assertTrue(error("hl.gesture({ fingers = 2, direction = 'up', action = 'workspace' })").contains("3 to 5 fingers"))
        val qs = load("hl.bind('gesture:edge_left', hl.dsp.quick_settings())")
        assertEquals("quicksettings", binds(qs).single { it.key == "GESTURE:EDGE_LEFT" }.dispatcher)
    }

    @Test
    fun mouseBindsSetTheMouseModifier() {
        val c = load("""
            hl.bind("CTRL + mouse:272", hl.dsp.window.drag(), { mouse = true })
            hl.bind("CTRL + mouse:273", hl.dsp.window.resize(), { mouse = true })
        """)
        assertEquals("CTRL", c.wire.values["mouse_mod"])
        assertEquals("ALT", c.wire.values["mod"])
    }

    @Test
    fun windowAndWorkspaceRules() {
        val c = load("""
            hl.window_rule({ name = "fox", match = { class = "firefox" }, workspace = "2 silent" })
            hl.window_rule({ match = { class = "^com\\.android\\.calculator2$" }, float = true, opacity = "0.9 0.7" })
            hl.window_rule({ match = { title = "Picture.in.picture" }, tile = true, pin = true })
            local off = hl.window_rule({ match = { class = "x" }, float = true })
            off:set_enabled(false)
            hl.workspace_rule({ workspace = "3", layout = "master", gaps_in = 0, gaps_out = 0, default_name = "code" })
            hl.workspace_rule({ workspace = 3, on_created_empty = "com.termux" })
        """)
        val rules = c.wire.lists.getValue("windowrule").map { WindowRule.decode(it) }
        assertEquals(3, rules.size)
        assertEquals(WindowRule(name = "fox", matchClass = "firefox", workspace = 2, silent = true), rules[0])
        assertTrue(rules[0].matches("org.mozilla.firefox", "x"))
        assertEquals(true, rules[1].float)
        assertEquals(0.7f, rules[1].opacityInactive)
        assertFalse(rules[1].matches("com.android.calculator2.x", null))
        assertEquals(false, rules[2].float)
        assertTrue(c.warnings.any { "pin is not supported" in it })
        val ws = c.wire.lists.getValue("workspace").map { WorkspaceRule.decode(it) }
        assertEquals(listOf(WorkspaceRule(3, "code", LayoutKind.MASTER, 0, 0, "com.termux")), ws)
        assertEquals(ws, c.workspaceRules)
        assertTrue(error("hl.workspace_rule({ workspace = 'w[tv1]', gaps_in = 0 })").contains("only numbered workspaces"))
        assertTrue(error("hl.window_rule({ match = { class = '(' }, float = true })").contains("match.class"))
        assertTrue(error("hl.window_rule({ match = { xwayland = true }, float = true })").contains("xwayland is not supported"))
    }

    @Test
    fun gesturesMonitorsAndTheBar() {
        val c = load("""
            hl.gesture({ fingers = 3, direction = "horizontal", action = "workspace" })
            hl.gesture({ fingers = 4, direction = "up", action = hl.dsp.overview() })
            hl.monitor({ output = "", mode = "preferred", reserved = { bottom = 40 } })
            hl.bar_module("hello", { text = function() return "hi " .. 1 + 1 end, interval = 500,
              on_click = hl.dsp.exec_cmd("com.android.settings"), tooltip = "Hello" })
            hl.bar({ modules_left = { "workspaces" }, modules_center = {}, modules_right = { "hello", "clock" },
              clock_format = "HH:mm", background = "rgba(000000cc)", height = 26 })
        """)
        assertEquals(listOf("fingers=3", "direction=horizontal", "action=workspace").joinToString("\u001f"),
            c.wire.lists.getValue("gesture")[0])
        assertTrue(c.wire.lists.getValue("gesture")[1].contains("action=overview"))
        assertEquals("40", c.wire.values["reserved_bottom"])
        assertTrue(c.warnings.any { "mode is set by Android" in it })
        assertEquals(listOf("workspaces"), c.bar.left)
        assertEquals(emptyList<String>(), c.bar.center)
        assertEquals(listOf("hello", "clock"), c.bar.right)
        assertEquals("HH:mm", c.bar.clockFormat)
        assertEquals(0xcc000000.toInt(), c.bar.background)
        assertEquals(26, c.bar.heightDp)
        val m = c.bar.modules.getValue("hello")
        assertEquals(500L, m.intervalMs)
        assertEquals("exec" to "com.android.settings", m.click)
        assertEquals("hi 2", lua.call(m.textFn!!))
        assertTrue(error("hl.bar({ modules_left = { 'nope' } })").contains("unknown module nope"))
        assertTrue(error("hl.bar({ clock_format = 'HH:mm q' })").contains("clock_format"))
    }

    @Test
    fun barAutohideAndDispatchers() {
        val c = load("""
            hl.bar({ autohide = true, autohide_delay = 800 })
            hl.bind("ALT + B", hl.dsp.bar.toggle())
            hl.bind("ALT + SHIFT + B", hl.dsp.bar.reload())
            hl.bind("gesture:edge_top", hl.dsp.bar.reveal())
        """)
        assertTrue(c.bar.autohide)
        assertEquals(800L, c.bar.autohideDelayMs)
        assertTrue(Bind(setOf(Mod.ALT), "B", "bar", "toggle") in binds(c))
        assertTrue(Bind(setOf(Mod.ALT, Mod.SHIFT), "B", "bar", "reload") in binds(c))
        assertFalse(load("hl.bar({ height = 30 })").bar.autohide)
        assertTrue(error("hl.bar({ autohide = 'yes' })").contains("autohide is true or false"))
        assertTrue(error("hl.bar({ autohide_delay = -1 })").contains("autohide_delay"))
        assertTrue(error("hl.bind('ALT + B', hl.dsp.raw('bar', 'sideways'))").contains("bar takes"))
        assertTrue(error("hl.bind('ALT + B', hl.dsp.bar.build())").contains("not supported"))
    }

    @Test
    fun requireReadsModulesNextToInit() {
        val h = FakeHost(mapOf(
            "config/binds.lua" to "hl.bind('ALT + Q', hl.dsp.window.close())\nreturn { gap = 7 }",
            "broken.lua" to "hl.config({ general = { gaps_in = -1 } })",
        ))
        val l = HyprLua(h) { it in keys }
        val c = load("local m = require('config.binds')\nhl.config({ general = { gaps_in = m.gap } })\nrequire('config.binds')", l)
        assertEquals("7", c.wire.values["gaps_in"])
        assertTrue(Bind(setOf(Mod.ALT), "Q", "killactive") in binds(c))
        val e = (l.load("require('broken')") as HyprLua.Result.Failed).message
        assertTrue(e, e.startsWith("broken.lua:1: general.gaps_in"))
        val m = (l.load("require('../etc/passwd')") as HyprLua.Result.Failed).message
        assertTrue(m, m.contains("bad module name"))
        assertTrue((l.load("require('nope')") as HyprLua.Result.Failed).message.contains("module 'nope' not found"))
    }

    @Test
    fun sandboxHasNoOsIoDebugOrBytecode() {
        load("""
            assert(os == nil and io == nil and debug == nil and package == nil)
            assert(dofile == nil and loadfile == nil and string.dump == nil)
            assert(load(string.char(27) .. "Lua") == nil)
            assert(load("return 1 + 1")() == 2)
            assert(type(hl.time()) == "number" and type(hl.date("%Y")) == "string")
        """)
    }

    @Test
    fun runawayScriptsAreStopped() {
        error("while true do end").let { assertTrue(it, it.contains("instruction limit exceeded")) }
        // A pcall cannot swallow the limit.
        error("while true do pcall(function() while true do end end) end")
            .let { assertTrue(it, it.contains("instruction limit exceeded")) }
        error("local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1000) .. i end")
            .let { assertTrue(it, it.contains("not enough memory") || it.contains("instruction limit")) }
        // The state still works after a runaway hook.
        val c = load("hl.on('window.close', function() while true do end end)\nhl.on('window.open', function(w) hl.notify('open ' .. w.class) end)")
        assertNotNull(c)
        lua.fire("window.close", "9")
        lua.fire("window.open", "9", "org.mozilla.firefox", "Mozilla Firefox", "2")
        assertTrue(host.notes.toString(), host.notes.any { "instruction limit" in it })
        assertTrue(host.notes.contains("open org.mozilla.firefox"))
    }

    @Test
    fun aFailedLoadKeepsTheOldState() {
        load("hl.on('workspace.active', function(ws) hl.notify('ws ' .. ws.id .. ' ' .. ws.name) end)")
        assertTrue(lua.load("hl.config({ general = { gaps_in = -3 } })") is HyprLua.Result.Failed)
        lua.fire("workspace.active", "2")
        assertEquals(listOf("ws 2 web"), host.notes)
    }

    @Test
    fun eventsGetHyprlandTables() {
        load("""
            hl.on("window.active", function(w)
              local ws = hl.get_active_workspace()
              hl.notify(w.class .. "|" .. w.title .. "|" .. tostring(w.floating) .. "|" .. ws.id .. "|" .. #hl.get_windows())
            end)
            hl.on("openwindow", function(w) hl.notify("legacy " .. w.id) end)
            hl.on("config.reloaded", function() hl.notify("reloaded") end)
        """)
        lua.fire("window.active", "7", "com.android.settings", "Settings", "1")
        lua.fire("window.open", "9", "org.mozilla.firefox", "Mozilla Firefox", "2")
        lua.fire("config.reloaded")
        assertEquals(listOf("com.android.settings|Settings|false|1|2", "legacy 9", "reloaded"), host.notes)
        assertTrue(error("hl.on('window.wobble', function() end)").contains("unknown event"))
    }

    @Test
    fun timersRunAndStopOnReload() {
        load("local n = 0\nhl.timer(function() n = n + 1; hl.notify('tick ' .. n) end, { timeout = 1000, type = 'repeat' })")
        assertEquals(1, host.timers.size)
        host.runTimers()
        host.runTimers()
        assertEquals(listOf("tick 1", "tick 2"), host.notes)
        load("")
        assertEquals(0, host.timers.size)
    }

    @Test
    fun runtimeConfigChangesArePushed() {
        load("hl.bind('ALT + B', function() hl.config({ general = { gaps_in = 20 } }) end)")
        lua.call("1")
        assertEquals("20", host.changed.single().wire.values["gaps_in"])
        lua.call("1")
        assertEquals(2, host.changed.size)
    }

    @Test
    fun getConfigAndPrint() {
        load("hl.config({ general = { gaps_out = 3 } })\nprint('gaps', hl.get_config('general.gaps_out'), hl.get_config('general.gaps_in'))")
        assertEquals(listOf("gaps\t3\t5"), host.logs)
        assertNull(null)
    }

    @Test
    fun hyprlandStyleConfigLoads() {
        // Pieces of Hyprland 0.57's own example configuration.
        val c = load("""
            hl.monitor({ output = "", mode = "preferred", position = "auto", scale = "auto" })
            local terminal = "com.termux"
            hl.env("XCURSOR_SIZE", "24")
            hl.config({
                general = { gaps_in = 5, gaps_out = 20, border_size = 2,
                    col = { active_border = { colors = {"rgba(33ccffee)", "rgba(00ff99ee)"}, angle = 45 },
                            inactive_border = "rgba(595959aa)" },
                    resize_on_border = false, allow_tearing = false, layout = "dwindle" },
                decoration = { rounding = 10, rounding_power = 2, active_opacity = 1.0, inactive_opacity = 1.0,
                    shadow = { enabled = true, range = 4, render_power = 3, color = 0xee1a1a1a },
                    blur = { enabled = true, size = 3, passes = 1, vibrancy = 0.1696 } },
                animations = { enabled = true },
            })
            hl.curve("easy", { type = "spring", mass = 1, stiffness = 238.1191, damping = 24.21279333 })
            hl.animation({ leaf = "windows", enabled = true, speed = 4.79, spring = "easy" })
            hl.config({ dwindle = { preserve_split = true } })
            hl.config({ master = { new_status = "master" } })
            local mainMod = "SUPER"
            hl.bind(mainMod .. " + Q", hl.dsp.exec_cmd(terminal))
            local closeWindowBind = hl.bind(mainMod .. " + C", hl.dsp.window.close())
            hl.bind(mainMod .. " + V", hl.dsp.window.float({ action = "toggle" }))
            hl.bind(mainMod .. " + J", hl.dsp.layout("togglesplit"))
            for i = 1, 9 do
                hl.bind(mainMod .. " + " .. i, hl.dsp.focus({ workspace = i }))
                hl.bind(mainMod .. " + SHIFT + " .. i, hl.dsp.window.move({ workspace = i }))
            end
            hl.bind(mainMod .. " + mouse:272", hl.dsp.window.drag(), { mouse = true })
            hl.bind(mainMod .. " + mouse:273", hl.dsp.window.resize(), { mouse = true })
            hl.bind(mainMod .. " + mouse_down", hl.dsp.focus({ workspace = "e+1" }))
            hl.window_rule({ name = "suppress-maximize-events", match = { class = ".*" }, suppress_event = "maximize" })
        """)
        assertEquals("20", c.wire.values["gaps_out"])
        assertEquals("SUPER", c.wire.values["mouse_mod"])
        assertTrue(Bind(setOf(Mod.SUPER, Mod.SHIFT), "4", "movetoworkspace", "4") in binds(c))
        assertTrue(c.warnings.isNotEmpty())
    }

    @Test
    fun luaStateSurvivesBadBytes() {
        load("hl.notify('\\255\\0x' .. utf8.char(0x1F600))")
        assertEquals(3 + 1, host.notes.single().length - 1)
    }

    @Test
    fun unknownTopLevelIsAnError() {
        try {
            ConfigBuilder { true }.set("general.nope", "1", "number")
            fail()
        } catch (e: LuaConfigException) {
            assertEquals("unknown option general.nope", e.message)
        }
    }

    @Test
    fun workspacesUpToTheConfiguredCountAndKeypadBinds() {
        val l = HyprLua(host) { it in keys || it.startsWith("NUMPAD_") }
        val c = load("""
            hl.config({ shell = { workspaces = 20 } })
            local kp = { "KP_End", "KP_Down", "KP_Next", "KP_Left", "KP_Begin", "KP_Right",
                "KP_Home", "KP_Up", "KP_Prior", "KP_Insert" }
            for i, key in ipairs(kp) do
                hl.bind("ALT + " .. key, hl.dsp.focus({ workspace = i + 10 }))
                hl.bind("ALT + CTRL + " .. key, hl.dsp.window.move({ workspace = i + 10 }))
                hl.bind("ALT + SHIFT + " .. key, hl.dsp.window.move({ workspace = i + 10, follow = false }))
            end
            hl.bind("ALT + Q", hl.dsp.focus({ workspace = "e+1" }))
            hl.window_rule({ name = "far", match = { class = "x" }, workspace = "300" })
            hl.workspace_rule({ workspace = 999, layout = "master" })
        """, l)
        assertEquals("20", c.wire.values["workspaces"])
        val b = binds(c)
        assertTrue(b.toString(), Bind(setOf(Mod.ALT), "KP_END", "workspace", "11") in b)
        assertTrue(Bind(setOf(Mod.ALT, Mod.CTRL), "KP_INSERT", "movetoworkspace", "20") in b)
        assertTrue(Bind(setOf(Mod.ALT, Mod.SHIFT), "KP_PRIOR", "movetoworkspacesilent", "19") in b)
        // The default binds reach 10 with MOD + 0.
        assertTrue(Bind(setOf(Mod.ALT), "0", "workspace", "10") in b)
        assertTrue(c.warnings.toString(), c.warnings.any { "workspace 300 is past shell.workspaces = 20" in it })
        assertEquals(999, c.workspaceRules.single().workspace)
        // The default count is 10: a bind past it loads, with a warning.
        val d = load("hl.bind('ALT + Q', hl.dsp.focus({ workspace = 11 }))")
        assertEquals("10", d.wire.values["workspaces"])
        assertTrue(d.warnings.toString(), d.warnings.any { "workspace 11 is past shell.workspaces = 10" in it })
        assertTrue(error("hl.config({ shell = { workspaces = 1001 } })").contains("1..1000"))
    }

    @Test
    fun getWorkspacesListsTheExistingOnes() {
        host.snap = host.snap.copy(active = 12, existing = listOf(2, 30, 1))
        load("""
            local ids = {}
            for _, w in ipairs(hl.get_workspaces()) do ids[#ids + 1] = w.id .. (w.active and "*" or "") end
            hl.notify(table.concat(ids, " "))
        """)
        assertEquals(listOf("1 2 12* 30"), host.notes)
    }

    @Test
    fun systemActionDispatchers() {
        val all = HyprLua(FakeHost()) { true }
        val b = binds(load("""
            hl.bind("ALT + SHIFT + V", hl.dsp.clipboard_history())
            hl.bind("ALT + SHIFT + T", hl.dsp.translate())
            hl.bind("ALT + SHIFT + S", hl.dsp.screenshot())
            hl.bind("ALT + CTRL + SHIFT + S", hl.dsp.screenshot({ mode = "full" }))
            hl.bind("ALT + SHIFT + Escape", hl.dsp.lock())
            hl.bind("ALT + Escape", hl.dsp.user_switcher())
        """, all))
        val shift = setOf(Mod.ALT, Mod.SHIFT)
        assertTrue(b.toString(), Bind(shift, "V", "clipboardhistory") in b)
        assertTrue(Bind(shift, "T", "translate") in b)
        assertTrue(Bind(shift, "S", "screenshot", "area") in b)
        assertTrue(Bind(shift + Mod.CTRL, "S", "screenshot", "full") in b)
        assertTrue(Bind(shift, "ESCAPE", "lock") in b)
        assertTrue(Bind(setOf(Mod.ALT), "ESCAPE", "userswitcher") in b)
        assertTrue(error("hl.bind('ALT + Q', hl.dsp.screenshot({ mode = 'window' }))").contains("'area' or 'full'"))
    }
}
