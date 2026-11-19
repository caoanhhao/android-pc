#!/usr/bin/env bash
# Package an AOSP 17 USB stick image as a GitHub release (docs/release.md).
#
#   tools/make-release.sh [options] [IMG]      default IMG: out/aosp17-usb.img
#
# Writes out/release/<tag>/: the image compressed with xz (pinned to the E-cores) and split
# into parts under GitHub's 2 GiB asset limit, build.txt, SHA256SUMS, INSTALL.md and NOTES.md,
# and with --waydroid the same build packed for Waydroid (docs/waydroid-aosp17.md).
# Before that it checks the build's partition images for pieces that may not be redistributed
# (Play Store, Google WebView, libndk_translation) and refuses if one is there: the image has
# only empty placeholders for them, which users fill with tools/add-google-extras.sh
# (docs/google-extras.md, shipped with the release).
#
# Options: --tag TAG      release tag (default aosp17-YYYYMMDD)
#          --product DIR  images the stick was made from (default aosp/out/target/product/pc_x86_64)
#          --check-only   run the licence and key checks only
#          --commit SHA   the commit the images were built from (default HEAD)
#          --waydroid DIR also ship DIR/system.img + vendor.img (tools/waydroid/aosp17-pack.sh
#                         of the same build) with the scripts that install them
#          --upload       also create a DRAFT GitHub release (gh) for that commit;
#                         publishing it is left to the user
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
product=$ROOT/aosp/out/target/product/pc_x86_64
host=$ROOT/aosp/out/host/linux-x86/bin
tag=aosp17-$(date +%Y%m%d) check_only=0 upload=0 img="" commit=HEAD wd=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag) tag="$2"; shift ;;
        --product) product="$2"; shift ;;
        --commit) commit="$2"; shift ;;
        --waydroid) wd="$2"; shift ;;
        --check-only) check_only=1 ;;
        --upload) upload=1 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        -*) echo "make-release.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
        *) img="$1" ;;
    esac
    shift
done
img=${img:-$ROOT/out/aosp17-usb.img}
commit=$(git -C "$ROOT" rev-parse --verify "$commit^{commit}")
die() { echo "make-release.sh: $*" >&2; exit 1; }
for f in "$host/dump.erofs" "$host/fsck.erofs" "$host/avbtool"; do [[ -x $f ]] || die "missing $f (build hosttools)"; done

# --- licences: pieces we may not redistribute ----------------------------------------------
has() {   # has IMAGE PATH: PATH is in the erofs IMAGE and not empty (an extras placeholder)
    local out; out=$("$host/dump.erofs" --path="$2" "$1" 2>/dev/null || true)
    [[ $out == "Path :"* && $out != *"Size: 0 "* ]]
}
blocked=""
check_google() {   # check_google SYSTEM_IMG PRODUCT_IMG PRODUCT_PREFIX
    has "$2" "$3/priv-app/Phonesky/Phonesky.apk" && blocked+="  $3/priv-app/Phonesky in $2 (Google Play Store)\n"
    has "$2" "$3/app/WebViewGoogle" && blocked+="  $3/app/WebViewGoogle in $2 (Google's Android System WebView)\n"
    has "$1" /system/lib64/libndk_translation.so && blocked+="  libndk_translation.so + proxies in $1 (Google's ARM64 translator)\n"
    return 0
}
check_google "$product/system.img" "$product/product.img" ""
if [[ -n $wd ]]; then
    # Waydroid's system.img carries system_ext, product and odm at its root.
    [[ -f $wd/system.img && -f $wd/vendor.img ]] || die "no system.img/vendor.img in $wd"
    cmp -s "$wd/vendor.img" "$product/vendor.img" || die "$wd/vendor.img is not this build's (run tools/waydroid/aosp17-pack.sh)"
    check_google "$wd/system.img" "$wd/system.img" /product
