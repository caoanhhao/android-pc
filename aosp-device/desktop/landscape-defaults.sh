#!/system/bin/sh
# Landscape apps (docs/desktop-mode.md, "Every app in landscape"), started by
# init.landscape.rc once the boot completes, on fresh and existing userdata alike.
export PATH=/system/bin:$PATH
DONE=/data/misc/pc-landscape-rotation.done
k() { echo "landscape-defaults: $*" > /dev/kmsg; }

# WindowManager ignores the fixed orientation, aspect ratio and resizability of every app
# (except games) on a large screen, as it already does for apps that target SDK 36
# (UNIVERSAL_RESIZABLE_BY_DEFAULT): portrait-only and non-resizable apps fill their desktop
# window or the fullscreen task in landscape instead of being letterboxed. Set only while
# unset, so a value someone put on purpose ("none") stays; WindowManager listens to the key.
if [ "$(device_config get window_manager ignore_activity_orientation_request_screens)" = null ]; then
    device_config put window_manager ignore_activity_orientation_request_screens large
    k "ignore_activity_orientation_request_screens=large"
fi

# Rotation locked at the panel's landscape, once per userdata (the SettingsProvider overlay
# sets the same defaults on a fresh one; an older userdata has auto-rotation on from
# Cuttlefish's defaults). A later choice in Settings stays.
if [ ! -e $DONE ]; then
    settings put system accelerometer_rotation 0
    settings put system user_rotation 0
    touch $DONE
    k "rotation locked at 0"
fi
