package com.android.hyprtile

import java.util.Locale

/*
 * Hyprland's IPC (Phase E12, docs/hypr-ipc.md): the data PcSystemUI collects about windows,
 * workspaces and monitors, and Hyprland's request and event formats for it, as hyprctl prints
 * them (text and `-j` JSON with Hyprland's field names). No Android types, so the formats are
 * unit-tested on the host; HyprIpc in PcSystemUI fills these classes and serves the sockets.
 */

/** A workspace as Hyprland names it: `id` and `name` (special workspaces: negative ids). */
data class IpcWorkspaceRef(val id: Int, val name: String) {
    companion object {
        val NONE = IpcWorkspaceRef(0, "")
    }
}

/**
 * One window (an Android task). [fullscreen] is Hyprland's mode: 0 none, 1 maximized, 2
 * fullscreen. [focusHistoryId] is the place in the focus history (0 = focused), -1 when not in
 * it. [hidden] is a minimized window.
 */
data class IpcWindow(
    val taskId: Int,
    val cls: String,
    val title: String,
    val x: Int,
    val y: Int,
    val width: Int,
    val height: Int,
    val workspace: IpcWorkspaceRef,
    val floating: Boolean,
    val pinned: Boolean,
    val fullscreen: Int,
    val hidden: Boolean,
    val monitor: Int,
    val pid: Int,
    val focusHistoryId: Int,
) {
    /** Hyprland's window address: here the task id in hex (`address:0x<hex>` selects it). */
    val address: String get() = ipcAddress(taskId)
}

/** `0x` + [id] in hex: windows by task id, devices by input device id. */
fun ipcAddress(id: Int): String = "0x" + Integer.toHexString(id)

/** A window's address in events: hex without `0x`, as Hyprland prints `{:x}`. */
fun ipcEventAddress(id: Int): String = Integer.toHexString(id)

data class IpcWorkspace(
    val ref: IpcWorkspaceRef,
    val monitor: String,
    val monitorId: Int,
    val windows: Int,
    val hasFullscreen: Boolean,
    /** Task id of the workspace's last focused window, -1 for none. */
    val lastWindow: Int,
    val lastWindowTitle: String,
    val persistent: Boolean,
    val layout: String,
)

data class IpcMonitor(
    val id: Int,
    val name: String,
    val description: String,
    val make: String,
    val model: String,
    val serial: String,
    val width: Int,
    val height: Int,
    val physicalWidth: Int,
    val physicalHeight: Int,
    val refreshRate: Float,
    val x: Int,
    val y: Int,
    val activeWorkspace: IpcWorkspaceRef,
    val specialWorkspace: IpcWorkspaceRef,
    /** Left, top, right, bottom: the bar and system bars (Hyprland's reserved area). */
    val reserved: List<Int>,
    val scale: Float,
    val transform: Int,
    val focused: Boolean,
    val dpmsStatus: Boolean,
    val disabled: Boolean,
    val mirrorOf: String,
    val availableModes: List<String>,
)

enum class IpcDeviceKind { MOUSE, KEYBOARD, TABLET, TOUCH, SWITCH }

data class IpcDevice(
    val kind: IpcDeviceKind,
    val id: Int,
    val name: String,
    val layout: String = "",
    val keymap: String = "",
    val capsLock: Boolean = false,
    val numLock: Boolean = false,
    val main: Boolean = false,
)

/** A bind as `binds` lists it; [flags] are Hyprland's bind flag letters (`m` mouse, `e` repeat, ...). */
data class IpcBind(
    val mods: Set<Mod>,
    val key: String,
    val dispatcher: String,
    val arg: String,
    val flags: String = "",
    val description: String = "",
) {
    val modmask: Int get() = mods.sumOf { MODMASK.getValue(it) }

    companion object {
        /** Hyprland's (wlroots') modifier bits. */
        val MODMASK = mapOf(Mod.SHIFT to 1, Mod.CTRL to 4, Mod.ALT to 8, Mod.SUPER to 64)
    }
}

