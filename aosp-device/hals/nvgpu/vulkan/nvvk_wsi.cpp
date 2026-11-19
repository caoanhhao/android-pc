/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// vulkan.nvidia: VK_ANDROID_native_buffer and VK_ANDROID_external_memory_android_hardware_buffer
// on NVIDIA's dma-buf, DRM format modifier and sync fd support.
//
// A VkImage for gralloc memory (a swapchain image or an image with the AHB handle type) is
// created with VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT and the explicit block-linear layout of
// nvgr_layout.h, computed from its format and size alone; the buffer bound to it later must
// have exactly that layout (minigbm's "nvidia" backend allocates every image-format buffer so),
// which the import checks.

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <memory>

#include "nvvk_priv.h"

namespace nvvk {

namespace {

// gralloc / AHardwareBuffer usage bits (hardware/gralloc.h, android/hardware_buffer.h)
constexpr uint64_t kGrallocHwTexture = 0x100;
constexpr uint64_t kGrallocHwRender = 0x200;
constexpr uint64_t kAhbGpuSampledImage = 1ull << 8;
constexpr uint64_t kAhbGpuFramebuffer = 1ull << 9;
constexpr uint64_t kAhbProtected = 1ull << 14;
constexpr uint64_t kAhbGpuDataBuffer = 1ull << 24;
constexpr uint64_t kAhbGpuCubeMap = 1ull << 25;
constexpr uint32_t kAhbFormatBlob = 0x21;

constexpr VkExternalMemoryHandleTypeFlagBits kAhb =
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID;
constexpr VkExternalMemoryHandleTypeFlagBits kDmaBuf =
    VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

const void* Find(const void* chain, VkStructureType type) {
    for (auto* s = static_cast<const VkBaseInStructure*>(chain); s; s = s->pNext)
        if (s->sType == type)
            return s;
    return nullptr;
}

void* FindOut(void* chain, VkStructureType type) {
    for (auto* s = static_cast<VkBaseOutStructure*>(chain); s; s = s->pNext)
        if (s->sType == type)
            return s;
    return nullptr;
}

// Removes one struct from an output chain for a call into NVIDIA and puts it back afterwards.
class Unlink {
  public:
    Unlink(void* head_holder_struct, VkStructureType type) {
        auto* prev = static_cast<VkBaseOutStructure*>(head_holder_struct);
        for (auto* s = prev->pNext; s; prev = s, s = s->pNext) {
            if (s->sType == type) {
                prev_ = prev;
                removed_ = s;
                prev->pNext = s->pNext;
                break;
            }
        }
    }
    ~Unlink() {
        if (prev_)
            prev_->pNext = removed_;
    }
    void* removed() const { return removed_; }

  private:
    VkBaseOutStructure* prev_ = nullptr;
    VkBaseOutStructure* removed_ = nullptr;
};

// The size of the input structs that may share a chain with the Android ones and that NVIDIA
// should still see; others are dropped (with a warning) when a chain has to be rebuilt.
size_t KnownSize(VkStructureType type) {
    switch (type) {
        case VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO:
            return sizeof(VkImageFormatListCreateInfo);
        case VK_STRUCTURE_TYPE_IMAGE_STENCIL_USAGE_CREATE_INFO:
            return sizeof(VkImageStencilUsageCreateInfo);
        case VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO:
            return sizeof(VkMemoryDedicatedAllocateInfo);
        case VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO:
            return sizeof(VkMemoryAllocateFlagsInfo);
        case VK_STRUCTURE_TYPE_MEMORY_PRIORITY_ALLOCATE_INFO_EXT:
            return sizeof(VkMemoryPriorityAllocateInfoEXT);
        case VK_STRUCTURE_TYPE_MEMORY_OPAQUE_CAPTURE_ADDRESS_ALLOCATE_INFO:
            return sizeof(VkMemoryOpaqueCaptureAddressAllocateInfo);
        case VK_STRUCTURE_TYPE_BUFFER_USAGE_FLAGS_2_CREATE_INFO:
            return sizeof(VkBufferUsageFlags2CreateInfo);
        case VK_STRUCTURE_TYPE_BUFFER_OPAQUE_CAPTURE_ADDRESS_CREATE_INFO:
            return sizeof(VkBufferOpaqueCaptureAddressCreateInfo);
        case VK_STRUCTURE_TYPE_BUFFER_DEVICE_ADDRESS_CREATE_INFO_EXT:
            return sizeof(VkBufferDeviceAddressCreateInfoEXT);
        case VK_STRUCTURE_TYPE_SAMPLER_YCBCR_CONVERSION_INFO:
            return sizeof(VkSamplerYcbcrConversionInfo);
        case VK_STRUCTURE_TYPE_IMAGE_VIEW_USAGE_CREATE_INFO:
            return sizeof(VkImageViewUsageCreateInfo);
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_VIEW_IMAGE_FORMAT_INFO_EXT:
            return sizeof(VkPhysicalDeviceImageViewImageFormatInfoEXT);
        default:
            return 0;
    }
}

// A copy of an input pNext chain with some structs left out and some added in front.
class Chain {
  public:
    // Copies the known structs of chain except those of the types in skip.
    Chain(const void* chain, std::initializer_list<VkStructureType> skip) {
        for (auto* s = static_cast<const VkBaseInStructure*>(chain); s; s = s->pNext) {
            bool skipped = false;
            for (VkStructureType t : skip)
                skipped = skipped || s->sType == t;
            if (skipped)
                continue;
            const size_t size = KnownSize(s->sType);
            if (!size) {
                NVVK_LOGW("dropping unexpected struct %d from an Android chain", s->sType);
                continue;
            }
            Add(s, size);
        }
    }
    void Add(const void* s, size_t size) {
        auto copy = std::make_unique<uint8_t[]>(size);
        memcpy(copy.get(), s, size);
        auto* base = reinterpret_cast<VkBaseOutStructure*>(copy.get());
        base->pNext = static_cast<VkBaseOutStructure*>(head_);
        head_ = base;
        storage_.push_back(std::move(copy));
    }
    template <typename T>
    void Add(const T& s) {
        Add(&s, sizeof(T));
    }
    const void* head() const { return head_; }

