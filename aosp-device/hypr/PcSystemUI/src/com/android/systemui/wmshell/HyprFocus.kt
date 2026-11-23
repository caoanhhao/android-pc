package com.android.systemui.wmshell

import android.app.ActivityManager
import android.app.ActivityManager.RunningTaskInfo
import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.os.RemoteException
import android.os.UserHandle
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Log
import android.view.WindowManager.TRANSIT_TO_BACK
import android.window.WindowContainerTransaction
import com.android.hyprtile.CycleOptions
import com.android.hyprtile.FocusHistory
import com.android.hyprtile.WindowFacts
import com.android.hyprtile.WindowSelector
import com.android.hyprtile.WorkspaceHistory
import com.android.hyprtile.cycle
import com.android.hyprtile.previousWorkspaceArg
import com.android.wm.shell.ShellTaskOrganizer
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.shared.FocusTransitionListener
import com.android.wm.shell.transition.FocusTransitionObserver
import com.android.wm.shell.transition.Transitions

/**
 * Window history and Hyprland's ways of switching windows (Phase E3, docs/hypr-desktop.md,
 * "Window history (E3)").
 *
 * - A focus history per display ([history]): every standard task that gets the focus on its
 *   display goes to the front, across workspaces; closed tasks drop out, a task moved to
 *   another display takes its place along.
 * - A workspace history (Hyprland 0.57's most-recently-used list) for `workspace previous`,
 *   `previous_per_monitor` and `binds.workspace_back_and_forth` ([resolveWorkspace]).
 * - Urgent windows: Hyprland's urgency is an app asking for attention while unfocused; here a
 *   window whose app posts an alerting notification while the window is not focused. It ends
 *   when the window gets the focus or the notification goes.
 * - Dispatchers ([dispatch]): `focuscurrentorlast`, `focusurgentorlast`, `cyclenext`
 *   (`cycleprev`), `swapnext`, `alterzorder`, `focuswindow <selector>`, `swapwindow <selector>`.
 *
 * E4 (the ALT + Tab switcher) builds on [history], [urgent] and [focus]; ALT + Tab itself reaches
 * HyprLauncher (the recents provider's key gesture), which runs the ALT + Tab bind.
 */
