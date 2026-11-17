/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

/*
 * ahb_vktest: gralloc buffers through vulkan.nvidia in Waydroid (docs/waydroid-nvidia.md).
 *
 * For a few sizes and usages, allocates an AHardwareBuffer (Waydroid's gralloc with
 * aosp-device/waydroid/gralloc's wrapper), imports it into Vulkan the way ANGLE and HWUI do
 * (vkGetAndroidHardwareBufferPropertiesANDROID, a VkImage with VkExternalMemoryImageCreateInfo,
 * VkImportAndroidHardwareBufferInfoANDROID, bind), clears it on the GPU, and for buffers the CPU
 * may read checks the pixels through AHardwareBuffer_lock, then makes an EGLImage of it through the
 * EGL driver (ANGLE on vulkan.nvidia), as SurfaceFlinger's RenderEngine does. Also prints the
 * device and the extension count. Exit status 0 when every case passes.
 *
 * Build: tools/waydroid/nvidia-build.sh (NDK, API 33); run as /data/local/tmp/ahb_vktest.
 */

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <android/hardware_buffer.h>
#define VK_USE_PLATFORM_ANDROID_KHR
#include <vulkan/vulkan.h>

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(call)                                                                                \
	do {                                                                                       \
		VkResult r_ = (call);                                                              \
		if (r_ != VK_SUCCESS) {                                                            \
			printf("  FAIL %s: %d\n", #call, r_);                                      \
			return false;                                                              \
		}                                                                                  \
	} while (0)

static VkInstance inst;
static VkPhysicalDevice phys;
static VkDevice dev;
static VkQueue queue;
static uint32_t queue_family;
static PFN_vkGetAndroidHardwareBufferPropertiesANDROID get_ahb_props;

static EGLDisplay egl_dpy = EGL_NO_DISPLAY;
static PFNEGLGETNATIVECLIENTBUFFERANDROIDPROC get_client_buffer;
static PFNEGLCREATEIMAGEKHRPROC create_image;
static PFNEGLDESTROYIMAGEKHRPROC destroy_image;

static bool setup_egl(void)
{
	egl_dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
	EGLint major, minor;
	if (!eglInitialize(egl_dpy, &major, &minor)) {
		printf("FAIL eglInitialize: %#x\n", eglGetError());
		return false;
	}
	printf("EGL %d.%d %s\n", major, minor, eglQueryString(egl_dpy, EGL_VENDOR));
	get_client_buffer = (PFNEGLGETNATIVECLIENTBUFFERANDROIDPROC)eglGetProcAddress(
		"eglGetNativeClientBufferANDROID");
	create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
	destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
	return get_client_buffer && create_image && destroy_image;
}

/* An EGLImage of the buffer, as RenderEngine makes one (EGL_IMAGE_PRESERVED_KHR). */
static bool egl_image(AHardwareBuffer *ahb)
{
	const EGLint attrs[] = { EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE };
	EGLImageKHR img = create_image(egl_dpy, EGL_NO_CONTEXT, EGL_NATIVE_BUFFER_ANDROID,
				       get_client_buffer(ahb), attrs);
	if (img == EGL_NO_IMAGE_KHR) {
		printf("  FAIL eglCreateImageKHR: %#x\n", eglGetError());
		return false;
	}
	destroy_image(egl_dpy, img);
	printf("  EGLImage ok\n");
	return true;
}

static bool setup(void)
{
	VkApplicationInfo app = { VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "ahb_vktest", 1, NULL,
				  0, VK_API_VERSION_1_1 };
	VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app };
	CHECK(vkCreateInstance(&ici, NULL, &inst));
	uint32_t n = 1;
	CHECK(vkEnumeratePhysicalDevices(inst, &n, &phys));
	VkPhysicalDeviceProperties props;
	vkGetPhysicalDeviceProperties(phys, &props);
	uint32_t next = 0;
	vkEnumerateDeviceExtensionProperties(phys, NULL, &next, NULL);
	printf("device %s (%04x:%04x), Vulkan %u.%u, %u device extensions\n", props.deviceName,
	       props.vendorID, props.deviceID, VK_VERSION_MAJOR(props.apiVersion),
	       VK_VERSION_MINOR(props.apiVersion), next);

	uint32_t nq = 0;
	vkGetPhysicalDeviceQueueFamilyProperties(phys, &nq, NULL);
	VkQueueFamilyProperties qf[16];
	nq = nq > 16 ? 16 : nq;
	vkGetPhysicalDeviceQueueFamilyProperties(phys, &nq, qf);
	for (queue_family = 0; queue_family < nq; queue_family++)
		if (qf[queue_family].queueFlags & VK_QUEUE_GRAPHICS_BIT)
			break;
	const char *exts[] = { "VK_ANDROID_external_memory_android_hardware_buffer",
			       "VK_KHR_sampler_ycbcr_conversion", "VK_EXT_queue_family_foreign" };
	float prio = 1;
	VkDeviceQueueCreateInfo qci = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0,
					queue_family, 1, &prio };
	VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, NULL, 0, 1, &qci, 0,
				   NULL, 3, exts };
	CHECK(vkCreateDevice(phys, &dci, NULL, &dev));
	vkGetDeviceQueue(dev, queue_family, 0, &queue);
	get_ahb_props = (PFN_vkGetAndroidHardwareBufferPropertiesANDROID)vkGetDeviceProcAddr(
		dev, "vkGetAndroidHardwareBufferPropertiesANDROID");
	return get_ahb_props != NULL;
}

