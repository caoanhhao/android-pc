# Wi-Fi HAL over nl80211 (android.hardware.wifi V4). It replaces Cuttlefish's Wi-Fi APEX
# (com.android.hardware.wifi: AOSP's default HAL over the emulator legacy HAL) through
# "overrides" in Android.bp. PC_WIFI_HAL=legacy (environment or product makefile) builds
# that APEX instead: the fallback.
PC_WIFI_HAL ?= rust
ifeq ($(PC_WIFI_HAL),rust)
PRODUCT_PACKAGES += android.hardware.wifi-service.pc
endif

# Let Wi-Fi connect while Ethernet is up (both HALs): see overlay/res/values/defaults.xml.
PRODUCT_PACKAGES += pc_overlay_settings_provider_wifi