/**
 * A layer surface: HyprLauncher's bar, launcher and so on. [level] is Hyprland's 0 background,
 * 1 bottom, 2 top, 3 overlay.
 */
data class IpcLayer(
    val id: Int,
    val level: Int,
    val namespace: String,
    val x: Int,
    val y: Int,
    val width: Int,
    val height: Int,
    val pid: Int,
) {
    companion object {
        /** `level namespace x y w h pid` (HyprLauncher's answer to `layers`). */
        fun decode(id: Int, line: String): IpcLayer? {
            val f = line.trim().split(' ')
            if (f.size < 7) return null
            val n = f.drop(2).map { it.toIntOrNull() ?: return null }
            val level = f[0].toIntOrNull() ?: return null
            return IpcLayer(id, level, f[1], n[0], n[1], n[2], n[3], n[4])
        }
    }
}

data class IpcVersion(
    val version: String,
    val branch: String,
    val commit: String,
    val commitDate: String,
    val tag: String,
    val android: String,
)

/** One request: [flags] are the letters before `/` (`j` JSON, `r`, `a`, ...). */
data class IpcRequest(val flags: String, val command: String, val args: String) {
    val json: Boolean get() = 'j' in flags

    /** [args] split on spaces. */
    val argv: List<String> get() = args.trim().split(' ').filter { it.isNotEmpty() }

    companion object {
        const val BATCH = "[[BATCH]]"

        /**
         * Hyprland's `[flags]/command args`: the flags are what comes before a `/` that is
         * before the first space (`j/clients`, `/dispatch exec x`, `dispatch exec /bin/x`).
         */
        fun parse(raw: String): IpcRequest {
            var text = raw.trim()
            var flags = ""
            val slash = text.indexOf('/')
            val space = text.indexOf(' ').let { if (it < 0) text.length else it }
            if (slash in 0 until space) {
                flags = text.substring(0, slash)
                text = text.substring(slash + 1)
            }
            val sp = text.indexOf(' ')
            return if (sp < 0) IpcRequest(flags, text, "")
            else IpcRequest(flags, text.substring(0, sp), text.substring(sp + 1).trim())
        }

        /** The commands of a `[[BATCH]]` request: split on `;`, `\;` and `\\` escaped. */
        fun splitBatch(raw: String): List<String> {
            val body = raw.removePrefix(BATCH)
            val out = ArrayList<String>()
            val cur = StringBuilder()
            var i = 0
            while (i < body.length) {
                val c = body[i]
                if (c == '\\' && i + 1 < body.length && (body[i + 1] == ';' || body[i + 1] == '\\')) {
                    cur.append(body[i + 1])
                    i += 2
                    continue
                }
                if (c == ';') {
                    out += cur.toString().trim()
                    cur.clear()
                } else {
                    cur.append(c)
                }
                i++
            }
            out += cur.toString().trim()
            return out.filter { it.isNotEmpty() }
        }
    }
}

/** A minimal JSON writer with Hyprland's layout (4-space indent, objects in insertion order). */
object Json {
    fun quote(s: String): String {
        val b = StringBuilder(s.length + 2).append('"')
        for (c in s) {
            when (c) {
                '"' -> b.append("\\\"")
                '\\' -> b.append("\\\\")
                '\n' -> b.append("\\n")
                '\r' -> b.append("\\r")
                '\t' -> b.append("\\t")
                else -> if (c < ' ') b.append(String.format(Locale.ROOT, "\\u%04x", c.code)) else b.append(c)
            }
        }
        return b.append('"').toString()
    }

    /** Writes maps (objects), lists, strings, numbers, booleans and null. */
    fun write(v: Any?, indent: String = ""): String = when (v) {
        null -> "null"
        is String -> quote(v)
        is Boolean, is Int, is Long -> v.toString()
        is Float -> num(v.toDouble())
        is Double -> num(v)
        is Map<*, *> -> if (v.isEmpty()) "{}" else {
            val inner = "$indent    "
            v.entries.joinToString(",\n", "{\n", "\n$indent}") { (k, value) ->
                "$inner${quote(k.toString())}: ${write(value, inner)}"
            }
        }
        is List<*> -> if (v.isEmpty()) "[]" else
            v.joinToString(",", "[", "]") { write(it, indent) }
        else -> quote(v.toString())
    }

