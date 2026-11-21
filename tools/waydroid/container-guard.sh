#!/bin/bash
# Waydroid container guard (docs/waydroid-aosp17.md, "Container guard").
#
# Waydroid's LXC container keeps CAP_SYS_ADMIN and mknod and has no device cgroup rules, and
# /data is a bind mount of a directory on the host's root filesystem. On 2026-11-18 our
# AOSP 17 image failed early, rebooted, and init's shutdown path ran EXT4_IOC_SHUTDOWN on
# /data: the kernel shut down the host's / and the host froze. The guard puts the container
# boundary back on the host side, whatever image runs inside:
#   - seccomp: the filesystem shutdown and freeze ioctls (EXT4/XFS/generic FS_IOC_SHUTDOWN,
#     F2FS_IOC_SHUTDOWN, FIFREEZE, FITHAW) fail with EPERM;
#   - device cgroup: the host's disk block devices (sd, nvme, virtio, mmc, md, dm, nbd, ...)
#     cannot be created or opened; loop devices (APEX) stay allowed.
#
#   container-guard.sh install   add the guard to Waydroid's config templates
#                                (/usr/lib/waydroid/data/configs, used by every
#                                `waydroid init/upgrade`) and to the current container config,
#                                and install a pacman hook that adds it again after every
#                                waydroid package update
#   container-guard.sh apply     (root, used by the pacman hook) add it where missing
#   container-guard.sh check     exit 1 unless every file has the guard
#   container-guard.sh test      run a throwaway LXC container (not Waydroid) with the guard
#                                and probe it (tools/waydroid/guard/guardtest.c);
#                                NOGUARD=1 runs it without the guard as a control
#   container-guard.sh gamepads [on|off]
#                                let the host's game pads into the container (the default,
#                                for playing Android games) or keep them out (while playing
#                                on the host): a device cgroup rule on evdev/joydev (char
#                                major 13), kept across `waydroid upgrade` and package
#                                updates; a running container follows at once. No argument:
#                                show the setting
# Run as your user: it asks for the sudo password itself.
set -euo pipefail

TEMPLATES=/usr/lib/waydroid/data/configs
LXC=/var/lib/waydroid/lxc/waydroid
INSTALLED=/usr/local/lib/android-pc/waydroid-container-guard.sh
HOOK=/etc/pacman.d/hooks/waydroid-container-guard.hook
BEGIN="# android-pc container guard begin"
END="# android-pc container guard end"
PADS_STATE=/var/lib/waydroid/android-pc-gamepads
PADS_BEGIN="# android-pc game pads off begin"
PADS_END="# android-pc game pads off end"
SUDO=sudo
[ "$(id -u)" = 0 ] && SUDO=

die() { echo "container-guard: $*" >&2; exit 1; }

# The request argument is masked to 32 bits: the kernel takes an unsigned int, and bionic
# passes ioctl's int request sign-extended to 64 bits.
seccomp_block() {
    cat <<EOF
$BEGIN
# filesystem shutdown: EXT4_IOC_SHUTDOWN = XFS_IOC_GOINGDOWN = FS_IOC_SHUTDOWN
ioctl errno 1 [1,0x8004587d,SCMP_CMP_MASKED_EQ,0xffffffff]
# F2FS_IOC_SHUTDOWN
ioctl errno 1 [1,0x8004f519,SCMP_CMP_MASKED_EQ,0xffffffff]
# FIFREEZE, FITHAW
ioctl errno 1 [1,0xc0045877,SCMP_CMP_MASKED_EQ,0xffffffff]
ioctl errno 1 [1,0xc0045878,SCMP_CMP_MASKED_EQ,0xffffffff]
$END
EOF
}

# Block majors of disks (Documentation/admin-guide/devices.txt): 3 ide, 8 65-71 128-135 sd,
# 9 md, 11 sr, 43 nbd, 179 mmc, 202 xvd, 259 blkext (nvme, partitions); 234-254 are the
# dynamic block majors (virtblk, device-mapper, zram, ...). Loop (7) stays allowed.
config_block() {
    echo "$BEGIN"
    echo "lxc.cgroup2.devices.allow = a"
    local m
    for m in 3 8 9 11 43 $(seq 65 71) $(seq 128 135) 179 202 $(seq 234 254) 259; do
        echo "lxc.cgroup2.devices.deny = b $m:* rwm"
    done
    echo "$END"
}