  private:
    void* head_ = nullptr;
    std::vector<std::unique_ptr<uint8_t[]>> storage_;
};

// The image format a gralloc buffer is sampled as, also for an external format.
VkFormat ResolveFormat(VkFormat format, const void* chain) {
    if (format != VK_FORMAT_UNDEFINED)
        return format;
    auto* ext = static_cast<const VkExternalFormatANDROID*>(
        Find(chain, VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID));
    if (!ext || !ext->externalFormat)
        return VK_FORMAT_UNDEFINED;
    // external formats are DRM fourccs (GetAndroidHardwareBufferPropertiesANDROID)
    if (ext->externalFormat == NVGR_DRM_NV21)
        return VK_FORMAT_G8_B8R8_2PLANE_420_UNORM;
    return VkFromDrm(static_cast<uint32_t>(ext->externalFormat));
}

// Swapchain images (VkNativeBufferANDROID from the loader, or an app's image for a swapchain that
// the loader binds with one later) and images for AHB memory.
bool IsGrallocImage(const VkImageCreateInfo* info) {
    if (Find(info->pNext, VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID) ||
        Find(info->pNext, VK_STRUCTURE_TYPE_IMAGE_SWAPCHAIN_CREATE_INFO_KHR))
        return true;
    auto* ext = static_cast<const VkExternalMemoryImageCreateInfo*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO));
    return ext && (ext->handleTypes & kAhb);
}

// NVIDIA's create info for a gralloc image (see the file comment).
struct GrallocImageCreate {
    VkImageCreateInfo info;
    ImageInfo image;
    VkSubresourceLayout planes[NVGR_MAX_PLANES];
    VkImageDrmFormatModifierExplicitCreateInfoEXT modifier;
    VkExternalMemoryImageCreateInfo external;
    std::unique_ptr<Chain> chain;

    VkResult Init(const VkImageCreateInfo* in) {
        const VkFormat format = ResolveFormat(in->format, in->pNext);
        const nvgr_fmt* fmt = FmtFromVk(format);
        if (!fmt || in->imageType != VK_IMAGE_TYPE_2D || in->mipLevels != 1 ||
            in->arrayLayers != 1 || in->samples != VK_SAMPLE_COUNT_1_BIT) {
            NVVK_LOGE("gralloc image not supported: format %d type %d %ux%u mips %u layers %u "
                      "samples %d",
                      format, in->imageType, in->extent.width, in->extent.height, in->mipLevels,
                      in->arrayLayers, in->samples);
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
        }
        nvgr_compute_layout(fmt, in->extent.width, in->extent.height, &image.layout);
        image.format = format;
        image.extent = in->extent;
        image.usage = in->usage;

        for (uint32_t p = 0; p < image.layout.num_planes; p++)
            planes[p] = {image.layout.offsets[p], 0, image.layout.pitches[p], 0, 0};
        modifier = {VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT, nullptr,
                    image.layout.modifier, image.layout.num_planes, planes};
        external = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO, nullptr,
                    static_cast<VkExternalMemoryHandleTypeFlags>(kDmaBuf)};
        auto* ext = static_cast<const VkExternalMemoryImageCreateInfo*>(
            Find(in->pNext, VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO));
        if (ext)
            external.handleTypes |= ext->handleTypes & ~static_cast<VkFlags>(kAhb);

        chain = std::make_unique<Chain>(
            in->pNext, std::initializer_list<VkStructureType>{
                           VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID,
                           VK_STRUCTURE_TYPE_SWAPCHAIN_IMAGE_CREATE_INFO_ANDROID,
                           VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID,
                           VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
                           VK_STRUCTURE_TYPE_IMAGE_SWAPCHAIN_CREATE_INFO_KHR,
                           VK_STRUCTURE_TYPE_IMAGE_COMPRESSION_CONTROL_EXT});
        chain->Add(modifier);
        chain->Add(external);
        info = *in;
        info.pNext = chain->head();
        info.format = format;
        info.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
        // Android's YUV buffers are sampled through an external-format conversion; the planes
        // are never viewed on their own.
        info.flags &= ~VK_IMAGE_CREATE_DISJOINT_BIT;
        return VK_SUCCESS;
    }
};

