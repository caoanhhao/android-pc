# Phase L3 (docs/nvidia-native.md): NVIDIA's own Vulkan driver as the device's driver on the
# NVIDIA (proprietary) entry: the HAL module, the entry's init script, and NVIDIA's userspace
# driver in /vendor/lib64/nvidia, where nvglibc (Phase L2) loads it from (the sphal linker
# namespace reaches /vendor/lib64 only). The libraries come from tools/nvidia-userspace-build.sh,
# which takes them from the build machine's NVIDIA install (the kernel modules' version,
# 620.70.06): copied here, never committed, for this personal stick only.
# nvglibc's soname libraries are listed by hals/nvglibc/hal.mk.
PRODUCT_PACKAGES += \
    init.nvgpu.rc \
    pcwwmvk.sh \
    vulkan.nvidia

PC_NVIDIA_LIB_DIR := device/android-pc/nvidia-userspace/lib
ifeq (,$(wildcard $(PC_NVIDIA_LIB_DIR)/libGLX_nvidia.so.0))
$(warning nvgpu: no NVIDIA userspace in $(PC_NVIDIA_LIB_DIR), run tools/nvidia-userspace-build.sh; vulkan.nvidia will fail to load)
endif
# The ICD and what it loads: GLVND's libEGL and NVIDIA's EGL driver (NVIDIA's Vulkan driver
# initialises through them), the compiler and core libraries, ray tracing. The glibc libraries
# themselves are nvglibc's trampolines (/vendor/lib64/nvglibc).
PC_NVIDIA_LIBS := \
    libGLX_nvidia.so.0 \
    libEGL.so.1 \
    libGLdispatch.so.0 \
    libEGL_nvidia.so.0 \
    $(notdir $(wildcard $(PC_NVIDIA_LIB_DIR)/libnvidia-*.so.6*)) \
    libnvidia-allocator.so.1
# GLVND's EGL vendor JSON names NVIDIA's EGL driver by file name (nvglibc's driver path finds it);
# vulkan.nvidia points __EGL_VENDOR_LIBRARY_FILENAMES at it.
PRODUCT_COPY_FILES += \
    $(foreach f,$(PC_NVIDIA_LIBS),$(PC_NVIDIA_LIB_DIR)/$(f):vendor/lib64/nvidia/$(f)) \
    $(PC_DEVICE_DIR)/hals/nvgpu/10_nvidia.json:vendor/lib64/nvidia/10_nvidia.json
