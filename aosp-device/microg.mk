# microG GmsCore (open-source Google Play services), preinstalled and enabled in
# /product/priv-app, next to Google's Play Store (docs/arm-translation.md). The APKs come
# from tools/fetch-apps.sh microg playstore (pc/pc.mk installs every fetched app).
# microG's signature check needs tools/patches/aosp17-microg-signature-spoofing.patch in frameworks/base.

ifneq (,$(wildcard device/android-pc/apps/GmsCore/GmsCore.apk))
PRODUCT_PACKAGES += privapp-permissions-microg.xml sysconfig-microg.xml microg.xml
endif
