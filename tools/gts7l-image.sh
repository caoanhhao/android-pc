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
#   super.img    sparse, the stock super size, one group: system system_ext product vendor, and
#                gts7l_fw (the device firmware, out/gts7l/firmware/gts7l_fw.img of
#                tools/gts7l-firmware.sh) when it exists; GTS7L_FIRMWARE=0 leaves it out.
#                With gts7l_fw, super.img and the Odin tar carry Samsung/Qualcomm firmware of
#                this tablet: keep them on this host, never publish them.
#   recovery.img the bring-up image (tools/gts7l-bringup.sh --firmware with
#                GTS7L_BRINGUP_OUT=out/gts7l/recovery): busybox shell over USB NCM, pstore. An
#                Android init failure reboots there (androidboot.init_fatal_reboot_target).
#                Copied when out/gts7l/recovery/boot.img exists (GTS7L_RECOVERY=FILE: another).
#   vbmeta.img   empty, verification and hashtree disabled (flags 3)
#   dtbo.img     zeroed: ABL finds no overlay table, so the stock board overlays (written for
#                Samsung's 4.19 DT) are not applied onto our mainline DTB, as on gts7xl-mainline
#   AP_gts7l-<build>.tar.md5   the images lz4-compressed for Odin's AP slot. Odin writes
#                only these partitions: userdata, efs and the firmware partitions stay as they are.
#   SHA256SUMS, CHECK.txt   checksums; what the checks at the end found (boot ramdisk, command
#                lines, DTB, super's partitions)
# tools/gts7l-android-flash.sh writes them from the bring-up shell over the USB network.
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
FW_IMG=$K/firmware/gts7l_fw.img
RECOVERY=${GTS7L_RECOVERY:-$K/recovery/boot.img}
RECOVERY_SIZE=86888448   # sda24, 84852 KiB

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
# gts7l_fw is not a Soong partition (it never enters the build): added here, mounted by
# fstab.gts7l at /mnt/vendor/firmware.
if [[ ${GTS7L_FIRMWARE:-1} != 0 && -f $FW_IMG ]]; then
    args+=(--partition "gts7l_fw:readonly:$(stat -c %s "$FW_IMG"):gts7l_dynamic_partitions" --image "gts7l_fw=$FW_IMG")
    echo "gts7l-image: super.img includes gts7l_fw ($FW_IMG)"
else
    echo "gts7l-image: WARNING: no gts7l_fw in super.img (run tools/gts7l-firmware.sh): no GPU/Wi-Fi/BT/DSP firmware" >&2
fi
lpmake "${args[@]}" >/dev/null

# vbmeta.img, dtbo.img
avbtool make_vbmeta_image --flags 3 --padding_size 4096 --output "$OUT/vbmeta.img"
head -c 4096 /dev/zero > "$OUT/dtbo.img"

# recovery.img: the bring-up image.
images=(boot super vbmeta dtbo)
rm -f "$OUT/recovery.img"
if [[ -f $RECOVERY ]]; then
    [[ $(head -c 8 "$RECOVERY") == ANDROID! ]] || { echo "gts7l-image: $RECOVERY is not a boot image" >&2; exit 1; }
    (( $(stat -c %s "$RECOVERY") <= RECOVERY_SIZE )) || { echo "gts7l-image: $RECOVERY larger than recovery" >&2; exit 1; }
    cp "$RECOVERY" "$OUT/recovery.img"
    images+=(recovery)
else
    echo "gts7l-image: WARNING: no $RECOVERY (GTS7L_BRINGUP_OUT=$K/recovery tools/gts7l-bringup.sh --firmware BUSYBOX): no recovery.img" >&2
fi

