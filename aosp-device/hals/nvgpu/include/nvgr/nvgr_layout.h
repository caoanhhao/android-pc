/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

/*
 * nvgr: the buffer layout shared by android-pc's gralloc on nvidia-drm (minigbm's "nvidia"
 * backend) and the Vulkan HAL on NVIDIA's own driver (vulkan.nvidia, Phase L3).
 *
 * Every buffer of a format Vulkan can import (nvgr_fmt_from_drm() != NULL) is NVIDIA
 * block-linear with a layout that depends only on the format and the size. The Vulkan HAL
 * therefore knows the layout when it creates a VkImage for an AHardwareBuffer or a swapchain
 * buffer it has not seen yet (VK_ANDROID_external_memory_android_hardware_buffer and deferred
 * swapchain images), and NVIDIA's linear layout (which it cannot render to) is never needed.
 * The CPU sees these buffers through a linear shadow that the mapper fills and writes back
 * with nvgr_bl_copy().
 *
 * The rules were measured on the RTX 4070 (AD104) with NVIDIA 620.70.06 (docs/nvidia-native.md):
 * - modifier DRM_FORMAT_MOD_NVIDIA_BLOCK_LINEAR_2D(0, 1, 2, 6, h): no compression, sector
 *   layout 1 (desktop), page kind generation 2, generic page kind 0x06, block height 2^h GOBs;
 * - a GOB is 64 bytes x 8 rows (512 bytes); blocks are 1 GOB wide and 2^h GOBs high and are
 *   laid out row by row across the pitch;
 * - pitch = plane width in bytes aligned to 64; plane rows aligned to the block height;
 * - the planes of multi-planar formats follow each other with no gap: for an explicit layout
 *   (VkImageDrmFormatModifierExplicitCreateInfoEXT) NVIDIA sizes the image, and its GPU mapping,
 *   as the sum of the plane sizes, so a plane past that sum (NVIDIA's own implicit layouts put
 *   planes on 64 KiB boundaries) faults (Xid 39) when it is read. Every plane size is a whole
 *   number of blocks, so each plane still starts on a block boundary.
 *
 * Header only, C99, used by C (minigbm) and C++ (the Vulkan HAL and its host test).
 */

#ifndef NVGR_LAYOUT_H
#define NVGR_LAYOUT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

#define NVGR_MAX_PLANES 3

/* drm_fourcc.h's fourcc_code() and the formats used here, so this header needs no libdrm. */
#define NVGR_FOURCC(a, b, c, d)                                                                   \
	((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) | ((uint32_t)(d) << 24))
#define NVGR_DRM_ABGR8888 NVGR_FOURCC('A', 'B', '2', '4')
#define NVGR_DRM_XBGR8888 NVGR_FOURCC('X', 'B', '2', '4')
#define NVGR_DRM_ARGB8888 NVGR_FOURCC('A', 'R', '2', '4')
#define NVGR_DRM_XRGB8888 NVGR_FOURCC('X', 'R', '2', '4')
#define NVGR_DRM_RGB565 NVGR_FOURCC('R', 'G', '1', '6')
#define NVGR_DRM_ABGR2101010 NVGR_FOURCC('A', 'B', '3', '0')
#define NVGR_DRM_XBGR2101010 NVGR_FOURCC('X', 'B', '3', '0')
#define NVGR_DRM_ABGR16161616F NVGR_FOURCC('A', 'B', '4', 'H')
#define NVGR_DRM_R8 NVGR_FOURCC('R', '8', ' ', ' ')
#define NVGR_DRM_GR88 NVGR_FOURCC('G', 'R', '8', '8')
#define NVGR_DRM_R16 NVGR_FOURCC('R', '1', '6', ' ')
#define NVGR_DRM_GR1616 NVGR_FOURCC('G', 'R', '3', '2')
#define NVGR_DRM_NV12 NVGR_FOURCC('N', 'V', '1', '2')
#define NVGR_DRM_NV21 NVGR_FOURCC('N', 'V', '2', '1')
#define NVGR_DRM_YVU420 NVGR_FOURCC('Y', 'V', '1', '2')
#define NVGR_DRM_YVU420_ANDROID NVGR_FOURCC('9', '9', '9', '7') /* minigbm's own */
#define NVGR_DRM_P010 NVGR_FOURCC('P', '0', '1', '0')

#define NVGR_MOD_LINEAR 0ull
/* DRM_FORMAT_MOD_NVIDIA_BLOCK_LINEAR_2D(c, s, g, k, h) of drm_fourcc.h */
#define NVGR_MOD_NVIDIA_BL_2D(c, s, g, k, h)                                                      \
	((0x03ull << 56) | 0x10 | ((uint64_t)(h) & 0xf) | (((uint64_t)(k) & 0xff) << 12) |        \
	 (((uint64_t)(g) & 0x3) << 20) | (((uint64_t)(s) & 0x1) << 22) |                           \
	 (((uint64_t)(c) & 0x7) << 23))
