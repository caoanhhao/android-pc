# Phase K2 (docs/nvidia-proprietary.md): the NVIDIA glibc sidecar for the NVIDIA (proprietary,
# Venus) entry. nvsidecar.sh and its init script, plus the glibc root that
# tools/nvsidecar-build.sh puts in device/android-pc/nvsidecar (NVIDIA's userspace at the
# kernel modules' version: built on the build machine, never committed, for this personal stick
# only). Under /vendor/bin so that fs_config makes the loader and the programs executable.
# The copies are ELF files: BoardConfig.mk's BUILD_BROKEN_ELF_PREBUILT_PRODUCT_COPY_FILES (set
# for ndk_translation) lets them through.
PRODUCT_PACKAGES += nvsidecar.sh

# Phase L3 (docs/nvidia-native.md): NVIDIA's own libraries from this root go to /vendor/lib64/nvidia
# (hals/nvgpu/hal.mk), where vulkan.nvidia loads them through nvglibc; the rest of the root
# (glibc, libvulkan, nvidia-smi, the vtest server) stays here for L1's glibc test client
# (pcnvtest.sh, LD_LIBRARY_PATH=...:/vendor/lib64/nvidia). Its two JSONs named the libraries by
# their old absolute paths: hals/nvgpu's name them bare. The server itself starts only on a Venus
# entry, and there is none now.
PC_NVSIDECAR_DIR := device/android-pc/nvsidecar
ifeq (,$(wildcard $(PC_NVSIDECAR_DIR)/VERSION))
$(warning nvsidecar: no $(PC_NVSIDECAR_DIR), run tools/nvsidecar-build.sh; no NVIDIA userspace on the image)
endif
PC_NVSIDECAR_JSONS := share/vulkan/icd.d/nvidia_icd.json share/glvnd/egl_vendor.d/10_nvidia.json
PRODUCT_COPY_FILES += $(foreach f,$(filter-out $(PC_NVSIDECAR_JSONS),$(shell cd $(PC_NVSIDECAR_DIR) 2>/dev/null && find . -type f ! -name 'libGLX_nvidia.so.*' ! -name 'libEGL*' ! -name 'libGLdispatch.so.*' ! -name 'libnvidia-*.so.6*' ! -name 'libnvidia-allocator.so.1' | sed 's|^\./||' | sort)),\
    $(PC_NVSIDECAR_DIR)/$(f):vendor/bin/nvsidecar/$(f))
PRODUCT_COPY_FILES += \
    $(PC_DEVICE_DIR)/hals/nvgpu/nvidia_icd.json:vendor/bin/nvsidecar/share/vulkan/icd.d/nvidia_icd.json \
    $(PC_DEVICE_DIR)/hals/nvgpu/10_nvidia.json:vendor/bin/nvsidecar/share/glvnd/egl_vendor.d/10_nvidia.json