has_guard() { grep -qxF "$BEGIN" "$1" 2>/dev/null; }

add_guard() {   # add_guard FILE BLOCKFN
    local f=$1
    [ -f "$f" ] || return 0
    has_guard "$f" && return 0
    { echo; "$2"; } >> "$f"
    echo "container-guard: added to $f"
}

apply() {
    [ "$(id -u)" = 0 ] || die "apply runs as root"
    add_guard "$TEMPLATES/waydroid.seccomp" seccomp_block
    add_guard "$TEMPLATES/config_base" config_block
    add_guard "$LXC/waydroid.seccomp" seccomp_block
    add_guard "$LXC/config" config_block
    pads_sync "$TEMPLATES/config_base"
    pads_sync "$LXC/config"
}

# Game pads (docs/waydroid-aosp17.md, "Host game pads"). The container gets every host uevent,
# and its ueventd makes /dev/input nodes for game pads and joysticks, so a pad the user plays
# with on the host also drives Android (B = BACK). Off, the device cgroup refuses the input
# char devices (major 13: event*, js*, mouse*): ueventd's mknod fails and nothing in the
# container can open one. The composer's wl_*_events are FIFOs and the virtual pads apps make
# go through /dev/uinput and /dev/uhid (other majors), so none of that changes.
pads_state() { cat "$PADS_STATE" 2>/dev/null || echo on; }

pads_block() {
    echo "$PADS_BEGIN"
    echo "lxc.cgroup2.devices.deny = c 13:* rwm"
    echo "$PADS_END"
}

# pads_sync FILE: the block in FILE as the setting asks (root)
pads_sync() {
    local f=$1
    [ -f "$f" ] || return 0
    if grep -qxF "$PADS_BEGIN" "$f"; then
        [ "$(pads_state)" = off ] && return 0
        sed -i "/^$PADS_BEGIN\$/,/^$PADS_END\$/d" "$f"
    else
        [ "$(pads_state)" = on ] && return 0
        pads_block >> "$f"
    fi
}

# The input nodes for the host's game pads and joysticks, as the image's ueventd picks them
# (a key in BTN_JOYSTICK..BTN_THUMBR): "name major minor" per line.
host_pads() {
    python3 -I - <<'EOF'
import glob, os
for dev in sorted(glob.glob("/sys/class/input/event*") + glob.glob("/sys/class/input/js*")):
    try:
        words = open(dev + "/device/capabilities/key").read().split()
        major, minor = open(dev + "/dev").read().strip().split(":")
    except OSError:
        continue
    bits = int("".join(w.rjust(16, "0") for w in words), 16)
    if bits >> 0x120 & (1 << 0x20) - 1:
        print(os.path.basename(dev), major, minor)
EOF
}

# A running container follows the setting at once: the device cgroup rule is changed live,
# and the nodes are removed or made from the host through the container init's root (works
# also while Waydroid has the container frozen). EventHub sees the nodes go or come.
pads_live() {
    local lxcpath pid name major minor
    lxcpath=$(dirname "$LXC")
    pid=$($SUDO lxc-info -P "$lxcpath" -n waydroid -pH 2>/dev/null) || return 0
    [ -n "$pid" ] || return 0
    local dev=/proc/$pid/root/dev/input
    if [ "$(pads_state)" = off ]; then
        $SUDO lxc-cgroup -P "$lxcpath" -n waydroid devices.deny "c 13:* rwm"
        for name in $($SUDO find "$dev" -maxdepth 1 -type c -printf '%f\n' 2>/dev/null); do
            $SUDO /usr/bin/rm -f "$dev/$name"
            echo "container-guard: removed /dev/input/$name in the container"
        done
    else
        $SUDO lxc-cgroup -P "$lxcpath" -n waydroid devices.allow "c 13:* rwm"
        $SUDO test -d "$dev" || return 0
        host_pads | while read -r name major minor; do
            $SUDO test -e "$dev/$name" && continue
            # ueventd.rc: /dev/input/* 0660 root input (AID_INPUT 1004). Made with that group
            # at once: EventHub opens the node on IN_CREATE, before a chown could run.
            $SUDO setpriv --regid 1004 --clear-groups mknod -m 0660 "$dev/$name" c "$major" "$minor"
            echo "container-guard: made /dev/input/$name in the container"
        done
    fi
}

