#!/system/bin/sh
# Phase H4 (docs/desktop-mode.md), started by init.desktop.rc once the first boot completes:
# makes Smart Dock (cu.axel.smartdock, a /product app from tools/fetch-apps.sh) the default
# home app and gives it what its setup screen would otherwise ask for one by one. It runs
# once per userdata: after that the user's own choices (another launcher, the dock's
# accessibility service turned off) stay.
export PATH=/system/bin:$PATH
PKG=cu.axel.smartdock
DONE=/data/misc/smartdock-setup.done
k() { echo "smartdock-setup: $*" > /dev/kmsg; }

[ -e $DONE ] && exit 0
pm path $PKG > /dev/null 2>&1 || { k "$PKG not installed"; exit 0; }

# Its notification panel is a notification listener.
cmd notification allow_listener $PKG/$PKG.services.NotificationService

# Overlay windows (the dock and its menus), running apps (usage stats), and secure settings
# (it hides the navigation bar and turns freeform on itself).
appops set $PKG SYSTEM_ALERT_WINDOW allow
appops set $PKG GET_USAGE_STATS allow
pm grant $PKG android.permission.WRITE_SECURE_SETTINGS

# Launcher3 and Smart Dock both offer HOME at priority 0, so without this Android asks.
if cmd role add-role-holder --user 0 android.app.role.HOME $PKG 0; then
    k "home is $(cmd role get-role-holders --user 0 android.app.role.HOME)"
else
    k "could not make $PKG the home app"
    exit 0
fi

# The first boot reached the HOME intent before this ran, while Launcher3 and Smart Dock
# still tied, so the "Use Smart Dock as Home?" chooser is up: close it, and HOME now goes
# to the role holder.
if dumpsys activity activities | grep -m1 ' ResumedActivity:' | grep -q ResolverActivity; then
    input keyevent KEYCODE_BACK
    sleep 2
    am start -a android.intent.action.MAIN -c android.intent.category.HOME > /dev/null
fi

# The dock itself is an accessibility service (with no lock screen, see the
# config_disableLockscreenByDefault overlay, it shows its dock when it starts).
a11y=$PKG/$PKG.services.DockService
cur=$(settings get secure enabled_accessibility_services)
case "$cur" in
    *$a11y*) ;;
    ""|null) settings put secure enabled_accessibility_services $a11y ;;
    *) settings put secure enabled_accessibility_services "$cur:$a11y" ;;
esac
settings put secure accessibility_enabled 1
touch $DONE
