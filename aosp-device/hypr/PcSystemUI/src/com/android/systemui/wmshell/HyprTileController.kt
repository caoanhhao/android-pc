package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.app.WindowConfiguration.ACTIVITY_TYPE_STANDARD
import android.app.WindowConfiguration.WINDOWING_MODE_FREEFORM
import android.app.WindowConfiguration.WINDOWING_MODE_UNDEFINED
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Rect
import android.os.IBinder
import android.os.SystemProperties
import android.os.UserHandle
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.Display.INVALID_DISPLAY
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
import com.android.hyprtile.Direction
import com.android.hyprtile.DwindleLayout
import com.android.hyprtile.HyprTile
import com.android.hyprtile.Insets
import com.android.hyprtile.LayoutKind
import com.android.hyprtile.TileConfig
import com.android.hyprtile.WindowTraits
import com.android.hyprtile.neighbor
import com.android.hyprtile.resolveWorkspace
import com.android.hyprtile.shouldFloat
import com.android.wm.shell.ShellTaskOrganizer
import com.android.wm.shell.common.DisplayController
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.desktopmode.DesktopModeEventLogger.Companion.EnterReason
import com.android.wm.shell.desktopmode.DesktopModeKeyGestureHandler
import com.android.wm.shell.desktopmode.DesktopTasksController
import com.android.wm.shell.desktopmode.DesktopUserRepositories
import com.android.wm.shell.desktopmode.data.DesktopRepository
import com.android.wm.shell.desktopmode.multidesks.DeskSwitchTransitionHandler
import com.android.wm.shell.shared.desktopmode.DesktopModeTransitionSource
import com.android.wm.shell.sysui.ShellInit
import com.android.wm.shell.transition.FocusTransitionObserver
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
 *
 * Each desk (Shell's multidesks) is a Hyprland workspace with its own layout tree: workspace N
 * of a display is its N-th desk, created on demand. [dispatch] runs Hyprland's dispatchers
 * (Phase D2); [HyprKeybinds] calls it from key gestures.
 */
class HyprTileController(
    private val context: Context,
    shellInit: ShellInit,
    private val transitions: Transitions,
    private val taskOrganizer: ShellTaskOrganizer,
    private val displayController: DisplayController,
    private val userRepositories: Optional<DesktopUserRepositories>,
    private val desktopTasksController: Optional<DesktopTasksController>,
    private val deskSwitchTransitionHandler: DeskSwitchTransitionHandler,
    private val focusTransitionObserver: FocusTransitionObserver,
    private val shellKeyGestureHandler: Optional<DesktopModeKeyGestureHandler>,
    private val mainExecutor: ShellExecutor,
) : Transitions.TransitionHandler, Transitions.TransitionObserver {

    private val tile = HyprTile(readConfig())
    /** Bounds hyprtile last gave each task; a task is only moved when its tile changes. */
    private val applied = HashMap<Int, Rect>()
    private val syncRunnable = Runnable { sync() }
    private var inRequest = false
    /** windowIsFloating of activity themes, looked up once per component. */
    private val dialogThemes = HashMap<ComponentName, Boolean>()
    /** Tasks floated with `togglefloating`; float rules and sync leave them floating. */
    private val userFloated = HashSet<Int>()
    /** Tiled tasks maximized with `fullscreen 1`; they keep their tile in the layout. */
    private val maximized = HashSet<Int>()

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

    /** Tiling is on (persist.hypr.tiling); with it off, Shell's own behaviour comes back. */
    val enabled get() = userRepositories.isPresent && SystemProperties.getBoolean(PROP_ENABLED, true)

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
                tile.open(task.taskId, deskId, focused,
                    float = task.taskId in userFloated || floats(task))
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
            // A desk's layout can change while it is hidden (movetoworkspace from it); its
            // tiles are written when it shows again.
            if (tile.tiled(deskId).any { it !in applied }) changed = true
            // Oldest first, each next to the one opened before it.
            // A floating task is checked again: it may have floated only because a translucent
            // activity (a permission dialog) was on top when it opened. Tasks the user floated
            // stay floating.
            for (id in expanded.asReversed()) {
                if (tile.isTiled(id) || id in userFloated) continue
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
        maximized.retainAll { tile.isTiled(it) }
        userFloated.retainAll { repo.getDeskIdForTask(it) != null }
    }

    // ---- dispatchers (Hyprland's names; Phase D2) ----

    /**
     * Runs a Hyprland dispatcher: `workspace N`, `movetoworkspace N`, `movefocus l|r|u|d`,
     * `movewindow l|r|u|d`, `togglefloating`, `togglesplit`, `fullscreen [0|1]`, `killactive`,
     * `exec <package>`. Workspace arguments may be relative (`e+1`, `-1`). Must be called on
     * Shell's main thread. Returns false for an unknown dispatcher or a bad argument.
     */
    fun dispatch(name: String, arg: String = ""): Boolean {
        Log.d(TAG, "dispatch $name $arg")
        when (name) {
            "workspace" -> {
                val displayId = focusedDisplayId()
                val n = resolveWorkspace(arg, currentWorkspace(displayId))
                    ?.takeIf { it <= MAX_WORKSPACES } ?: return false
                withWorkspaceDesk(displayId, n) { switchToDesk(displayId, it, moving = null) }
            }
            "movetoworkspace" -> {
                val task = focusedTask() ?: return true
                val n = resolveWorkspace(arg, currentWorkspace(task.displayId))
                    ?.takeIf { it <= MAX_WORKSPACES } ?: return false
                withWorkspaceDesk(task.displayId, n) { switchToDesk(task.displayId, it, moving = task) }
            }
            "movefocus" -> moveFocus(Direction.parse(arg) ?: return false)
            "movewindow" -> moveWindow(Direction.parse(arg) ?: return false)
            "togglefloating" -> toggleFloating()
            "togglesplit" -> toggleSplit()
            "fullscreen" -> if (arg.trim() == "1") toggleMaximize() else toggleFullscreen()
            "killactive" -> shellKeyGestureHandler.ifPresent { it.quitFocusedDesktopTask() }
            "exec" -> exec(arg.trim())
            else -> return false
        }
        return true
    }

    private fun focusedDisplayId(): Int =
        focusTransitionObserver.globallyFocusedDisplayId.takeIf { it != INVALID_DISPLAY }
            ?: DEFAULT_DISPLAY

    /** The focused task when it is a desktop task of the active desk. */
    private fun focusedTask(): RunningTaskInfo? = taskOrganizer.getRunningTasks().find {
        repository.isActiveTask(it.taskId) && focusTransitionObserver.hasGlobalFocus(it)
    }

    /** The desks of [displayId] in Shell's order; workspace N is the N-th. */
    private fun orderedDesks(displayId: Int): List<Int> {
        val repo = repository
        return repo.getDeskIds(displayId).sortedBy { repo.getDeskPosition(it) ?: Int.MAX_VALUE }
    }

    private fun currentWorkspace(displayId: Int): Int {
        val active = repository.getActiveDeskId(displayId) ?: return 1
        return orderedDesks(displayId).indexOf(active) + 1
    }

    /**
     * Runs [block] with the desk of workspace [n], creating the missing desks up to it first
     * (desks are ordered, so workspace 5 needs desks 1-4; they stay empty until used).
     */
    private fun withWorkspaceDesk(displayId: Int, n: Int, block: (Int) -> Unit) {
        val desks = orderedDesks(displayId)
        if (n <= desks.size) {
            block(desks[n - 1])
            return
        }
        val controller = desktopTasksController.orElse(null) ?: return
        Log.d(TAG, "workspace $n: creating desk ${desks.size + 1} on display $displayId")
        controller.createDesk(
            displayId = displayId,
            userId = repository.userId,
            enforceDeskLimit = false,
            activateDesk = false,
            enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER,
        ) { _ ->
            mainExecutor.execute {
                if (orderedDesks(displayId).size > desks.size) {
                    withWorkspaceDesk(displayId, n, block)
                } else {
                    Log.w(TAG, "desk creation on display $displayId did not add a desk")
                }
            }
        }
    }

    /**
     * Activates [to] with Shell's desk-switch transition (the slide between desks), carrying
     * [moving] along when set (`movetoworkspace`). Both desks' new tiles ride in the same
     * transaction, so the target desk shows already tiled.
     */
    private fun switchToDesk(displayId: Int, to: Int, moving: RunningTaskInfo?) {
        val controller = desktopTasksController.orElse(null) ?: return
        val repo = repository
        val from = repo.getActiveDeskId(displayId)
        if (from == to) return
        if (from == null) {
            // No desk is active (home or a fullscreen app): Shell's own activation.
            controller.activateDesk(to, repo.userId, enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER)
            return
        }
        updateConfig()
        val wct = WindowContainerTransaction()
        val runOnStart = controller.addDeskActivationChanges(
            deskId = to,
            wct = wct,
            newTask = moving,
            userId = repo.userId,
            enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER,
        )
        if (moving != null) {
            val id = moving.taskId
            controller.addMoveToDeskTaskChanges(wct, moving, to)
            val floating = tile.isFloating(id) || id in userFloated
            maximized -= id
            applied.remove(id)
            tile.close(id)?.let { addBounds(it, displayId, wct) }
            val focused = repo.getExpandedTasksIdsInDeskOrdered(to).firstOrNull { tile.isTiled(it) }
            tile.open(id, to, focused, float = floating)
        }
        addBounds(to, displayId, wct, moving)
        Log.d(TAG, "switch desk $from -> $to on display $displayId" +
            (moving?.let { ", moving task ${it.taskId}" } ?: ""))
        val transition = deskSwitchTransitionHandler.startTransition(wct, repo.userId, displayId, from, to)
        runOnStart?.invoke(transition)
    }

    /** Brings the window next to the focused one in [dir] to front (and so focuses it). */
    private fun moveFocus(dir: Direction) {
        val task = focusedTask() ?: return
        val deskId = repository.getDeskIdForTask(task.taskId) ?: return
        val boxes = repository.getExpandedTasksIdsInDeskOrdered(deskId).mapNotNull { id ->
            taskOrganizer.getRunningTaskInfo(id)?.let { id to it.bounds() }
        }.toMap()
        val target = neighbor(boxes, task.taskId, dir, slack = slack()) ?: return
        taskOrganizer.getRunningTaskInfo(target)?.let { desktopTasksController.get().moveTaskToFront(it) }
    }

    /**
     * Swaps the focused tiled window with its neighbour in [dir]; a floating window moves to
     * that edge of the work area instead.
     */
    private fun moveWindow(dir: Direction) {
        val task = focusedTask() ?: return
        val deskId = repository.getDeskIdForTask(task.taskId) ?: return
        val area = workArea(task.displayId) ?: return
        val wct = WindowContainerTransaction()
        if (tile.isTiled(task.taskId)) {
            val boxes = tile.arrange(deskId, area)
            val target = neighbor(boxes, task.taskId, dir, slack = slack()) ?: return
            tile.swap(task.taskId, target)
            maximized -= task.taskId
            addBounds(deskId, task.displayId, wct)
        } else {
            val b = task.bounds()
            val gap = tile.config.gapsOut
            val inner = area.inset(tile.config.reserved)
            val (dx, dy) = when (dir) {
                Direction.LEFT -> inner.left + gap - b.left to 0
                Direction.RIGHT -> inner.right - gap - b.right to 0
                Direction.UP -> 0 to inner.top + gap - b.top
                Direction.DOWN -> 0 to inner.bottom - gap - b.bottom
            }
            wct.setBounds(task.token, Rect(b.left + dx, b.top + dy, b.right + dx, b.bottom + dy))
        }
        if (!wct.isEmpty) transitions.startTransition(TRANSIT_CHANGE, wct, null)
    }

    private fun toggleFloating() {
        val task = focusedTask() ?: return
        val id = task.taskId
        val deskId = repository.getDeskIdForTask(id) ?: return
        val area = workArea(task.displayId) ?: return
        val wct = WindowContainerTransaction()
        if (tile.isTiled(id)) {
            tile.toggleFloating(id, deskId)
            userFloated += id
            maximized -= id
            applied.remove(id)
            // Hyprland centres a window that starts floating; half the width, two thirds of
            // the height of the work area.
            val w = area.width / 2
            val h = area.height * 2 / 3
            val left = area.left + (area.width - w) / 2
            val top = area.top + (area.height - h) / 2
            wct.setBounds(task.token, Rect(left, top, left + w, top + h))
        } else {
            if (!task.isResizeable) return
            userFloated -= id
            val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId)
                .firstOrNull { it != id && tile.isTiled(it) }
            tile.toggleFloating(id, deskId, focused)
        }
        addBounds(deskId, task.displayId, wct, task)
        if (!wct.isEmpty) transitions.startTransition(TRANSIT_CHANGE, wct, null)
    }

    private fun toggleSplit() {
        val task = focusedTask() ?: return
        val deskId = tile.workspaceOf(task.taskId) ?: return
        val layout = tile.layout(deskId) as? DwindleLayout ?: return
        layout.toggleSplit(task.taskId)
        val wct = WindowContainerTransaction()
        addBounds(deskId, task.displayId, wct)
        if (!wct.isEmpty) transitions.startTransition(TRANSIT_CHANGE, wct, null)
    }

    /**
     * Hyprland's `fullscreen 1`: the focused tiled window takes the whole work area (no gaps)
     * and keeps its place in the layout; again, and it goes back to its tile.
     */
    private fun toggleMaximize() {
        val task = focusedTask() ?: return
        val id = task.taskId
        val deskId = tile.workspaceOf(id) ?: return
        val stable = Rect().also {
            displayController.getDisplayLayout(task.displayId)?.getStableBoundsForDesktopMode(it)
                ?: return
        }
        val wct = WindowContainerTransaction()
        if (maximized.remove(id)) {
            applied.remove(id)
            addBounds(deskId, task.displayId, wct)
        } else {
            maximized += id
            wct.setBounds(task.token, stable)
            wct.reorder(task.token, true)
        }
        transitions.startTransition(TRANSIT_CHANGE, wct, null)
    }

    /** Hyprland's `fullscreen 0`: Shell's real fullscreen, out of the desk and back. */
    private fun toggleFullscreen() {
        desktopTasksController.orElse(null)?.toggleFocusedTaskFullscreenState(
            displayId = focusedDisplayId(),
            userId = repository.userId,
            transitionSource = DesktopModeTransitionSource.KEYBOARD_SHORTCUT,
        )
    }

    private fun exec(packageName: String) {
        val intent = context.packageManager.getLaunchIntentForPackage(packageName)
        if (intent == null) {
            Log.w(TAG, "exec: no launcher activity in $packageName")
            return
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivityAsUser(intent, UserHandle.of(repository.userId))
    }

    /**
     * Shell asked to snap [task] to one half (its caption's snap menu, a drag to the screen
     * edge, the snap key gestures); with tiling on this replaces Shell's two-window tiling:
     * a tiled window swaps with its neighbour on that side, a floating one joins the layout.
     */
    fun onSnap(task: RunningTaskInfo, left: Boolean) {
        val id = task.taskId
        val deskId = repository.getDeskIdForTask(id) ?: return
        val area = workArea(task.displayId) ?: return
        val dir = if (left) Direction.LEFT else Direction.RIGHT
        if (tile.isTiled(id)) {
            neighbor(tile.arrange(deskId, area), id, dir, slack = slack())?.let { tile.swap(id, it) }
        } else if (task.isResizeable) {
            userFloated -= id
            val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId)
                .firstOrNull { it != id && tile.isTiled(it) }
            tile.open(id, deskId, focused)
        }
        // The drag moved the window: write its tile again even if the tile did not change.
        maximized -= id
        applied.remove(id)
        val wct = WindowContainerTransaction()
        addBounds(deskId, task.displayId, wct, task)
        if (!wct.isEmpty) transitions.startTransition(TRANSIT_CHANGE, wct, null)
    }

    private fun workArea(displayId: Int): Box? {
        val layout = displayController.getDisplayLayout(displayId) ?: return null
        val stable = Rect().also { layout.getStableBoundsForDesktopMode(it) }
        return Box(stable.left, stable.top, stable.right, stable.bottom)
    }

    /** How far apart two neighbours may be: the gaps between them and a little more. */
    private fun slack() = 2 * tile.config.gapsIn + 4

    private fun RunningTaskInfo.bounds(): Box =
        configuration.windowConfiguration.bounds.let { Box(it.left, it.top, it.right, it.bottom) }

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
            if (id in maximized) continue
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
        const val MAX_WORKSPACES = 9

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
