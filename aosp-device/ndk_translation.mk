# ARM64 app support: Google's libndk_translation native bridge (docs/arm-translation.md).
# The proprietary part (the translator, its host-side proxy libraries and the binfmt_misc
# program runner) is fetched by tools/fetch-ndk-translation.sh into
# device/android-pc/ndk_translation/system/, not in git. Everything else is built from AOSP
# source, as Google's own ndk_translation makefile does: the arm64 guest libraries
# (native_bridge_support, for TARGET_NATIVE_BRIDGE_ARCH arm64 from the Cuttlefish board),
# the binfmt_misc/init/linker config files (frameworks/libs/binary_translation/prebuilt) and
# libberberis_exec_region (the low-2G code region the translator maps at a fixed path).
# Without the fetch nothing changes.

PC_NDKT_DIR := device/android-pc/ndk_translation/system
ifneq (,$(wildcard $(PC_NDKT_DIR)/lib64/libndk_translation.so))

PRODUCT_COPY_FILES += $(foreach f,$(shell cd $(PC_NDKT_DIR) && find . -type f | sed 's|^\./||' | sort),\
    $(PC_NDKT_DIR)/$(f):$(TARGET_COPY_OUT_SYSTEM)/$(f))

include frameworks/libs/native_bridge_support/native_bridge_support.mk
# The guest libc (libnative_bridge_guest_libc) lives in its own Soong namespace.
PRODUCT_SOONG_NAMESPACES += frameworks/libs/native_bridge_support/android_api/libc
PRODUCT_PACKAGES += \
    $(NATIVE_BRIDGE_PRODUCT_PACKAGES) \
    arm64_dyn \
    arm64_exe \
    berberis_arm_or_arm64.rc \
    berberis_cpuinfo_arm64 \
    ld.config.arm64.txt \
    libberberis_exec_region \
    libpc_native_bridge

# Overrides ro.dalvik.vm.native.bridge?=0 from build/make/target/product/runtime_libart.mk.
# ro.dalvik.vm.isa.arm64 also triggers the binfmt_misc registration in
# berberis_arm_or_arm64.rc, so arm64 executables run too (ro.enable.native.bridge.exec).
# PC_NATIVE_BRIDGE=off keeps every file but leaves the bridge switched off (fallback/debug).
# ART loads libpc_native_bridge (nbshim/), which forwards to libndk_translation and adds the
# H3 handler for arm64 code that PLT-hooks host libraries (docs/arm-translation.md, "H3").
ifneq ($(PC_NATIVE_BRIDGE),off)
PRODUCT_SYSTEM_PROPERTIES += \
    ro.dalvik.vm.native.bridge=libpc_native_bridge.so \
    ro.dalvik.vm.isa.arm64=x86_64 \
    ro.enable.native.bridge.exec=1
endif
# The Cuttlefish phone inherits generic_system.mk, whose artifact path check rejects device
# system properties and files unless they are listed here.
PRODUCT_ARTIFACT_PATH_REQUIREMENT_SYSPROP_ALLOWED_LIST += \
    ro.dalvik.vm.native.bridge \
    ro.dalvik.vm.isa.arm64 \
    ro.enable.native.bridge.exec
PRODUCT_ARTIFACT_PATH_REQUIREMENT_ALLOWED_LIST += \
    system/etc/berberis/cpuinfo.arm64.txt \
    system/etc/binfmt_misc/arm64_dyn \
    system/etc/binfmt_misc/arm64_exe \
    system/etc/init/berberis_arm_or_arm64.rc \
    system/etc/ld.config.arm64.txt \
    system/lib64/libberberis_exec_region.so \
    system/lib64/libpc_native_bridge.so \
    system/lib64/libblasV8.so

# Cuttlefish's fixed Soong system image ignores device packages and PRODUCT_COPY_FILES on
# /system: drop it, so Soong generates /system from this product (BoardConfig.mk clears
# USE_SOONG_DEFINED_SYSTEM_IMAGE, which the product makefiles cannot).
PRODUCT_SOONG_DEFINED_SYSTEM_IMAGE :=

endif
