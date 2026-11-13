# WebView provider (phase F9p, docs/arm-translation.md): Google's Android System WebView
# (com.google.android.webview, fetched from the emulator image by tools/fetch-apps.sh
# webview) is the default, as on a Google device; AOSP's com.android.webview stays
# installed and can be picked in Developer options > WebView implementation.
# WebViewUpdateService uses the first availableByDefault provider and does not fall back to
# the next one, so the overlay is only added when the APK has been fetched.
ifneq (,$(wildcard device/android-pc/apps/WebViewGoogle/WebViewGoogle.apk))
DEVICE_PACKAGE_OVERLAYS += $(PC_DEVICE_DIR)/webview/overlay
endif