gamepads() {
    case ${1:-} in
        "") echo "container-guard: host game pads in the container: $(pads_state)"; return ;;
        on|off) ;;
        *) die "gamepads on|off" ;;
    esac
    echo "$1" | $SUDO tee "$PADS_STATE" >/dev/null
    # The pacman hook runs the installed copy: keep it the one that knows the setting.
    $SUDO install -D -m 755 "$0" "$INSTALLED"
    $SUDO "$INSTALLED" apply
    pads_live
    echo "container-guard: host game pads in the container: $1"
}

check() {
    local f bad=0
    for f in "$TEMPLATES/waydroid.seccomp" "$TEMPLATES/config_base" \
             "$LXC/waydroid.seccomp" "$LXC/config"; do
        if $SUDO test -f "$f" && ! $SUDO grep -qxF "$BEGIN" "$f"; then
            echo "container-guard: MISSING in $f" >&2
            bad=1
        fi
    done
    [ -f "$HOOK" ] || { echo "container-guard: MISSING $HOOK" >&2; bad=1; }
    [ $bad = 0 ] && echo "container-guard: present"
    echo "container-guard: host game pads in the container: $(pads_state)"
    return $bad
}

install_guard() {
    $SUDO install -D -m 755 "$0" "$INSTALLED"
    $SUDO mkdir -p "$(dirname "$HOOK")"
    $SUDO tee "$HOOK" >/dev/null <<EOF
# android-pc: put the container guard back into Waydroid's config templates after every
# waydroid package update (tools/waydroid/container-guard.sh in the android-pc repo).
[Trigger]
Operation = Install
Operation = Upgrade
Type = Package
Target = waydroid

[Action]
Description = Adding the android-pc container guard to Waydroid's LXC config...
When = PostTransaction
Exec = $INSTALLED apply
EOF
    $SUDO "$INSTALLED" apply
    check
}

# A throwaway privileged LXC container with Waydroid's capabilities, seccomp profile and
# the guard, running only the probe; nothing of Waydroid's is started or mounted.
test_guard() {
    local root
    root="$(cd "$(dirname "$0")/../.." && pwd)"
    work=$(mktemp -d "${TMPDIR:-/tmp}/container-guard.XXXXXX")
    trap '$SUDO /usr/bin/rm -rf "$work"' EXIT
    mkdir -p "$work/c/rootfs/dev" "$work/c/rootfs/proc"
    gcc -static -O2 -o "$work/c/rootfs/guardtest" "$root/tools/waydroid/guard/guardtest.c"
    # NOGUARD=1: the same container without the guard, as a control (the probe then
    # reports FAIL with ENOTTY, from tmpfs, and opens the disks read-only).
    local guard=seccomp_block devices=config_block
    [ "${NOGUARD:-}" = 1 ] && guard=true devices=true
    grep -vxF -f <(seccomp_block) "$TEMPLATES/waydroid.seccomp" > "$work/c/seccomp"
    "$guard" >> "$work/c/seccomp"
    cat > "$work/c/config" <<EOF
lxc.rootfs.path = dir:$work/c/rootfs
lxc.uts.name = guardtest
lxc.autodev = 0
lxc.mount.entry = tmpfs dev tmpfs nosuid 0 0
lxc.mount.auto = proc
lxc.net.0.type = empty
lxc.console.path = none
lxc.apparmor.profile = unconfined
$(grep '^lxc.cap.keep' "$TEMPLATES/config_base")
lxc.seccomp.profile = $work/c/seccomp
lxc.seccomp.allow_nesting = 1
lxc.no_new_privs = 1
$("$devices")
lxc.init.cmd = /guardtest
EOF
    $SUDO lxc-start -P "$work" -n c -F -l INFO -o "$work/lxc.log" || true
    if ! $SUDO test -f "$work/c/rootfs/result"; then
        $SUDO tail -20 "$work/lxc.log"
        die "the probe did not run"
    fi
    cat "$work/c/rootfs/result"
    ! grep -q '^FAIL' "$work/c/rootfs/result"
}

case ${1:-} in
    install) install_guard ;;
    apply) apply ;;
    check) check ;;
    test) test_guard ;;
    gamepads) gamepads "${2:-}" ;;
    *) sed -n '2,/^set -e/p' "$0" | sed '$d'; exit 1 ;;
esac
