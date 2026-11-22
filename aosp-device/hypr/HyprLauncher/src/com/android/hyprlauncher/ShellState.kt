package com.android.hyprlauncher

import android.graphics.Rect
import android.os.Bundle
import com.android.hyprtile.shownWorkspaces

/** A window as the shell reports it (IHyprShell.getState). */
data class Win(
    val taskId: Int,
    /** 1-based workspace number; a special workspace's key (negative) for its windows. */
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
    /** The workspaces that exist (Phase E2), in order. */
    val existing: List<Int> = emptyList(),
    /** Phase E3: focus history (task ids, most recent first), urgent windows, previous workspace. */
    val focusHistory: List<Int> = emptyList(),
    val urgent: List<Int> = emptyList(),
    val lastWorkspace: Int = 0,
    /** Special workspaces (E5): name by key, in key order; windows on one have its key. */
    val specials: Map<Int, String> = emptyMap(),
    /** The special workspace shown over the active one, or null. */
    val specialShown: String? = null,
) {
    /** Special workspaces that have windows or are shown, as key and name. */
    fun occupiedSpecials(): List<Pair<Int, String>> = specials.entries
        .filter { (k, n) -> n == specialShown || windows.any { it.workspace == k } }.map { it.key to it.value }

    /** The key of the special workspace shown, or null. */
    val shownSpecialKey: Int? get() = specials.entries.firstOrNull { it.value == specialShown }?.key

    /** Whether [w] is on screen: on the active workspace or the special one shown over it. */
    fun isVisible(w: Win) = !w.minimized && (w.workspace == active || w.workspace == shownSpecialKey)

    /** What the bar and overview call workspace [n]: its number, or `special:name`. */
    fun label(n: Int, names: Map<Int, String> = emptyMap()): String =
        specials[n]?.let { "special:$it" } ?: names[n] ?: "$n"

    fun on(workspace: Int) = windows.filter { it.workspace == workspace }

    /**
     * The workspaces the bar and the overview show: the active one and the occupied ones. The
     * shell removes empty desks, so the existing ones count as occupied (minimized windows are
     * not in [windows] but keep their workspace).
     */
    fun shown(): List<Int> = shownWorkspaces(active, windows.map { it.workspace } + existing)

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
                existing = (b.getIntArray("workspaceIds") ?: IntArray(0)).toList(),
                active = b.getInt("active"),
                focusedTask = b.getInt("focusedTask", -1),
                focusedPackage = b.getString("focusedPackage"),
                focusedTitle = b.getString("focusedTitle"),
                windows = windows,
                displayWidth = display.getOrElse(0) { 0 },
                displayHeight = display.getOrElse(1) { 0 },
                workArea = if (work.size == 4) Rect(work[0], work[1], work[2], work[3]) else Rect(),
                focusHistory = (b.getIntArray("focusHistory") ?: IntArray(0)).toList(),
                urgent = (b.getIntArray("urgentTasks") ?: IntArray(0)).toList(),
                lastWorkspace = b.getInt("lastWorkspace"),
                specials = (b.getIntArray("specialKeys") ?: IntArray(0)).toList()
                    .zip((b.getStringArray("specialNames") ?: emptyArray()).toList()).toMap(),
                specialShown = b.getString("specialShown"),
            )
        }
    }
}
