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
# Mesa exposes GL_ANDROID_extension_pack_es31a on the a650, as stock declares: Play hides
# apps that require opengles.aep (Genshin Impact) without it.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.vulkan.level-1.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.vulkan.level.xml \
    frameworks/native/data/etc/android.hardware.vulkan.version-1_3.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.vulkan.version.xml \
    frameworks/native/data/etc/android.software.vulkan.deqp.level-latest.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.vulkan.deqp.level.xml \
    frameworks/native/data/etc/android.software.opengles.deqp.level-latest.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.opengles.deqp.level.xml \
    frameworks/native/data/etc/android.hardware.opengles.aep.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.opengles.aep.xml
else
$(warning aosp_gts7l: no Mesa in $(GTS7L_MESA_DIR), the image will have no GPU driver)
endif
PRODUCT_VENDOR_PROPERTIES += ro.opengles.version=196610

# KeyMint and gatekeeper in software: mainline Linux has no path to TrustZone's keymaster
# (docs/gts7l.md §5, §7), so /data starts unencrypted.
PRODUCT_PACKAGES += \
    com.android.hardware.keymint.rust_nonsecure \
    com.android.hardware.gatekeeper.nonsecure

# Audio: android-pc's Rust HAL (hals/audio, in hals.list) serves IModule/default over the
# sm8250 card (MultiMedia1 to the four CS35L41, VA macro mic); the mixer routes are in
# audio_mixer_setup.conf (docs/gts7l-hal/audio.md). AOSP's default AIDL HAL keeps IConfig,
# r_submix, bluetooth and the effects; it creates one module per <module> of
# /vendor/etc/audio_policy_configuration.xml, so that file (hals/audio's) has no primary
# module. audioserver waits for every IModule the APEX's VINTF fragment declares (default,
# r_submix, bluetooth): without the policy file system_server's watchdog killed AudioService's
# start (S7 attempt 3), without bluetooth audioserver waited forever (attempt 5). Copied, not
# PRODUCT_PACKAGES: Cuttlefish's auto targets define modules with the same names, and the
# generated vendor image left them out (attempt 4).
PRODUCT_PACKAGES += com.android.hardware.audio
PRODUCT_COPY_FILES += \
    $(PC_DEVICE_DIR)/hals/audio/audio_policy_configuration.xml:$(TARGET_COPY_OUT_VENDOR)/etc/audio_policy_configuration.xml \
    frameworks/av/services/audiopolicy/config/bluetooth_with_le_audio_policy_configuration_7_0.xml:$(TARGET_COPY_OUT_VENDOR)/etc/bluetooth_with_le_audio_policy_configuration_7_0.xml \
    $(GTS7L_DEVICE_DIR)/audio_mixer_setup.conf:$(TARGET_COPY_OUT_VENDOR)/etc/audio_mixer_setup.conf
# The microphone is on MultiMedia2 (PCM 1), the speakers on MultiMedia1 (PCM 0).
PRODUCT_VENDOR_PROPERTIES += ro.vendor.audio.pc.capture_device=1
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
# The USB HAL (hals.list) takes the gadget over once the framework runs and keeps the ACM
# console in every configuration; the framework reads the gadget state from the UDC (rro/).
PRODUCT_PACKAGES += gts7l_overlay_frameworks_base_core
PRODUCT_VENDOR_PROPERTIES += persist.vendor.usb.gadget.acm=1
PRODUCT_PRODUCT_PROPERTIES += \
    persist.sys.usb.config=adb \
    ro.adb.secure=0 \
    service.adb.tcp.port=5555

# Fingerprint as on aosp_pc_x86_64 (docs/lockdown-biometric.md): the tablet's own side-key
# sensor belongs to TrustZone, so AOSP's virtual HAL (an under-display sensor driven over adb,
# tools/fp.sh) stands in, as Cuttlefish's biometrics_fingerprint/device_vendor.mk adds it. The
# sensor sits low in the middle of the 1600x2560 panel (natural orientation). No
# enrollment_hit: only tools/fp.sh touch makes a press on the spot match, as on the real PC.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.fingerprint.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.fingerprint.xml
PRODUCT_PACKAGES += \
    com.android.hardware.biometrics.fingerprint.virtual \
    android.hardware.biometrics.fingerprint-service.default
PRODUCT_VENDOR_PROPERTIES += \
    persist.vendor.fingerprint.virtual.type=udfps \
    persist.vendor.fingerprint.virtual.sensor_location=800:2240:100

# Our 7.3 kernel has no ashmem (gone upstream), and libcutils uses memfd only for apps that
# target SDK 37+: older apps (Play Store, targetSdk 36) fail every CursorWindow and crash.
# sys.use_memfd makes every process use memfd.
PRODUCT_PRODUCT_PROPERTIES += sys.use_memfd=true

# Sensors (docs/gts7l.md §19): the SLPI's accelerometer through gts7l-sensord, which also
# serves the DSP its configuration (sensors/).
PRODUCT_PACKAGES += \
    android.hardware.sensors-service.gts7l \
    gts7l-sensors-setup

# Wi-Fi (docs/gts7l.md §23): the QCA6390 on ath11k_pci as wlan0, our Rust HAL
# (hals/wifi, from hals.list) and wpa_supplicant/hostapd from AOSP's generic APEX
# (BoardConfig.mk). wifi.direct is declared with stock's features below; P2P is untested.
PRODUCT_PACKAGES += com.android.hardware.wpa_supplicant
PRODUCT_VENDOR_PROPERTIES += \
    wifi.interface=wlan0 \
    wifi.direct.interface=p2p-dev-wlan0