# Checks: the boot ramdisk, the command lines, the DTB, super's partitions.
check() {
    local u=$tmp/unpack c=$OUT/CHECK.txt want f
    rm -rf "$u"; mkdir -p "$u/rd"
    unpack_bootimg --boot_img "$OUT/boot.img" --out "$u" > "$u/info.txt"
    {
        echo "boot.img: $(sed -n 's/^boot image header version: */header v/p' "$u/info.txt"), $(stat -c %s "$OUT/boot.img") bytes"
        cmp -s "$u/dtb" "$K/sm8250-samsung-gts7l.dtb" && echo "dtb: = sm8250-samsung-gts7l.dtb" ||
            { echo "FAIL dtb differs from $K/sm8250-samsung-gts7l.dtb"; }
        cmp -s "$u/kernel" "$K/Image.gz" && echo "kernel: = Image.gz ($(cat "$K/kernelrelease" 2>/dev/null))" ||
            echo "FAIL kernel differs from $K/Image.gz"
        echo "cmdline (boot.img): $(sed -n 's/^command line args: //p' "$u/info.txt")"
        # The kernel uses its own built-in command line (CONFIG_CMDLINE_FORCE): it must carry
        # what Android needs here.
        local kc; kc=$(sed -n 's/^CONFIG_CMDLINE="\(.*\)"$/\1/p' "$K/config")
        echo "cmdline (kernel, forced: $(grep -c '^CONFIG_CMDLINE_FORCE=y' "$K/config")): $kc"
        for want in androidboot.hardware=gts7l androidboot.boot_devices=soc@0/1d84000.ufshc \
            androidboot.usbcontroller=a600000.usb androidboot.selinux=permissive \
            androidboot.init_fatal_reboot_target=recovery; do
            [[ " $kc " == *" $want "* ]] && echo "  kernel cmdline has $want" || echo "FAIL kernel cmdline lacks $want"
        done
        for want in CONFIG_PSTORE_RAM=y CONFIG_PSTORE_CONSOLE=y CONFIG_PSTORE_PMSG=y \
            CONFIG_USB_CONFIGFS_F_FS=y CONFIG_USB_CONFIGFS_NCM=y CONFIG_F2FS_FS=y CONFIG_BLK_DEV_DM=y; do
            grep -qx "$want" "$K/config" && echo "  kernel $want" || echo "FAIL kernel config lacks $want"
        done
        # The ramdisk is two concatenated newc archives (cpio stops at the first trailer).
        lz4 -dc "$u/ramdisk" | python3 -I -c '
import os, sys
d, out, i = sys.stdin.buffer.read(), sys.argv[1], 0
while i + 110 <= len(d):
    if d[i:i + 6] != b"070701": i += 1; continue
    f = [int(d[i + 6 + 8 * k:i + 14 + 8 * k], 16) for k in range(13)]
    mode, size, nlen = f[1], f[6], f[11]
    name = d[i + 110:i + 110 + nlen - 1].decode()
    i = (i + 110 + nlen + 3) & ~3
    data = d[i:i + size]; i = (i + size + 3) & ~3
    if name == "TRAILER!!!" or name == ".": continue
    p = os.path.join(out, name)
    if mode & 0o170000 == 0o040000: os.makedirs(p, exist_ok=True)
    elif mode & 0o170000 == 0o120000:
        os.makedirs(os.path.dirname(p), exist_ok=True)
        if not os.path.lexists(p): os.symlink(data.decode(), p)
    else:
        os.makedirs(os.path.dirname(p), exist_ok=True); open(p, "wb").write(data)
' "$u/rd"
        for f in init system/bin/init fstab.gts7l first_stage_ramdisk/fstab.gts7l; do
            [[ -e $u/rd/$f || -L $u/rd/$f ]] && echo "  ramdisk has /$f" || echo "  ramdisk has no /$f"
        done
        [[ -e $u/rd/init && -e $u/rd/fstab.gts7l ]] || echo "FAIL ramdisk lacks init or fstab.gts7l"
        cmp -s "$u/rd/fstab.gts7l" "$WT/aosp-device/gts7l/fstab.gts7l" || echo "FAIL ramdisk fstab.gts7l differs"
        echo "super.img:"
        simg2img "$OUT/super.img" "$tmp/super.raw"
        lpdump "$tmp/super.raw" | sed -n 's/^  Name: /  partition /p'
        for f in $parts gts7l_fw; do
            lpdump "$tmp/super.raw" | grep -qx "  Name: $f" || echo "FAIL super has no $f"
        done
        rm -f "$tmp/super.raw"
        [[ -f $OUT/recovery.img ]] && {
            unpack_bootimg --boot_img "$OUT/recovery.img" --out "$u/r" > "$u/r.txt"
            echo "recovery.img: $(sed -n 's/^boot image header version: */header v/p' "$u/r.txt"), $(stat -c %s "$OUT/recovery.img") bytes, ramdisk $(gzip -dc "$u/r/ramdisk" | cpio -t --quiet 2>/dev/null | grep -c '^lib/firmware/') firmware entries"
        }
    } | tee "$c"
    ! grep -q '^FAIL' "$c"
}
check || { echo "gts7l-image: checks failed, see $OUT/CHECK.txt" >&2; exit 1; }

# Odin AP tar
build=$(prop ro.build.version.incremental)
for f in "${images[@]}"; do lz4 -q -f -B6 --content-size "$OUT/$f.img" "$tmp/$f.img.lz4"; done
rm -f "$OUT"/AP_gts7l-*.tar.md5
tar=$OUT/AP_gts7l-$build.tar
tar -C "$tmp" -H ustar -cf "$tar" "${images[@]/%/.img.lz4}"
md5sum -t "$tar" | sed "s|$OUT/||" >> "$tar"
mv "$tar" "$tar.md5"
(cd "$OUT" && sha256sum "${images[@]/%/.img}" "${tar##*/}.md5" > SHA256SUMS)
echo "$build" > "$OUT/BUILD"
ls -la "$OUT"
echo "gts7l-image: build $build"
