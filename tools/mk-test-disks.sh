#!/usr/bin/env bash
# Extra virtual disks for the external-storage test (F9i, docs/usb-boot-aosp17.md §9):
# sparse image files in DIR (default out/f9i-test) and the QEMU_EXTRA line that attaches
# them to tools/run-qemu-aosp17-usb.sh. Recreate them before each run: adopting writes them.
#   nvme-arch.img    GPT ESP (FAT32) + Linux (ext4), like the PC's Arch disk -> nvme0n1, ignored
#   nvme-blank.img   all zeros                                              -> offered, adoptable
#   sata-foreign.img GPT basic-data partition holding ext4                  -> ignored
#   usb-fat.img      MBR, one FAT32 partition                               -> portable storage
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
d=${1:-$ROOT/out/f9i-test}
mkdir -p "$d"
cd "$d"
/usr/bin/rm -f nvme-arch.img nvme-blank.img sata-foreign.img usb-fat.img p.fat p.ext4
put() { dd if="$1" of="$2" bs=1M seek="$3" conv=notrunc,sparse status=none; }
truncate -s 256M p.fat && mkfs.fat -F 32 -n ESP p.fat >/dev/null
truncate -s 64M p.ext4 && mkfs.ext4 -q p.ext4
truncate -s 4G nvme-arch.img
printf 'label: gpt\nsize=256MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B\ntype=0FC63DAF-8483-4772-8E79-3D69D8477DE4\n' | sfdisk -q nvme-arch.img
put p.fat nvme-arch.img 1
put p.ext4 nvme-arch.img 257
truncate -s 8G nvme-blank.img
truncate -s 2G sata-foreign.img
printf 'label: gpt\ntype=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7\n' | sfdisk -q sata-foreign.img
put p.ext4 sata-foreign.img 1
truncate -s 2G usb-fat.img
printf 'label: dos\ntype=c\n' | sfdisk -q usb-fat.img
truncate -s 2047M p.fat && mkfs.fat -F 32 -n PCUSB p.fat >/dev/null
put p.fat usb-fat.img 1
/usr/bin/rm -f p.fat p.ext4
echo "QEMU_EXTRA=\"-drive if=none,id=n0,format=raw,file=$d/nvme-arch.img -device nvme,serial=ARCH0,drive=n0" \
     "-drive if=none,id=n1,format=raw,file=$d/nvme-blank.img -device nvme,serial=BLANK1,drive=n1" \
     "-drive if=none,id=s0,format=raw,file=$d/sata-foreign.img -device ide-hd,drive=s0,bus=ide.0" \
     "-drive if=none,id=u1,format=raw,file=$d/usb-fat.img -device usb-storage,bus=xhci.0,drive=u1" \
     "-device vhost-vsock-pci,guest-cid=3\""
