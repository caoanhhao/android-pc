// SPDX-License-Identifier: GPL-2.0-only
/*
 * Plane update for pcfb: the ranchu composer scans out XBGR8888/ABGR8888 buffers, while
 * UEFI GOP framebuffers are XRGB8888 (or XBGR8888). drm_fb_blit() only converts from
 * XRGB8888, so the red/blue swap the other way round and the alpha formats are done here.
 * tools/gki-build.sh makes drm_sysfb_plane_helper_atomic_update() call this instead of
 * drm_fb_blit() and adds ARGB8888, XBGR8888 and ABGR8888 to the plane's formats.
 */

#include <drm/drm_format_helper.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_rect.h>
#include <linux/iosys-map.h>

#include "pcfb.h"

int pcfb_blit(struct iosys_map *dst, const unsigned int *dst_pitch, uint32_t dst_format,
	      const struct iosys_map *src, const struct drm_framebuffer *fb,
	      const struct drm_rect *clip, struct drm_format_conv_state *state)
{
	u32 fb_format = fb->format->format;

	/* The primary plane is the only one: alpha has nothing to blend with. */
	if (fb_format == DRM_FORMAT_ARGB8888)
		fb_format = DRM_FORMAT_XRGB8888;
	else if (fb_format == DRM_FORMAT_ABGR8888)
		fb_format = DRM_FORMAT_XBGR8888;

	if (fb_format == dst_format) {
		drm_fb_memcpy(dst, dst_pitch, src, fb, clip);
		return 0;
	}
	/* XRGB8888 <-> XBGR8888 is the same red/blue swap in both directions. */
	if ((fb_format == DRM_FORMAT_XBGR8888 && dst_format == DRM_FORMAT_XRGB8888) ||
	    (fb_format == DRM_FORMAT_XRGB8888 && dst_format == DRM_FORMAT_XBGR8888)) {
		drm_fb_xrgb8888_to_xbgr8888(dst, dst_pitch, src, fb, clip, state);
		return 0;
	}
	return drm_fb_blit(dst, dst_pitch, dst_format, src, fb, clip, state);
}
