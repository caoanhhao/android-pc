# aosp_gts7l: android-pc on the Samsung Galaxy Tab S7 LTE (SM-T875, kona/SM8250), docs/gts7l.md.
# An arm64 generic system with the hypr desktop shell, AOSP's open graphics stack for Adreno
# (Mesa freedreno/Turnip, drm_hwcomposer, minigbm) and our Rust HALs from hals.list. Unlike
# aosp_pc_x86_64 it does not inherit Cuttlefish: nothing of the virtual device is on it.

PC_DEVICE_DIR := device/android-pc/pc_x86_64
GTS7L_DEVICE_DIR := $(PC_DEVICE_DIR)/gts7l

# System, system_ext and product: the generic system image plus the handheld and telephony
# parts (as Cuttlefish's arm64 phone has them). The hypr shell and other android-pc apps add
# to system_ext/product, so the artifact path check is relaxed, as on Cuttlefish.
$(call inherit-product, $(SRC_TARGET_DIR)/product/core_64_bit_only.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/generic_system.mk)
PRODUCT_ENFORCE_ARTIFACT_PATH_REQUIREMENTS := relaxed
$(call inherit-product, $(SRC_TARGET_DIR)/product/handheld_system_ext.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/telephony_system_ext.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/aosp_product.mk)

# Vendor: the handheld vendor set, every language, updatable APEXes, /sdcard emulated on /data.
$(call inherit-product, $(SRC_TARGET_DIR)/product/handheld_vendor.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/languages_full.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/updatable_apex.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/emulated_storage.mk)
# TODO(S7): dalvik heap for 6 GB RAM / xhdpi once the first boot shows the memory budget.
$(call inherit-product, frameworks/native/build/tablet-10in-xhdpi-2048-dalvik-heap.mk)

# Rust HALs: only hal.mk of each HAL in hals.list. hal-pc.mk holds the PC product's
# Cuttlefish switches and PC-only files, so it is never read here.
include $(GTS7L_DEVICE_DIR)/hals.mk
$(foreach hal,$(GTS7L_HALS),$(eval include $(PC_DEVICE_DIR)/hals/$(hal)/hal.mk))

# Desktop mode, tablet variant: freeform desktop on the panel, rotation allowed.
PC_DESKTOP_VARIANT := tablet
include $(PC_DEVICE_DIR)/desktop/desktop.mk

# Phase D, the Hyprland-style desktop shell: PcSystemUI and HyprLauncher (docs/hypr-desktop.md).
include $(PC_DEVICE_DIR)/hypr/hypr.mk

# Composer: drm_hwcomposer (HWC3) on the msm DRM driver's card.
PRODUCT_PACKAGES += com.android.hardware.graphics.composer.drm_hwcomposer
PRODUCT_VENDOR_PROPERTIES += vendor.hwc.drm.device=/dev/dri/card0

# Gralloc: minigbm's AIDL allocator and stable-C mapper. They are built with minigbm's
# generic backend (dma-buf system heap); the msm backend (msm_cflags) exists only for the
# HIDL gralloc4 and gralloc0 modules.
# TODO(T3): an msm build of allocator-service.minigbm/mapper.minigbm (patch to
# external/minigbm in tools/patches/) for UBWC and the GPU's tiling.
PRODUCT_PACKAGES += \
    android.hardware.graphics.allocator-service.minigbm \
    mapper.minigbm
PRODUCT_VENDOR_PROPERTIES += ro.hardware.gralloc=minigbm

# GPU: Mesa freedreno (EGL/GLES) and Turnip (Vulkan) for the Adreno 650. AOSP 17's
# external/mesa3d builds only lavapipe and gfxstream, so Mesa comes from a separate meson
# cross build installed as prebuilts in device/android-pc/gts7l-mesa (outside the mirror,
# like the PC kernel's device/android-pc/kernel). Without it the image has no GLES driver
# and SurfaceFlinger cannot start, but the product still analyses and builds.
# TODO(T3): tools/gts7l-mesa.sh (meson cross build of freedreno + turnip for Android
# arm64, installing libEGL_mesa, libGLESv1_CM_mesa, libGLESv2_mesa, libgallium_dri and
# vulkan.freedreno with an Android.bp into device/android-pc/gts7l-mesa).
GTS7L_MESA_DIR := device/android-pc/gts7l-mesa
ifneq (,$(wildcard $(GTS7L_MESA_DIR)/Android.bp))
PRODUCT_PACKAGES += \
    libEGL_mesa \
    libGLESv1_CM_mesa \
    libGLESv2_mesa \
    libgallium_dri \
    vulkan.freedreno
PRODUCT_VENDOR_PROPERTIES += \
    ro.hardware.egl=mesa \
    ro.hardware.vulkan=freedreno
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.vulkan.level-1.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.vulkan.level.xml \
    frameworks/native/data/etc/android.hardware.vulkan.version-1_3.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.vulkan.version.xml \
    frameworks/native/data/etc/android.software.vulkan.deqp.level-latest.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.vulkan.deqp.level.xml \
    frameworks/native/data/etc/android.software.opengles.deqp.level-latest.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.opengles.deqp.level.xml
