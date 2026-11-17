#!/usr/bin/env bash
# Build NVIDIA's own Vulkan driver for Waydroid's Android 13 (docs/waydroid-nvidia.md) and stage
# the overlay tools/waydroid/nvidia-install.sh copies into /var/lib/waydroid/overlay.
#
#   tools/waydroid/nvidia-build.sh [--no-aosp]
#
# Steps (each heavy one through tools/limited-build.sh, one build at a time):
#   1. AOSP 17 tree: libnvglibc and its soname libraries, vulkan.nvidia.waydroid, nvglibc_vktest
#      (--no-aosp reuses what aosp/out has)
#   2. tools/waydroid/nvtls-rewrite (cargo)
#   3. NVIDIA's userspace (tools/nvidia-userspace-build.sh), initial-exec TLS rewritten
#   4. Waydroid's /system/lib64/libvulkan.so and libEGL.so (read from its system.img) with
#      libnvglibc_tls.so as a DT_NEEDED
#   5. NDK: libnvglibc_tls.so, libgbm_mesa_wrapper.so (aosp-device/waydroid/gralloc), the
#      composer wrapper (aosp-device/waydroid/composer; the vendor image's composer, read from
#      its vendor.img, goes beside it as ...-service.real) and the test
#      out/waydroid-nvidia/ahb_vktest (aosp-device/waydroid/test)
# Output: out/waydroid-nvidia/overlay/{system,vendor} (NVIDIA's libraries included: never
# committed or redistributed).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT=$ROOT/out/waydroid-nvidia
OV=$OUT/overlay
WD_IMAGES=${WD_IMAGES:-/var/lib/waydroid/images}
NDK=${NDK:-/opt/android-ndk}
CC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android33-clang
AOSP_OUT=$ROOT/aosp/out/target/product/pc_x86_64
NVUS=$ROOT/aosp/device/android-pc/nvidia-userspace
LB=$ROOT/tools/limited-build.sh

wait_builds() {
    while pgrep -x 'soong_ui|soong_build|siso|ninja|cargo|rustc' >/dev/null; do
        echo "another build is running, waiting"; sleep 60
    done
}

if [[ ${1:-} != --no-aosp ]]; then
    wait_builds
    STEPS="libnvglibc+libnvglibc_c+libnvglibc_dl+libnvglibc_gcc_s+libnvglibc_ld+libnvglibc_m+libnvglibc_pthread+libnvglibc_rt+libnvglibc_x11+libnvglibc_xext+vulkan.nvidia.waydroid+nvglibc_vktest" \
        "$ROOT/tools/aosp17-build.sh"
fi

wait_builds
(cd "$ROOT/tools/waydroid/nvtls-rewrite" && "$LB" cargo build --release -q)
RW=$ROOT/tools/waydroid/nvtls-rewrite/target/release/nvtls-rewrite

[[ -f $NVUS/lib/libGLX_nvidia.so.0 ]] || "$ROOT/tools/nvidia-userspace-build.sh"
host_nv=$(pacman -Q nvidia-utils | awk '{print $2}' | sed 's/-[0-9]*$//')
grep -q "nvidia $host_nv " "$NVUS/VERSION" || {
    echo "NVIDIA userspace in $NVUS is not the host's $host_nv: run tools/nvidia-userspace-build.sh" >&2
    exit 1
}

/usr/bin/rm -rf "$OV"
mkdir -p "$OV/vendor/lib64/hw" "$OV/vendor/lib64/nvglibc" "$OV/vendor/bin" "$OV/system/lib64" \
         "$OUT/stubs"

