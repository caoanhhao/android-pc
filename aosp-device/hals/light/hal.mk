# Lights HAL over /sys/class/leds and /sys/class/backlight. Replaces Cuttlefish's
# vsock lights HAL (com.google.cf.light), which serves the same ILights/default instance.
LOCAL_ENABLE_LIGHT := false
PRODUCT_PACKAGES += android.hardware.light-service.pc
