/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// Host test of vulkan.nvidia and minigbm's "nvidia" backend on the build host's RTX 4070
// (tools/nvgpu-hosttest.sh builds it with glibc: NVIDIA's ICD through plain dlopen, minigbm
// through its gbm API on nvidia-drm's render node). It plays Android's parts:
//   - the Vulkan loader (hwvulkan module, instance, device, VK_ANDROID_native_buffer swapchain
//     images, acquire/release with sync fds, deferred binding),
//   - SurfaceFlinger/HWUI (AHardwareBuffer import through
//     VK_ANDROID_external_memory_android_hardware_buffer, export),
//   - CPU producers and consumers (the mapper: lock/unlock through minigbm),
// and checks every pixel that crosses between them.

#include <dlfcn.h>
#include <fcntl.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <functional>
#include <string>

#include <cros_gralloc/cros_gralloc_handle.h>
#include <gbm.h>
#include <hardware/hwvulkan.h>

#include "../vulkan/nvvk_priv.h"

// ---------------------------------------------------------------------------------------------
// The platform layer of vulkan.nvidia, host flavour

struct AHardwareBuffer {
    gbm_bo* bo;
    cros_gralloc_handle handle;
    nvvk::AhbDesc desc;
    int refs;
};

namespace {
gbm_device* g_gbm;
int g_failures;
bool g_verbose = getenv("NVVK_VERBOSE") != nullptr;
}  // namespace

namespace nvvk {

void Log(LogLevel level, const char* fmt, ...) {
    if (level == kLogDebug && !g_verbose)
        return;
    static const char* kName[] = {"E", "W", "I", "D"};
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "  [%s %s] ", NVVK_TAG, kName[level]);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
}

int GetPropertyInt(const char* name, int def) {
    const char* v = getenv(name);
    return v ? atoi(v) : def;
}

void* IcdOpen(const char* name, char* err, size_t err_size) {
    void* lib = dlopen(name, RTLD_NOW | RTLD_LOCAL);
    if (!lib)
        snprintf(err, err_size, "%s", dlerror());
    return lib;
}

void* IcdSym(void* lib, const char* name) {
    return dlsym(lib, name);
}

const native_handle* AhbHandle(const AHardwareBuffer* ahb) {
    return &ahb->handle;
}

void AhbDescribe(const AHardwareBuffer* ahb, AhbDesc* desc) {
    *desc = ahb->desc;
}

// AHB format -> DRM format, as cros_gralloc does
static uint32_t DrmFromAhb(uint32_t f) {
    switch (f) {
        case 1: return NVGR_DRM_ABGR8888;
        case 2: return NVGR_DRM_XBGR8888;
        case 4: return NVGR_DRM_RGB565;
        case 5: return NVGR_DRM_ARGB8888;
        case 0x16: return NVGR_DRM_ABGR16161616F;
        case 0x21: return NVGR_DRM_R8;  // blob
        case 0x23: return NVGR_DRM_NV12;
        case 0x2b: return NVGR_DRM_ABGR2101010;
        case 0x32315659: return NVGR_DRM_YVU420_ANDROID;  // HAL_PIXEL_FORMAT_YV12
        case 0x11: return NVGR_DRM_NV21;  // HAL_PIXEL_FORMAT_YCrCb_420_SP
        case 0x36: return NVGR_DRM_P010;
        case 0x38: return NVGR_DRM_R8;
        default: return 0;
    }
}

int AhbAllocate(const AhbDesc& desc, AHardwareBuffer** out) {
    uint32_t flags = 0;
    if (desc.usage & (1ull << 8)) flags |= GBM_BO_USE_TEXTURING;
    if (desc.usage & (1ull << 9)) flags |= GBM_BO_USE_RENDERING;
    if (desc.usage & 0xf) flags |= GBM_BO_USE_SW_READ_OFTEN;
    if (desc.usage & 0xf0) flags |= GBM_BO_USE_SW_WRITE_OFTEN;
    if (desc.usage & (1ull << 11)) flags |= GBM_BO_USE_SCANOUT;  // COMPOSER_OVERLAY
    if (desc.usage & (1ull << 24)) flags |= GBM_BO_USE_GPU_DATA_BUFFER;
    const uint32_t format = DrmFromAhb(desc.format);
    gbm_bo* bo = gbm_bo_create(g_gbm, desc.width, desc.height, format, flags);
    if (!bo)
        return -1;
    auto* ahb = new AHardwareBuffer();
    ahb->bo = bo;
    ahb->desc = desc;
    ahb->refs = 1;
    cros_gralloc_handle& h = ahb->handle;
    const int planes = gbm_bo_get_plane_count(bo);
    h.version = sizeof(native_handle_t);
    h.numFds = planes;
    h.numInts = (sizeof(cros_gralloc_handle) - sizeof(native_handle_t)) / sizeof(int) - DRV_MAX_FDS;
    for (int p = 0; p < planes; p++) {
        h.fds[p] = gbm_bo_get_fd_for_plane(bo, p);
        h.strides[p] = gbm_bo_get_stride_for_plane(bo, p);
        h.offsets[p] = gbm_bo_get_offset(bo, p);
    }
    h.width = desc.width;
    h.height = desc.height;
    h.format = format;
    h.format_modifier = gbm_bo_get_modifier(bo);
    h.magic = 0xABCDDCBA;
    h.droid_format = static_cast<int32_t>(desc.format);
    h.usage = static_cast<int64_t>(desc.usage);
    h.num_planes = planes;
    h.total_size = lseek(h.fds[0], 0, SEEK_END);
    *out = ahb;
    return 0;
}

