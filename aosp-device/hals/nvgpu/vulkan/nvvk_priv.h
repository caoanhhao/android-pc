/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// vulkan.nvidia: Android's Vulkan HAL module on NVIDIA's own Vulkan driver (Phase L3).
//
// NVIDIA's ICD (libGLX_nvidia.so.0, glibc) is loaded in-process through nvglibc (Phase L2). Its
// dispatchable objects carry the loader magic 0x01CDC0DE that Android's loader also expects
// (HWVULKAN_DISPATCH_MAGIC), so every handle is NVIDIA's own and almost every entry point is
// NVIDIA's function itself. This module only adds what Android needs on top:
//   - VK_ANDROID_native_buffer (the platform swapchain: gralloc buffers as VkImages, acquire
//     and release with sync fds), and
//   - VK_ANDROID_external_memory_android_hardware_buffer,
// both built on NVIDIA's VK_EXT_external_memory_dma_buf + VK_EXT_image_drm_format_modifier +
// sync fd import/export, with the block-linear layout of nvgr_layout.h that the gralloc
// (minigbm "nvidia" backend) gives every buffer of an image format.

#pragma once

#include <stdint.h>
#include <stdio.h>

#include <mutex>
#include <unordered_map>
#include <vector>

#include <vulkan/vulkan.h>
#include <vulkan/vk_android_native_buffer.h>

#include <nvgr/nvgr_layout.h>

struct AHardwareBuffer;
struct native_handle;

#define NVVK_TAG "vulkan.nvidia"

