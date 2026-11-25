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
# The msm card is card1 on the tablet (S7 first boot; minor 0 is taken during probe), and
# drm_hwcomposer's card% pattern stops at the first missing card0, so name it.
PRODUCT_VENDOR_PROPERTIES += vendor.hwc.drm.device=/dev/dri/card1

# Gralloc: minigbm's AIDL allocator and stable-C mapper, built with minigbm's msm backend
# (soong config minigbm.platform=msm, which applies to every minigbm module of this product).
# The generic backend has no "msm" entry, so the allocator found no driver for card1 and
# exited at start (S7 attempt 2). gralloc.minigbm (gralloc0) is not a HAL here: Mesa's
# u_gralloc loads it through libhardware (ro.hardware.gralloc) for the CrOS perform() that
# reads a buffer's planes, strides and modifier. Mesa built with the NDK has no IMapper
# path, so without it EGL/Vulkan fall back to guessing a single plane.
$(call soong_config_set,minigbm,platform,msm)
PRODUCT_PACKAGES += \
    android.hardware.graphics.allocator-service.minigbm \
    mapper.minigbm \
    gralloc.minigbm
PRODUCT_VENDOR_PROPERTIES += ro.hardware.gralloc=minigbm

# GPU: Mesa freedreno (EGL/GLES) and Turnip (Vulkan) for the Adreno 650. AOSP 17's
# external/mesa3d has Soong files only for lavapipe, zink and gfxstream, so
# tools/gts7l-mesa.sh cross builds external/mesa3d (and a static external/libdrm) with meson
# and the NDK, and installs the libraries with a generated Android.bp in
# out/gts7l-mesa/prebuilt; tools/aosp-device-link.sh copies that to
# device/android-pc/gts7l-mesa (outside the mirror, like the kernel). Without it the image
# has no GLES driver and SurfaceFlinger cannot start, but the product still analyses and
# builds. The kernel side: the msm DRM driver with the Adreno GPU (card1 + renderD128) and the
# GPU firmware (a650_sqe.fw, a650_gmu.bin and the device's signed zap shader), docs/gts7l.md
# §13.
GTS7L_MESA_DIR := device/android-pc/gts7l-mesa
ifneq (,$(wildcard $(GTS7L_MESA_DIR)/Android.bp))
PRODUCT_PACKAGES += \
    libEGL_mesa \
    libGLESv1_CM_mesa \
    libGLESv2_mesa \
    libgallium_dri \
    vulkan.freedreno
# HWUI stays on GLES (skiagl) until Turnip is proven on the device.
PRODUCT_VENDOR_PROPERTIES += \
    debug.hwui.renderer=skiagl \
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

# Audio: AOSP's default AIDL audio and effects HALs (as on Cuttlefish) until a CS35L41/WCD
# HAL exists. Without any audio HAL audioserver crashes in AudioFlinger::onFirstRef and
# keeps restarting (S7 first boot).
# The HAL creates its modules from /vendor/etc/audio_policy_configuration.xml, and audioserver
# waits for every IModule its VINTF fragment declares (default, r_submix, bluetooth). The
# vendor_1 makefile installs only the files the top file includes. Without the top file the
# HAL registered no IModule and system_server's watchdog killed AudioService's start after
# 66 s (S7 attempt 3); with frameworks/av's generic one (primary + r_submix) audioserver waited
# for IModule/bluetooth (attempt 5). Cuttlefish's set has all three. Copied, not
# PRODUCT_PACKAGES: Cuttlefish's auto targets define modules with the same names, and the
# generated vendor image left them out (attempt 4).
PRODUCT_PACKAGES += com.android.hardware.audio
PRODUCT_COPY_FILES += \
    device/google/cuttlefish/shared/config/audio/policy/audio_policy_configuration.xml:$(TARGET_COPY_OUT_VENDOR)/etc/audio_policy_configuration.xml \
    device/google/cuttlefish/shared/config/audio/policy/primary_audio_policy_configuration.xml:$(TARGET_COPY_OUT_VENDOR)/etc/primary_audio_policy_configuration.xml \
    frameworks/av/services/audiopolicy/config/bluetooth_with_le_audio_policy_configuration_7_0.xml:$(TARGET_COPY_OUT_VENDOR)/etc/bluetooth_with_le_audio_policy_configuration_7_0.xml
