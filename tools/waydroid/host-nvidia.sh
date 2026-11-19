#!/bin/bash
# Waydroid with the AOSP 17 image: use the host's NVIDIA user-space driver instead of the copy
# in the image (docs/waydroid-aosp17.md, "NVIDIA driver from the host").
#
# The container shares the host's kernel, so NVIDIA's kernel module is the host's, and NVIDIA's
# kernel module accepts only the user-space driver of its own version. The image carries
# NVIDIA's user-space libraries in /vendor/lib64/nvidia, unmodified copies of one version
# (620.70.06). This script binds the host's own copies of the same libraries over that
# directory at every container start, so the two always match, also after a driver update.
# When the host has no NVIDIA module or not the libraries of its version, nothing is bound and
# the image's copies are used.
#
#   host-nvidia.sh install   add the LXC hook to Waydroid's config template and to the current
#                            container config, and a pacman hook that adds it again after
#                            every waydroid package update
#   host-nvidia.sh remove    take it out again (the image's copies are used)
#   host-nvidia.sh check     exit 1 unless every config has it
#   host-nvidia.sh apply     (root, used by the pacman hook) add it where missing
#   host-nvidia.sh prestart  (root, LXC pre-start hook) prepare the host's libraries
# Run as your user: it asks for the sudo password itself.
set -euo pipefail

TEMPLATES=/usr/lib/waydroid/data/configs
LXC=/var/lib/waydroid/lxc/waydroid
ROOTFS=/var/lib/waydroid/rootfs
DST=/var/lib/waydroid/host-nvidia
INSTALLED=/usr/local/lib/android-pc/waydroid-host-nvidia.sh
HOOK=/etc/pacman.d/hooks/waydroid-host-nvidia.hook
BEGIN="# android-pc host NVIDIA driver begin"
END="# android-pc host NVIDIA driver end"
SUDO=sudo
[ "$(id -u)" = 0 ] && SUDO=

die() { echo "host-nvidia: $*" >&2; exit 1; }

# The pre-start hook runs on the host before LXC makes the mount entries; the entry is optional,
# so with no $DST the image's directory stays.
config_block() {
    cat <<EOF
$BEGIN
lxc.hook.pre-start = $INSTALLED prestart
lxc.mount.entry = $DST vendor/lib64/nvidia none bind,ro,optional 0 0
$END
EOF
}

has_block() { grep -qxF "$BEGIN" "$1" 2>/dev/null; }

add_block() {
    local f=$1
    [ -f "$f" ] || return 0
    has_block "$f" && return 0
    { echo; config_block; } >> "$f"
    echo "host-nvidia: added to $f"
}

del_block() {
    local f=$1
    has_block "$f" || return 0
    sed -i "/^$BEGIN\$/,/^$END\$/d" "$f"
    echo "host-nvidia: removed from $f"
}

# The libraries of /vendor/lib64/nvidia (tools/nvidia-userspace-build.sh), each under its
# soname: NVIDIA's of the module's version, and GLVND's libEGL and libGLdispatch.
prestart() {
    local ver lib src
    /usr/bin/rm -rf "$DST" "$DST.tmp"
    ver=$(cat /sys/module/nvidia/version 2>/dev/null) || return 0
    [ -f "/usr/lib/libnvidia-glcore.so.$ver" ] || return 0
    # Only the android-pc image (its vendor has the Waydroid bind list): LineageOS 20 with
    # tools/waydroid/nvidia-install.sh carries NVIDIA's libraries rewritten for its own loader.
    [ -f "$ROOTFS/vendor/etc/waydroid/bind-mounts" ] && [ -d "$ROOTFS/vendor/lib64/nvidia" ] || return 0
    mkdir -p "$DST.tmp"
    for lib in libGLX_nvidia.so.0 libEGL_nvidia.so.0 libnvidia-allocator.so.1 \
               libnvidia-glcore.so.$ver libnvidia-glsi.so.$ver libnvidia-eglcore.so.$ver \
               libnvidia-tls.so.$ver libnvidia-glvkspirv.so.$ver libnvidia-gpucomp.so.$ver \
               libnvidia-rtcore.so.$ver libEGL.so.1 libGLdispatch.so.0; do
        src=$(readlink -f "/usr/lib/$lib") && [ -f "$src" ] || {
            echo "host-nvidia: no /usr/lib/$lib, using the image's driver" >&2
            /usr/bin/rm -rf "$DST.tmp"
            return 0
        }
        # A hard link when /usr and /var are one filesystem: no copy, and a driver update
        # replaces the files in /usr/lib rather than changing them.
        ln "$src" "$DST.tmp/$lib" 2>/dev/null || cp "$src" "$DST.tmp/$lib"
    done
    # Everything else in the image's directory (the EGL vendor JSON).
    for src in "$ROOTFS"/vendor/lib64/nvidia/*; do
        case ${src##*/} in lib*.so*) ;; *) cp "$src" "$DST.tmp/" ;; esac
    done
    echo "nvidia $ver (host)" > "$DST.tmp/VERSION"
    mv "$DST.tmp" "$DST"
}

apply() {
    [ "$(id -u)" = 0 ] || die "apply runs as root"
    add_block "$TEMPLATES/config_base"
    add_block "$LXC/config"
}

check() {
    local f bad=0
    for f in "$TEMPLATES/config_base" "$LXC/config"; do
        if $SUDO test -f "$f" && ! $SUDO grep -qxF "$BEGIN" "$f"; then
            echo "host-nvidia: MISSING in $f" >&2
            bad=1
        fi
    done
    [ -f "$HOOK" ] || { echo "host-nvidia: MISSING $HOOK" >&2; bad=1; }
    [ $bad = 0 ] && echo "host-nvidia: present"
    return $bad
}

install_hook() {
    $SUDO install -D -m 755 "$0" "$INSTALLED"
    $SUDO mkdir -p "$(dirname "$HOOK")"
    $SUDO tee "$HOOK" >/dev/null <<EOF
# android-pc: put the host NVIDIA driver hook back into Waydroid's config template after every
# waydroid package update (tools/waydroid/host-nvidia.sh in the android-pc repo).
[Trigger]
Operation = Install
Operation = Upgrade
Type = Package
Target = waydroid

[Action]
Description = Adding the android-pc host NVIDIA driver hook to Waydroid's LXC config...
When = PostTransaction
Exec = $INSTALLED apply
EOF
    $SUDO "$INSTALLED" apply
    check
}

remove() {
    $SUDO bash -c "$(declare -f del_block has_block); BEGIN='$BEGIN' END='$END'
        del_block $TEMPLATES/config_base; del_block $LXC/config"
    $SUDO /usr/bin/rm -f "$HOOK" "$INSTALLED"
    $SUDO /usr/bin/rm -rf "$DST"
    echo "host-nvidia: removed; the image's driver is used from the next start"
}

case ${1:-} in
    install) install_hook ;;
    remove) remove ;;
    check) check ;;
    apply) apply ;;
    prestart) prestart ;;
    *) sed -n '2,/^set -e/p' "$0" | sed '$d'; exit 1 ;;
esac
