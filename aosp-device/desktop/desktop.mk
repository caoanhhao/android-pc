# Desktop mode (phase F9f, docs/desktop-mode.md): AOSP's own desktop windowing on the
# primary display (freeform windows with caption bars, Launcher3's taskbar), landscape only.

# Freeform windows: without this feature WindowManager refuses WINDOWING_MODE_FREEFORM.
PRODUCT_COPY_FILES += \
    frameworks/native/data/etc/android.software.freeform_window_management.xml:$(TARGET_COPY_OUT_VENDOR)/etc/permissions/android.software.freeform_window_management.xml

# The primary display (port 0, the PC's GOP framebuffer or Cuttlefish's display 0) starts
# in freeform (desktop) mode, rotation locked at 0 (the panel's own landscape).
PRODUCT_COPY_FILES += \
    $(PC_DEVICE_DIR)/desktop/display_settings.xml:$(TARGET_COPY_OUT_VENDOR)/etc/display_settings.xml

# Desktop mode supported on the internal display; auto-rotation off.
DEVICE_PACKAGE_OVERLAYS += $(PC_DEVICE_DIR)/desktop/overlay

# WM Shell (DesktopDisplayModeController) makes the default display desktop-first only on a
# laptop (touchpad + keyboard) or with an external display, and turns the freeform display
# back to fullscreen otherwise. A desktop PC has a mouse and keyboard but no touchpad, so
# desktop-first is forced.
PRODUCT_PRODUCT_PROPERTIES += \
    persist.wm.debug.force_desktop_first_on_default_display_for_testing=true
