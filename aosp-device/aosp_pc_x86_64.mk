# aosp_pc_x86_64: the Cuttlefish x86_64-only phone plus the android-pc Rust HALs.

PC_DEVICE_DIR := device/android-pc/pc_x86_64

# Every HAL: hals/<name>/hal.mk adds its packages to PRODUCT_PACKAGES (any product), and
# hals/<name>/hal-pc.mk, where present, switches off the Cuttlefish HAL it replaces
# (LOCAL_ENABLE_* ?= switches in device/google/cuttlefish) or adds PC-only files. The HAL
# makefiles are read before the Cuttlefish product. Other products pick their own list
# (gts7l/hals.list). PC_SKIP_HALS are for other products only: usb (gts7l's IUsb/IUsbGadget)
# would declare IUsb/default a second time next to Cuttlefish's com.android.hardware.usb APEX,
# and that VINTF conflict stops keystore2 and gatekeeper from registering (Waydroid boot loop,
# 2026-11-26). fingerprint drives gts7l's Goodix sensor through its TrustZone app; the PC keeps
# Cuttlefish's virtual IFingerprint/default (BoardConfig.mk leaves out its sepolicy).
PC_SKIP_HALS := usb fingerprint
$(foreach hal_dir,$(sort $(filter-out $(foreach h,$(PC_SKIP_HALS),%/hals/$(h)/),\
        $(dir $(wildcard $(PC_DEVICE_DIR)/hals/*/hal.mk)))),\
    $(eval include $(hal_dir)hal.mk)\
    $(if $(wildcard $(hal_dir)hal-pc.mk),$(eval include $(hal_dir)hal-pc.mk)))

$(call inherit-product, device/google/cuttlefish/vsoc_x86_64_only/phone/aosp_cf.mk)

# Booting on the real PC from a USB stick (phase F3a).
include $(PC_DEVICE_DIR)/pc/pc.mk

# ARM64 apps through the libndk_translation native bridge.
include $(PC_DEVICE_DIR)/ndk_translation.mk

# microG (Google Play services replacement) and Google Play Store, preinstalled and enabled.
include $(PC_DEVICE_DIR)/microg.mk

# Desktop mode: freeform windows, landscape only (phase F9f).
include $(PC_DEVICE_DIR)/desktop/desktop.mk

# Phase D, a Hyprland-style desktop shell: PcSystemUI in place of SystemUI (docs/hypr-desktop.md).
include $(PC_DEVICE_DIR)/hypr/hypr.mk

# Play Store and libndk_translation as user-added extras, not built in (docs/google-extras.md).
include $(PC_DEVICE_DIR)/google_extras/extras.mk

# The same images also run in Waydroid's LXC container on a Linux desktop; these pieces only act
# there (docs/waydroid-aosp17.md).
include $(PC_DEVICE_DIR)/waydroid/waydroid.mk

PRODUCT_NAME := aosp_pc_x86_64
PRODUCT_DEVICE := pc_x86_64
PRODUCT_BRAND := Android
PRODUCT_MANUFACTURER := android-pc
PRODUCT_MODEL := android-pc x86_64

# The Cuttlefish makefile sets these only when TARGET_PRODUCT is its own product name.
PRODUCT_SOONG_ONLY := $(RELEASE_SOONG_ONLY_CUTTLEFISH)
PRODUCT_SOONG_INCREMENTAL_ANALYSIS := $(RELEASE_SOONG_INCREMENTAL_ANALYSIS)

# Last, after pc/pc.mk has added every fetched app (google_extras/extras.mk).
PRODUCT_PACKAGES := $(filter-out $(PC_EXTRAS_NOT_BUILT_IN),$(PRODUCT_PACKAGES))
