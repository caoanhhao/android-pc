# Google extras (docs/google-extras.md): Google Play Store and libndk_translation are proprietary
# and not in the image. tools/add-google-extras.sh takes them from Google's Android Emulator images
# on the user's own computer and puts them on the stick's ESP; pcextras.sh copies them to
# /data/vendor/pc-extras on the next boot and mounts them over the image's placeholders, before
# zygote starts, so Android sees them as the system apps and libraries they were when built in.
# The libndk_translation placeholders are in ndk_translation.mk.
PRODUCT_PACKAGES += pcextras.sh init.pcextras.rc

# Play Store: an empty /product/priv-app/Phonesky (a directory needs a file to exist in the image)
# and its permission and sysconfig files as empty placeholders.
PRODUCT_COPY_FILES += \
    $(PC_DEVICE_DIR)/google_extras/placeholder/empty:$(TARGET_COPY_OUT_PRODUCT)/priv-app/Phonesky/.placeholder \
    $(PC_DEVICE_DIR)/google_extras/placeholder/permissions.xml:$(TARGET_COPY_OUT_PRODUCT)/etc/permissions/privapp-permissions-phonesky.xml \
    $(PC_DEVICE_DIR)/google_extras/placeholder/sysconfig.xml:$(TARGET_COPY_OUT_PRODUCT)/etc/sysconfig/sysconfig-phonesky.xml

# Pieces fetched for builds before the extras (tools/fetch-apps.sh playstore webview) must not
# end up in the image: pc/pc.mk installs every app in device/android-pc/apps.
PC_EXTRAS_NOT_BUILT_IN := Phonesky WebViewGoogle privapp-permissions-phonesky.xml sysconfig-phonesky.xml
