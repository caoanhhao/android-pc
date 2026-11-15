# Phase G3/G4 (docs/nvidia.md): Vulkan and GLES checks on the nouveau boot entry (pcvktest.sh,
# dEQP APK). G4's frame-rate benchmark (pcgfxbench.sh) was removed in I4 (docs/nvidia.md).
# vulkan.nouveau itself ships in the com.google.cf.vulkan APEX (cuttlefish patch).
PRODUCT_PACKAGES += pcvktest.sh pc_deqp_apk pcvkprobe
