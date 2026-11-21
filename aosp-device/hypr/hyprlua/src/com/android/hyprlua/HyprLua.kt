package com.android.hyprlua

import com.android.hyprtile.parseColor

/** A window as Lua sees it (`hl.get_windows`). */
data class SnapWindow(
    val id: Int,
    val workspace: Int,
    val packageName: String,
    val title: String,
    val floating: Boolean,
    val minimized: Boolean,
    val x: Int = 0,
    val y: Int = 0,
    val width: Int = 0,
    val height: Int = 0,
)

/** The shell's state for `hl.get_*`: workspaces, the focused window, windows, workspace names. */
data class Snapshot(
    val active: Int,
    val workspaces: Int,
    val focused: Int,
    val windows: List<SnapWindow>,
    val names: Map<Int, String> = emptyMap(),
)

/** What the Lua runtime needs from HyprLauncher. Called on the runtime's thread. */
interface LuaHost {
    /** The text of [path] (`config/binds.lua`) next to init.lua, or null when there is none. */
    fun readModule(path: String): String?

    fun dispatch(name: String, arg: String)

    fun notify(text: String, timeoutMs: Long, color: Int?)

    fun snapshot(): Snapshot

    fun log(message: String)

    /** Runs [task] on the runtime's thread after [delayMs]; returns a token for [cancel]. */
    fun schedule(delayMs: Long, task: Runnable): Any

    fun cancel(token: Any)

    /** A hook or timer changed the configuration (hl.config, hl.bind, ... at runtime). */
    fun configChanged(config: HyprConfig)
}

/**
 * HyprLauncher's Lua runtime (Phase D5): loads init.lua into a fresh sandboxed [LuaState]
 * with the `hl` API (prelude.lua), keeps it for event hooks, Lua binds, timers and bar
 * modules, and keeps the last good state when a new configuration fails. One thread only.
 */
class HyprLua(private val host: LuaHost, private val isKey: (String) -> Boolean) {

    sealed interface Result {
        data class Loaded(val config: HyprConfig) : Result
        data class Failed(val message: String) : Result
    }

    private class Timer(val fnId: String, var ms: Long, val repeat: Boolean) {
        var enabled = true
        var token: Any? = null
    }

    private var state: LuaState? = null
    private var builder: ConfigBuilder? = null
    private var timers = HashMap<String, Timer>()
    /** Set while a configuration is being loaded: the calls go to these. */
    private var loading: Triple<LuaState, ConfigBuilder, HashMap<String, Timer>>? = null

    val loaded: Boolean get() = state != null

    /**
     * Loads [source] as init.lua. On success the new state replaces the old one (its timers
     * stop) and the result has the configuration; on failure the old state stays and the
     * result has the error, `init.lua:LINE: message`.
     */
    fun load(source: String, name: String = "init.lua"): Result {
        val b = ConfigBuilder(isKey)
        val newTimers = HashMap<String, Timer>()
        val s = try {
            LuaState(::onHost)
        } catch (e: RuntimeException) {
            return Result.Failed("lua: ${e.message}")
        } catch (e: LinkageError) {
            return Result.Failed("lua engine missing: ${e.message}")
        }
        loading = Triple(s, b, newTimers)
        val config = try {
            s.run(PRELUDE, "=prelude")
            s.run(source, "@$name")
            b.build()
        } catch (e: LuaException) {
            s.close()
            return Result.Failed(firstLines(e.message))
        } catch (e: LuaConfigException) {
            s.close()
            return Result.Failed("$name: ${e.message}")
        } finally {
            loading = null
        }
        close()
        state = s
        builder = b
        timers = newTimers
        for ((id, t) in timers) start(id, t)
        return Result.Loaded(config)
    }

    /** Runs the `hl.on` handlers of [event]; errors are shown as notifications. */
    fun fire(event: String, vararg args: String?) {
        val s = state ?: return
        val errors = guarded("event $event") { s.call("__hl_fire", event, *args, budget = HOOK_BUDGET) }
        errors?.takeIf { it.isNotEmpty() }?.let { host.notify("$event: ${firstLines(it)}", ERROR_MS, ERROR_COLOR) }
        afterCall()
    }