/* Clears the image to `color` (RGBA8) on the GPU and waits. */
static bool clear(VkImage image, const VkClearColorValue *color)
{
	VkCommandPool pool;
	VkCommandPoolCreateInfo pci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, 0,
					queue_family };
	CHECK(vkCreateCommandPool(dev, &pci, NULL, &pool));
	VkCommandBuffer cb;
	VkCommandBufferAllocateInfo cai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL,
					    pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1 };
	CHECK(vkAllocateCommandBuffers(dev, &cai, &cb));
	VkCommandBufferBeginInfo bi = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL,
					VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
	vkBeginCommandBuffer(cb, &bi);
	VkImageSubresourceRange range = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
	VkImageMemoryBarrier to_dst = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, 0,
					VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED,
					VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
					VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED, image,
					range };
	vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
			     0, 0, NULL, 0, NULL, 1, &to_dst);
	vkCmdClearColorImage(cb, image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, color, 1, &range);
	/* hand the image to the "foreign" owner (gralloc/CPU), as a producer must */
	VkImageMemoryBarrier release = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL,
					 VK_ACCESS_TRANSFER_WRITE_BIT, 0,
					 VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
					 VK_IMAGE_LAYOUT_GENERAL, queue_family,
					 VK_QUEUE_FAMILY_FOREIGN_EXT, image, range };
	vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT,
			     VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 0, NULL, 1,
			     &release);
	vkEndCommandBuffer(cb);
	VkSubmitInfo si = { VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &cb };
	CHECK(vkQueueSubmit(queue, 1, &si, VK_NULL_HANDLE));
	CHECK(vkQueueWaitIdle(queue));
	vkDestroyCommandPool(dev, pool, NULL);
	return true;
}

