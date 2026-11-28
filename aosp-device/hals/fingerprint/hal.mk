# Fingerprint HAL: the Goodix GW36T1 in the power key through Samsung's securefp TA
# (/dev/qseecom, /dev/goodix_fp; docs/gts7l.md §25). gts7l only: no hal-pc.mk.
PRODUCT_PACKAGES += \
    android.hardware.biometrics.fingerprint-service.gts7l \
    gts7l_fpctl
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.hardware.fingerprint.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.hardware.fingerprint.xml
