# PC product: replaces Cuttlefish's example power APEX (com.android.hardware.power, via
# "overrides" in Android.bp). ueventd.pc.rc opens the intel_pstate knobs to the HAL.
PRODUCT_PACKAGES += ueventd.pc.rc
