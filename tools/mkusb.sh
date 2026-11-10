#!/usr/bin/env bash
# Put the kernel, the AOSP init initramfs and Limine on a USB stick's EFI
# system partition, without root (see docs/usb-boot.md).
#
#   tools/mkusb.sh --esp DIR      build, then copy into the mounted ESP DIR
#                                 (FAT32, label ANDROIDPC, partition 1 of the
#                                 stick) and print the root commands still
#                                 needed for the other partitions
#   tools/mkusb.sh --stage DIR    build into DIR only (run-qemu.sh
#                                 --usb-boot-test makes its disk image from it)
#
# The stick: GPT, 1 ESP (1 GiB, FAT32 "ANDROIDPC"), 2 super (8 GiB, AOSP's
# super.img), 3 frp (1 MiB, zeroed), 4 misc (1 MiB, zeroed), 5 userdata (the
# rest, unused: /data is tmpfs). The kernel finds the disk with the "super"
# partition at boot (androidboot.partition_map=auto), so the stick works on
# any USB port and next to other disks.
#
# ESP contents: EFI/BOOT/BOOTX64.EFI (Limine), boot/limine/limine.conf,
# boot/kernel, boot/initramfs.cpio. Only those files are written.
#
# Options: --resolution WxH (GOP mode Limine sets, default 1920x1080; the
#          firmware's default mode if it has no such mode),
#          --density N (androidboot.lcd_density, default 240),
#          --serial (also log to the COM1 UART; off by default since the
#          target PC has no serial port and a UART would pace the log),
#          --append "WORDS" (extra kernel command line),
#          --kernel ELF (use this kernel instead of building one),
#          --release (build the release profile; default: dev, as tested).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tools/aosp-bootargs.sh
source "$ROOT/tools/aosp-bootargs.sh"

esp="" stage="" resolution=1920x1080 density=240 serial=0 extra="" elf="" profile=dev
while [[ $# -gt 0 ]]; do
    case "$1" in
        --esp) esp="$2"; shift ;;
        --stage) stage="$2"; shift ;;
        --resolution) resolution="$2"; shift ;;
        --density) density="$2"; shift ;;
        --serial) serial=1 ;;
        --append) extra="$2"; shift ;;
        --kernel) elf="$2"; shift ;;
        --release) profile=release ;;
        -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
        *) echo "mkusb.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done
[[ -n $esp || -n $stage ]] || { echo "mkusb.sh: give --esp DIR (the mounted ESP) or --stage DIR" >&2; exit 2; }
[[ $resolution =~ ^[0-9]+x[0-9]+$ ]] || { echo "mkusb.sh: --resolution WxH" >&2; exit 2; }

LIMINE_DIR="${LIMINE_DIR:-/usr/share/limine}"
[[ -f $LIMINE_DIR/BOOTX64.EFI ]] || { echo "mkusb.sh: no $LIMINE_DIR/BOOTX64.EFI (install limine)" >&2; exit 1; }
aosp_top="$(cd "$ROOT" && git worktree list --porcelain | sed -n '1s/^worktree //p')/aosp"
product="${AOSP_PRODUCT_OUT:-$aosp_top/out/target/product/vsoc_x86_64_only}"
[[ -f $product/super.img ]] || { echo "mkusb.sh: no $product/super.img (build AOSP first)" >&2; exit 1; }

# --- the ESP mount: check it is the stick's first partition --------------
disk="" byid=""
if [[ -n $esp ]]; then
    [[ -d $esp ]] || { echo "mkusb.sh: $esp is not a directory; plug the stick in (udisks mounts it at /run/media/$USER/ANDROIDPC)" >&2; exit 1; }
    src="$(findmnt -no SOURCE --target "$esp" 2>/dev/null || true)"
    fstype="$(findmnt -no FSTYPE --target "$esp" 2>/dev/null || true)"
    target="$(findmnt -no TARGET --target "$esp" 2>/dev/null || true)"
    if [[ $fstype != vfat || $target != "$(realpath "$esp")" ]]; then
        echo "mkusb.sh: $esp is not the mount point of a FAT file system (found '$fstype' at '$target')" >&2
        exit 1
    fi
    disk="/dev/$(lsblk -no PKNAME "$src" | head -1)"
    partn="$(lsblk -no PARTN "$src" 2>/dev/null | head -1 || true)"
    [[ -z $partn ]] && partn="${src##*[!0-9]}"
    label="$(lsblk -no LABEL "$src" | head -1)"
    if [[ $partn != 1 || $label != ANDROIDPC ]]; then
        echo "mkusb.sh: $src (partition ${partn:-?}, label '$label') is not partition 1 labelled ANDROIDPC; see docs/usb-boot.md for the layout" >&2
        exit 1
    fi
    if [[ $(lsblk -dno TRAN "$disk") != usb ]]; then
        echo "mkusb.sh: $disk is not a USB disk; refusing" >&2
        exit 1
    fi
    # Partitions 2-4 by name and size (from udev; reading them needs root).
    want=(ESP super frp misc userdata)
    sizes=([2]=$((8 << 30)) [3]=$((1 << 20)) [4]=$((1 << 20)))
    while read -r name pl size; do
        n="${name##*[!0-9]}"
        if [[ $n -ge 1 && $n -le 5 && $pl != "${want[$((n - 1))]}" ]]; then
            echo "mkusb.sh: warning: $name is named '$pl', expected '${want[$((n - 1))]}'" >&2
        fi
        if [[ -n ${sizes[$n]:-} && $size != "${sizes[$n]}" ]]; then
            echo "mkusb.sh: warning: $name is $size bytes, expected ${sizes[$n]}" >&2
        fi
    done < <(lsblk -lnbo NAME,PARTLABEL,SIZE "$disk" | tail -n +2)
    for l in /dev/disk/by-id/usb-*; do
        [[ $l == *-part* ]] && continue
        [[ $(realpath "$l") == "$disk" ]] && { byid="$l"; break; }
    done
    [[ -n $byid ]] || byid="$disk"
