#!/usr/bin/env bash
# Phase L3 host test (docs/nvidia-native.md): vulkan.nvidia (aosp-device/hals/nvgpu/vulkan) and
# minigbm's "nvidia" backend (aosp/external/minigbm/nvidia.c, tools/patches/
# aosp17-minigbm-nvidia.patch) built for the build host with gcc and run on its RTX 4070:
# NVIDIA's ICD through plain dlopen (on the device it is nvglibc), minigbm through its gbm API
# on nvidia-drm's render node. The test (aosp-device/hals/nvgpu/hosttest/nvvk_hosttest.cpp)
# plays Android's Vulkan loader, SurfaceFlinger and the mapper and checks every pixel.
#
#   tools/nvgpu-hosttest.sh            build and run
#   NVVK_VERBOSE=1 tools/nvgpu-hosttest.sh
#
# Small build (a few files), pinned to the E-cores like every build here. The shared AOSP tree is
# not modified.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
A=$ROOT/aosp
G=$A/external/minigbm
N=$ROOT/aosp-device/hals/nvgpu
OUT=${OUT:-${XDG_RUNTIME_DIR:-/tmp}/nvgpu-hosttest}
mkdir -p "$OUT"

# minigbm with the nvidia backend: the tree's own if the patch is applied there, else a copy with it
if [[ ! -f $G/nvidia.c ]]; then
    rsync -a --delete "$G/" "$OUT/minigbm/"
    patch -s -d "$OUT/minigbm" -p1 < "$ROOT/tools/patches/aosp17-minigbm-nvidia.patch"
    G=$OUT/minigbm
fi

inc=(-I"$N/include" -I"$G" -I"$A/external/vulkan-headers/include"
     -I"$A/frameworks/native/vulkan/include" -I"$A/hardware/libhardware/include"
     -I"$A/system/core/libcutils/include" -I"$A/system/core/libsystem/include")
drm=($(pkg-config --cflags libdrm))
pin=(taskset -c 8-15 nice -n 10)

srcs_c=(drv.c drv_helpers.c drv_array_helpers.c dumb_driver.c nvidia.c nouveau.c virtgpu.c
        virtgpu_cross_domain.c virtgpu_virgl.c backend_mock.c gbm.c gbm_helpers.c)
objs=()
for f in "${srcs_c[@]}"; do
    o=$OUT/${f%.c}.o
    "${pin[@]}" gcc -c -O2 -g -std=gnu11 -D_GNU_SOURCE -D_FILE_OFFSET_BITS=64 -Wall -Wno-unused-parameter \
        "${inc[@]}" "${drm[@]}" "$G/$f" -o "$o"
    objs+=("$o")
done
for f in vulkan/nvvk_hal.cpp vulkan/nvvk_wsi.cpp vulkan/nvvk_buffer.cpp hosttest/nvvk_hosttest.cpp; do
    o=$OUT/$(basename "${f%.cpp}").o
    "${pin[@]}" g++ -c -O2 -g -std=c++20 -Wall -Wextra -Wno-unused-parameter -Wno-missing-field-initializers \
        -DVK_USE_PLATFORM_ANDROID_KHR "${inc[@]}" "${drm[@]}" "$N/$f" -o "$o"
    objs+=("$o")
done
"${pin[@]}" g++ -o "$OUT/nvvk_hosttest" "${objs[@]}" $(pkg-config --libs libdrm) -ldl -lpthread
echo "== $OUT/nvvk_hosttest"
exec "${pin[@]}" "$OUT/nvvk_hosttest"
