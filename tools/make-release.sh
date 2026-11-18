#!/usr/bin/env bash
# Package an AOSP 17 USB stick image as a GitHub release (docs/release.md).
#
#   tools/make-release.sh [options] [IMG]      default IMG: out/aosp17-usb.img
#
# Writes out/release/<tag>/: the image compressed with xz (pinned to the E-cores) and split
# into parts under GitHub's 2 GiB asset limit, build.txt, SHA256SUMS, INSTALL.md and NOTES.md.
# Before that it checks the build's partition images for pieces that may not be redistributed
# (Play Store, Google WebView, libndk_translation) and refuses if one is there.
#
# Options: --tag TAG      release tag (default aosp17-YYYYMMDD)
#          --product DIR  images the stick was made from (default aosp/out/target/product/pc_x86_64)
#          --check-only   run the licence and key checks only
#          --upload       also create a DRAFT GitHub release (gh) for the current commit;
#                         publishing it is left to the user
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
product=$ROOT/aosp/out/target/product/pc_x86_64
host=$ROOT/aosp/out/host/linux-x86/bin
tag=aosp17-$(date +%Y%m%d) check_only=0 upload=0 img=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag) tag="$2"; shift ;;
        --product) product="$2"; shift ;;
        --check-only) check_only=1 ;;
        --upload) upload=1 ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        -*) echo "make-release.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
        *) img="$1" ;;
    esac
    shift
done
img=${img:-$ROOT/out/aosp17-usb.img}
die() { echo "make-release.sh: $*" >&2; exit 1; }
for f in "$host/dump.erofs" "$host/fsck.erofs" "$host/avbtool"; do [[ -x $f ]] || die "missing $f (build hosttools)"; done

# --- licences: pieces we may not redistribute ----------------------------------------------
has() {   # has IMAGE PATH: PATH exists in the erofs IMAGE (dump.erofs exits non-zero either way)
    local out; out=$("$host/dump.erofs" --path="$2" "$product/$1" 2>/dev/null || true)
    [[ $out == "Path :"* ]]
}
blocked=""
has product.img /priv-app/Phonesky && blocked+="  product/priv-app/Phonesky (Google Play Store)\n"
has product.img /app/WebViewGoogle && blocked+="  product/app/WebViewGoogle (Google's Android System WebView)\n"
has system.img /system/lib64/libndk_translation.so && blocked+="  system/lib64/libndk_translation.so + proxies (Google's ARM64 translator)\n"
if [[ -n $blocked ]]; then
    printf "make-release.sh: the build contains proprietary Google software that may not be redistributed:\n$blocked" >&2
    echo "Build without it first (docs/release.md, \"the image cannot be published\")." >&2
    exit 3
fi

# --- secrets: the AVB key must be AOSP's public test key -----------------------------------
avb_key=$("$host/avbtool" info_image --image "$product/vbmeta.img" | awk '/^Public key \(sha1\):/ { print $4 }')
rollback=$("$host/avbtool" info_image --image "$product/vbmeta.img" | awk '/^Rollback Index:/ { print $3 }')
[[ $avb_key == 2597c218aae470a130f61162feaae70afd97f011 ]] ||
    echo "make-release.sh: note: vbmeta is not signed with AOSP's testkey_rsa4096 ($avb_key); check that its private key is not in the release" >&2
echo "Licence and key checks: OK (AVB key $avb_key, rollback index $rollback)"
[[ $check_only == 1 ]] && exit 0

# --- package ---------------------------------------------------------------------------------
[[ -f $img ]] || die "no $img (make it with tools/mkusb-aosp17.sh)"
rel=$ROOT/out/release/$tag
img_bytes=$(stat -c %s "$img")
free=$(df -B1 --output=avail "$ROOT/out" | tail -1)
(( free > img_bytes + 10 * 1024**3 )) || die "not enough free space for $rel (keep 10 GB free on /)"
/usr/bin/rm -rf "$rel"
mkdir -p "$rel"
work=$(mktemp -d "$ROOT/out/.make-release.XXXXXX")
trap '/usr/bin/rm -rf "$work"' EXIT