$(call inherit-product, frameworks/av/services/audiopolicy/audio_policy_config_vendor_1.mk)
$(call inherit-product, hardware/interfaces/audio/aidl/default/audio_effects.mk)

# Media: AOSP's software Codec2 codecs (com.android.media.swcodec). The AIDL software store
# is the real platform store only with the AIDL HAL selected; unset it defaults to HIDL, the
# AIDL /software instance is registered empty and the HIDL store is not in VINTF, so no codec
# but the in-process AAC decoder existed (docs/gts7l-hal/security-usb-media.md §5). There is
# no IOmx either, so encoders take their input surface from AidlGraphicBufferSource in the
# client (c2inputsurface=-1); without it screenrecord crashed in CCodec::createInputSurface.
# Both as on Cuttlefish (device/google/cuttlefish/shared/device.mk).
PRODUCT_VENDOR_PROPERTIES += \
    media.c2.hal.selection?=aidl \
    debug.stagefright.c2inputsurface=-1
# Hardware video decode: the SM8250 video codec (qcom-iris, its firmware from gts7l_fw) behind
# AOSP's V4L2 Codec2 components, served as the AIDL IComponentStore/default (media/).
PRODUCT_SOONG_NAMESPACES += \
    external/v4l2_codec2 \
    $(GTS7L_DEVICE_DIR)/media
PRODUCT_PACKAGES += android.hardware.media.c2-service.gts7l

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
# Its modules in /vendor/lib/modules (kernel/modules.bp, there as Android.bp).
ifneq (,$(wildcard device/android-pc/gts7l-kernel/Android.bp))
PRODUCT_PACKAGES += gts7l_kernel_modules
endif

# Board files: fstab (also in the boot ramdisk, for first-stage mount), init, ueventd, the
# firmware tree of the gts7l_fw partition.
PRODUCT_COPY_FILES += \
    $(GTS7L_DEVICE_DIR)/fstab.gts7l:$(TARGET_COPY_OUT_RAMDISK)/fstab.gts7l \
    $(GTS7L_DEVICE_DIR)/fstab.gts7l:$(TARGET_COPY_OUT_VENDOR)/etc/fstab.gts7l \
    $(GTS7L_DEVICE_DIR)/init.gts7l.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/init.gts7l.rc \
    $(GTS7L_DEVICE_DIR)/ueventd.gts7l.rc:$(TARGET_COPY_OUT_VENDOR)/etc/ueventd.rc \
    $(GTS7L_DEVICE_DIR)/firmware/init.gts7l.firmware.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/init.gts7l.firmware.rc \
    $(GTS7L_DEVICE_DIR)/init.gts7l.usb.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/init.gts7l.usb.rc

# Reachable without the screen (docs/gts7l.md §15): usbroute switches the MAX77705's USB data
# lines to the SoC, init.gts7l.usb.rc sets up the adb gadget (FunctionFS + an NCM function on
# 172.16.42.1), adbd needs no key confirmation and also listens on TCP 5555.
PRODUCT_PACKAGES += \
    usbroute \
    gts7l-usb.sh
PRODUCT_PRODUCT_PROPERTIES += \
    persist.sys.usb.config=adb \
    ro.adb.secure=0 \
    service.adb.tcp.port=5555

# Sensors (docs/gts7l.md §19): the SLPI's accelerometer through gts7l-sensord, which also
# serves the DSP its configuration (sensors/).
PRODUCT_PACKAGES += \
    android.hardware.sensors-service.gts7l \
    gts7l-sensors-setup

# Hardware the framework may rely on. TODO(T4): camera, gyroscope/compass/light, telephony
# and Wi-Fi features once their HALs exist.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.bluetooth.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.bluetooth.xml \
    frameworks/native/data/etc/android.hardware.bluetooth_le.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.bluetooth_le.xml \
    frameworks/native/data/etc/android.hardware.sensor.accelerometer.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.accelerometer.xml \
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

# Non-A/B, dynamic partitions in super (stock layout, BoardConfig.mk). No Android recovery: the
# recovery partition holds the bring-up image (tools/gts7l-bringup.sh --firmware), where
# androidboot.init_fatal_reboot_target=recovery lands with pstore (docs/gts7l.md §15).
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
