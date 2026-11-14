#!/bin/bash
# Build the android17-6.18 GKI kernel and the vendor modules for aosp_pc_x86_64 (phase F3a)
# from the kernel checkout in gki/ (see docs/usb-boot-aosp17.md), then install them where
# aosp-device/BoardConfig.mk takes them from:
#   aosp/device/android-pc/kernel/6.18/kernel-6.18    the GKI bzImage
#   aosp/device/android-pc/kernel/6.18/system_dlkm/  GKI modules (system_dlkm)
#   aosp/device/android-pc/kernel/6.18/vendor/       Cuttlefish modules + the PC modules
#   aosp/device/android-pc/firmware/                 device firmware for /vendor/firmware
#
# STEPS picks what runs (default "gki virt pc install firmware"); each Bazel step goes through
# tools/limited-build.sh and is retried (the host is flaky). Extra Bazel flags: BAZEL_FLAGS.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GKI=$ROOT/gki
PKG=$GKI/android-pc
DEST=$ROOT/aosp/device/android-pc/kernel/6.18
STEPS=${STEPS:-"gki virt pc install firmware"}
# --notrim: export every symbol the GKI image has (vendor modules use more than the KMI
# symbol lists); --lto=none: much faster and lighter than the release LTO build.
# --defconfig_fragment (Phase G6, docs/nvidia.md): CONFIG_RUST_FW_LOADER_ABSTRACTIONS=y in the
# GKI image itself. It is a bool that adds kernel::firmware to the Rust kernel crate, which
# lives in vmlinux; nova-core (a vendor module) calls it. Every step must pass the same flag,
# or the vendor builds would rebuild their base kernel without it.
FLAGS=(--notrim --lto=none --defconfig_fragment=//android-pc:gki_rust_defconfig ${BAZEL_FLAGS:-})
[ -d "$GKI/common" ] || { echo "gki-build: no kernel checkout in $GKI (see docs/usb-boot-aosp17.md)"; exit 1; }

# Patches to the kernel tree itself (nouveau is an in-tree module), applied once:
# - gki-nouveau-gsp-thermal.patch (Phase G5, docs/nvidia.md): GPU temperature as hwmon
#   temp1_input and a debugfs "perf" entry (GSP-RM's P-state) for GPUs booted with GSP-RM.
# - gki-nova-boot-log.patch (Phase G6): nova-core reports VRAM, WPR2 and how far its GSP
#   boot got at info level (dev_dbg! is compiled out of release kernels).
# - gki-vrealloc-export.patch (Phase G6): exports vrealloc_node_align_noprof, which Rust modules
#   using VVec/KVVec call directly because RUST_INLINE_HELPERS inlines the Rust helpers.
for p in gki-nouveau-gsp-thermal.patch gki-nova-boot-log.patch gki-vrealloc-export.patch; do
  git -C "$GKI/common" apply --reverse --check "$ROOT/tools/patches/$p" 2>/dev/null && continue
  git -C "$GKI/common" apply "$ROOT/tools/patches/$p" || { echo "gki-build: $p does not apply"; exit 1; }
done

# Our Kleaf package: this repo's aosp-device/kernel plus generated files.
rsync -a --delete --exclude pcfb/gen "$ROOT/aosp-device/kernel/" "$PKG/"
cp "$GKI"/common-modules/virtual-device/virtual_device{,_core}.fragment "$PKG/"
# pcfb: efidrm and its drm_sysfb helpers, copied from the kernel tree with these changes:
# - efidrm.c: the DRM driver name, a render node (minigbm allocates there, like on
#   virtio-gpu; on card0 it would become DRM master and lock the composer out), no
#   aperture/fbdev-client calls (APERTURE_HELPERS and the
#   DRM client library are not in the GKI image), no module_platform_driver (pcfb.c
#   registers both the device and the driver);
# - drm_sysfb_modeset.c / drm_sysfb_helper.h: the plane also takes ARGB8888, XBGR8888 and
#   ABGR8888 (the ranchu composer's formats), converted by pcfb_blit() (pcfb_blit.c), and
#   the CRTC may be on without a primary plane (the composer sets the mode first, as
#   virtio-gpu allows).
SYSFB=$GKI/common/drivers/gpu/drm/sysfb
G=$PKG/pcfb/gen
mkdir -p "$G"
cp "$SYSFB"/drm_sysfb.c "$SYSFB"/drm_sysfb_screen_info.c "$G/"
sed -e 's/^#define DRIVER_NAME\t"efidrm"/#define DRIVER_NAME\t"virtio_gpu"/' \
    -e '/#include <linux\/aperture.h>/d' -e '/#include <drm\/clients\/drm_client_setup.h>/d' \
    -e 's/ret = devm_aperture_acquire_for_platform_device(pdev, res->start, vsize);/ret = 0;/' \
    -e '/drm_client_setup(dev, sysfb->fb_format);/d' \
    -e '/^module_platform_driver(efidrm_platform_driver);/d' \
    -e 's/DRIVER_ATOMIC | DRIVER_GEM | DRIVER_MODESET,/DRIVER_ATOMIC | DRIVER_GEM | DRIVER_MODESET | DRIVER_RENDER,/' \
    "$SYSFB/efidrm.c" > "$G/efidrm_impl.h"
sed -e 's/^\t((_num_native) + 1)$/\t((_num_native) + 4)/' \
    "$SYSFB/drm_sysfb_helper.h" > "$G/drm_sysfb_helper.h"
sed -e 's/^\t\tDRM_FORMAT_XRGB8888,$/\t\tDRM_FORMAT_XRGB8888, DRM_FORMAT_ARGB8888, DRM_FORMAT_XBGR8888, DRM_FORMAT_ABGR8888,/' \
    -e 's/^\tret = drm_atomic_helper_check_crtc_primary_plane(new_crtc_state);$/\tret = 0;/' \
    -e 's/^\t\tdrm_fb_blit(&dst, &dst_pitch,/\t\tpcfb_blit(\&dst, \&dst_pitch,/' \
    -e '/^#include "drm_sysfb_helper.h"/a #include "../pcfb.h"' \
    "$SYSFB/drm_sysfb_modeset.c" > "$G/drm_sysfb_modeset.c"
check() { grep -qF -- "$2" "$G/$1" || { echo "gki-build: $1 changed upstream, fix its sed ($2)"; exit 1; }; }
check efidrm_impl.h '"virtio_gpu"'; check efidrm_impl.h 'ret = 0;'; check efidrm_impl.h 'DRIVER_RENDER'
check drm_sysfb_helper.h '+ 4)'; check drm_sysfb_modeset.c 'DRM_FORMAT_ABGR8888,'
check drm_sysfb_modeset.c 'pcfb_blit(&dst'; check drm_sysfb_modeset.c $'\tret = 0;'; check drm_sysfb_modeset.c '#include "../pcfb.h"'

bazel_run() {   # bazel_run TARGET: build and install a dist target, with retries
  for try in 1 2 3; do
    while pgrep -x 'soong_ui|soong_build|siso|ninja|cargo|rustc' >/dev/null || pgrep -f GradleDaemon >/dev/null; do
      echo "other build running, waiting"; sleep 60; done
    echo "=== $1 $(date) free=$(df -BG --output=avail / | tail -1)"
    (cd "$GKI" && "$ROOT/tools/limited-build.sh" tools/bazel --max_idle_secs=300 run "${FLAGS[@]}" "$1") && return 0
    echo "RETRY $try $1"
  done
  echo "STEP_FAILED $1"; exit 1
}

for s in $STEPS; do case $s in
  gki)  bazel_run //common:kernel_x86_64_dist ;;
  virt) bazel_run //common-modules/virtual-device:virtual_device_x86_64_dist ;;
  pc)   bazel_run //android-pc:pc_x86_64_dist ;;
  install)
    D=$GKI/out
    rm -rf "$DEST.new" && mkdir -p "$DEST.new/system_dlkm" "$DEST.new/vendor" || exit 1
    cp "$D/kernel_x86_64/dist/bzImage" "$DEST.new/kernel-6.18" || exit 1
    # GKI modules (system_dlkm), flat as in kernel/prebuilts/6.18/x86_64.
    cp "$D"/kernel_x86_64/dist/*.ko "$DEST.new/system_dlkm/" || exit 1
    cp "$D"/virtual_device_x86_64/dist/*.ko "$DEST.new/vendor/" || exit 1
    # Then every module of the PC build, replacing virtual-device's copies: its config is a
    # superset (same fragments plus pc_x86_64.fragment), and the extra options can add
    # exports (IWLWIFI_LEDS selects MAC80211_LEDS, whose LED triggers iwlmvm needs).
    # Only virtual-device's external modules (goldfish, v4l2loopback, virtio-media) stay.
    cp "$D"/pc_x86_64/dist/*.ko "$DEST.new/vendor/" || exit 1
    rm -rf "$DEST" && mv "$DEST.new" "$DEST"
    echo "installed into $DEST: $(ls "$DEST/system_dlkm" | wc -l) system_dlkm, $(ls "$DEST/vendor" | wc -l) vendor modules"
    strings "$DEST/kernel-6.18" | grep -m1 '^6\.18\.' ;;
  firmware)
    # From the host's linux-firmware (zstd-compressed; the GKI image has no
    # FW_LOADER_COMPRESS), only what AOSP's external/linux-firmware lacks (aosp-device/pc/pc.mk
    # takes the AX211's iwlwifi and ibt-0040-0041 from there): RTL8125 (r8169) and the
    # ibt-1040-0041 name btintel may ask for (the same files as ibt-0040-0041).
    FW=/usr/lib/firmware FWD=$ROOT/aosp/device/android-pc/firmware
    rm -rf "$FWD" && mkdir -p "$FWD/rtl_nic" "$FWD/intel" || exit 1
    for f in "$FW"/rtl_nic/rtl8125*.fw.zst "$FW"/intel/ibt-1040-0041.{sfi,ddc}.zst; do
      rel=${f#$FW/}
      zstd -q -d -f "$f" -o "$FWD/${rel%.zst}" || exit 1
    done
    # RTX 4070 (AD104) for nouveau (Phase G1): the GSP-RM 570.144 set nouveau asks for
    # (nvkm/subdev/gsp/ad102.c). ad104 is a symlink to ad102 in linux-firmware, and gsp-*.bin
    # a symlink to ga102's; zstd follows both, so real files land under nvidia/ad104/gsp.
    mkdir -p "$FWD/nvidia/ad104/gsp" || exit 1
    for n in booter_load booter_unload bootloader gsp; do
      zstd -q -d -f "$FW/nvidia/ad104/gsp/$n-570.144.bin.zst" -o "$FWD/nvidia/ad104/gsp/$n-570.144.bin" || exit 1
    done
    echo "firmware: $(find "$FWD" -type f | wc -l) files, $(du -sh "$FWD" | cut -f1)" ;;
  *) echo "gki-build: unknown step $s"; exit 2 ;;
esac; done
echo GKI_BUILD_OK
