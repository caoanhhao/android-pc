# Real-PC boot support (phase F3a): init script, boot log saver and device firmware.
PRODUCT_PACKAGES += init.pc.rc init.pc.chromium.rc pclog.sh pcgpu.sh pcwake.sh pcnova.sh pcnvidia.sh pcnvtest.sh pclogstrip.sh PcLogStrip ueventd-watch.sh hals.pc.conf android.hardware.bluetooth-service.pc fstab.pc.f2fs.hctr2

# AX211 firmware from AOSP's external/linux-firmware: Wi-Fi (iwlmvm API 89 + PNVM) and
# Bluetooth.
PRODUCT_PACKAGES += \
    linux_firmware_iwlwifi-so-a0-gf-a0-89 \
    linux_firmware_iwlwifi-so-a0-gf-a0-pnvm \
    linux_firmware_btusb-ibt_ax211

# Firmware AOSP does not have (RTL8125, ibt-1040-0041), from the build host's
# linux-firmware, decompressed into device/android-pc/firmware by tools/gki-build.sh (step
# "firmware"). The PC kernel command line sets firmware_class.path=/vendor/firmware.
PC_FIRMWARE_DIR := device/android-pc/firmware
PRODUCT_COPY_FILES += $(foreach f,$(shell cd $(PC_FIRMWARE_DIR) 2>/dev/null && find . -type f | sed 's|^\./||' | sort),\
    $(PC_FIRMWARE_DIR)/$(f):$(TARGET_COPY_OUT_VENDOR)/firmware/$(f))

# Phase K1d A/B: NVIDIA's modules with the RM cores built by gcc (tools/nvidia-build.sh,
# gki-build.sh install), outside vendor_dlkm because the names clash; pcnvidia.sh insmods them
# on the entry with androidboot.android_pc.nvidia_rm=gcc.
PRODUCT_COPY_FILES += $(foreach f,$(wildcard device/android-pc/kernel/6.18/nvidia-gcc/*.ko),\
    $(f):$(TARGET_COPY_OUT_VENDOR)/lib/nvidia-gcc/$(notdir $(f)))

# Phase L1: pcnvtest, a glibc Vulkan client built on the build host (tools/nvtest-build.sh), run
# through the sidecar's glibc root by pcnvtest.sh.
PRODUCT_COPY_FILES += $(foreach f,$(wildcard device/android-pc/nvtest/pcnvtest),\
    $(f):$(TARGET_COPY_OUT_VENDOR)/bin/pcnvtest)

# Third-party apps (Chromium: AOSP ships only the WebView test shell as a browser; F-Droid;
# microG; Play Store; Termux),
# set up in device/android-pc/apps/<Name> by tools/fetch-apps.sh.
PRODUCT_PACKAGES += $(notdir $(wildcard device/android-pc/apps/*))
# JNI libraries an app's APK keeps compressed (Termux): a /product app's libraries are not
# extracted at boot, so they go beside the APK as <app>/lib/x86_64/*.so (android_app_import
# cannot install them).
PRODUCT_COPY_FILES += $(foreach f,$(wildcard device/android-pc/apps/*/lib/x86_64/*.so),\
    $(f):$(TARGET_COPY_OUT_PRODUCT)/app/$(word 4,$(subst /, ,$(f)))/lib/x86_64/$(notdir $(f)))

# libdrm's modetest for pcgpu.sh and pclog.sh (Phase G1). The generated vendor partition
# takes a PRODUCT_PACKAGES name in all its variants, so the system one goes in and must be
# allowed past generic_system.mk's artifact path requirement.
PRODUCT_PACKAGES += modetest
PRODUCT_ARTIFACT_PATH_REQUIREMENT_ALLOWED_LIST += system/bin/modetest
