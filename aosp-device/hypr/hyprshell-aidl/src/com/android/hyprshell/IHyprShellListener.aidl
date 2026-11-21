package com.android.hyprshell;

import android.os.Bundle;

/** Events from the shell to HyprLauncher (Phase D4). */
oneway interface IHyprShellListener {
    /** The workspaces or windows of the display changed; [state] as IHyprShell.getState(). */
    void onStateChanged(int displayId, in Bundle state) = 1;

    /**
     * A Hyprland-style event, as Hyprland's event socket names them: workspace (data: N),
     * activewindow (package,title), openwindow (task id,package), closewindow (task id).
     */
    void onEvent(String name, String data) = 2;

    /** A dispatcher the launcher runs itself: launcher, overview (from a keybind). */
    void onDispatch(String name, String arg) = 3;
}
