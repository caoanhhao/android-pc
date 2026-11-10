# shellcheck shell=bash
# The kernel command line of the AOSP boots, shared by run-qemu.sh and
# mkusb.sh (sourced, needs $ROOT).
#
#   aosp_boot_args DISKS SIMPLEDRM DENSITY
#
# DISKS says where super, frp and misc are:
#   virtio  whole virtio disks vda, vdb, vdc at PCI 00:04.0-00:06.0
#   nvme    whole NVMe namespaces nvme0n1, nvme1n1, nvme2n1, same slots
#   usb     GPT partitions named super/frp/misc on a disk the kernel finds
#           at boot (a USB stick: androidboot.boot_devices=auto and
#           androidboot.partition_map=auto, filled in by the kernel)
# SIMPLEDRM=1 when the display is the firmware's framebuffer (the DRM
# driver then answers to the name the cuttlefish HALs look for).
# DENSITY is androidboot.lcd_density.
aosp_boot_args() {
    local disks=$1 simpledrm=$2 density=$3 a
    # Bootconfig values live on the command line; /proc/bootconfig shows
    # the androidboot.* ones.
    a="init=/init androidboot.hardware=cutf_cvm androidboot.fstab_suffix=rustpc"
    a+=" androidboot.slot_suffix=_a androidboot.force_normal_boot=0"
    case $disks in
        virtio|nvme)
            # The disks are PCI 00:04.0-00:06.0 (run-qemu.sh's -device
            # loop), so they are the boot devices.
            a+=" androidboot.boot_devices=pci0000:00/0000:00:04.0,pci0000:00/0000:00:05.0,pci0000:00/0000:00:06.0"
            if [[ $disks == nvme ]]; then
                a+=" androidboot.partition_map=nvme0n1,super;nvme1n1,frp;nvme2n1,misc"
            else
                a+=" androidboot.partition_map=vda,super;vdb,frp;vdc,misc"
            fi ;;
        usb) a+=" androidboot.boot_devices=auto androidboot.partition_map=auto" ;;
        *) echo "aosp_boot_args: unknown disks $disks" >&2; return 2 ;;
    esac
    # The fstab (with /misc) as a device-tree style fstab node: first
    # stage mounts from it, and fs_mgr's default fstab keeps it afterwards
    # (androidboot.android_dt_dir), see tools/mkaosp-init-initramfs.py.
    a+=" dtfstab=$(python3 "$ROOT/tools/mkaosp-init-initramfs.py" --dtfstab)"
    a+=" androidboot.android_dt_dir=/sys/firmware/devicetree/base/firmware/android/"
    a+=" androidboot.selinux=permissive androidboot.verifiedbootstate=orange"
    a+=" androidboot.veritymode=disabled androidboot.dynamic_partitions=true"
    a+=" androidboot.serialno=RUSTPC000001 androidboot.console=ttyS0 printk.devkmsg=on"
    # RAM disks: the block devices of the tmpfs /metadata and /data.
    a+=" brd.rd_nr=2 brd.rd_size=4096"
    a+=" androidboot.hardware.gralloc=minigbm androidboot.hardware.hwcomposer=ranchu"
    a+=" androidboot.hardware.egl=angle androidboot.hardware.vulkan=pastel androidboot.lcd_density=$density"
    # The rest of cuttlefish's --gpu_mode=guest_swiftshader settings
    # (ranchu composer on DRM, SwiftShader Vulkan under ANGLE).
    a+=" androidboot.hardware.hwcomposer.display_finder_mode=drm"
    a+=" androidboot.hardware.hwcomposer.display_framebuffer_format=rgba"
    a+=" androidboot.opengles.version=196609 androidboot.cpuvulkan.version=4202496"
    a+=" androidboot.hardware.angle_feature_overrides_enabled=mapUnspecifiedColorSpaceToPassThrough"
    a+=" androidboot.hardware.angle_feature_overrides_disabled=exposeES32ForTesting"
    a+=" androidboot.enable_bootanimation=1"
    [[ $simpledrm == 1 ]] && a+=" simpledrm.name=virtio_gpu"
    a+=" androidboot.setupwizard_mode=DISABLED androidboot.hw_timeout_multiplier=3"
    a+=" androidboot.hypervisor.vm.supported=0 androidboot.hypervisor.protected_vm.supported=0"
    # Cuttlefish picks one of several vendor APEX variants per module
    # (the values cvd-1's bootconfig uses); apexd aborts on duplicates.
    local v="androidboot.vendor.apex"
    a+=" $v.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure"
    a+=" $v.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure"
    a+=" $v.com.android.hardware.graphics.composer=com.android.hardware.graphics.composer.ranchu"
    a+=" $v.com.android.hardware.secure_element=com.android.hardware.secure_element"
    a+=" $v.com.android.hardware.strongbox=none $v.com.google.cf.disabled=none"
    a+=" $v.com.google.emulated.camera.provider.hal=com.google.emulated.camera.provider.hal"
    printf '%s\n' "$a"
}
