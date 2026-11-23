#!/bin/bash
# Pack the gts7l bring-up boot image (docs/gts7l.md, S6): our kernel with the gts7l DTB and a
# busybox initramfs that gives a shell over USB (ACM /dev/ttyACM0, NCM 172.16.42.1 + telnet)
# and keeps logs in pstore. It never mounts UFS.
#
#   tools/gts7l-bringup.sh BUSYBOX     BUSYBOX: a static arm64 busybox binary
#
# Output: out/gts7l/bringup/boot.img of the main checkout. Boot image layout as on
# gts7xl-mainline: header v1, Image.gz with the DTB appended, base 0x10000000, page 2048.
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
BUSYBOX=${1:?usage: tools/gts7l-bringup.sh BUSYBOX}
K=$ROOT/out/gts7l
OUT=$K/bringup
SRC=$WT/aosp-device/gts7l/bringup
export PATH=$ROOT/aosp/out/host/linux-x86/bin:$PATH
file "$BUSYBOX" | grep -q 'ARM aarch64.*statically linked' || { echo "gts7l-bringup: $BUSYBOX is not a static arm64 binary" >&2; exit 1; }

mkdir -p "$OUT"
"$ROOT/tools/limited-build.sh" cargo build --release --manifest-path "$SRC/usbroute/Cargo.toml" \
    --target aarch64-unknown-linux-musl --target-dir "$OUT/cargo" \
    --config 'target.aarch64-unknown-linux-musl.linker="rust-lld"' -j8

tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/rd"/{bin,dev,proc,sys,tmp}
install -m 0755 "$SRC/init" "$tmp/rd/init"
install -m 0755 "$BUSYBOX" "$tmp/rd/bin/busybox"
install -m 0755 "$OUT/cargo/aarch64-unknown-linux-musl/release/usbroute" "$tmp/rd/bin/usbroute"
(cd "$tmp/rd" && find . | sort | cpio -o -H newc -R 0:0 --quiet | gzip -9n) > "$OUT/ramdisk.cpio.gz"
cat "$K/Image.gz" "$K/sm8250-samsung-gts7l.dtb" > "$tmp/kernel-dtb"
mkbootimg --header_version 1 --pagesize 2048 --base 0x10000000 --kernel_offset 0x00008000 \
    --ramdisk_offset 0x01000000 --tags_offset 0x00000100 --os_version 17.0.0 --os_patch_level 2026-06 \
    --kernel "$tmp/kernel-dtb" --ramdisk "$OUT/ramdisk.cpio.gz" --output "$OUT/boot.img"
(cd "$OUT" && sha256sum boot.img > boot.img.sha256)
ls -la "$OUT/boot.img"; cat "$OUT/boot.img.sha256"
