# microG GmsCore (open-source Google Play services), preinstalled and enabled in
# /product/priv-app (docs/arm-translation.md), next to Google's Play Store once the user has
# added it (docs/google-extras.md). The APK comes from tools/fetch-apps.sh microg (pc/pc.mk
# installs every fetched app).
# microG's signature check needs tools/patches/aosp17-microg-signature-spoofing.patch in frameworks/base.

ifneq (,$(wildcard device/android-pc/apps/GmsCore/GmsCore.apk))
PRODUCT_PACKAGES += privapp-permissions-microg.xml sysconfig-microg.xml microg.xml
endif