// A memory type for an import of fd into image (or a buffer when image is null).
int ChooseMemoryType(Device* dev, uint32_t bits) {
    if (!bits)
        return -1;
    for (uint32_t i = 0; i < dev->memory_props.memoryTypeCount; i++)
        if ((bits >> i & 1) &&
            (dev->memory_props.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
            return static_cast<int>(i);
    return __builtin_ctz(bits);
}

uint32_t DeviceLocalTypes(Device* dev) {
    uint32_t bits = 0;
    for (uint32_t i = 0; i < dev->memory_props.memoryTypeCount; i++)
        if (dev->memory_props.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)
            bits |= 1u << i;
    return bits;
}

bool HasDeviceLocalType(Device* dev, uint32_t bits) {
    return (bits & DeviceLocalTypes(dev)) != 0;
}

uint32_t FdMemoryTypes(Device* dev, int fd) {
    VkMemoryFdPropertiesKHR props = {VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR};
    if (dev->fn.GetMemoryFdPropertiesKHR(dev->handle, kDmaBuf, fd, &props) != VK_SUCCESS)
        return 0;
    return props.memoryTypeBits;
}

// The memory types NVIDIA allows for block-linear images imported from a dma-buf.
uint32_t ImageMemoryTypes(Device* dev) {
    static const nvgr_fmt kRgba = {0, 1, {4}, {1}, {1}, false};
    std::lock_guard<std::mutex> guard(dev->lock);
    if (dev->image_memory_types)
        return dev->image_memory_types;
    VkImageCreateInfo probe = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    probe.imageType = VK_IMAGE_TYPE_2D;
    probe.format = VK_FORMAT_R8G8B8A8_UNORM;
    probe.extent = {64, 64, 1};
    probe.mipLevels = probe.arrayLayers = 1;
    probe.samples = VK_SAMPLE_COUNT_1_BIT;
    probe.usage = VK_IMAGE_USAGE_SAMPLED_BIT;
    nvgr_layout l;
    nvgr_compute_layout(&kRgba, 64, 64, &l);
    VkSubresourceLayout plane = {0, 0, l.pitches[0], 0, 0};
    VkImageDrmFormatModifierExplicitCreateInfoEXT mod = {
        VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT, nullptr, l.modifier,
        1, &plane};
    VkExternalMemoryImageCreateInfo ext = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
                                           &mod, static_cast<VkFlags>(kDmaBuf)};
    probe.pNext = &ext;
    probe.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
    VkImage image;
    if (dev->fn.CreateImage(dev->handle, &probe, nullptr, &image) != VK_SUCCESS) {
        NVVK_LOGE("cannot create a block-linear probe image");
        return 0;
    }
    VkMemoryRequirements req;
    dev->fn.GetImageMemoryRequirements(dev->handle, image, &req);
    dev->fn.DestroyImage(dev->handle, image, nullptr);
    dev->image_memory_types = req.memoryTypeBits;
    return req.memoryTypeBits;
}

// Checks that a gralloc buffer has the layout the image was created with.
bool CheckBufferLayout(const GrallocBuffer& b, const ImageInfo& image) {
    const nvgr_fmt* fmt = nvgr_fmt_from_drm(b.drm_format);
    nvgr_layout l;
    if (fmt)
        nvgr_compute_layout(fmt, b.width, b.height, &l);
    const nvgr_layout& want = image.layout;
    bool ok = fmt && b.modifier == want.modifier && l.num_planes == want.num_planes &&
              b.size >= want.size && b.width == image.extent.width &&
              b.height == image.extent.height;
    for (uint32_t p = 0; ok && p < want.num_planes; p++)
        ok = l.offsets[p] == want.offsets[p] && l.pitches[p] == want.pitches[p];
    if (!ok)
        NVVK_LOGE("gralloc buffer %.4s %ux%u modifier %#llx size %llu does not match image "
                  "format %d %ux%u (modifier %#llx size %llu); is the gralloc minigbm's "
                  "\"nvidia\" backend?",
                  reinterpret_cast<const char*>(&b.drm_format), b.width, b.height,
                  static_cast<unsigned long long>(b.modifier),
                  static_cast<unsigned long long>(b.size), image.format, image.extent.width,
                  image.extent.height, static_cast<unsigned long long>(want.modifier),
                  static_cast<unsigned long long>(want.size));
    return ok;
}

// Imports the dma-buf of b as dedicated memory for image (or buffer).
VkResult ImportDmaBuf(Device* dev, const GrallocBuffer& b, VkImage image, VkBuffer buffer,
                      int memory_type, const void* extra_chain, VkDeviceMemory* out) {
    const uint32_t fd_types = FdMemoryTypes(dev, b.fd);
    // A type GetAndroidHardwareBufferPropertiesANDROID added for ANGLE (below) is not one NVIDIA
    // imports this dma-buf with: choose one that is.
    if (memory_type >= 0 && !(fd_types & (1u << memory_type)))
        memory_type = -1;
    if (memory_type < 0) {
        uint32_t bits = fd_types;
        if (image) {
            VkMemoryRequirements req;
            dev->fn.GetImageMemoryRequirements(dev->handle, image, &req);
            bits &= req.memoryTypeBits;
        }
        memory_type = ChooseMemoryType(dev, bits);
        if (memory_type < 0) {
            NVVK_LOGE("no memory type for dma-buf (fd types %#x)", fd_types);
            return VK_ERROR_INVALID_EXTERNAL_HANDLE;
        }
    }
    const int fd = dup(b.fd);
    if (fd < 0)
        return VK_ERROR_TOO_MANY_OBJECTS;
    Chain chain(extra_chain, {VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID,
                              VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
                              VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO});
    VkMemoryDedicatedAllocateInfo dedicated = {VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
                                               nullptr, image, buffer};
    VkImportMemoryFdInfoKHR import = {VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR, nullptr,
                                      kDmaBuf, fd};
    if (image || buffer)
        chain.Add(dedicated);
    chain.Add(import);
    VkMemoryAllocateInfo info = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, chain.head(), b.size,
                                 static_cast<uint32_t>(memory_type)};
    VkResult result = dev->fn.AllocateMemory(dev->handle, &info, nullptr, out);
    if (result != VK_SUCCESS) {
        close(fd);  // ownership moves to the driver only on success
        NVVK_LOGE("dma-buf import failed: %d (size %llu, type %d, fd types %#x)", result,
                  static_cast<unsigned long long>(b.size), memory_type, fd_types);
    }
    return result;
}

// Binds a swapchain buffer to its image; the memory is freed with the image.
VkResult BindNativeBuffer(Device* dev, VkImage image, const VkNativeBufferANDROID* nb) {
    GrallocBuffer b;
    if (!ReadGrallocHandle(nb->handle, &b))
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    ImageInfo info;
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        auto it = dev->images.find(image);
        if (it == dev->images.end()) {
            NVVK_LOGE("native buffer for an image this HAL did not create");
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        info = it->second;
    }
    if (!CheckBufferLayout(b, info))
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    VkDeviceMemory memory;
    VkResult result = ImportDmaBuf(dev, b, image, VK_NULL_HANDLE, -1, nullptr, &memory);
    if (result != VK_SUCCESS)
        return result;
    VkBindImageMemoryInfo bind = {VK_STRUCTURE_TYPE_BIND_IMAGE_MEMORY_INFO, nullptr, image, memory,
                                  0};
    result = dev->fn.BindImageMemory2(dev->handle, 1, &bind);
    if (result != VK_SUCCESS) {
        NVVK_LOGE("binding a swapchain buffer failed: %d", result);
        dev->fn.FreeMemory(dev->handle, memory, nullptr);
        return result;
    }
    std::lock_guard<std::mutex> guard(dev->lock);
    dev->images[image].owned_memory = memory;
    return VK_SUCCESS;
}

uint64_t AhbUsageFor(VkImageUsageFlags usage, VkImageCreateFlags flags) {
    uint64_t ahb = 0;
    if (usage & (VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT |
                 VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_STORAGE_BIT))
        ahb |= kAhbGpuSampledImage;
    if (usage & (VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT))
        ahb |= kAhbGpuFramebuffer;
    if (flags & VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT)
        ahb |= kAhbGpuCubeMap;
    if (flags & VK_IMAGE_CREATE_PROTECTED_BIT)
        ahb |= kAhbProtected;
    return ahb ? ahb : kAhbGpuSampledImage;
}

}  // namespace

// ---- physical device ----

