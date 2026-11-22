package com.android.hyprlauncher

import android.app.AppOpsManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.FileObserver
import android.os.Handler
import android.os.HandlerThread
import android.os.Process
import android.os.UserManager
import android.graphics.Rect
import android.util.Log
import android.view.Display
import android.view.KeyEvent
import com.android.hyprlua.BarModule
import com.android.hyprlua.HyprConfig
import com.android.hyprlua.HyprLua
import com.android.hyprlua.LuaHost
import com.android.hyprlua.LuaState
import com.android.hyprlua.SnapMonitor
import com.android.hyprlua.SnapWindow
import com.android.hyprlua.Snapshot
import com.android.hyprlua.WireConfig
import com.android.hyprtile.WorkspaceRule
import java.io.File

/**
 * The Lua configuration (Phase D5, docs/hypr-lua.md): `Documents/hypr/init.lua` on the shared
 * storage (`/sdcard/Documents/hypr/`), a default copied there on first run, `require` for files
 * next to it. Saving reloads it (FileObserver, with a cheap modification-time check every few
 * seconds for writes that bypass the FUSE mount, as `waydroid shell` makes); on an error the
 * last good configuration stays and the error shows under the bar, as in Hyprland.
 *
 * Lua runs on its own thread ("hypr-lua"), never in SystemUI: the configuration reaches the
 * shell as a Bundle through IHyprShell.setConfig. The last good one is kept in
 * device-protected storage and sent before the user is unlocked (the shared storage is not
 * mounted yet). Event hooks, Lua binds, timers and bar modules run on the same thread.
 */
class LuaConfig(private val desktop: Desktop) : LuaHost {
    private val thread = HandlerThread("hypr-lua").apply { start() }
    private val lua = Handler(thread.looper)
    private val runtime = HyprLua(this) { KeyEvent.keyCodeFromString("KEYCODE_$it") != KeyEvent.KEYCODE_UNKNOWN }
    private val cache = desktop.context.createDeviceProtectedStorageContext()
        .getSharedPreferences("hypr-lua", Context.MODE_PRIVATE)
    private val dir = File(Environment.getExternalStorageDirectory(), "Documents/hypr")
    private val init = File(dir, "init.lua")

    /** The configuration in force (main thread reads it). */
    @Volatile
    var current: HyprConfig? = null
        private set
    private var observers = emptyList<FileObserver>()
    private var stamps = emptyMap<String, Long>()
    private var started = false
    private var everLoaded = false
    private val moduleTimers = ArrayList<Runnable>()
    private val reload = Runnable { load() }
    private val poll = object : Runnable {
        override fun run() {
            if (stamps != stampFiles()) scheduleReload()
            lua.postDelayed(this, POLL_MS)
        }
    }

    /** Sends the cached configuration, then loads init.lua once the user is unlocked. */
    fun start() {
        if (started) return
        started = true
        LuaState.fallbackPath = "${desktop.context.applicationInfo.sourceDir}!/lib/" +
            "${Build.SUPPORTED_64_BIT_ABIS.firstOrNull() ?: "x86_64"}/libhyprlua_jni.so"
        cachedWire()?.let { push(it) }
        val um = desktop.context.getSystemService(UserManager::class.java)
        if (um.isUserUnlocked) {
            lua.post { begin() }
        } else {
            desktop.context.registerReceiver(object : BroadcastReceiver() {
                override fun onReceive(c: Context, i: Intent) {
                    desktop.context.unregisterReceiver(this)
                    lua.post { begin() }
                }
            }, IntentFilter(Intent.ACTION_USER_UNLOCKED), Context.RECEIVER_NOT_EXPORTED)
        }
    }

    /** IHyprShell (re)connected: it needs the configuration again. */
    fun onShellAttached() {
        lua.post { (current?.wire ?: cachedWire())?.let { push(it) } }
    }

    /**
     * Right after unlock the shared storage can still be unattached in MediaProvider (opens
     * fail with EPERM): retried every 2 s for a minute before the error shows.
     */
    private fun begin(attempt: Int = 0) {
        allowStorage()
        try {
            if (!init.exists()) {
                dir.mkdirs()
                init.writeText(HyprLua.DEFAULT_INIT)
                Log.i(TAG, "wrote the default $init")
            }
            init.readText()
        } catch (e: Exception) {
            if (attempt < STORAGE_RETRIES) {
                Log.i(TAG, "shared storage not ready ($e), retrying")
                lua.postDelayed({ begin(attempt + 1) }, 2000)
                return
            }
            Log.w(TAG, "cannot use $init: $e")
        }
        load()
        watch()
        lua.postDelayed(poll, POLL_MS)
    }

    /**
     * The launcher reads `/sdcard/Documents` as a file manager does: MANAGE_EXTERNAL_STORAGE,
     * an app op the platform-signed launcher grants itself (MANAGE_APP_OPS_MODES).
     */
    private fun allowStorage() {
        if (Environment.isExternalStorageManager()) return
        try {
            desktop.context.getSystemService(AppOpsManager::class.java).setUidMode(
                AppOpsManager.OPSTR_MANAGE_EXTERNAL_STORAGE, Process.myUid(), AppOpsManager.MODE_ALLOWED)
            Log.i(TAG, "all-files access granted to itself")
        } catch (e: Exception) {
            Log.w(TAG, "cannot grant all-files access: $e")
        }
    }

