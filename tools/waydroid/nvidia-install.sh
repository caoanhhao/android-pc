#!/usr/bin/env bash
# Install NVIDIA's own Vulkan driver into Waydroid (docs/waydroid-nvidia.md), or take it out.
#
#   tools/waydroid/nvidia-install.sh [OVERLAY]    OVERLAY: out/waydroid-nvidia/overlay
#   tools/waydroid/nvidia-install.sh --uninstall  restore the newest backup
#
# Run it as the user, not with sudo: it asks for the password itself (sudo -v) and runs only the
# Waydroid commands as root. Before changing anything it backs up waydroid.cfg
# and every overlay file it replaces into /var/lib/waydroid/nvidia-backup/<date>/, with the list of
# files it adds (added.txt), which --uninstall removes again. Run again over an installed driver,
# it updates the files and keeps that first backup.
#
# What it changes:
#   - /var/lib/waydroid/overlay: tools/waydroid/nvidia-build.sh's files
#     (with /vendor/etc/init/waydroid-nvidia.rc, which makes /dev/nvidia{0,ctl,-modeset,-uvm},
#     /dev/udmabuf and the RTX 4070's render node as /dev/dri/renderD129 inside the container:
#     Waydroid rewrites its LXC configuration at every start, so bind mounts would not stay)
#   - waydroid.cfg [properties]: ro.hardware.vulkan=nvidia, gralloc.gbm.device (the NVIDIA node),
#     ro.zygote.disable_gl_preload=true, debug.hwui.renderer=skiavk,
#     debug.nvvk.pipeline_cache_max=262144; the vtest gralloc switch off
# then `waydroid upgrade --offline` and a restart of waydroid-container.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WD=/var/lib/waydroid
CFG=$WD/waydroid.cfg
BACKUPS=$WD/nvidia-backup
MARKER=$WD/overlay/vendor/lib64/hw/vulkan.nvidia.so  # installed by this script
NODE_IN_CONTAINER=/dev/dri/renderD129  # aosp-device/waydroid/init/waydroid-nvidia-nodes.sh

[[ $EUID -ne 0 ]] || { echo "run as your user; it asks for sudo itself" >&2; exit 1; }
sudo -v

set_prop() {  # waydroid refuses a cfg with a key twice
    sudo sed -i "/^$1[[:space:]]*=/d" "$CFG"
    sudo sed -i "/^\[properties\]/a $1 = $2" "$CFG"
}
del_prop() { sudo sed -i "/^$1[[:space:]]*=/d" "$CFG"; }

restart() {
    echo "== waydroid upgrade --offline, restart waydroid-container"
    sudo waydroid upgrade --offline
    sudo systemctl restart waydroid-container.service
}

if [[ ${1:-} == --uninstall ]]; then
    b=$(sudo sh -c "ls -1d $BACKUPS/*/ 2>/dev/null | tail -1")
    [[ -n $b ]] || { echo "no backup in $BACKUPS" >&2; exit 1; }
    echo "== restoring $b"
    sudo systemctl stop waydroid-container.service
    sudo xargs -r -a "$b/added.txt" rm -f --
    sudo cp -a "$b/overlay/." "$WD/overlay/"
    sudo cp -a "$b/waydroid.cfg" "$CFG"
    restart
    exit 0
fi

OV=$(realpath "${1:-$ROOT/out/waydroid-nvidia/overlay}")
[[ -f $OV/vendor/lib64/hw/vulkan.nvidia.so ]] || {
    echo "no overlay in $OV: run tools/waydroid/nvidia-build.sh" >&2; exit 1; }
grep -qw nvidia /proc/devices || { echo "the host has no nvidia kernel driver loaded" >&2; exit 1; }
[[ -c /dev/udmabuf ]] || { echo "no /dev/udmabuf (modprobe udmabuf)" >&2; exit 1; }
nv_node=
for d in /sys/class/drm/renderD*; do [[ $(cat "$d/device/vendor") == 0x10de ]] && nv_node=$d; done
[[ -n $nv_node ]] || {
    echo "the NVIDIA GPU has no render node: is it on nvidia (not vfio-pci)? " \
         "tools/run-qemu-aosp17-vfio.sh --release gives it back" >&2; exit 1; }

if sudo test -e "$MARKER"; then
    b=$(sudo sh -c "ls -1d $BACKUPS/*/ 2>/dev/null | tail -1")
    echo "== already installed: updating, the backup stays $b"
    # files new in this version: an overlay file it replaces for the first time is backed up,
    # one it adds is listed
    (cd "$OV" && find . -type f) | while read -r f; do
        f=${f#./}
        if sudo grep -qxF "$WD/overlay/$f" "$b/added.txt" || sudo test -e "$b/overlay/$f"; then
            continue
        elif sudo test -e "$WD/overlay/$f"; then
            sudo install -D -p "$WD/overlay/$f" "$b/overlay/$f"
        else
            echo "$WD/overlay/$f" | sudo tee -a "$b/added.txt" >/dev/null
        fi
    done
else
    b=$BACKUPS/$(date +%Y%m%d-%H%M%S)
    echo "== backup to $b"
    sudo mkdir -p "$b/overlay"
    sudo cp -a "$CFG" "$b/waydroid.cfg"
    added=$(mktemp)
    (cd "$OV" && find . -type f) | while read -r f; do
        f=${f#./}
        if sudo test -e "$WD/overlay/$f"; then
            sudo install -D -p "$WD/overlay/$f" "$b/overlay/$f"
        else
            echo "$WD/overlay/$f" >>"$added"
        fi
    done
    sudo install -m 644 "$added" "$b/added.txt"
    /usr/bin/rm -f "$added"
fi

sudo systemctl stop waydroid-container.service

echo "== overlay from $OV"
(cd "$OV" && find . -type f) | while read -r f; do
    f=${f#./}
    mode=644
    [[ $f == vendor/bin/* ]] && mode=755
    sudo install -D -o root -g root -m $mode "$OV/$f" "$WD/overlay/$f"
done

echo "== waydroid.cfg properties"
sudo sed -i 's/^mount_overlays = .*/mount_overlays = True/' "$CFG"
set_prop ro.hardware.vulkan nvidia
set_prop ro.hardware.egl angle
set_prop gralloc.gbm.device "$NODE_IN_CONTAINER"
set_prop ro.zygote.disable_gl_preload true
set_prop debug.hwui.renderer skiavk
# Android 13's SurfaceFlinger has no Vulkan RenderEngine (skiavk is not recognised): it draws
# through ANGLE. An earlier version of this script set it; take it out.
del_prop debug.renderengine.backend
# Android 13's HWUI crashes storing a large pipeline cache (vulkan.nvidia's GetPipelineCacheData)
set_prop debug.nvvk.pipeline_cache_max 262144
del_prop waydroid.gralloc.vtest
restart
echo "installed; backup in $b (--uninstall restores it)"
