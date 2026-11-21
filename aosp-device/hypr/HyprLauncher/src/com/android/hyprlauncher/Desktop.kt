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
import com.android.hyprshell.IHyprShell
import com.android.hyprshell.IHyprShellListener
import java.util.concurrent.Executors

/**
 * HyprLauncher's desktop on the default display (Phase D4): the bar, the app launcher and the
 * overview, and the connection to PcSystemUI's IHyprShell. One per process, used on the main
 * thread; calls into the shell go through a background thread so the UI never waits on
 * SystemUI.
 */
class Desktop private constructor(context: Context) {
    val context: Context = context.applicationContext
    val handler = Handler(Looper.getMainLooper())
    val display: Display = this.context.getSystemService(DisplayManager::class.java)
        .getDisplay(Display.DEFAULT_DISPLAY)
    val apps = AppCatalog(this.context)
    var state = ShellState.EMPTY
        private set

    private val shellThread = Executors.newSingleThreadExecutor()
    @Volatile
    private var shell: IHyprShell? = null
    private var started = false

    val bar = Bar(this)
    val launcher = AppLauncher(this)
    val overview = Overview(this)

    private val listener = object : IHyprShellListener.Stub() {
        override fun onStateChanged(displayId: Int, state: Bundle) {
            if (displayId != Display.DEFAULT_DISPLAY) return
            val parsed = ShellState.from(state)
            handler.post { setState(parsed) }
        }

        override fun onEvent(name: String, data: String) {
            Log.d(TAG, "event $name>>$data")
        }

        override fun onDispatch(name: String, arg: String) {
            handler.post {
                when (name) {
                    "launcher" -> launcher.toggle()
                    "overview", "hyprexpo:expo" -> overview.toggle()
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
        registerKeyGestures()
    }

    /**
     * The system key gestures a recents provider handles in AOSP 17 (Quickstep's
     * QuickstepKeyGestureEventsManager): Alt+Tab (RECENT_APPS_SWITCHER: overview on the next
     * window, focus it when Alt is released), META+Tab (RECENT_APPS: the overview) and the
     * all-apps key or META alone (ALL_APPS: the launcher). Each type has one handler, so they
     * are registered one by one; one that someone else holds is skipped.
     */
    private fun registerKeyGestures() {
        val im = context.getSystemService(InputManager::class.java) ?: return
        fun handle(event: KeyGestureEvent) {
            val start = event.action == KeyGestureEvent.ACTION_GESTURE_START
            val complete = event.action == KeyGestureEvent.ACTION_GESTURE_COMPLETE && !event.isCancelled
            handler.post {
                when (event.keyGestureType) {
                    KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS_SWITCHER ->
                        if (start) overview.show(altTab = true) else overview.commitAltTab()
                    KeyGestureEvent.KEY_GESTURE_TYPE_RECENT_APPS -> if (complete) overview.toggle()
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
        shellCall {
            it.addListener(listener)
            it.setReserved(Display.DEFAULT_DISPLAY, bar.reservedTop, 0)
        }
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
