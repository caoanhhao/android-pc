# Phase K2 (docs/nvidia-proprietary.md): the NVIDIA glibc sidecar for the NVIDIA (proprietary,
# Venus) entry. nvsidecar.sh and its init script, plus the glibc root that
# tools/nvsidecar-build.sh puts in device/android-pc/nvsidecar (NVIDIA's userspace at the
# kernel modules' version: built on the build machine, never committed, for this personal stick
# only). Under /vendor/bin so that fs_config makes the loader and the programs executable.
# The copies are ELF files: BoardConfig.mk's BUILD_BROKEN_ELF_PREBUILT_PRODUCT_COPY_FILES (set
# for ndk_translation) lets them through.
PRODUCT_PACKAGES += nvsidecar.sh

PC_NVSIDECAR_DIR := device/android-pc/nvsidecar
ifeq (,$(wildcard $(PC_NVSIDECAR_DIR)/VERSION))
$(warning nvsidecar: no $(PC_NVSIDECAR_DIR), run tools/nvsidecar-build.sh; the NVIDIA entry will have no vtest server)
endif
PRODUCT_COPY_FILES += $(foreach f,$(shell cd $(PC_NVSIDECAR_DIR) 2>/dev/null && find . -type f | sed 's|^\./||' | sort),\
    $(PC_NVSIDECAR_DIR)/$(f):vendor/bin/nvsidecar/$(f))