void AhbAcquire(AHardwareBuffer* ahb) {
    ahb->refs++;
}

void AhbRelease(AHardwareBuffer* ahb) {
    if (--ahb->refs)
        return;
    for (uint32_t p = 0; p < ahb->handle.num_planes; p++)
        close(ahb->handle.fds[p]);
    gbm_bo_destroy(ahb->bo);
    delete ahb;
}

}  // namespace nvvk

extern "C" hwvulkan_module_t HAL_MODULE_INFO_SYM;

// ---------------------------------------------------------------------------------------------
// The test

namespace {

using nvvk::AhbDesc;

#define CHECK(cond, ...)                                       \
    do {                                                       \
        if (!(cond)) {                                         \
            printf("FAIL %s:%d: %s: ", __FILE__, __LINE__, #cond); \
            printf(__VA_ARGS__);                               \
            printf("\n");                                      \
            g_failures++;                                      \
            return;                                            \
        }                                                      \
    } while (0)
#define VK(call) CHECK((call) == VK_SUCCESS, "%s", #call)

constexpr uint64_t kGpuSampled = 1ull << 8, kGpuFb = 1ull << 9, kCpuReadOften = 3,
                   kCpuWriteOften = 3 << 4, kComposer = 1ull << 11, kGpuData = 1ull << 24;

struct Vk {
    hwvulkan_device_t* hal;
    PFN_vkGetInstanceProcAddr gipa;
    PFN_vkGetDeviceProcAddr gdpa;
    VkInstance instance;
    VkPhysicalDevice physical;
    VkDevice device;
    VkQueue queue;
    VkCommandPool pool;
    VkPhysicalDeviceMemoryProperties mem;
} vk;

template <typename T>
T I(const char* name) {
    return reinterpret_cast<T>(vk.gipa(vk.instance, name));
}
template <typename T>
T D(const char* name) {
    return reinterpret_cast<T>(vk.gdpa(vk.device, name));
}
#define IFN(name) I<PFN_vk##name>("vk" #name)
#define DFN(name) D<PFN_vk##name>("vk" #name)

int NvidiaFds() {
    int n = 0;
    char path[64], target[128];
    for (int fd = 0; fd < 1024; fd++) {
        snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
        const ssize_t len = readlink(path, target, sizeof(target) - 1);
        if (len > 0) {
            target[len] = 0;
            n += !strncmp(target, "/dev/nvidia", 11);
        }
    }
    return n;
}

bool HasExt(const std::vector<VkExtensionProperties>& exts, const char* name) {
    for (auto& e : exts)
        if (!strcmp(e.extensionName, name))
            return true;
    return false;
}

void Setup() {
    hw_device_t* dev = nullptr;
    int ret = HAL_MODULE_INFO_SYM.common.methods->open(&HAL_MODULE_INFO_SYM.common,
                                                       HWVULKAN_DEVICE_0, &dev);
    CHECK(ret == 0, "hwvulkan open: %d", ret);
    vk.hal = reinterpret_cast<hwvulkan_device_t*>(dev);
    vk.gipa = vk.hal->GetInstanceProcAddr;

    uint32_t n = 0;
    vk.hal->EnumerateInstanceExtensionProperties(nullptr, &n, nullptr);
    std::vector<VkExtensionProperties> iexts(n);
    vk.hal->EnumerateInstanceExtensionProperties(nullptr, &n, iexts.data());
    CHECK(!HasExt(iexts, "VK_KHR_surface") && !HasExt(iexts, "VK_KHR_xcb_surface"),
          "window-system instance extensions not hidden");
    CHECK(HasExt(iexts, "VK_KHR_get_physical_device_properties2"), "instance exts");
    // what zygote does (it preloads the HAL): no NVIDIA device may be open yet
    CHECK(NvidiaFds() == 0, "%d /dev/nvidia* fds open after open + instance extensions", NvidiaFds());

    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, nullptr, "nvvk_hosttest", 1,
                             nullptr, 0, VK_API_VERSION_1_3};
    VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, nullptr, 0, &app};
    VK(vk.hal->CreateInstance(&ici, nullptr, &vk.instance));
    printf("ok   no /dev/nvidia* fd until vkCreateInstance (%d after it)\n", NvidiaFds());
    CHECK(*reinterpret_cast<uintptr_t*>(vk.instance) == HWVULKAN_DISPATCH_MAGIC,
          "instance does not start with the loader magic");
    uint32_t count = 8;
    VkPhysicalDevice pds[8];
    VK(IFN(EnumeratePhysicalDevices)(vk.instance, &count, pds));
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceProperties p;
        IFN(GetPhysicalDeviceProperties)(pds[i], &p);
        if (p.vendorID == 0x10de) {
            vk.physical = pds[i];
            printf("device: %s, driver %u.%u.%u, Vulkan %u.%u.%u\n", p.deviceName,
                   p.driverVersion >> 22, (p.driverVersion >> 14) & 0xff,
                   (p.driverVersion >> 6) & 0xff, VK_API_VERSION_MAJOR(p.apiVersion),
                   VK_API_VERSION_MINOR(p.apiVersion), VK_API_VERSION_PATCH(p.apiVersion));
        }
    }
    CHECK(vk.physical, "no NVIDIA GPU");
    IFN(GetPhysicalDeviceMemoryProperties)(vk.physical, &vk.mem);

    VkPhysicalDevicePresentationPropertiesANDROID pres = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PRESENTATION_PROPERTIES_ANDROID, nullptr, VK_TRUE};
    VkPhysicalDeviceProperties2 p2 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &pres};
    IFN(GetPhysicalDeviceProperties2)(vk.physical, &p2);
    CHECK(p2.pNext == &pres && !pres.sharedImage, "presentation properties");

    n = 0;
    IFN(EnumerateDeviceExtensionProperties)(vk.physical, nullptr, &n, nullptr);
    std::vector<VkExtensionProperties> dexts(n);
    IFN(EnumerateDeviceExtensionProperties)(vk.physical, nullptr, &n, dexts.data());
    CHECK(HasExt(dexts, "VK_ANDROID_native_buffer") &&
              HasExt(dexts, "VK_ANDROID_external_memory_android_hardware_buffer") &&
              !HasExt(dexts, "VK_KHR_swapchain"),
          "device extensions");

    // what Android's loader and HWUI pass: native buffer (for VK_KHR_swapchain), AHB, sync fd,
    // a REALTIME queue (capped to MEDIUM by the HAL)
    const char* names[] = {"VK_ANDROID_native_buffer",
                           "VK_ANDROID_external_memory_android_hardware_buffer",
                           "VK_KHR_external_semaphore_fd", "VK_EXT_queue_family_foreign"};
    float prio = 1.0f;
    VkDeviceQueueGlobalPriorityCreateInfoKHR gp = {
        VK_STRUCTURE_TYPE_DEVICE_QUEUE_GLOBAL_PRIORITY_CREATE_INFO_KHR, nullptr,
        VK_QUEUE_GLOBAL_PRIORITY_REALTIME_KHR};
    VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, &gp, 0, 0, 1, &prio};
    VkPhysicalDeviceSamplerYcbcrConversionFeatures ycbcr = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLER_YCBCR_CONVERSION_FEATURES, nullptr, VK_TRUE};
    VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &ycbcr, 0, 1, &qci, 0, nullptr,
                              4, names};
    VK(IFN(CreateDevice)(vk.physical, &dci, nullptr, &vk.device));
    vk.gdpa = IFN(GetDeviceProcAddr);
    DFN(GetDeviceQueue)(vk.device, 0, 0, &vk.queue);
    VkCommandPoolCreateInfo pci = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, nullptr,
                                   VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, 0};
    VK(DFN(CreateCommandPool)(vk.device, &pci, nullptr, &vk.pool));
}