    /** Reloads after a short pause, so an editor's several writes make one reload. */
    private fun scheduleReload() {
        lua.removeCallbacks(reload)
        lua.postDelayed(reload, RELOAD_DELAY_MS)
    }

    private fun load() {
        stamps = stampFiles()
        val source = try {
            init.readText()
        } catch (e: Exception) {
            showError("cannot read $init: ${e.message}")
            return
        }
        when (val r = runtime.load(source)) {
            is HyprLua.Result.Failed -> {
                Log.w(TAG, "init.lua: ${r.message}")
                showError(r.message)
            }
            is HyprLua.Result.Loaded -> {
                Log.i(TAG, "init.lua loaded: ${r.config.wire.lists["binds"]?.size} binds, " +
                    "${r.config.warnings.size} warnings")
                r.config.warnings.forEach { Log.i(TAG, "init.lua: $it") }
                apply(r.config)
                lastError = null
                desktop.handler.post { desktop.notifier.setError(null) }
                if (r.config.warnings.isNotEmpty()) {
                    notify("init.lua: ${r.config.warnings.size} warning(s)\n" +
                        r.config.warnings.take(3).joinToString("\n") +
                        if (r.config.warnings.size > 3) "\n..." else "", WARN_MS, WARN_COLOR)
                }
                runtime.fire(if (everLoaded) "config.reloaded" else "hyprland.start")
                if (!everLoaded) runtime.fire("config.reloaded")
                everLoaded = true
            }
        }
    }

    private fun showError(message: String) {
        lastError = message
        desktop.handler.post { desktop.notifier.setError(message) }
    }

    private fun apply(config: HyprConfig) {
        current = config
        push(config.wire)
        cache.edit().putString(KEY_WIRE, encode(config.wire)).apply()
        val names = config.workspaceRules.mapNotNull { r -> r.name?.let { r.workspace to it } }.toMap()
        desktop.handler.post { desktop.applyBar(config.bar, names) }
        startModules(config)
    }

    /** Runs the Lua bar modules' text functions every interval. */
    private fun startModules(config: HyprConfig) {
        moduleTimers.forEach(lua::removeCallbacks)
        moduleTimers.clear()
        for (m in config.bar.modules.values) {
            val fn = m.textFn ?: continue
            val tick = object : Runnable {
                override fun run() {
                    if (current !== config) return
                    val text = runtime.call(fn) ?: ""
                    desktop.handler.post { desktop.bar.setModuleText(m.name, text) }
                    lua.postDelayed(this, m.intervalMs)
                }
            }
            moduleTimers += tick
            lua.post(tick)
        }
    }

    /** The bar was re-created (`bar reload`): its Lua modules start over. */
    fun restartModules() {
        lua.post { current?.let { startModules(it) } }
    }

    /** A click on a Lua bar module. */
    fun click(m: BarModule) {
        val (name, arg) = m.click ?: return
        if (name == "lua") lua.post { runtime.call(arg) } else desktop.dispatch(name, arg)
    }

    /** `reload` (hyprctl reload, Phase E11): init.lua and its modules again, now. */
    fun reloadNow() {
        lua.removeCallbacks(reload)
        lua.post { if (everLoaded) load() }
    }

    /** Runs [block] with the Lua runtime on the Lua thread (hyprctl requests, Phase E12). */
    fun withRuntime(block: (HyprLua) -> Unit) {
        lua.post { block(runtime) }
    }

    /** `hyprctl configerrors`: the error in force and the last load's warnings. */
    fun errors(): List<String> = listOfNotNull(lastError) + (current?.warnings ?: emptyList())

    @Volatile
    private var lastError: String? = null

    /** A Lua function bound to keys (dispatcher `lua`, from the shell). */
    fun callBind(id: String) {
        lua.post { runtime.call(id) }
    }

    /** A Hyprland event from the shell (IHyprShellListener.onEvent). */
    fun onEvent(name: String, data: String) {
        val state = desktop.state
        lua.post {
            when (name) {
                "workspace" -> {
                    runtime.fire("workspace.active", data)
                    createdEmpty(data.toIntOrNull() ?: return@post, state)
                }
                "openwindow" -> {
                    val id = data.substringBefore(',')
                    val w = state.windows.find { "${it.taskId}" == id }
                    runtime.fire("window.open", id, data.substringAfter(',', ""), w?.title, w?.workspace?.toString())
                }
                "closewindow" -> runtime.fire("window.close", data)
                "activewindow" -> {
                    val w = state.windows.find { it.taskId == state.focusedTask }
                    runtime.fire("window.active", w?.taskId?.toString() ?: "-1", data.substringBefore(','),
                        data.substringAfter(',', ""), w?.workspace?.toString())
                }
            }
        }
    }

