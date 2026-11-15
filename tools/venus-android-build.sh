#!/bin/bash
# Build Mesa's Venus Vulkan driver (vulkan.virtio.so) for the NVIDIA proprietary entry (Phase K3,
# docs/nvidia-proprietary.md): Mesa 26.2.6 with waydroid-nvidia's Venus/vtest patches and
# android-pc's minigbm handle reader (tools/patches/venus/), cross-built with the Android NDK.
#
#   tools/venus-android-build.sh            build, then copy the stripped driver to
#                                           aosp-device/venus/prebuilt/x86_64/vulkan.virtio.so
#
# Only x86_64: the product is x86_64-only, and arm64 apps reach Vulkan through
# libndk_translation's proxy libvulkan, which calls the x86_64 loader and driver.
# Not a Soong build: AOSP's own Mesa (external/mesa3d, 26.1, NVK) has no Venus Android.bp,
# and two Mesa trees in one Soong graph collide. The NDK build links only libraries the
# vendor Vulkan APEX already gets (libdrm, libhardware, liblog, libnativewindow, libsync).
#
# Needs the NDK (r30, $NDK, default /opt/android-ndk), meson, ninja, python3-mako.
# Sources: $MESA_DIR (default out/venus/mesa), cloned from $MESA_URL at mesa-26.2.6; with
# $MESA_REFERENCE (default ~/projects/waydroid-nvidia/src/mesa if it exists) the objects come
# from that local clone instead of the network. Runs under tools/limited-build.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NDK=${NDK:-/opt/android-ndk}
MESA_TAG=mesa-26.2.6
MESA_URL=${MESA_URL:-https://gitlab.freedesktop.org/mesa/mesa.git}
MESA_DIR=${MESA_DIR:-$ROOT/out/venus/mesa}
REF=${MESA_REFERENCE:-$HOME/projects/waydroid-nvidia/src/mesa}
PATCHES=$ROOT/tools/patches/venus
BUILD=$MESA_DIR/build-android-x86_64
OUT=$ROOT/aosp-device/venus/prebuilt/x86_64
TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
[[ -x $TC/x86_64-linux-android33-clang ]] || { echo "venus-android-build: no NDK at $NDK" >&2; exit 1; }

# 1. Mesa at the tag plus the patches, on branch pc-venus. A checkout whose patch list changed
# is rebuilt from the tag (the branch is ours; nothing else lives there).
stamp=$(cat "$PATCHES"/*.patch | sha256sum | cut -c1-16)
if [[ ! -d $MESA_DIR/.git ]]; then
    mkdir -p "$(dirname "$MESA_DIR")"
    if [[ -d $REF/.git ]]; then
        git clone -q --no-checkout "$REF" "$MESA_DIR"
    else
        git clone -q --no-checkout "$MESA_URL" "$MESA_DIR"
    fi
fi
if [[ $(git -C "$MESA_DIR" config --get pc.patches || true) != "$stamp" ]]; then
    git -C "$MESA_DIR" fetch -q "$MESA_URL" "refs/tags/$MESA_TAG:refs/tags/$MESA_TAG" 2>/dev/null ||
        git -C "$MESA_DIR" rev-parse -q --verify "$MESA_TAG" >/dev/null
    git -C "$MESA_DIR" checkout -q -B pc-venus "$MESA_TAG"
    git -C "$MESA_DIR" -c user.name=android-pc -c user.email=android-pc@localhost am -q "$PATCHES"/*.patch
    git -C "$MESA_DIR" config pc.patches "$stamp"
    rm -rf "$BUILD"
fi

# 2. meson cross file for the NDK (API 33: the oldest API with every NDK symbol Venus uses;
# libc++ static, so no libc++_shared.so on the device).
mkdir -p "$BUILD"
cat > "$MESA_DIR/android-x86_64.cross" <<EOF
[binaries]
ar = '$TC/llvm-ar'
c = ['$TC/x86_64-linux-android33-clang']
cpp = ['$TC/x86_64-linux-android33-clang++', '-fno-exceptions', '-fno-unwind-tables', '-fno-asynchronous-unwind-tables', '--start-no-unused-arguments', '-static-libstdc++', '--end-no-unused-arguments']
c_ld = 'lld'
cpp_ld = 'lld'
strip = '$TC/llvm-strip'
pkg-config = 'false'

[host_machine]
system = 'android'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
if [[ ! -f $BUILD/build.ninja ]]; then
    (cd "$MESA_DIR" && meson setup "$BUILD" --cross-file android-x86_64.cross \
        -Dbuildtype=debugoptimized -Dplatforms=android -Dplatform-sdk-version=33 \
        -Dandroid-stub=true -Dandroid-libbacktrace=disabled \
        -Degl=disabled -Dgles1=disabled -Dgles2=disabled -Dopengl=false \
        -Dgallium-drivers= -Dvulkan-drivers=virtio -Dallow-fallback-for=libdrm \
        -Dllvm=disabled -Dzstd=disabled -Dvulkan-layers= -Dtools=)
fi
"$ROOT/tools/limited-build.sh" ninja -C "$BUILD" -j8 src/virtio/vulkan/libvulkan_virtio.so

# 3. The stripped driver for the vendor Vulkan APEX (aosp-device/venus/Android.bp); the
# unstripped one stays in $BUILD for symbolizing tombstones.
mkdir -p "$OUT"
"$TC/llvm-strip" --strip-debug -o "$OUT/vulkan.virtio.so" "$BUILD/src/virtio/vulkan/libvulkan_virtio.so"
{
    echo "mesa $(git -C "$MESA_DIR" describe --tags --always) (branch pc-venus, patches $stamp)"
    echo "ndk $(sed -n 's/^Pkg.Revision = //p' "$NDK/source.properties")"
    echo "sha256 $(sha256sum "$OUT/vulkan.virtio.so" | cut -d' ' -f1)"
} > "$OUT/VERSION"
readelf -d "$OUT/vulkan.virtio.so" | grep -E 'NEEDED|SONAME'
ls -l "$OUT/vulkan.virtio.so"; cat "$OUT/VERSION"
