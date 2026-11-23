#!/bin/bash
# Build and pack the flashable images of aosp_gts7l, the android-pc build for the Galaxy Tab S7
# LTE (SM-T875), step S4 of docs/gts7l.md.
#
#   tools/gts7l-image.sh            build the partition images, then pack
#   tools/gts7l-image.sh pack       pack only (images already built)
#
# Needs the kernel from tools/gts7l-kernel.sh in out/gts7l/ of the main checkout. Builds go
# through tools/aosp17-build.sh (one step at a time, tools/limited-build.sh, the shared lock).
# Output in out/gts7l/images/ of the main checkout:
#   boot.img     header v2 like stock: Image.gz, the generic ramdisk, the gts7l DTB in the dtb
#                section, stock offsets and our command line (BoardConfig.mk)
#   super.img    sparse, the stock super size, one group: system system_ext product vendor
#   vbmeta.img   empty, verification and hashtree disabled (flags 3)
#   dtbo.img     zeroed: ABL finds no overlay table, so the stock board overlays (written for
#                Samsung's 4.19 DT) are not applied onto our mainline DTB, as on gts7xl-mainline
#   AP_gts7l-<build>.tar.md5   the four images lz4-compressed for Odin's AP slot. Odin writes
#                only these partitions: userdata, efs and the firmware partitions stay as they are.
set -euo pipefail
WT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT=$WT
[[ -d $ROOT/aosp ]] || ROOT="$(cd "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
K=$ROOT/out/gts7l
OUT=$K/images
P=$ROOT/aosp/out/target/product/gts7l
# Host tools from the AOSP build, the system ones (simg2img) otherwise.
export PATH=$ROOT/aosp/out/host/linux-x86/bin:$PATH
BOARD=$WT/aosp-device/gts7l/BoardConfig.mk
RAMDISK=$ROOT/aosp/out/soong/.intermediates/build/soong/fsgen/aosp_gts7l_generated_ramdisk_image/android_common/ramdisk.img

for f in Image.gz sm8250-samsung-gts7l.dtb; do
    [[ -f $K/$f ]] || { echo "gts7l-image: $K/$f missing, run tools/gts7l-kernel.sh first" >&2; exit 1; }
done

if [[ ${1:-} != pack ]]; then
    # boot.img gives the generic ramdisk; the flashable boot.img is packed below.
    LOCK=$ROOT/out/.phase-e.lock
    flock "$LOCK" env PRODUCT=aosp_gts7l \
        STEPS="${STEPS:-system.img system_ext.img product.img vendor.img}" \
        "$WT/tools/aosp17-build.sh"
    # The Soong-only build's boot.img is GKI-style and carries no ramdisk (that goes to
    # init_boot); the generated ramdisk module has no phony target, so build its file.
    flock "$LOCK" bash -c 'cd "$1/aosp" && source build/envsetup.sh >/dev/null &&
        lunch aosp_gts7l-cp2a-userdebug >/dev/null &&
        SOONG_GOMEMLIMIT=14GiB SOONG_INCREMENTAL_ANALYSIS=false NINJA_ARGS= \
        "$1/tools/limited-build.sh" m -j8 "${2#$1/aosp/}"' _ "$ROOT" "$RAMDISK"
fi

# Board values, read from BoardConfig.mk so the two never disagree.
board() { sed -n "s/^$1 *:= *//p" "$BOARD" | head -1; }
cmdline=$(sed -n '/^BOARD_KERNEL_CMDLINE := \\$/,/^[^ ]/p' "$BOARD" | sed -n 's/^ \+\([^ ]*\).*/\1/p' | tr '\n' ' ')
grep -q '^GTS7L_SELINUX_PERMISSIVE ?= true' "$BOARD" && [[ ${GTS7L_SELINUX_PERMISSIVE:-true} == true ]] &&
    cmdline+="androidboot.selinux=permissive"
