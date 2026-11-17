/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

/*
 * libgbm_mesa_wrapper.so for Waydroid's minigbm_gbm_mesa gralloc on NVIDIA's own driver
 * (docs/waydroid-nvidia.md).
 *
 * Waydroid's gralloc (android_external_minigbm, gbm_mesa backend) allocates, imports and maps
 * every buffer through this library's struct gbm_ops. Buffers of the single-plane formats
 * vulkan.nvidia can image are allocated here exactly as android-pc's minigbm "nvidia" backend
 * does (tools/patches/aosp17-minigbm-nvidia.patch): NVIDIA block-linear with the layout of
 * nvgr_layout.h, which vulkan.nvidia computes from the format and size alone.
 *   - GPU-only buffers: video memory from nvidia-drm (DRM_IOCTL_NVIDIA_GEM_ALLOC_NVKMS_MEMORY on
 *     the render node in gralloc.gbm.device), exported as a dma-buf; Hyprland on the same GPU
 *     shows them without a copy.
 *   - Buffers the CPU touches (the backend asks for a map stride): system memory, a sealed memfd
 *     made a dma-buf by /dev/udmabuf. The CPU maps a linear shadow: map detiles, unmap tiles back.
 * Everything else (YUV, blobs, other formats) goes to the vendor image's own wrapper, renamed
 * libgbm_mesa_wrapper_intel.so, on the Intel iGPU (waydroid.nvgbm.inner_device).
 *
 * The backend sizes the buffer as out_stride x height after alloc() returns, so alloc() raises
 * args->height until that covers the block-linear layout (rows aligned to the block height).
 *
 * Properties:
 *   waydroid.nvgbm.inner          inner wrapper (default /vendor/lib64/libgbm_mesa_wrapper_intel.so)
 *   waydroid.nvgbm.inner_device   its DRM node (default /dev/dri/renderD128)
 */

#define _GNU_SOURCE
#define LOG_TAG "nvgbm"

#include <android/log.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/system_properties.h>
#include <unistd.h>

#include <nvgr/nvgr_layout.h>

#include "gbm_mesa_wrapper.h"

#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

/* drm.h and nvidia-drm's nv_drm_common_ioctl.h (MIT), the parts used here */
#define DRM_IOCTL_BASE 'd'
#define DRM_COMMAND_BASE 0x40
struct drm_gem_close {
	uint32_t handle;
	uint32_t pad;
};
struct drm_prime_handle {
	uint32_t handle;
	uint32_t flags;
	int32_t fd;
};
#define DRM_IOCTL_GEM_CLOSE _IOW(DRM_IOCTL_BASE, 0x09, struct drm_gem_close)
#define DRM_IOCTL_PRIME_HANDLE_TO_FD _IOWR(DRM_IOCTL_BASE, 0x2d, struct drm_prime_handle)
#define DRM_RDWR O_RDWR

#define NV_GEM_ALLOC_NO_SCANOUT (1 << 0)
struct drm_nvidia_get_dev_info_params {
	uint32_t gpu_id;
	uint32_t mig_device;
	uint32_t primary_index;
	uint32_t supports_alloc;
	uint32_t generic_page_kind;
	uint32_t page_kind_generation;
	uint32_t sector_layout;
	uint32_t supports_sync_fd;
	uint32_t supports_semsurf;
};
struct drm_nvidia_gem_alloc_nvkms_memory_params {
	uint32_t handle;
	uint8_t block_linear;
	uint8_t compressible;
	uint16_t __pad0;
	uint64_t memory_size;
	uint32_t flags;
	uint32_t __pad1;
};
#define DRM_IOCTL_NVIDIA_GET_DEV_INFO                                                              \
	_IOWR(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x03, struct drm_nvidia_get_dev_info_params)
#define DRM_IOCTL_NVIDIA_GEM_ALLOC_NVKMS_MEMORY                                                    \
	_IOWR(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0x0b,                                             \
	      struct drm_nvidia_gem_alloc_nvkms_memory_params)

/* the gbm_device this library hands out: NVIDIA's render node plus the inner wrapper's device */
struct nvdev {
	int fd;
	struct gbm_device *inner;
	int inner_fd;
};

/* the gbm_bo this library hands out: one of ours, or the inner wrapper's */
struct nvbo {
	struct gbm_bo *inner;
	int fd;
	const struct nvgr_fmt *fmt;
	uint32_t width, height;
	struct nvgr_layout layout;
};

