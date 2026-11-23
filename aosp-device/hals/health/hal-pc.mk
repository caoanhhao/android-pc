# PC product: replaces Cuttlefish's health APEX (com.google.cf.health, a fake battery), which
# Cuttlefish installs only while LOCAL_HEALTH_PRODUCT_PACKAGE is empty; recovery keeps
# Cuttlefish's own health HAL.
LOCAL_HEALTH_PRODUCT_PACKAGE := android.hardware.health-service.cuttlefish_recovery
