#!/usr/bin/env bash
# Build the NVIDIA glibc sidecar (Phase K2, docs/nvidia-proprietary.md): a minimal glibc root
# with NVIDIA's userspace Vulkan driver and the patched vtest server, installed read-only at
# /vendor/bin/nvsidecar by aosp-device/hals/nvsidecar/hal.mk.
#
#   tools/nvsidecar-build.sh [NV_VERSION]
#
# NV_VERSION must be the version of the nvidia-open kernel modules K1 builds (default: the
# build host's nvidia-utils). The NVIDIA files come from the host's /usr/lib when the host
# runs that version, otherwise from NVIDIA's NVIDIA-Linux-x86_64-<ver>.run (downloaded once
# into out/downloads/). glibc and the few other libraries come from the build host.
#
# Output: aosp/device/android-pc/nvsidecar/ (never committed: NVIDIA's userspace is for this
# personal stick only, not for redistribution)
#   bin/virgl_test_server, bin/virgl_render_server   linked with the interpreter
#                                                    /vendor/bin/nvsidecar/lib/ld-linux-x86-64.so.2
#   bin/nvidia-smi                                   NVIDIA's binary, run through lib/ld-linux
#   lib/                                             glibc, libvulkan, libvirglrenderer, NVIDIA
#   share/vulkan/icd.d/nvidia_icd.json               absolute path to lib/libGLX_nvidia.so.0
#   VERSION
#
# virglrenderer 1.3.0 with waydroid-nvidia's vtest patches (tools/patches/nvsidecar-vrend-*:
# dma-buf import v5, dma-buf image allocation v6, sync_file export v7) is built in
# out/nvsidecar/ under tools/limited-build.sh (E-cores, -j8).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX=/vendor/bin/nvsidecar
DST=$ROOT/aosp/device/android-pc/nvsidecar
WORK=$ROOT/out/nvsidecar
DL=$ROOT/out/downloads
NV_VERSION=${1:-$(pacman -Q nvidia-utils | awk '{print $2}' | sed 's/-[0-9]*$//')}
VIRGL_TAG=1.3.0
VIRGL_GIT=${VIRGL_GIT:-https://gitlab.freedesktop.org/virgl/virglrenderer.git}

mkdir -p "$WORK" "$DL"
need_build() {   # builds run one at a time on this host (CLAUDE.md)
    [[ -f $WORK/build/build.ninja ]] && ninja -C "$WORK/build" -n 2>/dev/null | grep -q "no work to do" && return
    for p in soong_ui siso ninja cargo rustc; do
        if pgrep -x "$p" >/dev/null; then echo "nvsidecar-build: a build is running ($p), wait for it" >&2; exit 1; fi
    done
    "$ROOT"/tools/limited-build.sh ninja -j8 -C "$WORK/build"
}

# --- virglrenderer --------------------------------------------------------------------------
SRC=$WORK/virglrenderer
if [[ ! -d $SRC ]]; then
    git clone --quiet --branch "$VIRGL_TAG" "$VIRGL_GIT" "$SRC"
    git -C "$SRC" switch --quiet -c nvsidecar
    git -C "$SRC" -c user.name=nvsidecar -c user.email=nvsidecar@localhost \
        am --quiet "$ROOT"/tools/patches/nvsidecar-vrend-*.patch
fi
if [[ ! -f $WORK/build/build.ninja ]]; then
    # The interpreter and run path point into the sidecar, so the binaries run as they are on
    # Android (virgl_test_server execs virgl_render_server itself).
    meson setup "$WORK/build" "$SRC" \
        --prefix=$PREFIX --libdir=lib --libexecdir=bin --bindir=bin \
        --buildtype=release \
        -Dvenus=true -Dvideo=false -Dplatforms=egl -Drender-server-worker=process
    # Set after the first configure: meson runs its compiler sanity check with the link
    # arguments, and the sidecar's interpreter does not exist on the build host.
    meson configure "$WORK/build" \
        -Dc_link_args="-Wl,--dynamic-linker=$PREFIX/lib/ld-linux-x86-64.so.2 -Wl,-rpath,$PREFIX/lib"
fi
need_build
/usr/bin/rm -rf "$WORK/install"
DESTDIR=$WORK/install meson install --quiet -C "$WORK/build" --no-rebuild

# --- NVIDIA userspace ------------------------------------------------------------------------
# What the Vulkan driver loads (LD_DEBUG=files of a Venus session in the bwrap sandbox of
# tools/nvsidecar-hosttest.sh), plus nvidia-smi's NVML. The Vulkan entry points live in
# libGLX_nvidia.so, which brings the driver up through EGL: GLVND's libEGL.so.1, then
# libEGL_nvidia.so and libnvidia-eglcore.so; without them vk_icdGetInstanceProcAddr returns no
# vkCreateInstance. Ray tracing (rtcore) is loaded when a device is created.
NV_LIBS=(libGLX_nvidia.so.$NV_VERSION libnvidia-glcore.so.$NV_VERSION libnvidia-glsi.so.$NV_VERSION
         libEGL_nvidia.so.$NV_VERSION libnvidia-eglcore.so.$NV_VERSION
         libnvidia-tls.so.$NV_VERSION libnvidia-glvkspirv.so.$NV_VERSION
         libnvidia-gpucomp.so.$NV_VERSION libnvidia-rtcore.so.$NV_VERSION
         libnvidia-allocator.so.$NV_VERSION libnvidia-ml.so.$NV_VERSION)
if [[ -f /usr/lib/libnvidia-glcore.so.$NV_VERSION ]]; then
    NVSRC=/usr/lib NVBIN=/usr/bin ICD=/usr/share/vulkan/icd.d/nvidia_icd.json
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
    NVSRC=$WORK/nv-$NV_VERSION NVBIN=$WORK/nv-$NV_VERSION ICD=$WORK/nv-$NV_VERSION/nvidia_icd.json.template
fi

# --- assemble --------------------------------------------------------------------------------
/usr/bin/rm -rf "$DST.tmp"
mkdir -p "$DST.tmp"/{bin,lib,share/vulkan/icd.d}
cp -a "$WORK/install$PREFIX/bin/virgl_test_server" "$WORK/install$PREFIX/bin/virgl_render_server" "$DST.tmp/bin/"
cp -L "$WORK/install$PREFIX/lib/libvirglrenderer.so.1" "$DST.tmp/lib/"
cp "$NVBIN/nvidia-smi" "$DST.tmp/bin/"
# Each library under its soname, the name the loaders open (libGLX_nvidia.so.0,
# libnvidia-ml.so.1, libnvidia-glcore.so.<ver>, ...): no symlinks, which PRODUCT_COPY_FILES
# would turn into copies.
for l in "${NV_LIBS[@]}"; do
    so=$(objdump -p "$NVSRC/$l" | awk '$1 == "SONAME" { print $2 }')
    cp "$NVSRC/$l" "$DST.tmp/lib/${so:-$l}"
done
# The Khronos loader, GLVND's EGL, and the X11 libraries libGLX_nvidia.so dlopens while it sets up Vulkan
# (without them vk_icdGetInstanceProcAddr returns no vkCreateInstance).
for l in libvulkan.so.1 libEGL.so.1 libGLdispatch.so.0 libX11-xcb.so.1 libxcb-dri3.so.0 libxcb-glx.so.0 libxcb-present.so.0 \
         libxcb-randr.so.0 libxcb-sync.so.1 libxcb-xfixes.so.0; do
    cp -L "/usr/lib/$l" "$DST.tmp/lib/"
done

# Every DT_NEEDED of the root's ELF files, from the build host, until nothing is missing.
# LD_LIBRARY_PATH lets ldd resolve the root's own files first.
while true; do
    missing=$(for f in "$DST.tmp"/bin/* "$DST.tmp"/lib/*.so*; do
        LD_LIBRARY_PATH=$DST.tmp/lib ldd "$f" 2>/dev/null | awk '$2 == "=>" && $3 ~ /^\// { print $3 }'
    done | sort -u | grep -v -e "^$DST.tmp/" -e /ld-linux-x86-64.so.2 || true)
    [[ -z $missing ]] && break
    for m in $missing; do cp -L "$m" "$DST.tmp/lib/$(basename "$m")"; done
done
cp -L /usr/lib/ld-linux-x86-64.so.2 "$DST.tmp/lib/"

# The ICD manifest names the driver by absolute path: the loader then needs no search path.
cat > "$DST.tmp/share/vulkan/icd.d/nvidia_icd.json" <<EOF
{
    "file_format_version" : "1.0.1",
    "ICD": {
        "library_path": "$PREFIX/lib/libGLX_nvidia.so.0",
        "api_version" : "$(sed -n 's/.*"api_version" *: *"\([^"]*\)".*/\1/p' "$ICD")"
    }
}
EOF

# GLVND's EGL vendor list: NVIDIA only (the service sets __EGL_VENDOR_LIBRARY_FILENAMES).
mkdir -p "$DST.tmp/share/glvnd/egl_vendor.d"
cat > "$DST.tmp/share/glvnd/egl_vendor.d/10_nvidia.json" <<EOF
{
    "file_format_version" : "1.0.0",
    "ICD" : {
        "library_path" : "$PREFIX/lib/libEGL_nvidia.so.0"
    }
}
EOF

cat > "$DST.tmp/VERSION" <<EOF
nvidia $NV_VERSION ($NVSRC)
virglrenderer $(git -C "$SRC" describe --tags) (nvsidecar: $(cd "$ROOT/tools/patches" && ls nvsidecar-vrend-* | sed -E 's/^nvsidecar-(vrend-[0-9]+).*/\1/' | tr '\n' ' '))
glibc $(pacman -Q glibc | awk '{print $2}')
built $(date -Iseconds)
EOF
chmod -R u=rwX,go=rX "$DST.tmp"
/usr/bin/rm -rf "$DST"
mv "$DST.tmp" "$DST"
echo "nvsidecar: $DST ($(du -sh "$DST" | cut -f1))"
cat "$DST/VERSION"
