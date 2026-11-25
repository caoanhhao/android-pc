# Camera HAL for USB webcams (V4L2) and raw CAMSS sensors with a software ISP (gts7l,
# docs/gts7l-hal/camera.md). Cuttlefish's own provider (emulated scenes, or AOSP's
# C++ external HAL from the .v4l2 APEX) stays: it serves another instance. On the real PC
# the boot config should switch it off (`androidboot.vendor.apex.com.google.emulated.camera.
# provider.hal=none`, see docs/rust-hals.md, F7).
PRODUCT_PACKAGES += android.hardware.camera.provider-service.pc