# Build info from the image's own ESP (tools/mkusb-aosp17.sh writes android/build.txt there).
esp_off=$(sfdisk --json "$img" | python3 -I -c '
import json, sys
for p in json.load(sys.stdin)["partitiontable"]["partitions"]:
    if p.get("name") == "esp": print(p["start"] * 512)')
MTOOLS_SKIP_CHECK=1 "$host/mcopy" -i "$img@@$esp_off" ::/android/build.txt "$work/build.txt"
prop() { sed -n "s/^$1=//p" "$work/build.txt"; }
{
    cat "$work/build.txt"
    echo "avb.public_key_sha1=$avb_key"
    echo "avb.rollback_index=$rollback"
    echo "git.commit=$(git -C "$ROOT" rev-parse HEAD)"
    echo "image=$(basename "$img") $img_bytes bytes"
} > "$rel/build.txt"
fingerprint=$(prop ro.system.build.fingerprint) incremental=$(prop ro.build.version.incremental)
spl=$(prop ro.build.version.security_patch)

name=android-pc-$tag.img.xz
echo "Compressing $img (xz -T4, CPUs 8-15)..."
taskset -c 8-15 nice -n 10 xz -T4 -6 -c "$img" > "$work/$name"
if (( $(stat -c %s "$work/$name") > 2000 * 1024**2 )); then
    split -b 2000M -d -a 2 "$work/$name" "$rel/$name.part"
    /usr/bin/rm -f "$work/$name"
    joined="cat $name.part* > $name"
else
    mv "$work/$name" "$rel/$name"
    joined=""
fi
cp "$ROOT/aosp/device/android-pc/nvidia-userspace/LICENSE" "$rel/LICENSE.nvidia" 2>/dev/null ||
    cp /usr/share/licenses/nvidia-utils/LICENSE "$rel/LICENSE.nvidia" 2>/dev/null ||
    echo "make-release.sh: warning: no NVIDIA LICENSE found to ship with the release" >&2

cat > "$rel/INSTALL.md" <<EOF
# Installing android-pc $tag

Needs a USB stick of at least 16 GB and a UEFI PC (Secure Boot off).

## New (empty) stick

1. Check the files: \`sha256sum -c SHA256SUMS\`.
2. ${joined:+Join the parts: \`$joined\`, then d}${joined:-D}ecompress: \`xz -dk $name\`.
3. Find the stick with \`lsblk -o NAME,SIZE,MODEL,TRAN\` and write the whole image to it
   (this erases the stick):
   \`sudo dd if=${name%.xz} of=/dev/sdX bs=4M conv=fsync oflag=direct status=progress\`
4. Boot the PC from the stick. The first boot formats \`userdata\` (4 GiB); later builds
   update it in place.

## Stick that already runs android-pc

Do not write the whole image again: that erases your apps and data. From a checkout of
https://github.com/caoanhhao/android-pc, with AOSP's host tools built:

    tools/update-usb-aosp17.sh --dry-run ${name%.xz} /dev/sdX
    tools/update-usb-aosp17.sh ${name%.xz} /dev/sdX

It rewrites only the OS partitions and keeps \`userdata\`, \`metadata\`, \`misc\`, \`frp\`
and \`pclog\`. It asks for the sudo password itself.
EOF

cat > "$rel/NOTES.md" <<EOF
AOSP 17 (\`android-17.0.0_r1\`, \`aosp_pc_x86_64-cp2a-userdebug\`) for a real x86_64 PC,
booting from a USB stick, with the device's vendor HALs written in Rust.

- Build: \`$incremental\`, security patch level $spl
- Fingerprint: \`$fingerprint\`
- Source: commit $(git -C "$ROOT" rev-parse --short HEAD) of this repository; see INSTALL.md

## Target hardware

Tested on one PC: Intel Core i7-13700K, NVIDIA GeForce RTX 4070, Intel AX211 Wi-Fi/Bluetooth,
Realtek RTL8125 Ethernet, onboard HD Audio, a 2560x1080 monitor, USB keyboard, mouse and
webcam. Other UEFI x86_64 PCs may boot; NVIDIA GPUs other than Ada (RTX 40) are untested.

## Features

- Rust vendor HALs (phases F1-F9): health, power, thermal, light, audio, camera (V4L2),
  Bluetooth, Wi-Fi (nl80211), plus the NVIDIA display and GPU pieces below.
- Boots from USB with Limine on UEFI, GKI 6.18 kernel; updates keep user data.
- Desktop mode: freeform windows, taskbar and Smart Dock, landscape, mouse and keyboard.
- NVIDIA's own driver (phases K/L): nvidia-open kernel modules, NVIDIA's Vulkan driver as
  the device GPU driver through a glibc sidecar, drm_hwcomposer on nvidia-drm.
- microG, F-Droid, Chromium and Termux preinstalled.

## Known issues

- Delta Force crashes about 22 s after start (in \`libUE4.so\`).
- Not yet checked on the real PC: Wi-Fi hotspot, Bluetooth audio and pairing round-trip,
  Play Integrity (at most basic integrity with microG).
- userdebug build, SELinux permissive, signed with AOSP's public test keys (AVB and APKs):
  anyone can sign an image this stick accepts. Not for devices holding sensitive data.

## Licences

AOSP and this repository: Apache 2.0 (kernel and GPL modules: GPL-2.0, sources in AOSP's
\`kernel/common\` \`android17-6.18\` and NVIDIA's open-gpu-kernel-modules 620.70.06).
NVIDIA's user-space libraries and GSP firmware 620.70.06 are included unmodified under
NVIDIA's licence (LICENSE.nvidia). Firmware from linux-firmware under its licences.
EOF

( cd "$rel" && sha256sum -- * > "$work/SHA256SUMS" && mv "$work/SHA256SUMS" SHA256SUMS && sha256sum -c --quiet SHA256SUMS )
if grep -rlI 'PRIVATE KEY' "$rel" >/dev/null; then die "a private key is in $rel"; fi
echo "Release files in $rel:"
ls -l "$rel"

if [[ $upload == 1 ]]; then
    git -C "$ROOT" diff --quiet HEAD || die "uncommitted changes: release only committed work"
    gh release create "$tag" --draft --target "$(git -C "$ROOT" rev-parse HEAD)" \
        --title "android-pc $tag" --notes-file "$rel/NOTES.md" "$rel"/*
fi