    private fun num(d: Double): String =
        if (d.isNaN() || d.isInfinite()) "null" else String.format(Locale.ROOT, "%.5f", d)
}

/** Hyprland's hyprctl output for the data above, text or JSON. */
object HyprCtlFormat {
    private fun b(v: Boolean) = if (v) 1 else 0

    fun windowMap(w: IpcWindow): Map<String, Any?> = linkedMapOf(
        "address" to w.address,
        "mapped" to true,
        "hidden" to w.hidden,
        "at" to listOf(w.x, w.y),
        "size" to listOf(w.width, w.height),
        "workspace" to linkedMapOf("id" to w.workspace.id, "name" to w.workspace.name),
        "floating" to w.floating,
        "pseudo" to false,
        "monitor" to w.monitor,
        "class" to w.cls,
        "title" to w.title,
        "initialClass" to w.cls,
        "initialTitle" to w.title,
        "pid" to w.pid,
        "xwayland" to false,
        "pinned" to w.pinned,
        "fullscreen" to w.fullscreen,
        "fullscreenClient" to w.fullscreen,
        "grouped" to emptyList<String>(),
        "tags" to emptyList<String>(),
        "swallowing" to "0x0",
        "focusHistoryID" to w.focusHistoryId,
        "inhibitingIdle" to false,
        "xdgTag" to "",
        "xdgDescription" to "",
        "contentType" to "none",
    )

    fun windowText(w: IpcWindow): String = buildString {
        append("Window ${Integer.toHexString(w.taskId)} -> ${w.title}:\n")
        append("\tmapped: 1\n\thidden: ${b(w.hidden)}\n")
        append("\tat: ${w.x},${w.y}\n\tsize: ${w.width},${w.height}\n")
        append("\tworkspace: ${w.workspace.id} (${w.workspace.name})\n")
        append("\tfloating: ${b(w.floating)}\n\tpseudo: 0\n\tmonitor: ${w.monitor}\n")
        append("\tclass: ${w.cls}\n\ttitle: ${w.title}\n")
        append("\tinitialClass: ${w.cls}\n\tinitialTitle: ${w.title}\n")
        append("\tpid: ${w.pid}\n\txwayland: 0\n\tpinned: ${b(w.pinned)}\n")
        append("\tfullscreen: ${w.fullscreen}\n\tfullscreenClient: ${w.fullscreen}\n")
        append("\tgrouped: 0\n\ttags: \n\tswallowing: 0\n")
        append("\tfocusHistoryID: ${w.focusHistoryId}\n\tinhibitingIdle: 0\n")
        append("\txdgTag: \n\txdgDescription: \n\tcontentType: none\n\n")
    }

    fun clients(windows: List<IpcWindow>, json: Boolean): String =
        if (json) Json.write(windows.map(::windowMap)) else windows.joinToString("") { windowText(it) }

    /** `activewindow`: Hyprland prints `Invalid` (JSON `{}`) without one. */
    fun activeWindow(w: IpcWindow?, json: Boolean): String = when {
        w == null -> if (json) "{}" else "Invalid\n"
        json -> Json.write(windowMap(w))
        else -> windowText(w)
    }

    fun workspaceMap(ws: IpcWorkspace): Map<String, Any?> = linkedMapOf(
        "id" to ws.ref.id,
        "name" to ws.ref.name,
        "monitor" to ws.monitor,
        "monitorID" to ws.monitorId,
        "windows" to ws.windows,
        "hasfullscreen" to ws.hasFullscreen,
        "lastwindow" to if (ws.lastWindow < 0) "0x0" else ipcAddress(ws.lastWindow),
        "lastwindowtitle" to ws.lastWindowTitle,
        "ispersistent" to ws.persistent,
        "tiledLayout" to ws.layout,
    )

