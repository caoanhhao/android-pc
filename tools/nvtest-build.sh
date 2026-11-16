#!/bin/bash
# Build pcnvtest (aosp-device/pc/nvtest, Phase L1) for glibc with the build host's gcc, into
# aosp/device/android-pc/nvtest/, where aosp-device/pc/pc.mk copies it to /vendor/bin. It runs on
# the PC through the sidecar's glibc root (/vendor/bin/nvsidecar: ld-linux, libvulkan, NVIDIA's
# ICD; tools/nvsidecar-build.sh), which carries the build host's glibc. Vulkan headers come from
# AOSP's external/vulkan-headers, the compute shader is compiled with the host's glslc.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC=$ROOT/aosp-device/pc/nvtest
OUT=$ROOT/aosp/device/android-pc/nvtest
TMP=$(mktemp -d)
trap '/usr/bin/rm -rf "$TMP"' EXIT
mkdir -p "$OUT"
glslc -O --target-env=vulkan1.1 -o "$TMP/fill.spv" "$SRC/fill.comp"
(cd "$TMP" && xxd -i -n fill_comp_spv fill.spv > fill_comp.h)
gcc -O2 -Wall -Wextra -Wno-missing-field-initializers -std=gnu11 \
    -I"$ROOT/aosp/external/vulkan-headers/include" -I"$TMP" \
    -o "$OUT/pcnvtest" "$SRC/pcnvtest.c" -lvulkan -lpthread
echo "pcnvtest: $OUT/pcnvtest ($(ldd "$OUT/pcnvtest" | grep -c '=>') libraries)"