/* a CPU mapping of one of our buffers */
struct nvmap {
	uint8_t *gpu;
	size_t gpu_length;
	uint8_t *shadow;
	size_t shadow_length;
};

static struct {
	pthread_once_t once;
	pthread_mutex_t lock;
	struct gbm_ops *inner;
	struct gbm_ops ops;
	char inner_device[PROP_VALUE_MAX];
	int udmabuf;
} g = {
	.once = PTHREAD_ONCE_INIT,
	.lock = PTHREAD_MUTEX_INITIALIZER,
	.udmabuf = -1,
};

static int ioctl_retry(int fd, unsigned long req, void *arg)
{
	int ret;

	do {
		ret = ioctl(fd, req, arg);
	} while (ret == -1 && (errno == EINTR || errno == EAGAIN));
	return ret;
}

/* The formats allocated here: single-plane formats Vulkan images (YUV stays on the inner one). */
static const struct nvgr_fmt *nv_format(uint32_t drm_format)
{
	const struct nvgr_fmt *f = nvgr_fmt_from_drm(drm_format);

	return f && f->num_planes == 1 ? f : NULL;
}

static uint32_t nv_get_gbm_format(uint32_t drm_format)
{
	if (nv_format(drm_format))
		return drm_format;
	return g.inner->get_gbm_format(drm_format);
}

static struct gbm_device *nv_dev_create(int fd)
{
	struct nvdev *dev = calloc(1, sizeof(*dev));
	struct drm_nvidia_get_dev_info_params info = { 0 };

	if (!dev)
		return NULL;
	dev->fd = fd;
	dev->inner_fd = open(g.inner_device, O_RDWR | O_CLOEXEC);
	if (dev->inner_fd >= 0)
		dev->inner = g.inner->dev_create(dev->inner_fd);
	if (!dev->inner)
		LOGE("inner device %s: %s", g.inner_device, strerror(errno));

	/* nvgr_layout.h's modifiers are the RTX 40 series' (Ada) */
	if (ioctl_retry(fd, DRM_IOCTL_NVIDIA_GET_DEV_INFO, &info) || !info.supports_alloc ||
	    info.generic_page_kind != NVGR_PAGE_KIND ||
	    info.page_kind_generation != NVGR_KIND_GENERATION ||
	    info.sector_layout != NVGR_SECTOR_LAYOUT)
		LOGE("gralloc.gbm.device is not nvidia-drm with nvgr_layout.h's block-linear "
		     "parameters (alloc %u kind %#x gen %u sector %u): buffers will not match "
		     "vulkan.nvidia",
		     info.supports_alloc, info.generic_page_kind, info.page_kind_generation,
		     info.sector_layout);
	return (struct gbm_device *)dev;
}

static void nv_dev_destroy(struct gbm_device *gbm)
{
	struct nvdev *dev = (struct nvdev *)gbm;

	if (dev->inner)
		g.inner->dev_destroy(dev->inner);
	if (dev->inner_fd >= 0)
		close(dev->inner_fd);
	free(dev);
}

/* System memory: a sealed memfd turned into a dma-buf by udmabuf, flushed from the CPU caches. */
static int alloc_sysmem(size_t size)
{
	struct udmabuf_create create = { 0 };
	int memfd, fd;
	void *map;

	pthread_mutex_lock(&g.lock);
	if (g.udmabuf == -1) {
		g.udmabuf = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
		if (g.udmabuf < 0) {
			LOGE("cannot open /dev/udmabuf (%s)", strerror(errno));
			g.udmabuf = -2;
		}
	}
	pthread_mutex_unlock(&g.lock);
	if (g.udmabuf < 0)
		return -ENODEV;

	memfd = memfd_create("nvgbm", MFD_CLOEXEC | MFD_ALLOW_SEALING);
	if (memfd < 0)
		return -errno;
	if (ftruncate(memfd, size) || fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK)) {
		fd = -errno;
		goto out;
	}
	create.memfd = memfd;
	create.flags = UDMABUF_FLAGS_CLOEXEC;
	create.size = size;
	fd = ioctl(g.udmabuf, UDMABUF_CREATE, &create);
	if (fd < 0) {
		fd = -errno;
		LOGE("UDMABUF_CREATE of %zu bytes failed (%s)", size, strerror(-fd));
		goto out;
	}
	/*
	 * The kernel zeroed the new pages through the cache: write those lines back now, or they
	 * would land on top of what the GPU writes later.
	 */
	map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
	if (map != MAP_FAILED) {
		nvgr_clflush(map, size);
		munmap(map, size);
	}
