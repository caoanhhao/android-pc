# Phase L2 (docs/nvidia-native.md): nvglibc, NVIDIA's glibc userspace driver loaded in-process by
# bionic code. The NVIDIA libraries are in /vendor/lib64/nvidia (hals/nvgpu/hal.mk), found through
# NVGLIBC_DEFAULT_DRIVER_PATH.
PRODUCT_PACKAGES += \
    libnvglibc \
    libnvglibc_c \
    libnvglibc_dl \
    libnvglibc_gcc_s \
    libnvglibc_ld \
    libnvglibc_m \
    libnvglibc_pthread \
    libnvglibc_rt \
    libnvglibc_x11 \
    libnvglibc_xext \
    nvglibc_vktest
