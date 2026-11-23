# android-pc products. This directory is mirrored into the AOSP tree as
# device/android-pc/pc_x86_64 by tools/aosp-device-link.sh (see docs/rust-hals.md).

PRODUCT_MAKEFILES := \
    aosp_pc_x86_64:$(LOCAL_DIR)/aosp_pc_x86_64.mk \
    aosp_gts7l:$(LOCAL_DIR)/gts7l/aosp_gts7l.mk

COMMON_LUNCH_CHOICES := \
    aosp_pc_x86_64-cp2a-userdebug \
    aosp_gts7l-cp2a-userdebug
