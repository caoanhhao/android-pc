#!/bin/bash
# Build NVK's internal OpenCL kernels (src/nouveau/vulkan/cl/*.cl) into the one SPIR-V
# library that the Soong build feeds to vtn_bindgen2 (aosp/external/mesa3d,
# src/nouveau/vulkan/cl/android/nvkcl.spv). Meson does this with mesa_clc (clang +
# SPIRV-LLVM-Translator + libclc), which Soong does not have; here the host clang's own
# SPIR-V backend (LLVM >= 20) and spirv-link (spirv-tools) do it with mesa_clc's flags.
# Rerun after a Mesa update that touches the kernels, then refresh the mesa patch
# (tools/patches/aosp17-mesa-nvk.patch). Host needs: clang with the spirv64 target,
# spirv-link, spirv-val, python3 + mako.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
M=${MESA:-$ROOT/aosp/external/mesa3d}
OUT=${1:-$M/src/nouveau/vulkan/cl/android/nvkcl.spv}
tmp=$(mktemp -d); trap '/usr/bin/rm -rf "$tmp"' EXIT

# nvk_copy_indirect.cl includes the generated nv_push_cl90b5.h.
python3 "$M/src/nouveau/headers/class_parser.py" \
    --in-h "$M/src/nouveau/headers/nvidia/classes/cl90b5.h" \
    --out-h "$tmp/nv_push_cl90b5.h" --out-c "$tmp/nv_push_cl90b5.c"

# mesa_clc: clc_helpers.cpp (clang options, OpenCL extensions it enables, features all
# on) and cl_args in the top-level meson.build.
ext=-all,+cl_khr_byte_addressable_store,+cl_khr_global_int32_base_atomics
ext+=,+cl_khr_global_int32_extended_atomics,+cl_khr_local_int32_base_atomics
ext+=,+cl_khr_local_int32_extended_atomics,+__cl_clang_bitfields
ext+=,+__opencl_c_atomic_order_seq_cst,+__opencl_c_atomic_scope_device,+cl_khr_fp16
ext+=,+cl_khr_fp64,+__opencl_c_fp64,+__opencl_c_generic_address_space,+cles_khr_int64
ext+=,+__opencl_c_int64,+__opencl_c_images,+cl_khr_subgroups,+__opencl_c_subgroups
objs=()
for cl in "$M"/src/nouveau/vulkan/cl/*.cl; do
    o="$tmp/$(basename "$cl" .cl).spv"
    clang --target=spirv64-unknown-unknown -c -O0 -fno-builtin-memset -fgnu89-inline \
        -cl-std=cl2.0 -D__OPENCL_VERSION__=200 -DHAVE___BUILTIN_FFS -DHAVE___BUILTIN_CLZ \
        -DNDEBUG -U__SPIR__ -U__SPIRV__ -Dcl_khr_expect_assume=1 -Xclang -cl-ext=$ext \
        -fmacro-prefix-map="$M/"= \
        -I"$M/src/compiler/libcl" -I"$M/src/nouveau/vulkan" -I"$M/src" \
        -I"$M/src/nouveau/headers" -I"$M/src/nouveau/headers/nvidia/classes" -I"$tmp" \
        "$cl" -o "$o"
    objs+=("$o")
done
mkdir -p "$(dirname "$OUT")"
spirv-link --create-library --allow-partial-linkage "${objs[@]}" -o "$OUT"
spirv-val "$OUT"
echo "$OUT: $(stat -c %s "$OUT") bytes, from $(clang --version | head -1)"
