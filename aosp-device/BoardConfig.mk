# Board config for pc_x86_64: Cuttlefish x86_64-only board plus our vendor SELinux policy.

include device/google/cuttlefish/vsoc_x86_64_only/BoardConfig.mk

# Shared policy (te_macros) first, then one sepolicy/ directory per HAL.
BOARD_VENDOR_SEPOLICY_DIRS += \
    device/android-pc/pc_x86_64/sepolicy/vendor \
    $(sort $(wildcard device/android-pc/pc_x86_64/hals/*/sepolicy))