// A host-visible buffer for copies.
struct Staging {
    VkBuffer buffer = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    uint8_t* data = nullptr;
    void Init(VkDeviceSize size) {
        VkBufferCreateInfo bci = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, nullptr, 0, size,
                                  VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT};
        DFN(CreateBuffer)(vk.device, &bci, nullptr, &buffer);
        VkMemoryRequirements req;
        DFN(GetBufferMemoryRequirements)(vk.device, buffer, &req);
        uint32_t type = 0;
        for (uint32_t i = 0; i < vk.mem.memoryTypeCount; i++)
            if ((req.memoryTypeBits >> i & 1) &&
                (vk.mem.memoryTypes[i].propertyFlags & 6) == 6) {
                type = i;
                break;
            }
        VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, nullptr, req.size, type};
        DFN(AllocateMemory)(vk.device, &mai, nullptr, &memory);
        DFN(BindBufferMemory)(vk.device, buffer, memory, 0);
        DFN(MapMemory)(vk.device, memory, 0, size, 0, reinterpret_cast<void**>(&data));
    }
    ~Staging() {
        if (buffer) {
            DFN(DestroyBuffer)(vk.device, buffer, nullptr);
            DFN(FreeMemory)(vk.device, memory, nullptr);
        }
    }
};

// Records cmds into a fresh command buffer and submits it; waits on wait (if any) and
// returns when done.
void Submit(const std::function<void(VkCommandBuffer)>& cmds, VkSemaphore wait = VK_NULL_HANDLE) {
    VkCommandBuffer cb;
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, nullptr,
                                       vk.pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1};
    DFN(AllocateCommandBuffers)(vk.device, &cai, &cb);
    VkCommandBufferBeginInfo bi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    DFN(BeginCommandBuffer)(cb, &bi);
    cmds(cb);
    DFN(EndCommandBuffer)(cb);
    VkPipelineStageFlags stage = VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
    VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO, nullptr, wait ? 1u : 0u, &wait, &stage, 1,
                       &cb, 0, nullptr};
    VkResult r1 = DFN(QueueSubmit)(vk.queue, 1, &si, VK_NULL_HANDLE);
    VkResult r2 = DFN(QueueWaitIdle)(vk.queue);
    if (r1 || r2) {
        printf("  submit failed: %d / %d (device lost?)\n", r1, r2);
        g_failures++;
    }
    DFN(FreeCommandBuffers)(vk.device, vk.pool, 1, &cb);
}

void Barrier(VkCommandBuffer cb, VkImage image, VkImageLayout from, VkImageLayout to,
             uint32_t src_family, uint32_t dst_family,
             VkImageAspectFlags aspect = VK_IMAGE_ASPECT_COLOR_BIT) {
    VkImageMemoryBarrier b = {VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, nullptr,
                              VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
                              from, to, src_family, dst_family, image, {aspect, 0, 1, 0, 1}};
    DFN(CmdPipelineBarrier)(cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                            VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, nullptr, 0, nullptr, 1, &b);
}