    fun workspaceText(ws: IpcWorkspace): String = buildString {
        append("workspace ID ${ws.ref.id} (${ws.ref.name}) on monitor ${ws.monitor}:\n")
        append("\tmonitorID: ${ws.monitorId}\n\twindows: ${ws.windows}\n")
        append("\thasfullscreen: ${b(ws.hasFullscreen)}\n")
        append("\tlastwindow: ${if (ws.lastWindow < 0) "0x0" else ipcAddress(ws.lastWindow)}\n")
        append("\tlastwindowtitle: ${ws.lastWindowTitle}\n")
        append("\tispersistent: ${b(ws.persistent)}\n\ttiledLayout: ${ws.layout}\n\n")
    }

    fun workspaces(list: List<IpcWorkspace>, json: Boolean): String =
        if (json) Json.write(list.map(::workspaceMap)) else list.joinToString("") { workspaceText(it) }

    fun activeWorkspace(ws: IpcWorkspace?, json: Boolean): String = when {
        ws == null -> if (json) "{}" else "Invalid\n"
        json -> Json.write(workspaceMap(ws))
        else -> workspaceText(ws)
    }

    private fun f2(v: Float) = String.format(Locale.ROOT, "%.2f", v)
    private fun f5(v: Float) = String.format(Locale.ROOT, "%.5f", v)

    fun monitorMap(m: IpcMonitor): Map<String, Any?> = linkedMapOf(
        "id" to m.id,
        "name" to m.name,
        "description" to m.description,
        "make" to m.make,
        "model" to m.model,
        "serial" to m.serial,
        "width" to m.width,
        "height" to m.height,
        "physicalWidth" to m.physicalWidth,
        "physicalHeight" to m.physicalHeight,
        "refreshRate" to m.refreshRate,
        "x" to m.x,
        "y" to m.y,
        "activeWorkspace" to linkedMapOf("id" to m.activeWorkspace.id, "name" to m.activeWorkspace.name),
        "specialWorkspace" to linkedMapOf("id" to m.specialWorkspace.id, "name" to m.specialWorkspace.name),
        "reserved" to m.reserved,
        "scale" to m.scale,
        "transform" to m.transform,
        "focused" to m.focused,
        "dpmsStatus" to m.dpmsStatus,
        "vrr" to false,
        "solitary" to "0",
        "activelyTearing" to false,
        "directScanoutTo" to "0",
        "disabled" to m.disabled,
        "currentFormat" to "XRGB8888",
        "mirrorOf" to m.mirrorOf,
        "availableModes" to m.availableModes,
    )

    fun monitorText(m: IpcMonitor): String = buildString {
        append("Monitor ${m.name} (ID ${m.id}):\n")
        append("\t${m.width}x${m.height}@${f5(m.refreshRate)} at ${m.x}x${m.y}\n")
        append("\tdescription: ${m.description}\n\tmake: ${m.make}\n\tmodel: ${m.model}\n")
        append("\tphysical size (mm): ${m.physicalWidth}x${m.physicalHeight}\n")
        append("\tserial: ${m.serial}\n")
        append("\tactive workspace: ${m.activeWorkspace.id} (${m.activeWorkspace.name})\n")
        append("\tspecial workspace: ${m.specialWorkspace.id} (${m.specialWorkspace.name})\n")
        append("\treserved: ${m.reserved.joinToString(" ")}\n")
        append("\tscale: ${f2(m.scale)}\n\ttransform: ${m.transform}\n")
        append("\tfocused: ${if (m.focused) "yes" else "no"}\n")
        append("\tdpmsStatus: ${b(m.dpmsStatus)}\n\tvrr: false\n\tsolitary: 0\n")
        append("\tactivelyTearing: false\n\tdirectScanoutTo: 0\n\tdisabled: ${m.disabled}\n")
        append("\tcurrentFormat: XRGB8888\n\tmirrorOf: ${m.mirrorOf}\n")
        append("\tavailableModes: ${m.availableModes.joinToString(" ")}\n\n")
    }

    fun monitors(list: List<IpcMonitor>, json: Boolean): String =
        if (json) Json.write(list.map(::monitorMap)) else list.joinToString("") { monitorText(it) }

