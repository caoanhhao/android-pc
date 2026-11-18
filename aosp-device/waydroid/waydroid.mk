# android-pc in Waydroid's LXC container (docs/waydroid-aosp17.md). The same images as the PC;
# these modules only do something when Waydroid's host side sets ro.boot.android_pc.waydroid=1.

# Waydroid's composer: hwcomposer.waydroid (hardware/waydroid, fetched by
# tools/waydroid/aosp17-setup.sh) in AOSP's HIDL composer 2.1 service, a Wayland client of the
# host's compositor.
PRODUCT_PACKAGES += \
    android.hardware.graphics.composer@2.1-service \
    hwcomposer.waydroid \
    hwservicemanager \
    init.waydroid.vendor.rc \
    waydroid-composer-hidl.xml \
    waydroid-bind-mounts \
    waydroid-vintf-placeholder \
    waydroid-nvidia-nodes.sh

# The container's first stage runs it (system/core/init/waydroid_first_stage.cpp).
PRODUCT_PACKAGES += waydroid-preinit
PRODUCT_ARTIFACT_PATH_REQUIREMENT_ALLOWED_LIST += system/bin/waydroid-preinit
