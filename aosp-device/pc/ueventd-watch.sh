#!/vendor/bin/sh
# ueventd watchdog (init.pc.rc, docs/usb-boot-aosp17.md "ueventd recovery").
#
# init restarts ueventd when it dies, but a restarted ueventd skips coldboot (ro.cold_boot_done
# is already set), so every device the kernel added while it was down never gets its /dev node:
# on the real PC a ueventd crash right after coldboot left pcfb's /dev/dri/card0, the ALSA PCMs
# and /dev/video* missing, and SurfaceFlinger never came up. This script notices the restart and
# replays "add" for each device whose node is missing, so the new ueventd creates it.
#
# While ueventd is down it also makes the missing /dev/block nodes (and the device-mapper links)
# itself: init does not restart ueventd while it is blocked in a command, and in mount_all vold
# waits for the dm node of userdata's default-key device; without it /data never mounts.
#
# Only devices with a missing node are replayed, not every /sys/**/uevent like a real coldboot:
# vold, netd and the health HAL listen to the same uevents, and vold adds a second Disk for a
# block device it already has.
#
# With androidboot.android_pc.test_ueventd_gap=1 it kills ueventd once, right after coldboot,
# to reproduce the PC failure (a test knob; see the docs).
export PATH=/system/bin:/vendor/bin:$PATH
state=/dev/.pc_ueventd_watch   # the ueventd pid already handled (survives a restart of this script)

say() { echo "pc-ueventd-watch: $*" > /dev/kmsg; }

# A dead ueventd stays a zombie until init reaps it, which init does not do while it is blocked
# in a command (mount_all, wait), so "alive" means "not a zombie".
alive() {
    local _a _b st
    [ -r /proc/$1/stat ] && read -r _a _b st _ < /proc/$1/stat 2>/dev/null && [ "$st" != Z ]
}
ueventd_pid() {
    local p
    for p in $(pidof ueventd 2>/dev/null); do alive $p && { echo $p; return; }; done
}

# Replays "add" for every device whose DEVNAME has no node under /dev (or /dev/block).
recoldboot() {
    local n=0 names="" u name k v
    for u in $(find /sys/devices -name uevent -type f 2>/dev/null); do
        name=""
        while IFS='=' read -r k v; do
            [ "$k" = DEVNAME ] && name=$v
        done < "$u" 2>/dev/null
        [ -z "$name" ] && continue
        [ -e "/dev/$name" ] || [ -e "/dev/block/$name" ] && continue
        echo add > "$u" 2>/dev/null || continue
        n=$((n + 1))
        names="$names $name"
    done
    say "replayed add for $n device(s):$names"
    # Give the new ueventd time to create the nodes before the HALs below reopen them.
    sleep 2
    restart_users "$names"
}

# Makes the /dev/block nodes ueventd would have made, and the /dev/block/mapper links of dm
# devices (their name only appears once the table is loaded, so this runs every 0.5 s).
bridged=""
bridge_block() {
    local s name mm dn du
    for s in /sys/class/block/*; do
        name=${s##*/}
        if [ ! -e /dev/block/$name ] && [ -r $s/dev ]; then
            read -r mm < $s/dev
            mknod -m 600 /dev/block/$name b ${mm%:*} ${mm#*:} || continue
            restorecon /dev/block/$name
            bridged="$bridged $name"
            say "made /dev/block/$name"
        fi
        case $name in dm-*) ;; *) continue ;; esac
        dn="" du=""
        [ -r $s/dm/name ] && read -r dn < $s/dm/name
        [ -r $s/dm/uuid ] && read -r du < $s/dm/uuid
        [ -n "$dn" ] && block_link /dev/block/mapper/$dn /dev/block/$name
        [ -n "$du" ] && block_link /dev/block/mapper/by-uuid/$du /dev/block/$name
    done
}
block_link() {
    [ -L $1 ] && return
    mkdir -p ${1%/*} && ln -s $2 $1 && restorecon $1 && say "linked $1 -> $2"
}

# Hands the nodes bridge_block made back to ueventd ("add" fixes the mode and owner of an
# existing node and makes the links). Not for whole disks: vold would add them twice.
replay_bridged() {
    local name
    for name in $bridged; do
        case $name in dm-*|loop*|zram*) echo add > /sys/class/block/$name/uevent ;; esac
    done
    [ -n "$bridged" ] && say "handed back to ueventd:$bridged"
    bridged=""
}

# Restarts the HALs (and their clients) that were already running without their nodes.
restart_users() {
    local svcs="" s
    case "$1" in *dri/*) svcs="$svcs vendor.graphics.allocator vendor.hwcomposer-3 surfaceflinger" ;; esac
    case "$1" in *snd/*) svcs="$svcs vendor.audio-hal vendor.audio-hal-aidl audioserver" ;; esac
    case "$1" in *video*) svcs="$svcs vendor.camera-pc cameraserver" ;; esac
    for s in $svcs; do
        case "$(getprop init.svc.$s)" in
            running|restarting)
                say "restarting $s"
                setprop ctl.restart "$s"
                ;;
        esac
    done
}

until [ "$(getprop ro.cold_boot_done)" = true ]; do sleep 0.1; done

pid=$(ueventd_pid)
if [ -f $state ]; then
    read -r handled < $state
    [ "$pid" != "$handled" ] && { say "ueventd restarted (pid $handled -> $pid)"; recoldboot; }
elif [ "$(getprop ro.boot.android_pc.test_ueventd_gap)" = 1 ]; then
    say "test_ueventd_gap: killing ueventd (pid $pid) after coldboot"
    kill -9 "$pid"
    pid=""
else
    # Started after a restart already happened? ueventd's start time (clock ticks since boot,
    # field 22 of /proc/PID/stat) against init's record of its first start (ns since boot).
    set -- $(cat /proc/$pid/stat 2>/dev/null)
    first=$(getprop ro.boottime.ueventd)
    if [ -n "${22}" ] && [ -n "$first" ] && [ $(( ${22} * 10 )) -gt $(( first / 1000000 + 1000 )) ]; then
        say "ueventd (pid $pid) was restarted before this watchdog started"
        recoldboot
    fi
fi
echo "$pid" > $state

while true; do
    while [ -n "$pid" ] && alive $pid; do sleep 2; done
    say "ueventd (pid ${pid:-?}) is gone, waiting for init to restart it"
    pid=""
    until [ -n "$pid" ]; do bridge_block; sleep 0.5; pid=$(ueventd_pid); done
    echo "$pid" > $state
    # The new ueventd opens its uevent socket first thing; let it get there.
    sleep 1
    say "ueventd restarted (pid $pid)"
    replay_bridged
    recoldboot
done