static bool test(uint32_t w, uint32_t h, uint64_t usage, const char *what)
{
	printf("%ux%u RGBA8888 %s (usage %#llx)\n", w, h, what, (unsigned long long)usage);
	AHardwareBuffer_Desc desc = { w, h, 1, AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM, usage };
	AHardwareBuffer *ahb = NULL;
	if (AHardwareBuffer_allocate(&desc, &ahb)) {
		printf("  FAIL AHardwareBuffer_allocate\n");
		return false;
	}

	VkAndroidHardwareBufferFormatPropertiesANDROID fmt = {
		VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_ANDROID
	};
	VkAndroidHardwareBufferPropertiesANDROID props = {
		VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID, &fmt
	};
	CHECK(get_ahb_props(dev, ahb, &props));
	printf("  size %llu, memory types %#x, format %d, features %#x\n",
	       (unsigned long long)props.allocationSize, props.memoryTypeBits, fmt.format,
	       fmt.formatFeatures);
	if (fmt.format != VK_FORMAT_R8G8B8A8_UNORM ||
	    !(fmt.formatFeatures & VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT)) {
		printf("  FAIL not an RGBA8 color attachment\n");
		return false;
	}

	VkExternalMemoryImageCreateInfo ext = {
		VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO, NULL,
		VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID
	};
	VkImageCreateInfo ici = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
				  &ext,
				  0,
				  VK_IMAGE_TYPE_2D,
				  VK_FORMAT_R8G8B8A8_UNORM,
				  { w, h, 1 },
				  1,
				  1,
				  VK_SAMPLE_COUNT_1_BIT,
				  VK_IMAGE_TILING_OPTIMAL,
				  VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT |
					  VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
				  VK_SHARING_MODE_EXCLUSIVE,
				  0,
				  NULL,
				  VK_IMAGE_LAYOUT_UNDEFINED };
	VkImage image;
	CHECK(vkCreateImage(dev, &ici, NULL, &image));
	VkImportAndroidHardwareBufferInfoANDROID import = {
		VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, NULL, ahb
	};
	VkMemoryDedicatedAllocateInfo dedicated = {
		VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO, &import, image, VK_NULL_HANDLE
	};
	uint32_t type = 0;
	while (type < 32 && !(props.memoryTypeBits & (1u << type)))
		type++;
	VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &dedicated,
				     props.allocationSize, type };
	VkDeviceMemory mem;
	CHECK(vkAllocateMemory(dev, &mai, NULL, &mem));
	CHECK(vkBindImageMemory(dev, image, mem, 0));

	const VkClearColorValue color = { .float32 = { 1.0f, 0.5f, 0.0f, 1.0f } };
	if (!clear(image, &color))
		return false;

	bool ok = egl_image(ahb);
	if (usage & AHARDWAREBUFFER_USAGE_CPU_READ_MASK) {
		void *p = NULL;
		AHardwareBuffer_Desc d;
		AHardwareBuffer_describe(ahb, &d);
		if (AHardwareBuffer_lock(ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, NULL, &p)) {
			printf("  FAIL AHardwareBuffer_lock\n");
			ok = false;
		} else {
			uint32_t bad = 0;
			for (uint32_t y = 0; y < h; y++)
				for (uint32_t x = 0; x < w; x++) {
					const uint8_t *px = (const uint8_t *)p + ((size_t)y * d.stride + x) * 4;
					bad += px[0] != 255 || px[1] < 127 || px[1] > 128 ||
					       px[2] != 0 || px[3] != 255;
				}
			AHardwareBuffer_unlock(ahb, NULL);
			printf("  CPU read back: %u of %u pixels wrong\n", bad, w * h);
			ok = ok && bad == 0;
		}
	}
	vkDestroyImage(dev, image, NULL);
	vkFreeMemory(dev, mem, NULL);
	AHardwareBuffer_release(ahb);
	printf("  %s\n", ok ? "ok" : "FAIL");
	return ok;
}

int main(void)
{
	if (!setup() || !setup_egl()) {
		printf("FAIL setup\n");
		return 1;
	}
	const uint64_t gpu = AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
			     AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT;
	bool ok = true;
	ok &= test(144, 27, gpu, "GPU only");
	ok &= test(144, 27, gpu | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, "GPU + CPU read");
	ok &= test(2560, 1080, gpu | AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY, "GPU + composer");
	ok &= test(144, 27, AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT |
				    AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, "screenshot (no sampling)");
	ok &= test(333, 77, gpu | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN |
				   AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, "GPU + CPU read/write");
	printf("%s\n", ok ? "PASS" : "FAIL");
	return ok ? 0 : 1;
}