// The Vulkan version the platform's loader knows: NVIDIA reports 1.4, which Android loaders before
// Android 16 (API 36) do not, and Android 13's vkjson (`cmd gpu vkjson`) then prints an empty
// device (Waydroid, docs/waydroid-nvidia.md).
static uint32_t ClampApiVersion(uint32_t version) {
    static const uint32_t max = GetPropertyInt("ro.build.version.sdk", 36) < 36
                                        ? VK_API_VERSION_1_3
                                        : ~0u;
    if (VK_API_VERSION_MAJOR(version) * 1000 + VK_API_VERSION_MINOR(version) <=
        VK_API_VERSION_MAJOR(max) * 1000 + VK_API_VERSION_MINOR(max))
        return version;
    return max | VK_API_VERSION_PATCH(version);
}

// Mobile games rank the device by the GPU they see and pick their quality presets and resource
// packs from that rank. Where Winds Meet does not know the RTX 4070: it ranks it "middle" and
// caps its best preset at 70% render scale, SMAA and 16-bit terrain textures (docs/waydroid-
// nvidia.md, "Where Winds Meet's quality"). So apps can see a Qualcomm Adreno instead: vendor,
// device, name, type and driver. Limits, features, extensions and formats stay NVIDIA's.
//
// - Off by default: with every app on the Adreno, WWM never reached its first textures and
//   TikTok's login hung on the PC (build 20261119.1409). persist.vendor.nvvk.adreno=1 turns it on.
// - When on, every app process (uid >= 10000 in its user) sees it; system processes
//   (SurfaceFlinger, system_server, gpuservice ...) see NVIDIA.
// - persist.vendor.nvvk.adreno_exclude: comma-separated process names that see NVIDIA.
// - persist.vendor.nvvk.adreno_apps: comma-separated process names that see the Adreno even
//   when they are not apps.
// - persist.vendor.nvvk.adreno_model: the model, "850" (the default, Snapdragon 8 Elite Gen 6
//   Pro, the fastest Android GPU), "740", "750", "830", "840" or another number, optionally
//   with the chip ID after a colon ("850:0x44070041").
struct Adreno {
    unsigned model;      // 0: the process sees NVIDIA
    uint32_t device_id;  // Qualcomm's chip ID
    uint32_t driver;     // the driver's build number (V@0744 ...)
};

// Chip IDs from Mesa's freedreno_devices.py. The Adreno 850 is not there yet: models it does
// not know take the newest one's (the 840's) unless the property gives one.
static uint32_t AdrenoChipId(unsigned model) {
    switch (model) {
        case 740: return 0x43050a01;
        case 750: return 0x43051401;
        case 830: return 0x44050001;
        default:  return 0x44050a31;  // Adreno 840
    }
}

static bool Listed(const char* property, const std::string& name) {
    return ("," + GetProperty(property) + ",").find("," + name + ",") != std::string::npos;
}

static const Adreno& ReportsAdreno() {
    static int pid = 0;
    static Adreno adreno;
    if (pid == getpid())
        return adreno;
    // Keyed on the pid: a process forked after the first call (zygote) reads it again.
    pid = getpid();
    adreno = {};
    if (GetPropertyInt("persist.vendor.nvvk.adreno", 0) == 0)
        return adreno;
    char cmdline[256] = "";
    if (FILE* f = fopen("/proc/self/cmdline", "re")) {
        const size_t n = fread(cmdline, 1, sizeof(cmdline) - 1, f);
        cmdline[n] = '\0';
        fclose(f);
    }
    const std::string name = cmdline;
    const bool app = getuid() % 100000 >= 10000;  // AID_USER_OFFSET, AID_APP_START
    if (name.empty() || Listed("persist.vendor.nvvk.adreno_exclude", name) ||
        (!app && !Listed("persist.vendor.nvvk.adreno_apps", name)))
        return adreno;
    const std::string spec = GetProperty("persist.vendor.nvvk.adreno_model");
    char* end = nullptr;
    adreno.model = static_cast<unsigned>(strtoul(spec.c_str(), &end, 10));
    if (adreno.model < 100 || adreno.model > 999)
        adreno.model = 850;
    adreno.device_id = end && *end == ':' ? static_cast<uint32_t>(strtoul(end + 1, nullptr, 0))
                                           : AdrenoChipId(adreno.model);
    adreno.driver = adreno.model >= 800 ? 800 : 744;
    NVVK_LOGI("%s sees the GPU as an Adreno %u (%#x)", cmdline, adreno.model, adreno.device_id);
    return adreno;
}

static void SetDriverIdentity(const Adreno& a, VkDriverId* id, char* name, char* info) {
    *id = VK_DRIVER_ID_QUALCOMM_PROPRIETARY;
    snprintf(name, VK_MAX_DRIVER_NAME_SIZE, "Qualcomm Technologies Inc. Adreno Vulkan Driver");
    snprintf(info, VK_MAX_DRIVER_INFO_SIZE, "Driver Build: %u.0", a.driver);
}

static void SetAdrenoIdentity(const Adreno& a, VkPhysicalDeviceProperties* p) {
    p->vendorID = 0x5143;  // Qualcomm
    p->deviceID = a.device_id;
    p->deviceType = VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU;
    p->driverVersion = VK_MAKE_VERSION(512, a.driver, 0);
    snprintf(p->deviceName, sizeof(p->deviceName), "Adreno (TM) %u", a.model);
}

VKAPI_ATTR void VKAPI_CALL GetPhysicalDeviceProperties(VkPhysicalDevice physical,
                                                       VkPhysicalDeviceProperties* props) {
    g_inst.GetPhysicalDeviceProperties(physical, props);
    props->apiVersion = ClampApiVersion(props->apiVersion);
    if (const Adreno& a = ReportsAdreno(); a.model)
        SetAdrenoIdentity(a, props);
}