out:
	close(memfd);
	return fd;
}

/* Video memory on nvidia-drm, block-linear, as a dma-buf. */
static int alloc_vidmem(int drm_fd, size_t size, bool scanout)
{
	struct drm_nvidia_gem_alloc_nvkms_memory_params alloc = { 0 };
	struct drm_prime_handle prime = { 0 };
	struct drm_gem_close gem_close = { 0 };
	int ret;

	alloc.block_linear = 1;
	alloc.memory_size = size;
	alloc.flags = scanout ? 0 : NV_GEM_ALLOC_NO_SCANOUT;
	if (ioctl_retry(drm_fd, DRM_IOCTL_NVIDIA_GEM_ALLOC_NVKMS_MEMORY, &alloc)) {
		ret = -errno;
		LOGE("GEM_ALLOC_NVKMS_MEMORY of %zu bytes failed (%s)", size, strerror(-ret));
		return ret;
	}
	prime.handle = alloc.handle;
	prime.flags = O_CLOEXEC | DRM_RDWR;
	ret = ioctl_retry(drm_fd, DRM_IOCTL_PRIME_HANDLE_TO_FD, &prime) ? -errno : prime.fd;
	gem_close.handle = alloc.handle; /* the dma-buf keeps the memory */
	ioctl_retry(drm_fd, DRM_IOCTL_GEM_CLOSE, &gem_close);
	if (ret < 0)
		LOGE("exporting a GEM buffer failed (%s)", strerror(-ret));
	return ret;
}

static int nv_alloc(struct alloc_args *args)
{
	struct nvdev *dev = (struct nvdev *)args->gbm;
	const struct nvgr_fmt *fmt = nv_format(args->drm_format);
	struct nvgr_layout layout;

	/* blobs (R8, one row before the backend made it 4096 wide) are linear for the inner one */
	if (!fmt || (args->drm_format == NVGR_DRM_R8 && args->force_linear)) {
		if (!dev->inner)
			return -ENODEV;
		args->gbm = dev->inner;
		int ret = g.inner->alloc(args);
		args->gbm = (struct gbm_device *)dev;
		return ret;
	}

	nvgr_compute_layout(fmt, args->width, args->height, &layout);
	const uint32_t pitch = layout.pitches[0];
	const uint32_t rows = (uint32_t)((layout.size + pitch - 1) / pitch);
	const size_t page = (size_t)getpagesize();
	const size_t size = ((size_t)pitch * rows + page - 1) / page * page;
	const int fd = args->needs_map_stride ? alloc_sysmem(size) :
						alloc_vidmem(dev->fd, size, args->use_scanout);
	if (fd < 0)
		return fd;

	args->out_fd = fd;
	args->out_stride = pitch;
	args->out_modifier = layout.modifier;
	args->out_map_stride = pitch;
	args->height = rows;
	return 0;
}

static struct gbm_bo *nv_import(struct gbm_device *gbm, int buf_fd, uint32_t width,
				uint32_t height, uint32_t stride, uint64_t modifier,
				uint32_t drm_format)
{
	struct nvdev *dev = (struct nvdev *)gbm;
	struct nvbo *bo = calloc(1, sizeof(*bo));

	if (!bo)
		return NULL;
	bo->fd = -1;
	bo->fmt = nv_format(drm_format);
	if (bo->fmt && nvgr_log2_block_h(modifier) >= 0) {
		nvgr_compute_layout(bo->fmt, width, height, &bo->layout);
		if (bo->layout.modifier != modifier || bo->layout.pitches[0] != stride) {
			LOGE("import of %.4s %ux%u: modifier %#llx stride %u are not nvgr's",
			     (const char *)&drm_format, width, height, (unsigned long long)modifier,
			     stride);
			free(bo);
			return NULL;
		}
		bo->fd = fcntl(buf_fd, F_DUPFD_CLOEXEC, 0);
		bo->width = width;
		bo->height = height;
		if (bo->fd < 0) {
			free(bo);
			return NULL;
		}
		return (struct gbm_bo *)bo;
	}
	if (dev->inner)
		bo->inner = g.inner->import(dev->inner, buf_fd, width, height, stride, modifier,
					    drm_format);
	if (!bo->inner) {
		free(bo);
		return NULL;
	}
	return (struct gbm_bo *)bo;
}

