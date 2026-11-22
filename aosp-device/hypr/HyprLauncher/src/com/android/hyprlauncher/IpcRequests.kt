package com.android.hyprlauncher

import android.graphics.Color
import android.os.Process
import android.os.RemoteException
import android.util.Log
import android.view.View
import android.view.WindowManager
import android.view.WindowManagerGlobal
import com.android.hyprlua.HyprLua
import com.android.hyprshell.IHyprReply
import com.android.hyprtile.HyprCtlFormat
import com.android.hyprtile.Json

/**
 * The hyprctl requests PcSystemUI's IPC hands the launcher (Phase E12, docs/hypr-ipc.md,
 * IHyprShellListener.onRequest): what lives here and not in the shell. The Lua configuration
 * (`eval`, `repl`, `keyword`, `getoption`, `configerrors`, `descriptions`) runs on the Lua
 * thread; the notifications (`notify`, `dismissnotify`, `seterror`) and the layer surfaces
 * (`layers`) on the main thread. Every request gets one reply.
 */
class IpcRequests(private val desktop: Desktop) {

    fun handle(command: String, args: String, json: Boolean, reply: IHyprReply) {
        fun send(text: String) {
            try {
                reply.reply(text)
            } catch (e: RemoteException) {
                Log.w(TAG, "reply to $command: $e")
            }
        }
        when (command) {
            "eval" -> desktop.lua.withRuntime { send(it.eval(args, repl = false)) }
            "repl" -> desktop.lua.withRuntime { send(it.eval(args, repl = true)) }
            "keyword" -> desktop.lua.withRuntime { send(keyword(it, args)) }
            "getoption" -> desktop.lua.withRuntime { send(option(it, args.trim(), json)) }
            "descriptions" -> desktop.lua.withRuntime { send(descriptions(it)) }
            "configerrors" -> {
                val errors = desktop.lua.errors()
                send(if (json) Json.write(errors) else errors.joinToString("\n", postfix = "\n"))
            }
            "notify" -> desktop.handler.post { send(notify(args)) }
            "dismissnotify" -> desktop.handler.post {
                desktop.notifier.dismiss(args.trim().ifEmpty { "-1" }.toIntOrNull() ?: -1)
                send("ok")
            }
            "seterror" -> desktop.handler.post { send(setError(args)) }
            "layers" -> desktop.handler.post { send(layers()) }
            else -> send("unknown request")
        }
    }

    private fun keyword(lua: HyprLua, args: String): String {
        val key = args.trim().substringBefore(' ')
        val value = args.trim().substringAfter(' ', "")
        if (key.isEmpty()) return "error: keyword SECTION:OPTION VALUE"
        return lua.keyword(key, value)?.let { "error: $it" } ?: "ok"
    }

    /** Hyprland's getoption types: int (booleans too), float, str, custom (colors, gradients). */
    private fun option(lua: HyprLua, key: String, json: Boolean): String {
        val value = lua.option(key) ?: return "no such option"
        val (type, shown) = typed(value)
        return HyprCtlFormat.option(key.replace('.', ':'), type, shown, lua.optionSet(key), json)
    }

    private fun typed(value: String): Pair<String, String> = when {
        value == "true" || value == "false" -> "int" to if (value == "true") "1" else "0"
        value.toLongOrNull() != null -> "int" to value
        value.toDoubleOrNull() != null -> "float" to value
        value.startsWith("0x") || value.startsWith("rgb") -> "custom" to value
        else -> "str" to value
    }

    private fun descriptions(lua: HyprLua): String = Json.write(lua.options().map { (k, v) ->
        val (type, shown) = typed(v)
        linkedMapOf(
            "value" to k.replace('.', ':'),
            "description" to "",
            "type" to when (type) { "int" -> 1; "float" -> 2; "str" -> 3; else -> 6 },
            "flags" to 0,
            "data" to linkedMapOf("default" to shown, "current" to shown, "explicit" to false),
        )
    })