// An AHB as SurfaceFlinger's RenderEngine imports it: properties, image, dedicated import.
struct AhbImage {
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkFormat format = VK_FORMAT_UNDEFINED;
    bool Import(AHardwareBuffer* ahb, VkImageUsageFlags usage) {
        VkAndroidHardwareBufferFormatPropertiesANDROID fp = {
            VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_ANDROID};
        VkAndroidHardwareBufferPropertiesANDROID props = {
            VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID, &fp};
        if (DFN(GetAndroidHardwareBufferPropertiesANDROID)(vk.device, ahb, &props) != VK_SUCCESS) {
            printf("  GetAndroidHardwareBufferPropertiesANDROID failed\n");
            return false;
        }
        format = fp.format;
        if (g_verbose)
            printf("  AHB props: size %llu types %#x format %d external %#llx features %#x\n",
                   (unsigned long long)props.allocationSize, props.memoryTypeBits, fp.format,
                   (unsigned long long)fp.externalFormat, fp.formatFeatures);
        VkExternalFormatANDROID ext_format = {VK_STRUCTURE_TYPE_EXTERNAL_FORMAT_ANDROID, nullptr,
                                              fp.format ? 0 : fp.externalFormat};
        VkExternalMemoryImageCreateInfo emi = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
                                               &ext_format,
                                               VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
        VkImageCreateInfo ici = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, &emi, 0, VK_IMAGE_TYPE_2D,
                                 fp.format, {ahb->desc.width, ahb->desc.height, 1}, 1, 1,
                                 VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL, usage,
                                 VK_SHARING_MODE_EXCLUSIVE, 0, nullptr, VK_IMAGE_LAYOUT_UNDEFINED};
        if (DFN(CreateImage)(vk.device, &ici, nullptr, &image) != VK_SUCCESS) {
            printf("  AHB vkCreateImage failed\n");
            return false;
        }
        VkMemoryDedicatedAllocateInfo ded = {VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
                                             nullptr, image, VK_NULL_HANDLE};
        VkImportAndroidHardwareBufferInfoANDROID imp = {
            VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, &ded, ahb};
        VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &imp,
                                    props.allocationSize,
                                    static_cast<uint32_t>(__builtin_ctz(props.memoryTypeBits))};
        if (DFN(AllocateMemory)(vk.device, &mai, nullptr, &memory) != VK_SUCCESS) {
            printf("  AHB import failed\n");
            return false;
        }
        return DFN(BindImageMemory)(vk.device, image, memory, 0) == VK_SUCCESS;
    }
    ~AhbImage() {
        if (image)
            DFN(DestroyImage)(vk.device, image, nullptr);
        if (memory)
            DFN(FreeMemory)(vk.device, memory, nullptr);
    }
};

uint32_t Pattern(uint32_t x, uint32_t y, uint32_t salt) {
    return (x * 7 + y * 131 + salt * 0x9e3779b9u) ^ (y << 16) ^ (x << 5);
}

// Fills (write) or checks (!write) one plane of a CPU mapping with Pattern bytes.
int CpuPlane(uint8_t* base, uint32_t stride, uint32_t width_bytes, uint32_t rows, uint32_t salt,
             bool write) {
    int bad = 0;
    for (uint32_t y = 0; y < rows; y++)
        for (uint32_t x = 0; x < width_bytes; x++) {
            const uint8_t v = static_cast<uint8_t>(Pattern(x, y, salt));
            if (write)
                base[(size_t)y * stride + x] = v;
            else if (base[(size_t)y * stride + x] != v && bad++ < 3)
                printf("  cpu (%u,%u): %02x want %02x\n", x, y, base[(size_t)y * stride + x], v);
        }
    return bad;
}

int StagingPlane(const uint8_t* data, uint32_t width_bytes, uint32_t rows, uint32_t salt) {
    int bad = 0;
    for (uint32_t y = 0; y < rows; y++)
        for (uint32_t x = 0; x < width_bytes; x++) {
            const uint8_t v = static_cast<uint8_t>(Pattern(x, y, salt));
            if (data[(size_t)y * width_bytes + x] != v && bad++ < 3)
                printf("  gpu (%u,%u): %02x want %02x\n", x, y, data[(size_t)y * width_bytes + x], v);
        }
    return bad;
}

struct PlaneDesc {
    VkImageAspectFlagBits aspect;
    uint32_t width, height, cpp;
};

std::vector<PlaneDesc> PlanesOf(uint32_t drm_format, uint32_t w, uint32_t h) {
    const nvgr_fmt* f = nvgr_fmt_from_drm(drm_format);
    std::vector<PlaneDesc> planes;
    for (uint32_t p = 0; p < f->num_planes; p++)
        planes.push_back({f->num_planes == 1 ? VK_IMAGE_ASPECT_COLOR_BIT
                                             : static_cast<VkImageAspectFlagBits>(
                                                   VK_IMAGE_ASPECT_PLANE_0_BIT << p),
                          (w + f->hsub[p] - 1) / f->hsub[p], (h + f->vsub[p] - 1) / f->vsub[p],
                          f->cpp[p]});
    return planes;
}

// CPU writes through minigbm (tile + flush), the GPU reads through an AHB image.
bool Selected(const char* name) {
    const char* only = getenv("NVVK_ONLY");
    return !only || strstr(name, only);
}

