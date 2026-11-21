package com.android.hyprtile

/** Layout algorithm of a workspace, Hyprland's `general:layout`. */
enum class LayoutKind { DWINDLE, MASTER }

/**
 * Declarative tiling configuration (Hyprland's `general`, `dwindle` and `master` sections).
 * Pixels are physical pixels of the display.
 */
data class TileConfig(
    val layout: LayoutKind = LayoutKind.DWINDLE,
    /** Gap between a window and its neighbour (each side, so neighbours are 2x apart). */
    val gapsIn: Int = 5,
    /** Gap between a window and the edge of the work area. */
    val gapsOut: Int = 12,
    /** Space kept free for bars (the work area is the display minus this). */
    val reserved: Insets = Insets.NONE,
    /** dwindle:split_width_multiplier: a box splits top/bottom when height * m > width. */
    val splitWidthMultiplier: Float = 1f,
    /** dwindle:default_split_ratio, 0.1..1.9; 1.0 splits in the middle. */
    val defaultSplitRatio: Float = 1f,
    /** dwindle:preserve_split: keep a split's direction when its box changes shape. */
    val preserveSplit: Boolean = false,
    /** dwindle:force_split: 1 puts the new window left/top, 2 right/bottom (no cursor here). */
    val forceSplit: Int = 2,
    /** master:mfact, share of the master area. */
    val masterFactor: Float = 0.55f,
    /** master:new_status = master: a new window becomes the master. */
    val newIsMaster: Boolean = false,
    /** master:new_on_top: a new stack window goes to the top of the stack. */
    val newOnTop: Boolean = false,
    /** Packages that always float (`persist.hypr.float_packages`; Lua has window rules). */
    val floatPackages: Set<String> = emptySet(),
    /** Hyprland's window rules (float, tile, workspace, opacity), later ones win. */
    val windowRules: List<WindowRule> = emptyList(),
) {
    init {
        require(gapsIn >= 0 && gapsOut >= 0) { "gaps must not be negative" }
        require(defaultSplitRatio in 0.1f..1.9f) { "default_split_ratio must be in 0.1..1.9" }
        require(masterFactor in 0.05f..0.95f) { "mfact must be in 0.05..0.95" }
        require(forceSplit in 1..2) { "force_split must be 1 or 2" }
    }
}