    /**
     * `notify ICON TIME_MS COLOR MESSAGE`: Hyprland's icons (-1 none, 0 warning, 1 info, 2
     * hint, 3 error, 4 confused, 5 ok) as a symbol before the text, COLOR `0` for the default,
     * `fontsize:N` before the message.
     */
    private fun notify(args: String): String {
        val f = args.trim().split(' ', limit = 4)
        if (f.size < 4) return "error: notify ICON TIME_MS COLOR MESSAGE"
        val icon = f[0].toIntOrNull() ?: return "error: bad icon ${f[0]}"
        val time = f[1].toLongOrNull()?.takeIf { it > 0 } ?: return "error: bad time ${f[1]}"
        val color = if (f[2] == "0") null else parseColor(f[2]) ?: return "error: bad color ${f[2]}"
        var text = f[3]
        var size = 13f
        Regex("^fontsize:(\\d+)\\s*").find(text)?.let {
            size = it.groupValues[1].toFloat().coerceIn(6f, 64f)
            text = text.substring(it.range.last + 1)
        }
        val symbol = ICONS[icon]
        desktop.notifier.notify(if (symbol != null) "$symbol  $text" else text, time, color ?: ICON_COLORS[icon], size)
        return "ok"
    }

    /** `seterror COLOR TEXT`, or `seterror disable`. */
    private fun setError(args: String): String {
        val a = args.trim()
        if (a == "disable" || a.isEmpty()) {
            desktop.notifier.setCustomError(null, null)
            return "ok"
        }
        val color = parseColor(a.substringBefore(' ')) ?: return "error: bad color ${a.substringBefore(' ')}"
        desktop.notifier.setCustomError(a.substringAfter(' ', ""), color)
        return "ok"
    }

    /**
     * The launcher's own windows as Hyprland's layer surfaces (`level namespace x y w h pid`,
     * one per line): the bar on top, the panels and messages as overlays, the desktop behind.
     */
    private fun layers(): String {
        val wmg = WindowManagerGlobal.getInstance()
        val out = StringBuilder()
        for (name in wmg.viewRootNames) {
            val view: View = wmg.getRootView(name) ?: continue
            if (!view.isAttachedToWindow || !view.isShown || view.width <= 0) continue
            val lp = view.layoutParams as? WindowManager.LayoutParams ?: continue
            val (level, ns) = LAYERS[lp.title?.toString()] ?: if (lp.type == WindowManager.LayoutParams.TYPE_BASE_APPLICATION) {
                0 to "desktop"
            } else {
                continue
            }
            val loc = IntArray(2)
            view.getLocationOnScreen(loc)
            out.append("$level $ns ${loc[0]} ${loc[1]} ${view.width} ${view.height} ${Process.myPid()}\n")
        }
        return out.toString()
    }

    companion object {
        private const val TAG = "HyprIpc"

        private val LAYERS = mapOf(
            "HyprBar" to (2 to "bar"),
            "HyprNotifications" to (3 to "notifications"),
            "HyprLauncher" to (3 to "launcher"),
            "HyprOverview" to (3 to "overview"),
            "HyprAltTab" to (3 to "alttab"),
        )

        private val ICONS = mapOf(0 to "⚠", 1 to "ℹ", 2 to "💡", 3 to "✖", 4 to "?", 5 to "✔")
        private val ICON_COLORS = mapOf(
            0 to 0xFFF9E2AF.toInt(), 1 to 0xFF89B4FA.toInt(), 2 to 0xFF94E2D5.toInt(),
            3 to 0xFFF38BA8.toInt(), 4 to 0xFFCBA6F7.toInt(), 5 to 0xFFA6E3A1.toInt(),
        )

        /** Hyprland's colors: `rgb(rrggbb)`, `rgba(rrggbbaa)`, `0xAARRGGBB`, `#rrggbb`. */
        fun parseColor(text: String): Int? {
            val t = text.trim().lowercase()
            Regex("^rgba?\\(([0-9a-f]{6})([0-9a-f]{2})?\\)$").find(t)?.let {
                val rgb = it.groupValues[1].toLong(16)
                val a = it.groupValues[2].ifEmpty { "ff" }.toLong(16)
                return ((a shl 24) or rgb).toInt()
            }
            if (t.startsWith("0x")) return t.substring(2).toLongOrNull(16)?.toInt()
            return runCatching { Color.parseColor(t) }.getOrNull()
        }
    }
}
