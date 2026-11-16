# Phase L2 (docs/nvidia-native.md): nvglibc, NVIDIA's glibc userspace driver loaded in-process by
# bionic code. The NVIDIA libraries come from K2's glibc root (/vendor/bin/nvsidecar/lib, found
# through NVGLIBC_DEFAULT_DRIVER_PATH) until L3 gives them their own directory.
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
