# Phase K3 (docs/nvidia-proprietary.md): Venus, ANGLE's gpuid list and the GLES feature files for
# the NVIDIA proprietary entry. vulkan.virtio itself goes into the Cuttlefish Vulkan APEX
# (tools/patches/aosp17-cf-vulkan-venus.patch); minigbm's vtest backend is
# tools/patches/aosp17-minigbm-vtest.patch, ANGLE's gpuid tools/patches/aosp17-angle-gpuid.patch.
PRODUCT_PACKAGES += \
    init.venus.rc \
    pcvenusprobe.sh \
    gpuid.conf \
    gpuid.nvidia.conf \
    android.hardware.opengles.aep.pc.xml \
    android.hardware.opengles.aep.venus.xml