    fun devices(list: List<IpcDevice>, json: Boolean): String {
        fun of(k: IpcDeviceKind) = list.filter { it.kind == k }
        if (json) {
            return Json.write(linkedMapOf(
                "mice" to of(IpcDeviceKind.MOUSE).map {
                    linkedMapOf("address" to ipcAddress(it.id), "name" to it.name, "defaultSpeed" to 0.0f, "scrollFactor" to 1.0f)
                },
                "keyboards" to of(IpcDeviceKind.KEYBOARD).map {
                    linkedMapOf(
                        "address" to ipcAddress(it.id), "name" to it.name, "rules" to "", "model" to "",
                        "layout" to it.layout, "variant" to "", "options" to "",
                        "active_layout_index" to 0, "active_keymap" to it.keymap,
                        "capsLock" to it.capsLock, "numLock" to it.numLock, "main" to it.main,
                    )
                },
                "tablets" to of(IpcDeviceKind.TABLET).map { linkedMapOf("address" to ipcAddress(it.id), "type" to "tabletTool", "name" to it.name) },
                "touch" to of(IpcDeviceKind.TOUCH).map { linkedMapOf("address" to ipcAddress(it.id), "name" to it.name) },
                "switches" to of(IpcDeviceKind.SWITCH).map { linkedMapOf("address" to ipcAddress(it.id), "name" to it.name) },
            ))
        }
        return buildString {
            append("mice:\n")
            for (d in of(IpcDeviceKind.MOUSE)) {
                append("\tMouse at ${ipcAddress(d.id)}:\n\t\t${d.name}\n\t\t\tdefault speed: 0.00000\n\t\t\tscroll factor: 1.00\n")
            }
            append("\n\nKeyboards:\n")
            for (d in of(IpcDeviceKind.KEYBOARD)) {
                append("\tKeyboard at ${ipcAddress(d.id)}:\n\t\t${d.name}\n")
                append("\t\t\trules: r \"\", m \"\", l \"${d.layout}\", v \"\", o \"\"\n")
                append("\t\t\tactive layout index: 0\n\t\t\tactive keymap: ${d.keymap}\n")
                append("\t\t\tcapsLock: ${if (d.capsLock) "yes" else "no"}\n")
                append("\t\t\tnumLock: ${if (d.numLock) "yes" else "no"}\n")
                append("\t\t\tmain: ${if (d.main) "yes" else "no"}\n")
            }
            append("\n\nTablets:\n")
            for (d in of(IpcDeviceKind.TABLET)) append("\tTablet at ${ipcAddress(d.id)}:\n\t\t${d.name}\n")
            append("\n\nTouch:\n")
            for (d in of(IpcDeviceKind.TOUCH)) append("\tTouch Device at ${ipcAddress(d.id)}:\n\t\t${d.name}\n")
            append("\n\nSwitches:\n")
            for (d in of(IpcDeviceKind.SWITCH)) append("\tSwitch Device at ${ipcAddress(d.id)}:\n\t\t${d.name}\n")
            append("\n")
        }
    }

    fun binds(list: List<IpcBind>, json: Boolean): String {
        if (json) {
            return Json.write(list.map {
                linkedMapOf(
                    "locked" to ('l' in it.flags), "mouse" to ('m' in it.flags),
                    "release" to ('r' in it.flags), "repeat" to ('e' in it.flags),
                    "longPress" to ('o' in it.flags), "non_consuming" to ('n' in it.flags),
                    "has_description" to it.description.isNotEmpty(), "modmask" to it.modmask,
                    "submap" to "", "submap_universal" to false, "key" to it.key, "keycode" to 0,
                    "catch_all" to false, "description" to it.description,
                    "dispatcher" to it.dispatcher, "arg" to it.arg,
                )
            })
        }
        return list.joinToString("") {
            "bind${it.flags}\n\tmodmask: ${it.modmask}\n\tsubmap: \n\tkey: ${it.key}\n\tkeycode: 0\n" +
                "\tcatchall: false\n\tdescription: ${it.description}\n\tdispatcher: ${it.dispatcher}\n" +
                "\targ: ${it.arg}\n\n"
        }
    }

