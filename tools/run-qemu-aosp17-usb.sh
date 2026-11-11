#!/usr/bin/env bash
# Boot a tools/mkusb-aosp17.sh image in QEMU the way the real PC boots it: UEFI (OVMF),
# the image as a USB mass-storage stick on an xHCI controller, a standard VGA (its GOP
# framebuffer is what pcfb drives), USB keyboard and tablet, an e1000e NIC with DHCP
# (user networking; adb on host port 5556 -> guest 5555).
#
#   tools/run-qemu-aosp17-usb.sh [IMAGE]    default out/aosp17-usb.img
#
# The image is not modified (QEMU snapshot mode): every run is a first boot. WRITABLE=1
# writes to it instead, so pclog.sh's logs stay on its pclog partition (read them with
# tools/pclog-read.sh IMAGE).
# Serial output (with console=ttyS0 in the image's command line, see
# `tools/mkusb-aosp17.sh --append`) goes to out/aosp17-usb-serial.log, the QEMU monitor to
# a UNIX socket out/aosp17-usb-monitor.sock
# (`echo "screendump out/s.ppm" | ncat -U out/aosp17-usb-monitor.sock`).
# NO_WINDOW=1 runs without a window. MEM (MiB, default 6144), CPUS (default 4).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
img=${1:-$ROOT/out/aosp17-usb.img}
[[ -f $img ]] || { echo "run-qemu-aosp17-usb.sh: no $img (run tools/mkusb-aosp17.sh)" >&2; exit 1; }
OVMF_CODE="" OVMF_VARS=""
for d in /usr/share/edk2/x64 /usr/share/edk2-ovmf/x64 /usr/share/OVMF; do
    if [[ -f $d/OVMF_CODE.4m.fd ]]; then OVMF_CODE=$d/OVMF_CODE.4m.fd OVMF_VARS=$d/OVMF_VARS.4m.fd; break; fi
done
[[ -n $OVMF_CODE ]] || { echo "run-qemu-aosp17-usb.sh: install edk2-ovmf" >&2; exit 1; }
vars=$ROOT/out/aosp17-usb-ovmf-vars.fd
cp "$OVMF_VARS" "$vars"
display=(-display gtk); [[ -n ${NO_WINDOW:-} ]] && display=(-display none)
exec qemu-system-x86_64 \
    -machine q35,accel=kvm -cpu host -smp "${CPUS:-4}" -m "${MEM:-6144}" \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$vars" \
    -device qemu-xhci,id=xhci \
    -drive if=none,id=stick,format=raw,file="$img",snapshot=$([[ -n ${WRITABLE:-} ]] && echo off || echo on) \
    -device usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=0 \
    -device usb-kbd,bus=xhci.0 -device usb-tablet,bus=xhci.0 \
    -netdev user,id=net0,hostfwd=tcp:127.0.0.1:5556-:5555 -device e1000e,netdev=net0 \
    -vga std "${display[@]}" \
    -serial file:"$ROOT/out/aosp17-usb-serial.log" \
    -monitor unix:"$ROOT/out/aosp17-usb-monitor.sock",server,nowait