namespace nvvk {

// ---- platform (nvvk_platform_android.cpp on the device, the host test's own on the host) ----

enum LogLevel { kLogError, kLogWarn, kLogInfo, kLogDebug };
void Log(LogLevel level, const char* fmt, ...) __attribute__((format(printf, 2, 3)));
#define NVVK_LOGE(...) ::nvvk::Log(::nvvk::kLogError, __VA_ARGS__)
#define NVVK_LOGW(...) ::nvvk::Log(::nvvk::kLogWarn, __VA_ARGS__)
#define NVVK_LOGI(...) ::nvvk::Log(::nvvk::kLogInfo, __VA_ARGS__)
#define NVVK_LOGD(...) ::nvvk::Log(::nvvk::kLogDebug, __VA_ARGS__)

// A system property, or def.
int GetPropertyInt(const char* name, int def);

// NVIDIA's ICD: open the library and look up its loader-interface entry points.
void* IcdOpen(const char* name, char* err, size_t err_size);
void* IcdSym(void* lib, const char* name);

// The AHardwareBuffer calls the HAL needs (libnativewindow on the device).
struct AhbDesc {
    uint32_t width, height, layers, format;  // AHARDWAREBUFFER_FORMAT_*
    uint64_t usage;                          // AHARDWAREBUFFER_USAGE_*
};
const native_handle* AhbHandle(const AHardwareBuffer* ahb);
void AhbDescribe(const AHardwareBuffer* ahb, AhbDesc* desc);
int AhbAllocate(const AhbDesc& desc, AHardwareBuffer** out);
void AhbAcquire(AHardwareBuffer* ahb);
void AhbRelease(AHardwareBuffer* ahb);

// ---- gralloc buffers (nvvk_buffer.cpp) ----

// A minigbm buffer as the HAL sees it.
struct GrallocBuffer {
    int fd;              // the dma-buf (owned by the handle, not by this struct)
    uint32_t drm_format;
    uint32_t width, height;
    uint64_t modifier;
    uint64_t size;       // of the dma-buf
    int32_t droid_format;
    uint64_t usage;
};
// false (and a log line saying why) when the handle is not a minigbm buffer.
bool ReadGrallocHandle(const native_handle* handle, GrallocBuffer* out);

// What a VkFormat looks like in memory, for nvgr_compute_layout(); null if no gralloc format
// has this layout.
const nvgr_fmt* FmtFromVk(VkFormat format);
// The VkFormat an AHardwareBuffer / gralloc buffer of a DRM format is sampled as;
// VK_FORMAT_UNDEFINED with a non-zero external format for NV21.
VkFormat VkFromDrm(uint32_t drm_format);
uint32_t AhbFormatFromVk(VkFormat format);

// ---- dispatch ----

struct InstanceFns {
#define NVVK_FN(name) PFN_vk##name name = nullptr;
#include "nvvk_instance_fns.inc"
#undef NVVK_FN
};

struct DeviceFns {
#define NVVK_FN(name) PFN_vk##name name = nullptr;
#include "nvvk_device_fns.inc"
#undef NVVK_FN
};

// What vkCreateImage decided for an image whose memory comes from gralloc.
struct ImageInfo {
    nvgr_layout layout;
    VkFormat format;        // the real format (also for VK_FORMAT_UNDEFINED + external format)
    VkExtent3D extent;
    VkImageUsageFlags usage;
    VkDeviceMemory owned_memory = VK_NULL_HANDLE;  // swapchain images: freed with the image
};

struct Device {
    VkDevice handle;
    VkPhysicalDevice physical;
    DeviceFns fn;
    VkPhysicalDeviceMemoryProperties memory_props;
    std::mutex lock;
    uint32_t image_memory_types = 0;  // for block-linear dma-buf images, probed once
    std::unordered_map<VkImage, ImageInfo> images;               // gralloc-backed images
    std::unordered_map<VkDeviceMemory, AHardwareBuffer*> ahbs;   // imported/exported AHBs
    std::unordered_map<VkQueue, VkSemaphore> release_semaphores; // per queue, sync fd export
};

extern InstanceFns g_inst;  // NVIDIA's physical-device and instance functions
Device* GetDevice(VkDevice device);
Device* GetDeviceForQueue(VkQueue queue);

// ---- the Android entry points (nvvk_wsi.cpp) ----

VKAPI_ATTR void VKAPI_CALL GetPhysicalDeviceProperties2(VkPhysicalDevice, VkPhysicalDeviceProperties2*);
VKAPI_ATTR VkResult VKAPI_CALL GetPhysicalDeviceImageFormatProperties2(
    VkPhysicalDevice, const VkPhysicalDeviceImageFormatInfo2*, VkImageFormatProperties2*);
VKAPI_ATTR void VKAPI_CALL GetPhysicalDeviceExternalBufferProperties(
    VkPhysicalDevice, const VkPhysicalDeviceExternalBufferInfo*, VkExternalBufferProperties*);

VKAPI_ATTR VkResult VKAPI_CALL CreateImage(VkDevice, const VkImageCreateInfo*,
                                           const VkAllocationCallbacks*, VkImage*);
VKAPI_ATTR void VKAPI_CALL DestroyImage(VkDevice, VkImage, const VkAllocationCallbacks*);
VKAPI_ATTR VkResult VKAPI_CALL BindImageMemory2(VkDevice, uint32_t, const VkBindImageMemoryInfo*);
VKAPI_ATTR VkResult VKAPI_CALL CreateImageView(VkDevice, const VkImageViewCreateInfo*,
                                               const VkAllocationCallbacks*, VkImageView*);
VKAPI_ATTR VkResult VKAPI_CALL CreateBuffer(VkDevice, const VkBufferCreateInfo*,
                                            const VkAllocationCallbacks*, VkBuffer*);
VKAPI_ATTR VkResult VKAPI_CALL AllocateMemory(VkDevice, const VkMemoryAllocateInfo*,
                                              const VkAllocationCallbacks*, VkDeviceMemory*);
VKAPI_ATTR void VKAPI_CALL FreeMemory(VkDevice, VkDeviceMemory, const VkAllocationCallbacks*);
VKAPI_ATTR VkResult VKAPI_CALL CreateSamplerYcbcrConversion(
    VkDevice, const VkSamplerYcbcrConversionCreateInfo*, const VkAllocationCallbacks*,
    VkSamplerYcbcrConversion*);
VKAPI_ATTR void VKAPI_CALL GetDeviceImageMemoryRequirements(VkDevice,
                                                            const VkDeviceImageMemoryRequirements*,
                                                            VkMemoryRequirements2*);
VKAPI_ATTR VkResult VKAPI_CALL GetAndroidHardwareBufferPropertiesANDROID(
    VkDevice, const AHardwareBuffer*, VkAndroidHardwareBufferPropertiesANDROID*);
VKAPI_ATTR VkResult VKAPI_CALL GetMemoryAndroidHardwareBufferANDROID(
    VkDevice, const VkMemoryGetAndroidHardwareBufferInfoANDROID*, AHardwareBuffer**);

VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsageANDROID(VkDevice, VkFormat,
                                                               VkImageUsageFlags, int*);
VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage2ANDROID(VkDevice, VkFormat,
                                                                VkImageUsageFlags,
                                                                VkSwapchainImageUsageFlagsANDROID,
                                                                uint64_t*, uint64_t*);
VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage3ANDROID(VkDevice,
                                                                const VkGrallocUsageInfoANDROID*,
                                                                uint64_t*);
VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage4ANDROID(VkDevice,
                                                                const VkGrallocUsageInfo2ANDROID*,
                                                                uint64_t*);
VKAPI_ATTR VkResult VKAPI_CALL AcquireImageANDROID(VkDevice, VkImage, int, VkSemaphore, VkFence);
VKAPI_ATTR VkResult VKAPI_CALL QueueSignalReleaseImageANDROID(VkQueue, uint32_t,
                                                              const VkSemaphore*, VkImage, int*);

}  // namespace nvvk
