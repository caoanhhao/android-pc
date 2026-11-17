#!/bin/bash
# nvglibc on Waydroid's own Android 13 bionic, on the build host (docs/waydroid-nvidia.md): runs
# nvglibc_vktest from tools/waydroid/nvidia-build.sh's overlay against the host's RTX 4070 in a
# bwrap sandbox with Waydroid's linker64/libc (no static TLS surplus patch), its /system/lib64
# and /vendor/lib64, then the overlay on top. libnvglibc_tls.so comes in through LD_PRELOAD, as
# libvulkan.so's DT_NEEDED brings it into Waydroid's processes. Exercises the TLS surplus path of
# hals/nvglibc/src/tls_surplus.c without the container (and without sudo: logcat needs it there).
# NVGLIBC_DEBUG=1 for the loader's log.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT=$ROOT/out/waydroid-nvidia
OV=$OUT/overlay
SR=$OUT/sysroot
WD_IMAGES=${WD_IMAGES:-/var/lib/waydroid/images}

if [[ ! -f $SR/apex/com.android.runtime/bin/linker64 ]]; then
    mkdir -p "$SR/system" "$SR/vendor" "$SR/apex"
    # debugfs cannot chown as a user and says so per file; the contents are complete
    debugfs -R "rdump /system/lib64 $SR/system" "$WD_IMAGES/system.img" 2>/dev/null || true
    debugfs -R "rdump /system/apex/com.android.runtime $SR/apex" "$WD_IMAGES/system.img" 2>/dev/null || true
    debugfs -R "rdump /lib64 $SR/vendor" "$WD_IMAGES/vendor.img" 2>/dev/null || true
fi
TMP=$(mktemp -d)
trap '/usr/bin/rm -rf "$TMP"' EXIT
mkdir -p "$TMP/linkerconfig" "$TMP/home"
: > "$TMP/linkerconfig/ld.config.txt"   # no namespaces: bionic's default search paths

devs=(); for d in /dev/nvidia* /dev/dri /dev/udmabuf; do [ -e "$d" ] && devs+=(--dev-bind "$d" "$d"); done
# Waydroid's libraries with the overlay on top, as hard links (same file system as out/)
M=$OUT/hosttest-root
/usr/bin/rm -rf "$M"
mkdir -p "$M/system" "$M/vendor/bin"
cp -al "$SR/system/lib64" "$M/system/"
cp -al "$SR/vendor/lib64" "$M/vendor/"
cp -a --remove-destination "$OV/." "$M/"
# the TLS library's DT_NEEDED stand-in (nvidia-build.sh's empty stub; the real one pulls in half
# of the platform)
victim=$(readelf -d "$OV/system/lib64/libnvglibc_tls.so" | sed -n 's/.*NEEDED.*\[\(.*\)\]/\1/p')
cp --remove-destination "$OUT/stubs/$victim" "$M/system/lib64/$victim"

bwrap --unshare-pid --die-with-parent \
    --ro-bind "$M/system" /system --ro-bind "$M/vendor" /vendor \
    --ro-bind "$SR/apex/com.android.runtime" /apex/com.android.runtime \
    --ro-bind "$TMP/linkerconfig" /linkerconfig \
    --dev /dev "${devs[@]}" --proc /proc --ro-bind /sys /sys \
    --bind "$TMP/home" /home --tmpfs /tmp --chdir / --clearenv \
    --setenv PATH /system/bin --setenv HOME /home \
    --setenv LD_PRELOAD /system/lib64/libnvglibc_tls.so \
    ${NVGLIBC_DEBUG:+--setenv NVGLIBC_DEBUG 1} \
    /apex/com.android.runtime/bin/linker64 /vendor/bin/nvglibc_vktest
