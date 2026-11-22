#!/bin/bash
# hyprctl on the Android side, from the host (Phase E12, docs/hypr-ipc.md): runs the image's
# /system_ext/bin/hyprctl in Waydroid's container (`sudo waydroid shell`) or on an adb device, with
# the arguments passed through unchanged. `tools/hyprctl.sh events` streams the event socket.
#
#   HYPRCTL_TARGET=waydroid|adb   default: waydroid when its container runs, else adb
#   ANDROID_SERIAL=...            the adb device, as for adb itself
set -euo pipefail
target=${HYPRCTL_TARGET:-}
if [[ -z $target ]]; then
    if pgrep -x lxc-start >/dev/null || pgrep -f '[l]xc-start .* waydroid' >/dev/null; then
        target=waydroid
    else
        target=adb
    fi
fi
case $target in
    waydroid)
        # lxc-attach makes file stdio root-owned: pipe it. stdin stays for `hyprctl repl`.
        sudo -n waydroid shell -- /system_ext/bin/hyprctl "$@" 2>&1 | cat
        exit "${PIPESTATUS[0]}"
        ;;
    adb)
        # adb shell joins its arguments for the device's sh: quote each one.
        q=()
        for a in "$@"; do q+=("'${a//\'/\'\\\'\'}'"); done
        exec adb shell /system_ext/bin/hyprctl "${q[@]}"
        ;;
    *)
        echo "hyprctl.sh: HYPRCTL_TARGET must be waydroid or adb" >&2
        exit 1
        ;;
esac
