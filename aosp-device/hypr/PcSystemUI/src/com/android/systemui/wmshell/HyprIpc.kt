package com.android.systemui.wmshell

import android.app.ActivityManager
import android.app.ActivityManager.RunningTaskInfo
import android.app.ActivityTaskManager
import android.app.WindowConfiguration.ACTIVITY_TYPE_STANDARD
import android.app.WindowConfiguration.WINDOWING_MODE_FULLSCREEN
import android.app.KeyguardManager
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.display.DisplayManager
import android.hardware.input.InputManager
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.os.SystemProperties
import android.os.UserHandle
import android.provider.Settings
import android.util.Log
import android.view.Display
import android.view.Display.DEFAULT_DISPLAY
import android.view.InputDevice
import android.view.InputEvent
import android.view.InputEventReceiver
import android.view.InputMonitor
import android.view.MotionEvent
import com.android.hyprtile.AnimationConfig
import com.android.hyprtile.HyprCtlFormat
import com.android.hyprtile.IpcBind
import com.android.hyprtile.IpcDevice
import com.android.hyprtile.IpcDeviceKind
import com.android.hyprtile.IpcEvents
import com.android.hyprtile.IpcLayer
import com.android.hyprtile.IpcMonitor
import com.android.hyprtile.IpcRequest
import com.android.hyprtile.IpcSnapshot
import com.android.hyprtile.IpcVersion
import com.android.hyprtile.IpcWindow
import com.android.hyprtile.IpcWorkspace
import com.android.hyprtile.IpcWorkspaceRef
import com.android.hyprtile.Json
import com.android.hyprtile.WindowFacts
import com.android.hyprtile.WindowSelector
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.ipcAddress
import com.android.hyprtile.ipcEventAddress
import com.android.hyprtile.parseBind
import com.android.systemui.hypr.HyprHostActions
import com.android.wm.shell.common.ShellExecutor
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Hyprland's two IPC sockets for the shell (Phase E12, docs/hypr-ipc.md), served from
 * PcSystemUI:
 *
 * - `@hypr/.socket.sock` (abstract namespace): one request per connection, Hyprland's
 *   `[flags]/command args` or `[[BATCH]]cmd;cmd`, answered as hyprctl prints it, then closed.
 * - `@hypr/.socket2.sock`: Hyprland's `EVENT>>DATA` lines, written to every connected client.
 *
 * Abstract sockets need no directory or init socket, so the same code works on Waydroid and
 * on the PC image, from `adb shell`, Termux and processes the shell starts (`execr`). The
 * peer's uid decides who may use them: root, shell, system and the packages in `ipc.allow`
 * (Lua) or `persist.hypr.ipc_allow` (default Termux and HyprLauncher); SELinux lets those
 * domains connect (sepolicy/hypr_ipc.te).
 *
 * Window and workspace requests run on Shell's main thread (blocking the socket's thread, not
 * Shell); the Lua side, notifications and layers are HyprLauncher's ([launcher]). Events come
 * from [onState], HyprShell's debounced state push.
 */