    /** Calls the Lua function [id] (a bind, a click); returns its first result as a string. */
    fun call(id: String, vararg args: String?): String? {
        val s = state ?: return null
        val r = guarded("function") { s.call("__hl_call", id, *args, budget = HOOK_BUDGET) }
        afterCall()
        return r
    }

    /** Stops the timers and closes the state. */
    fun close() {
        for (t in timers.values) t.token?.let(host::cancel)
        timers.clear()
        state?.close()
        state = null
        builder = null
    }

    private fun guarded(what: String, block: () -> String?): String? = try {
        block()
    } catch (e: LuaException) {
        host.notify("$what: ${firstLines(e.message)}", ERROR_MS, ERROR_COLOR)
        host.log("$what: ${e.message}")
        null
    }

    private fun afterCall() {
        val b = builder ?: return
        if (!b.dirty) return
        try {
            host.configChanged(b.build())
        } catch (e: LuaConfigException) {
            host.notify("configuration: ${e.message}", ERROR_MS, ERROR_COLOR)
        }
    }

    private fun start(id: String, t: Timer) {
        t.token?.let(host::cancel)
        t.token = null
        if (!t.enabled) return
        t.token = host.schedule(t.ms, Runnable {
            t.token = null
            if (timers[id] !== t) return@Runnable
            call(t.fnId)
            if (t.repeat && timers[id] === t) start(id, t)
        })
    }

    /** `__hl_call`-style calls from the prelude: `__host(op, ...)`. */
    private fun onHost(op: String, a: List<String?>): String? {
        val b = loading?.second ?: builder ?: throw LuaException("no configuration")
        val t = loading?.third ?: timers
        fun arg(i: Int) = a.getOrNull(i) ?: throw LuaConfigException("$op: argument ${i + 1} missing")
        fun pairs(): Map<String, String> {
            val m = LinkedHashMap<String, String>()
            var i = 0
            while (i + 1 < a.size) {
                val k = a[i]
                val v = a[i + 1]
                if (k != null && v != null) m[k] = v
                i += 2
            }
            return m
        }
        try {
            return when (op) {
                "set" -> { b.set(arg(0), arg(1), arg(2)); null }
                "get" -> b.get(arg(0))
                "curve" -> { b.curve(arg(0), arg(1), arg(2), arg(3), arg(4)); null }
                "spring" -> { b.spring(arg(0)); null }
                "animation" -> {
                    b.animation(arg(0), arg(1) == "1", a.getOrNull(2), a.getOrNull(3), a.getOrNull(4))
                    null
                }
                "warn" -> { b.warn(arg(0)); null }
                "bind" -> b.bind(arg(0), arg(1), a.getOrNull(2) ?: "", a.getOrNull(3) == "1",
                    a.getOrNull(4) ?: "", a.getOrNull(5)).toString()
                "unbind" -> { b.unbind(arg(0)); null }
                "bind_enabled" -> { b.setBindEnabled(arg(0).toInt(), arg(1) == "1"); null }
                "bind_is_enabled" -> if (b.isBindEnabled(arg(0).toInt())) "1" else "0"
                "bind_remove" -> { b.removeBind(arg(0).toInt()); null }
                "windowrule" -> b.windowRule(pairs()).toString()
                "workspacerule" -> b.workspaceRule(pairs()).toString()
                "rule_enabled" -> { b.setRuleEnabled(arg(0).toInt(), arg(1) == "1"); null }
                "rule_is_enabled" -> if (b.isRuleEnabled(arg(0).toInt())) "1" else "0"
                "gesture" -> { b.gesture(pairs()); null }
                "monitor" -> { b.monitor(pairs()); null }
                "bar" -> { b.bar(pairs()); null }
                "barmodule" -> {
                    b.barModule(arg(0), a.getOrNull(1), a.getOrNull(2), a.getOrNull(3), a.getOrNull(4),
                        a.getOrNull(5), a.getOrNull(6))
                    null
                }
                "dispatch" -> {
                    val name = arg(0)
                    val d = a.getOrNull(1) ?: ""
                    ConfigBuilder.checkDispatcher(name, d)?.let { throw LuaConfigException(it) }
                    host.dispatch(name, d)
                    null
                }
                "notify" -> {
                    val ms = a.getOrNull(1)?.toDoubleOrNull()?.toLong() ?: NOTIFY_MS
                    val color = a.getOrNull(2)?.let {
                        try {
                            parseColor(it)
                        } catch (e: IllegalArgumentException) {
                            throw LuaConfigException("notify: ${e.message}")
                        }
                    }
                    host.notify(arg(0), ms.coerceIn(500, 600_000), color)
                    null
                }
                "state" -> encode(host.snapshot())
                "timer" -> {
                    val ms = arg(1).toDoubleOrNull()?.toLong()?.takeIf { it >= MIN_TIMER_MS }
                        ?: throw LuaConfigException("timer: timeout is $MIN_TIMER_MS ms or more")
                    val repeat = when (arg(2)) {
                        "repeat" -> true
                        "oneshot" -> false
                        else -> throw LuaConfigException("timer: type is 'repeat' or 'oneshot'")
                    }
                    val timer = Timer(arg(0), ms, repeat)
                    t[arg(0)] = timer
                    if (loading == null) start(arg(0), timer)
                    null
                }
                "timer_enabled" -> {
                    t[arg(0)]?.let {
                        it.enabled = arg(1) == "1"
                        if (loading == null) start(arg(0), it)
                    }
                    null
                }
                "timer_is_enabled" -> if (t[arg(0)]?.enabled == true) "1" else "0"
                "timer_timeout" -> {
                    t[arg(0)]?.let {
                        it.ms = arg(1).toDoubleOrNull()?.toLong()?.takeIf { v -> v >= MIN_TIMER_MS }
                            ?: throw LuaConfigException("timer: timeout is $MIN_TIMER_MS ms or more")
                        if (loading == null) start(arg(0), it)
                    }
                    null
                }
                "log" -> { host.log(arg(0)); null }
                "readmodule" -> {
                    val name = arg(0)
                    if (!name.matches(MODULE_NAME)) throw LuaConfigException("bad module name '$name'")
                    val path = name.replace('.', '/') + ".lua"
                    host.readModule(path) ?: throw LuaConfigException("module '$name' not found ($path next to init.lua)")
                }
                else -> throw LuaConfigException("unknown host call $op")
            }
        } catch (e: LuaConfigException) {
            throw LuaException(e.message ?: op)
        } catch (e: NumberFormatException) {
            throw LuaException("$op: ${e.message}")
        }
    }