/* what nvidia-drm's DRM_NVIDIA_GET_DEV_INFO reports on the RTX 4070 */
#define NVGR_PAGE_KIND 0x06
#define NVGR_KIND_GENERATION 2
#define NVGR_SECTOR_LAYOUT 1
#define NVGR_MAX_LOG2_BLOCK_H 4

#define NVGR_GOB_WIDTH 64u
#define NVGR_GOB_HEIGHT 8u
#define NVGR_GOB_SIZE 512u
#define NVGR_SIZE_ALIGN 4096u

/* How a format splits into planes; in Vulkan's plane order (Y, Cb, Cr). */
struct nvgr_fmt {
	uint32_t drm_format;
	uint32_t num_planes;
	uint32_t cpp[NVGR_MAX_PLANES];
	uint32_t hsub[NVGR_MAX_PLANES];
	uint32_t vsub[NVGR_MAX_PLANES];
	/* the chroma of the second (interleaved) plane is CrCb, not CbCr (NV21) */
	bool swap_chroma;
};

struct nvgr_layout {
	uint64_t modifier;
	uint32_t log2_block_h;
	uint32_t num_planes;
	uint32_t offsets[NVGR_MAX_PLANES];
	uint32_t pitches[NVGR_MAX_PLANES];
	uint32_t widths[NVGR_MAX_PLANES]; /* plane width in pixels */
	uint32_t heights[NVGR_MAX_PLANES]; /* plane height in rows (not aligned) */
	uint32_t sizes[NVGR_MAX_PLANES];
	uint64_t size; /* what the memory must hold */
};