fi

# --- build -------------------------------------------------------------------
if [[ -z $elf ]]; then
    echo "mkusb.sh: building the kernel ($profile)"
    "$ROOT/tools/limited-build.sh" bash -c "cd '$ROOT/kernel' && env -u RUSTUP_TOOLCHAIN cargo build --profile $profile"
    dir=debug; [[ $profile == release ]] && dir=release
    elf="$ROOT/kernel/target/x86_64-unknown-none/$dir/kernel"
fi
echo "mkusb.sh: building the AOSP init initramfs"
python3 "$ROOT/tools/mkaosp-init-initramfs.py" >/dev/null
initrd="$ROOT/out/aosp-init-initramfs.cpio"

cmdline="$(aosp_boot_args usb 1 "$density")"
# No serial console on the target: the log goes to the screen.
cmdline="${cmdline/ androidboot.console=ttyS0/}"
[[ $serial == 1 ]] || cmdline+=" serial=off"
cmdline+=" loglevel=6"
[[ -n $extra ]] && cmdline+=" $extra"

out="${stage:-$(mktemp -d)}"
mkdir -p "$out/EFI/BOOT" "$out/boot/limine"
cp "$LIMINE_DIR/BOOTX64.EFI" "$out/EFI/BOOT/BOOTX64.EFI"
cp "$elf" "$out/boot/kernel"
cp "$initrd" "$out/boot/initramfs.cpio"
cat > "$out/boot/limine/limine.conf" <<CONF
# android-pc: AOSP on the Rust kernel (written by tools/mkusb.sh).
timeout: 3
verbose: yes

/android-pc
    protocol: limine
    path: boot():/boot/kernel
    module_path: boot():/boot/initramfs.cpio
    resolution: ${resolution}x32
    cmdline: $cmdline
CONF

if [[ -z $esp ]]; then
    echo "mkusb.sh: staged in $out"
    exit 0
fi

# --- install -------------------------------------------------------------------
need=$(du -sb "$out" | cut -f1)
free=$(df -B1 --output=avail "$esp" | tail -1 | tr -dc 0-9)
(( need < free )) || { echo "mkusb.sh: $esp has $free bytes free, need $need" >&2; exit 1; }
for f in EFI/BOOT/BOOTX64.EFI boot/limine/limine.conf boot/kernel boot/initramfs.cpio; do
    mkdir -p "$esp/$(dirname "$f")"
    cp "$out/$f" "$esp/$f.new"
    mv -f "$esp/$f.new" "$esp/$f"
done
sync -f "$esp/boot/kernel"
rm -rf "$out"
echo "mkusb.sh: ESP $esp ($src on $byid) now has:"
(cd "$esp" && find EFI boot -type f -printf '  %p (%s bytes)\n' | sort)

part() { [[ $byid == /dev/disk/by-id/* ]] && echo "$byid-part$1" || echo "$disk$1"; }
cat <<EOF

Still to do as root, once (skip what you already did):

  # AOSP's super.img (8 GiB) on partition 2:
  sudo dd if=$product/super.img of=$(part 2) bs=16M oflag=direct status=progress
  # frp and misc zeroed:
  sudo dd if=/dev/zero of=$(part 3) bs=1M count=1 oflag=direct
  sudo dd if=/dev/zero of=$(part 4) bs=1M count=1 oflag=direct
  sync

Then eject the stick (udisksctl power-off -b $disk) and boot it: docs/usb-boot.md.
EOF
