# Real-PC boot support (phase F3a): init script, boot log saver and device firmware.
PRODUCT_PACKAGES += init.pc.rc init.pc.chromium.rc pclog.sh hals.pc.conf android.hardware.bluetooth-service.pc

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

# Third-party apps (Chromium: AOSP ships only the WebView test shell as a browser; F-Droid),
# set up in device/android-pc/apps/<Name> by tools/fetch-apps.sh.
PRODUCT_PACKAGES += $(notdir $(wildcard device/android-pc/apps/*))
