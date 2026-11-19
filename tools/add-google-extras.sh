#!/usr/bin/env bash
# Add Google's Play Store and ARM64 translator (libndk_translation) to an android-pc stick
# (docs/google-extras.md). Both are proprietary Google software that the android-pc image may
# not contain; this script takes them from Google's official Android Emulator system images,
# on your own computer, and puts them on the stick's ESP. On the next boot Android copies them
# to /data/vendor/pc-extras (kept through OS updates) and uses them as built-in system pieces.
#
#   tools/add-google-extras.sh [options] TARGET
#
# TARGET: a stick image made by tools/mkusb-aosp17.sh (written in place, no root needed),
#         the stick itself (/dev/sdX, whole disk; asks for the sudo password for the write),
#         or --dir DIR: write the files to DIR instead (e.g. Waydroid's
#         ~/.local/share/waydroid/data/vendor/pc-extras).
# Options: --only playstore|ndk  add only one of the two (default: both)
#          --accept-license      do not ask; you accept the Android SDK License Agreement
#                                (https://developer.android.com/studio/terms) for the images
#          --cache DIR           where the downloads are kept (default ~/.cache/android-pc-extras;
#                                2.1 GB per image, reused by later runs)
#          --dir DIR             write to a directory instead of an ESP
# Needs: curl, unzip, sfdisk, sha1sum, sha256sum, debugfs (e2fsprogs), fsck.erofs (erofs-utils),
#        lpunpack (android-tools), mtools; about 9 GB free in the cache directory while it runs.
# The files are for your personal use only; do not redistribute them.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
only="" accept=0 dir="" target="" cache=${XDG_CACHE_HOME:-$HOME/.cache}/android-pc-extras
while [[ $# -gt 0 ]]; do
    case "$1" in
        --only) only="$2"; shift ;;
        --accept-license) accept=1 ;;
        --cache) cache="$2"; shift ;;
        --dir) dir="$2"; shift ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        -*) echo "add-google-extras.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
        *) target="$1" ;;
    esac
    shift
done
die() { echo "add-google-extras.sh: $*" >&2; exit 1; }
[[ -n $dir || -n $target ]] || die "no target (see --help)"
[[ -z $only || $only == playstore || $only == ndk ]] || die "--only takes playstore or ndk"

# Tools: from PATH, else from an AOSP build in this checkout.
aosp_host=$ROOT/aosp/out/host/linux-x86/bin
tool() {
    if command -v "$1" >/dev/null; then command -v "$1"
    elif [[ -x $aosp_host/$1 ]]; then echo "$aosp_host/$1"
    else die "needs $1 ($2)"; fi
}
for t in curl unzip sfdisk sha1sum sha256sum; do tool $t "your distribution" >/dev/null; done
DEBUGFS=$(tool debugfs e2fsprogs)
FSCK_EROFS=$(tool fsck.erofs erofs-utils)
LPUNPACK=$(tool lpunpack android-tools)
mtool() {   # mtool NAME: the mtools command NAME as words (AOSP's mtools binary takes -c NAME)
    if command -v "$1" >/dev/null; then command -v "$1"
    elif [[ -x $aosp_host/mtools ]]; then echo "$aosp_host/mtools -c $1"
    else die "needs $1 (mtools)"; fi
}
read -ra MCOPY <<<"$(mtool mcopy)"; read -ra MMD <<<"$(mtool mmd)"
read -ra MDELTREE <<<"$(mtool mdeltree)"; read -ra MDIR <<<"$(mtool mdir)"
export MTOOLS_SKIP_CHECK=1

# The emulator images, pinned by the SHA-1 Google publishes in its sys-img repository XML.
SYSIMG=https://dl.google.com/android/repository/sys-img
NDK_ZIP=google_apis-x86_64-37.0_r08.zip NDK_SHA1=a266cfbcf4d6c769b166c0af4d68eff0e4f4a5e3
NDK_URL=$SYSIMG/google_apis/x86_64-37.0_r08.zip
PS_ZIP=google_apis_playstore-x86_64-37.0_r08.zip PS_SHA1=7ba53fe131fad2cfc3de2b88e32fde5c529fc9c7
PS_URL=$SYSIMG/google_apis_playstore/x86_64-37.0_r08.zip

