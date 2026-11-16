/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// vulkan.nvidia: minigbm handles and the format tables.

#include <cros_gralloc/cros_gralloc_handle.h>

#include "nvvk_priv.h"

namespace nvvk {

namespace {

// cros_gralloc_helpers.h's cros_gralloc_magic
constexpr uint32_t kCrosGrallocMagic = 0xABCDDCBA;

// android/hardware_buffer.h (and HAL_PIXEL_FORMAT_BGRA_8888 = 5, which AHBs carry as well)
enum : uint32_t {
    kAhbR8G8B8A8 = 1,
    kAhbR8G8B8X8 = 2,
    kAhbR5G6B5 = 4,
    kAhbB8G8R8A8 = 5,
    kAhbR16G16B16A16F = 0x16,
    kAhbBlob = 0x21,
    kAhbY8Cb8Cr8_420 = 0x23,
    kAhbR10G10B10A2 = 0x2b,
    kAhbP010 = 0x36,
    kAhbR8 = 0x38,
    kAhbR16Uint = 0x39,
    kAhbR16G16Uint = 0x3a,
};

const nvgr_fmt kCpp1 = {0, 1, {1}, {1}, {1}, false};
const nvgr_fmt kCpp2 = {0, 1, {2}, {1}, {1}, false};
const nvgr_fmt kCpp4 = {0, 1, {4}, {1}, {1}, false};
const nvgr_fmt kCpp8 = {0, 1, {8}, {1}, {1}, false};

}  // namespace

bool ReadGrallocHandle(const native_handle* handle, GrallocBuffer* out) {
    if (!handle) {
        NVVK_LOGE("null gralloc handle");
        return false;
    }
    auto* h = reinterpret_cast<const cros_gralloc_handle*>(handle);
    const int ints = static_cast<int>((sizeof(cros_gralloc_handle) - sizeof(native_handle_t)) /
                                      sizeof(int)) - DRV_MAX_FDS;
    if (handle->numFds < 1 || handle->numInts < ints || h->magic != kCrosGrallocMagic) {
        NVVK_LOGE("not a minigbm buffer (fds %d ints %d magic %#x)", handle->numFds,
                  handle->numInts, handle->numInts >= ints ? h->magic : 0);
        return false;
    }
    out->fd = h->fds[0];  // all planes share one dma-buf (minigbm "nvidia" backend)
    out->drm_format = h->format;
    out->width = h->width;
    out->height = h->height;
    out->modifier = h->format_modifier;
    // total_size also counts the metadata region, which lives in its own fd
    // (cros_gralloc_driver.cc: total_size = reserved_region_size + the bo's size).
    out->size = h->total_size - h->reserved_region_size;
    out->droid_format = h->droid_format;
    out->usage = static_cast<uint64_t>(h->usage);
    return true;
}

const nvgr_fmt* FmtFromVk(VkFormat format) {
    switch (format) {
        case VK_FORMAT_R8_UNORM:
        case VK_FORMAT_R8_SNORM:
        case VK_FORMAT_R8_UINT:
        case VK_FORMAT_R8_SINT:
        case VK_FORMAT_R8_SRGB:
            return &kCpp1;
        case VK_FORMAT_R8G8_UNORM:
        case VK_FORMAT_R8G8_SNORM:
        case VK_FORMAT_R8G8_UINT:
        case VK_FORMAT_R8G8_SINT:
        case VK_FORMAT_R8G8_SRGB:
        case VK_FORMAT_R5G6B5_UNORM_PACK16:
        case VK_FORMAT_B5G6R5_UNORM_PACK16:
        case VK_FORMAT_R16_UNORM:
        case VK_FORMAT_R16_SNORM:
        case VK_FORMAT_R16_UINT:
        case VK_FORMAT_R16_SINT:
        case VK_FORMAT_R16_SFLOAT:
            return &kCpp2;
        case VK_FORMAT_R8G8B8A8_UNORM:
        case VK_FORMAT_R8G8B8A8_SNORM:
        case VK_FORMAT_R8G8B8A8_UINT:
        case VK_FORMAT_R8G8B8A8_SINT:
        case VK_FORMAT_R8G8B8A8_SRGB:
        case VK_FORMAT_B8G8R8A8_UNORM:
        case VK_FORMAT_B8G8R8A8_SRGB:
        case VK_FORMAT_A8B8G8R8_UNORM_PACK32:
        case VK_FORMAT_A8B8G8R8_SRGB_PACK32:
        case VK_FORMAT_A2B10G10R10_UNORM_PACK32:
        case VK_FORMAT_A2R10G10B10_UNORM_PACK32:
        case VK_FORMAT_R16G16_UNORM:
        case VK_FORMAT_R16G16_SNORM:
        case VK_FORMAT_R16G16_UINT:
        case VK_FORMAT_R16G16_SINT:
        case VK_FORMAT_R16G16_SFLOAT:
        case VK_FORMAT_R32_UINT:
        case VK_FORMAT_R32_SINT:
        case VK_FORMAT_R32_SFLOAT:
        case VK_FORMAT_B10G11R11_UFLOAT_PACK32:
            return &kCpp4;
        case VK_FORMAT_R16G16B16A16_UNORM:
        case VK_FORMAT_R16G16B16A16_SFLOAT:
        case VK_FORMAT_R16G16B16A16_UINT:
        case VK_FORMAT_R32G32_SFLOAT:
        case VK_FORMAT_R32G32_UINT:
            return &kCpp8;
        case VK_FORMAT_G8_B8R8_2PLANE_420_UNORM:
            return nvgr_fmt_from_drm(NVGR_DRM_NV12);
        case VK_FORMAT_G8_B8_R8_3PLANE_420_UNORM:
            return nvgr_fmt_from_drm(NVGR_DRM_YVU420);
        case VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16:
            return nvgr_fmt_from_drm(NVGR_DRM_P010);
        default:
            return nullptr;
    }
}

VkFormat VkFromDrm(uint32_t drm_format) {
    switch (drm_format) {
        case NVGR_DRM_ABGR8888:
        case NVGR_DRM_XBGR8888:
            return VK_FORMAT_R8G8B8A8_UNORM;
        case NVGR_DRM_ARGB8888:
        case NVGR_DRM_XRGB8888:
            return VK_FORMAT_B8G8R8A8_UNORM;
        case NVGR_DRM_RGB565:
            return VK_FORMAT_R5G6B5_UNORM_PACK16;
        case NVGR_DRM_ABGR2101010:
        case NVGR_DRM_XBGR2101010:
            return VK_FORMAT_A2B10G10R10_UNORM_PACK32;
        case NVGR_DRM_ABGR16161616F:
            return VK_FORMAT_R16G16B16A16_SFLOAT;
        case NVGR_DRM_R8:
            return VK_FORMAT_R8_UNORM;
        case NVGR_DRM_GR88:
            return VK_FORMAT_R8G8_UNORM;
        case NVGR_DRM_R16:
            return VK_FORMAT_R16_UINT;
        case NVGR_DRM_GR1616:
            return VK_FORMAT_R16G16_UINT;
        case NVGR_DRM_NV12:
            return VK_FORMAT_G8_B8R8_2PLANE_420_UNORM;
        case NVGR_DRM_YVU420:
        case NVGR_DRM_YVU420_ANDROID:
            return VK_FORMAT_G8_B8_R8_3PLANE_420_UNORM;
        case NVGR_DRM_P010:
            return VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16;
        default:  // NV21 (external format, swizzled) and everything linear
            return VK_FORMAT_UNDEFINED;
    }
}

uint32_t AhbFormatFromVk(VkFormat format) {
    switch (format) {
        case VK_FORMAT_R8G8B8A8_UNORM:
        case VK_FORMAT_R8G8B8A8_SRGB:
            return kAhbR8G8B8A8;
        case VK_FORMAT_B8G8R8A8_UNORM:
        case VK_FORMAT_B8G8R8A8_SRGB:
            return kAhbB8G8R8A8;
        case VK_FORMAT_R5G6B5_UNORM_PACK16:
            return kAhbR5G6B5;
        case VK_FORMAT_R16G16B16A16_SFLOAT:
            return kAhbR16G16B16A16F;
        case VK_FORMAT_A2B10G10R10_UNORM_PACK32:
            return kAhbR10G10B10A2;
        case VK_FORMAT_R8_UNORM:
            return kAhbR8;
        case VK_FORMAT_R16_UINT:
            return kAhbR16Uint;
        case VK_FORMAT_R16G16_UINT:
            return kAhbR16G16Uint;
        case VK_FORMAT_G8_B8R8_2PLANE_420_UNORM:
            return kAhbY8Cb8Cr8_420;
        case VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16:
            return kAhbP010;
        default:
            return 0;
    }
}

// R8G8B8X8 is used by callers that know their alpha is meaningless; same memory as RGBA.
static_assert(kAhbR8G8B8X8 == 2 && kAhbBlob == 0x21, "AHB format values");

}  // namespace nvvk
