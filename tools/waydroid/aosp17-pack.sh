#!/bin/bash
# Package the PC build (aosp_pc_x86_64, aosp/out/target/product/pc_x86_64) for Waydroid
# (docs/waydroid-aosp17.md). Waydroid mounts only system.img (as the container's rootfs) and
# vendor.img, so system_ext, product and odm go inside the root of the new system.img, where
# the PC mounts them. vendor.img is the PC's own.
#
# Usage: tools/waydroid/aosp17-pack.sh [OUT_DIR]   (default out/waydroid-aosp17/images)
# Run as your user: it asks for the sudo password itself (loop mounts keep owners, modes and
# file capabilities).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
P=$ROOT/aosp/out/target/product/pc_x86_64
OUT=${1:-$ROOT/out/waydroid-aosp17/images}
MKFS=$ROOT/aosp/out/host/linux-x86/bin/mkfs.erofs
SUDO=sudo
[ "$(id -u)" = 0 ] && SUDO=

for i in system system_ext product odm vendor; do
    [ -f "$P/$i.img" ] || { echo "aosp17-pack: no $P/$i.img (build the PC images first)" >&2; exit 1; }
done
[ -x "$MKFS" ] || { echo "aosp17-pack: no $MKFS" >&2; exit 1; }

work=$(mktemp -d "$ROOT/out/waydroid-aosp17/pack.XXXXXX")
cleanup() {
    for m in "$work"/mnt/*; do mountpoint -q "$m" && $SUDO umount "$m"; done
    $SUDO /usr/bin/rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/mnt" "$OUT"
for i in system system_ext product odm; do
    mkdir -p "$work/mnt/$i"
    $SUDO mount -o loop,ro "$P/$i.img" "$work/mnt/$i"
done

echo "aosp17-pack: copying system, system_ext, product, odm ..."
$SUDO mkdir "$work/root"
$SUDO cp -a "$work/mnt/system/." "$work/root/"
for i in system_ext product odm; do
    # The PC's system image has these as mount points (or symlinks under /system).
    $SUDO rm -rf "$work/root/$i"
    $SUDO mkdir "$work/root/$i"
    $SUDO cp -a "$work/mnt/$i/." "$work/root/$i/"
done

echo "aosp17-pack: writing $OUT/system.img ..."
$SUDO rm -f "$OUT/system.img"
$SUDO "$MKFS" -z lz4hc -T 0 "$OUT/system.img" "$work/root" >/dev/null
$SUDO chown "$(id -u):$(id -g)" "$OUT/system.img"
cp "$P/vendor.img" "$OUT/vendor.img"
ls -l "$OUT"