    private val LEVELS = listOf("background", "bottom", "top", "overlay")

    fun layers(byMonitor: Map<String, List<IpcLayer>>, json: Boolean): String {
        if (json) {
            return Json.write(byMonitor.mapValues { (_, layers) ->
                linkedMapOf("levels" to (0..3).associate { lvl ->
                    "$lvl" to layers.filter { it.level == lvl }.map {
                        linkedMapOf(
                            "address" to ipcAddress(it.id), "x" to it.x, "y" to it.y, "w" to it.width,
                            "h" to it.height, "namespace" to it.namespace, "pid" to it.pid,
                        )
                    }
                })
            })
        }
        return buildString {
            for ((mon, layers) in byMonitor) {
                append("Monitor $mon:\n")
                for (lvl in 0..3) {
                    append("\tLayer level $lvl (${LEVELS[lvl]}):\n")
                    for (l in layers.filter { it.level == lvl }) {
                        append("\t\tLayer ${Integer.toHexString(l.id)}: xywh: ${l.x} ${l.y} ${l.width} ${l.height}, " +
                            "namespace: ${l.namespace}, pid: ${l.pid}\n")
                    }
                    append("\n")
                }
                append("\n")
            }
        }
    }

    fun version(v: IpcVersion, json: Boolean): String {
        if (json) {
            return Json.write(linkedMapOf(
                "branch" to v.branch, "commit" to v.commit, "version" to v.version, "dirty" to false,
                "commit_message" to "Hypr-Android shell on ${v.android}", "commit_date" to v.commitDate,
                "tag" to v.tag, "commits" to "0", "buildAquamarine" to "", "buildHyprlang" to "",
                "buildHyprutils" to "", "buildHyprcursor" to "", "buildHyprgraphics" to "",
                "systemAquamarine" to "", "systemHyprlang" to "", "systemHyprutils" to "",
                "systemHyprcursor" to "", "systemHyprgraphics" to "", "abiHash" to v.commit,
                "flags" to listOf("android"),
            ))
        }
        return "Hyprland ${v.version} built from branch ${v.branch} at commit ${v.commit}  " +
            "(Hypr-Android shell on ${v.android}).\nDate: ${v.commitDate}\nTag: ${v.tag}, commits: 0\n\n" +
            "flags set:\nandroid\n"
    }

    fun cursorPos(x: Int, y: Int, json: Boolean): String =
        if (json) Json.write(linkedMapOf("x" to x, "y" to y)) else "$x, $y\n"

    /**
     * `getoption`: [type] is `int`, `float`, `str` or `custom`; [set] whether the configuration
     * sets it (else it is the default).
     */
    fun option(name: String, type: String, value: String, set: Boolean, json: Boolean): String {
        if (json) {
            val v: Any = when (type) {
                "int" -> value.toLongOrNull() ?: 0L
                "float" -> value.toDoubleOrNull() ?: 0.0
                else -> value
            }
            return Json.write(linkedMapOf("option" to name, type to v, "set" to set))
        }
        val shown = when (type) {
            "float" -> String.format(Locale.ROOT, "%.6f", value.toDoubleOrNull() ?: 0.0)
            else -> value
        }
        return "${if (type == "custom") "custom type" else type}: $shown\nset: $set\n"
    }

    /** One instance: Hyprland's `instances`. */
    fun instances(signature: String, time: Long, pid: Int, json: Boolean): String =
        if (json) Json.write(listOf(linkedMapOf("instance" to signature, "time" to time, "pid" to pid, "wl_socket" to "")))
        else "instance $signature:\n\ttime: $time\n\tpid: $pid\n\twl socket: \n\n"

    /** A plain answer as Hyprland wraps strings for `-j` where it has a JSON form. */
    fun plain(text: String, json: Boolean, key: String): String =
        if (json) Json.write(linkedMapOf(key to text)) else "$text\n"

