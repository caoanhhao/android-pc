package com.android.hyprtile

/**
 * The layout engine: one [TileLayout] per workspace (an AOSP desk), the configuration, and the
 * floating windows. Pure bookkeeping, no Android types: the Shell side feeds it window events
 * and applies the boxes it returns.
 */
class HyprTile(config: TileConfig = TileConfig()) {
    var config: TileConfig = config
        set(value) {
            val relayout = value.layout != field.layout
            field = value
            if (relayout) rebuildLayouts()
            layouts.values.forEach { it.configure(value) }
        }

    private val layouts = HashMap<Int, TileLayout>()
    private val workspaceOf = HashMap<Int, Int>()
    private val floating = HashSet<Int>()

    /** Tiled window ids of [workspace], in layout order. */
    fun tiled(workspace: Int): List<Int> = layouts[workspace]?.ids ?: emptyList()

    fun isTiled(id: Int) = id in workspaceOf

    fun isFloating(id: Int) = id in floating

    fun workspaceOf(id: Int): Int? = workspaceOf[id]

    /**
     * A window opened on [workspace]. It is tiled next to [focused] (when that one is tiled on
     * the same workspace) unless [float] is set. Returns true when the workspace's layout changed.
     */
    fun open(id: Int, workspace: Int, focused: Int? = null, float: Boolean = false): Boolean {
        if (float) {
            close(id)
            floating += id
            return false
        }
        if (workspaceOf[id] == workspace) return false
        close(id)
        val layout = layouts.getOrPut(workspace) { newLayout(config.layout) }
        layout.configure(config)
        layout.add(id, focused?.takeIf { workspaceOf[it] == workspace })
        workspaceOf[id] = workspace
        return true
    }

    /** A window closed, was minimized or became floating; returns its old workspace if tiled. */
    fun close(id: Int): Int? {
        floating -= id
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
        return layout.arrange(display.inset(config.reserved), config)
    }

    /** Hyprland's `resizeactive` for a tiled window: [dx] wider, [dy] taller. */
    fun resize(id: Int, dx: Int, dy: Int) {
        val ws = workspaceOf[id] ?: return
        layouts[ws]?.resize(id, dx, dy)
    }

    /** The layout of [workspace], for layout-specific dispatchers (togglesplit, ...). */
    fun layout(workspace: Int): TileLayout? = layouts[workspace]

    private fun rebuildLayouts() {
        for ((ws, old) in layouts.entries.toList()) {
            val fresh = newLayout(config.layout).also { it.configure(config) }
            old.ids.forEach { fresh.add(it) }
            layouts[ws] = fresh
        }
    }
}