static void nv_free(struct gbm_bo *gbo)
{
	struct nvbo *bo = (struct nvbo *)gbo;

	if (bo->inner)
		g.inner->free(bo->inner);
	if (bo->fd >= 0)
		close(bo->fd);
	free(bo);
}

static void bl_copy(struct nvbo *bo, struct nvmap *m, bool to_shadow)
{
	const struct nvgr_layout *l = &bo->layout;

	nvgr_bl_copy(m->gpu, m->shadow, l->pitches[0], l->pitches[0], l->log2_block_h,
		     l->widths[0] * bo->fmt->cpp[0], 0, l->heights[0], to_shadow);
}

static void nv_map(struct gbm_bo *gbo, int w, int h, void **addr, void **map_data)
{
	struct nvbo *bo = (struct nvbo *)gbo;
	struct nvmap *m;

	if (bo->inner) {
		g.inner->map(bo->inner, w, h, addr, map_data);
		return;
	}
	*addr = MAP_FAILED;
	m = calloc(1, sizeof(*m));
	if (!m)
		return;
	m->gpu_length = bo->layout.size;
	m->gpu = mmap(NULL, m->gpu_length, PROT_READ | PROT_WRITE, MAP_SHARED, bo->fd, 0);
	m->shadow_length = (size_t)bo->layout.pitches[0] * bo->height;
	m->shadow = mmap(NULL, m->shadow_length, PROT_READ | PROT_WRITE,
			 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (m->gpu == MAP_FAILED || m->shadow == MAP_FAILED) {
		LOGE("mapping a block-linear buffer failed (%s)", strerror(errno));
		if (m->gpu != MAP_FAILED)
			munmap(m->gpu, m->gpu_length);
		if (m->shadow != MAP_FAILED)
			munmap(m->shadow, m->shadow_length);
		free(m);
		return;
	}
	/* The mapper does not say whether the lock reads: always detile, always tile back. */
	nvgr_clflush(m->gpu, m->gpu_length);
	bl_copy(bo, m, true);
	*addr = m->shadow;
	*map_data = m;
}

static void nv_unmap(struct gbm_bo *gbo, void *map_data)
{
	struct nvbo *bo = (struct nvbo *)gbo;
	struct nvmap *m = map_data;

	if (bo->inner) {
		g.inner->unmap(bo->inner, map_data);
		return;
	}
	bl_copy(bo, m, false);
	nvgr_clflush(m->gpu, m->gpu_length);
	munmap(m->gpu, m->gpu_length);
	munmap(m->shadow, m->shadow_length);
	free(m);
}

static void nv_init(void)
{
	char inner_name[PROP_VALUE_MAX] = "/vendor/lib64/libgbm_mesa_wrapper_intel.so";
	char value[PROP_VALUE_MAX];

	if (__system_property_get("waydroid.nvgbm.inner", value) > 0)
		snprintf(inner_name, sizeof(inner_name), "%s", value);
	snprintf(g.inner_device, sizeof(g.inner_device), "/dev/dri/renderD128");
	if (__system_property_get("waydroid.nvgbm.inner_device", value) > 0)
		snprintf(g.inner_device, sizeof(g.inner_device), "%s", value);

	/* by path: the renamed inner library has the same SONAME as this one */
	void *lib = dlopen(inner_name, RTLD_NOW | RTLD_LOCAL);
	struct gbm_ops *(*get_ops)(void) =
		lib ? (struct gbm_ops * (*)(void)) dlsym(lib, "get_gbm_ops") : NULL;
	g.inner = get_ops ? get_ops() : NULL;
	if (!g.inner) {
		LOGE("cannot load the inner wrapper %s: %s", inner_name, dlerror());
		return;
	}
	g.ops = (struct gbm_ops){
		.get_gbm_format = nv_get_gbm_format,
		.dev_create = nv_dev_create,
		.dev_destroy = nv_dev_destroy,
		.alloc = nv_alloc,
		.import = nv_import,
		.free = nv_free,
		.map = nv_map,
		.unmap = nv_unmap,
	};
	LOGI("NVIDIA block-linear buffers, others through %s on %s", inner_name, g.inner_device);
}

__attribute__((visibility("default"))) struct gbm_ops *get_gbm_ops(void)
{
	pthread_once(&g.once, nv_init);
	return g.inner ? &g.ops : NULL;
}