if (( ! accept )); then
    cat <<EOF
This downloads Google's Android Emulator system images ($NDK_ZIP and/or $PS_ZIP,
2.1 GB each) from dl.google.com and copies Google Play Store and libndk_translation out of
them. They are licensed to you under the Android Software Development Kit License Agreement
(https://developer.android.com/studio/terms), for your personal use; do not redistribute them.
EOF
    read -r -p "Accept the license and continue? Type yes: " ok
    [[ $ok == yes ]] || die "not accepted"
fi

mkdir -p "$cache"
work=$(mktemp -d "$cache/work.XXXXXX")
trap 'rm -rf "$work"' EXIT
out=$work/extras
mkdir -p "$out"

fetch() {   # fetch ZIP URL SHA1
    [[ -f $cache/$1 ]] && return
    echo "Downloading $1..."
    curl -fL --progress-bar -o "$cache/$1.part" "$2"
    echo "$3  $cache/$1.part" | sha1sum -c --quiet || die "$1: SHA-1 mismatch"
    mv "$cache/$1.part" "$cache/$1"
}
partition() {   # partition ZIP NAME: the image's dynamic partition NAME, as $work/NAME.img
    local start
    unzip -q -o -d "$work" "$cache/$1" x86_64/system.img
    # x86_64/system.img is a GPT disk whose partition 2 is the dynamic-partition "super".
    start=$(sfdisk -d "$work/x86_64/system.img" | sed -n 's/.*: start= *\([0-9]*\),.*/\1/p' | sed -n 2p)
    dd if="$work/x86_64/system.img" of="$work/super.img" bs=512 skip="$start" status=none
    rm -f "$work/x86_64/system.img"
    "$LPUNPACK" -p "$2" "$work/super.img" "$work" >/dev/null
    rm -f "$work/super.img"
}

versions=()
if [[ $only != playstore ]]; then
    fetch "$NDK_ZIP" "$NDK_URL" "$NDK_SHA1"
    echo "Extracting libndk_translation..."
    partition "$NDK_ZIP" system   # ext4
    mkdir -p "$out/system/lib64" "$out/system/bin"
    for f in lib64/libndk_translation.so bin/ndk_translation_program_runner_binfmt_misc_arm64; do
        "$DEBUGFS" -R "dump /system/$f $out/system/$f" "$work/system.img" 2>/dev/null
    done
    for p in $("$DEBUGFS" -R "ls -p /system/lib64" "$work/system.img" 2>/dev/null |
               awk -F/ '$6 ~ /^libproxy_arm64_to_x86_64_/ {print $6}'); do
        "$DEBUGFS" -R "dump /system/lib64/$p $out/system/lib64/$p" "$work/system.img" 2>/dev/null
    done
    rm -f "$work/system.img"
    [[ -s $out/system/lib64/libndk_translation.so && -s $out/system/lib64/libproxy_arm64_to_x86_64_libc.so ]] ||
        die "libndk_translation extraction failed"
    versions+=("ndk_translation=$NDK_ZIP")
fi
if [[ $only != ndk ]]; then
    fetch "$PS_ZIP" "$PS_URL" "$PS_SHA1"
    echo "Extracting Play Store..."
    partition "$PS_ZIP" product   # EROFS
    "$FSCK_EROFS" --extract="$work/product" "$work/product.img" >/dev/null
    rm -f "$work/product.img"
    mkdir -p "$out/product/priv-app/Phonesky" "$out/product/etc/permissions" "$out/product/etc/sysconfig"
    cp "$work/product/priv-app/Phonesky/Phonesky.apk" "$out/product/priv-app/Phonesky/"
    # Its privileged permissions and sysconfig entries, as the emulator image has them. Above
    # all allow-in-power-save-except-idle: without it every download crashes
    # com.android.vending:background (docs/arm-translation.md, F9d).
    { echo '<?xml version="1.0" encoding="utf-8"?>'
      echo "<!-- From $PS_ZIP by tools/add-google-extras.sh. -->"
      echo '<permissions>'
      sed -n '/<privapp-permissions package="com.android.vending">/,/<\/privapp-permissions>/p' \
          "$work/product/etc/permissions/privapp-permissions-google-p.xml"
      echo '</permissions>'
    } > "$out/product/etc/permissions/privapp-permissions-phonesky.xml"
    { echo '<?xml version="1.0" encoding="utf-8"?>'
      echo "<!-- From $PS_ZIP (google.xml) by tools/add-google-extras.sh. -->"
      echo '<config>'
      grep 'package="com.android.vending"' "$work/product/etc/sysconfig/google.xml"
      echo '</config>'
    } > "$out/product/etc/sysconfig/sysconfig-phonesky.xml"
    rm -rf "$work/product"
    grep -q 'permission name=' "$out/product/etc/permissions/privapp-permissions-phonesky.xml" &&
        grep -q '<allow-in-power-save-except-idle package="com.android.vending"' \
            "$out/product/etc/sysconfig/sysconfig-phonesky.xml" || die "Play Store extraction failed"
    versions+=("playstore=$PS_ZIP")
fi
echo "${versions[*]} added $(date -u +%Y-%m-%dT%H:%MZ)" > "$out/VERSION"
( cd "$out" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort | xargs sha256sum > SHA256SUMS )
echo "Extras: $(( $(find "$out" -type f | wc -l) )) files, $(du -sh "$out" | cut -f1)"

# --- write -----------------------------------------------------------------------------------
if [[ -n $dir ]]; then
    # Waydroid's data/vendor belongs to root (the container's uid 0): write it with sudo.
    run=() p=$dir
    while [[ ! -e $p ]]; do p=$(dirname "$p"); done
    [[ -w $p ]] || { run=(sudo); sudo -v; }
    "${run[@]}" mkdir -p "$dir"
    "${run[@]}" find "$dir" -mindepth 1 -delete
    "${run[@]}" cp -R "$out/." "$dir/"
    echo "Written to $dir"
    exit 0
fi
run=()
if [[ -b $target ]]; then
    [[ $(lsblk -dno TYPE "$target") == disk && $(lsblk -dno TRAN "$target") == usb ]] ||
        die "$target is not a whole USB disk"
    lsblk -no MOUNTPOINTS "$target" | grep -q . && die "$target has mounted partitions"
    run=(sudo env MTOOLS_SKIP_CHECK=1)
    sudo -v
elif [[ ! -f $target ]]; then
    die "no $target"
fi
off=$("${run[@]}" sfdisk --json "$target" | python3 -I -c '
import json, sys
t = json.load(sys.stdin)["partitiontable"]
for p in t["partitions"]:
    if p.get("name") == "esp": print(p["start"] * t.get("sectorsize", 512))')
[[ -n $off ]] || die "$target has no esp partition: not an android-pc stick"
esp="$target@@$off"
"${run[@]}" "${MDIR[@]}" -i "$esp" ::/android/build.txt >/dev/null 2>&1 || die "$target: no android-pc ESP"
"${run[@]}" "${MDELTREE[@]}" -i "$esp" ::/android/extras >/dev/null 2>&1 || true
free=$("${run[@]}" "${MDIR[@]}" -i "$esp" ::/ | sed -n 's/^ *\([0-9 ]*\) bytes free.*/\1/p' | tr -d ' ')
need=$(du -sb "$out" | cut -f1)
(( ${free:-0} > need + 4 * 1048576 )) || die "the ESP has $free bytes free, the extras need $need"
"${run[@]}" "${MMD[@]}" -i "$esp" ::/android/extras
"${run[@]}" "${MCOPY[@]}" -s -i "$esp" "$out"/* ::/android/extras/
sync
echo "Written to the ESP of $target (android/extras). Android installs them on its next boot."
