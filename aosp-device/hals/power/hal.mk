# Power HAL over intel_pstate. Replaces Cuttlefish's example power APEX
# (com.android.hardware.power, via "overrides" in Android.bp), whose power.stats half is
# installed here on its own. ueventd.pc.rc opens the intel_pstate knobs to the HAL.
PRODUCT_PACKAGES += \
    android.hardware.power-service.pc \
    android.hardware.power.stats-service.example \
    ueventd.pc.rc
