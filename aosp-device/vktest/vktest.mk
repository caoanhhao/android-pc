# Phase G3/G4 (docs/nvidia.md): Vulkan and GLES checks on the nouveau boot entry (pcvktest.sh,
# dEQP APK) and the frame-rate benchmark on every entry (pcgfxbench.sh, angeles, hwuimacro).
# vulkan.nouveau itself ships in the com.google.cf.vulkan APEX (cuttlefish patch).
PRODUCT_PACKAGES += pcvktest.sh pc_deqp_apk pcgfxbench.sh pc_bench_angeles pc_bench_hwuimacro
