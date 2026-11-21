package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.app.WindowConfiguration.ACTIVITY_TYPE_STANDARD
import android.app.WindowConfiguration.WINDOWING_MODE_FREEFORM
import android.app.WindowConfiguration.WINDOWING_MODE_UNDEFINED
import android.content.ComponentName
import android.content.Context
import android.content.pm.ActivityInfo
import android.graphics.Rect
import android.os.IBinder
import android.os.SystemProperties
import android.util.Log
import android.view.SurfaceControl
import android.view.WindowManager.TRANSIT_CHANGE
import android.view.WindowManager.TRANSIT_CLOSE
import android.view.WindowManager.TRANSIT_OPEN
import android.view.WindowManager.TRANSIT_TO_BACK
import android.view.WindowManager.TRANSIT_TO_FRONT
import android.window.TransitionInfo
import android.window.TransitionRequestInfo
import android.window.WindowContainerTransaction
import com.android.hyprtile.Box
import com.android.hyprtile.HyprTile
import com.android.hyprtile.Insets
import com.android.hyprtile.LayoutKind
import com.android.hyprtile.TileConfig
import com.android.hyprtile.WindowTraits
import com.android.hyprtile.shouldFloat
import com.android.wm.shell.ShellTaskOrganizer
import com.android.wm.shell.common.DisplayController
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.desktopmode.DesktopUserRepositories
import com.android.wm.shell.desktopmode.data.DesktopRepository
import com.android.wm.shell.sysui.ShellInit
import com.android.wm.shell.transition.Transitions
import com.android.wm.shell.transition.Transitions.TransitionFinishCallback
import java.util.Optional

/**
 * Dynamic tiling of desktop tasks with [HyprTile] (Phase D1, docs/hypr-desktop.md).
 *
 * Two paths keep the layout of the active desk in step with its tasks:
 * - As the newest [Transitions.TransitionHandler], it sees a task open, come to front, close or
 *   go to back before anyone else. It lets the other handlers answer the request first
 *   ([Transitions.dispatchRequest] without itself) and adds the new bounds of every tiled task
 *   to their [WindowContainerTransaction], so the new window and its neighbours move inside
 *   the same Shell transition. It never animates and never takes the request from its owner.
 * - As a [Transitions.TransitionObserver], after every transition it compares the desk's
 *   expanded tasks ([DesktopRepository]) with the layout and, when they differ (a task closed
 *   from its caption, minimized, moved by Shell itself), starts one TRANSIT_CHANGE with the new
 *   bounds.
 *
 * A task's bounds are written only when its tile changes, so a window the user drags or
 * resizes stays where it was put until the next layout change. Floating windows (not
 * resizeable, translucent, too large a minimum size) are left alone.
 */