else
$(warning aosp_gts7l: no Mesa in $(GTS7L_MESA_DIR), the image will have no GPU driver)
endif
PRODUCT_VENDOR_PROPERTIES += ro.opengles.version=196610

# KeyMint and gatekeeper in software: mainline Linux has no path to TrustZone's keymaster
# (docs/gts7l.md §5, §7), so /data starts unencrypted.
PRODUCT_PACKAGES += \
    com.android.hardware.keymint.rust_nonsecure \
    com.android.hardware.gatekeeper.nonsecure

# Kernel: mainline v7.3 with our patches, built by tools/gts7l-kernel.sh into out/gts7l/
# (Image.gz, sm8250-samsung-gts7l.dtb, modules/), which tools/aosp-device-link.sh copies to
# device/android-pc/gts7l-kernel; the DTB goes in the boot image's own section
# (BoardConfig.mk). Without that build a placeholder lets the product analyse; its boot.img
# does not boot.
GTS7L_KERNEL := device/android-pc/gts7l-kernel/Image.gz
ifeq (,$(wildcard $(GTS7L_KERNEL)))
$(warning aosp_gts7l: no kernel at $(GTS7L_KERNEL), run tools/gts7l-kernel.sh; using a placeholder)
GTS7L_KERNEL := $(GTS7L_DEVICE_DIR)/prebuilt/kernel-placeholder
endif
PRODUCT_COPY_FILES += $(GTS7L_KERNEL):kernel

# Board files: fstab (also in the boot ramdisk, for first-stage mount), init, ueventd, the
# firmware tree of the gts7l_fw partition.
PRODUCT_COPY_FILES += \
    $(GTS7L_DEVICE_DIR)/fstab.gts7l:$(TARGET_COPY_OUT_RAMDISK)/fstab.gts7l \
    $(GTS7L_DEVICE_DIR)/fstab.gts7l:$(TARGET_COPY_OUT_VENDOR)/etc/fstab.gts7l \
    $(GTS7L_DEVICE_DIR)/init.gts7l.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/init.gts7l.rc \
    $(GTS7L_DEVICE_DIR)/ueventd.gts7l.rc:$(TARGET_COPY_OUT_VENDOR)/etc/ueventd.rc \
    $(GTS7L_DEVICE_DIR)/firmware/init.gts7l.firmware.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/init.gts7l.firmware.rc

# Hardware the framework may rely on. TODO(T4): camera, sensors, telephony, Wi-Fi and
# Bluetooth features once their HALs exist.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/tablet_core_hardware.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/tablet_core_hardware.xml \
    frameworks/native/data/etc/android.hardware.touchscreen.multitouch.jazzhand.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.touchscreen.multitouch.jazzhand.xml \
    frameworks/native/data/etc/android.hardware.faketouch.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.faketouch.xml \
    frameworks/native/data/etc/android.hardware.usb.accessory.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.usb.accessory.xml \
    frameworks/native/data/etc/android.software.midi.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.midi.xml \
    frameworks/native/data/etc/aosp_excluded_hardware.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/aosp_excluded_hardware.xml

# The 11" 2560x1600 panel. TODO(T5): density and orientation from the first boot (the
# panel may scan out in portrait, docs/gts7l.md §6/T5).
PRODUCT_AAPT_CONFIG := normal large xlarge
PRODUCT_AAPT_PREF_CONFIG := xhdpi
PRODUCT_CHARACTERISTICS := tablet
PRODUCT_VENDOR_PROPERTIES += ro.sf.lcd_density=320

# Non-A/B, dynamic partitions in super (stock layout, BoardConfig.mk), no recovery image of
# ours: TWRP stays in the recovery partition (docs/gts7l.md §6/T6).
AB_OTA_UPDATER := false
PRODUCT_USE_DYNAMIC_PARTITIONS := true
PRODUCT_BUILD_SUPER_PARTITION := true
PRODUCT_BUILD_RECOVERY_IMAGE := false

# Our vendor is new, so it targets the current level like the PC product.
PRODUCT_SHIPPING_API_LEVEL := 37
DEVICE_MANIFEST_FILE += $(GTS7L_DEVICE_DIR)/manifest.xml

PRODUCT_NAME := aosp_gts7l
PRODUCT_DEVICE := gts7l
PRODUCT_BRAND := Android
PRODUCT_MANUFACTURER := android-pc
PRODUCT_MODEL := android-pc Galaxy Tab S7 LTE
PRODUCT_VENDOR_PROPERTIES += \
    ro.soc.manufacturer=QTI \
    ro.soc.model=SM8250
PRODUCT_IGNORE_ALL_ANDROIDMK := true
# Soong-only build, like the PC product: with Kati the Soong analysis of this arm64 product
# does not fit the 20 GB cap of tools/limited-build.sh (OOM-killed at ~19 GB RSS).
PRODUCT_SOONG_ONLY := true
