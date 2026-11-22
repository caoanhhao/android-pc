package com.android.systemui.wmshell

import android.content.Context
import android.os.Bundle
import android.os.RemoteCallbackList
import android.os.RemoteException
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import com.android.hyprshell.IHyprReply
import com.android.hyprshell.IHyprShell
import com.android.hyprshell.IHyprShellListener
import com.android.wm.shell.common.ExternalInterfaceBinder
import com.android.wm.shell.common.RemoteCallable
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.sysui.ShellController
import com.android.wm.shell.sysui.ShellInit
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * IHyprShell for HyprLauncher (Phase D4, docs/hypr-desktop.md).
 *
 * Registered with [ShellController.addExternalInterface] under its descriptor, so SystemUI's
 * LauncherProxyService puts it into the Bundle it gives the recents provider in
 * `ILauncherProxy.onInitialize()`, next to IDesktopMode and IRecentTasks: the launcher gets it
 * the way Quickstep gets Shell's interfaces, and no permission of our own is needed (callers
 * need MANAGE_ACTIVITY_TASKS, as for Shell's other interfaces).
 *
 * State goes out to listeners after every layout sync and title change, debounced, together
 * with Hyprland-style events derived from the difference to the previous state. The same push
 * feeds Hyprland's IPC sockets ([HyprIpc], Phase E12), which ask the launcher for what is
 * its own (Lua, notifications, layers) through [IHyprShellListener.onRequest].
 */
class HyprShell(
    private val context: Context,
    shellInit: ShellInit,
    private val shellController: ShellController,
    private val controller: HyprTileController,
    private val mainExecutor: ShellExecutor,
) : RemoteCallable<HyprShell> {

    private val listeners = RemoteCallbackList<IHyprShellListener>()
    private val pushRunnable = Runnable { push() }
    private var last: Bundle? = null
    private val ipc = HyprIpc(context, controller, mainExecutor, ::askLauncher)

    init {
        shellInit.addInitCallback(::onInit, this)
    }

    private fun onInit() {
        shellController.addExternalInterface(IHyprShell.DESCRIPTOR, { Binder(this) }, this)
        ipc.start()
        controller.onStateChanged = {
            mainExecutor.removeCallbacks(pushRunnable)
            mainExecutor.executeDelayed(pushRunnable, PUSH_DELAY_MS)
        }
        controller.launcherDispatch = { name, arg ->
            val n = listeners.beginBroadcast()
            for (i in 0 until n) {
                try {
                    listeners.getBroadcastItem(i).onDispatch(name, arg)
                } catch (e: RemoteException) {
                    Log.w(TAG, "listener gone: $e")
                }
            }
            listeners.finishBroadcast()
            n > 0
        }
    }

    override fun getContext() = context

    override fun getRemoteCallExecutor() = mainExecutor

    private fun push() {
        val state = controller.state(DEFAULT_DISPLAY)
        val events = events(last, state)
        last = state
        ipc.onState(state)
        val n = listeners.beginBroadcast()
        for (i in 0 until n) {
            val l = listeners.getBroadcastItem(i)
            try {
                l.onStateChanged(DEFAULT_DISPLAY, state)
                for ((name, data) in events) l.onEvent(name, data)
            } catch (e: RemoteException) {
                Log.w(TAG, "listener gone: $e")
            }
        }
        listeners.finishBroadcast()
    }

    /**
     * A hyprctl request for the launcher (any thread but the main one): the first listener
     * that answers within [REQUEST_TIMEOUT_MS]; null without one.
     */
    private fun askLauncher(command: String, args: String, json: Boolean): String? {
        val latch = CountDownLatch(1)
        var answer: String? = null
        val reply = object : IHyprReply.Stub() {
            override fun reply(text: String?) {
                answer = text ?: ""
                latch.countDown()
            }
        }
        mainExecutor.execute {
            val n = listeners.beginBroadcast()
            var asked = false
            for (i in 0 until n) {
                try {
                    listeners.getBroadcastItem(i).onRequest(command, args, json, reply)
                    asked = true
                    break
                } catch (e: RemoteException) {
                    Log.w(TAG, "listener gone: $e")
                }
            }
            listeners.finishBroadcast()
            if (!asked) latch.countDown()
        }
        latch.await(REQUEST_TIMEOUT_MS, TimeUnit.MILLISECONDS)
        return answer
    }

    private fun addListener(l: IHyprShellListener) {
        listeners.register(l)
        // A new listener gets the current state at once.
        try {
            l.onStateChanged(DEFAULT_DISPLAY, controller.state(DEFAULT_DISPLAY))
        } catch (e: RemoteException) {
            Log.w(TAG, "new listener gone: $e")
        }
    }

    private class Binder(private var shell: HyprShell?) : IHyprShell.Stub(), ExternalInterfaceBinder {
        override fun invalidate() {
            shell = null
        }

        override fun setConfig(config: Bundle?) {
            executeRemoteCallWithTaskPermission(shell, "setConfig") {
                it.controller.configOverrides = config ?: Bundle()
                it.ipc.onConfig()
            }
        }

        override fun dispatch(name: String?, arg: String?): Boolean {
            var result = false
            executeRemoteCallWithTaskPermission(shell, "dispatch", {
                result = it.controller.dispatch(name ?: "", arg ?: "")
            }, true /* blocking */)
            return result
        }

        override fun setReserved(displayId: Int, top: Int, bottom: Int) {
            executeRemoteCallWithTaskPermission(shell, "setReserved") {
                it.controller.setBarReserved(displayId, top, bottom)
            }
        }

        override fun getState(displayId: Int): Bundle {
            var result = Bundle()
            executeRemoteCallWithTaskPermission(shell, "getState", {
                result = it.controller.state(displayId)
            }, true /* blocking */)
            return result
        }

        override fun addListener(listener: IHyprShellListener?) {
            listener ?: return
            executeRemoteCallWithTaskPermission(shell, "addListener") { it.addListener(listener) }
        }

        override fun removeListener(listener: IHyprShellListener?) {
            listener ?: return
            executeRemoteCallWithTaskPermission(shell, "removeListener") {
                it.listeners.unregister(listener)
            }
        }
    }

    companion object {
        private const val TAG = "HyprShell"
        private const val PUSH_DELAY_MS = 50L
        private const val REQUEST_TIMEOUT_MS = 5000L

        /** Hyprland's event names for what changed between [old] and [new]. */
        fun events(old: Bundle?, new: Bundle): List<Pair<String, String>> {
            val out = ArrayList<Pair<String, String>>()
            if (old == null) return out
            val active = new.getInt("active")
            if (active != old.getInt("active")) out += "workspace" to "$active"
            val oldIds = old.getIntArray("taskIds")?.toSet() ?: emptySet()
            val ids = new.getIntArray("taskIds") ?: IntArray(0)
            val pkgs = new.getStringArray("taskPackages") ?: emptyArray()
            for ((i, id) in ids.withIndex()) {
                if (id !in oldIds) out += "openwindow" to "$id,${pkgs.getOrElse(i) { "" }}"
            }
            for (id in oldIds) if (id !in ids) out += "closewindow" to "$id"
            val focus = "${new.getString("focusedPackage") ?: ""},${new.getString("focusedTitle") ?: ""}"
            val oldFocus = "${old.getString("focusedPackage") ?: ""},${old.getString("focusedTitle") ?: ""}"
            if (focus != oldFocus) out += "activewindow" to focus
            return out
        }
    }
}