class HyprFocus(
    private val context: Context,
    private val controller: HyprTileController,
    private val taskOrganizer: ShellTaskOrganizer,
    private val transitions: Transitions,
    private val focusObserver: FocusTransitionObserver,
    private val mainExecutor: ShellExecutor,
) {
    private val windows = FocusHistory()
    private val workspaces = WorkspaceHistory()
    /** Urgent task ids and the notification keys that made them urgent. */
    private val urgentKeys = LinkedHashMap<Int, MutableSet<String>>()
    /** `swapnext`: the window each window last swapped with, so repeated presses walk on. */
    private val lastCycled = HashMap<Int, Int>()

    fun start() {
        focusObserver.setLocalFocusTransitionListener(object : FocusTransitionListener {
            override fun onFocusedTaskChanged(
                taskInfo: RunningTaskInfo,
                isFocusedOnDisplay: Boolean,
                isFocusedGlobally: Boolean,
            ) {
                if (isFocusedGlobally) controller.onFocusChanged()
                if (isFocusedOnDisplay) onFocused(taskInfo)
            }
        }, mainExecutor)
        try {
            notifications.registerAsSystemService(context,
                ComponentName(context.packageName, HyprFocus::class.java.name + ".Urgency"),
                UserHandle.USER_ALL)
        } catch (e: RemoteException) {
            Log.w(TAG, "no notification listener, no urgent windows: $e")
        }
    }

    private fun onFocused(task: RunningTaskInfo) {
        if (!controller.isAppWindow(task)) return
        windows.focused(task.displayId, task.taskId)
        if (urgentKeys.remove(task.taskId) != null) controller.onStateChanged?.invoke()
    }

    /**
     * After every settled transition (the controller's sync): drops closed tasks, moves tasks
     * that changed display, notes new tasks and the active workspace of every display.
     */
    fun onSync() {
        val tasks = taskOrganizer.getRunningTasks().filter { controller.isAppWindow(it) }
        val byId = tasks.associateBy { it.taskId }
        windows.retain { it in byId }
        for (t in tasks) {
            windows.moveToDisplay(t.taskId, t.displayId)
            windows.opened(t.displayId, t.taskId)
        }
        urgentKeys.keys.retainAll(byId.keys)
        lastCycled.keys.retainAll(byId.keys)
        for (display in controller.displaysWithDesks()) {
            val ws = controller.activeWorkspace(display)
            if (ws > 0) workspaces.track(display, ws, controller.allowWorkspaceCycles())
        }
    }

    /** The focus history of [displayId], most recent first (task ids). */
    fun history(displayId: Int): List<Int> = windows.of(displayId).filter {
        taskOrganizer.getRunningTaskInfo(it) != null
    }

    /** Urgent tasks, oldest first. */
    val urgent: Set<Int> get() = urgentKeys.keys.toSet()

    /** Gives [taskId] the focus: its workspace becomes active, it comes to front. */
    fun focus(taskId: Int) = controller.focusTask(taskId)

    // ---- workspaces ----

    /**
     * Rewrites the workspace argument of `workspace`, `movetoworkspace` and
     * `movetoworkspacesilent`: `previous` and `previous_per_monitor` become the workspace's
     * number, and with `binds.workspace_back_and_forth` `workspace N` of the active one becomes
     * the previous one (2: on this display). Null when there is no previous workspace.
     */
    fun resolveWorkspace(name: String, arg: String, displayId: Int): String? {
        val current = WorkspaceHistory.Entry(displayId, controller.activeWorkspace(displayId))
        val perMonitor = previousWorkspaceArg(arg)
            ?: if (name == "workspace" && arg.trim().toIntOrNull() == current.workspace) {
                when (controller.workspaceBackAndForth()) {
                    1 -> false
                    2 -> true
                    else -> return arg
                }
            } else {
                return arg
            }
        val prev = if (perMonitor) workspaces.previousOnDisplay(current) else workspaces.previous(current)
        return prev?.workspace?.toString()
    }

    /** The workspace `workspace previous` goes to from [displayId]'s active one, 0 for none. */
    fun previousWorkspace(displayId: Int): Int =
        resolveWorkspace("workspace", "previous", displayId)?.toIntOrNull() ?: 0

    // ---- dispatchers ----

    /** Runs one of this class's dispatchers; false when [name] is not one or [arg] is bad. */
    fun dispatch(name: String, arg: String): Boolean {
        when (name) {
            "focuscurrentorlast" -> focusCurrentOrLast(urgentFirst = false)
            "focusurgentorlast" -> focusCurrentOrLast(urgentFirst = true)
            "cyclenext", "cycleprev" -> {
                val o = CycleOptions.parse(arg) ?: return false
                cycleNext(if (name == "cycleprev") o.copy(next = !o.next) else o)
            }
            "swapnext" -> swapNext(next = when (arg.trim().lowercase()) {
                "", "next" -> true
                "prev", "p", "b" -> false
                else -> return false
            })
            "alterzorder" -> {
                val mode = arg.substringBefore(',').trim().lowercase()
                if (mode != "top" && mode != "bottom") return false
                val target = selectOrFocused(arg.substringAfter(',', "")) ?: return true
                alterZOrder(target, top = mode == "top")
            }
            "focuswindow" -> {
                val sel = runCatching { WindowSelector.parse(arg) }.getOrElse { return false }
                select(sel)?.let { focus(it.taskId) }
            }
            "swapwindow" -> {
                val sel = runCatching { WindowSelector.parse(arg) }.getOrElse { return false }
                val task = controller.focusedWindow() ?: return true
                select(sel)?.takeIf { it.taskId != task.taskId }?.let { controller.swapTiled(task, it.taskId) }
            }
            else -> return false
        }
        return true
    }

    private fun usable(id: Int) = taskOrganizer.getRunningTaskInfo(id) != null

    /**
     * `focuscurrentorlast`: the window focused before the current one, or the most recent one
     * when none is focused; `focusurgentorlast` first takes the most recently urgent window.
     */
    private fun focusCurrentOrLast(urgentFirst: Boolean) {
        val display = controller.focusedDisplay()
        if (urgentFirst) {
            urgentKeys.keys.lastOrNull { usable(it) }?.let { focus(it); return }
        }
        val focused = focusObserver.getFocusedTaskIdOnDisplay(display).takeIf { it >= 0 }
        val target = windows.currentOrLast(display, focused, ::usable)
        Log.d(TAG, "focus last on display $display: $focused -> $target (${windows.of(display)})")
        target?.let { focus(it) }
    }

    /**
     * `cyclenext`: the next window of the focused one's workspace in the order windows opened
     * (Hyprland's window list), or in focus history order with `hist`; `tiled`/`floating`
     * limit it, `visible` takes the active workspace of every display. Minimized windows are
     * skipped. With no window focused, the first one of the active workspace.
     */
    private fun cycleNext(o: CycleOptions) {
        val display = controller.focusedDisplay()
        val focused = controller.focusedWindow()
        val candidates = if (o.visible) {
            controller.displaysWithDesks().flatMap { controller.activeWorkspaceTasks(it) }
        } else {
            controller.activeWorkspaceTasks(focused?.displayId ?: display)
        }.filter { !controller.isMinimized(it) }
        val order = if (o.hist) {
            // Hyprland's history runs oldest to newest.
            val h = (focused?.displayId?.let { windows.of(it) } ?: windows.of(display)).asReversed()
            h.filter { it in candidates } + candidates.filter { it !in h }.sorted()
        } else {
            candidates.sorted()
        }
        val wantFloating = o.floatingOnly
        val target = cycle(order, focused?.taskId, o.next) {
            wantFloating == null || controller.isTiled(it) != wantFloating
        }
        Log.d(TAG, "cyclenext $o: ${focused?.taskId} -> $target in $order")
        target?.let { focus(it) }
    }

    /**
     * `swapnext`: the focused tiled window swaps places with the next tiled window of its
     * workspace (in opening order); pressed again, it moves on from the window it swapped
     * with, so it walks through the layout. It keeps the focus.
     */
    private fun swapNext(next: Boolean) {
        val task = controller.focusedWindow() ?: return
        val id = task.taskId
        if (!controller.isTiled(id)) return
        val order = controller.activeWorkspaceTasks(task.displayId).filter { controller.isTiled(it) }.sorted()
        val from = lastCycled[id]?.takeIf { it in order } ?: id
        var other = cycle(order, from, next) { it != id }
        if (other == null || other == id) other = cycle(order, id, next) { true }
        other ?: return
        lastCycled[id] = other
        controller.swapTiled(task, other)
    }

    /**
     * `alterzorder top|bottom`: the window goes above or below the other windows of its
     * workspace (WindowManager's order of the desk's tasks). Going to the top focuses it, as
     * any raised Android window; going to the bottom passes the focus to the new top one.
     */
    private fun alterZOrder(task: RunningTaskInfo, top: Boolean) {
        if (top) {
            focus(task.taskId)
            return
        }
        val wct = WindowContainerTransaction().reorder(task.token, false)
        transitions.startTransition(TRANSIT_TO_BACK, wct, null)
    }

    /** The window a selector picks among all desktop windows, active workspace first. */
    private fun select(sel: WindowSelector): RunningTaskInfo? {
        val focused = controller.focusedWindow()?.taskId
        val display = controller.focusedDisplay()
        val active = controller.activeWorkspaceTasks(display).toSet()
        val pids = if (sel is WindowSelector.Pid) pidsByPackage() else emptyMap()
        val facts = controller.desktopTasks().map { t ->
            val pkg = (t.baseActivity ?: t.topActivity)?.packageName ?: ""
            WindowFacts(
                id = t.taskId,
                cls = pkg,
                title = t.taskDescription?.label?.toString() ?: "",
                pid = pids[pkg] ?: -1,
                floating = !controller.isTiled(t.taskId),
                focused = t.taskId == focused,
                onActiveWorkspace = t.taskId in active,
            )
        }.sortedWith(compareBy({ !it.onActiveWorkspace }, { it.id }))
        return sel.pick(facts)?.let { taskOrganizer.getRunningTaskInfo(it.id) }
    }

    private fun selectOrFocused(text: String): RunningTaskInfo? =
        if (text.isBlank()) {
            controller.focusedWindow()
        } else {
            runCatching { WindowSelector.parse(text) }.getOrNull()?.let { select(it) }
        }

    private fun pidsByPackage(): Map<String, Int> {
        val am = context.getSystemService(ActivityManager::class.java) ?: return emptyMap()
        val out = HashMap<String, Int>()
        for (p in am.runningAppProcesses ?: emptyList()) {
            for (pkg in p.pkgList ?: emptyArray()) out.putIfAbsent(pkg, p.pid)
        }
        return out
    }

    // ---- urgency ----

    private val notifications = object : NotificationListenerService() {
        override fun onNotificationPosted(sbn: StatusBarNotification, rankingMap: RankingMap) {
            val ranking = Ranking()
            val alerting = rankingMap.getRanking(sbn.key, ranking) &&
                ranking.importance >= NotificationManager.IMPORTANCE_DEFAULT
            if (!alerting || sbn.isOngoing || sbn.notification.isGroupSummary) return
            // Not the Service's own (Context) main executor: Shell's main thread.
            this@HyprFocus.mainExecutor.execute { markUrgent(sbn.packageName, sbn.key) }
        }

        override fun onNotificationRemoved(sbn: StatusBarNotification, rankingMap: RankingMap) {
            this@HyprFocus.mainExecutor.execute { clearKey(sbn.key) }
        }
    }

    /** The most recently focused unfocused window of [pkg] becomes urgent. */
    private fun markUrgent(pkg: String, key: String) {
        val tasks = controller.desktopTasks().filter {
            (it.baseActivity ?: it.topActivity)?.packageName == pkg
        }
        if (tasks.isEmpty()) return
        val recent = windows.displays().flatMap { windows.of(it) }
        val task = tasks.minByOrNull { recent.indexOf(it.taskId).let { i -> if (i < 0) Int.MAX_VALUE else i } }
            ?: return
        if (controller.focusedWindow()?.taskId == task.taskId) return
        // The newest urgent window goes last.
        val keys = urgentKeys.remove(task.taskId) ?: HashSet()
        keys += key
        urgentKeys[task.taskId] = keys
        Log.d(TAG, "task ${task.taskId} ($pkg) urgent")
        controller.onStateChanged?.invoke()
    }

    private fun clearKey(key: String) {
        val it = urgentKeys.entries.iterator()
        var changed = false
        while (it.hasNext()) {
            val e = it.next()
            if (e.value.remove(key) && e.value.isEmpty()) {
                it.remove()
                changed = true
            }
        }
        if (changed) controller.onStateChanged?.invoke()
    }

    private companion object {
        const val TAG = "HyprFocus"
    }
}