void TestCpuToGpu(uint32_t ahb_format, uint32_t w, uint32_t h, const char* name) {
    if (!Selected(name))
        return;
    AHardwareBuffer* ahb = nullptr;
    CHECK(nvvk::AhbAllocate({w, h, 1, ahb_format, kCpuWriteOften | kGpuSampled}, &ahb) == 0,
          "allocate %s", name);
    const uint32_t drm = ahb->handle.format;
    auto planes = PlanesOf(drm, w, h);
    // CPU plane order of YV12 is Y, V, U; Vulkan's is Y, U(Cb), V(Cr)
    const bool yvu = drm == NVGR_DRM_YVU420_ANDROID || drm == NVGR_DRM_YVU420;
    for (uint32_t p = 0; p < planes.size(); p++) {
        uint32_t stride;
        void* map_data;
        auto* addr = static_cast<uint8_t*>(gbm_bo_map2(ahb->bo, 0, 0, w, h, GBM_BO_TRANSFER_WRITE,
                                                       &stride, &map_data, p));
        CHECK(addr && addr != MAP_FAILED, "map plane %u", p);
        const uint32_t vp = yvu && p ? 3 - p : p;
        CpuPlane(addr, stride, planes[vp].width * planes[vp].cpp, planes[vp].height, vp, true);
        gbm_bo_unmap(ahb->bo, map_data);
    }
    AhbImage img;
    CHECK(img.Import(ahb, VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT),
          "import %s", name);
    int bad = 0;
    for (uint32_t p = 0; p < planes.size(); p++) {
        Staging s;
        s.Init((VkDeviceSize)planes[p].width * planes[p].height * planes[p].cpp);
        Submit([&](VkCommandBuffer cb) {
            Barrier(cb, img.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    VK_QUEUE_FAMILY_FOREIGN_EXT, 0, planes[p].aspect);
            VkBufferImageCopy r = {0, 0, 0, {static_cast<VkImageAspectFlags>(planes[p].aspect), 0, 0, 1},
                                   {0, 0, 0}, {planes[p].width, planes[p].height, 1}};
            DFN(CmdCopyImageToBuffer)(cb, img.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                                      s.buffer, 1, &r);
        });
        bad += StagingPlane(s.data, planes[p].width * planes[p].cpp, planes[p].height, p);
    }
    printf("%s cpu->gpu %s %ux%u (%.4s, modifier %#llx): %d bad bytes\n", bad ? "FAIL" : "ok  ",
           name, w, h, reinterpret_cast<const char*>(&drm),
           (unsigned long long)ahb->handle.format_modifier, bad);
    if (bad)
        g_failures++;
    nvvk::AhbRelease(ahb);
}

// The GPU writes into an AHB image, the CPU reads through minigbm (flush + detile).
void TestGpuToCpu(uint32_t ahb_format, uint32_t w, uint32_t h, uint64_t usage, const char* name) {
    if (!Selected(name))
        return;
    AHardwareBuffer* ahb = nullptr;
    CHECK(nvvk::AhbAllocate({w, h, 1, ahb_format, usage}, &ahb) == 0, "allocate %s", name);
    const uint32_t drm = ahb->handle.format;
    auto planes = PlanesOf(drm, w, h);
    AhbImage img;
    // YUV formats have no color attachment feature with block-linear modifiers: copies only
    CHECK(img.Import(ahb, planes.size() > 1 ? VK_IMAGE_USAGE_TRANSFER_DST_BIT
                                             : VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                                                   VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT),
          "import %s", name);
    for (uint32_t p = 0; p < planes.size(); p++) {
        Staging s;
        const uint32_t wb = planes[p].width * planes[p].cpp;
        s.Init((VkDeviceSize)wb * planes[p].height);
        for (uint32_t y = 0; y < planes[p].height; y++)
            for (uint32_t x = 0; x < wb; x++)
                s.data[(size_t)y * wb + x] = static_cast<uint8_t>(Pattern(x, y, p + 10));
        Submit([&](VkCommandBuffer cb) {
            Barrier(cb, img.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    VK_QUEUE_FAMILY_FOREIGN_EXT, 0, planes[p].aspect);
            VkBufferImageCopy r = {0, 0, 0, {static_cast<VkImageAspectFlags>(planes[p].aspect), 0, 0, 1},
                                   {0, 0, 0}, {planes[p].width, planes[p].height, 1}};
            DFN(CmdCopyBufferToImage)(cb, s.buffer, img.image,
                                      VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &r);
            Barrier(cb, img.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_GENERAL, 0,
                    VK_QUEUE_FAMILY_FOREIGN_EXT, planes[p].aspect);
        });
    }
    int bad = 0;
    const bool yvu = drm == NVGR_DRM_YVU420_ANDROID || drm == NVGR_DRM_YVU420;
    for (uint32_t p = 0; p < planes.size(); p++) {
        uint32_t stride;
        void* map_data;
        auto* addr = static_cast<uint8_t*>(gbm_bo_map2(ahb->bo, 0, 0, w, h, GBM_BO_TRANSFER_READ,
                                                       &stride, &map_data, p));
        CHECK(addr && addr != MAP_FAILED, "map plane %u", p);
        const uint32_t vp = yvu && p ? 3 - p : p;
        bad += CpuPlane(addr, stride, planes[vp].width * planes[vp].cpp, planes[vp].height,
                        vp + 10, false);
        gbm_bo_unmap(ahb->bo, map_data);
    }
    printf("%s gpu->cpu %s %ux%u (%.4s, modifier %#llx): %d bad bytes\n", bad ? "FAIL" : "ok  ",
           name, w, h, reinterpret_cast<const char*>(&drm),
           (unsigned long long)ahb->handle.format_modifier, bad);
    if (bad)
        g_failures++;
    nvvk::AhbRelease(ahb);
}

