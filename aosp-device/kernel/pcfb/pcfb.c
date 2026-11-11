// SPDX-License-Identifier: GPL-2.0-only
/*
 * pcfb: the UEFI GOP framebuffer as a DRM device for AOSP's Cuttlefish graphics HALs.
 *
 * The GKI image has no sysfb (CONFIG_SYSFB is off), so nothing registers the
 * "efi-framebuffer" platform device and efidrm cannot be a plain vendor module.
 * This module registers that device from the boot screen_info, then binds Linux's
 * efidrm driver to it (efidrm.c and the drm_sysfb helpers are copied from the
 * kernel tree by tools/gki-build.sh, with the changes listed there).
 *
 * The DRM driver is named "virtio_gpu" and has a render node: minigbm picks its allocation backend by the
 * driver name and has none for efidrm, and the ranchu HWC3 composer only accepts a
 * driver whose name contains "virtio". Both then use generic KMS only (dumb buffers,
 * PRIME, atomic commits), which efidrm provides; minigbm's virtgpu backend tolerates
 * the missing VIRTGPU_GETPARAM ioctl and runs as the 2D (non-virgl) virtio-gpu.
 *
 * The module is in the same vendor_dlkm as Cuttlefish's virtio-gpu, so it does nothing
 * unless the kernel command line has pcfb.enable=1 (set by tools/mkusb-aosp17.sh).
 */

#include "gen/efidrm_impl.h"

static bool enable;
module_param(enable, bool, 0444);
MODULE_PARM_DESC(enable, "Drive the UEFI framebuffer (off: the module does nothing)");

static struct platform_device *pcfb_device;

static int __init pcfb_init(void)
{
	struct resource res[SCREEN_INFO_MAX_RESOURCES];
	struct screen_info si = screen_info;
	ssize_t n;
	int ret;

	if (!enable)
		return 0;
	if (screen_info_video_type(&si) != VIDEO_TYPE_EFI) {
		pr_info("pcfb: no EFI framebuffer (video type %u)\n", screen_info_video_type(&si));
		return -ENODEV;
	}
	n = screen_info_resources(&si, res, ARRAY_SIZE(res));
	if (n <= 0)
		return n ? n : -ENODEV;

	pcfb_device = platform_device_alloc("efi-framebuffer", 0);
	if (!pcfb_device)
		return -ENOMEM;
	ret = platform_device_add_resources(pcfb_device, res, n);
	if (!ret)
		ret = platform_device_add_data(pcfb_device, &si, sizeof(si));
	if (!ret)
		ret = platform_device_add(pcfb_device);
	if (ret) {
		platform_device_put(pcfb_device);
		return ret;
	}

	ret = platform_driver_register(&efidrm_platform_driver);
	if (ret) {
		platform_device_unregister(pcfb_device);
		return ret;
	}
	pr_info("pcfb: %ux%u, %u bpp, stride %u, at %pR\n", si.lfb_width, si.lfb_height,
		si.lfb_depth, si.lfb_linelength, &res[0]);
	return 0;
}

static void __exit pcfb_exit(void)
{
	if (!pcfb_device)
		return;
	platform_driver_unregister(&efidrm_platform_driver);
	platform_device_unregister(pcfb_device);
}

module_init(pcfb_init);
module_exit(pcfb_exit);