    /** Workspace rules' `on_created_empty`: an app started on an empty workspace switched to. */
    private fun createdEmpty(n: Int, state: ShellState) {
        val rule: WorkspaceRule = current?.workspaceRules?.find { it.workspace == n } ?: return
        val pkg = rule.onCreatedEmpty ?: return
        if (state.windows.none { it.workspace == n }) desktop.dispatch("exec", pkg)
    }

    private fun watch() {
        observers.forEach { it.stopWatching() }
        val dirs = listOf(dir) + (dir.listFiles()?.filter { it.isDirectory } ?: emptyList())
        observers = dirs.map { d ->
            object : FileObserver(d, CLOSE_WRITE or MOVED_TO or CREATE or DELETE) {
                override fun onEvent(event: Int, path: String?) {
                    if (path == null || path.endsWith(".lua") || event and CREATE != 0) {
                        lua.post {
                            if (event and CREATE != 0 && path != null && File(d, path).isDirectory) watch()
                            scheduleReload()
                        }
                    }
                }
            }.also { it.startWatching() }
        }
    }

    /** Modification times of the .lua files under the configuration directory. */
    private fun stampFiles(): Map<String, Long> =
        dir.walkTopDown().maxDepth(4).filter { it.isFile && it.name.endsWith(".lua") }
            .associate { it.path to it.lastModified() + it.length() }

    private fun push(wire: WireConfig) {
        val b = Bundle()
        for ((k, v) in wire.values) b.putString(k, v)
        for ((k, v) in wire.lists) b.putStringArray(k, v.toTypedArray())
        desktop.shellCall { it.setConfig(b) }
    }

    // ---- LuaHost ----

    override fun readModule(path: String): String? {
        val f = File(dir, path).canonicalFile
        if (!f.path.startsWith(dir.canonicalPath + "/")) return null
        return if (f.isFile) f.readText() else null
    }

    override fun dispatch(name: String, arg: String) {
        if (name == "lua") runtime.call(arg) else desktop.dispatch(name, arg)
    }

    override fun notify(text: String, timeoutMs: Long, color: Int?) {
        desktop.handler.post { desktop.notifier.notify(text, timeoutMs, color) }
    }

    override fun snapshot(): Snapshot {
        val s = desktop.state
        return Snapshot(
            active = s.active,
            workspaces = s.workspaces,
            focused = s.focusedTask,
            windows = s.windows.map {
                SnapWindow(it.taskId, it.workspace, it.packageName, it.title, it.floating, it.minimized,
                    it.bounds.left, it.bounds.top, it.bounds.width(), it.bounds.height(), it.pinned)
            },
            monitor = s.takeIf { it.displayWidth > 0 }?.let {
                val w = if (it.workArea.isEmpty) Rect(0, 0, it.displayWidth, it.displayHeight) else it.workArea
                SnapMonitor(Display.DEFAULT_DISPLAY, it.displayWidth, it.displayHeight,
                    w.left, w.top, w.right, w.bottom)
            },
            names = current?.workspaceRules?.mapNotNull { r -> r.name?.let { r.workspace to it } }?.toMap()
            existing = s.existing,
                ?: emptyMap(),
            history = s.focusHistory,
            urgent = s.urgent,
            lastWorkspace = s.lastWorkspace,
            specials = s.specials,
            specialShown = s.specialShown,
        )
    }

    override fun log(message: String) {
        Log.i(TAG, "lua: $message")
    }

    override fun schedule(delayMs: Long, task: Runnable): Any {
        lua.postDelayed(task, delayMs)
        return task
    }

    override fun cancel(token: Any) {
        lua.removeCallbacks(token as Runnable)
    }

    override fun configChanged(config: HyprConfig) {
        apply(config)
    }

    private fun cachedWire(): WireConfig? = cache.getString(KEY_WIRE, null)?.let {
        runCatching { decode(it) }.onFailure { e -> Log.w(TAG, "bad cached configuration: $e") }.getOrNull()
    }

    companion object {
        const val TAG = "HyprLua"
        const val POLL_MS = 3000L
        const val STORAGE_RETRIES = 30
        const val RELOAD_DELAY_MS = 300L
        const val WARN_MS = 8000L
        val WARN_COLOR = 0xFFF9E2AF.toInt()
        private const val KEY_WIRE = "wire"

        /** The wire configuration as one string for the cache: records 0x1e, fields 0x1f. */
        fun encode(w: WireConfig): String {
            val recs = ArrayList<String>()
            for ((k, v) in w.values) recs += "v\u001f$k\u001f$v"
            for ((k, l) in w.lists) recs += (listOf("l", k) + l).joinToString("\u001d")
            return recs.joinToString("\u001e")
        }

        fun decode(s: String): WireConfig {
            val values = LinkedHashMap<String, String>()
            val lists = LinkedHashMap<String, List<String>>()
            for (r in s.split('\u001e')) {
                if (r.startsWith("v\u001f")) {
                    val f = r.split('\u001f', limit = 3)
                    values[f[1]] = f[2]
                } else if (r.startsWith("l\u001d")) {
                    val f = r.split('\u001d')
                    lists[f[1]] = f.drop(2)
                }
            }
            return WireConfig(values, lists)
        }
    }
}