// Android's swapchain: window buffers from gralloc with the HAL's usage, images with
// VkNativeBufferANDROID (one deferred), acquire -> render -> release, then the composer
// (SurfaceFlinger) side imports the buffer as an AHB after waiting on the release fence.
void TestSwapchain() {
    const uint32_t w = 1280, h = 720;
    // the loader's first question: AHB usage for the swapchain format
    VkAndroidHardwareBufferUsageANDROID ahb_usage = {
        VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_USAGE_ANDROID};
    VkImageFormatProperties2 ifp = {VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2, &ahb_usage};
    VkPhysicalDeviceExternalImageFormatInfo ext = {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO, nullptr,
        VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
    VkPhysicalDeviceImageFormatInfo2 ifi = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2,
                                            &ext, VK_FORMAT_R8G8B8A8_UNORM, VK_IMAGE_TYPE_2D,
                                            VK_IMAGE_TILING_OPTIMAL,
                                            VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT |
                                                VK_IMAGE_USAGE_TRANSFER_DST_BIT,
                                            0};
    VK(IFN(GetPhysicalDeviceImageFormatProperties2)(vk.physical, &ifi, &ifp));
    CHECK((ahb_usage.androidHardwareBufferUsage & kGpuFb) && ifp.pNext == &ahb_usage,
          "AHB usage %#llx", (unsigned long long)ahb_usage.androidHardwareBufferUsage);
    uint64_t usage4 = 0;
    VkGrallocUsageInfo2ANDROID gui = {VK_STRUCTURE_TYPE_GRALLOC_USAGE_INFO_2_ANDROID, nullptr,
                                      VK_FORMAT_R8G8B8A8_UNORM,
                                      VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, 0};
    VK(D<PFN_vkGetSwapchainGrallocUsage4ANDROID>("vkGetSwapchainGrallocUsage4ANDROID")(
        vk.device, &gui, &usage4));
    CHECK(usage4 == 0x300, "gralloc usage %#llx", (unsigned long long)usage4);

    // three window buffers: SurfaceFlinger adds the composer usage
    AHardwareBuffer* bufs[3];
    VkImage images[3];
    for (int i = 0; i < 3; i++) {
        CHECK(nvvk::AhbAllocate({w, h, 1, 1, kGpuSampled | kGpuFb | kComposer}, &bufs[i]) == 0,
              "window buffer");
        VkSwapchainImageCreateInfoANDROID sci = {
            VK_STRUCTURE_TYPE_SWAPCHAIN_IMAGE_CREATE_INFO_ANDROID, nullptr, 0};
        VkNativeBufferANDROID nb = {VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID, &sci,
                                    i == 2 ? nullptr : &bufs[i]->handle, 0, 1, 0x300, {}, 0x300,
                                    nullptr};
        VkImageCreateInfo ici = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, &nb, 0, VK_IMAGE_TYPE_2D,
                                 VK_FORMAT_R8G8B8A8_UNORM, {w, h, 1}, 1, 1,
                                 VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
                                 VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT |
                                     VK_IMAGE_USAGE_TRANSFER_DST_BIT,
                                 VK_SHARING_MODE_EXCLUSIVE, 0, nullptr, VK_IMAGE_LAYOUT_UNDEFINED};
        VK(DFN(CreateImage)(vk.device, &ici, nullptr, &images[i]));
    }
    // deferred allocation: the loader binds the third one when it dequeues it
    VkBindImageMemorySwapchainInfoKHR bsi = {VK_STRUCTURE_TYPE_BIND_IMAGE_MEMORY_SWAPCHAIN_INFO_KHR,
                                             nullptr, reinterpret_cast<VkSwapchainKHR>(0x1234), 2};
    VkNativeBufferANDROID nb = {VK_STRUCTURE_TYPE_NATIVE_BUFFER_ANDROID, &bsi, &bufs[2]->handle, 0,
                                1, 0x300, {}, 0x300, nullptr};
    VkBindImageMemoryInfo bind = {VK_STRUCTURE_TYPE_BIND_IMAGE_MEMORY_INFO, &nb, images[2],
                                  VK_NULL_HANDLE, 0};
    VK(DFN(BindImageMemory2)(vk.device, 1, &bind));

    auto acquire = D<PFN_vkAcquireImageANDROID>("vkAcquireImageANDROID");
    auto release = D<PFN_vkQueueSignalReleaseImageANDROID>("vkQueueSignalReleaseImageANDROID");
    VkSemaphoreCreateInfo sci = {VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
    VkSemaphore acquired, rendered;
    VK(DFN(CreateSemaphore)(vk.device, &sci, nullptr, &acquired));
    VK(DFN(CreateSemaphore)(vk.device, &sci, nullptr, &rendered));
    VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    VK(DFN(CreateFence)(vk.device, &fci, nullptr, &fence));

    int last_fence = -1;
    for (int frame = 0; frame < 6; frame++) {
        const int i = frame % 3;
        // dequeueBuffer's fence: the previous frame's release fence (or none)
        VK(acquire(vk.device, images[i], last_fence, acquired, fence));
        last_fence = -1;
        VK(DFN(WaitForFences)(vk.device, 1, &fence, VK_TRUE, 2000000000ull));
        DFN(ResetFences)(vk.device, 1, &fence);
        VkClearColorValue color = {.uint32 = {}};
        color.float32[0] = (frame * 40 % 256) / 255.0f;
        color.float32[1] = 1.0f;
        color.float32[2] = 0.0f;
        color.float32[3] = 1.0f;
        VkCommandBuffer cb;
        VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                           nullptr, vk.pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1};
        DFN(AllocateCommandBuffers)(vk.device, &cai, &cb);
        VkCommandBufferBeginInfo bi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
        DFN(BeginCommandBuffer)(cb, &bi);
        Barrier(cb, images[i], VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED);
        VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
        DFN(CmdClearColorImage)(cb, images[i], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1,
                                &range);
        Barrier(cb, images[i], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                VK_IMAGE_LAYOUT_PRESENT_SRC_KHR, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED);
        DFN(EndCommandBuffer)(cb);
        VkPipelineStageFlags stage = VK_PIPELINE_STAGE_TRANSFER_BIT;
        VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO, nullptr, 1, &acquired, &stage, 1, &cb,
                           1, &rendered};
        VK(DFN(QueueSubmit)(vk.queue, 1, &si, VK_NULL_HANDLE));
        // vkQueuePresentKHR
        int release_fd = -1;
        VK(release(vk.queue, 1, &rendered, images[i], &release_fd));
        CHECK(release_fd >= 0, "no release fence");
        pollfd pfd = {release_fd, POLLIN, 0};
        CHECK(poll(&pfd, 1, 2000) == 1, "release fence never signalled");

        // SurfaceFlinger: the queued buffer as an AHB, sampled
        AhbImage sf;
        CHECK(sf.Import(bufs[i], VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT),
              "composer import of window buffer %d", i);
        Staging s;
        s.Init(4);
        Submit([&](VkCommandBuffer c) {
            Barrier(c, sf.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                    VK_QUEUE_FAMILY_FOREIGN_EXT, 0);
            VkBufferImageCopy r = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
                                   {static_cast<int32_t>(w - 1), static_cast<int32_t>(h - 1), 0},
                                   {1, 1, 1}};
            DFN(CmdCopyImageToBuffer)(c, sf.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, s.buffer,
                                      1, &r);
        });
        uint32_t px;
        memcpy(&px, s.data, 4);
        const uint32_t want = 0xff00ff00u | static_cast<uint32_t>(frame * 40 % 256);
        CHECK(px == want, "frame %d buffer %d: pixel %08x want %08x", frame, i, px, want);
        last_fence = release_fd;  // the composer hands it back with the buffer
        DFN(FreeCommandBuffers)(vk.device, vk.pool, 1, &cb);
    }
    if (last_fence >= 0)
        close(last_fence);
    printf("ok   swapchain: 3 window buffers (one bound late), 6 frames acquire/render/release, "
           "composer import sees each frame\n");
    DFN(DestroySemaphore)(vk.device, acquired, nullptr);
    DFN(DestroySemaphore)(vk.device, rendered, nullptr);
    DFN(DestroyFence)(vk.device, fence, nullptr);
    for (int i = 0; i < 3; i++) {
        DFN(DestroyImage)(vk.device, images[i], nullptr);
        nvvk::AhbRelease(bufs[i]);
    }
}

