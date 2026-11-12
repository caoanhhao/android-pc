# aosp_pc_x86_64: the Cuttlefish x86_64-only phone plus the android-pc Rust HALs.

PC_DEVICE_DIR := device/android-pc/pc_x86_64

# Every hals/<name>/hal.mk adds its packages to PRODUCT_PACKAGES and may switch off the
# Cuttlefish HAL it replaces (LOCAL_ENABLE_* ?= switches in device/google/cuttlefish),
# so the HAL makefiles are read before the Cuttlefish product.
$(foreach hal_mk,$(sort $(wildcard $(PC_DEVICE_DIR)/hals/*/hal.mk)),$(eval include $(hal_mk)))

$(call inherit-product, device/google/cuttlefish/vsoc_x86_64_only/phone/aosp_cf.mk)

# Booting on the real PC from a USB stick (phase F3a).
include $(PC_DEVICE_DIR)/pc/pc.mk

# ARM64 apps through the libndk_translation native bridge.
include $(PC_DEVICE_DIR)/ndk_translation.mk

# microG (Google Play services replacement) and Google Play Store, preinstalled and enabled.
include $(PC_DEVICE_DIR)/microg.mk

PRODUCT_NAME := aosp_pc_x86_64
PRODUCT_DEVICE := pc_x86_64
PRODUCT_BRAND := Android
PRODUCT_MANUFACTURER := android-pc
PRODUCT_MODEL := android-pc x86_64

# The Cuttlefish makefile sets these only when TARGET_PRODUCT is its own product name.
PRODUCT_SOONG_ONLY := $(RELEASE_SOONG_ONLY_CUTTLEFISH)
PRODUCT_SOONG_INCREMENTAL_ANALYSIS := $(RELEASE_SOONG_INCREMENTAL_ANALYSIS)
