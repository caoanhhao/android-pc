package com.android.hyprtile

/** The tiled windows of one workspace and how they share its work area. */
interface TileLayout {
    /** Window ids in layout order (dwindle: tree order; master: masters, then the stack). */
    val ids: List<Int>

    operator fun contains(id: Int): Boolean

    /** Adds window [id] next to [target] (the focused window), or next to the newest one. */
    fun add(id: Int, target: Int? = null)

    /** Removes [id]; returns false when it was not tiled here. */
    fun remove(id: Int): Boolean

    /** Takes the settings that shape new windows (split ratio, side, master placement). */
    fun configure(cfg: TileConfig)

    /** Exchanges the places of two tiled windows. */
    fun swap(a: Int, b: Int)

    /** Window boxes inside [area] (the display minus reserved space), gaps applied. */
    fun arrange(area: Box, cfg: TileConfig): Map<Int, Box>
}

fun newLayout(kind: LayoutKind): TileLayout = when (kind) {
    LayoutKind.DWINDLE -> DwindleLayout()
    LayoutKind.MASTER -> MasterLayout()
}