fi
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
    echo "git.commit=$commit"
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
cp "$ROOT/tools/add-google-extras.sh" "$rel/"
if [[ -n $wd ]]; then
    # The Waydroid images with the scripts that install them, at their paths in the repository.
    wname=android-pc-$tag-waydroid
    mkdir -p "$work/$wname/images" "$work/$wname/tools/waydroid/guard"
    cp "$wd/system.img" "$wd/vendor.img" "$work/$wname/images/"
    cp "$ROOT/tools/add-google-extras.sh" "$work/$wname/tools/"
    cp "$ROOT/tools/waydroid/aosp17-switch.sh" "$ROOT/tools/waydroid/container-guard.sh" "$work/$wname/tools/waydroid/"
    cp "$ROOT/tools/waydroid/guard/guardtest.c" "$work/$wname/tools/waydroid/guard/"
    echo "Compressing the Waydroid images (xz -T4, CPUs 8-15)..."
    tar -C "$work" -cf - "$wname" | taskset -c 8-15 nice -n 10 xz -T4 -6 > "$work/$wname.tar.xz"
    /usr/bin/rm -rf "${work:?}/$wname"
    if (( $(stat -c %s "$work/$wname.tar.xz") > 2000 * 1024**2 )); then
        split -b 2000M -d -a 2 "$work/$wname.tar.xz" "$rel/$wname.tar.xz.part"
        /usr/bin/rm -f "$work/$wname.tar.xz"
        wjoined="cat $wname.tar.xz.part* > $wname.tar.xz && "
    else
        mv "$work/$wname.tar.xz" "$rel/"
        wjoined=""
    fi
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
4. Optional, before or after step 3: add Google Play Store and ARM64 app support, which
   the image may not contain (they come from Google's emulator images; Android SDK License,
   personal use). On Linux, with curl, unzip, e2fsprogs, erofs-utils, android-tools and
   mtools installed: \`./add-google-extras.sh ${name%.xz}\` (or \`./add-google-extras.sh /dev/sdX\`
   for the written stick).
5. Boot the PC from the stick. The first boot formats \`userdata\` (4 GiB) and installs the
   extras; later builds update it in place.

## Stick that already runs android-pc

Do not write the whole image again: that erases your apps and data. From a checkout of
https://github.com/caoanhhao/android-pc, with AOSP's host tools built:

    tools/update-usb-aosp17.sh --dry-run ${name%.xz} /dev/sdX
    tools/update-usb-aosp17.sh ${name%.xz} /dev/sdX

It rewrites only the OS partitions and keeps \`userdata\`, \`metadata\`, \`misc\`, \`frp\`
and \`pclog\`, and copies the Google extras on the stick's ESP back after rewriting it.
It asks for the sudo password itself.
EOF
[[ -n $wd ]] && cat >> "$rel/INSTALL.md" <<EOF

## Waydroid (Linux desktop)

\`$wname.tar.xz\` is the same build for Waydroid: its \`system.img\` (system with system_ext,
product and odm inside) and \`vendor.img\`, with the scripts that install them.

Needs:
- Linux with Waydroid 1.6, already set up with its own LineageOS image (\`sudo waydroid init\`),
  run as your desktop user. Tested on Arch Linux with Hyprland.
- An NVIDIA GPU with NVIDIA's driver on the host. Tested with \`nvidia-open\` 620.70.06 on an
  RTX 4070. The image carries NVIDIA's user-space driver 620.70.06; NVIDIA's kernel module
  accepts only the user-space driver of its own version ("NVRM: API mismatch" in the host's
  kernel log otherwise), so with another host driver version Android has no GPU. Also
  \`/dev/udmabuf\` (\`sudo modprobe udmabuf\`) and the Wayland compositor running on the
  NVIDIA GPU.
- About 10 GB free on \`/\` plus the size of your Waydroid userdata (it is backed up).

Install:

    ${wjoined}tar -xJf $wname.tar.xz && cd $wname
    tools/waydroid/container-guard.sh install    # once: see below
    tools/waydroid/container-guard.sh check
    tools/waydroid/aosp17-switch.sh backup        # LineageOS userdata, images and config
    tools/waydroid/aosp17-switch.sh install images
    waydroid show-full-ui

The scripts ask for the sudo password themselves; do not run them with \`sudo\`.

- **The container guard is required.** Waydroid's container is privileged: when Android
  reboots or shuts down inside it, init may issue filesystem-shutdown ioctls on \`/data\`,
  which in Waydroid is a directory on the host's root filesystem. That shut down the host's
  \`/\` during development. This image has that path removed, and \`container-guard.sh\`
  also blocks those ioctls and the host's disks for the container (seccomp, device cgroup,
  and a pacman hook that keeps it after Waydroid updates; on other distributions run
  \`container-guard.sh install\` again after each Waydroid update). \`aosp17-switch.sh\`
  refuses to switch without it.
- **Your existing Waydroid data is upgraded to Android 17**, after \`backup\` copies it to
  \`~/.local/share/waydroid-backup/\`. Keys in the Android keystore are lost (apps may ask you
  to sign in again; remove any lock-screen PIN before switching), Google Play services is
  replaced by microG (add your Google account again), and 32-bit arm apps stop working.
  Back to LineageOS: \`aosp17-switch.sh restore ~/.local/share/waydroid-backup/lineage20-<date>\`,
  then \`aosp17-switch.sh use lineage20\`.
- Play Store and ARM64 apps, optional, with Waydroid stopped (\`waydroid session stop\`):
  \`tools/add-google-extras.sh --dir ~/.local/share/waydroid/data/vendor/pc-extras\`.
  Android mounts them at its next start.
- Updating to a later release: \`waydroid session stop\`, then
  \`tools/waydroid/aosp17-switch.sh install images\` from the new release; data is kept.
EOF

wd_note=""
[[ -n $wd ]] && wd_note="- Waydroid: the same build runs in Waydroid on a Linux desktop with an NVIDIA GPU
  (\`$wname.tar.xz\`, INSTALL.md \"Waydroid\"): window resizing, sound and microphone through
  PulseAudio/PipeWire, the host's webcam, gamepads, network.
"
cat > "$rel/NOTES.md" <<EOF
AOSP 17 (\`android-17.0.0_r1\`, \`aosp_pc_x86_64-cp2a-userdebug\`) for a real x86_64 PC,
booting from a USB stick, with the device's vendor HALs written in Rust.

- Build: \`$incremental\`, security patch level $spl
- Fingerprint: \`$fingerprint\`
- Source: commit ${commit:0:12} of this repository; see INSTALL.md

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
- microG, F-Droid, Chromium and Termux preinstalled; AOSP's WebView.
- Google Play Store and ARM64 apps (Google's libndk_translation) as extras you add with
  add-google-extras.sh from Google's own emulator images; they are not in the image.
${wd_note}
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
    gh release create "$tag" --draft --target "$commit" \
        --title "android-pc $tag" --notes-file "$rel/NOTES.md" "$rel"/*
fi
