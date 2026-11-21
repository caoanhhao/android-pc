package com.android.hyprshell;

import android.os.Bundle;
import com.android.hyprshell.IHyprShellListener;

/**
 * The Hyprland-style shell in PcSystemUI's WM Shell, for HyprLauncher (Phase D4). Callers need
 * MANAGE_ACTIVITY_TASKS (the recents provider has it).
 */
interface IHyprShell {
    /**
     * Configuration over the persist.hypr.* properties: string values under the property names
     * without the prefix (layout, gaps_in, gaps_out, reserved_top, reserved_bottom,
     * float_packages). An empty Bundle goes back to the properties. Phase D5 fills it from Lua.
     */
    void setConfig(in Bundle config) = 1;

    /**
     * Runs a Hyprland dispatcher (workspace, movetoworkspace, movefocus, movewindow,
     * togglefloating, togglesplit, fullscreen, killactive, exec, focuswindow). Returns false for
     * an unknown dispatcher or a bad argument.
     */
    boolean dispatch(String name, String arg) = 2;

    /** The bar keeps these strips of the display out of the layout (waybar's exclusive zone). */
    void setReserved(int displayId, int top, int bottom) = 3;

    /** Workspaces and windows of the display, as HyprTileController.state() describes. */
    Bundle getState(int displayId) = 4;

    /** Adds a listener; it is dropped when its process dies. */
    void addListener(IHyprShellListener listener) = 5;

    void removeListener(IHyprShellListener listener) = 6;
}
