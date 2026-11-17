# Board config for pc_x86_64: Cuttlefish x86_64-only board plus our vendor SELinux policy.

# Kernel: the android17-6.18 GKI image and modules built from source by tools/gki-build.sh
# (Cuttlefish's virtual-device modules plus the real-PC ones). Without that build the
# Cuttlefish prebuilts in kernel/prebuilts are used, as before F3a.
PC_KERNEL_DIR := device/android-pc/kernel/6.18
ifneq (,$(wildcard $(PC_KERNEL_DIR)/kernel-6.18))
SYSTEM_DLKM_SRC := $(PC_KERNEL_DIR)/system_dlkm
KERNEL_MODULES_PATH := $(PC_KERNEL_DIR)/vendor
TARGET_KERNEL_PATH := $(PC_KERNEL_DIR)/kernel-6.18
endif

include device/google/cuttlefish/vsoc_x86_64_only/BoardConfig.mk

# Cuttlefish's module blocklist plus NVIDIA's own GPU modules, which init.pc.rc loads only on the
# NVIDIA (proprietary) entry.
BOARD_VENDOR_KERNEL_MODULES_BLOCKLIST_FILE := device/android-pc/pc_x86_64/pc/modules.blocklist

# ndk_translation.mk installs the fetched libndk_translation ELF files with
# PRODUCT_COPY_FILES (as the emulator's own ndk_translation makefile does).
ifneq (,$(wildcard device/android-pc/ndk_translation/system/lib64/libndk_translation.so))
BUILD_BROKEN_ELF_PREBUILT_PRODUCT_COPY_FILES := true
# The Cuttlefish phone builds /system from the fixed Soong module aosp_shared_system_image,
# which ignores device PRODUCT_COPY_FILES. Without it Soong generates the system image
# from this product's own packages and copy files, as for most products. Set here because
# the board config is read after the product makefiles, which reset it to aosp_cf.mk's
# value; ndk_translation.mk clears PRODUCT_SOONG_DEFINED_SYSTEM_IMAGE (read-only here).
USE_SOONG_DEFINED_SYSTEM_IMAGE :=
endif

# Shared policy (te_macros) first, then the real-PC boot support, then one sepolicy/
# directory per HAL.
BOARD_VENDOR_SEPOLICY_DIRS += \
    device/android-pc/pc_x86_64/sepolicy/vendor \
    device/android-pc/pc_x86_64/pc/sepolicy \
    $(sort $(wildcard device/android-pc/pc_x86_64/hals/*/sepolicy))