VKAPI_ATTR void VKAPI_CALL GetPhysicalDeviceProperties2(VkPhysicalDevice physical,
                                                        VkPhysicalDeviceProperties2* props) {
    Unlink presentation(props, VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENTATION_PROPERTIES_ANDROID);
    g_inst.GetPhysicalDeviceProperties2(physical, props);
    props->properties.apiVersion = ClampApiVersion(props->properties.apiVersion);
    if (auto* p = static_cast<VkPhysicalDevicePresentationPropertiesANDROID*>(presentation.removed()))
        p->sharedImage = VK_FALSE;  // no front-buffer rendering
    if (const Adreno& a = ReportsAdreno(); a.model) {
        SetAdrenoIdentity(a, &props->properties);
        // Every struct of the chain: vkjson chains the driver properties twice.
        for (auto* s = static_cast<VkBaseOutStructure*>(props->pNext); s; s = s->pNext) {
            if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES) {
                auto* d = reinterpret_cast<VkPhysicalDeviceDriverProperties*>(s);
                SetDriverIdentity(a, &d->driverID, d->driverName, d->driverInfo);
            } else if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_PROPERTIES) {
                auto* v = reinterpret_cast<VkPhysicalDeviceVulkan12Properties*>(s);
                SetDriverIdentity(a, &v->driverID, v->driverName, v->driverInfo);
            }
        }
    }
}

VKAPI_ATTR VkResult VKAPI_CALL GetPhysicalDeviceImageFormatProperties2(
    VkPhysicalDevice physical, const VkPhysicalDeviceImageFormatInfo2* info,
    VkImageFormatProperties2* props) {
    auto* ext_info = static_cast<const VkPhysicalDeviceExternalImageFormatInfo*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO));
    const bool ahb = ext_info && ext_info->handleType == kAhb;
    Unlink usage(props, VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_USAGE_ANDROID);
    if (auto* u = static_cast<VkAndroidHardwareBufferUsageANDROID*>(usage.removed()))
        u->androidHardwareBufferUsage = AhbUsageFor(info->usage, info->flags);
    if (!ahb)
        return g_inst.GetPhysicalDeviceImageFormatProperties2(physical, info, props);

    // What NVIDIA allows for the block-linear layout gralloc gives such an image.
    if (!FmtFromVk(info->format) || info->type != VK_IMAGE_TYPE_2D ||
        (info->flags & (VK_IMAGE_CREATE_SPARSE_BINDING_BIT | VK_IMAGE_CREATE_DISJOINT_BIT)))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    Chain chain(info->pNext, {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO,
                              VK_STRUCTURE_TYPE_IMAGE_COMPRESSION_CONTROL_EXT});
    VkPhysicalDeviceImageDrmFormatModifierInfoEXT mod = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT, nullptr,
        nvgr_modifier(NVGR_MAX_LOG2_BLOCK_H), VK_SHARING_MODE_EXCLUSIVE, 0, nullptr};
    VkPhysicalDeviceExternalImageFormatInfo dmabuf = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO, nullptr, kDmaBuf};
    chain.Add(mod);
    chain.Add(dmabuf);
    VkPhysicalDeviceImageFormatInfo2 nv = *info;
    nv.pNext = chain.head();
    nv.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
    VkResult result = g_inst.GetPhysicalDeviceImageFormatProperties2(physical, &nv, props);
    if (result != VK_SUCCESS)
        return result;
    props->imageFormatProperties.maxMipLevels = 1;
    props->imageFormatProperties.maxArrayLayers = 1;
    props->imageFormatProperties.sampleCounts = VK_SAMPLE_COUNT_1_BIT;
    if (auto* e = static_cast<VkExternalImageFormatProperties*>(
            FindOut(props->pNext, VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES)))
        e->externalMemoryProperties = {VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT |
                                           VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT |
                                           VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT,
                                       static_cast<VkFlags>(kAhb), static_cast<VkFlags>(kAhb)};
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL GetPhysicalDeviceExternalBufferProperties(
    VkPhysicalDevice physical, const VkPhysicalDeviceExternalBufferInfo* info,
    VkExternalBufferProperties* props) {
    if (info->handleType != kAhb) {
        g_inst.GetPhysicalDeviceExternalBufferProperties(physical, info, props);
        return;
    }
    props->externalMemoryProperties = {
        VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT | VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT,
        static_cast<VkFlags>(kAhb), static_cast<VkFlags>(kAhb)};
}

// ---- images, buffers, memory ----

VKAPI_ATTR VkResult VKAPI_CALL CreateImage(VkDevice device, const VkImageCreateInfo* info,
                                           const VkAllocationCallbacks* allocator,
                                           VkImage* out) {
    Device* dev = GetDevice(device);
    if (!IsGrallocImage(info))
        return dev->fn.CreateImage(device, info, allocator, out);

    GrallocImageCreate create;
    VkResult result = create.Init(info);
    if (result != VK_SUCCESS)
        return result;
    result = dev->fn.CreateImage(device, &create.info, allocator, out);
    if (result != VK_SUCCESS) {
        NVVK_LOGE("NVIDIA vkCreateImage (block-linear, modifier %#llx, format %d %ux%u) failed: %d",
                  static_cast<unsigned long long>(create.image.layout.modifier),
                  create.info.format, info->extent.width, info->extent.height, result);
        return result;
    }
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        dev->images[*out] = create.image;
    }
    // VK_ANDROID_native_buffer: an image with a buffer is bound at once; without one
    // (deferred allocation) the loader binds it later through vkBindImageMemory2.
    auto* nb = static_cast<const VkNativeBufferANDROID*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID));
    if (nb && nb->handle) {
        result = BindNativeBuffer(dev, *out, nb);
        if (result != VK_SUCCESS) {
            DestroyImage(device, *out, allocator);
            *out = VK_NULL_HANDLE;
        }
    }
    return result;
}

VKAPI_ATTR void VKAPI_CALL DestroyImage(VkDevice device, VkImage image,
                                        const VkAllocationCallbacks* allocator) {
    if (!image)
        return;
    Device* dev = GetDevice(device);
    VkDeviceMemory owned = VK_NULL_HANDLE;
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        auto it = dev->images.find(image);
        if (it != dev->images.end()) {
            owned = it->second.owned_memory;
            dev->images.erase(it);
        }
    }
    dev->fn.DestroyImage(device, image, allocator);
    if (owned)
        dev->fn.FreeMemory(device, owned, nullptr);
}

VKAPI_ATTR VkResult VKAPI_CALL BindImageMemory2(VkDevice device, uint32_t count,
                                                const VkBindImageMemoryInfo* infos) {
    Device* dev = GetDevice(device);
    std::vector<VkBindImageMemoryInfo> rest;
    rest.reserve(count);
    for (uint32_t i = 0; i < count; i++) {
        auto* nb = static_cast<const VkNativeBufferANDROID*>(
            Find(infos[i].pNext, VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID));
        if (!nb) {
            rest.push_back(infos[i]);
            continue;
        }
        VkResult result = BindNativeBuffer(dev, infos[i].image, nb);
        if (result != VK_SUCCESS)
            return result;
    }
    if (rest.empty())
        return VK_SUCCESS;
    return dev->fn.BindImageMemory2(device, static_cast<uint32_t>(rest.size()), rest.data());
}