# The QCA6390 has no MAC address in OTP, so ath11k makes one up at every boot; the HAL
# gives wlan0 the tablet's own address from efs instead (docs/gts7l.md §23).
PRODUCT_VENDOR_PROPERTIES += \
    ro.vendor.wifi.factory_mac_file=/mnt/vendor/efs/wifi/.mac.info
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.wifi.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.wifi.xml

# Bluetooth profiles (docs/gts7l.md §20). The Bluetooth app starts only the profiles whose
# bluetooth.profile.*.enabled is set; without HID Host, keyboards and mice paired but never
# connected. Only what works without a Bluetooth audio HAL.
PRODUCT_PRODUCT_PROPERTIES += \
    bluetooth.profile.gatt.enabled=true \
    bluetooth.profile.hid.host.enabled=true

# Hardware the framework may rely on. The rear camera has autofocus and a flash, as stock
# declares: Play hides apps that list camera.autofocus/camera.flash (Where Winds Meet) without them.
PRODUCT_COPY_FILES += \
    $(GTS7L_DEVICE_DIR)/media_profiles_V1_0.xml:$(TARGET_COPY_OUT_VENDOR)/etc/media_profiles_V1_0.xml \
    frameworks/native/data/etc/android.hardware.camera.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.xml \
    frameworks/native/data/etc/android.hardware.camera.front.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.front.xml \
    frameworks/native/data/etc/android.hardware.camera.flash-autofocus.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.flash-autofocus.xml \
    frameworks/native/data/etc/android.hardware.bluetooth.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.bluetooth.xml \
    frameworks/native/data/etc/android.hardware.bluetooth_le.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.bluetooth_le.xml \
    frameworks/native/data/etc/android.hardware.sensor.accelerometer.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.accelerometer.xml \
    frameworks/native/data/etc/tablet_core_hardware.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/tablet_core_hardware.xml \
    frameworks/native/data/etc/android.hardware.touchscreen.multitouch.jazzhand.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.touchscreen.multitouch.jazzhand.xml \
    frameworks/native/data/etc/android.hardware.faketouch.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.faketouch.xml \
    frameworks/native/data/etc/android.hardware.usb.accessory.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.usb.accessory.xml \
    frameworks/native/data/etc/android.software.midi.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.midi.xml

# Every feature stock's vendor declares (SM-T875 T875XXS4DXG1, vendor/etc/permissions), so Play
# offers the apps stock gets. tablet_core stands for stock's handheld_core and our vulkan.version
# 1.3 for its 1.1; gts7l_stock_features.xml has what stock's handheld_core adds over tablet_core.
# No aosp_excluded_hardware.xml: it removes location.network, which microG provides.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.audio.low_latency.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.audio.low_latency.xml \
    frameworks/native/data/etc/android.hardware.camera.concurrent.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.concurrent.xml \
    frameworks/native/data/etc/android.hardware.camera.full.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.full.xml \
    frameworks/native/data/etc/android.hardware.camera.raw.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.camera.raw.xml \
    frameworks/native/data/etc/android.hardware.location.gps.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.location.gps.xml \
    frameworks/native/data/etc/android.hardware.sensor.gyroscope.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.gyroscope.xml \
    frameworks/native/data/etc/android.hardware.sensor.light.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.light.xml \
    frameworks/native/data/etc/android.hardware.sensor.stepcounter.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.stepcounter.xml \
    frameworks/native/data/etc/android.hardware.sensor.stepdetector.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.sensor.stepdetector.xml \
    frameworks/native/data/etc/android.hardware.strongbox_keystore.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.strongbox_keystore.xml \
    frameworks/native/data/etc/android.hardware.telephony.gsm.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.telephony.gsm.xml \
    frameworks/native/data/etc/android.hardware.telephony.ims.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.telephony.ims.xml \
    frameworks/native/data/etc/android.hardware.usb.host.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.usb.host.xml \
    frameworks/native/data/etc/android.hardware.vulkan.compute-0.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.vulkan.compute-0.xml \
    frameworks/native/data/etc/android.hardware.wifi.aware.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.wifi.aware.xml \
    frameworks/native/data/etc/android.hardware.wifi.direct.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.wifi.direct.xml \
    frameworks/native/data/etc/android.hardware.wifi.passpoint.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.wifi.passpoint.xml \
    frameworks/native/data/etc/android.hardware.wifi.rtt.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.wifi.rtt.xml \
    frameworks/native/data/etc/android.software.sip.voip.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.sip.voip.xml \
    frameworks/native/data/etc/android.software.verified_boot.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.verified_boot.xml \
    $(GTS7L_DEVICE_DIR)/gts7l_stock_features.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/gts7l_stock_features.xml

# Doze/Data Saver allowlists a preinstalled GmsCore and Play Store get (gts7l_sysconfig_google.xml).
PRODUCT_COPY_FILES += \
    $(GTS7L_DEVICE_DIR)/gts7l_sysconfig_google.xml:$(TARGET_COPY_OUT_PRODUCT)/etc/sysconfig/gts7l_sysconfig_google.xml

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