static const struct nvgr_fmt nvgr_formats[] = {
	{ NVGR_DRM_ABGR8888, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_XBGR8888, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_ARGB8888, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_XRGB8888, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_RGB565, 1, { 2 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_ABGR2101010, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_XBGR2101010, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_ABGR16161616F, 1, { 8 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_R8, 1, { 1 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_GR88, 1, { 2 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_R16, 1, { 2 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_GR1616, 1, { 4 }, { 1 }, { 1 }, false },
	{ NVGR_DRM_NV12, 2, { 1, 2 }, { 1, 2 }, { 1, 2 }, false },
	{ NVGR_DRM_NV21, 2, { 1, 2 }, { 1, 2 }, { 1, 2 }, true },
	{ NVGR_DRM_YVU420, 3, { 1, 1, 1 }, { 1, 2, 2 }, { 1, 2, 2 }, false },
	{ NVGR_DRM_YVU420_ANDROID, 3, { 1, 1, 1 }, { 1, 2, 2 }, { 1, 2, 2 }, false },
	{ NVGR_DRM_P010, 2, { 2, 4 }, { 1, 2 }, { 1, 2 }, false },
};

/* NULL: not a block-linear format (the buffer is linear and Vulkan sees no image in it). */
static inline const struct nvgr_fmt *nvgr_fmt_from_drm(uint32_t drm_format)
{
	for (size_t i = 0; i < sizeof(nvgr_formats) / sizeof(nvgr_formats[0]); i++)
		if (nvgr_formats[i].drm_format == drm_format)
			return &nvgr_formats[i];
	return NULL;
}

static inline uint64_t nvgr_align(uint64_t v, uint64_t a)
{
	return (v + a - 1) / a * a;
}

static inline uint64_t nvgr_modifier(uint32_t log2_block_h)
{
	return NVGR_MOD_NVIDIA_BL_2D(0, NVGR_SECTOR_LAYOUT, NVGR_KIND_GENERATION, NVGR_PAGE_KIND,
				     log2_block_h);
}

/* the block height of nvgr_modifier(), or -1 for other modifiers */
static inline int nvgr_log2_block_h(uint64_t modifier)
{
	for (uint32_t h = 0; h <= NVGR_MAX_LOG2_BLOCK_H; h++)
		if (modifier == nvgr_modifier(h))
			return (int)h;
	return -1;
}

/* The smallest block that holds the whole height, at most 16 GOBs (128 rows). */
static inline uint32_t nvgr_choose_log2_block_h(uint32_t height)
{
	uint32_t h = 0;

	while (h < NVGR_MAX_LOG2_BLOCK_H && (NVGR_GOB_HEIGHT << h) < height)
		h++;
	return h;
}

static inline void nvgr_compute_layout(const struct nvgr_fmt *f, uint32_t width, uint32_t height,
				       struct nvgr_layout *l)
{
	const uint32_t h = nvgr_choose_log2_block_h(height);
	const uint32_t block_rows = NVGR_GOB_HEIGHT << h;
	uint64_t end = 0;

	memset(l, 0, sizeof(*l));
	l->modifier = nvgr_modifier(h);
	l->log2_block_h = h;
	l->num_planes = f->num_planes;
	for (uint32_t p = 0; p < f->num_planes; p++) {
		const uint32_t pw = (width + f->hsub[p] - 1) / f->hsub[p];
		const uint32_t ph = (height + f->vsub[p] - 1) / f->vsub[p];

		l->widths[p] = pw;
		l->heights[p] = ph;
		l->pitches[p] = (uint32_t)nvgr_align((uint64_t)pw * f->cpp[p], NVGR_GOB_WIDTH);
		l->sizes[p] = l->pitches[p] * (uint32_t)nvgr_align(ph, block_rows);
		l->offsets[p] = (uint32_t)end;
		end += l->sizes[p];
	}
	l->size = nvgr_align(end, NVGR_SIZE_ALIGN);
}

/* Byte offset of byte column x (in bytes) and row y of a block-linear plane. */
static inline uint64_t nvgr_bl_offset(uint32_t x, uint32_t y, uint32_t pitch, uint32_t log2_block_h)
{
	const uint32_t block_rows = NVGR_GOB_HEIGHT << log2_block_h;
	const uint64_t block = (uint64_t)(y / block_rows) * (pitch / NVGR_GOB_WIDTH) + x / NVGR_GOB_WIDTH;
	/* inside a GOB: 16-byte sectors, 4 rows of the left 32 bytes, then of the right 32 */
	const uint32_t gx = x % NVGR_GOB_WIDTH, gy = y % NVGR_GOB_HEIGHT;
	const uint32_t in_gob = ((gx & 32) << 3) | ((gy & 4) << 5) | ((gx & 16) << 2) |
				((gy & 3) << 4) | (gx & 15);

	return block * (NVGR_GOB_SIZE << log2_block_h) +
	       ((y % block_rows) / NVGR_GOB_HEIGHT) * NVGR_GOB_SIZE + in_gob;
}

/*
 * Copies rows [y0, y1) of one plane between block-linear memory (bl, the plane's start) and a
 * linear picture (lin, its row 0, lin_stride bytes per row). width_bytes: the bytes of a row that
 * hold pixels. to_linear: detile (bl -> lin), else tile (lin -> bl). A whole GOB row is 512
 * contiguous bytes in bl, so bl is read and written GOB by GOB (sequential for write-combined
 * video memory).
 */
static inline void nvgr_bl_copy(uint8_t *bl, uint8_t *lin, uint32_t lin_stride, uint32_t pitch,
				uint32_t log2_block_h, uint32_t width_bytes, uint32_t y0, uint32_t y1,
				bool to_linear)
{
	uint8_t gob[NVGR_GOB_SIZE];

	for (uint32_t gy = y0 / NVGR_GOB_HEIGHT * NVGR_GOB_HEIGHT; gy < y1; gy += NVGR_GOB_HEIGHT) {
		for (uint32_t gx = 0; gx < width_bytes; gx += NVGR_GOB_WIDTH) {
			uint8_t *src = bl + nvgr_bl_offset(gx, gy, pitch, log2_block_h);
			const uint32_t ry0 = gy < y0 ? y0 : gy;
			const uint32_t ry1 = gy + NVGR_GOB_HEIGHT < y1 ? gy + NVGR_GOB_HEIGHT : y1;
			const bool partial = ry0 != gy || ry1 != gy + NVGR_GOB_HEIGHT ||
					     gx + NVGR_GOB_WIDTH > width_bytes;

			if (to_linear || partial)
				memcpy(gob, src, NVGR_GOB_SIZE);
			for (uint32_t y = ry0; y < ry1; y++) {
				for (uint32_t sx = 0; sx < NVGR_GOB_WIDTH && gx + sx < width_bytes;
				     sx += 16) {
					const uint32_t n = gx + sx + 16 <= width_bytes ?
								   16 :
								   width_bytes - gx - sx;
					uint8_t *g = gob + (((sx & 32) << 3) | (((y - gy) & 4) << 5) |
							    ((sx & 16) << 2) | (((y - gy) & 3) << 4));
					uint8_t *l = lin + (size_t)y * lin_stride + gx + sx;

					if (to_linear)
						memcpy(l, g, n);
					else
						memcpy(g, l, n);
				}
			}
			if (!to_linear)
				memcpy(src, gob, NVGR_GOB_SIZE);
		}
	}
}

/*
 * NVIDIA reads and writes imported system memory (udmabuf) without snooping the CPU caches
 * (measured: stale and lost cache lines without this). Write back and drop the CPU's lines of
 * a range before the GPU reads it and before the CPU reads what the GPU wrote. x86 only.
 */
static inline void nvgr_clflush(const void *addr, size_t size)
{
#if defined(__x86_64__) || defined(__i386__)
	const uintptr_t start = (uintptr_t)addr & ~(uintptr_t)63;
	const uintptr_t end = (uintptr_t)addr + size;

	__asm__ volatile("mfence" ::: "memory");
	for (uintptr_t p = start; p < end; p += 64)
		__asm__ volatile("clflush (%0)" ::"r"(p) : "memory");
	__asm__ volatile("mfence" ::: "memory");
#else
	(void)addr;
	(void)size;
#endif
}

#ifdef __cplusplus
}
#endif

#endif /* NVGR_LAYOUT_H */