class HyprTileController(
    private val context: Context,
    shellInit: ShellInit,
    private val transitions: Transitions,
    private val taskOrganizer: ShellTaskOrganizer,
    private val displayController: DisplayController,
    private val userRepositories: Optional<DesktopUserRepositories>,
    private val mainExecutor: ShellExecutor,
) : Transitions.TransitionHandler, Transitions.TransitionObserver {

    private val tile = HyprTile(readConfig())
    /** Bounds hyprtile last gave each task; a task is only moved when its tile changes. */
    private val applied = HashMap<Int, Rect>()
    private val syncRunnable = Runnable { sync() }
    private var inRequest = false
    /** windowIsFloating of activity themes, looked up once per component. */
    private val dialogThemes = HashMap<ComponentName, Boolean>()

    init {
        shellInit.addInitCallback(::onInit, this)
    }

    private fun onInit() {
        if (!userRepositories.isPresent) {
            Log.w(TAG, "desktop mode is off, no tiling")
            return
        }
        transitions.addHandler(this)
        transitions.registerObserver(this)
        Log.i(TAG, "hyprtile ready: ${tile.config}")
    }

    private val enabled get() = SystemProperties.getBoolean(PROP_ENABLED, true)

    private val repository: DesktopRepository get() = userRepositories.get().current

    // ---- request path: bounds ride along with the transition that opens or closes a task ----

    override fun handleRequestOnly(
        transition: IBinder,
        request: TransitionRequestInfo,
    ): Transitions.RequestResult? {
        if (!enabled) return null
        val task = request.triggerTask ?: return null
        if (task.activityType != ACTIVITY_TYPE_STANDARD) return null
        val opening = request.type == TRANSIT_OPEN || request.type == TRANSIT_TO_FRONT
        val closing = request.type == TRANSIT_CLOSE || request.type == TRANSIT_TO_BACK
        if (!opening && !closing) return null
        if (request.requestedLocation != null || request.userChange != null) return null
        val deskId = repository.getActiveDeskId(task.displayId)
        Log.d(TAG, "request type=${request.type} task=${task.taskId} " +
            "mode=${task.windowingMode} desk=$deskId")
        if (deskId == null) return null

        // From here on we answer for everyone below us, so they are asked exactly once. Mixed
        // handlers dispatch the request again from inside their own handleRequest, which comes
        // back here; the nested call steps aside.
        if (inRequest) return null
        inRequest = true
        val other = try {
            transitions.dispatchRequest(transition, request, this)
        } finally {
            inRequest = false
        }
        val wct = other?.mWct ?: WindowContainerTransaction()
        updateConfig()
        val changed = if (opening) {
            if (!becomesFreeform(task, wct)) {
                tile.close(task.taskId) != null
            } else if (tile.workspaceOf(task.taskId) == deskId) {
                // Already tiled here: a dialog activity opening on top of it (a permission
                // prompt) does not make the window float.
                false
            } else {
                val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId)
                    .firstOrNull { it != task.taskId && tile.isTiled(it) }
                tile.open(task.taskId, deskId, focused, float = floats(task))
            }
        } else {
            tile.close(task.taskId) != null
        }
        if (changed) {
            Log.d(TAG, "request ${request.type} task ${task.taskId} desk $deskId -> relayout")
            addBounds(deskId, task.displayId, wct, task)
        }
        return other ?: Transitions.RequestResult(wct)
    }

    /**
     * The task is or becomes a freeform (desktop) task with this transition. Moving a task into
     * a desk sets its own mode to UNDEFINED, so that it inherits FREEFORM from the desk root.
     */
    private fun becomesFreeform(task: RunningTaskInfo, wct: WindowContainerTransaction): Boolean {
        return when (val mode = wct.changes[task.token.asBinder()]?.windowingMode ?: -1) {
            -1 -> task.isFreeform
            else -> mode == WINDOWING_MODE_FREEFORM || mode == WINDOWING_MODE_UNDEFINED
        }
    }

    override fun startAnimation(
        transition: IBinder,
        info: TransitionInfo,
        startTransaction: SurfaceControl.Transaction,
        finishTransaction: SurfaceControl.Transaction,
        finishCallback: TransitionFinishCallback,
    ): Boolean = false // other handlers animate; this one only adds bounds

    // ---- observer path: catch up with changes made without a request ----

    override fun onTransitionReady(
        transition: IBinder,
        info: TransitionInfo,
        startTransaction: SurfaceControl.Transaction,
        finishTransaction: SurfaceControl.Transaction,
    ) {}

    override fun onTransitionFinished(transition: IBinder, aborted: Boolean) {
        // The repository learns about task changes from task-info callbacks that can arrive
        // just after the transition; sync once things settle.
        mainExecutor.removeCallbacks(syncRunnable)
        mainExecutor.executeDelayed(syncRunnable, SYNC_DELAY_MS)
    }

    private fun sync() {
        if (!enabled) return
        val configChanged = updateConfig()
        val repo = repository
        val displays = repo.getAllDeskIds()
            .mapNotNull { runCatching { repo.getDisplayForDesk(it) }.getOrNull() }.toSet()
        for (displayId in displays) {
            val deskId = repo.getActiveDeskId(displayId) ?: continue
            val expanded = repo.getExpandedTasksIdsInDeskOrdered(deskId).filter {
                !repo.isClosingTask(it) &&
                    taskOrganizer.getRunningTaskInfo(it)?.windowingMode == WINDOWING_MODE_FREEFORM
            }
            var changed = configChanged
            for (id in tile.tiled(deskId)) if (id !in expanded) { tile.close(id); changed = true }
            // Oldest first, each next to the one opened before it.
            // A floating task is checked again: it may have floated only because a translucent
            // activity (a permission dialog) was on top when it opened. (Once D2 adds
            // togglefloating, tasks the user floated must be kept out of this.)
            for (id in expanded.asReversed()) {
                if (tile.isTiled(id)) continue
                val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
                val float = floats(task)
                if (float && tile.isFloating(id)) continue
                val focused = expanded.firstOrNull { it != id && tile.isTiled(it) }
                changed = tile.open(id, deskId, focused, float) || changed
            }
            if (!changed) continue
            val wct = WindowContainerTransaction()
            addBounds(deskId, displayId, wct)
            if (!wct.isEmpty) {
                Log.d(TAG, "sync desk $deskId: ${tile.tiled(deskId)}")
                transitions.startTransition(TRANSIT_CHANGE, wct, null)
            }
        }
        applied.keys.retainAll { tile.isTiled(it) }
    }

    // ---- shared ----

    /**
     * Adds the bounds of every tiled task of [deskId] whose tile moved. [trigger] is the task of
     * a request: a new task is not known to the task organizer until its transition plays.
     */
    private fun addBounds(
        deskId: Int,
        displayId: Int,
        wct: WindowContainerTransaction,
        trigger: RunningTaskInfo? = null,
    ) {
        val layout = displayController.getDisplayLayout(displayId) ?: return
        val stable = Rect().also { layout.getStableBoundsForDesktopMode(it) }
        val boxes = tile.arrange(deskId, Box(stable.left, stable.top, stable.right, stable.bottom))
        for ((id, box) in boxes) {
            val bounds = Rect(box.left, box.top, box.right, box.bottom)
            if (applied[id] == bounds) continue
            val task = taskOrganizer.getRunningTaskInfo(id)
                ?: trigger?.takeIf { it.taskId == id } ?: continue
            wct.setBounds(task.token, bounds)
            applied[id] = bounds
        }
    }

    private fun floats(task: RunningTaskInfo): Boolean {
        val layout = task.topActivityInfo?.windowLayout
        return shouldFloat(
            WindowTraits(
                packageName = task.baseActivity?.packageName,
                resizeable = task.isResizeable,
                translucent = task.isTopActivityTransparent,
                dialog = task.topActivityInfo?.let { isDialogTheme(it) } ?: false,
                minWidth = layout?.minWidth ?: 0,
                minHeight = layout?.minHeight ?: 0,
            ),
            tile.config,
        )
    }

    private fun isDialogTheme(info: ActivityInfo): Boolean =
        dialogThemes.getOrPut(ComponentName(info.packageName, info.name)) {
            try {
                val res = context.packageManager.getResourcesForApplication(info.applicationInfo)
                val theme = res.newTheme().apply { applyStyle(info.themeResource, true) }
                val a = theme.obtainStyledAttributes(intArrayOf(android.R.attr.windowIsFloating))
                a.getBoolean(0, false).also { a.recycle() }
            } catch (e: Exception) {
                Log.w(TAG, "theme of ${info.name}: $e")
                false
            }
        }

    /** Reads the persist.hypr.* properties (stand-ins for the Lua configuration of D5). */
    private fun updateConfig(): Boolean {
        val fresh = readConfig()
        if (fresh == tile.config) return false
        tile.config = fresh
        applied.clear()
        return true
    }

    private companion object {
        const val TAG = "HyprTile"
        const val PROP_ENABLED = "persist.hypr.tiling"
        const val SYNC_DELAY_MS = 100L

        fun readConfig(): TileConfig {
            val d = TileConfig()
            fun int(name: String, def: Int) = SystemProperties.getInt("persist.hypr.$name", def)
            return d.copy(
                layout = if (SystemProperties.get("persist.hypr.layout") == "master") {
                    LayoutKind.MASTER
                } else {
                    LayoutKind.DWINDLE
                },
                gapsIn = int("gaps_in", d.gapsIn).coerceAtLeast(0),
                gapsOut = int("gaps_out", d.gapsOut).coerceAtLeast(0),
                reserved = Insets(top = int("reserved_top", 0), bottom = int("reserved_bottom", 0)),
                floatPackages = SystemProperties.get("persist.hypr.float_packages")
                    .split(',').map { it.trim() }.filter { it.isNotEmpty() }.toSet(),
            )
        }
    }
}
