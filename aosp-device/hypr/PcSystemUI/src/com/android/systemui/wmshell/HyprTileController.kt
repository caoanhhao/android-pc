package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.app.StatusBarManager
import android.app.WindowConfiguration.ACTIVITY_TYPE_STANDARD
import android.app.WindowConfiguration.WINDOWING_MODE_FREEFORM
import android.app.WindowConfiguration.WINDOWING_MODE_UNDEFINED
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Rect
import android.hardware.input.InputManager
import android.os.Bundle
import android.os.IBinder
import android.os.SystemProperties
import android.os.UserHandle
import android.util.Log
import android.view.Display.DEFAULT_DISPLAY
import android.view.Display.INVALID_DISPLAY
import android.view.MotionEvent
import android.view.SurfaceControl
import android.view.WindowManager.TRANSIT_CHANGE
import android.view.WindowManager.TRANSIT_CLOSE
import android.view.WindowManager.TRANSIT_OPEN
import android.view.WindowManager.TRANSIT_TO_BACK
import android.view.WindowManager.TRANSIT_TO_FRONT
import android.window.TransitionInfo
import android.window.TransitionInfo.FLAG_IS_WALLPAPER
import android.window.TransitionRequestInfo
import android.window.WindowContainerTransaction
import com.android.hyprtile.Box
import com.android.hyprtile.Direction
import com.android.hyprtile.DwindleLayout
import com.android.hyprtile.HyprTile
import com.android.hyprtile.Insets
import com.android.hyprtile.Coord
import com.android.hyprtile.LayoutKind
import com.android.hyprtile.Switch
import com.android.hyprtile.TileConfig
import com.android.hyprtile.WindowRule
import com.android.hyprtile.WindowTraits
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.effect
import com.android.hyprtile.neighbor
import com.android.hyprtile.parsePin
import com.android.hyprtile.parseWindow
import com.android.hyprtile.parseXY
import com.android.hyprtile.DEFAULT_WORKSPACES
import com.android.hyprtile.MAX_WORKSPACES
import com.android.hyprtile.WorkspaceMap
import com.android.hyprtile.resolveWorkspace
import com.android.hyprtile.shouldFloat
import com.android.hyprtile.DEFAULT_DIM_SPECIAL
import com.android.hyprtile.SpecialWorkspaces
import com.android.hyprtile.specialName
import com.android.systemui.hypr.HyprHostActions
import com.android.systemui.hypr.HyprSystemActions
import com.android.wm.shell.ShellTaskOrganizer
import com.android.wm.shell.common.DisplayController
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.desktopmode.DesktopModeEventLogger.Companion.EnterReason
import com.android.wm.shell.desktopmode.DesktopModeEventLogger.Companion.ExitReason
import com.android.wm.shell.desktopmode.DesktopModeEventLogger.Companion.UnminimizeReason
import com.android.wm.shell.desktopmode.DesktopMixedTransitionHandler
import com.android.wm.shell.desktopmode.DesktopModeKeyGestureHandler
import com.android.wm.shell.desktopmode.DesktopModeTransitionTypes.TRANSIT_DESKTOP_MODE_DESK_TO_DESK_SWITCH
import com.android.wm.shell.desktopmode.DesktopTasksController
import com.android.wm.shell.desktopmode.DesktopUserRepositories
import com.android.wm.shell.desktopmode.SnapController
import com.android.wm.shell.desktopmode.data.DesktopRepository
import com.android.wm.shell.desktopmode.multidesks.DeskSwitchTransitionHandler
import com.android.wm.shell.shared.desktopmode.DesktopModeTransitionSource
import com.android.wm.shell.sysui.ShellInit
import com.android.wm.shell.transition.FocusTransitionObserver
import com.android.wm.shell.transition.Transitions
import com.android.wm.shell.transition.Transitions.TransitionFinishCallback
import com.android.wm.shell.windowdecor.DesktopModeWindowDecorViewModel
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
 *
 * Phase D3: [HyprDecorations] styles the windows (corners, gradient border, opacity, caption
 * only on floating windows) and [HyprAnimator] animates the transitions this controller starts
 * (re-tiling, workspace switches) and the desk transitions it sees first (windows opening and
 * closing): as the newest handler it is asked before Shell's own animations. [HyprMouse] adds
 * Hyprland's mouse binds (MOD + left drag moves, MOD + right drag resizes, MOD + middle click
 * toggles floating).
 *
 * Phase D4: [HyprShell] hands HyprLauncher (home, bar, launcher, overview) the dispatchers and
 * a [state] of the workspaces and windows; the bar's strip is kept out of the layout
 * ([setBarReserved]), and the `launcher` and `overview` dispatchers go to the launcher.
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
    private val snapController: SnapController,
    private val mixedHandler: Optional<DesktopMixedTransitionHandler>,
    inputManager: InputManager,
    private val decorViewModel: Optional<DesktopModeWindowDecorViewModel>,
) : Transitions.TransitionHandler, Transitions.TransitionObserver {

    /**
     * Configuration pushed by the launcher (IHyprShell.setConfig), over the properties; from
     * HyprLauncher's Lua (D5) it is complete (`source` = `lua`).
     */
    @Volatile
    var configOverrides: Bundle = Bundle()
        set(value) {
            field = value
            mainExecutor.execute {
                onConfigChanged?.invoke()
                mouse.onConfigChanged()
                zoom.onConfigChanged(value.getString("zoom_factor"))
                sync()
            }
        }
    /** Called on Shell's main thread after [configOverrides] changed (binds, mouse modifier). */
    var onConfigChanged: (() -> Unit)? = null
    private val tile = HyprTile(readConfig(Bundle()))
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
    /** Tasks opened since the last sync: window rules with a workspace apply to them once. */
    private val newTasks = HashSet<Int>()
    /**
     * Bounds the dispatchers gave floating tasks in transitions that may not have landed yet:
     * a Lua function that floats, resizes and moves a window in a row builds on them.
     */
    private val pendingBounds = HashMap<Int, Rect>()
    /** Opacity of tasks a window rule gives one (active, inactive). */
    private val ruleOpacity = HashMap<Int, Pair<Float, Float>>()

    val decorations = HyprDecorations(
        enabled = { enabled },
        isTiled = { tile.isTiled(it) },
        isVisible = { userRepositories.isPresent && repository.isVisibleTask(it) },
        opacityRule = { ruleOpacity[it] },
    )
    private val animator = HyprAnimator()
    private var animationsEnabled = HyprStyleProps.animationsEnabled(Bundle())
    private val mouse = HyprMouse(inputManager, this)
    private val touch = HyprTouch(context, inputManager, this, mainExecutor)
    /** Window history and Hyprland's window switching dispatchers (Phase E3). */
    val focus = HyprFocus(context, this, taskOrganizer, transitions, focusTransitionObserver, mainExecutor)
    private val zoom = HyprZoom(context, inputManager, mainExecutor)
    /** The window a dispatcher acts on instead of the focused one (the touch window menu). */
    private var dispatchTarget: RunningTaskInfo? = null
    /** TRANSIT_CHANGE transitions this controller started (re-tiling): it animates them. */
    private val ownChanges = HashSet<IBinder>()
    /** Desk switches this controller started and animates. */
    private val deskSwitches = HashMap<IBinder, DeskSwitch>()
    /** Requests this controller saw: the trigger task and whether it was visible before. */
    private val requests = HashMap<IBinder, Pair<Int, Boolean>>()
    /** Where a window really is when a transition starts (a mouse-dragged window), absolute. */
    private val startOverride = HashMap<Int, Rect>()
    private var mouseDrag: MouseDrag? = null
    /** Strips the launcher's bar keeps per display: top and bottom heights (Phase D4). */
    private val barReserved = HashMap<Int, Pair<Int, Int>>()
    /** Workspace numbers of each user's displays' desks (Phase E2): workspace N is a desk of its own. */
    private val workspaceMaps = HashMap<String, WorkspaceMap>()
    /** Displays with a desk being made for a workspace (its number); new desks wait for it. */
    private val creatingDesks = HashMap<Int, Int>()
    /** Empty desks being removed. */
    private val removingDesks = HashSet<Int>()
    /**
     * Desk switches until they finish ([deskSwitches] loses them when the animation starts, before
     * Shell's repository has the new active desk): no desk is removed meanwhile.
     */
    private val runningDeskSwitches = HashSet<IBinder>()
    private val removeEmptyDesksRunnable = Runnable { removeEmptyDesks() }
    /** Workspace number of each task, by [workspaceKey], as last saved (see [rememberTaskNumbers]). */
    private val taskNumbers = HashMap<String, Map<Int, Int>>()
    /** Lua's workspace rules, by number; [mapWorkspaceRules] turns them into rules by desk. */
    private var workspaceRuleList: List<WorkspaceRule> = emptyList()
    /** Special workspaces (E5): which windows are on which, which one each display shows. */
    private val special by lazy { SpecialWorkspaces.decode(workspacePrefs.getString(PREF_SPECIAL, "") ?: "") }
    /** Per display, the desk holding the windows of hidden special workspaces ([stashDesk]). */
    private val stashDesks = HashMap<Int, Int>()
    /** Per display, the last active desk that is not the stash desk. */
    private val lastNormalDesk = HashMap<Int, Int>()
    /** Dim layers over the windows behind a shown special workspace, by task ([updateDim]). */
    private val dims = HashMap<Int, SurfaceControl>()
    /** The workspace numbers, kept over a SystemUI restart. */
    private val workspacePrefs by lazy {
        context.createDeviceProtectedStorageContext()
            .getSharedPreferences(PREFS_WORKSPACES, Context.MODE_PRIVATE)
    }

    /** Dispatchers the launcher runs (`launcher`, `overview`); set by [HyprShell]. */
    var launcherDispatch: ((String, String) -> Boolean)? = null
    /** Called on Shell's main thread when workspaces, windows or focus may have changed. */
    var onStateChanged: (() -> Unit)? = null

    private data class DeskSwitch(val displayId: Int, val from: Int, val to: Int)

    /** A window being moved or resized with MOD + mouse. */
    private class MouseDrag(
        val task: RunningTaskInfo,
        val resize: Boolean,
        val x0: Float,
        val y0: Float,
        /** Absolute bounds when the drag started. */
        val bounds: Rect,
        /** Which corner a resize drags: the one nearest to where the button went down. */
        val right: Boolean,
        val bottom: Boolean,
        val leash: SurfaceControl?,
    ) {
        var lastDx = 0
        var lastDy = 0
        var current = Rect(bounds)
    }

    init {
        shellInit.addInitCallback(::onInit, this)
    }

    private fun onInit() {
        zoom.start()
        if (!userRepositories.isPresent) {
            Log.w(TAG, "desktop mode is off, no tiling")
            return
        }
        transitions.addHandler(this)
        transitions.registerObserver(this)
        decorations.install()
        mouse.start()
        touch.start()
        focus.start()
        // Window titles change without a transition; the bar shows the focused one.
        taskOrganizer.addTaskInfoChangedListener(object : ShellTaskOrganizer.TaskInfoChangedListener {
            override fun onTaskInfoChanged(taskInfo: RunningTaskInfo) {
                onStateChanged?.invoke()
            }
        })
        Log.i(TAG, "hyprtile ready: ${tile.config}, animations " +
            (if (animationsEnabled) "on" else "off"))
    }

    /** Tiling is on (persist.hypr.tiling, Lua's shell.tiling); off, Shell's own behaviour comes back. */
    val enabled get() = userRepositories.isPresent &&
        (configOverrides.getString("tiling")?.toBooleanStrictOrNull()
            ?: SystemProperties.getBoolean(PROP_ENABLED, true))

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
        requests[transition] = task.taskId to task.isVisible
        // E5: the app of a hidden special window came to front (started again). Shell would
        // activate the stash desk; the window shows over the active desk instead.
        val reshow = special.of(task.taskId)?.takeIf {
            opening && special.shownOn(task.displayId) != it &&
                repository.getDeskIdForTask(task.taskId) == stashDesk(task.displayId)
        }
        if (reshow != null) {
            Log.d(TAG, "task ${task.taskId} of hidden special:$reshow came to front")
            repository.addTaskToDesk(task.displayId, deskId, task.taskId, isVisible = true,
                taskBounds = Rect(task.configuration.windowConfiguration.bounds))
            mainExecutor.execute {
                hideSpecial(task.displayId)
                showSpecial(task.displayId, deskId, reshow)
            }
        }
        inRequest = true
        val other = try {
            transitions.dispatchRequest(transition, request, this)
        } finally {
            inRequest = false
        }
        val wct = other?.mWct ?: WindowContainerTransaction()
        updateConfig()
        // The desk's layout, or a special workspace's (E5).
        var layoutKey: Int = deskId
        val changed = if (opening) {
            if (becomesFreeform(task, wct)) layoutKey = requestLayout(task, deskId, wct)
            if (!becomesFreeform(task, wct)) {
                tile.close(task.taskId) != null
            } else if (tile.workspaceOf(task.taskId) == layoutKey) {
                // Already tiled here: a dialog activity opening on top of it (a permission
                // prompt) does not make the window float.
                false
            } else {
                val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId)
                    .firstOrNull { it != task.taskId && tile.isTiled(it) }
                newTasks += task.taskId
                tile.open(task.taskId, layoutKey, focused,
                    float = task.taskId in userFloated || floats(task))
            }
        } else {
            tile.close(task.taskId)?.also { layoutKey = it } != null
        }
        if (changed) {
            Log.d(TAG, "request ${request.type} task ${task.taskId} desk $deskId -> relayout")
            addBounds(layoutKey, task.displayId, wct, task)
        }
        if (opening && !tile.isPinned(task.taskId)) keepPinnedOnTop(deskId, wct)
        return other ?: Transitions.RequestResult(wct)
    }

    /**
     * A task that opens goes to the top of its desk, above always-on-top siblings too (only
     * reordering keeps them above), so the desk's pinned windows are reordered above it again.
     */
    private fun keepPinnedOnTop(deskId: Int, wct: WindowContainerTransaction) {
        for (id in tile.pinned()) {
            if (repository.getDeskIdForTask(id) != deskId || repository.isMinimizedTask(id)) continue
            val pinned = taskOrganizer.getRunningTaskInfo(id) ?: continue
            wct.reorder(pinned.token, true)
        }
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

    /**
     * Animates the transitions this controller started (re-tiling, desk switches) and desk
     * transitions that come from a request it saw (a window opening, coming to front, closing
     * or going to back, with its neighbours moving), also when Shell's mixed handler has a
     * pending launch for them (it is asked after this handler).
     * Anything else (leaving the desk, fullscreen, home, wallpaper, activities inside a task)
     * goes to Shell's handlers.
     */
    override fun startAnimation(
        transition: IBinder,
        info: TransitionInfo,
        startTransaction: SurfaceControl.Transaction,
        finishTransaction: SurfaceControl.Transaction,
        finishCallback: TransitionFinishCallback,
    ): Boolean {
        val desk = deskSwitches.remove(transition)
        val own = ownChanges.remove(transition)
        val request = requests.remove(transition)
        if (!animationsEnabled || !enabled) return false
        if (desk != null) {
            return animateDeskSwitch(desk, info, startTransaction, finishTransaction, finishCallback)
        }
        if (!own && request == null) return false
        val changes = windowChanges(info, finishTransaction, request) ?: return false
        // Shell's desktop launches leave a pending launch in its mixed handler, which would
        // animate next; it is told the transition was consumed so it drops that state.
        if (!own) {
            mixedHandler.ifPresent {
                if (it.hasTransition(transition)) it.onTransitionConsumed(transition, false, null)
            }
        }
        Log.d(TAG, "animating ${if (own) "change" else "request"} ${info.debugId}: " +
            changes.joinToString { "${it.kind}" })
        animator.animateWindows(changes, startTransaction) { finishCallback.onTransitionFinished(null) }
        return true
    }

    /**
     * The window changes of a desk transition, or null when it holds anything else. Sets each
     * window's final state in [finishT].
     */
    private fun windowChanges(
        info: TransitionInfo,
        finishT: SurfaceControl.Transaction,
        request: Pair<Int, Boolean>?,
    ): List<HyprAnimator.WindowChange>? {
        if (info.rootCount != 1) return null
        val display = info.getRoot(0).let { root ->
            displayController.getDisplayLayout(root.displayId)?.let { Rect(0, 0, it.width(), it.height()) }
        } ?: return null
        val out = ArrayList<HyprAnimator.WindowChange>()
        val deskIds = repository.getAllDeskIds()
        for (c in info.changes) {
            val task = c.taskInfo
            if (task == null || c.hasFlags(FLAG_IS_WALLPAPER)) {
                Log.d(TAG, "not animating ${info.debugId}: non-task change ${c.mode}")
                return null
            }
            if (task.taskId in deskIds) {
                // The desk root itself (it may come to front with its first task).
                if (c.mode == TRANSIT_OPEN || c.mode == TRANSIT_CLOSE) return null
                continue
            }
            // A closing task may already be gone from the repository: its parent is the desk.
            if (task.parentTaskId !in deskIds && repository.getDeskIdForTask(task.taskId) == null) {
                Log.d(TAG, "not animating ${info.debugId}: task ${task.taskId} outside desks")
                return null
            }
            if (task.windowingMode != WINDOWING_MODE_FREEFORM ||
                task.activityType != ACTIVITY_TYPE_STANDARD) {
                Log.d(TAG, "not animating ${info.debugId}: task ${task.taskId} not freeform")
                return null
            }
            val (from, to) = HyprAnimator.relBounds(c)
            val alpha = decorations.opacity(task.taskId, task.isFocused)
            val kind = when (c.mode) {
                TRANSIT_OPEN -> HyprAnimator.WindowChange.Kind.OPEN
                TRANSIT_TO_FRONT ->
                    if (request != null && request.first == task.taskId && !request.second) {
                        HyprAnimator.WindowChange.Kind.OPEN
                    } else {
                        HyprAnimator.WindowChange.Kind.MOVE
                    }
                TRANSIT_CLOSE, TRANSIT_TO_BACK -> HyprAnimator.WindowChange.Kind.CLOSE
                else -> HyprAnimator.WindowChange.Kind.MOVE
            }
            val start = startOverride.remove(task.taskId)?.let {
                Rect(it).apply { offset(from.left - c.startAbsBounds.left, from.top - c.startAbsBounds.top) }
            } ?: from
            out += HyprAnimator.WindowChange(c.leash, kind, if (start.isEmpty) to else start, to,
                alpha, display)
            finishT.setMatrix(c.leash, 1f, 0f, 0f, 1f)
            if (kind != HyprAnimator.WindowChange.Kind.CLOSE) {
                finishT.setPosition(c.leash, to.left.toFloat(), to.top.toFloat())
                finishT.setAlpha(c.leash, alpha)
            }
        }
        return out.takeIf { it.isNotEmpty() }
    }

    private fun animateDeskSwitch(
        desk: DeskSwitch,
        info: TransitionInfo,
        startT: SurfaceControl.Transaction,
        finishT: SurfaceControl.Transaction,
        finishCallback: TransitionFinishCallback,
    ): Boolean {
        val repo = repository
        val layout = displayController.getDisplayLayout(desk.displayId) ?: return false
        val display = Rect(0, 0, layout.width(), layout.height())
        val fromDesk = info.changes.find { it.taskInfo?.taskId == desk.from }
        val toDesk = info.changes.find { it.taskInfo?.taskId == desk.to }
        // Pinned windows come along to the new desk and stay where they are.
        fun slides(c: TransitionInfo.Change) = c.taskInfo?.let { !tile.isPinned(it.taskId) } ?: true
        val fromTasks = info.changes.filter { it.taskInfo?.parentTaskId == desk.from && slides(it) }
        val toTasks = info.changes.filter { it.taskInfo?.parentTaskId == desk.to && slides(it) }
        for (c in info.changes) {
            val id = c.taskInfo?.takeIf { tile.isPinned(it.taskId) }?.taskId ?: continue
            val alpha = decorations.opacity(id, c.taskInfo?.isFocused == true)
            startT.setAlpha(c.leash, alpha)
            finishT.setAlpha(c.leash, alpha)
        }
        // Workspaces slide by number, as Hyprland's do.
        val numbers = workspaces(desk.displayId)
        val fromPos = numbers.numberOf(desk.from) ?: return false
        val toPos = numbers.numberOf(desk.to) ?: return false
        Log.d(TAG, "animating desk switch ${desk.from} -> ${desk.to}")
        snapController.onDeskSwitchAnimationStarting(desk.displayId, desk.from, desk.to)
        val toAlphas = toTasks.map {
            it.leash to decorations.opacity(it.taskInfo?.taskId ?: -1, it.taskInfo?.isFocused == true)
        }
        // As Shell's switch: the old desk's tasks end hidden, the new desk's shown.
        fromDesk?.let { finishT.setPosition(it.leash, display.left.toFloat(), display.top.toFloat()) }
        toDesk?.let { finishT.setPosition(it.leash, display.left.toFloat(), display.top.toFloat()) }
        for (c in fromTasks) finishT.setAlpha(c.leash, 0f)
        for ((leash, a) in toAlphas) finishT.setAlpha(leash, a)
        animator.animateDeskSwitch(display, fromDesk?.leash, toDesk?.leash,
            fromTasks.map { it.leash }, toAlphas, forward = toPos > fromPos, startT) {
            snapController.onDeskSwitchAnimationEnded(desk.displayId, desk.to)
            finishCallback.onTransitionFinished(null)
        }
        return true
    }

    /**
     * Relayouts the decorations whose caption no longer matches the window's state. A window
     * tiled without moving (its tile is where it already was, e.g. a desk restored at boot)
     * gets no transition, so Shell would not decorate it again.
     */
    private fun refreshCaptions() {
        val vm = decorViewModel.orElse(null) ?: return
        for (id in decorations.staleCaptions()) {
            taskOrganizer.getRunningTaskInfo(id)?.let {
                Log.d(TAG, "caption of task $id out of date, relayout")
                vm.onTaskInfoChanged(it)
            }
        }
    }

    /** Starts a TRANSIT_CHANGE with new bounds that this controller animates. */
    private fun startChange(wct: WindowContainerTransaction) {
        ownChanges += transitions.startTransition(TRANSIT_CHANGE, wct, this)
    }

    // ---- observer path: catch up with changes made without a request ----

    override fun onTransitionReady(
        transition: IBinder,
        info: TransitionInfo,
        startTransaction: SurfaceControl.Transaction,
        finishTransaction: SurfaceControl.Transaction,
    ) {}

    override fun onTransitionFinished(transition: IBinder, aborted: Boolean) {
        ownChanges -= transition
        deskSwitches -= transition
        runningDeskSwitches -= transition
        requests -= transition
        // Shell's animations end at alpha 1; this runs after the finish transaction is applied.
        mainExecutor.execute { decorations.reapplyOpacity() }
        // The repository learns about task changes from task-info callbacks that can arrive
        // just after the transition; sync once things settle.
        mainExecutor.removeCallbacks(syncRunnable)
        mainExecutor.executeDelayed(syncRunnable, SYNC_DELAY_MS)
    }

    private fun sync() {
        if (!enabled) {
            onStateChanged?.invoke()
            return
        }
        val configChanged = updateConfig()
        val repo = repository
        pendingBounds.clear()
        val displays = repo.getAllDeskIds()
            .mapNotNull { runCatching { repo.getDisplayForDesk(it) }.getOrNull() }.toSet()
        for (displayId in displays) {
            val deskId = repo.getActiveDeskId(displayId) ?: continue
            // E5: Shell brought the stash desk of hidden special windows to front.
            stashDesk(displayId)?.let { if (it == deskId) { recoverFromStash(displayId, it); continue } }
            lastNormalDesk[displayId] = deskId
            syncSpecial(displayId, deskId)
            // Pinned windows left behind by a desk switch this controller did not start (Shell's
            // own, from home or a window brought to front on another desk) follow now.
            val pinWct = WindowContainerTransaction()
            val carried = carryPinned(displayId, deskId, pinWct)
            if (carried.isNotEmpty()) {
                Log.d(TAG, "pinned ${carried.map { it.taskId }} follow to desk $deskId")
                startChange(pinWct)
                recordCarried(displayId, deskId, carried)
            }
            // App windows only: a new desk's minimization root (a root task of undefined type)
            // can show up in the active desk's list for a moment when a desk is made (E5).
            val expanded = repo.getExpandedTasksIdsInDeskOrdered(deskId).filter {
                !repo.isClosingTask(it) && taskOrganizer.getRunningTaskInfo(it)?.let { t ->
                    t.windowingMode == WINDOWING_MODE_FREEFORM && t.activityType == ACTIVITY_TYPE_STANDARD
                } == true
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
                // A task Shell put on another desk than the request said (it relaunched into
                // the desk it last lived on) is tiled again here.
                if (tile.workspaceOf(id) == deskId || id in userFloated || special.isMember(id)) continue
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
                startChange(wct)
            }
        }
        applied.keys.retainAll { tile.isTiled(it) }
        maximized.retainAll { tile.isTiled(it) }
        refreshCaptions()
        userFloated.retainAll { repo.getDeskIdForTask(it) != null }
        for (id in tile.pinned().toList()) if (repo.getDeskIdForTask(id) == null) tile.close(id)
        applyWindowRules()
        syncSpecialAfter()
        focus.onSync()
        // Once things settle: removing a desk Shell still takes for the active one sends the
        // display home.
        mainExecutor.removeCallbacks(removeEmptyDesksRunnable)
        mainExecutor.executeDelayed(removeEmptyDesksRunnable, REMOVE_DESKS_DELAY_MS)
        onStateChanged?.invoke()
    }

    /**
     * Window rules: the opacity of every desktop task, and, once for a task opened since the
     * last sync, the workspace it belongs on (moved there and followed, or moved silently).
     */
    private fun applyWindowRules() {
        val rules = tile.config.windowRules
        ruleOpacity.clear()
        val opened = newTasks.toList()
        newTasks.clear()
        if (rules.isEmpty()) return
        for (task in taskOrganizer.getRunningTasks()) {
            if (repository.getDeskIdForTask(task.taskId) == null) continue
            val pkg = task.baseActivity?.packageName
            val title = task.taskDescription?.label?.toString()
            val active = rules.effect(pkg, title) { it.opacityActive }
            val inactive = rules.effect(pkg, title) { it.opacityInactive }
            if (active != null || inactive != null) {
                ruleOpacity[task.taskId] = (active ?: decorations.config.activeOpacity) to
                    (inactive ?: decorations.config.inactiveOpacity)
            }
        }
        for (id in opened) {
            val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
            val pkg = task.baseActivity?.packageName
            val title = task.taskDescription?.label?.toString()
            val rule = rules.lastOrNull { (it.workspace != null || it.special != null) && it.matches(pkg, title) } ?: continue
            // `workspace = "special:name [silent]"` (E5).
            val ruleSpecial = rule.special
            if (ruleSpecial != null) {
                if (special.of(id) == ruleSpecial) continue
                Log.d(TAG, "window rule ${rule.name}: task $id to special:$ruleSpecial")
                moveToSpecial(task, ruleSpecial, rule.silent)
                opened.drop(opened.indexOf(id) + 1).let { newTasks.addAll(it) }
                break
            }
            val n = rule.workspace ?: continue
            if (n > workspaceLimit || currentWorkspaceOf(task) == n) continue
            Log.d(TAG, "window rule ${rule.name}: task $id to workspace $n" + if (rule.silent) " (silent)" else "")
            withWorkspaceDesk(task.displayId, n) { desk ->
                if (rule.silent) moveSilently(task, desk) else switchToDesk(task.displayId, desk, moving = task)
            }
            // One move per sync: the next sync, after its transition, takes the next one.
            opened.drop(opened.indexOf(id) + 1).let { newTasks.addAll(it) }
            break
        }
        decorations.reapplyOpacity()
    }

    /** The workspace number of [task]'s desk, 0 when it is on none. */
    private fun currentWorkspaceOf(task: RunningTaskInfo): Int {
        val desk = repository.getDeskIdForTask(task.taskId) ?: return 0
        return workspaces(task.displayId).numberOf(desk) ?: 0
    }

    /**
     * Hyprland's `movetoworkspacesilent`: [task] goes to [deskId] without switching to it; the
     * desk it leaves re-tiles in the same transition.
     */
    private fun moveSilently(task: RunningTaskInfo, deskId: Int) {
        val controller = desktopTasksController.orElse(null) ?: return
        val from = repository.getDeskIdForTask(task.taskId) ?: return
        if (from == deskId) return
        val id = task.taskId
        val floating = tile.isFloating(id) || id in userFloated
        val wct = WindowContainerTransaction()
        controller.addMoveToDeskTaskChanges(wct, task, deskId)
        maximized -= id
        applied.remove(id)
        tile.close(id)
        addBounds(from, task.displayId, wct)
        val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId).firstOrNull { tile.isTiled(it) }
        tile.open(id, deskId, focused, float = floating)
        addBounds(deskId, task.displayId, wct, task)
        Log.d(TAG, "move task $id silently: desk $from -> $deskId")
        startChange(wct)
        // Shell's own moves record the new desk in their transition handlers; this one is ours,
        // and without it the repository keeps the task on the old desk (whose layout would
        // keep a hole for it).
        repository.addTaskToDesk(task.displayId, deskId, id, isVisible = false,
            taskBounds = Rect(task.configuration.windowConfiguration.bounds))
    }

    // ---- dispatchers (Hyprland's names; Phase D2) ----

    /**
     * Runs a Hyprland dispatcher: `workspace N`, `movetoworkspace N`, `movefocus l|r|u|d`,
     * `movewindow l|r|u|d`, `togglefloating`/`setfloating`/`settiled [taskid:N]`,
     * `pin [toggle|on|off] [taskid:N]`, `resizeactive`/`moveactive [exact] X Y`, `togglesplit`,
     * `fullscreen [0|1]`, `killactive`, `exec <package>`. Workspace arguments may be relative
     * (`e+1`, `-1`). Must be called on Shell's main thread. Returns false for an unknown dispatcher or a bad argument.
     */
    fun dispatch(name: String, arg: String = ""): Boolean {
        Log.d(TAG, "dispatch $name $arg")
        if (dispatchSpecial(name, arg)) return true
        // `previous`, `previous_per_monitor` and binds.workspace_back_and_forth (E3).
        if (name == "workspace" || name == "movetoworkspace" || name == "movetoworkspacesilent") {
            val display = dispatchTarget?.displayId ?: focusedDisplayId()
            val resolved = focus.resolveWorkspace(name, arg, display) ?: return true
            if (resolved != arg) return dispatch(name, resolved)
        }
        when (name) {
            "workspace" -> {
                val displayId = focusedDisplayId()
                val n = resolveWorkspaceOn(displayId, arg) ?: return false
                withWorkspaceDesk(displayId, n) { switchToDesk(displayId, it, moving = null) }
            }
            "movetoworkspace" -> {
                val task = focusedTask() ?: return true
                val n = resolveWorkspaceOn(task.displayId, arg) ?: return false
                withWorkspaceDesk(task.displayId, n) { switchToDesk(task.displayId, it, moving = task) }
            }
            "movetoworkspacesilent" -> {
                val task = focusedTask() ?: return true
                val n = resolveWorkspaceOn(task.displayId, arg) ?: return false
                withWorkspaceDesk(task.displayId, n) { moveSilently(task, it) }
            }
            "resizeactive" -> resizeActive(parseXY(arg) ?: return false)
            "moveactive" -> moveActive(parseXY(arg) ?: return false)
            "movefocus" -> moveFocus(Direction.parse(arg) ?: return false)
            "movewindow" -> moveWindow(Direction.parse(arg) ?: return false)
            "togglefloating" -> toggleFloating(windowTask(parseWindow(arg).getOrElse { return false }))
            "setfloating", "settiled" -> {
                val task = windowTask(parseWindow(arg).getOrElse { return false }) ?: return true
                if (tile.isTiled(task.taskId) == (name == "setfloating")) toggleFloating(task)
            }
            "pin" -> {
                val (action, window) = parsePin(arg) ?: return false
                pin(windowTask(window) ?: return true, action)
            }
            "togglesplit" -> toggleSplit()
            "fullscreen" -> if (arg.trim() == "1") toggleMaximize() else toggleFullscreen()
            "killactive" -> {
                val target = dispatchTarget
                if (target != null) {
                    decorViewModel.ifPresent { it.closeTask(target) }
                } else {
                    shellKeyGestureHandler.ifPresent { it.quitFocusedDesktopTask() }
                }
            }
            "notifications" ->
                context.getSystemService(StatusBarManager::class.java)?.expandNotificationsPanel()
            "quicksettings" ->
                context.getSystemService(StatusBarManager::class.java)?.expandSettingsPanel()
            "exec" -> exec(arg.trim())
            // SystemUI-side system actions (E9): clipboard, translate, screenshot, lock, users.
            in HyprSystemActions.NAMES -> return HyprSystemActions.dispatch(name, arg)
            "zoom" -> return zoom.dispatch(arg)
            // Run by HyprLauncher (rofi-like launcher, hyprexpo-like overview, Lua functions, the bar).
            "launcher", "overview", "hyprexpo:expo", "lua", "bar" ->
                return launcherDispatch?.invoke(name, arg) ?: false
            // Host-only functions (E11): system keys, display modes; reload and wallpaper are
            // HyprLauncher's.
            in HyprHostActions.NAMES -> return HyprHostActions.dispatch(context, name, arg)
            "forcekillactive" -> (focusedTask() ?: taskOrganizer.getRunningTasks()
                .find { focusTransitionObserver.hasGlobalFocus(it) })
                ?.let { HyprHostActions.forceStop(context, it) }
            "reload", "wallpaper" -> return launcherDispatch?.invoke(name, arg) ?: false
            // Window history and switching (E3): focuscurrentorlast, cyclenext, focuswindow, ...
            else -> return focus.dispatch(name, arg)
        }
        return true
    }

    /**
     * Runs [name] on window [taskId] instead of the focused one (the touch window menu): the
     * dispatchers that act on "the focused window" take it. Main thread.
     */
    fun dispatchOn(taskId: Int, name: String, arg: String = ""): Boolean {
        val task = taskOrganizer.getRunningTaskInfo(taskId) ?: return false
        dispatchTarget = task
        try {
            return dispatch(name, arg)
        } finally {
            dispatchTarget = null
        }
    }

    /**
     * Hyprland's `focuswindow`, by task id: switches to the window's workspace if needed and
     * brings it to front (a minimized window comes back), with Shell's own transition.
     */
    private fun focusWindow(taskId: Int) {
        val controller = desktopTasksController.orElse(null) ?: return
        val task = taskOrganizer.getRunningTaskInfo(taskId)
        // A window of a hidden special workspace: that special workspace shows (E5).
        val name = special.of(taskId)
        if (task != null && name != null && special.shownOn(task.displayId) != name) {
            toggleSpecial(task.displayId, name)
            return
        }
        if (task == null) {
            controller.moveTaskToFront(taskId, repository.userId, null, UnminimizeReason.TASKBAR_TAP)
        } else {
            controller.moveTaskToFront(task, null, UnminimizeReason.TASKBAR_TAP)
        }
    }

    private fun focusedDisplayId(): Int =
        focusTransitionObserver.globallyFocusedDisplayId.takeIf { it != INVALID_DISPLAY }
            ?: DEFAULT_DISPLAY

    /** The focused task when it is a desktop task of the active desk. */
    private fun focusedTask(): RunningTaskInfo? = dispatchTarget ?: taskOrganizer.getRunningTasks().find {
        repository.isActiveTask(it.taskId) && focusTransitionObserver.hasGlobalFocus(it)
    }

    /** The desktop task [taskId] names, or the focused one when it is null. */
    private fun windowTask(taskId: Int?): RunningTaskInfo? {
        if (taskId == null) return focusedTask()
        return taskOrganizer.getRunningTaskInfo(taskId)?.takeIf { repository.getDeskIdForTask(taskId) != null }
    }

    /** The desks of [displayId] in Shell's order. */
    private fun orderedDesks(displayId: Int): List<Int> {
        val repo = repository
        val stash = stashDesk(displayId)
        return repo.getDeskIds(displayId).filter { it != stash }
            .sortedBy { repo.getDeskPosition(it) ?: Int.MAX_VALUE }
    }

    private fun currentWorkspace(displayId: Int): Int {
        val active = repository.getActiveDeskId(displayId) ?: return 1
        return workspaces(displayId).numberOf(active) ?: 1
    }

    /** How many workspaces there are (Lua's `shell.workspaces`), 1..1000. */
    private val workspaceLimit: Int
        get() = (configOverrides.getString("workspaces")?.trim()?.toIntOrNull() ?: DEFAULT_WORKSPACES)
            .coerceIn(1, MAX_WORKSPACES)

    /** A workspace argument (`7`, `e+1`, `-1`) on [displayId]; null when bad or past the limit. */
    private fun resolveWorkspaceOn(displayId: Int, arg: String): Int? =
        resolveWorkspace(arg, currentWorkspace(displayId), workspaces(displayId).numbers, workspaceLimit)

    private fun workspaceKey(displayId: Int) = "u${repository.userId}d$displayId"

    /** The workspace numbers of [displayId]'s desks as last known (loaded once). */
    private fun workspaceMap(displayId: Int): WorkspaceMap = workspaceMaps.getOrPut(workspaceKey(displayId)) {
        WorkspaceMap.decode(workspacePrefs.getString(workspaceKey(displayId), "") ?: "")
    }

    /**
     * The workspace numbers of [displayId]'s desks, brought in line with Shell's desks: a desk the
     * shell did not make (Shell's default desk, desks restored at boot) gets the lowest free number.
     */
    private fun workspaces(displayId: Int): WorkspaceMap {
        val map = workspaceMap(displayId)
        // While a desk is being made for a workspace, the new desk waits for that number.
        if (displayId !in creatingDesks &&
            map.sync(orderedDesks(displayId), ::tasksOfDesk, savedTaskNumbers(displayId))
        ) {
            saveWorkspaces(displayId)
        }
        return map
    }

    private fun tasksOfDesk(deskId: Int): Collection<Int> = repository.getActiveTaskIdsInDesk(deskId)

    /**
     * Shell restores its desks under new ids when SystemUI restarts (and at boot), with the same
     * tasks: each task's workspace number is kept so a restored desk gets its number back.
     */
    private fun savedTaskNumbers(displayId: Int): Map<Int, Int> = taskNumbers.getOrPut(workspaceKey(displayId)) {
        WorkspaceMap.decodeTasks(workspacePrefs.getString(workspaceKey(displayId) + "t", "") ?: "")
    }

    private fun rememberTaskNumbers(displayId: Int) {
        val now = workspaces(displayId).taskNumbers(::tasksOfDesk)
        if (now == savedTaskNumbers(displayId)) return
        taskNumbers[workspaceKey(displayId)] = now
        workspacePrefs.edit().putString(workspaceKey(displayId) + "t", WorkspaceMap.encodeTasks(now)).apply()
    }

    private fun saveWorkspaces(displayId: Int) {
        workspacePrefs.edit().putString(workspaceKey(displayId), workspaceMap(displayId).encode()).apply()
        // A workspace that got (or lost) its desk takes its rule along.
        if (mapWorkspaceRules()) applied.clear()
    }

    /**
     * Workspace rules are by number, the layouts by desk: maps [workspaceRuleList] onto the desks
     * the workspaces have now. Returns true when the rules by desk changed.
     */
    private fun mapWorkspaceRules(): Boolean {
        val byDesk = HashMap<Int, WorkspaceRule>()
        if (workspaceRuleList.isNotEmpty() && userRepositories.isPresent) {
            val repo = repository
            for (displayId in repo.getAllDeskIds().mapNotNull {
                runCatching { repo.getDisplayForDesk(it) }.getOrNull()
            }.toSet()) {
                val numbers = workspaceMap(displayId)
                for (r in workspaceRuleList) numbers.deskOf(r.workspace)?.let { byDesk[it] = r }
            }
        }
        // Rules for special workspaces go to their layouts (E5).
        for (r in workspaceRuleList) r.special?.let { byDesk[special.keyOf(it)] = r }
        if (byDesk == tile.workspaceRules) return false
        tile.workspaceRules = byDesk
        return true
    }

    /**
     * Runs [block] with the desk of workspace [n], creating it first when it does not exist
     * (Hyprland makes workspaces on demand; [removeEmptyDesks] takes them away again).
     */
    private fun withWorkspaceDesk(displayId: Int, n: Int, block: (Int) -> Unit) {
        workspaces(displayId).deskOf(n)?.let { block(it); return }
        val controller = desktopTasksController.orElse(null) ?: return
        if (displayId in creatingDesks) {
            Log.d(TAG, "workspace $n: workspace ${creatingDesks[displayId]} is still being made")
            return
        }
        Log.d(TAG, "workspace $n: creating a desk on display $displayId")
        creatingDesks[displayId] = n
        controller.createDesk(
            displayId = displayId,
            userId = repository.userId,
            enforceDeskLimit = false,
            activateDesk = false,
            enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER,
        ) { deskId ->
            mainExecutor.execute {
                creatingDesks.remove(displayId)
                if (deskId in orderedDesks(displayId)) {
                    workspaceMap(displayId).put(deskId, n)
                    saveWorkspaces(displayId)
                    workspaces(displayId)
                    block(deskId)
                } else {
                    Log.w(TAG, "desk creation on display $displayId did not add a desk ($deskId)")
                }
            }
        }
    }

    /**
     * Hyprland removes a workspace when it is empty and not shown: empty desks that are not
     * active are removed, one desk per display stays. Not while desks are switched or made.
     */
    private fun removeEmptyDesks() {
        if (!enabled) return
        val controller = desktopTasksController.orElse(null) ?: return
        if (deskSwitches.isNotEmpty() || runningDeskSwitches.isNotEmpty() || creatingDesks.isNotEmpty()) return
        val repo = repository
        removingDesks.retainAll(repo.getAllDeskIds())
        val displays = repo.getAllDeskIds()
            .mapNotNull { runCatching { repo.getDisplayForDesk(it) }.getOrNull() }.toSet()
        for (displayId in displays) {
            rememberTaskNumbers(displayId)
            val active = repo.getActiveDeskId(displayId)
            val numbers = workspaces(displayId)
            val desks = numbers.numbers.mapNotNull { numbers.deskOf(it) }.filter { it !in removingDesks }
            val empty = desks.filter { it != active && repo.getActiveTaskIdsInDesk(it).isEmpty() }
            // With nothing active or occupied, the lowest empty workspace stays.
            for (d in if (empty.size == desks.size) empty.drop(1) else empty) {
                Log.d(TAG, "removing empty workspace ${numbers.numberOf(d)} (desk $d)")
                removingDesks += d
                controller.removeDesk(d, repo.userId, ExitReason.UNKNOWN_EXIT, removeTasks = false)
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
        val carried = carryPinned(displayId, to, wct, skip = moving?.taskId) +
            presentSpecial(displayId, to, wct, skip = moving?.taskId)
        Log.d(TAG, "switch desk $from -> $to on display $displayId" +
            (moving?.let { ", moving task ${it.taskId}" } ?: ""))
        val transition = if (animationsEnabled) {
            transitions.startTransition(TRANSIT_DESKTOP_MODE_DESK_TO_DESK_SWITCH, wct, this).also {
                deskSwitches[it] = DeskSwitch(displayId, from, to)
            }
        } else {
            deskSwitchTransitionHandler.startTransition(wct, repo.userId, displayId, from, to)
        }
        runningDeskSwitches += transition
        runOnStart?.invoke(transition)
        recordCarried(displayId, to, carried)
    }

    /**
     * Moves the pinned windows of [displayId] that are on another desk into desk [to], on top,
     * keeping their bounds (Hyprland shows a pinned window on every workspace). Minimized ones
     * stay where they are. Returns the moved tasks, for [recordCarried] once the transition
     * started.
     */
    private fun carryPinned(
        displayId: Int,
        to: Int,
        wct: WindowContainerTransaction,
        skip: Int? = null,
    ): List<RunningTaskInfo> {
        val controller = desktopTasksController.orElse(null) ?: return emptyList()
        val repo = repository
        val moved = ArrayList<RunningTaskInfo>()
        for (id in tile.pinned().toList()) {
            if (id == skip || repo.isMinimizedTask(id)) continue
            val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
            if (task.displayId != displayId) continue
            val desk = repo.getDeskIdForTask(id) ?: continue
            if (desk == to) continue
            val bounds = floatingBounds(task)
            controller.addMoveToDeskTaskChanges(wct, task, to)
            // Shell's move gives the task its cascaded bounds; it keeps its own.
            wct.setBounds(task.token, bounds)
            wct.reorder(task.token, true)
            moved += task
        }
        return moved
    }

    /** Shell's repository learns about moves from its own handlers; ours are recorded here. */
    private fun recordCarried(displayId: Int, to: Int, moved: List<RunningTaskInfo>) {
        for (task in moved) {
            repository.addTaskToDesk(displayId, to, task.taskId, isVisible = true,
                taskBounds = floatingBounds(task))
        }
    }

    /** Where a floating task is, or will be once the dispatchers' transitions land. */
    private fun floatingBounds(task: RunningTaskInfo): Rect =
        pendingBounds[task.taskId]?.let { Rect(it) } ?: Rect(task.configuration.windowConfiguration.bounds)

    private fun setFloatingBounds(task: RunningTaskInfo, bounds: Rect, wct: WindowContainerTransaction) {
        wct.setBounds(task.token, bounds)
        pendingBounds[task.taskId] = Rect(bounds)
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
        if (!wct.isEmpty) startChange(wct)
    }

    private fun toggleFloating(task: RunningTaskInfo? = focusedTask()) {
        task ?: return
        val id = task.taskId
        val deskId = repository.getDeskIdForTask(id) ?: return
        val wct = WindowContainerTransaction()
        if (tile.isTiled(id)) {
            if (!floatOut(task, deskId, wct)) return
        } else {
            if (!task.isResizeable) return
            userFloated -= id
            pendingBounds.remove(id)
            // Tiled again, a pinned window is no longer pinned (hyprtile drops the pin).
            if (tile.isPinned(id)) wct.setAlwaysOnTop(task.token, false)
            val focused = repository.getExpandedTasksIdsInDeskOrdered(deskId)
                .firstOrNull { it != id && tile.isTiled(it) }
            tile.toggleFloating(id, deskId, focused)
        }
        addBounds(deskId, task.displayId, wct, task)
        if (!wct.isEmpty) startChange(wct)
    }

    /**
     * Takes tiled [task] out of its layout as a floating window; Hyprland centres a window that
     * starts floating: half the width, two thirds of the height of the work area. False when
     * the display has no work area.
     */
    private fun floatOut(task: RunningTaskInfo, deskId: Int, wct: WindowContainerTransaction): Boolean {
        val area = workArea(task.displayId) ?: return false
        val id = task.taskId
        tile.toggleFloating(id, deskId)
        userFloated += id
        maximized -= id
        applied.remove(id)
        val w = area.width / 2
        val h = area.height * 2 / 3
        val left = area.left + (area.width - w) / 2
        val top = area.top + (area.height - h) / 2
        setFloatingBounds(task, Rect(left, top, left + w, top + h), wct)
        return true
    }

    /**
     * Hyprland's `pin`: a pinned window floats (a tiled one leaves its layout, centred), stays
     * above the other windows of its desk (WindowManager's always-on-top among the desk root's
     * tasks) and goes along to whichever desk becomes active ([carryPinned]). Unpinned, it
     * stays floating where it is.
     */
    private fun pin(task: RunningTaskInfo, action: Switch) {
        val id = task.taskId
        val on = action.apply(tile.isPinned(id))
        if (on == tile.isPinned(id)) return
        val deskId = repository.getDeskIdForTask(id) ?: return
        val wct = WindowContainerTransaction()
        if (on) {
            if (tile.isTiled(id) && !floatOut(task, deskId, wct)) return
            userFloated += id
            tile.setPinned(id, true)
            wct.setAlwaysOnTop(task.token, true)
            wct.reorder(task.token, true)
        } else {
            tile.setPinned(id, false)
            wct.setAlwaysOnTop(task.token, false)
        }
        Log.d(TAG, "pin task $id: $on")
        addBounds(deskId, task.displayId, wct, task)
        startChange(wct)
        onStateChanged?.invoke()
    }

    // ---- special workspaces (Hyprland's scratchpads; Phase E5) ----

    /**
     * Runs the special-workspace forms of the dispatchers: `togglespecialworkspace [name]`,
     * `workspace special[:name]` (the same toggle), `movetoworkspace[silent] special[:name]`.
     * A window of a special workspace moved to a numbered workspace leaves its special workspace
     * first. Returns true when the dispatcher was handled here.
     */
    private fun dispatchSpecial(name: String, arg: String): Boolean {
        val target = specialName(arg)
        when {
            name == "togglespecialworkspace" ->
                toggleSpecial(focusedDisplayId(), target ?: arg.trim().ifEmpty { "special" })
            name == "workspace" && target != null -> toggleSpecial(focusedDisplayId(), target)
            (name == "movetoworkspace" || name == "movetoworkspacesilent") && target != null ->
                focusedTask()?.let { moveToSpecial(it, target, silent = name == "movetoworkspacesilent") }
            name == "movetoworkspace" || name == "movetoworkspacesilent" -> {
                focusedTask()?.takeIf { special.isMember(it.taskId) }?.let { leaveSpecial(it) }
                return false
            }
            name == "workspace" -> {
                // binds.hide_special_on_workspace_change; otherwise the special workspace stays
                // shown over the new workspace (switchToDesk carries its windows).
                if (configOverrides.getString("hide_special_on_workspace_change") == "true") {
                    hideSpecial(focusedDisplayId())
                }
                return false
            }
            else -> return false
        }
        return true
    }

    /**
     * Hyprland's `togglespecialworkspace`: special workspace [name] shows over the active
     * workspace of [displayId] (in place of another one shown there), or hides again. With no
     * desk active (home in front), the current workspace becomes active first.
     */
    private fun toggleSpecial(displayId: Int, name: String) {
        if (special.shownOn(displayId) == name) {
            hideSpecial(displayId)
            return
        }
        hideSpecial(displayId)
        val desk = repository.getActiveDeskId(displayId)
        if (desk != null && desk != stashDesk(displayId)) {
            showSpecial(displayId, desk, name)
            return
        }
        withWorkspaceDesk(displayId, currentWorkspace(displayId)) { d ->
            desktopTasksController.orElse(null)?.activateDesk(d, repository.userId,
                enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER)
            showSpecial(displayId, d, name)
        }
    }

    /** Shows special workspace [name] over desk [deskId]: its windows come in on top, tiled. */
    private fun showSpecial(displayId: Int, deskId: Int, name: String) {
        special.show(displayId, name)
        saveSpecial()
        Log.d(TAG, "special:$name shown over desk $deskId: ${special.windows(name)}")
        val wct = WindowContainerTransaction()
        val moved = presentSpecial(displayId, deskId, wct, all = true)
        if (!wct.isEmpty) startChange(wct)
        recordCarried(displayId, deskId, moved)
        focusSpecial(displayId, deskId)
        updateDim()
        onStateChanged?.invoke()
    }

    /**
     * Hyprland focuses a special workspace's window when it shows: its newest window comes to
     * front once the move into [deskId] has landed (always-on-top windows do not take the focus
     * by being reordered).
     */
    private fun focusSpecial(displayId: Int, deskId: Int) {
        mainExecutor.executeDelayed({
            val name = special.shownOn(displayId) ?: return@executeDelayed
            val id = special.windows(name).lastOrNull {
                repository.getDeskIdForTask(it) == deskId && !repository.isMinimizedTask(it)
            } ?: return@executeDelayed
            taskOrganizer.getRunningTaskInfo(id)?.let { desktopTasksController.orElse(null)?.moveTaskToFront(it) }
        }, FOCUS_SPECIAL_DELAY_MS)
    }

    /** Hides the special workspace shown on [displayId]: its windows go to the stash desk. */
    private fun hideSpecial(displayId: Int) {
        val name = special.shownOn(displayId) ?: return
        special.hide(displayId)
        saveSpecial()
        Log.d(TAG, "special:$name hidden on display $displayId")
        stashWindows(displayId, special.windows(name))
        updateDim()
        onStateChanged?.invoke()
    }

    /**
     * Adds to [wct] what brings the windows of the special workspace shown on [displayId] into
     * desk [deskId]: moved from the stash desk (or the desk they were left on), always on top
     * of the workspace's windows, tiled in the special workspace's layout. With [all], windows
     * already in [deskId] come to front too. Returns the moved tasks, for [recordCarried].
     */
    private fun presentSpecial(
        displayId: Int,
        deskId: Int,
        wct: WindowContainerTransaction,
        all: Boolean = false,
        skip: Int? = null,
    ): List<RunningTaskInfo> {
        val name = special.shownOn(displayId) ?: return emptyList()
        val controller = desktopTasksController.orElse(null) ?: return emptyList()
        val key = special.keyOf(name)
        val moved = ArrayList<RunningTaskInfo>()
        for (id in special.windows(name)) {
            if (id == skip) continue
            val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
            // Shell's repository and the task's real parent (WindowManager may have moved it).
            if (repository.getDeskIdForTask(id) != deskId || task.parentTaskId != deskId) {
                val bounds = floatingBounds(task)
                controller.addMoveToDeskTaskChanges(wct, task, deskId)
                // Shell's move gives the task cascaded bounds: a floating one keeps its own, a
                // tiled one gets its tile again below.
                if (tile.workspaceOf(id) == key) applied.remove(id) else wct.setBounds(task.token, bounds)
                moved += task
            } else if (!all) {
                continue
            }
            if (tile.workspaceOf(id) == null && !tile.isFloating(id)) {
                tile.open(id, key, float = id in userFloated || floats(task))
            }
            wct.setAlwaysOnTop(task.token, true)
            wct.reorder(task.token, true)
        }
        addBounds(key, displayId, wct)
        return moved
    }

    /** Moves [ids] (windows of a special workspace) to the stash desk of [displayId], hidden. */
    private fun stashWindows(displayId: Int, ids: List<Int>) {
        if (ids.isEmpty()) return
        withStashDesk(displayId) { stash ->
            val controller = desktopTasksController.orElse(null) ?: return@withStashDesk
            val wct = WindowContainerTransaction()
            val moved = ArrayList<Pair<RunningTaskInfo, Rect>>()
            for (id in ids) {
                // Shown again meanwhile (a quick double toggle): it stays.
                if (special.of(id)?.let { special.shownOn(displayId) == it } == true) continue
                val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
                if (repository.getDeskIdForTask(id) == stash) continue
                val bounds = floatingBounds(task)
                controller.addMoveToDeskTaskChanges(wct, task, stash)
                wct.setBounds(task.token, bounds)
                wct.setAlwaysOnTop(task.token, false)
                moved += task to bounds
            }
            if (wct.isEmpty) return@withStashDesk
            startChange(wct)
            for ((task, bounds) in moved) {
                repository.addTaskToDesk(displayId, stash, task.taskId, isVisible = false, taskBounds = bounds)
            }
            mainExecutor.removeCallbacks(syncRunnable)
            mainExecutor.executeDelayed(syncRunnable, SYNC_DELAY_MS)
        }
    }

    /**
     * Hyprland's `movetoworkspace[silent] special:name`: [task] joins special workspace [name],
     * which shows (unless [silent]; then the window disappears into it). A window already on
     * [name] goes back to the active workspace instead, as in Hyprland.
     */
    private fun moveToSpecial(task: RunningTaskInfo, name: String, silent: Boolean) {
        val id = task.taskId
        val displayId = task.displayId
        val target = special.moveTarget(id, name)
        if (target == null) {
            leaveSpecial(task)
            return
        }
        val desk = repository.getActiveDeskId(displayId) ?: return
        val floating = tile.isFloating(id) || id in userFloated
        val wct = WindowContainerTransaction()
        maximized -= id
        applied.remove(id)
        tile.close(id)?.let { addBounds(it, displayId, wct) }
        special.add(id, target)
        saveSpecial()
        val key = special.keyOf(target)
        val focused = special.windows(target).lastOrNull { it != id && tile.workspaceOf(it) == key }
        tile.open(id, key, focused, float = floating)
        Log.d(TAG, "task $id to special:$target" + if (silent) " (silent)" else "")
        if (special.shownOn(displayId) == target) {
            wct.setAlwaysOnTop(task.token, true)
            wct.reorder(task.token, true)
            addBounds(key, displayId, wct, task)
            startChange(wct)
        } else if (silent) {
            if (!wct.isEmpty) startChange(wct)
            stashWindows(displayId, listOf(id))
        } else {
            if (!wct.isEmpty) startChange(wct)
            hideSpecial(displayId)
            showSpecial(displayId, desk, target)
        }
        updateDim()
        onStateChanged?.invoke()
    }

    /**
     * [task] leaves its special workspace for the workspace it is on (the active one when the
     * special workspace is shown): no longer on top, tiled in that workspace's layout.
     */
    private fun leaveSpecial(task: RunningTaskInfo) {
        val id = task.taskId
        val name = special.remove(id) ?: return
        saveSpecial()
        val displayId = task.displayId
        val floating = tile.isFloating(id) || id in userFloated
        val wct = WindowContainerTransaction()
        wct.setAlwaysOnTop(task.token, false)
        applied.remove(id)
        tile.close(id)
        addBounds(special.keyOf(name), displayId, wct)
        val desk = repository.getDeskIdForTask(id)
        if (desk != null && desk != stashDesk(displayId)) {
            val focused = repository.getExpandedTasksIdsInDeskOrdered(desk)
                .firstOrNull { it != id && tile.workspaceOf(it) == desk }
            tile.open(id, desk, focused, float = floating)
            addBounds(desk, displayId, wct, task)
        }
        Log.d(TAG, "task $id left special:$name")
        // Emptied, a shown special workspace goes away, as Hyprland removes it.
        if (special.windows(name).isEmpty() && special.shownOn(displayId) == name) special.hide(displayId)
        startChange(wct)
        updateDim()
        onStateChanged?.invoke()
    }

    /**
     * The layout a window of a request belongs to: its special workspace's, or, for a new
     * window while a special workspace is shown on its display, that one (Hyprland opens new
     * windows on the focused workspace); else the desk's.
     */
    private fun requestLayout(task: RunningTaskInfo, deskId: Int, wct: WindowContainerTransaction): Int {
        special.of(task.taskId)?.let { return special.keyOf(it) }
        val shown = special.shownOn(task.displayId) ?: return deskId
        if (tile.workspaceOf(task.taskId) != null || tile.isFloating(task.taskId)) return deskId
        special.add(task.taskId, shown)
        saveSpecial()
        wct.setAlwaysOnTop(task.token, true)
        Log.d(TAG, "new task ${task.taskId} opens on special:$shown")
        return special.keyOf(shown)
    }

    /**
     * The special-workspace part of [sync] for [displayId], whose active desk is [deskId]: the
     * shown special workspace's windows follow a desk switch Shell made, windows of hidden
     * special workspaces left in the desk go to the stash, the special layout re-tiles.
     */
    private fun syncSpecial(displayId: Int, deskId: Int) {
        val shown = special.shownOn(displayId)
        val stray = repository.getActiveTaskIdsInDesk(deskId)
            .filter { special.isMember(it) && !special.isShown(it, displayId) }
        if (stray.isNotEmpty()) stashWindows(displayId, stray)
        if (shown == null) return
        val key = special.keyOf(shown)
        val wct = WindowContainerTransaction()
        val moved = presentSpecial(displayId, deskId, wct)
        val expanded = repository.getExpandedTasksIdsInDeskOrdered(deskId)
        for (id in tile.tiled(key)) if (id !in expanded && moved.none { it.taskId == id }) tile.close(id)
        addBounds(key, displayId, wct)
        if (!wct.isEmpty) {
            Log.d(TAG, "sync special:$shown on desk $deskId: ${tile.tiled(key)}")
            startChange(wct)
        }
        recordCarried(displayId, deskId, moved)
    }

    /**
     * After [sync]: windows that closed leave their special workspace (an emptied one that was
     * shown hides, as Hyprland removes it), and the dim layers follow.
     */
    private fun syncSpecialAfter() {
        val gone = special.allWindows().filter { taskOrganizer.getRunningTaskInfo(it) == null }
        for (id in gone) {
            val name = special.remove(id) ?: continue
            tile.close(id)
            for (d in special.shownDisplays().toList()) {
                if (special.shownOn(d) == name && special.windows(name).isEmpty()) special.hide(d)
            }
        }
        if (gone.isNotEmpty()) saveSpecial()
        updateDim()
    }

    /**
     * Shell activated the stash desk (an app of a hidden special window came to front): the
     * last normal desk comes back with that special workspace shown over it.
     */
    private fun recoverFromStash(displayId: Int, stash: Int) {
        val repo = repository
        val front = repo.getExpandedTasksIdsInDeskOrdered(stash)
        // A window Shell put there that is no special window's: it goes along, as a normal one.
        val orphan = front.firstOrNull { !special.isMember(it) }?.let { taskOrganizer.getRunningTaskInfo(it) }
        val name = front.firstNotNullOfOrNull { special.of(it) } ?: special.occupied().firstOrNull()
        val numbers = workspaces(displayId)
        val target = lastNormalDesk[displayId]?.takeIf { it in repo.getDeskIds(displayId) && it != stash }
            ?: numbers.numbers.firstOrNull()?.let { numbers.deskOf(it) } ?: return
        Log.d(TAG, "stash desk $stash came to front: back to desk $target, special:$name shown")
        if (name != null) special.show(displayId, name)
        saveSpecial()
        switchToDesk(displayId, target, moving = orphan)
    }

    /**
     * The desk of [displayId] that holds the windows of hidden special workspaces: a desk of
     * its own with no workspace number, never active. Shell gives desks new ids when SystemUI
     * restarts; then the inactive desk holding only special windows is it.
     */
    private fun stashDesk(displayId: Int): Int? {
        if (!userRepositories.isPresent) return null
        val repo = repository
        val desks = repo.getDeskIds(displayId)
        stashDesks[displayId]?.let { if (it in desks) return it }
        if (special.allWindows().isEmpty()) return null
        val active = repo.getActiveDeskId(displayId)
        val found = desks.firstOrNull { d ->
            d != active && repo.getActiveTaskIdsInDesk(d).let { t -> t.isNotEmpty() && t.all { special.isMember(it) } }
        } ?: return null
        Log.i(TAG, "desk $found holds hidden special windows: the stash desk of display $displayId")
        stashDesks[displayId] = found
        workspaceMap(displayId).remove(found)
        workspacePrefs.edit().putString(workspaceKey(displayId), workspaceMap(displayId).encode()).apply()
        return found
    }

    /** Runs [block] with the stash desk of [displayId], making it first when there is none. */
    private fun withStashDesk(displayId: Int, block: (Int) -> Unit) {
        stashDesk(displayId)?.let { block(it); return }
        val controller = desktopTasksController.orElse(null) ?: return
        if (displayId in creatingDesks) {
            // A workspace's desk is being made: try again once it is there.
            mainExecutor.executeDelayed({ withStashDesk(displayId, block) }, SYNC_DELAY_MS)
            return
        }
        Log.d(TAG, "creating the stash desk of display $displayId")
        // Holds back numbering and removing desks until it is known (see [workspaces]).
        creatingDesks[displayId] = 0
        controller.createDesk(
            displayId = displayId,
            userId = repository.userId,
            enforceDeskLimit = false,
            activateDesk = false,
            enterReason = EnterReason.KEYBOARD_SHORTCUT_ENTER,
        ) { deskId ->
            mainExecutor.execute {
                creatingDesks.remove(displayId)
                if (deskId in repository.getDeskIds(displayId)) {
                    stashDesks[displayId] = deskId
                    workspaceMap(displayId).remove(deskId)
                    saveWorkspaces(displayId)
                    block(deskId)
                } else {
                    Log.w(TAG, "stash desk creation on display $displayId did not add a desk ($deskId)")
                }
            }
        }
    }

    /**
     * Hyprland's `decoration:dim_special`: while a special workspace is shown, the windows of
     * the workspace behind it get a black layer of that opacity on top (the wallpaper stays).
     */
    private fun updateDim() {
        val alpha = configOverrides.getString("dim_special")?.toFloatOrNull()?.coerceIn(0f, 1f)
            ?: DEFAULT_DIM_SPECIAL
        val want = HashMap<Int, Rect>()
        if (enabled && alpha > 0f) {
            for (displayId in special.shownDisplays()) {
                val desk = repository.getActiveDeskId(displayId) ?: continue
                for (id in repository.getExpandedTasksIdsInDeskOrdered(desk)) {
                    if (special.isMember(id) || tile.isPinned(id)) continue
                    val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
                    want[id] = task.configuration.windowConfiguration.bounds
                }
            }
        }
        val t = SurfaceControl.Transaction()
        for (id in dims.keys.filter { it !in want }) dims.remove(id)?.let { t.remove(it) }
        for ((id, b) in want) {
            val parent = decorations.surfaceOf(id) ?: continue
            val dim = dims.getOrPut(id) {
                SurfaceControl.Builder().setName("hypr-dim-special #$id").setColorLayer()
                    .setParent(parent).setCallsite("HyprTileController.updateDim").build()
            }
            t.setColor(dim, floatArrayOf(0f, 0f, 0f)).setAlpha(dim, alpha).setLayer(dim, Int.MAX_VALUE)
                .setCrop(dim, Rect(0, 0, b.width(), b.height())).show(dim)
        }
        t.apply()
    }

    private fun saveSpecial() {
        workspacePrefs.edit().putString(PREF_SPECIAL, special.encode()).apply()
    }

    /**
     * Hyprland's `resizeactive`: `DX DY` moves a tiled window's splits or grows a floating
     * one; `exact W H` (pixels or `N%` of the work area) sizes a floating window, its top-left
     * corner staying.
     */
    private fun resizeActive(xy: Triple<Boolean, Coord, Coord>) {
        val (exact, cx, cy) = xy
        val task = focusedTask() ?: return
        val id = task.taskId
        val wct = WindowContainerTransaction()
        if (tile.isTiled(id)) {
            // A tile's size comes from the layout.
            if (exact) return
            maximized -= id
            tile.resize(id, cx.value, cy.value)
            tile.workspaceOf(id)?.let { addBounds(it, task.displayId, wct) }
        } else {
            val box = floatingBounds(task)
            val b = if (exact) {
                val area = desktopBounds(task.displayId) ?: return
                Rect(box.left, box.top, box.left + maxOf(MIN_SIZE, cx.resolve(area.width())),
                    box.top + maxOf(MIN_SIZE, cy.resolve(area.height())))
            } else {
                Rect(box.left, box.top, maxOf(box.left + MIN_SIZE, box.right + cx.value),
                    maxOf(box.top + MIN_SIZE, box.bottom + cy.value))
            }
            setFloatingBounds(task, b, wct)
        }
        if (!wct.isEmpty) startChange(wct)
    }

    /**
     * Hyprland's `moveactive` for a floating window: `DX DY`, or `exact X Y` (display pixels,
     * or `N%` across the work area). Tiled windows move with `movewindow`.
     */
    private fun moveActive(xy: Triple<Boolean, Coord, Coord>) {
        val (exact, cx, cy) = xy
        val task = focusedTask() ?: return
        if (tile.isTiled(task.taskId)) return
        val box = floatingBounds(task)
        val area = desktopBounds(task.displayId) ?: return
        val x = if (exact) cx.resolve(area.width(), area.left) else box.left + cx.value
        val y = if (exact) cy.resolve(area.height(), area.top) else box.top + cy.value
        val wct = WindowContainerTransaction()
        setFloatingBounds(task, Rect(box).apply { offsetTo(x, y) }, wct)
        startChange(wct)
    }

    private fun toggleSplit() {
        val task = focusedTask() ?: return
        val deskId = tile.workspaceOf(task.taskId) ?: return
        val layout = tile.layout(deskId) as? DwindleLayout ?: return
        layout.toggleSplit(task.taskId)
        val wct = WindowContainerTransaction()
        addBounds(deskId, task.displayId, wct)
        if (!wct.isEmpty) startChange(wct)
    }

    /**
     * Hyprland's `fullscreen 1`: the focused tiled window takes the whole work area (no gaps)
     * and keeps its place in the layout; again, and it goes back to its tile.
     */
    private fun toggleMaximize() {
        val task = focusedTask() ?: return
        val id = task.taskId
        val deskId = tile.workspaceOf(id) ?: return
        val stable = desktopBounds(task.displayId) ?: return
        val wct = WindowContainerTransaction()
        if (maximized.remove(id)) {
            applied.remove(id)
            addBounds(deskId, task.displayId, wct)
        } else {
            maximized += id
            wct.setBounds(task.token, stable)
            wct.reorder(task.token, true)
        }
        startChange(wct)
    }

    /** Hyprland's `fullscreen 0`: Shell's real fullscreen, out of the desk and back. */
    private fun toggleFullscreen() {
        dispatchTarget?.let { target ->
            desktopTasksController.orElse(null)?.moveToFullscreen(
                target.taskId, DesktopModeTransitionSource.KEYBOARD_SHORTCUT)
            return
        }
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
        if (!wct.isEmpty) startChange(wct)
    }

    // ---- window history (HyprFocus, Phase E3) ----

    internal fun focusedWindow(): RunningTaskInfo? = focusedTask()

    internal fun focusedDisplay(): Int = focusedDisplayId()

    /** Brings [taskId] to front on its workspace (`focuswindow`). */
    internal fun focusTask(taskId: Int) = focusWindow(taskId)

    internal fun isTiled(taskId: Int) = tile.isTiled(taskId)

    /** An app's window: a standard task, not a desk's root nor Shell's desktop wallpaper. */
    internal fun isAppWindow(task: RunningTaskInfo): Boolean =
        task.activityType == ACTIVITY_TYPE_STANDARD &&
            (!userRepositories.isPresent || task.taskId !in repository.getAllDeskIds()) &&
            task.baseActivity?.className?.endsWith(".DesktopWallpaperActivity") != true

    internal fun isMinimized(taskId: Int) = repository.isMinimizedTask(taskId)

    internal fun displaysWithDesks(): Set<Int> {
        if (!userRepositories.isPresent) return emptySet()
        val repo = repository
        return repo.getAllDeskIds().mapNotNull { runCatching { repo.getDisplayForDesk(it) }.getOrNull() }.toSet()
    }

    /** The active workspace's number on [displayId], 0 when no desk is active (home). */
    internal fun activeWorkspace(displayId: Int): Int =
        if (repository.getActiveDeskId(displayId) == null) 0 else currentWorkspace(displayId)

    /** The tasks of [displayId]'s active workspace, minimized ones included. */
    internal fun activeWorkspaceTasks(displayId: Int): List<Int> {
        val desk = repository.getActiveDeskId(displayId) ?: return emptyList()
        return repository.getActiveTaskIdsInDesk(desk).filter { taskOrganizer.getRunningTaskInfo(it) != null }
    }

    /** Every task on a desk of any display. */
    internal fun desktopTasks(): List<RunningTaskInfo> = taskOrganizer.getRunningTasks().filter {
        it.activityType == ACTIVITY_TYPE_STANDARD && repository.getDeskIdForTask(it.taskId) != null
    }

    /** Swaps tiled [task] with tiled [other] in their layout (`swapnext`, `swapwindow`). */
    internal fun swapTiled(task: RunningTaskInfo, other: Int) {
        val deskId = tile.workspaceOf(task.taskId) ?: return
        if (tile.workspaceOf(other) != deskId) return
        tile.swap(task.taskId, other)
        maximized -= task.taskId
        maximized -= other
        val wct = WindowContainerTransaction()
        addBounds(deskId, task.displayId, wct)
        if (!wct.isEmpty) startChange(wct)
    }

    /** `binds.workspace_back_and_forth`: 0 off, 1 on, 2 on this display only. */
    internal fun workspaceBackAndForth(): Int = (configOverrides.getString("workspace_back_and_forth")
        ?: SystemProperties.get("persist.hypr.workspace_back_and_forth")).trim().let {
        when (it) {
            "true" -> 1
            "false", "" -> 0
            else -> it.toIntOrNull()?.coerceIn(0, 2) ?: 0
        }
    }

    internal fun allowWorkspaceCycles(): Boolean = (configOverrides.getString("allow_workspace_cycles")
        ?: SystemProperties.get("persist.hypr.allow_workspace_cycles")).trim() == "true"

    // ---- touch (HyprTouch) ----

    /** The topmost desktop window of the active desk under ([x], [y]), if any. */
    fun windowAt(displayId: Int, x: Float, y: Float): RunningTaskInfo? {
        if (!enabled) return null
        val deskId = repository.getActiveDeskId(displayId) ?: return null
        return repository.getExpandedTasksIdsInDeskOrdered(deskId).asSequence()
            .mapNotNull { taskOrganizer.getRunningTaskInfo(it) }
            .firstOrNull { it.isFreeform && it.bounds().let { b -> x >= b.left && x < b.right && y >= b.top && y < b.bottom } }
    }

    /**
     * The window whose frame ([border] pixels inside its edges, the gap next to them, and
     * [caption] pixels at its top) is under ([x], [y]): where a touch long-press opens the
     * window menu. The topmost window under the point wins; a point in the middle of a window
     * belongs to its app.
     */
    fun frameAt(displayId: Int, x: Float, y: Float, border: Int, caption: Int): RunningTaskInfo? {
        if (!enabled) return null
        val deskId = repository.getActiveDeskId(displayId) ?: return null
        val tasks = repository.getExpandedTasksIdsInDeskOrdered(deskId)
            .mapNotNull { taskOrganizer.getRunningTaskInfo(it) }.filter { it.isFreeform }
        val under = tasks.firstOrNull { it.bounds().let { b -> x >= b.left && x < b.right && y >= b.top && y < b.bottom } }
        if (under != null) {
            val b = under.bounds()
            // Tiled windows have no caption: their top belongs to the app (toolbars).
            val top = if (tile.isTiled(under.taskId)) border else maxOf(border, caption)
            val inFrame = x < b.left + border || x >= b.right - border || y >= b.bottom - border ||
                y < b.top + top
            return under.takeIf { inFrame }
        }
        // In a gap: the nearest window within reach.
        return tasks.filter {
            val b = it.bounds()
            x >= b.left - border && x < b.right + border && y >= b.top - border && y < b.bottom + border
        }.minByOrNull {
            val b = it.bounds()
            maxOf(b.left - x, x - b.right, 0f) + maxOf(b.top - y, y - b.bottom, 0f)
        }
    }

    /** What the window menu shows: the workspaces of a window's display, its own, its state. */
    data class MenuState(val workspaces: Int, val current: Int, val tiled: Boolean, val pinned: Boolean)

    fun menuState(task: RunningTaskInfo): MenuState {
        val numbers = workspaces(task.displayId)
        val ws = repository.getDeskIdForTask(task.taskId)?.let { numbers.numberOf(it) } ?: 0
        return MenuState(numbers.numbers.maxOrNull() ?: 0, ws, tile.isTiled(task.taskId), tile.isPinned(task.taskId))
    }

    /** A window dragged by touch was dropped: as [mouseUp], plus snapping at the screen edges. */
    fun touchDrop(x: Float, y: Float, edge: Int) {
        val d = mouseDrag
        if (d != null && !d.resize && !tile.isTiled(d.task.taskId)) {
            val width = displayController.getDisplayLayout(d.task.displayId)?.width() ?: 0
            if (width > 0 && (x < edge || x >= width - edge)) {
                // A floating window dropped on the left or right edge joins the layout there.
                mouseDrag = null
                startOverride[d.task.taskId] = d.current
                onSnap(d.task, left = x < edge)
                return
            }
        }
        mouseUp(x, y, cancelled = false)
    }

    // ---- mouse binds (HyprMouse) ----

    /**
     * MOD + a mouse button went down at ([x], [y]) on [displayId]: [button] is
     * MotionEvent.BUTTON_PRIMARY (move), BUTTON_SECONDARY (resize) or BUTTON_TERTIARY (toggle
     * floating). Returns true when it hit a desktop window of the active desk, which then gets
     * the gesture.
     */
    fun mouseDown(displayId: Int, x: Float, y: Float, button: Int): Boolean {
        if (!enabled) return false
        val deskId = repository.getActiveDeskId(displayId) ?: return false
        val task = repository.getExpandedTasksIdsInDeskOrdered(deskId).asSequence()
            .mapNotNull { taskOrganizer.getRunningTaskInfo(it) }
            .firstOrNull { it.isFreeform && it.bounds().let { b -> x >= b.left && x < b.right && y >= b.top && y < b.bottom } }
            ?: return false
        if (button == MotionEvent.BUTTON_TERTIARY) {
            toggleFloating(task)
            return true
        }
        startDrag(task, x, y, resize = button == MotionEvent.BUTTON_SECONDARY)
        return true
    }

    /** A touch long-press on [task]'s frame: it follows the finger like a MOD + left drag. */
    fun touchHold(task: RunningTaskInfo, x: Float, y: Float) {
        if (enabled) startDrag(task, x, y, resize = false)
    }

    private fun startDrag(task: RunningTaskInfo, x: Float, y: Float, resize: Boolean) {
        val b = task.configuration.windowConfiguration.bounds
        mouseDrag = MouseDrag(
            task = task,
            resize = resize,
            x0 = x,
            y0 = y,
            bounds = Rect(b),
            right = x >= b.exactCenterX(),
            bottom = y >= b.exactCenterY(),
            leash = decorations.surfaceOf(task.taskId),
        )
        Log.d(TAG, "drag ${if (resize) "resize" else "move"} task ${task.taskId} from $b")
    }

    fun mouseMove(x: Float, y: Float) {
        val d = mouseDrag ?: return
        val dx = (x - d.x0).toInt()
        val dy = (y - d.y0).toInt()
        if (!d.resize) {
            // Moving follows the pointer on the surface; bounds change when the button is up.
            val leash = d.leash ?: return
            val p = d.task.positionInParent
            SurfaceControl.Transaction().setPosition(leash, (p.x + dx).toFloat(), (p.y + dy).toFloat())
                .apply()
            d.current = Rect(d.bounds).apply { offset(dx, dy) }
            return
        }
        // Resizing lands when the button is up, in one animated transition: bounds changed
        // outside a transition reach WindowManager but not the task surfaces (Shell sets their
        // position and crop in transitions only).
        val id = d.task.taskId
        if (tile.isTiled(id)) {
            val gx = (if (d.right) dx else -dx) - (if (d.right) d.lastDx else -d.lastDx)
            val gy = (if (d.bottom) dy else -dy) - (if (d.bottom) d.lastDy else -d.lastDy)
            if (gx == 0 && gy == 0) return
            tile.resize(id, gx, gy)
        } else {
            d.current = resized(d, dx, dy)
        }
        d.lastDx = dx
        d.lastDy = dy
    }

    /** The button is up (or the gesture was cancelled): the window lands. */
    fun mouseUp(x: Float, y: Float, cancelled: Boolean) {
        val d = mouseDrag ?: return
        mouseDrag = null
        val id = d.task.taskId
        val deskId = repository.getDeskIdForTask(id) ?: return
        if (d.resize) {
            val wct = WindowContainerTransaction()
            if (tile.isTiled(id)) {
                maximized -= id
                tile.workspaceOf(id)?.let { addBounds(it, d.task.displayId, wct) }
            } else if (d.current != d.bounds) {
                wct.setBounds(d.task.token, d.current)
            }
            if (!wct.isEmpty) startChange(wct)
            return
        }
        val leash = d.leash
        val dragged = d.current
        val wct = WindowContainerTransaction()
        if (tile.isTiled(id)) {
            // Dropped on another tiled window: they swap places. Else it goes back to its tile.
            val target = if (cancelled) null else repository.getExpandedTasksIdsInDeskOrdered(deskId)
                .firstOrNull { other ->
                    other != id && tile.isTiled(other) && tile.workspaceOf(other) == deskId &&
                        taskOrganizer.getRunningTaskInfo(other)?.bounds()
                            ?.let { x >= it.left && x < it.right && y >= it.top && y < it.bottom } == true
                }
            if (target != null) {
                tile.swap(id, target)
                maximized -= id
                applied.remove(id)
                startOverride[id] = dragged
                addBounds(deskId, d.task.displayId, wct)
            } else if (leash != null) {
                val p = d.task.positionInParent
                val home = Rect(d.bounds).apply { offsetTo(p.x, p.y) }
                val now = Rect(dragged).apply { offset(p.x - d.bounds.left, p.y - d.bounds.top) }
                animator.animateLeash(leash, now, home)
            }
        } else if (dragged != d.bounds) {
            startOverride[id] = dragged
            wct.setBounds(d.task.token, dragged)
        }
        if (!wct.isEmpty) startChange(wct)
    }

    /** Floating resize: the corner nearest to the press follows the pointer. */
    private fun resized(d: MouseDrag, dx: Int, dy: Int): Rect {
        val b = d.bounds
        val minW = d.task.topActivityInfo?.windowLayout?.minWidth?.takeIf { it > 0 } ?: MIN_SIZE
        val minH = d.task.topActivityInfo?.windowLayout?.minHeight?.takeIf { it > 0 } ?: MIN_SIZE
        val r = Rect(b)
        if (d.right) r.right = maxOf(b.left + minW, b.right + dx) else r.left = minOf(b.right - minW, b.left + dx)
        if (d.bottom) r.bottom = maxOf(b.top + minH, b.bottom + dy) else r.top = minOf(b.bottom - minH, b.top + dy)
        return r
    }

    private fun workArea(displayId: Int): Box? {
        val stable = desktopBounds(displayId) ?: return null
        return Box(stable.left, stable.top, stable.right, stable.bottom)
    }

    /**
     * Shell's stable bounds for desktop windows without the launcher's bar: the bar provides
     * its own insets, which Shell's [com.android.wm.shell.common.DisplayLayout] does not count
     * (it only takes the navigation bar and the status bar's resource height).
     */
    private fun desktopBounds(displayId: Int): Rect? {
        val layout = displayController.getDisplayLayout(displayId) ?: return null
        val stable = Rect().also { layout.getStableBoundsForDesktopMode(it) }
        barReserved[displayId]?.let { (top, bottom) ->
            stable.top = maxOf(stable.top, top)
            stable.bottom = minOf(stable.bottom, layout.height() - bottom)
        }
        return stable
    }

    /**
     * The launcher's bar keeps [top] and [bottom] pixels of [displayId] (Hyprland's
     * `reserved`, set by the bar itself as waybar's exclusive zone is). Re-tiles at once.
     */
    fun setBarReserved(displayId: Int, top: Int, bottom: Int) {
        val value = top.coerceAtLeast(0) to bottom.coerceAtLeast(0)
        if (barReserved[displayId] == value) return
        Log.i(TAG, "bar reserves top=$top bottom=$bottom on display $displayId")
        barReserved[displayId] = value
        applied.clear()
        mainExecutor.removeCallbacks(syncRunnable)
        mainExecutor.execute(syncRunnable)
    }

    /**
     * The workspaces and windows of [displayId] for the launcher's bar and overview (Phase D4):
     * `workspaces` (count), `active` (1-based, 0 = none: home), `focusedTask`, and per window
     * the parallel arrays `taskIds`, `taskWorkspaces`, `taskPackages`, `taskTitles`,
     * `taskBounds` (left, top, right, bottom each), `taskFloating`, `taskMinimized`,
     * `taskPinned`, ordered front to back within each workspace; `display` (width, height) and
     * `workArea` (left, top, right, bottom: without the bar and system bars).
     */
    fun state(displayId: Int): Bundle {
        val out = Bundle()
        if (!userRepositories.isPresent) return out
        val repo = repository
        // Desks in workspace order (Phase E2); `workspaceIds` are the workspaces that exist,
        // `workspaces` how many there may be.
        val numbers = workspaces(displayId)
        val desks = numbers.numbers.mapNotNull { numbers.deskOf(it) }
        val active = repo.getActiveDeskId(displayId)
        out.putInt("workspaces", workspaceLimit)
        out.putIntArray("workspaceIds", numbers.numbers.toIntArray())
        out.putInt("active", active?.let { numbers.numberOf(it) } ?: 0)
        val focused = focusedTask()
        out.putInt("focusedTask", focused?.taskId ?: -1)
        out.putString("focusedPackage", focused?.let { packageOf(it) })
        out.putString("focusedTitle", focused?.let { titleOf(it) })
        displayController.getDisplayLayout(displayId)?.let {
            out.putIntArray("display", intArrayOf(it.width(), it.height()))
        }
        desktopBounds(displayId)?.let {
            out.putIntArray("workArea", intArrayOf(it.left, it.top, it.right, it.bottom))
        }
        val ids = ArrayList<Int>()
        val ws = ArrayList<Int>()
        val pkgs = ArrayList<String>()
        val titles = ArrayList<String>()
        val bounds = ArrayList<Int>()
        val floating = ArrayList<Boolean>()
        val minimized = ArrayList<Boolean>()
        val pinned = ArrayList<Boolean>()
        // The stash desk's windows are the hidden special workspaces' (E5).
        for (deskId in desks + listOfNotNull(stashDesk(displayId))) {
            val visible = repo.getExpandedTasksIdsInDeskOrdered(deskId)
            val all = visible + repo.getActiveTaskIdsInDesk(deskId).filter { it !in visible }
            for (id in all) {
                val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
                if (task.activityType != ACTIVITY_TYPE_STANDARD) continue
                ids += id
                ws += special.of(id)?.let { special.keyOf(it) } ?: numbers.numberOf(deskId) ?: 0
                pkgs += packageOf(task) ?: ""
                titles += titleOf(task) ?: ""
                val b = task.configuration.windowConfiguration.bounds
                bounds += listOf(b.left, b.top, b.right, b.bottom)
                floating += !tile.isTiled(id)
                minimized += repo.isMinimizedTask(id)
                pinned += tile.isPinned(id)
            }
        }
        out.putIntArray("taskIds", ids.toIntArray())
        out.putIntArray("taskWorkspaces", ws.toIntArray())
        out.putStringArray("taskPackages", pkgs.toTypedArray())
        out.putStringArray("taskTitles", titles.toTypedArray())
        out.putIntArray("taskBounds", bounds.toIntArray())
        out.putBooleanArray("taskFloating", floating.toBooleanArray())
        out.putBooleanArray("taskMinimized", minimized.toBooleanArray())
        // E3: the focus history (most recent first) and the urgent windows.
        out.putIntArray("focusHistory", focus.history(displayId).toIntArray())
        out.putIntArray("urgentTasks", focus.urgent.toIntArray())
        out.putInt("lastWorkspace", focus.previousWorkspace(displayId))
        out.putBooleanArray("taskPinned", pinned.toBooleanArray())
        // E5: special workspaces by key (windows on one have its key as workspace), the shown one.
        val specials = special.occupied().map { special.keyOf(it) to it } +
            listOfNotNull(special.shownOn(displayId)).filter { it !in special.occupied() }.map { special.keyOf(it) to it }
        out.putIntArray("specialKeys", specials.map { it.first }.toIntArray())
        out.putStringArray("specialNames", specials.map { it.second }.toTypedArray())
        out.putString("specialShown", special.shownOn(displayId))
        return out
    }

    private fun packageOf(task: RunningTaskInfo): String? =
        (task.baseActivity ?: task.topActivity)?.packageName

    private fun titleOf(task: RunningTaskInfo): String? =
        task.taskDescription?.label?.takeIf { it.isNotEmpty() }

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
        val stable = desktopBounds(displayId) ?: return
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
                title = task.taskDescription?.label?.toString(),
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

    /**
     * Reads the configuration: the launcher's (Lua, IHyprShell.setConfig) over the
     * persist.hypr.* properties. Returns true when the tiling changed.
     */
    private fun updateConfig(): Boolean {
        val o = configOverrides
        decorations.setConfig(HyprStyleProps.decoration(o))
        animator.config = HyprStyleProps.animations(o)
        animationsEnabled = HyprStyleProps.animationsEnabled(o)
        val fresh = readConfig(o)
        workspaceRuleList = o.getStringArray("workspace")?.mapNotNull {
            runCatching { WorkspaceRule.decode(it) }.onFailure { e -> Log.w(TAG, "bad workspace rule: $e") }
                .getOrNull()
        } ?: emptyList()
        val rulesChanged = mapWorkspaceRules()
        if (fresh == tile.config && !rulesChanged) return false
        tile.config = fresh
        applied.clear()
        return true
    }

    private companion object {
        const val TAG = "HyprTile"
        const val PROP_ENABLED = "persist.hypr.tiling"
        const val SYNC_DELAY_MS = 100L
        /** How long after a sync empty desks are removed (Hyprland removes them at once). */
        const val REMOVE_DESKS_DELAY_MS = 400L
        const val PREFS_WORKSPACES = "hypr_workspaces"
        /** Special workspaces' windows and keys in [PREFS_WORKSPACES] (E5). */
        const val PREF_SPECIAL = "special"
        /** After a special workspace showed, when its window takes the focus. */
        const val FOCUS_SPECIAL_DELAY_MS = 300L
        /** Smallest floating window a mouse resize makes, unless the app asks for more. */
        const val MIN_SIZE = 200

        /**
         * The tiling configuration: [overrides] (string values under the property names without
         * `persist.hypr.`, from IHyprShell.setConfig) first, then the persist.hypr.* properties.
         */
        fun readConfig(overrides: Bundle): TileConfig {
            val d = TileConfig()
            fun str(name: String) = overrides.getString(name) ?: SystemProperties.get("persist.hypr.$name")
            fun int(name: String, def: Int) = str(name).trim().toIntOrNull() ?: def
            fun float(name: String, def: Float, range: ClosedFloatingPointRange<Float>) =
                str(name).trim().toFloatOrNull()?.takeIf { it in range } ?: def
            fun bool(name: String, def: Boolean) = str(name).trim().toBooleanStrictOrNull() ?: def
            return d.copy(
                splitWidthMultiplier = float("split_width_multiplier", d.splitWidthMultiplier, 0.1f..10f),
                defaultSplitRatio = float("default_split_ratio", d.defaultSplitRatio, 0.1f..1.9f),
                preserveSplit = bool("preserve_split", d.preserveSplit),
                forceSplit = int("force_split", d.forceSplit).takeIf { it in 1..2 } ?: d.forceSplit,
                masterFactor = float("mfact", d.masterFactor, 0.05f..0.95f),
                specialScaleFactor = float("special_scale_factor", d.specialScaleFactor, 0.1f..1f),
                newIsMaster = str("new_status").trim() == "master",
                newOnTop = bool("new_on_top", d.newOnTop),
                windowRules = overrides.getStringArray("windowrule")?.mapNotNull {
                    runCatching { WindowRule.decode(it) }.onFailure { e -> Log.w(TAG, "bad window rule: $e") }
                        .getOrNull()
                } ?: emptyList(),
                layout = if (str("layout") == "master") {
                    LayoutKind.MASTER
                } else {
                    LayoutKind.DWINDLE
                },
                gapsIn = int("gaps_in", d.gapsIn).coerceAtLeast(0),
                gapsOut = int("gaps_out", d.gapsOut).coerceAtLeast(0),
                reserved = Insets(top = int("reserved_top", 0), bottom = int("reserved_bottom", 0)),
                floatPackages = str("float_packages")
                    .split(',').map { it.trim() }.filter { it.isNotEmpty() }.toSet(),
            )
        }
    }
}