# 1: the HAL and nvglibc
cp "$AOSP_OUT/vendor/lib64/waydroid/vulkan.nvidia.waydroid.so" "$OV/vendor/lib64/hw/vulkan.nvidia.so"
cp "$AOSP_OUT/vendor/lib64/libnvglibc.so" "$OV/vendor/lib64/"
cp "$AOSP_OUT"/vendor/lib64/nvglibc/*.so "$OV/vendor/lib64/nvglibc/"
cp "$AOSP_OUT/vendor/bin/nvglibc_vktest" "$OV/vendor/bin/"

# 3: NVIDIA's userspace, TLS rewritten
"$RW" tls "$OV/vendor/lib64/nvidia" "$NVUS"/lib/*
cp "$ROOT/aosp-device/hals/nvgpu/10_nvidia.json" "$OV/vendor/lib64/nvidia/"
# NVIDIA's device nodes, made inside the container at early-init
mkdir -p "$OV/vendor/etc/init"
cp "$ROOT/aosp-device/waydroid/init/waydroid-nvidia-nodes.sh" "$OV/vendor/bin/"
cp "$ROOT/aosp-device/waydroid/init/waydroid-nvidia.rc" "$OV/vendor/etc/init/"

# 4: libvulkan.so and libEGL.so load libnvglibc_tls.so at process start (libEGL for processes
# that reach Vulkan only through ANGLE, such as the composer)
victim=
for lib in libvulkan.so libEGL.so; do
    debugfs -R "dump /system/lib64/$lib $OUT/${lib%.so}.orig.so" "$WD_IMAGES/system.img" 2>/dev/null
    [[ -s $OUT/${lib%.so}.orig.so ]] || { echo "cannot read $lib from $WD_IMAGES/system.img" >&2; exit 1; }
    v=$("$RW" add-needed "$OUT/${lib%.so}.orig.so" "$OV/system/lib64/$lib" libnvglibc_tls.so)
    [[ -z $victim || $v == "$victim" ]] || {
        echo "$lib: libnvglibc_tls.so stands in for $v, not $victim as in libvulkan.so" >&2; exit 1; }
    victim=$v
    echo "$lib: libnvglibc_tls.so stands in for $victim, which it needs itself"
done

# 5: NDK libraries. An empty stub gives libnvglibc_tls.so its DT_NEEDED on $victim.
"$CC" -shared -nostdlib -Wl,-soname,"$victim" -o "$OUT/stubs/$victim" -x c /dev/null
"$CC" -O2 -fPIC -shared -nostdlib -Wall -Werror -Wl,-soname,libnvglibc_tls.so \
    -Wl,--no-as-needed -L"$OUT/stubs" -l:"$victim" \
    -o "$OV/system/lib64/libnvglibc_tls.so" "$ROOT/aosp-device/waydroid/tls/nvglibc_tls.c"
"$CC" -O2 -g -fPIC -shared -std=c11 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Wno-missing-field-initializers -fvisibility=hidden \
    -I"$ROOT/aosp-device/hals/nvgpu/include" -I"$ROOT/aosp-device/waydroid/gralloc" \
    -Wl,-soname,libgbm_mesa_wrapper.so -Wl,--no-undefined \
    -o "$OV/vendor/lib64/libgbm_mesa_wrapper.so" \
    "$ROOT/aosp-device/waydroid/gralloc/nvgbm_wrapper.c" -llog -ldl
"$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" --strip-unneeded \
    "$OV/vendor/lib64/libgbm_mesa_wrapper.so"
# the composer with libEGL.so preloaded (its EGL copies the mouse cursor)
COMPOSER=bin/hw/android.hardware.graphics.composer@2.1-service
mkdir -p "$OV/vendor/bin/hw"
debugfs -R "dump /$COMPOSER $OV/vendor/$COMPOSER.real" "$WD_IMAGES/vendor.img" 2>/dev/null
[[ -s $OV/vendor/$COMPOSER.real ]] || { echo "cannot read $COMPOSER from $WD_IMAGES/vendor.img" >&2; exit 1; }
"$CC" -O2 -Wall -Werror -o "$OV/vendor/$COMPOSER" \
    "$ROOT/aosp-device/waydroid/composer/composer-preload-wrapper.c"
# the gralloc -> Vulkan -> EGL check (run as /data/local/tmp/ahb_vktest, not in the overlay)
"$CC" -O1 -Wall -Werror -o "$OUT/ahb_vktest" "$ROOT/aosp-device/waydroid/test/ahb_vktest.c" \
    -lvulkan -landroid -lnativewindow -lEGL

cat > "$OUT/VERSION" <<EOF
android-pc $(git -C "$ROOT" describe --always --dirty)
$(cat "$NVUS/VERSION")
built $(date -Iseconds)
EOF
echo "overlay: $OV ($(du -sh "$OV" | cut -f1))"
find "$OV" -type f | sed "s|$OV||" | sort