// HWUI-style export: memory allocated with the AHB export type, then the AHB taken out.
void TestExport() {
    VkExternalMemoryImageCreateInfo emi = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
                                           nullptr,
                                           VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
    VkImageCreateInfo ici = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, &emi, 0, VK_IMAGE_TYPE_2D,
                             VK_FORMAT_R8G8B8A8_UNORM, {256, 100, 1}, 1, 1, VK_SAMPLE_COUNT_1_BIT,
                             VK_IMAGE_TILING_OPTIMAL,
                             VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
                             VK_SHARING_MODE_EXCLUSIVE, 0, nullptr, VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage image;
    VK(DFN(CreateImage)(vk.device, &ici, nullptr, &image));
    VkMemoryDedicatedAllocateInfo ded = {VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO, nullptr,
                                         image, VK_NULL_HANDLE};
    VkExportMemoryAllocateInfo exp = {VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO, &ded,
                                      VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
    VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &exp, 0, 0};
    VkDeviceMemory mem;
    VK(DFN(AllocateMemory)(vk.device, &mai, nullptr, &mem));
    VK(DFN(BindImageMemory)(vk.device, image, mem, 0));
    VkMemoryGetAndroidHardwareBufferInfoANDROID gi = {
        VK_STRUCTURE_TYPE_MEMORY_GET_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, nullptr, mem};
    AHardwareBuffer* ahb = nullptr;
    VK(DFN(GetMemoryAndroidHardwareBufferANDROID)(vk.device, &gi, &ahb));
    CHECK(ahb && ahb->desc.width == 256 && ahb->desc.height == 100 && ahb->refs == 2,
          "exported AHB");
    nvvk::AhbRelease(ahb);
    DFN(DestroyImage)(vk.device, image, nullptr);
    DFN(FreeMemory)(vk.device, mem, nullptr);
    printf("ok   AHB export: dedicated image memory -> AHardwareBuffer 256x100\n");
}

