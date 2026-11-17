#!/bin/bash
# Build NVIDIA's open kernel modules (open-gpu-kernel-modules: nvidia, nvidia-modeset,
# nvidia-drm, nvidia-uvm) for the android17-6.18 GKI kernel (Phase K1, docs/nvidia-proprietary.md).
#
# Needs a finished tools/gki-build.sh "gki" step (gki/out/kernel_x86_64/dist: the GKI .config
# and Module.symvers) and the source in gki/nvidia-open at tag $NV_VERSION:
#   git clone --depth 1 --branch 620.70.06 https://github.com/NVIDIA/open-gpu-kernel-modules gki/nvidia-open
# The version must match the NVIDIA userspace (tools/nvidia-userspace-build.sh) exactly.
#
# Not a Kleaf build: nvidia's build is two stages (the OS-agnostic nv-kernel.o and
# nv-modeset-kernel.o with nvidia's own makefiles, then kernel-open/ with kbuild), so it runs
# against a plain kernel output directory prepared from the GKI .config with the same
# prebuilt clang, and the GKI Module.symvers for the symbol CRCs. Result: gki/out/nvidia/dist.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GKI=$ROOT/gki
NV=$GKI/nvidia-open
NV_VERSION=${NV_VERSION:-620.70.06}
DIST=$GKI/out/kernel_x86_64/dist
KO=$GKI/out/nvidia/kbuild          # kernel output directory (modules_prepare)
OUT=$GKI/out/nvidia/dist
LB=$ROOT/tools/limited-build.sh
[ -f "$DIST/kernel_x86_64_dot_config" ] || { echo "nvidia-build: run tools/gki-build.sh first (no $DIST)"; exit 1; }
[ -f "$NV/version.mk" ] || { echo "nvidia-build: no source in $NV (see the header)"; exit 1; }
grep -q "^NVIDIA_VERSION = $NV_VERSION\$" "$NV/version.mk" || { echo "nvidia-build: $NV is not $NV_VERSION"; exit 1; }
# nvidia-open-gki-ns.patch: nvidia and nvidia-modeset import the GKI namespace that
# filp_open() and close_fd() are exported in (modpost refuses the modules otherwise).
# nvidia-open-kcfi.patch (K1c): functions whose definition does not match the function
# pointer type they are called through, which kCFI traps (the first real-PC boot panicked in
# the SEC2 scrubber one): two generated BINDATA getters of KernelSec2 that the NVOC method
# table calls with (pGpu, this), and two IOM destructors stored as NVOC_DYNAMIC_DTOR.
for p in nvidia-open-gki-ns.patch nvidia-open-kcfi.patch; do
  P=$ROOT/tools/patches/$p
  git -C "$NV" apply --reverse --check "$P" 2>/dev/null || git -C "$NV" apply "$P" \
    || { echo "nvidia-build: $P does not apply"; exit 1; }
done

# The GKI toolchain (gki/build/kernel/kleaf: clang r584948c, rust 1.91.1.p3, bindgen).
CLANG=$GKI/prebuilts/clang/host/linux-x86/clang-r584948c/bin
# pahole: the one Kleaf builds from source (//common:pahole, v1.31); its version decides
# DEBUG_INFO_BTF and with it SCHED_CLASS_EXT, which changes struct task_struct.
PAHOLE=$(ls "$GKI"/bazel-out/k8-opt-exec-*/bin/common/pahole 2>/dev/null | head -1)
[ -x "$PAHOLE" ] || { echo "nvidia-build: no Kleaf pahole (run tools/gki-build.sh gki)"; exit 1; }
mkdir -p "$GKI/out/nvidia/bin" && ln -sf "$PAHOLE" "$GKI/out/nvidia/bin/pahole"
export PATH=$CLANG:$GKI/prebuilts/rust-toolchain/linux-x86/1.91.1.p3/bin:$GKI/prebuilts/clang-tools/linux-x86/bin:$GKI/out/nvidia/bin:$PATH
KMAKE=(make -C "$GKI/common" O="$KO" ARCH=x86_64 LLVM=1 LLVM_IAS=1 -j8)

