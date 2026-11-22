package com.android.hyprlauncher

import android.content.Context
import android.hardware.display.DisplayManager
import android.hardware.input.InputManager
import android.hardware.input.KeyGestureEvent
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.RemoteException
import android.util.Log
import android.util.TypedValue
import android.view.Display
import com.android.hyprlua.BarConfig
import com.android.hyprtile.Bind
import com.android.hyprtile.Mod
import com.android.hyprtile.defaultBinds
import com.android.hyprtile.parseBind
import com.android.hyprshell.IHyprShell
import com.android.hyprshell.IHyprShellListener
import java.util.concurrent.Executors

/**
 * HyprLauncher's desktop on the default display (Phase D4): the bar, the app launcher and the
 * overview, and the connection to PcSystemUI's IHyprShell; the Lua configuration and its
 * messages (Phase D5). One per process, used on the main thread; calls into the shell go
 * through a background thread so the UI never waits on SystemUI.
 */
class Desktop private constructor(context: Context) {
    val context: Context = context.applicationContext
    val handler = Handler(Looper.getMainLooper())
    val display: Display = this.context.getSystemService(DisplayManager::class.java)
        .getDisplay(Display.DEFAULT_DISPLAY)
    val apps = AppCatalog(this.context)
    @Volatile
    var state = ShellState.EMPTY
        private set

    private val shellThread = Executors.newSingleThreadExecutor()
    @Volatile
    private var shell: IHyprShell? = null
    private var started = false

    val bar = Bar(this)
    val launcher = AppLauncher(this)
    val overview = Overview(this)
    val altTab = AltTabSwitcher(this)
    val notifier = Notifier(this)
    private val wallpapers = Wallpapers(this)
    val lua = LuaConfig(this)

    private val listener = object : IHyprShellListener.Stub() {
        override fun onStateChanged(displayId: Int, state: Bundle) {
            if (displayId != Display.DEFAULT_DISPLAY) return
            val parsed = ShellState.from(state)
            handler.post { setState(parsed) }
        }

        override fun onEvent(name: String, data: String) {
            Log.d(TAG, "event $name>>$data")
            lua.onEvent(name, data)
        }

        override fun onDispatch(name: String, arg: String) {
            handler.post {
                when (name) {
                    "launcher" -> launcher.toggle()
                    "overview", "hyprexpo:expo" -> overview.toggle()
                    "lua" -> lua.callBind(arg)
                    "reload" -> lua.reloadNow()
                    "wallpaper" -> wallpapers.dispatch(arg)
                }
            }
        }
    }

    /** Shows the bar; safe to call again. */
    fun start() {
        if (started) return
        started = true
        apps.load()
        bar.show()
        notifier.setTop(bar.reservedTop)
        registerKeyGestures()
        lua.start()
    }

    /**
     * The system key gestures a recents provider handles in AOSP 17 (Quickstep's
     * QuickstepKeyGestureEventsManager): Alt+Tab (RECENT_APPS_SWITCHER), META+Tab (RECENT_APPS)
     * and the all-apps key or META alone (ALL_APPS: the launcher). Each type has one handler, so
     * they are registered one by one; one that someone else holds is skipped.
     *
     * InputManager keeps ALT + Tab (and ALT + SHIFT + Tab) and META + Tab for these gestures, so
     * HyprKeybinds cannot install binds on them. ALT + Tab is [AltTabSwitcher]'s (Phase E4,
     * `shell.alt_tab`: the switcher, or the bind on `ALT + Tab` as in E3, ...); a bind on
     * `SUPER + Tab` runs here when Tab goes down, without one META+Tab toggles the overview.
     */
    private fun registerKeyGestures() {
        val im = context.getSystemService(InputManager::class.java) ?: return
        fun handle(event: KeyGestureEvent) {
            val start = event.action == KeyGestureEvent.ACTION_GESTURE_START
            val complete = event.action == KeyGestureEvent.ACTION_GESTURE_COMPLETE && !event.isCancelled
            handler.post {
                when (event.keyGestureType) {
                    KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS_SWITCHER ->
                        altTab.onGesture(start, cancelled = event.isCancelled)
                    KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS -> if (complete) {
                        tabBind(setOf(Mod.SUPER))?.let { runBind(it) } ?: overview.toggle()
                    }
                    KeyGestureEvent.KEY_GESTURE_TYPE_ALL_APPS -> if (complete) launcher.toggle()
                }
            }
        }
        for (type in listOf(
            KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS_SWITCHER,
            KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS,
            KeyGestureEvent.KEY_GESTURE_TYPE_ALL_APPS,
        )) {
            try {
                im.registerKeyGestureEventHandler(listOf(type)) { event, _ -> handle(event) }
            } catch (e: Exception) {
                Log.w(TAG, "key gesture $type: $e")
            }
        }
    }

