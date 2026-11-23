# Board config for gts7l: Samsung Galaxy Tab S7 LTE (SM-T875), Snapdragon 865+ (kona, SM8250),
# booted by Samsung's ABL (docs/gts7l.md). No Cuttlefish board underneath.

PC_DEVICE_DIR := device/android-pc/pc_x86_64
GTS7L_DEVICE_DIR := $(PC_DEVICE_DIR)/gts7l

# Kryo 585: Cortex-A77 big cores and A55 little ones (ARMv8.2-A with dot product). 64-bit only.
TARGET_ARCH := arm64
TARGET_ARCH_VARIANT := armv8-2a-dotprod
TARGET_CPU_ABI := arm64-v8a
TARGET_CPU_VARIANT := cortex-a76
TARGET_2ND_ARCH :=
TARGET_2ND_ARCH_VARIANT :=
TARGET_2ND_CPU_ABI :=
TARGET_2ND_CPU_VARIANT :=
AUDIOSERVER_MULTILIB := first

TARGET_BOARD_PLATFORM := kona
TARGET_BOOTLOADER_BOARD_NAME := kona
TARGET_NO_BOOTLOADER := true
# TWRP keeps the recovery partition; we build no recovery image (docs/gts7l.md §6/T6).
TARGET_NO_RECOVERY := true

# Kernel modules from tools/gts7l-kernel.sh (out/gts7l/modules, copied to
# device/android-pc/gts7l-kernel by tools/aosp-device-link.sh); the kernel itself is a
# PRODUCT_COPY_FILES entry in aosp_gts7l.mk.
GTS7L_KERNEL_DIR := device/android-pc/gts7l-kernel
BOARD_VENDOR_KERNEL_MODULES := $(sort $(wildcard $(GTS7L_KERNEL_DIR)/modules/*.ko))

# Boot image: header v1 with the DTB appended to Image.gz (Samsung's ABL on kona), the
# generic ramdisk with first-stage init and fstab.gts7l. Offsets are those of the stock
# boot.img. The Soong-only build's boot.img (fsgen) ignores BOARD_MKBOOTIMG_ARGS, so
# tools/gts7l-image.sh (S4) packs the flashable boot.img with them. TODO(S1): check header version, base, offsets and page size against the unpacked
# stock T875XXS8DXH1 boot.img (docs/gts7l-inventory.md).
BOARD_BOOT_HEADER_VERSION := 1
BOARD_KERNEL_BASE := 0x00000000
BOARD_KERNEL_PAGESIZE := 4096
BOARD_MKBOOTIMG_ARGS += \
    --kernel_offset 0x00008000 \
    --ramdisk_offset 0x02000000 \
    --tags_offset 0x01e00000
# androidboot.boot_devices: the UFS controller's platform device on mainline, for the
# /dev/block/by-name links. fstab_suffix: ABL may pass its own androidboot.hardware (qcom).
# firmware_class.path: the link tree into the tablet's firmware partitions
# (firmware/init.gts7l.firmware.rc).
# TODO(S6): check the UFS device path and the console UART on the running kernel.
BOARD_KERNEL_CMDLINE := \
    androidboot.boot_devices=soc@0/1d84000.ufshc \
    androidboot.fstab_suffix=gts7l \
    androidboot.hardware=gts7l \
    firmware_class.path=/mnt/vendor/firmware
# Bring-up: our vendor policy is new and untested on hardware, so SELinux starts permissive
# until the first boots have shown the denials. TODO(S7): set to false.
GTS7L_SELINUX_PERMISSIVE ?= true
ifeq ($(GTS7L_SELINUX_PERMISSIVE),true)
BOARD_KERNEL_CMDLINE += androidboot.selinux=permissive
endif

# Partitions: non-A/B, with the stock UFS layout. super (sda29) holds the logical partitions;
# the stock odm is not used (odm files go to vendor/odm).
TARGET_USERIMAGES_USE_EXT4 := true
TARGET_USERIMAGES_USE_F2FS := true
BOARD_FLASH_BLOCK_SIZE := 131072
BOARD_USES_METADATA_PARTITION := true
BOARD_SYSTEMIMAGE_FILE_SYSTEM_TYPE := ext4
BOARD_USES_SYSTEM_EXTIMAGE := true
BOARD_SYSTEM_EXTIMAGE_FILE_SYSTEM_TYPE := ext4
BOARD_USES_PRODUCTIMAGE := true
BOARD_PRODUCTIMAGE_FILE_SYSTEM_TYPE := ext4
BOARD_USES_VENDORIMAGE := true
BOARD_VENDORIMAGE_FILE_SYSTEM_TYPE := ext4
TARGET_COPY_OUT_SYSTEM_EXT := system_ext
TARGET_COPY_OUT_PRODUCT := product
TARGET_COPY_OUT_VENDOR := vendor
# userdata is formatted on the tablet (TWRP, first boot), never written by an update.
BOARD_USERDATAIMAGE_FILE_SYSTEM_TYPE := f2fs

# super: one group for the four images. The size is a placeholder (8 GiB); the group leaves
# 4 MiB for the non-A/B metadata.
# TODO(S1): BOARD_SUPER_PARTITION_SIZE = the exact byte size of sda29 from the stock layout
# (docs/gts7l-inventory.md), and the group size = that minus 4 MiB.
BOARD_SUPER_PARTITION_SIZE := 8589934592
BOARD_SUPER_PARTITION_GROUPS := gts7l_dynamic_partitions
BOARD_GTS7L_DYNAMIC_PARTITIONS_SIZE := 8585740288
BOARD_GTS7L_DYNAMIC_PARTITIONS_PARTITION_LIST := system system_ext product vendor

# Verified boot stays off: the unlocked ABL ignores it, and S4 flashes a vbmeta with
# verification disabled (docs/gts7l.md §8).
BOARD_AVB_ENABLE := false

# SELinux: the shared te_macros, the tablet's own policy, then one directory per HAL in
# hals.list. The hypr shell's system_ext policy as on the PC.
include $(GTS7L_DEVICE_DIR)/hals.mk
BOARD_VENDOR_SEPOLICY_DIRS += \
    $(PC_DEVICE_DIR)/sepolicy/vendor \
    $(GTS7L_DEVICE_DIR)/sepolicy \
    $(foreach hal,$(GTS7L_HALS),$(wildcard $(PC_DEVICE_DIR)/hals/$(hal)/sepolicy))
SYSTEM_EXT_PRIVATE_SEPOLICY_DIRS += \
    $(PC_DEVICE_DIR)/hypr/sepolicy \
    $(PC_DEVICE_DIR)/hypr/HyprTop/sepolicy
