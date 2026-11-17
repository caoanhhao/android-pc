#!/system/bin/sh
# Waydroid: the device nodes of NVIDIA's own driver (docs/waydroid-nvidia.md), made at early-init
# by waydroid-nvidia.rc. Waydroid rewrites its LXC configuration at every container start, so
# instead of bind mounts from the host the nodes are made here, in the container's /dev tmpfs,
# from the host's numbers (/proc/devices, /sys/class): /dev/nvidia0, nvidiactl, nvidia-modeset,
# nvidia-uvm, /dev/udmabuf, and NVIDIA's render node as /dev/dri/renderD129 (gralloc.gbm.device).
# The container may make device nodes (CAP_MKNOD, no device cgroup rules); their modes here are the
# container's own and leave the host's nodes alone.

log() { echo "waydroid-nvidia-nodes: $*" > /dev/kmsg; }

node() {  # path mode major minor
    [ -e "$1" ] && return
    mknod -m "$2" "$1" c "$3" "$4" && log "made $1 ($3:$4)" || log "cannot make $1"
}

major() { while read -r num name; do [ "$name" = "$1" ] && echo "$num"; done < /proc/devices; }

nv=$(major nvidia)
if [ -z "$nv" ]; then
    log "the host has no nvidia kernel driver"
    exit 0
fi
node /dev/nvidia0 0666 "$nv" 0
node /dev/nvidiactl 0666 "$nv" 255
node /dev/nvidia-modeset 0666 "$nv" 254
uvm=$(major nvidia-uvm)
[ -n "$uvm" ] && node /dev/nvidia-uvm 0666 "$uvm" 0

if [ -r /sys/class/misc/udmabuf/dev ]; then
    IFS=: read -r ma mi < /sys/class/misc/udmabuf/dev
    node /dev/udmabuf 0666 "$ma" "$mi"
fi

mkdir -p /dev/dri
for d in /sys/class/drm/renderD*; do
    [ "$(cat "$d/device/vendor" 2>/dev/null)" = 0x10de ] || continue
    IFS=: read -r ma mi < "$d/dev"
    node /dev/dri/renderD129 0666 "$ma" "$mi"
    break
done
