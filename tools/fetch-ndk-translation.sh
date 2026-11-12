#!/usr/bin/env bash
# Fetch Google's ARM64 -> x86_64 translator (libndk_translation, its proxy libraries and
# binfmt_misc runner) for aosp_pc_x86_64 into aosp/device/android-pc/ndk_translation/system/;
# aosp-device/ndk_translation.mk installs it with the AOSP-built guest libraries and switches
# the native bridge on.
#
#   tools/fetch-ndk-translation.sh
#
# Source: the official Android Emulator system image "Google APIs, x86_64, API 37"
# (google_apis/x86_64-37.0_r08, https://dl.google.com/android/repository/sys-img/), whose
# x86_64-only system translates arm64-v8a apps, like ours. Its SHA-1 is checked against
# the one Google publishes in sys-img2-3.xml (pinned below).
#
# libndk_translation is proprietary Google software, licensed with the emulator image for
# development use. This script fetches it on the build machine; it is never committed, and
# images built with it are for personal use only, not for redistribution.
# docs/arm-translation.md has the details.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DST=$ROOT/aosp/device/android-pc/ndk_translation
DL=$ROOT/out/downloads/emu-37
ZIP=google_apis-x86_64-37.0_r08.zip
URL=https://dl.google.com/android/repository/sys-img/google_apis/x86_64-37.0_r08.zip
SHA1=a266cfbcf4d6c769b166c0af4d68eff0e4f4a5e3

mkdir -p "$DL"
if [[ ! -f $DL/$ZIP ]]; then
    curl -sSf -o "$DL/$ZIP.part" "$URL"
    echo "$SHA1  $DL/$ZIP.part" | sha1sum -c --quiet
    mv "$DL/$ZIP.part" "$DL/$ZIP"
fi

# system.img in the zip is a GPT disk: partition 2 is the dynamic-partition "super".
work=$(mktemp -d "$DL/extract.XXXXXX")
trap 'rm -rf "$work"' EXIT
unzip -q -d "$work" "$DL/$ZIP" x86_64/system.img
start=$(sfdisk -d "$work/x86_64/system.img" | sed -n 's/.*: start= *\([0-9]*\),.*/\1/p' | sed -n 2p)
dd if="$work/x86_64/system.img" of="$work/super.img" bs=512 skip="$start" status=none
rm "$work/x86_64/system.img"
lpunpack -p system "$work/super.img" "$work" >/dev/null
rm "$work/super.img"

# Only the proprietary part; the arm64 guest libraries and the config files are built
# from AOSP source (aosp-device/ndk_translation.mk).
rm -rf "$DST" && mkdir -p "$DST/system/lib64" "$DST/system/bin"
for f in lib64/libndk_translation.so bin/ndk_translation_program_runner_binfmt_misc_arm64; do
    debugfs -R "dump /system/$f $DST/system/$f" "$work/system.img" 2>/dev/null
done
for p in $(debugfs -R "ls -p /system/lib64" "$work/system.img" 2>/dev/null |
           awk -F/ '$6 ~ /^libproxy_arm64_to_x86_64_/ {print $6}'); do
    debugfs -R "dump /system/lib64/$p $DST/system/lib64/$p" "$work/system.img" 2>/dev/null
done
chmod -R u+rwX,go+rX "$DST"
[[ -s $DST/system/lib64/libndk_translation.so && -s $DST/system/lib64/libproxy_arm64_to_x86_64_libc.so ]] ||
    { echo "fetch-ndk-translation.sh: extraction failed" >&2; exit 1; }
echo "ndk_translation: $(find "$DST" -type f | wc -l) files, $(du -sh "$DST" | cut -f1), in $DST"