VKAPI_ATTR VkResult VKAPI_CALL CreateImageView(VkDevice device, const VkImageViewCreateInfo* info,
                                               const VkAllocationCallbacks* allocator,
                                               VkImageView* out) {
    Device* dev = GetDevice(device);
    if (info->format != VK_FORMAT_UNDEFINED)
        return dev->fn.CreateImageView(device, info, allocator, out);
    // a view of an external-format image: its real format
    VkImageViewCreateInfo nv = *info;
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        auto it = dev->images.find(info->image);
        if (it != dev->images.end())
            nv.format = it->second.format;
    }
    return dev->fn.CreateImageView(device, &nv, allocator, out);
}

VKAPI_ATTR VkResult VKAPI_CALL CreateBuffer(VkDevice device, const VkBufferCreateInfo* info,
                                            const VkAllocationCallbacks* allocator,
                                            VkBuffer* out) {
    Device* dev = GetDevice(device);
    auto* ext = static_cast<const VkExternalMemoryBufferCreateInfo*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO));
    if (!ext || !(ext->handleTypes & kAhb))
        return dev->fn.CreateBuffer(device, info, allocator, out);
    Chain chain(info->pNext, {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO});
    VkExternalMemoryBufferCreateInfo dmabuf = *ext;
    dmabuf.handleTypes = (ext->handleTypes & ~static_cast<VkFlags>(kAhb)) | kDmaBuf;
    chain.Add(dmabuf);
    VkBufferCreateInfo nv = *info;
    nv.pNext = chain.head();
    return dev->fn.CreateBuffer(device, &nv, allocator, out);
}

VKAPI_ATTR VkResult VKAPI_CALL AllocateMemory(VkDevice device, const VkMemoryAllocateInfo* info,
                                              const VkAllocationCallbacks* allocator,
                                              VkDeviceMemory* out) {
    Device* dev = GetDevice(device);
    auto* import = static_cast<const VkImportAndroidHardwareBufferInfoANDROID*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID));
    auto* exp = static_cast<const VkExportMemoryAllocateInfo*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO));
    const bool export_ahb = exp && (exp->handleTypes & kAhb);
    if (!import && !export_ahb)
        return dev->fn.AllocateMemory(device, info, allocator, out);

    auto* dedicated = static_cast<const VkMemoryDedicatedAllocateInfo*>(
        Find(info->pNext, VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO));
    const VkImage image = dedicated ? dedicated->image : VK_NULL_HANDLE;
    const VkBuffer buffer = dedicated ? dedicated->buffer : VK_NULL_HANDLE;
    ImageInfo image_info = {};
    if (image) {
        std::lock_guard<std::mutex> guard(dev->lock);
        auto it = dev->images.find(image);
        if (it == dev->images.end()) {
            NVVK_LOGE("AHB memory for an image without the AHB handle type");
            return VK_ERROR_INVALID_EXTERNAL_HANDLE;
        }
        image_info = it->second;
    }

    AHardwareBuffer* ahb = import ? import->buffer : nullptr;
    if (import) {
        AhbAcquire(ahb);
    } else {
        AhbDesc desc = {};
        if (image) {
            desc = {image_info.extent.width, image_info.extent.height, 1,
                    AhbFormatFromVk(image_info.format), AhbUsageFor(image_info.usage, 0)};
            if (!desc.format) {
                NVVK_LOGE("no AHardwareBuffer format for VkFormat %d", image_info.format);
                return VK_ERROR_FORMAT_NOT_SUPPORTED;
            }
        } else {
            desc = {static_cast<uint32_t>(info->allocationSize), 1, 1, kAhbFormatBlob,
                    kAhbGpuDataBuffer};
        }
        if (AhbAllocate(desc, &ahb) != 0) {
            NVVK_LOGE("AHardwareBuffer_allocate %ux%u format %#x usage %#llx failed", desc.width,
                      desc.height, desc.format, static_cast<unsigned long long>(desc.usage));
            return VK_ERROR_OUT_OF_DEVICE_MEMORY;
        }
    }

    GrallocBuffer b;
    VkResult result = VK_ERROR_INVALID_EXTERNAL_HANDLE;
    if (ReadGrallocHandle(AhbHandle(ahb), &b) && (!image || CheckBufferLayout(b, image_info))) {
        // the app picked the type from GetAndroidHardwareBufferPropertiesANDROID; an export
        // has none of its own yet
        result = ImportDmaBuf(dev, b, image, buffer, import ? static_cast<int>(info->memoryTypeIndex) : -1,
                              info->pNext, out);
    }
    if (result != VK_SUCCESS) {
        AhbRelease(ahb);
        return result;
    }
    std::lock_guard<std::mutex> guard(dev->lock);
    dev->ahbs[*out] = ahb;
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL FreeMemory(VkDevice device, VkDeviceMemory memory,
                                      const VkAllocationCallbacks* allocator) {
    if (!memory)
        return;
    Device* dev = GetDevice(device);
    AHardwareBuffer* ahb = nullptr;
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        auto it = dev->ahbs.find(memory);
        if (it != dev->ahbs.end()) {
            ahb = it->second;
            dev->ahbs.erase(it);
        }
    }
    dev->fn.FreeMemory(device, memory, allocator);
    if (ahb)
        AhbRelease(ahb);
}

