#!/bin/bash
# Phase L2 (docs/nvidia-native.md): nvglibc on the build host. Runs the Soong-built (bionic,
# vendor) nvglibc_vktest against the host's RTX 4070 and NVIDIA driver, in a bwrap sandbox that
# looks like the device: /system and /vendor/lib64 from the product out directory (bionic: the
# bootstrap linker and libc, which carry tools/patches/aosp17-bionic-static-tls-surplus.patch)
# and K2's glibc root (tools/nvsidecar-build.sh) at /vendor/bin/nvsidecar, where nvglibc finds
# the NVIDIA driver and its EGL vendor JSON by default. HOSTLIB=1 uses the host's /usr/lib
# instead. Then the same test source built for glibc, as the baseline. The host driver is only
# read.
# Needs: m libnvglibc libnvglibc_{c,dl,gcc_s,ld,m,pthread,rt,x11,xext} nvglibc_vktest
#        $P/system/bin/bootstrap/linker64 $P/system/lib64/bootstrap/libc.so
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
P=${P:-$ROOT/aosp/out/target/product/pc_x86_64}
SRC=$ROOT/aosp-device/hals/nvglibc
TMP=$(mktemp -d)
trap '/usr/bin/rm -rf "$TMP"' EXIT
mkdir -p "$TMP/linkerconfig" "$TMP/home"
: > "$TMP/linkerconfig/ld.config.txt"   # no namespaces: bionic's default search paths

devs=(); for d in /dev/nvidia* /dev/dri; do [ -e "$d" ] && devs+=(--dev-bind "$d" "$d"); done
SIDECAR=$ROOT/aosp/device/android-pc/nvsidecar
if [ -z "${HOSTLIB:-}" ] && [ -f "$SIDECAR/VERSION" ]; then
    driver=(--ro-bind "$SIDECAR" /vendor/bin/nvsidecar)
else
    driver=(--ro-bind /usr/lib /hostlib --ro-bind /usr/share/glvnd /share/glvnd
            --setenv NVGLIBC_DRIVER_PATH /hostlib)
fi
echo "== bionic: /vendor/bin/nvglibc_vktest through nvglibc"
bwrap --unshare-pid --die-with-parent \
    --ro-bind "$P/system" /system --tmpfs /vendor --ro-bind "$P/vendor/lib64" /vendor/lib64 \
    --ro-bind "$P/vendor/bin/nvglibc_vktest" /vendor/bin/nvglibc_vktest \
    --ro-bind "$P/system/bin/bootstrap/linker64" /apex/com.android.runtime/bin/linker64 \
    --ro-bind "$P/system/lib64/bootstrap" /apex/com.android.runtime/lib64/bionic \
    --ro-bind "$TMP/linkerconfig" /linkerconfig \
    --dev /dev "${devs[@]}" --proc /proc --ro-bind /sys /sys \
    --bind "$TMP/home" /home --tmpfs /tmp --chdir / --clearenv \
    --setenv PATH /system/bin --setenv HOME /home "${driver[@]}" \
    ${NVGLIBC_DEBUG:+--setenv NVGLIBC_DEBUG 1} \
    /vendor/bin/nvglibc_vktest


echo "== glibc baseline (same source, host gcc)"
gcc -O2 -I"$ROOT/aosp/external/vulkan-headers/include" "$SRC/test/nvglibc_vktest.c" -o "$TMP/vktest_glibc" -ldl
env -i HOME="$TMP/home" "$TMP/vktest_glibc"
echo "== both passed"
