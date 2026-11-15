#!/usr/bin/env bash
# Make a USB stick image that boots AOSP 17 (aosp_pc_x86_64) with the stock GKI kernel on
# a real UEFI PC (phase F3a, docs/usb-boot-aosp17.md). Needs no root: it writes an image
# file only; writing it to a stick is the user's step (the script prints the command).
#
#   tools/mkusb-aosp17.sh [options] [OUT.img]     default OUT: out/aosp17-usb.img
#
# The image is GPT:
#   ESP (FAT32 "ANDROIDPC17": Limine as EFI/BOOT/BOOTX64.EFI, boot/limine/limine.conf,
#        android/kernel, android/initrd.img = vendor ramdisk + generic ramdisk + bootconfig,
#        android/build.txt = the build's number and fingerprints),
#   pclog (FAT32 "PCLOG": boot logs written by pclog.sh, readable on any PC),
#   misc, frp, metadata (zeroed; fs_mgr formats metadata and userdata on first boot),
#   boot_a, init_boot_a, vendor_boot_a, vbmeta*_a (the build's images: first-stage init
#   verifies the vbmeta chain itself, in AVB permissive mode since the stick is "unlocked"),
#   super (super.img), userdata (zeroed head, formatted on first boot).
# First-stage init finds the stick by the ESP's partition UUID
# (androidboot.boot_part_uuid), whatever USB port or controller it is on.
#
# Options: --product DIR   images (default aosp/out/target/product/pc_x86_64)
#          --disk DEV      size the image for this USB stick (lsblk size, no root needed;
#                          refused for a non-USB disk or one with mounted partitions):
#                          userdata takes all the space left and the backup GPT sits at the
#                          stick's real end; the printed commands write only the used part
#          --userdata SIZE userdata partition size, MiB (default 4096; ignored with --disk)
#          --resolution WxH GOP mode for Android (default 2560x1080, the user's ultrawide
#                          monitor, in landscape; Limine falls back to another mode if the
#                          monitor lacks it); "auto": Limine picks the EDID preferred mode
#          --density N     androidboot.lcd_density (default 160: 1 dp = 1 px, a desktop-sized
#                          UI for mouse and keyboard; 240 is the earlier, touch-sized one)
#          --append "WORDS" extra kernel command line for every entry
#          --enforcing     boot with SELinux enforcing (default: permissive, so pclog.sh runs)
#          --menu          a Limine menu with debug entries (default: one entry, no menu):
#                          permissive, enforcing, kernel log on screen, no pcfb, NVIDIA
#                          nouveau (Phase G: KMS desktop, VTS run), Nova (G6), NVIDIA (K)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
product=$ROOT/aosp/out/target/product/pc_x86_64
host=$ROOT/aosp/out/host/linux-x86/bin
# Graphics VTS (Phase G2), copied to the ESP when built (m -j12 $vts_tests).
vts_tests="VtsHalGraphicsComposer3_TargetTest VtsHalGraphicsAllocatorAidl_TargetTest VtsHalGraphicsMapperStableC_TargetTest"
userdata_mib=4096 resolution=2560x1080 density=160 extra="" enforcing=0 menu=0 disk="" out=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --product) product="$2"; shift ;;
        --userdata) userdata_mib="$2"; shift ;;
        --resolution) resolution="$2"; shift ;;
        --density) density="$2"; shift ;;
        --append) extra="$2"; shift ;;
        --enforcing) enforcing=1 ;;
        --menu) menu=1 ;;
        --disk) disk="$2"; shift ;;
        -h|--help) sed -n '2,35p' "$0"; exit 0 ;;
        -*) echo "mkusb-aosp17.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
        *) out="$1" ;;
    esac
    shift
done
out=${out:-$ROOT/out/aosp17-usb.img}
LIMINE_DIR="${LIMINE_DIR:-/usr/share/limine}"
for f in "$LIMINE_DIR/BOOTX64.EFI" "$host/unpack_bootimg" "$host/mcopy" "$host/mmd" "$host/fsck.erofs"; do
    [[ -e $f ]] || { echo "mkusb-aosp17.sh: missing $f" >&2; exit 1; }
