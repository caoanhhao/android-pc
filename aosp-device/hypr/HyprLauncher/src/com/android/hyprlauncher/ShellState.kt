package com.android.hyprlauncher

import android.graphics.Rect
import android.os.Bundle

/** A window as the shell reports it (IHyprShell.getState). */
data class Win(
    val taskId: Int,
    /** 1-based workspace (desk) number. */
    val workspace: Int,
    val packageName: String,
    val title: String,
    val bounds: Rect,
    val floating: Boolean,
    val minimized: Boolean,
    val pinned: Boolean = false,
)

/**
 * The workspaces and windows of the default display, from the Bundle of
 * HyprTileController.state() in PcSystemUI. Windows are ordered by workspace, front to back.
 */
data class ShellState(
    val workspaces: Int,
    /** The active workspace, 0 when none is (home in front). */
    val active: Int,
    val focusedTask: Int,
    val focusedPackage: String?,
    val focusedTitle: String?,
    val windows: List<Win>,
    /** The display's size, 0 x 0 until the shell answers. */
    val displayWidth: Int = 0,
    val displayHeight: Int = 0,
    /** The work area: the display without the bar and the system bars. */
    val workArea: Rect = Rect(),
) {
    fun on(workspace: Int) = windows.filter { it.workspace == workspace }

    companion object {
        val EMPTY = ShellState(0, 0, -1, null, null, emptyList())

        fun from(b: Bundle): ShellState {
            val ids = b.getIntArray("taskIds") ?: IntArray(0)
            val ws = b.getIntArray("taskWorkspaces") ?: IntArray(0)
            val pkgs = b.getStringArray("taskPackages") ?: emptyArray()
            val titles = b.getStringArray("taskTitles") ?: emptyArray()
            val bounds = b.getIntArray("taskBounds") ?: IntArray(0)
            val floating = b.getBooleanArray("taskFloating") ?: BooleanArray(0)
            val minimized = b.getBooleanArray("taskMinimized") ?: BooleanArray(0)
            val pinned = b.getBooleanArray("taskPinned") ?: BooleanArray(0)
            val display = b.getIntArray("display") ?: IntArray(0)
            val work = b.getIntArray("workArea") ?: IntArray(0)
            val windows = ids.indices.map { i ->
                Win(
                    taskId = ids[i],
                    workspace = ws.getOrElse(i) { 0 },
                    packageName = pkgs.getOrElse(i) { "" },
                    title = titles.getOrElse(i) { "" },
                    bounds = Rect(
                        bounds.getOrElse(4 * i) { 0 }, bounds.getOrElse(4 * i + 1) { 0 },
                        bounds.getOrElse(4 * i + 2) { 0 }, bounds.getOrElse(4 * i + 3) { 0 },
                    ),
                    floating = floating.getOrElse(i) { false },
                    minimized = minimized.getOrElse(i) { false },
                    pinned = pinned.getOrElse(i) { false },
                )
            }
            return ShellState(
                workspaces = b.getInt("workspaces"),
                active = b.getInt("active"),
                focusedTask = b.getInt("focusedTask", -1),
                focusedPackage = b.getString("focusedPackage"),
                focusedTitle = b.getString("focusedTitle"),
                windows = windows,
                displayWidth = display.getOrElse(0) { 0 },
                displayHeight = display.getOrElse(1) { 0 },
                workArea = if (work.size == 4) Rect(work[0], work[1], work[2], work[3]) else Rect(),
            )
        }
    }
}
