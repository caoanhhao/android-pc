package com.android.hyprshell;

/** The answer to IHyprShellListener.onRequest (Phase E12, docs/hypr-ipc.md). */
oneway interface IHyprReply {
    /** hyprctl's output for the request, as Hyprland prints it (text or JSON). */
    void reply(String text) = 1;
}
