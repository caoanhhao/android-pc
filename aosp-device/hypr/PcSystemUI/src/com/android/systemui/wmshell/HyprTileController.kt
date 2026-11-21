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
import com.android.hyprtile.LayoutKind
import com.android.hyprtile.TileConfig
import com.android.hyprtile.WindowRule
import com.android.hyprtile.WindowTraits
import com.android.hyprtile.WorkspaceRule
import com.android.hyprtile.effect
import com.android.hyprtile.neighbor
import com.android.hyprtile.resolveWorkspace
import com.android.hyprtile.shouldFloat
import com.android.wm.shell.ShellTaskOrganizer
import com.android.wm.shell.common.DisplayController
import com.android.wm.shell.common.ShellExecutor
import com.android.wm.shell.desktopmode.DesktopModeEventLogger.Companion.EnterReason
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
        if (!userRepositories.isPresent) {
            Log.w(TAG, "desktop mode is off, no tiling")
            return
        }
        transitions.addHandler(this)
        transitions.registerObserver(this)
        decorations.install()
        mouse.start()
        touch.start()
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
                newTasks += task.taskId
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
        val fromTasks = info.changes.filter { it.taskInfo?.parentTaskId == desk.from }
        val toTasks = info.changes.filter { it.taskInfo?.parentTaskId == desk.to }
        val fromPos = repo.getDeskPosition(desk.from) ?: return false
        val toPos = repo.getDeskPosition(desk.to) ?: return false
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
                // A task Shell put on another desk than the request said (it relaunched into
                // the desk it last lived on) is tiled again here.
                if (tile.workspaceOf(id) == deskId || id in userFloated) continue
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
        applyWindowRules()
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
            val rule = rules.lastOrNull { it.workspace != null && it.matches(pkg, title) } ?: continue
            val n = rule.workspace ?: continue
            if (n > MAX_WORKSPACES || currentWorkspaceOf(task) == n) continue
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
        return orderedDesks(task.displayId).indexOf(desk) + 1
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
            "movetoworkspacesilent" -> {
                val task = focusedTask() ?: return true
                val n = resolveWorkspace(arg, currentWorkspace(task.displayId))
                    ?.takeIf { it <= MAX_WORKSPACES } ?: return false
                withWorkspaceDesk(task.displayId, n) { moveSilently(task, it) }
            }
            "resizeactive" -> {
                val p = arg.trim().split(Regex("\\s+")).map { it.toIntOrNull() ?: return false }
                if (p.size != 2) return false
                resizeActive(p[0], p[1])
            }
            "movefocus" -> moveFocus(Direction.parse(arg) ?: return false)
            "movewindow" -> moveWindow(Direction.parse(arg) ?: return false)
            "togglefloating" -> toggleFloating()
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
            "focuswindow" -> focusWindow(arg.trim().removePrefix("taskid:").toIntOrNull()
                ?: return false)
            // Run by HyprLauncher (rofi-like launcher, hyprexpo-like overview, Lua functions).
            "launcher", "overview", "hyprexpo:expo", "lua" ->
                return launcherDispatch?.invoke(name, arg) ?: false
            else -> return false
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
        val transition = if (animationsEnabled) {
            transitions.startTransition(TRANSIT_DESKTOP_MODE_DESK_TO_DESK_SWITCH, wct, this).also {
                deskSwitches[it] = DeskSwitch(displayId, from, to)
            }
        } else {
            deskSwitchTransitionHandler.startTransition(wct, repo.userId, displayId, from, to)
        }
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
        if (!wct.isEmpty) startChange(wct)
    }

    private fun toggleFloating(task: RunningTaskInfo? = focusedTask()) {
        task ?: return
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
        if (!wct.isEmpty) startChange(wct)
    }

    /** Hyprland's `resizeactive DX DY`: a tiled window moves its splits, a floating one grows. */
    private fun resizeActive(dx: Int, dy: Int) {
        val task = focusedTask() ?: return
        val id = task.taskId
        val wct = WindowContainerTransaction()
        if (tile.isTiled(id)) {
            maximized -= id
            tile.resize(id, dx, dy)
            tile.workspaceOf(id)?.let { addBounds(it, task.displayId, wct) }
        } else {
            val box = task.bounds()
            val b = Rect(box.left, box.top, maxOf(box.left + MIN_SIZE, box.right + dx),
                maxOf(box.top + MIN_SIZE, box.bottom + dy))
            wct.setBounds(task.token, b)
        }
        if (!wct.isEmpty) startChange(wct)
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

    /** What the window menu needs: workspaces of the display and the window's own. */
    fun menuState(task: RunningTaskInfo): Triple<Int, Int, Boolean> {
        val desks = orderedDesks(task.displayId)
        val ws = repository.getDeskIdForTask(task.taskId)?.let { desks.indexOf(it) + 1 } ?: 0
        return Triple(desks.size, ws, tile.isTiled(task.taskId))
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
     * `taskBounds` (left, top, right, bottom each), `taskFloating`, `taskMinimized`, ordered
     * front to back within each workspace.
     */
    fun state(displayId: Int): Bundle {
        val out = Bundle()
        if (!userRepositories.isPresent) return out
        val repo = repository
        val desks = orderedDesks(displayId)
        val active = repo.getActiveDeskId(displayId)
        out.putInt("workspaces", desks.size)
        out.putInt("active", active?.let { desks.indexOf(it) + 1 } ?: 0)
        val focused = focusedTask()
        out.putInt("focusedTask", focused?.taskId ?: -1)
        out.putString("focusedPackage", focused?.let { packageOf(it) })
        out.putString("focusedTitle", focused?.let { titleOf(it) })
        val ids = ArrayList<Int>()
        val ws = ArrayList<Int>()
        val pkgs = ArrayList<String>()
        val titles = ArrayList<String>()
        val bounds = ArrayList<Int>()
        val floating = ArrayList<Boolean>()
        val minimized = ArrayList<Boolean>()
        for ((i, deskId) in desks.withIndex()) {
            val visible = repo.getExpandedTasksIdsInDeskOrdered(deskId)
            val all = visible + repo.getActiveTaskIdsInDesk(deskId).filter { it !in visible }
            for (id in all) {
                val task = taskOrganizer.getRunningTaskInfo(id) ?: continue
                if (task.activityType != ACTIVITY_TYPE_STANDARD) continue
                ids += id
                ws += i + 1
                pkgs += packageOf(task) ?: ""
                titles += titleOf(task) ?: ""
                val b = task.configuration.windowConfiguration.bounds
                bounds += listOf(b.left, b.top, b.right, b.bottom)
                floating += !tile.isTiled(id)
                minimized += repo.isMinimizedTask(id)
            }
        }
        out.putIntArray("taskIds", ids.toIntArray())
        out.putIntArray("taskWorkspaces", ws.toIntArray())
        out.putStringArray("taskPackages", pkgs.toTypedArray())
        out.putStringArray("taskTitles", titles.toTypedArray())
        out.putIntArray("taskBounds", bounds.toIntArray())
        out.putBooleanArray("taskFloating", floating.toBooleanArray())
        out.putBooleanArray("taskMinimized", minimized.toBooleanArray())
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
        // Workspace rules are by number; the layouts are by desk.
        val byDesk = HashMap<Int, WorkspaceRule>()
        val rules = o.getStringArray("workspace")?.mapNotNull {
            runCatching { WorkspaceRule.decode(it) }.onFailure { e -> Log.w(TAG, "bad workspace rule: $e") }
                .getOrNull()
        } ?: emptyList()
        if (rules.isNotEmpty() && userRepositories.isPresent) {
            val repo = repository
            for (displayId in repo.getAllDeskIds().mapNotNull {
                runCatching { repo.getDisplayForDesk(it) }.getOrNull()
            }.toSet()) {
                val desks = orderedDesks(displayId)
                for (r in rules) desks.getOrNull(r.workspace - 1)?.let { byDesk[it] = r }
            }
        }
        val rulesChanged = byDesk != tile.workspaceRules
        if (fresh == tile.config && !rulesChanged) return false
        tile.config = fresh
        tile.workspaceRules = byDesk
        applied.clear()
        return true
    }

    private companion object {
        const val TAG = "HyprTile"
        const val PROP_ENABLED = "persist.hypr.tiling"
        const val SYNC_DELAY_MS = 100L
        const val MAX_WORKSPACES = 9
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
