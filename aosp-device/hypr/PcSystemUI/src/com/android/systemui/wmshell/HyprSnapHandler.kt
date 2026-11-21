package com.android.systemui.wmshell

import android.app.ActivityManager.RunningTaskInfo
import android.graphics.Rect
import com.android.wm.shell.desktopmode.DesktopTasksController.SnapPosition
import com.android.wm.shell.desktopmode.SnapController
import com.android.wm.shell.sysui.ShellInit
import com.android.wm.shell.windowdecor.DesktopModeWindowDecorViewModel
import com.android.wm.shell.windowdecor.tiling.SnapEventHandler
import java.util.Optional

/**
 * Takes Shell's snap events (the caption menu's snap buttons, dragging a window to the left or
 * right screen edge, the snap key gestures) away from Shell's two-window tiling
 * (`windowdecor/tiling`) while hyprtile is on (Phase D2).
 *
 * Every snap goes through [SnapController], which forwards to the one [SnapEventHandler] passed
 * to its public `start()`; [DesktopModeWindowDecorViewModel] passes itself in its constructor.
 * Calling `start()` again at ShellInit, after Shell built its objects, puts this handler in
 * front: snaps go to [HyprTileController.onSnap], everything else (and every call while tiling
 * is off) to Shell's handler, which then never has a tiled pair to manage.
 */
class HyprSnapHandler(
    shellInit: ShellInit,
    private val snapController: SnapController,
    private val shellHandler: Optional<DesktopModeWindowDecorViewModel>,
    private val controller: HyprTileController,
) : SnapEventHandler {

    init {
        shellInit.addInitCallback(::onInit, this)
    }

    private fun onInit() {
        if (shellHandler.isPresent) snapController.start(this)
    }

    private val shell: SnapEventHandler get() = shellHandler.get()

    override fun snapToHalfScreen(
        taskInfo: RunningTaskInfo,
        currentDragBounds: Rect,
        position: SnapPosition,
    ): Boolean {
        if (!controller.enabled) return shell.snapToHalfScreen(taskInfo, currentDragBounds, position)
        controller.onSnap(taskInfo, left = position == SnapPosition.LEFT)
        return false
    }

    override fun snapPersistedTaskToHalfScreen(
        taskInfo: RunningTaskInfo,
        currentDragBounds: Rect,
        position: SnapPosition,
    ): Boolean {
        // Shell restores a pair it tiled before; hyprtile has its own layout instead.
        if (!controller.enabled) {
            return shell.snapPersistedTaskToHalfScreen(taskInfo, currentDragBounds, position)
        }
        return false
    }

    override fun removeTaskIfTiled(displayId: Int, taskId: Int) =
        shell.removeTaskIfTiled(displayId, taskId)

    override fun onUserChange(userId: Int) = shell.onUserChange(userId)

    override fun onRecentsAnimationEndedToSameDesk() = shell.onRecentsAnimationEndedToSameDesk()

    override fun moveTaskToFrontIfTiled(taskInfo: RunningTaskInfo): Boolean =
        shell.moveTaskToFrontIfTiled(taskInfo)

    override fun getLeftSnapBoundsIfTiled(displayId: Int): Rect =
        shell.getLeftSnapBoundsIfTiled(displayId)

    override fun getRightSnapBoundsIfTiled(displayId: Int): Rect =
        shell.getRightSnapBoundsIfTiled(displayId)

    override fun onDeskDeactivated(deskId: Int) = shell.onDeskDeactivated(deskId)

    override fun onDisplayDisconnected(disconnectedDisplayId: Int) =
        shell.onDisplayDisconnected(disconnectedDisplayId)

    override fun onDeskActivated(deskId: Int, displayId: Int) =
        shell.onDeskActivated(deskId, displayId)

    override fun onDeskRemoved(deskId: Int) = shell.onDeskRemoved(deskId)

    override fun notifyTilingOfExplodedViewReorder(deskId: Int, topTaskId: Int) =
        shell.notifyTilingOfExplodedViewReorder(deskId, topTaskId)

    override fun getDividerBounds(deskId: Int): Rect = shell.getDividerBounds(deskId)

    override fun onDisplayLayoutChange(
        displayId: Int,
        config: android.content.res.Configuration?,
        oldStableBounds: Rect,
        newToOldDpiRatio: Double,
    ) = shell.onDisplayLayoutChange(displayId, config, oldStableBounds, newToOldDpiRatio)

    override fun onTaskLaunchStarted() = shell.onTaskLaunchStarted()

    override fun onDeskSwitchAnimationStarting(displayId: Int, fromDeskId: Int, toDeskId: Int) =
        shell.onDeskSwitchAnimationStarting(displayId, fromDeskId, toDeskId)

    override fun onDeskSwitchAnimationEnded(displayId: Int, deskId: Int) =
        shell.onDeskSwitchAnimationEnded(displayId, deskId)
}