    /**
     * SystemUI (re)connected: [binder] is IHyprShell from its onInitialize() Bundle, or null when
     * PcSystemUI does not provide it (AOSP's SystemUI): the bar then shows the clock and tray only.
     */
    fun attach(binder: IBinder?) {
        val s = binder?.let { IHyprShell.Stub.asInterface(it) }
        shell = s
        Log.i(TAG, "IHyprShell ${if (s != null) "connected" else "missing"}")
        if (s == null) return
        val top = bar.reservedTop
        shellCall {
            it.addListener(listener)
            it.setReserved(Display.DEFAULT_DISPLAY, top, 0)
        }
        lua.onShellAttached()
    }

    /** The bar's part of init.lua; a new height moves the reserved strip and the messages. */
    fun applyBar(cfg: BarConfig, workspaceNames: Map<Int, String>) {
        if (bar.configure(cfg, workspaceNames)) {
            val top = bar.reservedTop
            notifier.setTop(top)
            shellCall { it.setReserved(Display.DEFAULT_DISPLAY, top, 0) }
        }
    }

    /**
     * The bind on [mods] + Tab: from init.lua (its list has the defaults in it), or the default
     * binds before init.lua loaded.
     */
    private fun tabBind(mods: Set<Mod>): Bind? {
        val binds = lua.current?.wire?.lists?.get("binds")
            ?.mapNotNull { runCatching { parseBind(it) }.getOrNull() }
            ?: defaultBinds(setOf(Mod.ALT))
        return binds.lastOrNull { it.key == "TAB" && it.mods == mods }
    }

    /** Whether the configuration binds `ALT + Tab` or `ALT + SHIFT + Tab` (`shell.alt_tab = "bind"`). */
    fun hasTabBind() = tabBind(setOf(Mod.ALT)) != null || tabBind(setOf(Mod.ALT, Mod.SHIFT)) != null

    /** Runs the bind on `ALT + Tab`, with [shift] the one on `ALT + SHIFT + Tab` when there is one. */
    fun runTabBind(shift: Boolean) {
        val bind = (if (shift) tabBind(setOf(Mod.ALT, Mod.SHIFT)) else null)
            ?: tabBind(setOf(Mod.ALT)) ?: tabBind(setOf(Mod.ALT, Mod.SHIFT)) ?: return
        runBind(bind)
    }

    private fun runBind(bind: Bind) {
        Log.d(TAG, "key gesture bind $bind")
        if (bind.dispatcher == "lua") lua.callBind(bind.arg) else dispatch(bind.dispatcher, bind.arg)
    }

    /** Runs a Hyprland dispatcher in the shell. */
    fun dispatch(name: String, arg: String = "") {
        shellCall { if (!it.dispatch(name, arg)) Log.w(TAG, "dispatch $name $arg refused") }
    }

    fun shellCall(block: (IHyprShell) -> Unit) {
        val s = shell ?: return
        shellThread.execute {
            try {
                block(s)
            } catch (e: RemoteException) {
                Log.w(TAG, "IHyprShell call failed: $e")
            } catch (e: RuntimeException) {
                Log.w(TAG, "IHyprShell call failed: $e")
            }
        }
    }

    private fun setState(s: ShellState) {
        state = s
        bar.update(s)
        overview.update(s)
        altTab.update(s)
    }

    /** Closes the launcher and the overview (HOME pressed, an app opened). */
    fun closePanels() {
        launcher.hide()
        overview.hide()
    }

    fun dp(v: Float): Int =
        TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, context.resources.displayMetrics)
            .toInt()

    companion object {
        const val TAG = "HyprLauncher"

        @Volatile
        private var instance: Desktop? = null

        fun get(context: Context): Desktop =
            instance ?: synchronized(this) { instance ?: Desktop(context).also { instance = it } }
    }
}
