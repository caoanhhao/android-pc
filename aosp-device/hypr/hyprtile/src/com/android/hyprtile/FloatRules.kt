package com.android.hyprtile

/** What the Shell side knows about a new window, reduced to what tiling decisions need. */
data class WindowTraits(
    val packageName: String? = null,
    /** The app can be resized to any box (TaskInfo.isResizeable). */
    val resizeable: Boolean = true,
    /** The top activity is translucent (TaskInfo.isTopActivityTransparent). */
    val translucent: Boolean = false,
    /** The top activity's theme sets android:windowIsFloating (dialog-styled activities). */
    val dialog: Boolean = false,
    /** Minimum size the app declares (<layout android:minWidth>), 0 when none. */
    val minWidth: Int = 0,
    val minHeight: Int = 0,
)

/**
 * Hyprland floats dialogs and fixed-size windows by default; here: apps that cannot be resized,
 * translucent and dialog-themed activities, packages listed in [TileConfig.floatPackages], and apps
 * whose minimum size is larger than the work area could ever give them.
 */
fun shouldFloat(w: WindowTraits, cfg: TileConfig, workArea: Box? = null): Boolean {
    if (!w.resizeable || w.translucent || w.dialog) return true
    if (w.packageName != null && w.packageName in cfg.floatPackages) return true
    if (workArea != null && (w.minWidth > workArea.width || w.minHeight > workArea.height)) return true
    return false
}