    companion object {
        /** Instructions for one hook, bind or timer call. */
        const val HOOK_BUDGET = 2_000_000L
        const val NOTIFY_MS = 5000L
        const val ERROR_MS = 10_000L
        val ERROR_COLOR = 0xFFFF5555.toInt()
        const val MIN_TIMER_MS = 100L
        private val MODULE_NAME = Regex("[A-Za-z0-9_-]+(\\.[A-Za-z0-9_-]+)*")

        val PRELUDE: String by lazy {
            HyprLua::class.java.getResourceAsStream("prelude.lua")!!.bufferedReader().readText()
        }

        /** The default init.lua, copied for the user on first run. */
        val DEFAULT_INIT: String by lazy {
            HyprLua::class.java.getResourceAsStream("default-init.lua")!!.bufferedReader().readText()
        }

        /** An error without Lua's stack traceback, which only the log gets. */
        fun firstLines(message: String?): String =
            (message ?: "error").substringBefore("\nstack traceback:").trim()

        /** The snapshot as the prelude reads it: records split by 0x1e, fields by 0x1f. */
        fun encode(s: Snapshot): String {
            fun clean(v: String) = v.replace('\u001e', ' ').replace('\u001f', ' ')
            val recs = ArrayList<String>()
            recs += listOf("S", s.active, s.workspaces, s.focused).joinToString("\u001f")
            for (w in s.windows) {
                recs += listOf("W", w.id, w.workspace, clean(w.packageName), clean(w.title),
                    if (w.floating) 1 else 0, if (w.minimized) 1 else 0, w.x, w.y, w.width, w.height)
                    .joinToString("\u001f")
            }
            for ((n, name) in s.names) recs += listOf("N", n, clean(name)).joinToString("\u001f")
            return recs.joinToString("\u001e")
        }
    }
}
