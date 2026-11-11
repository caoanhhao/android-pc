/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef PCFB_H
#define PCFB_H

#include <linux/types.h>

struct drm_format_conv_state;
struct drm_framebuffer;
struct drm_rect;
struct iosys_map;

/* drm_fb_blit() plus the conversions the ranchu composer needs (pcfb_blit.c). */
int pcfb_blit(struct iosys_map *dst, const unsigned int *dst_pitch, uint32_t dst_format,
	      const struct iosys_map *src, const struct drm_framebuffer *fb,
	      const struct drm_rect *clip, struct drm_format_conv_state *state);

#endif
