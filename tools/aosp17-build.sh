#!/bin/bash
# Build the AOSP 17 (android-17.0.0_r1) Cuttlefish images one step at a time via tools/limited-build.sh.
# A step is "+"-joined targets; names ending in .img/.txt are files in $PRODUCT_OUT.
# The *_dlkm + vbmeta* + super group must stay in one step (see docs/aosp-build.md).
# PRODUCT picks the lunch product: aosp_pc_x86_64 (ours, aosp-device/) or aosp_cf_x86_64_only_phone (stock).
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PRODUCT=${PRODUCT:-aosp_pc_x86_64}
"$ROOT/tools/aosp-device-link.sh" || exit 1
cd "$ROOT/aosp" || exit 1
export TARGET_KERNEL_USE=${TARGET_KERNEL_USE:-6.18}   # android17-6.18 GKI instead of the 6.12 default
export SOONG_GOMEMLIMIT=${SOONG_GOMEMLIMIT:-18GiB}   # soong_build heap cap; analysis OOMs at 25 GB otherwise (local build/soong patch)
source build/envsetup.sh >/dev/null
lunch $PRODUCT-${RELEASE:-cp2a}-userdebug >/dev/null || exit 1
LB=$ROOT/tools/limited-build.sh
P=${ANDROID_PRODUCT_OUT#$PWD/}   # out/target/product/<PRODUCT_DEVICE>
STEPS=${STEPS:-"system.img system_ext.img product.img vendor.img odm.img boot.img+init_boot.img+vendor_boot.img vendor_dlkm.img+odm_dlkm.img+system_dlkm.img+vbmeta.img+vbmeta_system.img+vbmeta_system_dlkm.img+vbmeta_vendor_dlkm.img+super.img userdata.img hosttools droid"}
for s in $STEPS; do
  t=; for x in ${s//+/ }; do case "$x" in
    *.img|*.txt) t+=" $P/$x";;
    hosttools) t+=" out/host/linux-x86/cvd-host_package.tar.gz adb root-canal mkfs.erofs fsck.erofs lz4 unpack_bootimg";;
    *) t+=" $x";; esac; done
  case "$s" in system.img|system_ext.img|product.img|vendor.img|droid) J=3;; *) J=${J_DEFAULT:-8};; esac
  echo "=== STEP$t $(date) free=$(df -BG --output=avail / | tail -1)"
  # Match process names, not command lines: a shell whose command text mentions them must not block us.
  while pgrep -x 'soong_ui|soong_build|siso|ninja|cargo|rustc' >/dev/null || pgrep -f "java .*GradleDaemon" >/dev/null; do
    echo "other build running, waiting"; sleep 60; done
  ok=; for try in 1 2 3 4 5; do NINJA_HIGHMEM_NUM_JOBS=1 NINJA_ARGS= $LB m -j$J $t && { ok=1; break; }; echo "RETRY $try$t"
    grep -q "FAILED: out/soong/build\." out/error.log 2>/dev/null && { echo "Soong analysis error, not retrying"; break; }; done
  [ -n "$ok" ] || { echo "STEP_FAILED$t"; exit 1; }
done
echo ALL_STEPS_OK $(date) free=$(df -BG --output=avail / | tail -1)
