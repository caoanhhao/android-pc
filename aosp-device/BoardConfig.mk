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

# Shared policy (te_macros) first, then the real-PC boot support, then one sepolicy/
# directory per HAL.
BOARD_VENDOR_SEPOLICY_DIRS += \
    device/android-pc/pc_x86_64/sepolicy/vendor \
    device/android-pc/pc_x86_64/pc/sepolicy \
    $(sort $(wildcard device/android-pc/pc_x86_64/hals/*/sepolicy))