class HyprIpc(
    private val context: Context,
    private val controller: HyprTileController,
    private val mainExecutor: ShellExecutor,
    /** Asks HyprLauncher (IHyprShellListener.onRequest); null when it did not answer. */
    private val launcher: (command: String, args: String, json: Boolean) -> String?,
) {
    private val started = System.currentTimeMillis() / 1000
    private val workers = Executors.newFixedThreadPool(WORKERS)
    private val eventWriter = Executors.newSingleThreadExecutor()
    private val eventClients = CopyOnWriteArrayList<LocalSocket>()
    private val pm: PackageManager = context.packageManager
    private val displays = context.getSystemService(DisplayManager::class.java)
    private val inputManager = context.getSystemService(InputManager::class.java)
    private var last: IpcSnapshot? = null
    private var lastFocusedMonitor: String? = null
    @Volatile
    private var keywordUntil = 0L
    private var kill: KillMode? = null
    /** The last workspace each display had active (0 while a fullscreen task hides the desk). */
    private val lastActive = HashMap<Int, Int>()

    fun start() {
        listen(REQUEST_SOCKET) { client, ok -> serveRequest(client, ok) }
        listen(EVENT_SOCKET) { client, ok ->
            if (ok) {
                addEventClient(client)
            } else {
                runCatching { client.outputStream.write(DENIED.toByteArray()) }
                runCatching { client.close() }
            }
        }
        displays.registerDisplayListener(object : DisplayManager.DisplayListener {
            override fun onDisplayAdded(id: Int) {
                val d = displays.getDisplay(id) ?: return
                emit("monitoradded", monitorName(id))
                emit("monitoraddedv2", "$id,${monitorName(id)},${d.name}")
            }

            override fun onDisplayRemoved(id: Int) {
                emit("monitorremoved", monitorName(id))
                emit("monitorremovedv2", "$id,${monitorName(id)},")
            }

            override fun onDisplayChanged(id: Int) {}
        }, Handler(Looper.getMainLooper()))
        Log.i(TAG, "listening on @$REQUEST_SOCKET and @$EVENT_SOCKET")
    }

    // ---- sockets ----

    /** Accepts on @[name]; [serve] gets each client and whether its uid may use the socket. */
    private fun listen(name: String, serve: (LocalSocket, Boolean) -> Unit) {
        val thread = Thread({
            val server = try {
                LocalServerSocket(name)
            } catch (e: IOException) {
                Log.e(TAG, "cannot listen on @$name", e)
                return@Thread
            }
            while (true) {
                val client = try {
                    server.accept()
                } catch (e: IOException) {
                    Log.w(TAG, "accept on @$name: $e")
                    continue
                }
                serve(client, allowed(client))
            }
        }, "hypr-ipc-${name.substringAfterLast('.').substringBefore('.')}")
        thread.isDaemon = true
        thread.start()
    }

    /**
     * Root, shell and system (any user), this process, and the apps whose packages are in the
     * allow list: `*` lets every app in, empty only the built-in ones.
     */
    private fun allowed(client: LocalSocket): Boolean {
        val uid = try {
            client.peerCredentials.uid
        } catch (e: IOException) {
            return false
        }
        val app = UserHandle.getAppId(uid)
        if (app == Process.ROOT_UID || app == Process.SYSTEM_UID || app == Process.SHELL_UID ||
            uid == Process.myUid()) {
            return true
        }
        val allow = allowList()
        if ("*" in allow) return true
        val ok = pm.getPackagesForUid(uid)?.any { it in allow } ?: false
        if (!ok) Log.w(TAG, "uid $uid (${pm.getPackagesForUid(uid)?.joinToString()}) refused: not in ipc.allow")
        return ok
    }

    private fun allowList(): Set<String> {
        val v = controller.configOverrides.getString("ipc_allow")
            ?: SystemProperties.get(PROP_ALLOW, DEFAULT_ALLOW)
        return v.split(',', ' ').map { it.trim() }.filter { it.isNotEmpty() }.toSet()
    }

    private fun serveRequest(client: LocalSocket, ok: Boolean) {
        workers.execute {
            try {
                // A refused client's request is read too: closing on unread data resets the
                // connection before it reads the answer.
                val request = readRequest(client) ?: return@execute
                val reply = if (ok) handleRaw(request) else DENIED
                client.outputStream.write(reply.toByteArray())
                client.outputStream.flush()
            } catch (e: Exception) {
                Log.w(TAG, "request failed", e)
                runCatching { client.outputStream.write("error: ${e.message}\n".toByteArray()) }
            } finally {
                runCatching { client.close() }
            }
        }
    }

    /**
     * Reads one request: until the client shuts its side down, or until no more bytes come for
     * a moment once some did (clients that write and then wait, like Hyprland's own hyprctl).
     */
    private fun readRequest(client: LocalSocket): String? {
        val buf = ByteArray(8192)
        val out = ByteArrayOutputStream()
        client.soTimeout = FIRST_BYTE_TIMEOUT_MS
        val input = client.inputStream
        while (out.size() < MAX_REQUEST) {
            val n = try {
                input.read(buf)
            } catch (e: IOException) {
                break // the timeout
            }
            if (n < 0) break
            out.write(buf, 0, n)
            client.soTimeout = MORE_BYTES_TIMEOUT_MS
        }
        return out.toString(Charsets.UTF_8).takeIf { it.isNotBlank() }
    }

    private fun addEventClient(client: LocalSocket) {
        eventClients += client
        Log.i(TAG, "event client connected (${eventClients.size})")
    }

    /** Writes `name>>data` to every event client; any thread. */
    fun emit(name: String, data: String) {
        if (eventClients.isEmpty()) return
        val line = "$name>>${data.replace('\n', ' ')}\n".toByteArray()
        eventWriter.execute {
            for (c in eventClients) {
                try {
                    c.outputStream.write(line)
                    c.outputStream.flush()
                } catch (e: IOException) {
                    eventClients.remove(c)
                    runCatching { c.close() }
                }
            }
        }
    }

    // ---- events ----

    /** The default display's state after a change (HyprShell.push, main thread). */
    fun onState(state: Bundle) {
        val snapshot = snapshot(DEFAULT_DISPLAY, state, emptyMap())
        for ((name, data) in IpcEvents.diff(last, snapshot)) emit(name, data)
        last = snapshot
        val mon = monitorName(controller.focusedDisplay())
        if (lastFocusedMonitor != null && mon != lastFocusedMonitor) {
            val ws = if (controller.focusedDisplay() == DEFAULT_DISPLAY) snapshot.active
            else ref(controller.focusedDisplay(), controller.state(controller.focusedDisplay()).getInt("active"))
            emit("focusedmon", "$mon,${ws.name}")
            emit("focusedmonv2", "$mon,${ws.id}")
        }
        lastFocusedMonitor = mon
    }

    /** A new configuration (HyprLauncher's setConfig): Hyprland's `configreloaded`. */
    fun onConfig() {
        if (SystemClock.uptimeMillis() < keywordUntil) return
        emit("configreloaded", "")
    }

    // ---- requests ----

    private fun handleRaw(raw: String): String {
        if (raw.startsWith(IpcRequest.BATCH)) {
            return IpcRequest.splitBatch(raw).joinToString("\n\n\n") { handle(IpcRequest.parse(it)) }
        }
        return handle(IpcRequest.parse(raw))
    }

    private fun handle(r: IpcRequest): String {
        Log.d(TAG, "request ${r.flags}/${r.command} ${r.args}")
        val j = r.json
        return when (r.command) {
            "dispatch" -> dispatch(r.args)
            "clients" -> onMain { HyprCtlFormat.clients(allWindows(), j) }
            "activewindow" -> onMain {
                val d = controller.focusedDisplay()
                val s = snapshot(d, controller.state(d), pids())
                HyprCtlFormat.activeWindow(s.windows.find { it.taskId == s.focused }, j)
            }
            "workspaces" -> onMain { HyprCtlFormat.workspaces(allWorkspaces(), j) }
            "activeworkspace" -> onMain {
                // Hyprland always has an active workspace: with home in front and no desk active
                // yet it is 1, and one without a desk (E2 makes them on demand) is listed empty.
                val d = controller.focusedDisplay()
                val state = controller.state(d)
                val active = snapshot(d, state, emptyMap()).active.takeIf { it.id != 0 }
                    ?: ref(state, 1, workspaceNames())
                val ws = allWorkspaces().find { it.ref.id == active.id } ?: IpcWorkspace(
                    ref = active, monitor = monitorName(d), monitorId = d, windows = 0,
                    hasFullscreen = false, lastWindow = -1, lastWindowTitle = "", persistent = false,
                    layout = controller.configOverrides.getString("layout")
                        ?: SystemProperties.get("persist.hypr.layout").ifEmpty { "dwindle" },
                )
                HyprCtlFormat.activeWorkspace(ws, j)
            }
            "monitors" -> onMain { HyprCtlFormat.monitors(monitors(r.args.trim() == "all"), j) }
            "devices" -> HyprCtlFormat.devices(devices(), j)
            "binds" -> HyprCtlFormat.binds(binds(), j)
            "layers" -> layers(j)
            "version" -> HyprCtlFormat.version(version(), j)
            "splash" -> HyprCtlFormat.SPLASHES.random() + "\n"
            "cursorpos" -> onMain {
                val p = runCatching { inputManager.getCursorPosition(controller.focusedDisplay()) }.getOrNull()
                HyprCtlFormat.cursorPos(p?.x?.toInt() ?: 0, p?.y?.toInt() ?: 0, j)
            }
            "instances" -> HyprCtlFormat.instances(SIGNATURE, started, Process.myPid(), j)
            "layouts" -> if (j) Json.write(listOf("dwindle", "master")) else "dwindle\nmaster\n"
            "submap" -> "default\n"
            "locked" -> {
                val locked = context.getSystemService(KeyguardManager::class.java)?.isKeyguardLocked ?: false
                if (j) Json.write(linkedMapOf("locked" to locked)) else "$locked\n"
            }
            "globalshortcuts" -> if (j) "[]" else "\n"
            "animations" -> animations(j)
            "workspacerules" -> workspaceRules(j)
            "decorations" -> decorations(r.args, j)
            "reload" -> onMain { controller.dispatch("reload", "") }.let { "ok" }
            "kill" -> onMain { startKillMode(controller.focusedDisplay()) }.let { "ok" }
            "setprop" -> onMain { setProp(r.argv) }
            "getprop" -> onMain { getProp(r.argv, j) }
            "switchxkblayout" -> switchLayout(r.argv)
            "output" -> output(r.argv)
            "keyword" -> {
                keywordUntil = SystemClock.uptimeMillis() + KEYWORD_QUIET_MS
                fromLauncher(r)
            }
            "eval", "repl", "getoption", "configerrors", "descriptions", "notify", "dismissnotify",
            "seterror" -> fromLauncher(r)
            "setcursor" -> "error: Android draws its own pointer; cursor themes cannot be set"
            "plugin" -> "error: plugins are not supported on Android"
            "hyprpaper" -> onMain { controller.dispatch("wallpaper", r.args) }.let { if (it == true) "ok" else "error" }
            "rollinglog" -> "error: run `hyprctl rollinglog`, which reads logcat on the client"
            else -> "unknown request"
        }
    }

    private fun fromLauncher(r: IpcRequest): String =
        launcher(r.command, r.args, r.json) ?: "error: HyprLauncher did not answer (is it the home app?)"

    /**
     * `dispatch NAME ARGS`, or Hyprland 0.57's `dispatch 'hl.dsp...'` (Lua, run by the
     * launcher). Window selectors (`address:0x..`, `class:`, `title:`, `pid:`) in the arguments
     * become task ids; `movetoworkspace[silent] WS,WINDOW` and `closewindow`/`killwindow
     * WINDOW` act on that window.
     */
    private fun dispatch(args: String): String {
        val a = args.trim()
        if (a.startsWith("hl.")) {
            return launcher("eval", "hl.dispatch($a)", false) ?: "error: HyprLauncher did not answer"
        }
        val name = a.substringBefore(' ').trim()
        var arg = a.substringAfter(' ', "").trim()
        if (name.isEmpty()) return "Invalid dispatcher"
        if (name == "execr") return if (HyprHostActions.dispatch(context, "execr", arg)) "ok" else "error"
        return onMain {
            val windows by lazy { allWindows() }
            fun resolve(sel: String): Int? = try {
                val facts = windows.map { WindowFacts(it.taskId, it.cls, it.title, it.pid, it.floating,
                    it.taskId == controller.focusedWindow()?.taskId) }
                WindowSelector.parse(sel).pick(facts)?.id
            } catch (e: IllegalArgumentException) {
                null
            }
            when (name) {
                "closewindow", "killwindow" -> {
                    val id = resolve(arg) ?: return@onMain "Window not found"
                    if (name == "closewindow") controller.dispatchOn(id, "killactive")
                    else controller.dispatchOn(id, "forcekillactive")
                    return@onMain "ok"
                }
                "movetoworkspace", "movetoworkspacesilent" -> if (',' in arg) {
                    val id = resolve(arg.substringAfter(',').trim()) ?: return@onMain "Window not found"
                    return@onMain if (controller.dispatchOn(id, name, arg.substringBefore(',').trim())) "ok"
                    else "Invalid dispatcher or argument"
                }
            }
            arg = arg.split(' ').joinToString(" ") { word ->
                if (SELECTOR.containsMatchIn(word) && name != "focuswindow") {
                    resolve(word)?.let { "taskid:$it" } ?: word
                } else {
                    word
                }
            }
            if (controller.dispatch(name, arg)) "ok" else "Invalid dispatcher or argument"
        }
    }

    /** Runs [block] on Shell's main thread and waits for it (at most [REQUEST_TIMEOUT_MS]). */
    private fun <T> onMain(block: () -> T): T {
        if (Looper.myLooper() == Looper.getMainLooper()) return block()
        val latch = CountDownLatch(1)
        var result: Result<T>? = null
        mainExecutor.execute {
            result = runCatching(block)
            latch.countDown()
        }
        if (!latch.await(REQUEST_TIMEOUT_MS, TimeUnit.MILLISECONDS)) throw IOException("the shell did not answer")
        return result!!.getOrThrow()
    }

    // ---- windows and workspaces (main thread) ----

    private fun workspaceNames(): Map<Int, String> =
        controller.configOverrides.getStringArray("workspace")?.mapNotNull {
            runCatching { WorkspaceRule.decode(it) }.getOrNull()
        }?.filter { it.special == null && it.name != null }?.associate { it.workspace to it.name!! } ?: emptyMap()

    /** Workspace [n] of [state]: numbered, or a special one by its (negative) key. */
    private fun ref(state: Bundle, n: Int, names: Map<Int, String>): IpcWorkspaceRef {
        if (n < 0) {
            val keys = state.getIntArray("specialKeys") ?: IntArray(0)
            val name = state.getStringArray("specialNames")?.getOrNull(keys.indexOf(n)) ?: "special"
            return IpcWorkspaceRef(n, "special:$name")
        }
        if (n == 0) return IpcWorkspaceRef.NONE
        return IpcWorkspaceRef(n, names[n] ?: "$n")
    }

    private fun ref(displayId: Int, n: Int): IpcWorkspaceRef =
        ref(controller.state(displayId), n, workspaceNames())

    private fun pids(): Map<String, Int> {
        val am = context.getSystemService(ActivityManager::class.java) ?: return emptyMap()
        val out = HashMap<String, Int>()
        for (p in am.runningAppProcesses ?: emptyList()) {
            out.putIfAbsent(p.processName, p.pid)
            for (pkg in p.pkgList ?: emptyArray()) out.putIfAbsent(pkg, p.pid)
        }
        return out
    }

    private fun snapshot(displayId: Int, state: Bundle, pids: Map<String, Int>): IpcSnapshot {
        val names = workspaceNames()
        val ids = state.getIntArray("taskIds") ?: IntArray(0)
        val ws = state.getIntArray("taskWorkspaces") ?: IntArray(0)
        val pkgs = state.getStringArray("taskPackages") ?: emptyArray()
        val titles = state.getStringArray("taskTitles") ?: emptyArray()
        val bounds = state.getIntArray("taskBounds") ?: IntArray(0)
        val floating = state.getBooleanArray("taskFloating") ?: BooleanArray(0)
        val minimized = state.getBooleanArray("taskMinimized") ?: BooleanArray(0)
        val pinned = state.getBooleanArray("taskPinned") ?: BooleanArray(0)
        val history = state.getIntArray("focusHistory")?.toList() ?: emptyList()
        val display = state.getIntArray("display")
        val work = state.getIntArray("workArea")
        val windows = ids.indices.map { i ->
            val l = bounds.getOrElse(4 * i) { 0 }
            val t = bounds.getOrElse(4 * i + 1) { 0 }
            val r = bounds.getOrElse(4 * i + 2) { 0 }
            val b = bounds.getOrElse(4 * i + 3) { 0 }
            val fullscreen = when {
                display != null && l <= 0 && t <= 0 && r >= display[0] && b >= display[1] -> 2
                work != null && floating.getOrElse(i) { false } && l == work[0] && t == work[1] &&
                    r == work[2] && b == work[3] -> 1
                else -> 0
            }
            val pkg = pkgs.getOrElse(i) { "" }
            IpcWindow(
                taskId = ids[i], cls = pkg, title = titles.getOrElse(i) { "" },
                x = l, y = t, width = r - l, height = b - t,
                workspace = ref(state, ws.getOrElse(i) { 0 }, names),
                floating = floating.getOrElse(i) { false }, pinned = pinned.getOrElse(i) { false },
                fullscreen = fullscreen, hidden = minimized.getOrElse(i) { false },
                monitor = displayId, pid = pids[pkg] ?: -1,
                focusHistoryId = history.indexOf(ids[i]),
            )
        }
        // Shell takes a fullscreen task out of its desk (and the desk stops being active);
        // Hyprland keeps it a window of its workspace, fullscreen 2.
        val active = state.getInt("active").takeIf { it != 0 }?.also { lastActive[displayId] = it }
            ?: lastActive[displayId] ?: 0
        val full = fullscreenTasks(displayId, ids.toSet())
        val allWindows = windows + full.map { t ->
            val b = t.configuration.windowConfiguration.bounds
            val pkg = (t.baseActivity ?: t.topActivity)?.packageName ?: ""
            IpcWindow(
                taskId = t.taskId, cls = pkg, title = t.taskDescription?.label?.toString() ?: "",
                x = b.left, y = b.top, width = b.width(), height = b.height(),
                workspace = ref(state, active, names), floating = false, pinned = false,
                fullscreen = 2, hidden = false, monitor = displayId, pid = pids[pkg] ?: -1,
                focusHistoryId = if (t.isFocused) 0 else -1,
            )
        }
        val existing = (state.getIntArray("workspaceIds") ?: IntArray(0)).map { ref(state, it, names) } +
            (state.getIntArray("specialKeys") ?: IntArray(0)).map { ref(state, it, names) }
        val shown = state.getString("specialShown")
        val keys = state.getIntArray("specialKeys") ?: IntArray(0)
        val special = shown?.let { s ->
            state.getStringArray("specialNames")?.indexOf(s)?.takeIf { it >= 0 }?.let { IpcWorkspaceRef(keys[it], "special:$s") }
        }
        return IpcSnapshot(
            monitor = monitorName(displayId),
            active = ref(state, active, names),
            workspaces = existing,
            special = special,
            windows = allWindows,
            focused = state.getInt("focusedTask", -1).takeIf { it >= 0 }
                ?: full.find { it.isFocused }?.taskId ?: -1,
            urgent = (state.getIntArray("urgentTasks") ?: IntArray(0)).toSet(),
        )
    }

    private fun fullscreenTasks(displayId: Int, known: Set<Int>): List<RunningTaskInfo> =
        runCatching { ActivityTaskManager.getInstance().getTasks(MAX_TASKS) }.getOrDefault(emptyList()).filter {
            it.displayId == displayId && it.isVisible && it.activityType == ACTIVITY_TYPE_STANDARD &&
                it.windowingMode == WINDOWING_MODE_FULLSCREEN && it.taskId !in known
        }

    private fun shellDisplays(): List<Int> =
        (controller.displaysWithDesks() + DEFAULT_DISPLAY).toSortedSet().toList()

    private fun allWindows(): List<IpcWindow> {
        val pids = pids()
        return shellDisplays().flatMap { snapshot(it, controller.state(it), pids).windows }
    }

    private fun allWorkspaces(): List<IpcWorkspace> {
        val layout = controller.configOverrides.getString("layout")
            ?: SystemProperties.get("persist.hypr.layout").ifEmpty { "dwindle" }
        val rules = controller.configOverrides.getStringArray("workspace")?.mapNotNull {
            runCatching { WorkspaceRule.decode(it) }.getOrNull()
        } ?: emptyList()
        val out = ArrayList<IpcWorkspace>()
        for (d in shellDisplays()) {
            val s = snapshot(d, controller.state(d), emptyMap())
            for (ws in s.workspaces) {
                val on = s.windows.filter { it.workspace.id == ws.id }
                val lastWin = on.minByOrNull { if (it.focusHistoryId < 0) Int.MAX_VALUE else it.focusHistoryId }
                val rule = rules.find { if (ws.id < 0) "special:${it.special}" == ws.name else it.workspace == ws.id }
                out += IpcWorkspace(
                    ref = ws, monitor = s.monitor, monitorId = d, windows = on.size,
                    hasFullscreen = on.any { it.fullscreen != 0 }, lastWindow = lastWin?.taskId ?: -1,
                    lastWindowTitle = lastWin?.title ?: "", persistent = false,
                    layout = rule?.layout?.name?.lowercase() ?: layout,
                )
            }
        }
        return out.sortedBy { it.ref.id }
    }

    private fun monitorName(displayId: Int) = "DISPLAY-$displayId"

    private fun monitors(all: Boolean): List<IpcMonitor> {
        val list = if (all) displays.getDisplays(DisplayManager.DISPLAY_CATEGORY_ALL_INCLUDING_DISABLED)
        else displays.displays
        val focused = controller.focusedDisplay()
        val shellOnes = shellDisplays()
        return list.map { d ->
            val state = if (d.displayId in shellOnes) controller.state(d.displayId) else Bundle()
            val mode = d.mode
            val w = mode.physicalWidth
            val h = mode.physicalHeight
            val metrics = android.util.DisplayMetrics().also { d.getRealMetrics(it) }
            val work = state.getIntArray("workArea")
            val names = workspaceNames()
            val shown = state.getString("specialShown")
            val keys = state.getIntArray("specialKeys") ?: IntArray(0)
            val special = shown?.let { s ->
                state.getStringArray("specialNames")?.indexOf(s)?.takeIf { it >= 0 }
                    ?.let { IpcWorkspaceRef(keys[it], "special:$s") }
            } ?: IpcWorkspaceRef.NONE
            val product = d.deviceProductInfo
            IpcMonitor(
                id = d.displayId, name = monitorName(d.displayId), description = d.name,
                make = product?.manufacturerPnpId ?: "Android", model = product?.name ?: d.name,
                serial = d.uniqueId ?: "", width = w, height = h,
                physicalWidth = if (metrics.xdpi > 0) (w / metrics.xdpi * 25.4f).toInt() else 0,
                physicalHeight = if (metrics.ydpi > 0) (h / metrics.ydpi * 25.4f).toInt() else 0,
                refreshRate = d.refreshRate, x = 0, y = 0,
                activeWorkspace = ref(state, state.getInt("active").takeIf { it != 0 } ?: lastActive[d.displayId] ?: 0, names),
                specialWorkspace = special,
                reserved = if (work != null) listOf(work[0], work[1], w - work[2], h - work[3]) else listOf(0, 0, 0, 0),
                scale = d.densityDpiCompat() / 160f, transform = d.rotation,
                focused = d.displayId == focused, dpmsStatus = d.state == Display.STATE_ON,
                disabled = d.state == Display.STATE_OFF && all, mirrorOf = "none",
                availableModes = d.supportedModes.map {
                    String.format(Locale.ROOT, "%dx%d@%.2fHz", it.physicalWidth, it.physicalHeight, it.refreshRate)
                },
            )
        }
    }

    private fun Display.densityDpiCompat(): Int =
        android.util.DisplayMetrics().also { getRealMetrics(it) }.densityDpi

    private fun devices(): List<IpcDevice> {
        val out = ArrayList<IpcDevice>()
        var mainKeyboard = true
        for (id in inputManager.inputDeviceIds) {
            val d = inputManager.getInputDevice(id) ?: continue
            if (d.isVirtual) continue
            val name = d.name.lowercase(Locale.ROOT).replace(' ', '-')
            if (d.supportsSource(InputDevice.SOURCE_MOUSE) || d.supportsSource(InputDevice.SOURCE_TOUCHPAD)) {
                out += IpcDevice(IpcDeviceKind.MOUSE, id, name)
            }
            if (d.keyboardType == InputDevice.KEYBOARD_TYPE_ALPHABETIC) {
                val lang = d.keyboardLanguageTag ?: ""
                out += IpcDevice(IpcDeviceKind.KEYBOARD, id, name, layout = lang,
                    keymap = d.keyboardLayoutType ?: lang, main = mainKeyboard)
                mainKeyboard = false
            }
            if (d.supportsSource(InputDevice.SOURCE_TOUCHSCREEN)) out += IpcDevice(IpcDeviceKind.TOUCH, id, name)
            if (d.supportsSource(InputDevice.SOURCE_STYLUS)) out += IpcDevice(IpcDeviceKind.TABLET, id, name)
        }
        return out
    }

    private fun binds(): List<IpcBind> {
        val list = controller.configOverrides.getStringArray("binds") ?: return emptyList()
        return list.mapNotNull { runCatching { parseBind(it) }.getOrNull() }.map {
            val mouse = it.key.startsWith("MOUSE:") && it.dispatcher in setOf("movewindow", "resizewindow")
            val flags = (if (mouse) "m" else "") + (if (it.repeating) "e" else "")
            IpcBind(it.mods, it.baseKey, it.dispatcher, it.arg, flags = flags)
        }
    }

    private fun layers(json: Boolean): String {
        val reply = launcher("layers", "", false) ?: ""
        val layers = reply.lines().filter { it.isNotBlank() }.mapIndexedNotNull { i, line -> IpcLayer.decode(i + 1, line) }
        return HyprCtlFormat.layers(mapOf(monitorName(DEFAULT_DISPLAY) to layers), json)
    }

    private fun version(): IpcVersion = IpcVersion(
        version = HYPRLAND_VERSION,
        branch = "android-pc",
        commit = Build.VERSION.INCREMENTAL,
        commitDate = SimpleDateFormat("EEE MMM d HH:mm:ss yyyy", Locale.US).format(Date(Build.TIME)),
        tag = "v$HYPRLAND_VERSION-android",
        android = "Android ${Build.VERSION.RELEASE} (${Build.DISPLAY})",
    )

    private fun animations(json: Boolean): String {
        val o = controller.configOverrides
        val anims = o.getStringArray("animation")?.toList() ?: AnimationConfig.DEFAULT_ANIMATIONS.map { (k, s) ->
            listOfNotNull(k, if (s.enabled) "1" else "0", s.speed.toString(), s.curve, s.style.ifEmpty { null })
                .joinToString(", ")
        }
        val curves = o.getStringArray("bezier")?.toList() ?: AnimationConfig.DEFAULT_BEZIERS.map { (k, b) ->
            "$k, ${b.x1}, ${b.y1}, ${b.x2}, ${b.y2}"
        }
        val a = anims.map { it.split(',').map(String::trim) }.filter { it.size >= 4 }
        val c = curves.map { it.split(',').map(String::trim) }.filter { it.size >= 5 }
        if (json) {
            return Json.write(listOf(
                a.map { linkedMapOf("name" to it[0], "overridden" to true, "bezier" to it[3],
                    "enabled" to (it[1] == "1"), "speed" to (it[2].toFloatOrNull() ?: 0f), "style" to it.getOrElse(4) { "" }) },
                c.map { linkedMapOf("name" to it[0], "X0" to (it[1].toFloatOrNull() ?: 0f), "Y0" to (it[2].toFloatOrNull() ?: 0f),
                    "X1" to (it[3].toFloatOrNull() ?: 0f), "Y1" to (it[4].toFloatOrNull() ?: 0f)) },
            ))
        }
        fun f(s: String) = String.format(Locale.ROOT, "%.2f", s.toFloatOrNull() ?: 0f)
        return buildString {
            append("animations:\n\n")
            for (x in a) {
                append("\tname: ${x[0]}\n\t\toverriden: 1\n\t\tbezier: ${x[3]}\n\t\tenabled: ${x[1]}\n")
                append("\t\tspeed: ${f(x[2])}\n\t\tstyle: ${x.getOrElse(4) { "" }}\n\n")
            }
            append("beziers:\n\n")
            for (x in c) {
                append("\tname: ${x[0]}\n\t\tX0: ${f(x[1])}\n\t\tY0: ${f(x[2])}\n\t\tX1: ${f(x[3])}\n\t\tY1: ${f(x[4])}\n\n")
            }
        }
    }

    private fun workspaceRules(json: Boolean): String {
        val rules = controller.configOverrides.getStringArray("workspace")?.mapNotNull {
            runCatching { WorkspaceRule.decode(it) }.getOrNull()
        } ?: emptyList()
        fun ws(r: WorkspaceRule) = r.special?.let { "special:$it" } ?: "${r.workspace}"
        if (json) {
            return Json.write(rules.map { r ->
                linkedMapOf<String, Any?>("workspaceString" to ws(r)).apply {
                    r.name?.let { put("defaultName", it) }
                    r.layout?.let { put("layout", it.name.lowercase()) }
                    r.gapsIn?.let { put("gapsIn", listOf(it, it, it, it)) }
                    r.gapsOut?.let { put("gapsOut", listOf(it, it, it, it)) }
                    r.onCreatedEmpty?.let { put("onCreatedEmpty", it) }
                }
            })
        }
        fun v(x: Any?) = x?.toString() ?: "<unset>"
        return rules.joinToString("") { r ->
            "Workspace rule ${ws(r)}:\n\tmonitor: <unset>\n\tdefault: <unset>\n\tpersistent: <unset>\n" +
                "\tgapsIn: ${v(r.gapsIn)}\n\tgapsOut: ${v(r.gapsOut)}\n\tborderSize: <unset>\n" +
                "\tborder: <unset>\n\trounding: <unset>\n\tdecorate: <unset>\n\tshadow: <unset>\n" +
                "\tdefaultName: ${v(r.name)}\n\tlayout: ${v(r.layout?.name?.lowercase())}\n" +
                "\tonCreatedEmpty: ${v(r.onCreatedEmpty)}\n\n"
        }
    }

    private fun decorations(args: String, json: Boolean): String {
        val list = listOf("Border" to 10000, "Drop Shadow" to 1000, "Caption bar" to 100)
        if (args.isBlank()) return "error: decorations needs a window"
        return if (json) Json.write(list.map { linkedMapOf("decorationName" to it.first, "priority" to it.second) })
        else list.joinToString("") { "Decoration: ${it.first}\n\tpriority: ${it.second}\n\n" }
    }

    private fun selectTask(sel: String): Int? = try {
        val focused = controller.focusedWindow()?.taskId
        val facts = allWindows().map { WindowFacts(it.taskId, it.cls, it.title, it.pid, it.floating, it.taskId == focused) }
        WindowSelector.parse(sel).pick(facts)?.id
    } catch (e: IllegalArgumentException) {
        null
    }

    /** `setprop WINDOW alpha|alphainactive|opacity|opacity_inactive VALUE|unset [lock]`. */
    private fun setProp(argv: List<String>): String {
        if (argv.size < 3) return "error: setprop WINDOW PROP VALUE"
        val id = selectTask(argv[0]) ?: return "Window not found"
        val value = if (argv[2] == "unset") null else argv[2].toFloatOrNull()?.coerceIn(0f, 1f)
            ?: return "error: bad value ${argv[2]}"
        val deco = controller.decorations
        val (active, inactive) = deco.propOpacity[id] ?: (null to null)
        deco.propOpacity[id] = when (argv[1].lowercase()) {
            "alpha", "opacity" -> value to inactive
            "alphainactive", "opacity_inactive" -> active to value
            "alphaoverride", "opacity_override" -> value to value
            else -> return "prop not found"
        }
        deco.reapplyOpacity()
        return "ok"
    }

    private fun getProp(argv: List<String>, json: Boolean): String {
        if (argv.size < 2) return "error: getprop WINDOW PROP"
        val id = selectTask(argv[0]) ?: return "Window not found"
        val w = allWindows().find { it.taskId == id }!!
        val value: Any = when (argv[1].lowercase()) {
            "alpha", "opacity" -> controller.decorations.opacity(id, true)
            "alphainactive", "opacity_inactive" -> controller.decorations.opacity(id, false)
            "pinned" -> w.pinned
            "floating" -> w.floating
            "fullscreen" -> w.fullscreen
            else -> return "prop not found"
        }
        return if (json) Json.write(linkedMapOf(argv[1] to value)) else "$value\n"
    }

    /** Android switches the active keyboard layout in order: `next`/`prev` only. */
    private fun switchLayout(argv: List<String>): String {
        val dir = when (argv.getOrNull(1)) {
            "next" -> "next"
            "prev" -> "previous"
            else -> return "error: Android switches layouts in order; use next or prev"
        }
        return if (HyprHostActions.dispatch(context, "inputmethod", dir)) "ok" else "error"
    }

    /**
     * `output create|add [headless|auto] [WxH]` / `output remove|destroy DISPLAY-N`: Android's
     * overlay displays (Settings.Global.OVERLAY_DISPLAY_DEVICES, the developer option), the
     * closest thing to a headless output it has.
     */
    private fun output(argv: List<String>): String {
        val resolver = context.contentResolver
        val current = Settings.Global.getString(resolver, Settings.Global.OVERLAY_DISPLAY_DEVICES).orEmpty()
        val specs = current.split(';').filter { it.isNotBlank() }.toMutableList()
        when (argv.getOrNull(0)) {
            "create", "add" -> {
                val backend = argv.getOrNull(1) ?: "auto"
                if (backend !in setOf("headless", "auto")) return "error: backend $backend is not available on Android (headless, auto)"
                val size = argv.getOrNull(2)?.takeIf { Regex("\\d+x\\d+").matches(it) } ?: "1920x1080"
                specs += "$size/160"
            }
            "remove", "destroy" -> {
                val id = argv.getOrNull(1)?.removePrefix("DISPLAY-")?.toIntOrNull() ?: return "error: output remove DISPLAY-N"
                val name = displays.getDisplay(id)?.name ?: return "error: no such output"
                val n = Regex("#(\\d+)").find(name)?.groupValues?.get(1)?.toIntOrNull()
                    ?: return "error: only outputs made with `output create` can be removed"
                if (n < 1 || n > specs.size) return "error: no such output"
                specs.removeAt(n - 1)
            }
            else -> return "error: output create|remove"
        }
        Settings.Global.putString(resolver, Settings.Global.OVERLAY_DISPLAY_DEVICES, specs.joinToString(";"))
        return "ok"
    }

    // ---- kill mode ----

    /**
     * `hyprctl kill`: the next click (or tap) on a window force-stops its app, as Hyprland's
     * kill mode; a right click or [KILL_TIMEOUT_MS] without one leaves the mode.
     */
    private fun startKillMode(displayId: Int) {
        kill?.stop()
        kill = KillMode(displayId)
    }

    private inner class KillMode(private val displayId: Int) {
        private val monitor: InputMonitor = inputManager.monitorGestureInput("hypr-kill", displayId)
        private val receiver = object : InputEventReceiver(monitor.inputChannel, Looper.myLooper()) {
            override fun onInputEvent(event: InputEvent) {
                var handled = false
                if (event is MotionEvent && event.actionMasked == MotionEvent.ACTION_DOWN) {
                    handled = true
                    monitor.pilferPointers()
                    if (!event.isButtonPressed(MotionEvent.BUTTON_SECONDARY)) {
                        val target = controller.windowAt(displayId, event.rawX, event.rawY)
                        if (target == null) Log.i(TAG, "kill: no window at ${event.rawX},${event.rawY}")
                        target?.let { task ->
                            Log.i(TAG, "kill: task ${task.taskId}")
                            emit("kill", ipcEventAddress(task.taskId))
                            HyprHostActions.forceStop(context, task)
                        }
                    }
                    mainExecutor.execute { stop() }
                }
                finishInputEvent(event, handled)
            }
        }
        private val timeout = Runnable { stop() }

        init {
            mainExecutor.executeDelayed(timeout, KILL_TIMEOUT_MS)
        }

        fun stop() {
            mainExecutor.removeCallbacks(timeout)
            if (kill === this) kill = null
            receiver.dispose()
            monitor.dispose()
        }
    }

    companion object {
        private const val TAG = "HyprIpc"
        const val REQUEST_SOCKET = "hypr/.socket.sock"
        const val EVENT_SOCKET = "hypr/.socket2.sock"
        /** HYPRLAND_INSTANCE_SIGNATURE: one instance per device. */
        const val SIGNATURE = "android"
        /** The Hyprland whose Lua API and IPC this shell follows. */
        const val HYPRLAND_VERSION = "0.57.0"
        const val PROP_ALLOW = "persist.hypr.ipc_allow"
        const val DEFAULT_ALLOW = "com.termux,com.android.hyprlauncher"
        private const val DENIED = "permission denied\n"
        private const val WORKERS = 4
        private const val MAX_TASKS = 100
        private const val MAX_REQUEST = 1 shl 20
        private const val FIRST_BYTE_TIMEOUT_MS = 5000
        private const val MORE_BYTES_TIMEOUT_MS = 100
        private const val REQUEST_TIMEOUT_MS = 5000L
        private const val KEYWORD_QUIET_MS = 2000L
        private const val KILL_TIMEOUT_MS = 15000L
        private val SELECTOR = Regex("^(address|class|title|initialclass|initialtitle|pid):")
    }
}
