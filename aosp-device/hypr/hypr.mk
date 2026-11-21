# Phase D, a Hyprland-style desktop shell (docs/hypr-desktop.md).

# PcSystemUI: a SystemUI variant (system_ext, platform key) built like CarSystemUI. It overrides
# SystemUI and composes WM Shell through its own Dagger module (PcWMShellModule), which hosts the
# hyprtile layout engine and IHyprShell.
PRODUCT_PACKAGES += PcSystemUI

# HyprLauncher (D4): home and recents provider with the bar, the app launcher and the overview.
# It overrides Launcher3/Quickstep (Android.bp); hypr_overlay_frameworks_base_core makes it the
# recents provider and gives SystemUI's status bar no height. Smart Dock (H4) is no longer
# installed, and its first-boot setup (desktop/smartdock-setup.sh) is gone from desktop.mk; a
# userdata from an earlier build moves its HOME role to HyprLauncher (HomeRole.kt).
PRODUCT_PACKAGES += HyprLauncher hypr_overlay_frameworks_base_core
PRODUCT_PACKAGES := $(filter-out SmartDock,$(PRODUCT_PACKAGES))