super_size=$(board BOARD_SUPER_PARTITION_SIZE)
group_size=$(board BOARD_GTS7L_DYNAMIC_PARTITIONS_SIZE)
boot_size=$(board BOARD_BOOTIMAGE_PARTITION_SIZE)
parts=$(board BOARD_GTS7L_DYNAMIC_PARTITIONS_PARTITION_LIST)

# The Soong-only build stages nothing under $P/system: read build.prop from the image.
prop() { debugfs -R 'cat /system/build.prop' "$P/system.img" 2>/dev/null | sed -n "s/^$1=//p"; }

mkdir -p "$OUT"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

# boot.img. The generated ramdisk lacks the ramdisk PRODUCT_COPY_FILES, so fstab.gts7l (for
# first-stage mount, found as /fstab.<androidboot.fstab_suffix>) is appended as a second cpio
# archive; the kernel unpacks both.
[[ -f $RAMDISK ]] || { echo "gts7l-image: $RAMDISK missing" >&2; exit 1; }
mkdir -p "$tmp/frag"
install -m 0644 "$WT/aosp-device/gts7l/fstab.gts7l" "$tmp/frag/fstab.gts7l"
lz4 -dc "$RAMDISK" > "$tmp/ramdisk.cpio"
(cd "$tmp/frag" && echo fstab.gts7l | cpio -o -H newc -R 0:0 --quiet) >> "$tmp/ramdisk.cpio"
lz4 -l -12 -q -f "$tmp/ramdisk.cpio" "$tmp/ramdisk"
mkbootimg --header_version 2 --os_version 17.0.0 \
    --os_patch_level "$(prop ro.build.version.security_patch | cut -c1-7)" \
    --kernel "$K/Image.gz" --ramdisk "$tmp/ramdisk" --dtb "$K/sm8250-samsung-gts7l.dtb" \
    --base 0x00000000 --pagesize 4096 --kernel_offset 0x00008000 --ramdisk_offset 0x02000000 \
    --tags_offset 0x01e00000 --dtb_offset 0x01f00000 \
    --cmdline "${cmdline% }" --output "$OUT/boot.img"
(( $(stat -c %s "$OUT/boot.img") <= boot_size )) || { echo "gts7l-image: boot.img larger than $boot_size" >&2; exit 1; }

# super.img
args=(--metadata-size 65536 --super-name super --metadata-slots 2 --device "super:$super_size"
      --group "gts7l_dynamic_partitions:$group_size" --sparse --output "$OUT/super.img")
for p in $parts; do
    img=$P/$p.img
    [[ -f $img ]] || { echo "gts7l-image: $img missing" >&2; exit 1; }
    # Partition images may be sparse; lpmake wants the real size.
    if [[ $(od -An -tx4 -N4 "$img" | tr -d ' ') == ed26ff3a ]]; then
        simg2img "$img" "$tmp/$p.raw"; img=$tmp/$p.raw
    fi
    args+=(--partition "$p:readonly:$(stat -c %s "$img"):gts7l_dynamic_partitions" --image "$p=$img")
done
lpmake "${args[@]}" >/dev/null

# vbmeta.img, dtbo.img
avbtool make_vbmeta_image --flags 3 --padding_size 4096 --output "$OUT/vbmeta.img"
head -c 4096 /dev/zero > "$OUT/dtbo.img"

# Odin AP tar
build=$(prop ro.build.version.incremental)
for f in boot super vbmeta dtbo; do lz4 -q -f -B6 --content-size "$OUT/$f.img" "$tmp/$f.img.lz4"; done
tar=$OUT/AP_gts7l-$build.tar
tar -C "$tmp" -H ustar -cf "$tar" boot.img.lz4 super.img.lz4 vbmeta.img.lz4 dtbo.img.lz4
md5sum -t "$tar" | sed "s|$OUT/||" >> "$tar"
mv "$tar" "$tar.md5"
(cd "$OUT" && sha256sum boot.img super.img vbmeta.img dtbo.img "${tar##*/}.md5" > SHA256SUMS)
ls -la "$OUT"
echo "gts7l-image: build $build"
