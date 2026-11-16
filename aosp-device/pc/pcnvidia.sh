#!/vendor/bin/sh
# Phase K1/K1b (docs/nvidia-proprietary.md): loads NVIDIA's own kernel modules and creates
# their device nodes, which the modules register (char major 195, nvidia-uvm and nvidia-caps
# dynamic) without creating any node (on desktop Linux nvidia-modprobe does it):
#   /dev/nvidiactl 195:255, /dev/nvidia-modeset 195:254, /dev/nvidiaN 195:N per GPU,
#   /dev/nvidia-uvm and /dev/nvidia-uvm-tools (minors 0, 1), /dev/nvidia-caps/nvidia-capN,
# mode 0666, group graphics, as on desktop Linux; the K2 sidecar opens them.
#
#   pcnvidia.sh load    the nvidia entries (init.pc.rc, post-fs): waits until pclog writes the
#                       kernel log to the stick, then loads nvidia, nvidia-modeset,
#                       nvidia-drm modeset=1 and nvidia-uvm one at a time, with a kernel log
#                       marker and a sync before each, so a crash or hang leaves the last
#                       step on the stick. vkms is moved off card0 first (Phase I1).
#                       With androidboot.android_pc.nvidia_rm=gcc (K1d A/B entry) the modules
#                       come from /vendor/lib/nvidia-gcc instead: the same build with the RM
#                       cores compiled by gcc (tools/nvidia-build.sh step 4), loaded by path.
export PATH=/system/bin:/vendor/bin:$PATH
k() { echo "<4>pcnvidia: $*" > /dev/kmsg; }
if [ "$(getprop ro.boot.android_pc.nvidia_rm)" = gcc ]; then
    # Same module names as the vendor_dlkm set, so not through modprobe: insmod by path, in
    # dependency order (nvidia, nvidia-modeset, nvidia-drm; nvidia-uvm needs only nvidia).
    mp() {
        case "$1" in
        -r) modprobe -r -d /vendor/lib/modules "$2" ;;
        nvidia*) local m=$1; shift; insmod /vendor/lib/nvidia-gcc/$m "$@" ;;
        *) modprobe -d /vendor/lib/modules "$@" ;;
        esac
    }
else
    mp() { modprobe -d /vendor/lib/modules "$@"; }
fi
step() {   # step PAUSE COMMAND...: marker, sync, pause, run, result, sync
    local pause=$1; shift
    k "step: $* (uptime $(cut -d' ' -f1 /proc/uptime))"
    sync; sleep "$pause"
    "$@" > /dev/kmsg 2>&1
    local rc=$?
    k "step done ($rc): $*"
    sync
    return $rc
}
node() {   # node PATH MAJOR MINOR
    [ -e "$1" ] || mknod -m 0666 "$1" c "$2" "$3"
    chown root:graphics "$1"
}
major() { grep " $1\$" /proc/devices | cut -d' ' -f1; }
nodes() {
    if ! grep -q '^nvidia ' /proc/modules; then
        k "nvidia.ko not loaded (no NVIDIA GPU, or it failed: see the NVRM lines above)"
        return 1
    fi
    node /dev/nvidiactl 195 255
    grep -q '^nvidia_modeset ' /proc/modules && node /dev/nvidia-modeset 195 254
    local n=0 g minor m c
    for g in /proc/driver/nvidia/gpus/*; do
        [ -d "$g" ] || continue
        minor=$(sed -n 's/^Device Minor:[[:space:]]*//p' "$g/information")
        node /dev/nvidia${minor:-$n} 195 ${minor:-$n}
        n=$((n + 1))
    done
    m=$(major nvidia-uvm)
    if [ -n "$m" ]; then node /dev/nvidia-uvm "$m" 0; node /dev/nvidia-uvm-tools "$m" 1; fi
    m=$(major nvidia-caps)
    if [ -n "$m" ]; then
        mkdir -p /dev/nvidia-caps
        for c in /proc/driver/nvidia/capabilities/mig/config /proc/driver/nvidia/capabilities/mig/monitor; do
            minor=$(sed -n 's/^DeviceFileMinor:[[:space:]]*//p' "$c" 2>/dev/null)
            [ -n "$minor" ] && node /dev/nvidia-caps/nvidia-cap$minor "$m" "$minor"
        done
    fi
    k "$n GPU(s); $(ls /dev/nvidia* /dev/nvidia-caps/* 2>/dev/null | tr '\n' ' ')"
    k "$(head -n 1 /proc/driver/nvidia/version 2>/dev/null)"
}
cards() {
    local c
    for c in /sys/class/drm/card?; do
        [ -e "$c/device/driver" ] && k "${c##*/}: $(basename "$(readlink $c/device/driver)")"
    done
}

case "$1" in
load)
    # pclog (started at init) mounts its partition and runs dmesg -w into it; wait for that
    # (at most 15 s) so the steps below are on the stick even if the kernel dies in one.
    for i in $(seq 1 30); do grep -q ' /mnt/vendor/pclog ' /proc/mounts && break; sleep 0.5; done
    grep -q ' /mnt/vendor/pclog ' /proc/mounts || k "pclog is not mounted, loading anyway"
    k "modules: $( [ "$(getprop ro.boot.android_pc.nvidia_rm)" = gcc ] && echo /vendor/lib/nvidia-gcc '(RM built by gcc)' || echo /vendor/lib/modules)"
    step 1 mp -r vkms.ko
    step 1 mp nvidia.ko && step 1 mp nvidia-modeset.ko && step 1 mp nvidia-drm.ko modeset=1
    step 1 mp nvidia-uvm.ko
    step 0 mp vkms.ko
    nodes
    cards
    ;;
*)
    echo "usage: pcnvidia.sh load" >&2; exit 2 ;;
esac