done
for f in boot init_boot vendor_boot vbmeta vbmeta_system vbmeta_vendor_dlkm vbmeta_system_dlkm super system vendor; do
    [[ -f $product/$f.img ]] || { echo "mkusb-aosp17.sh: no $product/$f.img (build AOSP first)" >&2; exit 1; }
done
command -v sfdisk >/dev/null && command -v mkfs.fat >/dev/null || { echo "mkusb-aosp17.sh: needs sfdisk and mkfs.fat" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
work=$(mktemp -d "$(dirname "$out")/.mkusb-aosp17.XXXXXX")   # on disk: super.img may be unsparsed here
trap 'rm -rf "$work"' EXIT

# --- kernel and initrd from the Android boot images ----------------------------------
"$host/unpack_bootimg" --boot_img "$product/boot.img" --out "$work/boot" >/dev/null
"$host/unpack_bootimg" --boot_img "$product/init_boot.img" --out "$work/init_boot" >/dev/null
"$host/unpack_bootimg" --boot_img "$product/vendor_boot.img" --out "$work/vendor_boot" >/dev/null
# The board's command line (BOARD_KERNEL_CMDLINE) as stored in vendor_boot.
board_cmdline=$("$host/unpack_bootimg" --boot_img "$product/vendor_boot.img" --out "$work/vendor_boot" --format=mkbootimg 2>/dev/null |
    sed -n "s/.*--vendor_cmdline '\([^']*\)'.*/\1/p")
board_cmdline=${board_cmdline/panic=-1/panic=10}   # reboot 10 s after a panic (efi-pstore keeps the log)
board_cmdline=${board_cmdline/firmware_class.path=\/vendor\/etc\//firmware_class.path=/vendor/firmware}

# ESP partition UUID: fixed, so the bootconfig can name it.
esp_uuid=a17c0de0-0017-4c0d-9e00-0000000000e5
cat > "$work/bootconfig.txt" <<EOF
androidboot.android_pc=1
androidboot.force_normal_boot=1
androidboot.android_pc.log=1
androidboot.hardware=cutf_cvm
androidboot.boot_part_uuid=$esp_uuid
androidboot.slot_suffix=_a
androidboot.verifiedbootstate=orange
androidboot.vbmeta.device_state=unlocked
androidboot.fstab_suffix=cf.f2fs.hctr2
androidboot.serialno=ANDROIDPC17
androidboot.lcd_density=$density
androidboot.setupwizard_mode=DISABLED
androidboot.enable_bootanimation=1
androidboot.enable_confirmationui=0
androidboot.serialconsole=0
androidboot.openthread_node_id=1
androidboot.hypervisor.vm.supported=0
androidboot.hypervisor.protected_vm.supported=0
androidboot.hardware.gralloc=minigbm
androidboot.hardware.hwcomposer=ranchu
androidboot.hardware.hwcomposer.display_framebuffer_format=rgba
androidboot.hardware.egl=angle
androidboot.cpuvulkan.version=4202496
androidboot.hardware.angle_feature_overrides_enabled=mapUnspecifiedColorSpaceToPassThrough
androidboot.hardware.angle_feature_overrides_disabled=exposeES32ForTesting
androidboot.vendor.apex.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure
androidboot.vendor.apex.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure
androidboot.vendor.apex.com.android.hardware.secure_element=com.android.hardware.secure_element
androidboot.vendor.apex.com.android.hardware.strongbox=none
androidboot.vendor.apex.com.google.emulated.camera.provider.hal=none
EOF
# initrd = vendor ramdisk + generic ramdisk (each lz4; the kernel unpacks concatenated
# archives) + the bootconfig block the kernel looks for at its end (with "bootconfig" on
# the command line): data, NUL-padded to 4 bytes, u32 size, u32 checksum, "#BOOTCONFIG\n".
cat "$work"/vendor_boot/vendor_ramdisk* "$work/init_boot/ramdisk" > "$work/initrd.img"
python3 -I - "$work/bootconfig.txt" "$work/initrd.img" <<'EOF'
import struct, sys
data = open(sys.argv[1], 'rb').read()
data += b'\0' * (-len(data) % 4 or 4)
with open(sys.argv[2], 'ab') as f:
    f.write(data + struct.pack('<II', len(data), sum(data) & 0xffffffff) + b'#BOOTCONFIG\n')
EOF

# Which build this is, for tools/update-usb-aosp17.sh (it compares the stick's with the new one's).
# Read from the images themselves: $product/system/build.prop is a stale staging copy (Soong
# builds the partition images from its own staging directories).
for i in system vendor; do
    "$host/fsck.erofs" --extract="$work/$i.fs" "$product/$i.img" >/dev/null 2>&1 ||
        { echo "mkusb-aosp17.sh: cannot read $product/$i.img (fsck.erofs)" >&2; exit 1; }
done
{
    grep -E '^ro\.(build\.(version\.(incremental|security_patch)|date)|system\.build\.fingerprint)=' "$work/system.fs/system/build.prop"
    grep -E '^ro\.vendor\.build\.fingerprint=' "$work/vendor.fs/build.prop"
} > "$work/build.txt"
rm -rf "$work/system.fs" "$work/vendor.fs"

# --- Limine ---------------------------------------------------------------------------
# ALSA card 0 must be the HD Audio controller (Cuttlefish's audio HAL plays to card 0):
# snd-usb-audio is built in and would otherwise number a USB microphone (the webcam) first.
pc_cmdline="$board_cmdline pcfb.enable=1 pchvc.enable=1 snd_hda_intel.index=0,1 snd_usb_audio.index=2,3,4,5 $extra"
pc_cmdline=$(echo $pc_cmdline)   # squeeze spaces
# The composer (its vendor APEX), the ranchu composer's display finder and the Vulkan driver
# are per entry (on the command line, not in the shared bootconfig, which would win): ranchu
# with finder "drm" (pcfb) and SwiftShader's Vulkan ("pastel") unless the entry sets its own.
composer_apex=androidboot.vendor.apex.com.android.hardware.graphics.composer
entry() {   # entry NAME EXTRA-CMDLINE [COMMENT]
    local finder=androidboot.hardware.hwcomposer.display_finder_mode=drm
    local composer=$composer_apex=com.android.hardware.graphics.composer.ranchu
    local vulkan=androidboot.hardware.vulkan=pastel
    local gles=androidboot.opengles.version=196609   # ES 3.1 (the Venus probe sets its own, K3)
    local own="$2 $extra"   # the entry's words and --append's: either overrides a default
    [[ $own == *display_finder_mode=* ]] && finder=
    [[ $own == *$composer_apex=* ]] && composer=
    [[ $own == *androidboot.hardware.vulkan=* ]] && vulkan=
    [[ $own == *androidboot.android_pc.vulkan=* ]] && vulkan=   # pcvkprobe chooses (J1)
    [[ $own == *androidboot.opengles.version=* ]] && gles=
    [[ $own == *androidboot.android_pc.vulkan=venus* ]] && gles=   # pcvenusprobe chooses (K3)
    cat <<EOF
/$1
$([[ -n $3 ]] && echo "    comment: $3")
    protocol: linux
    path: boot():/android/kernel
    module_path: boot():/android/initrd.img
$([[ $resolution != auto ]] && echo "    resolution: ${resolution}x32")
    cmdline: $(echo $pc_cmdline $finder $composer $vulkan $gles $2)

EOF
}
permissive=androidboot.selinux=permissive; [[ $enforcing == 1 ]] && permissive=
if [[ $menu == 1 ]]; then
    {
        echo "timeout: 5"
        echo "default_entry: $(( enforcing + 1 ))"
        # H1: the entry picked last boots again (an EFI variable; default_entry until then),
        # so a user who wants the monitor's speakers picks a nouveau entry once.
        echo "remember_last_entry: yes"
        echo
        # The RTX 4070 sends audio over HDMI/DP only when its driver (nouveau, through
        # GSP-RM) enables it with the mode; on pcfb (the GOP framebuffer) the codec never
        # sees the monitor (docs/rust-hals.md, "NVIDIA without a GPU driver").
        nosound="Display on the UEFI framebuffer: no sound from a monitor on the NVIDIA card (pick an NVIDIA nouveau entry for that)"
        sound="NVIDIA driver: sound plays on the monitor (HDMI/DP) when one is connected"
        entry "Android 17 (SELinux permissive)" "androidboot.selinux=permissive" "$nosound"
        entry "Android 17 (SELinux enforcing)" "" "$nosound"
        entry "Android 17, kernel log on screen (slow)" "androidboot.selinux=permissive earlycon=efifb keep_bootcon loglevel=7"
        entry "Android 17, no pcfb display driver" "androidboot.selinux=permissive pcfb.enable=0"
        # Phase G (docs/nvidia.md): the RTX 4070 through nouveau + GSP; pcfb stays off and
        # init.pc.rc loads nouveau. G2: Android on drm_hwcomposer (KMS, native mode, hotplug).
        nv="androidboot.selinux=permissive pcfb.enable=0 androidboot.android_pc.gpu=nouveau"
        drm="$composer_apex=com.android.hardware.graphics.composer.drm_hwcomposer"
        # K3 (docs/nvidia-proprietary.md): the graphics half of the NVIDIA proprietary entry, for
        # its command line next to K1's modules and K4's composer: Vulkan is Venus over K2's vtest
        # socket, GLES is ANGLE on it with ES 3.2 and the Android Extension Pack, HWUI is Skia on
        # Vulkan. pcvenusprobe.sh (init.venus.rc) sets ro.hardware.vulkan and ro.opengles.version:
        # without the socket (QEMU) the entry falls back to SwiftShader and ES 3.1.
        venus="androidboot.android_pc.vulkan=venus androidboot.hardware.guest_hwui_renderer=skiavk"
        entry "Android 17, NVIDIA nouveau" "$nv $drm" "$sound"
        # G3/G4: the whole graphics stack on the GPU. Vulkan is NVK (vulkan.nouveau in the
        # com.google.cf.vulkan APEX) and GLES is ANGLE (androidboot.hardware.egl=angle) on it, so
        # SurfaceFlinger's client composition for drm_hwcomposer, HWUI, Chromium and games all
        # draw on the RTX 4070; SwiftShader (pastel) is not loaded on this entry. The entry
        # above keeps SwiftShader on KMS as the fallback. pcvktest.sh runs vkjson and the
        # dEQP-VK and dEQP-GLES3 smoke groups here once per build (pclog boot-N/vk/, gles/).
        # J1: the entry asks for NVK with androidboot.android_pc.vulkan; pcvkprobe (init.pc.rc,
        # post-fs-data) sets ro.hardware.vulkan to nouveau when NVK creates a device, else to
        # pastel, so the entry falls back to SwiftShader instead of a SurfaceFlinger crash loop.
        entry "Android 17, NVIDIA GPU (NVK + ANGLE)" "$nv $drm androidboot.android_pc.vulkan=nouveau" "$sound"
        # G2 check: the VTS composer/allocator/mapper subsets from the ESP (pclog.sh; Android
        # restarts during the composer test). G1's "modetest only" entry passed and was removed
        # in I4; --append "androidboot.android_pc.kms=modetest ..." still starts pcgpu.sh.
        entry "Android 17, NVIDIA nouveau, VTS run" "$nv $drm androidboot.android_pc.vts=1"
        # G6: NVIDIA's Rust drivers Nova (nova-core + nova DRM), an experiment: Android stays on
        # pcfb (Nova has no display yet), nouveau is not loaded, pcnova.sh loads Nova after boot.
        entry "Android 17, NVIDIA Nova (Rust driver experiment)" "androidboot.selinux=permissive androidboot.android_pc.gpu=nova"
        # Phase K (docs/nvidia-proprietary.md): NVIDIA's own driver. K1: the open kernel modules
        # (init.pc.rc loads nvidia-drm modeset=1 and nvidia-uvm), nouveau and Nova blacklisted,
        # pcfb off, drm_hwcomposer on nvidia-drm with SwiftShader until K3/K4. cfi=kcfi: the
        # DisplayPort library in nvidia-modeset is C++, whose virtual calls clang does not kCFI-check;
        # FineIBT (the default on this CPU) removes the ENDBR they land on.
        nvp="androidboot.selinux=permissive pcfb.enable=0 androidboot.android_pc.gpu=nvidia cfi=kcfi module_blacklist=nouveau,nova_core,nova"
        entry "Android 17, NVIDIA (proprietary, Venus)" "$nvp $drm" "$sound"
    } > "$work/limine.conf"
else
    { echo "timeout: 0"; echo; entry "Android 17" "$permissive"; } > "$work/limine.conf"
fi

# --- partitions -------------------------------------------------------------------------
# name size(MiB) type source   (source: file to copy, "fat:LABEL", or "zero")
ESP=C12A7328-F81F-11D2-BA4B-00A0C93EC93B LINUX=0FC63DAF-8483-4772-8E79-3D69D8477DE4
MSDATA=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7
mib() {   # size in MiB, rounded up; for an Android sparse image, its unpacked size
    python3 -I -c '
import os, struct, sys
with open(sys.argv[1], "rb") as f:
    magic, _, _, _, _, blk_sz, total_blks = struct.unpack("<IHHHHII", f.read(20))
size = blk_sz * total_blks if magic == 0xed26ff3a else os.path.getsize(sys.argv[1])
print((size + 1048575) // 1048576)' "$1"
}
parts=(
    "esp 256 $ESP fat:ANDROIDPC17"
    "pclog 256 $MSDATA fat:PCLOG"
    "misc 1 $LINUX zero"
    "frp 1 $LINUX zero"
    "metadata 64 $LINUX zero"
    "vbmeta_a 1 $LINUX $product/vbmeta.img"
    "vbmeta_system_a 1 $LINUX $product/vbmeta_system.img"
    "vbmeta_vendor_dlkm_a 1 $LINUX $product/vbmeta_vendor_dlkm.img"
    "vbmeta_system_dlkm_a 1 $LINUX $product/vbmeta_system_dlkm.img"
    "boot_a $(mib "$product/boot.img") $LINUX $product/boot.img"
    "init_boot_a $(mib "$product/init_boot.img") $LINUX $product/init_boot.img"
    "vendor_boot_a $(mib "$product/vendor_boot.img") $LINUX $product/vendor_boot.img"
    "super $(mib "$product/super.img") $LINUX $product/super.img"
    "userdata $userdata_mib $LINUX zero"
)
total=2   # MiB: 1 before the first partition, 1 for the backup GPT
for p in "${parts[@]}"; do set -- $p; total=$((total + $2)); done
if [[ -n $disk ]]; then
    # Only a USB stick with nothing mounted: the printed dd commands erase it.
    [[ -b $disk && $(lsblk -dno TYPE "$disk") == disk ]] || { echo "mkusb-aosp17.sh: $disk is not a whole disk" >&2; exit 1; }
    [[ $(lsblk -dno TRAN "$disk") == usb ]] || { echo "mkusb-aosp17.sh: $disk is not a USB disk" >&2; exit 1; }
    if lsblk -no MOUNTPOINTS "$disk" | grep -q .; then
        echo "mkusb-aosp17.sh: $disk has mounted partitions" >&2; exit 1
    fi
    disk_mib=$(( $(lsblk -bndo SIZE "$disk") / 1048576 ))
    userdata_mib=$(( disk_mib - total + userdata_mib ))
    (( userdata_mib >= 1024 )) || { echo "mkusb-aosp17.sh: $disk ($disk_mib MiB) is too small" >&2; exit 1; }
    parts[-1]="userdata $userdata_mib $LINUX zero"
    total=$disk_mib
fi
need_gb=$(( (total * 1048576 / 1000000000) + 1 ))
mkdir -p "$(dirname "$out")"
rm -f "$out"
truncate -s "${total}M" "$out"
{
    echo "label: gpt"
    echo "first-lba: 2048"
    for p in "${parts[@]}"; do
        set -- $p
        uuid=""; [[ $1 == esp ]] && uuid=", uuid=$esp_uuid"
        echo "size=${2}MiB, type=$3, name=$1$uuid"
    done
} | sfdisk --quiet "$out"

start_of() { sfdisk --json "$out" | python3 -I -c "
import json,sys
for p in json.load(sys.stdin)['partitiontable']['partitions']:
    if p['name'] == sys.argv[1]: print(p['start'] * 512 // 1048576)" "$1"; }

i=0
for p in "${parts[@]}"; do
    set -- $p
    off=$(start_of "$1")
    case $4 in
        fat:*)
            fat=$work/$1.fat
            truncate -s "${2}M" "$fat"
            mkfs.fat -F 32 -n "${4#fat:}" "$fat" >/dev/null
            if [[ $1 == esp ]]; then
                export MTOOLS_SKIP_CHECK=1
                "$host/mmd" -i "$fat" ::/EFI ::/EFI/BOOT ::/boot ::/boot/limine ::/android
                "$host/mcopy" -i "$fat" "$LIMINE_DIR/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
                "$host/mcopy" -i "$fat" "$work/limine.conf" ::/boot/limine/limine.conf
                "$host/mcopy" -i "$fat" "$work/boot/kernel" ::/android/kernel
                "$host/mcopy" -i "$fat" "$work/initrd.img" ::/android/initrd.img
                "$host/mcopy" -i "$fat" "$work/bootconfig.txt" ::/android/bootconfig.txt
                "$host/mcopy" -i "$fat" "$work/build.txt" ::/android/build.txt
                # Phase G2: the graphics VTS binaries, run by pclog.sh on the "VTS run" entry
                # (the ESP is rewritten by every update, unlike pclog).
                vts=$(for t in $vts_tests; do ls "$product"/data/nativetest64/$t/$t 2>/dev/null; done)
                if [[ -n $vts ]]; then
                    "$host/mmd" -i "$fat" ::/android/vts
                    for t in $vts; do "$host/mcopy" -i "$fat" "$t" ::/android/vts/; done
                fi
            fi
            dd if="$fat" of="$out" bs=1M seek="$off" conv=notrunc,sparse status=none ;;
        zero) ;;   # the image file is sparse zeros already
        *)
            src=$4
            # Android sparse image (the build's super.img; Cuttlefish unsparses it in place)?
            if [[ $(od -An -tx4 -N4 "$src" | tr -d ' ') == ed26ff3a ]]; then
                "$host/simg2img" "$src" "$work/$1.raw"
                src=$work/$1.raw
            fi
            dd if="$src" of="$out" bs=1M seek="$off" conv=notrunc,sparse status=none
            rm -f "$work/$1.raw" ;;
    esac
done
sfdisk --verify "$out" >/dev/null

echo "Image: $out ($((total / 1024)).$(( (total % 1024) * 10 / 1024 )) GiB, needs a stick of at least $need_gb GB)"
echo "Kernel: $(strings "$work/boot/kernel" | grep -m1 -o '^6\.18\.[^ ]*')"
echo "Command line: $pc_cmdline"
echo
echo "A stick that already runs Android: update it in place, keeping apps and data (no dd):"
echo "  sudo tools/update-usb-aosp17.sh --dry-run $out /dev/sdX"
echo "  sudo tools/update-usb-aosp17.sh $out /dev/sdX"
echo "A new, empty stick only:"
if [[ -n $disk ]]; then
    # Everything up to userdata plus its first 64 MiB (zeros: fs_mgr formats a wiped
    # userdata), then the backup GPT in the last MiB; the rest of the stick is userdata.
    used=$(( $(start_of userdata) + 64 ))
    echo "Write it to $disk (this erases the stick; check the device name with lsblk):"
    echo "  sudo dd if=$out of=$disk bs=1M count=$used conv=fsync oflag=direct status=progress"
    echo "  sudo dd if=$out of=$disk bs=1M skip=$(( total - 1 )) seek=$(( total - 1 )) count=1 conv=fsync oflag=direct"
else
    echo "Write it to the WHOLE stick (this erases the stick; check the device name with lsblk):"
    echo "  sudo dd if=$out of=/dev/sdX bs=4M conv=fsync oflag=direct status=progress"
fi