// A blob AHB as a VkBuffer (linear system memory).
void TestBlob() {
    AHardwareBuffer* ahb = nullptr;
    CHECK(nvvk::AhbAllocate({65536, 1, 1, 0x21, kCpuWriteOften | kGpuData}, &ahb) == 0, "blob");
    CHECK(ahb->handle.format_modifier == 0, "blob not linear");
    uint32_t stride;
    void* map_data;
    auto* addr = static_cast<uint32_t*>(
        gbm_bo_map(ahb->bo, 0, 0, 65536, 1, GBM_BO_TRANSFER_WRITE, &stride, &map_data));
    for (int i = 0; i < 16384; i++)
        addr[i] = Pattern(i, 0, 99);
    gbm_bo_unmap(ahb->bo, map_data);

    VkAndroidHardwareBufferPropertiesANDROID props = {
        VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID};
    VK(DFN(GetAndroidHardwareBufferPropertiesANDROID)(vk.device, ahb, &props));
    VkExternalMemoryBufferCreateInfo emb = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO,
                                            nullptr,
                                            VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
    VkBufferCreateInfo bci = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, &emb, 0, 65536,
                              VK_BUFFER_USAGE_TRANSFER_SRC_BIT};
    VkBuffer buffer;
    VK(DFN(CreateBuffer)(vk.device, &bci, nullptr, &buffer));
    VkImportAndroidHardwareBufferInfoANDROID imp = {
        VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, nullptr, ahb};
    VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &imp, props.allocationSize,
                                static_cast<uint32_t>(__builtin_ctz(props.memoryTypeBits))};
    VkDeviceMemory mem;
    VK(DFN(AllocateMemory)(vk.device, &mai, nullptr, &mem));
    VK(DFN(BindBufferMemory)(vk.device, buffer, mem, 0));
    Staging s;
    s.Init(65536);
    Submit([&](VkCommandBuffer cb) {
        VkBufferCopy r = {0, 0, 65536};
        DFN(CmdCopyBuffer)(cb, buffer, s.buffer, 1, &r);
    });
    int bad = 0;
    for (int i = 0; i < 16384; i++)
        bad += reinterpret_cast<uint32_t*>(s.data)[i] != Pattern(i, 0, 99);
    DFN(DestroyBuffer)(vk.device, buffer, nullptr);
    DFN(FreeMemory)(vk.device, mem, nullptr);
    nvvk::AhbRelease(ahb);
    CHECK(!bad, "blob: %d bad words", bad);
    printf("ok   blob AHB 64 KiB as VkBuffer: CPU writes seen by the GPU\n");
}

}  // namespace

int main() {
    const char* node = getenv("NVVK_DRM_NODE");
    if (!node) {
        static char path[64];
        for (int i = 128; i < 136 && !node; i++) {
            snprintf(path, sizeof(path), "/sys/class/drm/renderD%d/device/driver", i);
            char link[256] = "";
            if (readlink(path, link, sizeof(link) - 1) > 0 && strstr(link, "/nvidia")) {
                snprintf(path, sizeof(path), "/dev/dri/renderD%d", i);
                node = path;
            }
        }
    }
    if (!node) {
        printf("no nvidia-drm render node\n");
        return 1;
    }
    setenv("ro.hardware.vulkan", "nvidia", 1);  // minigbm reads properties from the environment
    const int fd = open(node, O_RDWR | O_CLOEXEC);
    g_gbm = fd >= 0 ? gbm_create_device(fd) : nullptr;
    if (!g_gbm) {
        printf("gbm on %s failed\n", node);
        return 1;
    }
    printf("gralloc: minigbm backend %s on %s\n", gbm_device_get_backend_name(g_gbm), node);

    Setup();
    if (g_failures)
        return 1;
    TestSwapchain();
    TestExport();
    TestBlob();
    // CPU producers (bitmaps, software decoders, camera): system memory, block-linear
    TestCpuToGpu(1, 333, 77, "RGBA8888");
    TestCpuToGpu(1, 2560, 1080, "RGBA8888");
    TestCpuToGpu(4, 101, 50, "RGB565");
    TestCpuToGpu(0x16, 200, 129, "RGBA_FP16");
    TestCpuToGpu(0x2b, 640, 480, "RGBA_1010102");
    TestCpuToGpu(0x38, 77, 300, "R8");
    TestCpuToGpu(0x23, 1920, 1080, "YCbCr_420 (NV12)");
    TestCpuToGpu(0x23, 176, 144, "YCbCr_420 (NV12)");
    TestCpuToGpu(0x32315659, 320, 240, "YV12");  // Vulkan: _420 sizes are even
    TestCpuToGpu(0x36, 1280, 720, "P010");
    // CPU consumers (screenshots, ImageReader): GPU writes, mapper reads
    TestGpuToCpu(1, 2560, 1080, kCpuReadOften | kGpuFb, "RGBA8888 (system memory)");
    TestGpuToCpu(1, 300, 200, kGpuFb | kGpuSampled, "RGBA8888 (video memory)");
    TestGpuToCpu(0x23, 640, 360, kCpuReadOften | kGpuSampled, "NV12");
    TestGpuToCpu(0x32315659, 320, 240, kCpuReadOften | kGpuSampled, "YV12");

    printf("%s: %d failure(s)\n", g_failures ? "FAILED" : "PASSED", g_failures);
    return g_failures ? 1 : 0;
}
