# Phase D, a Hyprland-style desktop shell (docs/hypr-desktop.md).

# PcSystemUI: a SystemUI variant (system_ext, platform key) built like CarSystemUI. It overrides
# SystemUI and composes WM Shell through its own Dagger module (PcWMShellModule), which will
# host the hyprtile layout engine. D0: the module provides exactly what AOSP's WMShellModule
# does, so behaviour is unchanged. Smart Dock and Launcher3 (desktop/desktop.mk) stay for now.
PRODUCT_PACKAGES += PcSystemUI