VKAPI_ATTR VkResult VKAPI_CALL CreateSamplerYcbcrConversion(
    VkDevice device, const VkSamplerYcbcrConversionCreateInfo* info,
    const VkAllocationCallbacks* allocator, VkSamplerYcbcrConversion* out) {
    Device* dev = GetDevice(device);
    if (!Find(info->pNext, VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID))
        return dev->fn.CreateSamplerYcbcrConversion(device, info, allocator, out);
    VkSamplerYcbcrConversionCreateInfo nv = *info;
    nv.format = ResolveFormat(info->format, info->pNext);
    Chain chain(info->pNext, {VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID});
    nv.pNext = chain.head();
    if (nv.format == VK_FORMAT_UNDEFINED) {
        NVVK_LOGE("unknown external format in a sampler Y'CbCr conversion");
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    return dev->fn.CreateSamplerYcbcrConversion(device, &nv, allocator, out);
}

VKAPI_ATTR void VKAPI_CALL GetDeviceImageMemoryRequirements(
    VkDevice device, const VkDeviceImageMemoryRequirements* info, VkMemoryRequirements2* reqs) {
    Device* dev = GetDevice(device);
    if (!IsGrallocImage(info->pCreateInfo)) {
        dev->fn.GetDeviceImageMemoryRequirements(device, info, reqs);
        return;
    }
    GrallocImageCreate create;
    if (create.Init(info->pCreateInfo) != VK_SUCCESS) {
        reqs->memoryRequirements = {};
        return;
    }
    VkDeviceImageMemoryRequirements nv = *info;
    nv.pCreateInfo = &create.info;
    dev->fn.GetDeviceImageMemoryRequirements(device, &nv, reqs);
}

VKAPI_ATTR VkResult VKAPI_CALL GetAndroidHardwareBufferPropertiesANDROID(
    VkDevice device, const AHardwareBuffer* ahb, VkAndroidHardwareBufferPropertiesANDROID* props) {
    Device* dev = GetDevice(device);
    GrallocBuffer b;
    if (!ReadGrallocHandle(AhbHandle(ahb), &b))
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    const nvgr_fmt* fmt = nvgr_fmt_from_drm(b.drm_format);
    nvgr_layout l = {};
    if (fmt)
        nvgr_compute_layout(fmt, b.width, b.height, &l);
    const bool image = fmt && b.modifier == l.modifier;  // else a linear buffer (blob, ...)

    props->allocationSize = b.size;
    props->memoryTypeBits = FdMemoryTypes(dev, b.fd);
    if (image)
        props->memoryTypeBits &= ImageMemoryTypes(dev);
    if (!props->memoryTypeBits) {
        NVVK_LOGE("AHardwareBuffer %.4s: no memory type NVIDIA can import it with",
                  reinterpret_cast<const char*>(&b.drm_format));
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    }
    // NVIDIA imports system memory (udmabuf: buffers the CPU touches) only into a host memory
    // type, and ANGLE binds an AHB image only to a device-local one (Android 13's SurfaceFlinger
    // draws through ANGLE: Waydroid, docs/waydroid-nvidia.md). Offer the device-local image types
    // too; ImportDmaBuf() then takes a type NVIDIA accepts for the dma-buf.
    if (image && !HasDeviceLocalType(dev, props->memoryTypeBits))
        props->memoryTypeBits |= ImageMemoryTypes(dev) & DeviceLocalTypes(dev);

    auto* f1 = static_cast<VkAndroidHardwareBufferFormatPropertiesANDROID*>(FindOut(
        props->pNext, VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_ANDROID));
    auto* f2 = static_cast<VkAndroidHardwareBufferFormatProperties2ANDROID*>(FindOut(
        props->pNext, VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_2_ANDROID));
    if (auto* resolve = static_cast<VkAndroidHardwareBufferFormatResolvePropertiesANDROID*>(
            FindOut(props->pNext,
                    VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_RESOLVE_PROPERTIES_ANDROID)))
        resolve->colorAttachmentFormat = VK_FORMAT_UNDEFINED;
    if (!f1 && !f2)
        return VK_SUCCESS;

    const VkFormat format = image ? VkFromDrm(b.drm_format) : VK_FORMAT_UNDEFINED;
    const VkFormat sampled = b.drm_format == NVGR_DRM_NV21 ? VK_FORMAT_G8_B8R8_2PLANE_420_UNORM
                                                          : format;
    VkFormatFeatureFlags2 features = 0;
    if (image && sampled != VK_FORMAT_UNDEFINED) {
        VkDrmFormatModifierPropertiesList2EXT list = {
            VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_2_EXT};
        VkFormatProperties2 fp = {VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2, &list};
        g_inst.GetPhysicalDeviceFormatProperties2(dev->physical, sampled, &fp);
        std::vector<VkDrmFormatModifierProperties2EXT> mods(list.drmFormatModifierCount);
        list.pDrmFormatModifierProperties = mods.data();
        g_inst.GetPhysicalDeviceFormatProperties2(dev->physical, sampled, &fp);
        for (const auto& m : mods)
            if (m.drmFormatModifier == b.modifier)
                features = m.drmFormatModifierTilingFeatures;
    }
    const bool yuv = fmt && fmt->num_planes > 1;
    VkComponentMapping components = {VK_COMPONENT_SWIZZLE_IDENTITY, VK_COMPONENT_SWIZZLE_IDENTITY,
                                     VK_COMPONENT_SWIZZLE_IDENTITY, VK_COMPONENT_SWIZZLE_IDENTITY};
    if (fmt && fmt->swap_chroma) {  // NV21 sampled as NV12: Cr and Cb trade places
        components.r = VK_COMPONENT_SWIZZLE_B;
        components.b = VK_COMPONENT_SWIZZLE_R;
    }
    const uint64_t external = image ? b.drm_format : 0;
    const VkSamplerYcbcrModelConversion model =
        yuv ? VK_SAMPLER_YCBCR_MODEL_CONVERSION_YCBCR_601 : VK_SAMPLER_YCBCR_MODEL_CONVERSION_RGB_IDENTITY;
    const VkSamplerYcbcrRange range = yuv ? VK_SAMPLER_YCBCR_RANGE_ITU_NARROW : VK_SAMPLER_YCBCR_RANGE_ITU_FULL;
    if (f1) {
        f1->format = format;
        f1->externalFormat = external;
        f1->formatFeatures = static_cast<VkFormatFeatureFlags>(features);
        f1->samplerYcbcrConversionComponents = components;
        f1->suggestedYcbcrModel = model;
        f1->suggestedYcbcrRange = range;
        f1->suggestedXChromaOffset = VK_CHROMA_LOCATION_MIDPOINT;
        f1->suggestedYChromaOffset = VK_CHROMA_LOCATION_MIDPOINT;
    }
    if (f2) {
        f2->format = format;
        f2->externalFormat = external;
        f2->formatFeatures = features;
        f2->samplerYcbcrConversionComponents = components;
        f2->suggestedYcbcrModel = model;
        f2->suggestedYcbcrRange = range;
        f2->suggestedXChromaOffset = VK_CHROMA_LOCATION_MIDPOINT;
        f2->suggestedYChromaOffset = VK_CHROMA_LOCATION_MIDPOINT;
    }
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL GetMemoryAndroidHardwareBufferANDROID(
    VkDevice device, const VkMemoryGetAndroidHardwareBufferInfoANDROID* info,
    AHardwareBuffer** out) {
    Device* dev = GetDevice(device);
    std::lock_guard<std::mutex> guard(dev->lock);
    auto it = dev->ahbs.find(info->memory);
    if (it == dev->ahbs.end()) {
        NVVK_LOGE("vkGetMemoryAndroidHardwareBufferANDROID: memory has no AHardwareBuffer");
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    }
    AhbAcquire(it->second);
    *out = it->second;
    return VK_SUCCESS;
}

// ---- VK_ANDROID_native_buffer ----

VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsageANDROID(VkDevice, VkFormat format,
                                                               VkImageUsageFlags, int* usage) {
    if (!FmtFromVk(format))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    // GPU only: the gralloc puts such buffers in video memory, block-linear
    *usage = static_cast<int>(kGrallocHwRender | kGrallocHwTexture);
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage2ANDROID(
    VkDevice device, VkFormat format, VkImageUsageFlags image_usage,
    VkSwapchainImageUsageFlagsANDROID, uint64_t* consumer, uint64_t* producer) {
    int usage = 0;
    VkResult result = GetSwapchainGrallocUsageANDROID(device, format, image_usage, &usage);
    *consumer = 1ull << 8;  // GRALLOC1_CONSUMER_USAGE_GPU_TEXTURE
    *producer = 1ull << 9;  // GRALLOC1_PRODUCER_USAGE_GPU_RENDER_TARGET
    return result;
}

VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage3ANDROID(
    VkDevice device, const VkGrallocUsageInfoANDROID* info, uint64_t* usage) {
    int u = 0;
    VkResult result = GetSwapchainGrallocUsageANDROID(device, info->format, info->imageUsage, &u);
    *usage = static_cast<uint64_t>(u);
    return result;
}

VKAPI_ATTR VkResult VKAPI_CALL GetSwapchainGrallocUsage4ANDROID(
    VkDevice device, const VkGrallocUsageInfo2ANDROID* info, uint64_t* usage) {
    int u = 0;
    VkResult result = GetSwapchainGrallocUsageANDROID(device, info->format, info->imageUsage, &u);
    *usage = static_cast<uint64_t>(u);
    return result;
}

VKAPI_ATTR VkResult VKAPI_CALL AcquireImageANDROID(VkDevice device, VkImage, int fence_fd,
                                                   VkSemaphore semaphore, VkFence fence) {
    Device* dev = GetDevice(device);
    VkResult result = VK_SUCCESS;
    // A sync fd import of -1 means "already signaled"; each import takes its own fd.
    if (semaphore) {
        const int fd = fence_fd >= 0 ? dup(fence_fd) : -1;
        VkImportSemaphoreFdInfoKHR import = {VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR,
                                             nullptr, semaphore,
                                             VK_SEMAPHORE_IMPORT_TEMPORARY_BIT,
                                             VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT, fd};
        result = dev->fn.ImportSemaphoreFdKHR(device, &import);
        if (result != VK_SUCCESS) {
            NVVK_LOGE("acquire: semaphore sync fd import failed: %d", result);
            if (fd >= 0)
                close(fd);
        }
    }
    if (fence && result == VK_SUCCESS) {
        const int fd = fence_fd >= 0 ? dup(fence_fd) : -1;
        VkImportFenceFdInfoKHR import = {VK_STRUCTURE_TYPE_IMPORT_FENCE_FD_INFO_KHR, nullptr,
                                         fence, VK_FENCE_IMPORT_TEMPORARY_BIT,
                                         VK_EXTERNAL_FENCE_HANDLE_TYPE_SYNC_FD_BIT, fd};
        result = dev->fn.ImportFenceFdKHR(device, &import);
        if (result != VK_SUCCESS) {
            NVVK_LOGE("acquire: fence sync fd import failed: %d", result);
            if (fd >= 0)
                close(fd);
        }
    }
    if (fence_fd >= 0)
        close(fence_fd);  // the HAL owns the caller's fd
    return result;
}

VKAPI_ATTR VkResult VKAPI_CALL QueueSignalReleaseImageANDROID(VkQueue queue, uint32_t wait_count,
                                                              const VkSemaphore* waits, VkImage,
                                                              int* native_fence_fd) {
    Device* dev = GetDeviceForQueue(queue);
    *native_fence_fd = -1;
    if (!dev) {
        NVVK_LOGE("release: unknown queue");
        return VK_ERROR_DEVICE_LOST;
    }
    VkSemaphore signal;
    {
        std::lock_guard<std::mutex> guard(dev->lock);
        VkSemaphore& sem = dev->release_semaphores[queue];
        if (!sem) {
            VkExportSemaphoreCreateInfo exp = {VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO,
                                               nullptr,
                                               VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT};
            VkSemaphoreCreateInfo info = {VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO, &exp, 0};
            VkResult result = dev->fn.CreateSemaphore(dev->handle, &info, nullptr, &sem);
            if (result != VK_SUCCESS) {
                NVVK_LOGE("release: cannot create an exportable semaphore: %d", result);
                sem = VK_NULL_HANDLE;
                return result;
            }
        }
        signal = sem;
    }
    // Signals once everything submitted to the queue so far (the frame) and the waits are done.
    std::vector<VkPipelineStageFlags> stages(wait_count, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, nullptr, wait_count, waits,
                           stages.data(), 0, nullptr, 1, &signal};
    VkResult result = dev->fn.QueueSubmit(queue, 1, &submit, VK_NULL_HANDLE);
    if (result != VK_SUCCESS) {
        NVVK_LOGE("release: vkQueueSubmit failed: %d", result);
        return result;
    }
    VkSemaphoreGetFdInfoKHR get = {VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR, nullptr, signal,
                                   VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT};
    result = dev->fn.GetSemaphoreFdKHR(dev->handle, &get, native_fence_fd);
    if (result != VK_SUCCESS) {
        NVVK_LOGE("release: sync fd export failed: %d", result);
        *native_fence_fd = -1;
    }
    return result;
}

}  // namespace nvvk
