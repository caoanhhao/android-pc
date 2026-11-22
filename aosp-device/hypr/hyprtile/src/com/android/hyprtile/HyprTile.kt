package com.android.hyprtile

/**
 * The layout engine: one [TileLayout] per workspace (an AOSP desk), the configuration, and the
 * floating windows. Pure bookkeeping, no Android types: the Shell side feeds it window events
 * and applies the boxes it returns.
 */
class HyprTile(config: TileConfig = TileConfig()) {
    var config: TileConfig = config
        set(value) {
            field = value
            reconfigure()
        }

    /**
     * Hyprland's workspace rules (own layout and gaps), keyed like the workspaces here (the
     * Shell side maps workspace numbers to desks).
     */
    var workspaceRules: Map<Int, WorkspaceRule> = emptyMap()
        set(value) {
            if (value == field) return
            field = value
            reconfigure()
        }

    private val layouts = HashMap<Int, TileLayout>()
    private val workspaceOf = HashMap<Int, Int>()
    private val floating = HashSet<Int>()
    private val pinned = HashSet<Int>()

    /** Tiled window ids of [workspace], in layout order. */
    fun tiled(workspace: Int): List<Int> = layouts[workspace]?.ids ?: emptyList()

    fun isTiled(id: Int) = id in workspaceOf

    fun isFloating(id: Int) = id in floating

    fun workspaceOf(id: Int): Int? = workspaceOf[id]

    /** Hyprland's pinned windows: floating, on top, shown on every workspace. */
    fun isPinned(id: Int) = id in pinned

    fun pinned(): Set<Int> = pinned

    /**
     * Hyprland's `pin` for [id]: pinning takes a tiled window out of its layout and makes it
     * float (Hyprland pins only floating windows); unpinning leaves it floating. Returns the
     * workspace whose layout lost the window, if any.
     */
    fun setPinned(id: Int, on: Boolean): Int? {
        if (!on) {
            pinned -= id
            return null
        }
        val ws = close(id)
        floating += id
        pinned += id
        return ws
    }

    /**
     * A window opened on [workspace]. It is tiled next to [focused] (when that one is tiled on
     * the same workspace) unless [float] is set. Returns true when the workspace's layout changed.
     */
    fun open(id: Int, workspace: Int, focused: Int? = null, float: Boolean = false): Boolean {
        if (float) {
            // Moving a pinned window to another workspace keeps it pinned.
            val pin = id in pinned
            close(id)
            floating += id
            if (pin) pinned += id
            return false
        }
        if (workspaceOf[id] == workspace) return false
        close(id)
        val cfg = configOf(workspace)
        val layout = layouts.getOrPut(workspace) { newLayout(cfg.layout) }
        layout.configure(cfg)
        layout.add(id, focused?.takeIf { workspaceOf[it] == workspace })
        workspaceOf[id] = workspace
        return true
    }

    /**
     * A window closed, was minimized or became floating (no longer pinned then); returns its old
     * workspace if tiled.
     */
    fun close(id: Int): Int? {
        floating -= id
        pinned -= id
        val ws = workspaceOf.remove(id) ?: return null
        layouts[ws]?.let {
            it.remove(id)
            if (it.ids.isEmpty()) layouts.remove(ws)
        }
        return ws
    }

    /** Hyprland's `togglefloating`; returns true when [id] is now floating. */
    fun toggleFloating(id: Int, workspace: Int, focused: Int? = null): Boolean {
        return if (isTiled(id)) {
            close(id); floating += id; true
        } else {
            open(id, workspace, focused); false
        }
    }

    fun swap(a: Int, b: Int) {
        val ws = workspaceOf[a]
        require(ws != null && ws == workspaceOf[b]) { "$a and $b are not on one workspace" }
        layouts.getValue(ws).swap(a, b)
    }

    /** Window boxes of [workspace] on a display of the given bounds. */
    fun arrange(workspace: Int, display: Box): Map<Int, Box> {
        val layout = layouts[workspace] ?: return emptyMap()
        val cfg = configOf(workspace)
        val area = display.inset(cfg.reserved)
        // Special workspaces (negative keys, Special.kt) may take a smaller, centred area.
        return layout.arrange(if (workspace < 0) specialArea(area, cfg.specialScaleFactor) else area, cfg)
    }

    /** Hyprland's `resizeactive` for a tiled window: [dx] wider, [dy] taller. */
    fun resize(id: Int, dx: Int, dy: Int) {
        val ws = workspaceOf[id] ?: return
        layouts[ws]?.resize(id, dx, dy)
    }

    /** The layout of [workspace], for layout-specific dispatchers (togglesplit, ...). */
    fun layout(workspace: Int): TileLayout? = layouts[workspace]

    /** The configuration of [workspace]: [config] with its workspace rule applied. */
    fun configOf(workspace: Int): TileConfig = workspaceRules[workspace]?.apply(config) ?: config

    /** Applies the configuration to every layout; one whose kind changed is rebuilt, windows kept. */
    private fun reconfigure() {
        for ((ws, old) in layouts.entries.toList()) {
            val cfg = configOf(ws)
            val kind = if (old is MasterLayout) LayoutKind.MASTER else LayoutKind.DWINDLE
            if (kind == cfg.layout) {
                old.configure(cfg)
                continue
            }
            val fresh = newLayout(cfg.layout).also { it.configure(cfg) }
            old.ids.forEach { fresh.add(it) }
            layouts[ws] = fresh
        }
    }
}