# 1. Kernel output directory with the GKI image's exact config.
mkdir -p "$KO"
if ! cmp -s "$DIST/kernel_x86_64_dot_config" "$KO/.config.gki"; then
  cp "$DIST/kernel_x86_64_dot_config" "$KO/.config"
  cp "$DIST/kernel_x86_64_dot_config" "$KO/.config.gki"
  "$LB" "${KMAKE[@]}" olddefconfig modules_prepare
  # olddefconfig must not have changed anything (another toolchain would change *_VERSION).
  diff <(grep -v '^#' "$DIST/kernel_x86_64_dot_config" | sort) <(grep -v '^#' "$KO/.config" | sort) \
    || { echo "nvidia-build: the prepared .config differs from the GKI one"; exit 1; }
fi
# Exported symbols of the GKI image and its system_dlkm modules, with their CRCs.
cp "$DIST/kernel_x86_64_Module.symvers" "$KO/Module.symvers"

# 2. The OS-agnostic parts with the code generation flags of the GKI's own modules (from a
# kbuild .o.cmd): kCFI (CONFIG_CFI) with normalized integers, FineIBT/call-depth padding
# (-fpatchable-function-entry=11,11, CONFIG_FUNCTION_PADDING_BYTES), IBT (endbr),
# retpolines and return thunks. nvidia-drm calls nvidia-modeset's KAPI function table and
# the kernel calls back into these objects, so their kCFI type hashes must be there.
# objtool then processes them with the rest of each module (delay-objtool with IBT).
# Plus the kernel's own semantics flags (no null-check deletion, wrapping overflow, zeroed
# locals), which nvidia's code is written for (it is normally built by gcc for distro kernels).
# The C++ parts (DisplayPort library) get kCFI preambles too, but clang does not check C++
# virtual calls: under FineIBT those would land on a removed ENDBR, hence cfi=kcfi on the
# nvidia boot entry (docs/nvidia-proprietary.md).
KFLAGS="-fcf-protection=branch -fno-jump-tables -mretpoline-external-thunk -mindirect-branch-cs-prefix \
 -mfunction-return=thunk-extern -fpatchable-function-entry=11,11 -fsanitize=kcfi \
 -fsanitize-cfi-icall-experimental-normalize-integers -falign-functions=16 \
 -fno-asynchronous-unwind-tables -mskip-rax-setup -mstack-alignment=8 -mno-avx -mno-sse4a \
 -fno-delete-null-pointer-checks -fno-strict-overflow -ftrivial-auto-var-init=zero"
NVMAKE=(make CC=clang CXX=clang++ LD=ld.lld AR=llvm-ar OBJCOPY=llvm-objcopy TARGET_ARCH=x86_64 -j8)
# nvidia's makefiles do not track flags: rebuild from scratch when they change.
if [ "$(cat "$GKI/out/nvidia/kflags" 2>/dev/null)" != "$KFLAGS" ]; then
  /usr/bin/rm -rf "$NV/src/nvidia/_out" "$NV/src/nvidia-modeset/_out"
  echo "$KFLAGS" > "$GKI/out/nvidia/kflags"
fi
"$LB" "${NVMAKE[@]}" -C "$NV/src/nvidia" EXTRA_CFLAGS="$KFLAGS"
"$LB" "${NVMAKE[@]}" -C "$NV/src/nvidia-modeset" EXTRA_CFLAGS="$KFLAGS"
# 3. kernel-open with kbuild against the prepared directory, linked with the cores.
# kbuild relinks nvidia.ko/nvidia-modeset.ko only when the core object is newer, so the linked
# objects are removed first.
link_modules() {   # link_modules CORE-OUTPUTDIR DIST
  ln -sf ../../src/nvidia/$1/nv-kernel.o "$NV/kernel-open/nvidia/nv-kernel.o_binary"
  ln -sf ../../src/nvidia-modeset/$1/nv-modeset-kernel.o "$NV/kernel-open/nvidia-modeset/nv-modeset-kernel.o_binary"
  /usr/bin/rm -f "$NV"/kernel-open/{nvidia,nvidia-modeset}.{o,ko}
  "$LB" make -C "$NV/kernel-open" SYSSRC="$GKI/common" SYSOUT="$KO" ARCH=x86_64 LLVM=1 LLVM_IAS=1 \
    CC=clang LD=ld.lld -j8 modules || return 1
  mkdir -p "$2" && /usr/bin/rm -f "$2"/*.ko
  for m in nvidia nvidia-modeset nvidia-drm nvidia-uvm; do
    llvm-strip --strip-debug -o "$2/$m.ko" "$NV/kernel-open/$m.ko" || return 1
  done
  echo "nvidia $NV_VERSION modules in $2:"; ls -la "$2"
}

link_modules _out/Linux_x86_64 "$OUT" || exit 1
echo NVIDIA_BUILD_OK
