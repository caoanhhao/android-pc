# Health HAL over /sys/class/power_supply. Replaces Cuttlefish's health APEX
# (com.google.cf.health, a fake battery); recovery keeps Cuttlefish's own health HAL.
LOCAL_HEALTH_PRODUCT_PACKAGE := \
    android.hardware.health-service.pc \
    android.hardware.health-service.cuttlefish_recovery
