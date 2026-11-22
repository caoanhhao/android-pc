package com.android.hyprshell;

import android.os.Bundle;
import com.android.hyprshell.IHyprReply;

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

    /**
     * A hyprctl request the launcher answers (Phase E12, docs/hypr-ipc.md): the Lua side
     * (`eval`, `repl`, `keyword`, `getoption`, `configerrors`, `descriptions`), its
     * notifications (`notify`, `dismissnotify`, `seterror`) and its layer surfaces (`layers`).
     * [args] as typed after the command; [json] for `-j`.
     */
    void onRequest(String command, String args, boolean json, IHyprReply reply) = 4;
}