    val SPLASHES = listOf(
        "Tiling on a phone OS, because why not.",
        "Android underneath, Hyprland on top.",
        "Every window is a task, every task a window.",
        "Built with Soong, configured with Lua.",
        "hyprctl, now with binder underneath.",
        "Workspaces are desks, desks are workspaces.",
        "Sockets in the abstract namespace, no XDG_RUNTIME_DIR needed.",
        "Nobody asked for a btop on Android. Here it is anyway.",
    )
}

/**
 * What an event diff compares: the state of one display (HyprShell's push), in Hyprland's
 * terms. [windows] in any order.
 */
data class IpcSnapshot(
    val monitor: String,
    val active: IpcWorkspaceRef,
    val workspaces: List<IpcWorkspaceRef>,
    val special: IpcWorkspaceRef?,
    val windows: List<IpcWindow>,
    val focused: Int,
    val urgent: Set<Int>,
)

/** Hyprland's event socket lines (`EVENT>>DATA`) for what changed between two snapshots. */
object IpcEvents {
    fun diff(old: IpcSnapshot?, new: IpcSnapshot): List<Pair<String, String>> {
        val out = ArrayList<Pair<String, String>>()
        if (old == null) return out
        val oldWs = old.workspaces.associateBy { it.id }
        val newWs = new.workspaces.associateBy { it.id }
        for (ws in new.workspaces) {
            if (ws.id !in oldWs) {
                out += "createworkspace" to ws.name
                out += "createworkspacev2" to "${ws.id},${ws.name}"
            }
        }
        if (new.active != old.active && new.active.id != 0) {
            out += "workspace" to new.active.name
            out += "workspacev2" to "${new.active.id},${new.active.name}"
        }
        if (new.special != old.special) {
            val s = new.special
            out += "activespecial" to "${s?.name ?: ""},${new.monitor}"
            out += "activespecialv2" to "${s?.id?.toString() ?: ""},${s?.name ?: ""},${new.monitor}"
        }
        val before = old.windows.associateBy { it.taskId }
        val after = new.windows.associateBy { it.taskId }
        for (w in new.windows) {
            val a = ipcEventAddress(w.taskId)
            val o = before[w.taskId]
            if (o == null) {
                out += "openwindow" to "$a,${w.workspace.name},${w.cls},${w.title}"
                continue
            }
            if (o.workspace != w.workspace) {
                out += "movewindow" to "$a,${w.workspace.name}"
                out += "movewindowv2" to "$a,${w.workspace.id},${w.workspace.name}"
            }
            if (o.floating != w.floating) out += "changefloatingmode" to "$a,${if (w.floating) 1 else 0}"
            if (o.pinned != w.pinned) out += "pin" to "$a,${if (w.pinned) 1 else 0}"
            if ((o.fullscreen != 0) != (w.fullscreen != 0)) out += "fullscreen" to if (w.fullscreen != 0) "1" else "0"
            if (o.hidden != w.hidden) out += "minimized" to "$a,${if (w.hidden) 1 else 0}"
            if (o.title != w.title) {
                out += "windowtitle" to a
                out += "windowtitlev2" to "$a,${w.title}"
            }
        }
        for (w in old.windows) if (w.taskId !in after) out += "closewindow" to ipcEventAddress(w.taskId)
        val focusedNow = after[new.focused]
        val focusedBefore = before[old.focused]
        if (new.focused != old.focused || focusedNow?.title != focusedBefore?.title) {
            out += "activewindow" to (focusedNow?.let { "${it.cls},${it.title}" } ?: ",")
            if (new.focused != old.focused) {
                out += "activewindowv2" to (focusedNow?.let { ipcEventAddress(it.taskId) } ?: "")
            }
        }
        for (id in new.urgent) if (id !in old.urgent) out += "urgent" to ipcEventAddress(id)
        for (ws in old.workspaces) {
            if (ws.id !in newWs) {
                out += "destroyworkspace" to ws.name
                out += "destroyworkspacev2" to "${ws.id},${ws.name}"
            }
        }
        return out
    }
}
