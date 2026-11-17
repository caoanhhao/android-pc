#!/usr/bin/env bash
# Collect NVIDIA's userspace Vulkan driver for the NVIDIA (proprietary) entry (Phase L3,
# docs/nvidia-native.md): aosp-device/hals/nvgpu/hal.mk installs these libraries in
# /vendor/lib64/nvidia, where vulkan.nvidia loads them through nvglibc (Phase L2).
#
#   tools/nvidia-userspace-build.sh [NV_VERSION]
#
# NV_VERSION must be the version of the nvidia-open kernel modules tools/nvidia-build.sh builds
# (default: the build host's nvidia-utils). The files come from the host's /usr/lib when the host
# runs that version, otherwise from NVIDIA's NVIDIA-Linux-x86_64-<ver>.run (downloaded once into
# out/downloads/). GLVND's libEGL and libGLdispatch come from the build host.
#
# Output: aosp/device/android-pc/nvidia-userspace/ (never committed: NVIDIA's userspace is for
# this personal stick only, not for redistribution)
#   lib/      each library under its soname (libGLX_nvidia.so.0, libnvidia-glcore.so.<ver>, ...)
#   VERSION
# The glibc libraries these need are nvglibc's trampolines on the device; the ICD and EGL vendor
# JSONs are aosp-device/hals/nvgpu/{nvidia_icd,10_nvidia}.json.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DST=$ROOT/aosp/device/android-pc/nvidia-userspace
WORK=$ROOT/out/nvidia-userspace
DL=$ROOT/out/downloads
NV_VERSION=${1:-$(pacman -Q nvidia-utils | awk '{print $2}' | sed 's/-[0-9]*$//')}
mkdir -p "$WORK" "$DL"

# What the Vulkan driver loads. The Vulkan entry points live in libGLX_nvidia.so, which brings the
# driver up through EGL: GLVND's libEGL.so.1, then libEGL_nvidia.so and libnvidia-eglcore.so;
# without them vk_icdGetInstanceProcAddr returns no vkCreateInstance. Ray tracing (rtcore) is
# loaded when a device is created.
NV_LIBS=(libGLX_nvidia.so.$NV_VERSION libnvidia-glcore.so.$NV_VERSION libnvidia-glsi.so.$NV_VERSION
         libEGL_nvidia.so.$NV_VERSION libnvidia-eglcore.so.$NV_VERSION
         libnvidia-tls.so.$NV_VERSION libnvidia-glvkspirv.so.$NV_VERSION
         libnvidia-gpucomp.so.$NV_VERSION libnvidia-rtcore.so.$NV_VERSION
         libnvidia-allocator.so.$NV_VERSION)
if [[ -f /usr/lib/libnvidia-glcore.so.$NV_VERSION ]]; then
    NVSRC=/usr/lib
else
    run=NVIDIA-Linux-x86_64-$NV_VERSION.run
    if [[ ! -f $DL/$run ]]; then
        curl -sSf -o "$DL/$run.part" "https://download.nvidia.com/XFree86/Linux-x86_64/$NV_VERSION/$run"
        mv "$DL/$run.part" "$DL/$run"
    fi
    if [[ ! -d $WORK/nv-$NV_VERSION ]]; then
        (cd "$WORK" && sh "$DL/$run" --extract-only --target "nv-$NV_VERSION.tmp" >/dev/null)
        mv "$WORK/nv-$NV_VERSION.tmp" "$WORK/nv-$NV_VERSION"
    fi
    NVSRC=$WORK/nv-$NV_VERSION
fi

/usr/bin/rm -rf "$DST.tmp"
mkdir -p "$DST.tmp/lib"
# Each library under its soname, the name the loaders open: no symlinks, which
# PRODUCT_COPY_FILES would turn into copies.
for l in "${NV_LIBS[@]}"; do
    so=$(objdump -p "$NVSRC/$l" | awk '$1 == "SONAME" { print $2 }')
    cp "$NVSRC/$l" "$DST.tmp/lib/${so:-$l}"
done
for l in libEGL.so.1 libGLdispatch.so.0; do
    cp -L "/usr/lib/$l" "$DST.tmp/lib/"
done

cat > "$DST.tmp/VERSION" <<EOF
nvidia $NV_VERSION ($NVSRC)
libglvnd $(pacman -Q libglvnd | awk '{print $2}')
built $(date -Iseconds)
EOF
chmod -R u=rwX,go=rX "$DST.tmp"
/usr/bin/rm -rf "$DST"
mv "$DST.tmp" "$DST"
echo "nvidia-userspace: $DST ($(du -sh "$DST" | cut -f1))"
cat "$DST/VERSION"
