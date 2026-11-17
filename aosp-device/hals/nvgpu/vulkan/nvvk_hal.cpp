/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// vulkan.nvidia: the hwvulkan module, NVIDIA's ICD, and the instance/device plumbing. The
// Android-specific entry points are in nvvk_wsi.cpp; see nvvk_priv.h for the design.

#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <unistd.h>
#include <string.h>

#include <algorithm>
#include <string>

#include <hardware/hwvulkan.h>
#include <vulkan/vk_icd.h>

#include "nvvk_priv.h"

namespace nvvk {

InstanceFns g_inst;

namespace {

// The ICD named by NVIDIA's nvidia_icd.json.
constexpr char kIcdLibrary[] = "libGLX_nvidia.so.0";

struct Icd {
    void* lib = nullptr;
    PFN_vkNegotiateLoaderICDInterfaceVersion negotiate = nullptr;
    PFN_vkGetInstanceProcAddr gipa = nullptr;  // vk_icdGetInstanceProcAddr
    uint32_t interface_version = 0;
    std::once_flag started_once;
    bool started = false;
};
Icd g_icd;

std::mutex g_lock;  // the maps below and g_inst
std::unordered_map<VkDevice, Device*> g_devices;
std::unordered_map<VkQueue, Device*> g_queues;
bool g_inst_resolved = false;
VkInstance g_first_instance = VK_NULL_HANDLE;  // for nvvk_next_instance_proc

// Instance extensions that need a window system or a display Android does not have, or that
// Android's loader implements itself; never shown to the loader.
const char* const kHiddenInstanceExtensions[] = {
    "VK_KHR_surface",
    "VK_KHR_display",
    "VK_KHR_get_display_properties2",
    "VK_KHR_get_surface_capabilities2",
    "VK_KHR_surface_protected_capabilities",
    "VK_KHR_surface_maintenance1",
    "VK_KHR_wayland_surface",
    "VK_KHR_xcb_surface",
    "VK_KHR_xlib_surface",
    "VK_KHR_portability_enumeration",
    "VK_EXT_acquire_drm_display",
    "VK_EXT_acquire_xlib_display",
    "VK_EXT_direct_mode_display",
    "VK_EXT_display_surface_counter",
    "VK_EXT_headless_surface",
    "VK_EXT_surface_maintenance1",
    "VK_EXT_swapchain_colorspace",
    "VK_LUNARG_direct_driver_loading",
    "VK_NV_display_stereo",
};

// Device extensions of NVIDIA's own swapchain and display paths; Android's loader provides the
// swapchain on top of VK_ANDROID_native_buffer instead.
const char* const kHiddenDeviceExtensions[] = {
    "VK_KHR_swapchain",
    "VK_KHR_display_swapchain",
    "VK_KHR_swapchain_mutable_format",
    "VK_KHR_swapchain_maintenance1",
    "VK_KHR_present_id",
    "VK_KHR_present_id2",
    "VK_KHR_present_wait",
    "VK_KHR_present_wait2",
    "VK_KHR_incremental_present",
    "VK_KHR_shared_presentable_image",
    "VK_EXT_swapchain_maintenance1",
    "VK_EXT_display_control",
    "VK_EXT_full_screen_exclusive",
    "VK_EXT_hdr_metadata",
    "VK_EXT_present_mode_fifo_latest_ready",
    "VK_EXT_present_timing",
    "VK_NV_present_barrier",
    "VK_NV_acquire_winrt_display",
    "VK_NV_low_latency2",
};

// What this module adds (VK_ANDROID_native_buffer: version 8, swapchain images may be bound
// later through vkBindImageMemory2).
const VkExtensionProperties kAndroidDeviceExtensions[] = {
    {"VK_ANDROID_native_buffer", 8},
    {"VK_ANDROID_external_memory_android_hardware_buffer", 5},
};

// NVIDIA extensions the Android paths need; enabled on every device.
const char* const kRequiredDeviceExtensions[] = {
    "VK_KHR_external_memory_fd",       "VK_EXT_external_memory_dma_buf",
    "VK_EXT_image_drm_format_modifier", "VK_KHR_image_format_list",
    "VK_KHR_external_semaphore_fd",    "VK_KHR_external_fence_fd",
    "VK_EXT_queue_family_foreign",     "VK_KHR_bind_memory2",
    "VK_KHR_dedicated_allocation",     "VK_KHR_get_memory_requirements2",
    "VK_KHR_sampler_ycbcr_conversion", "VK_KHR_maintenance1",
};

template <size_t N>
bool InList(const char* const (&list)[N], const char* name) {
    for (const char* entry : list)
        if (!strcmp(entry, name))
            return true;
    return false;
}

VkResult FilterExtensions(VkResult result, std::vector<VkExtensionProperties>& exts,
                          bool device, uint32_t* count, VkExtensionProperties* props) {
    if (result != VK_SUCCESS)
        return result;
    exts.erase(std::remove_if(exts.begin(), exts.end(),
                              [device](const VkExtensionProperties& e) {
                                  return device ? InList(kHiddenDeviceExtensions, e.extensionName)
                                                : InList(kHiddenInstanceExtensions, e.extensionName);
                              }),
               exts.end());
    if (device)
        exts.insert(exts.end(), std::begin(kAndroidDeviceExtensions),
                    std::end(kAndroidDeviceExtensions));
    if (!props) {
        *count = static_cast<uint32_t>(exts.size());
        return VK_SUCCESS;
    }
    const uint32_t n = std::min<uint32_t>(*count, static_cast<uint32_t>(exts.size()));
    std::copy_n(exts.begin(), n, props);
    *count = n;
    return n < exts.size() ? VK_INCOMPLETE : VK_SUCCESS;
}

// Open fds of NVIDIA's device nodes in this process.
int CountGpuFds() {
    int n = 0;
    if (DIR* dir = opendir("/proc/self/fd")) {
        while (dirent* e = readdir(dir)) {
            char path[64], target[128];
            snprintf(path, sizeof(path), "/proc/self/fd/%s", e->d_name);
            const ssize_t len = readlink(path, target, sizeof(target) - 1);
            if (len <= 0)
                continue;
            target[len] = 0;
            n += !strncmp(target, "/dev/nvidia", 11);
        }
        closedir(dir);
    }
    return n;
}

// Opening the module only loads NVIDIA's libraries: that opens no device. Zygote opens this HAL
// to preload it (Android's loader also enumerates the instance extensions then), and every fd
// zygote holds at fork must be on its allowlist; NVIDIA's ICD opens /dev/nvidiactl, /dev/nvidia0,
// /dev/nvidia-modeset and the DRM render node as soon as its interface is negotiated. So that
// happens in StartIcd(), at the first call that needs the driver, in the process that renders.
bool LoadIcd() {
    char err[512] = "";

    g_icd.lib = IcdOpen(kIcdLibrary, err, sizeof(err));
    if (!g_icd.lib) {
        NVVK_LOGE("cannot load NVIDIA's Vulkan driver %s: %s", kIcdLibrary, err);
        return false;
    }
    g_icd.negotiate = reinterpret_cast<PFN_vkNegotiateLoaderICDInterfaceVersion>(
        IcdSym(g_icd.lib, "vk_icdNegotiateLoaderICDInterfaceVersion"));
    g_icd.gipa = reinterpret_cast<PFN_vkGetInstanceProcAddr>(
        IcdSym(g_icd.lib, "vk_icdGetInstanceProcAddr"));
    if (!g_icd.negotiate || !g_icd.gipa) {
        NVVK_LOGE("%s has no vk_icdNegotiateLoaderICDInterfaceVersion/vk_icdGetInstanceProcAddr",
                  kIcdLibrary);
        return false;
    }
    if (int fds = CountGpuFds())
        NVVK_LOGE("loading %s opened %d /dev/nvidia* fds; a process that forks (zygote) aborts",
                  kIcdLibrary, fds);
    return true;
}

bool StartIcd() {
    std::call_once(g_icd.started_once, [] {
        // Version 5: dispatchable objects start with the loader magic, the loader owns no
        // surfaces (this module creates none), and apiVersion > 1.0 is fine.
        g_icd.interface_version = 5;
        VkResult result = g_icd.negotiate(&g_icd.interface_version);
        if (result != VK_SUCCESS || g_icd.interface_version < 2) {
            NVVK_LOGE("%s: ICD interface negotiation failed (%d, version %u)", kIcdLibrary, result,
                      g_icd.interface_version);
            return;
        }
        NVVK_LOGI("started %s (ICD interface %u)", kIcdLibrary, g_icd.interface_version);
        g_icd.started = true;
    });
    return g_icd.started;
}

PFN_vkVoidFunction NvGlobal(const char* name) {
    return g_icd.gipa(VK_NULL_HANDLE, name);
}

void ResolveInstanceFns(VkInstance instance) {
    std::lock_guard<std::mutex> guard(g_lock);
    if (g_inst_resolved)
        return;
#define NVVK_FN(name) \
    g_inst.name = reinterpret_cast<PFN_vk##name>(g_icd.gipa(instance, "vk" #name));
#include "nvvk_instance_fns.inc"
#undef NVVK_FN
    g_first_instance = instance;
    if (!g_inst.GetPhysicalDeviceProperties2)
        g_inst.GetPhysicalDeviceProperties2 = reinterpret_cast<PFN_vkGetPhysicalDeviceProperties2>(
            g_icd.gipa(instance, "vkGetPhysicalDeviceProperties2KHR"));
    g_inst_resolved = true;
}

// ---- global and instance entry points ----

// NVIDIA 620's instance extensions minus kHiddenInstanceExtensions: the answer before the driver
// has started (Android's loader asks when it opens the module, in zygote too).
const VkExtensionProperties kInstanceExtensions[] = {
    {"VK_EXT_debug_report", 10},
    {"VK_EXT_debug_utils", 2},
    {"VK_EXT_layer_settings", 2},
    {"VK_KHR_device_group_creation", 1},
    {"VK_KHR_external_fence_capabilities", 1},
    {"VK_KHR_external_memory_capabilities", 1},
    {"VK_KHR_external_semaphore_capabilities", 1},
    {"VK_KHR_get_physical_device_properties2", 2},
};

std::vector<VkExtensionProperties> DriverInstanceExtensions() {
    auto fn = reinterpret_cast<PFN_vkEnumerateInstanceExtensionProperties>(
        NvGlobal("vkEnumerateInstanceExtensionProperties"));
    uint32_t n = 0;
    std::vector<VkExtensionProperties> exts;
    if (fn(nullptr, &n, nullptr) == VK_SUCCESS) {
        exts.resize(n);
        if (fn(nullptr, &n, exts.data()) != VK_SUCCESS)
            n = 0;
    }
    exts.resize(n);
    return exts;
}

VKAPI_ATTR VkResult VKAPI_CALL EnumerateInstanceExtensionProperties(const char* layer,
                                                                    uint32_t* count,
                                                                    VkExtensionProperties* props) {
    if (layer) {
        *count = 0;
        return VK_SUCCESS;
    }
    std::vector<VkExtensionProperties> exts;
    if (g_icd.started)
        exts = DriverInstanceExtensions();
    else
        exts.assign(std::begin(kInstanceExtensions), std::end(kInstanceExtensions));
    return FilterExtensions(VK_SUCCESS, exts, false, count, props);
}

VKAPI_ATTR VkResult VKAPI_CALL CreateInstance(const VkInstanceCreateInfo* info,
                                              const VkAllocationCallbacks* allocator,
                                              VkInstance* instance) {
    if (!StartIcd())
        return VK_ERROR_INITIALIZATION_FAILED;
    // The loader passes what EnumerateInstanceExtensionProperties listed, possibly the static
    // list: keep what the driver really has.
    const std::vector<VkExtensionProperties> have = DriverInstanceExtensions();
    std::vector<const char*> names;
    for (uint32_t i = 0; i < info->enabledExtensionCount; i++) {
        const char* name = info->ppEnabledExtensionNames[i];
        bool found = false;
        for (const auto& e : have)
            found = found || !strcmp(e.extensionName, name);
        if (found)
            names.push_back(name);
        else
            NVVK_LOGW("instance extension %s not in NVIDIA's driver; left out", name);
    }
    VkInstanceCreateInfo nv = *info;
    nv.enabledExtensionCount = static_cast<uint32_t>(names.size());
    nv.ppEnabledExtensionNames = names.data();
    auto fn = reinterpret_cast<PFN_vkCreateInstance>(NvGlobal("vkCreateInstance"));
    VkResult result = fn(&nv, allocator, instance);
    if (result != VK_SUCCESS) {
        NVVK_LOGE("NVIDIA vkCreateInstance failed: %d", result);
        return result;
    }
    ResolveInstanceFns(*instance);
    return VK_SUCCESS;
}

VKAPI_ATTR VkResult VKAPI_CALL EnumerateDeviceExtensionProperties(VkPhysicalDevice physical,
                                                                  const char* layer,
                                                                  uint32_t* count,
                                                                  VkExtensionProperties* props) {
    if (layer) {
        *count = 0;
        return VK_SUCCESS;
    }
    uint32_t n = 0;
    VkResult result = g_inst.EnumerateDeviceExtensionProperties(physical, nullptr, &n, nullptr);
    std::vector<VkExtensionProperties> exts(n);
    if (result == VK_SUCCESS)
        result = g_inst.EnumerateDeviceExtensionProperties(physical, nullptr, &n, exts.data());
    exts.resize(n);
    return FilterExtensions(result, exts, true, count, props);
}

// The queue global priority Android asks for (SurfaceFlinger and HWUI: REALTIME/HIGH) is
// capped at MEDIUM unless ro.boot.android_pc.nvprio=1: Phase K saw the GPU wedge with
// Xid 51 (BAD_TSG) right after a high-priority channel came up (docs/nvidia-native.md).
void ClampQueuePriorities(VkDeviceCreateInfo* info,
                          std::vector<VkDeviceQueueCreateInfo>& queues,
                          std::vector<VkDeviceQueueGlobalPriorityCreateInfoKHR>& prios) {
    if (GetPropertyInt("ro.boot.android_pc.nvprio", 0))
        return;
    queues.assign(info->pQueueCreateInfos, info->pQueueCreateInfos + info->queueCreateInfoCount);
    prios.reserve(queues.size());  // no reallocation: q.pNext points into it
    bool clamped = false;
    for (auto& q : queues) {
        for (auto* s = static_cast<const VkBaseInStructure*>(q.pNext); s; s = s->pNext) {
            if (s->sType != VK_STRUCTURE_TYPE_DEVICE_QUEUE_GLOBAL_PRIORITY_CREATE_INFO_KHR)
                continue;
            auto* p = reinterpret_cast<const VkDeviceQueueGlobalPriorityCreateInfoKHR*>(s);
            if (p->globalPriority <= VK_QUEUE_GLOBAL_PRIORITY_MEDIUM_KHR)
                break;
            if (s != q.pNext) {
                // The chain is the caller's (const): only a struct at its head can be replaced.
                NVVK_LOGW("queue global priority %d left as is (not first in the chain)",
                          p->globalPriority);
                break;
            }
            prios.push_back(*p);
            prios.back().globalPriority = VK_QUEUE_GLOBAL_PRIORITY_MEDIUM_KHR;
            q.pNext = &prios.back();
            clamped = true;
            break;
        }
    }
    if (clamped) {
        info->pQueueCreateInfos = queues.data();
        NVVK_LOGI("queue global priority capped at MEDIUM (ro.boot.android_pc.nvprio=1 lifts it)");
    }
}

VKAPI_ATTR VkResult VKAPI_CALL CreateDevice(VkPhysicalDevice physical,
                                            const VkDeviceCreateInfo* info,
                                            const VkAllocationCallbacks* allocator,
                                            VkDevice* out) {
    // The loader passes VK_ANDROID_native_buffer (for VK_KHR_swapchain) and the AHB extension;
    // NVIDIA knows neither. Enable the NVIDIA extensions they are built on instead.
    std::vector<const char*> names;
    for (uint32_t i = 0; i < info->enabledExtensionCount; i++) {
        const char* name = info->ppEnabledExtensionNames[i];
        if (strcmp(name, "VK_ANDROID_native_buffer") &&
            strcmp(name, "VK_ANDROID_external_memory_android_hardware_buffer"))
            names.push_back(name);
    }
    uint32_t n = 0;
    g_inst.EnumerateDeviceExtensionProperties(physical, nullptr, &n, nullptr);
    std::vector<VkExtensionProperties> available(n);
    g_inst.EnumerateDeviceExtensionProperties(physical, nullptr, &n, available.data());
    for (const char* req : kRequiredDeviceExtensions) {
        bool have = false, supported = false;
        for (const char* name : names)
            have = have || !strcmp(name, req);
        for (uint32_t i = 0; i < n; i++)
            supported = supported || !strcmp(available[i].extensionName, req);
        if (!supported) {
            NVVK_LOGE("NVIDIA's driver lacks %s, which the Android paths need", req);
            return VK_ERROR_EXTENSION_NOT_PRESENT;
        }
        if (!have)
            names.push_back(req);
    }
    VkDeviceCreateInfo nv = *info;
    nv.enabledExtensionCount = static_cast<uint32_t>(names.size());
    nv.ppEnabledExtensionNames = names.data();
    std::vector<VkDeviceQueueCreateInfo> queues;
    std::vector<VkDeviceQueueGlobalPriorityCreateInfoKHR> prios;
    ClampQueuePriorities(&nv, queues, prios);
    // textureCompressionASTC_LDR is the ASTC emulation's (astc/lib.rs), not NVIDIA's: never ask
    // the driver for it. The VkPhysicalDeviceFeatures2 in the chain is the app's and const, so it
    // is cleared for the call and put back.
    VkPhysicalDeviceFeatures features;
    if (info->pEnabledFeatures) {
        features = *info->pEnabledFeatures;
        features.textureCompressionASTC_LDR = VK_FALSE;
        nv.pEnabledFeatures = &features;
    }
    VkPhysicalDeviceFeatures2* features2 = nullptr;
    for (auto* s = static_cast<const VkBaseInStructure*>(info->pNext); s; s = s->pNext)
        if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2)
            features2 = const_cast<VkPhysicalDeviceFeatures2*>(
                reinterpret_cast<const VkPhysicalDeviceFeatures2*>(s));
    const VkBool32 astc = features2 ? features2->features.textureCompressionASTC_LDR : VK_FALSE;
    if (features2)
        features2->features.textureCompressionASTC_LDR = VK_FALSE;

    VkResult result = g_inst.CreateDevice(physical, &nv, allocator, out);
    if (features2)
        features2->features.textureCompressionASTC_LDR = astc;
    if (result != VK_SUCCESS) {
        NVVK_LOGE("NVIDIA vkCreateDevice failed: %d", result);
        return result;
    }

    auto* dev = new Device();
    dev->handle = *out;
    dev->physical = physical;
#define NVVK_FN(name) \
    dev->fn.name = reinterpret_cast<PFN_vk##name>(g_inst.GetDeviceProcAddr(*out, "vk" #name));
#include "nvvk_device_fns.inc"
#undef NVVK_FN
    g_inst.GetPhysicalDeviceMemoryProperties(physical, &dev->memory_props);
    static std::once_flag logged;
    std::call_once(logged, [physical] {
        VkPhysicalDeviceDriverProperties driver = {
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
        VkPhysicalDeviceProperties2 props = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
                                             &driver};
        g_inst.GetPhysicalDeviceProperties2(physical, &props);
        NVVK_LOGI("device %s, driver %s %s, Vulkan %u.%u.%u", props.properties.deviceName,
                  driver.driverName, driver.driverInfo,
                  VK_API_VERSION_MAJOR(props.properties.apiVersion),
                  VK_API_VERSION_MINOR(props.properties.apiVersion),
                  VK_API_VERSION_PATCH(props.properties.apiVersion));
    });
    {
        std::lock_guard<std::mutex> guard(g_lock);
        g_devices[*out] = dev;
    }
    return VK_SUCCESS;
}

VKAPI_ATTR void VKAPI_CALL DestroyDevice(VkDevice device, const VkAllocationCallbacks* allocator) {
    if (!device)
        return;
    Device* dev = nullptr;
    {
        std::lock_guard<std::mutex> guard(g_lock);
        auto it = g_devices.find(device);
        if (it != g_devices.end()) {
            dev = it->second;
            g_devices.erase(it);
        }
        for (auto q = g_queues.begin(); q != g_queues.end();)
            q = q->second == dev ? g_queues.erase(q) : std::next(q);
    }
    if (!dev)
        return;
    for (auto& [queue, sem] : dev->release_semaphores)
        dev->fn.DestroySemaphore(device, sem, nullptr);
    for (auto& [mem, ahb] : dev->ahbs)
        AhbRelease(ahb);
    dev->fn.DestroyDevice(device, allocator);
    delete dev;
}

void RememberQueue(VkDevice device, VkQueue queue) {
    std::lock_guard<std::mutex> guard(g_lock);
    auto it = g_devices.find(device);
    if (it != g_devices.end() && queue)
        g_queues[queue] = it->second;
}

VKAPI_ATTR void VKAPI_CALL GetDeviceQueue(VkDevice device, uint32_t family, uint32_t index,
                                          VkQueue* queue) {
    GetDevice(device)->fn.GetDeviceQueue(device, family, index, queue);
    RememberQueue(device, *queue);
}

VKAPI_ATTR void VKAPI_CALL GetDeviceQueue2(VkDevice device, const VkDeviceQueueInfo2* info,
                                           VkQueue* queue) {
    GetDevice(device)->fn.GetDeviceQueue2(device, info, queue);
    RememberQueue(device, *queue);
}

struct Hook {
    const char* name;
    PFN_vkVoidFunction fn;
};

#define NVVK_HOOK(name, fn) {"vk" #name, reinterpret_cast<PFN_vkVoidFunction>(fn)}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL GetDeviceProcAddr(VkDevice device, const char* name);

const Hook kDeviceHooks[] = {
    NVVK_HOOK(GetDeviceProcAddr, GetDeviceProcAddr),
    NVVK_HOOK(DestroyDevice, DestroyDevice),
    NVVK_HOOK(GetDeviceQueue, GetDeviceQueue),
    NVVK_HOOK(GetDeviceQueue2, GetDeviceQueue2),
    NVVK_HOOK(CreateImage, CreateImage),
    NVVK_HOOK(DestroyImage, DestroyImage),
    NVVK_HOOK(BindImageMemory2, BindImageMemory2),
    NVVK_HOOK(BindImageMemory2KHR, BindImageMemory2),
    NVVK_HOOK(CreateImageView, CreateImageView),
    NVVK_HOOK(CreateBuffer, CreateBuffer),
    NVVK_HOOK(AllocateMemory, AllocateMemory),
    NVVK_HOOK(FreeMemory, FreeMemory),
    NVVK_HOOK(CreateSamplerYcbcrConversion, CreateSamplerYcbcrConversion),
    NVVK_HOOK(CreateSamplerYcbcrConversionKHR, CreateSamplerYcbcrConversion),
    NVVK_HOOK(GetDeviceImageMemoryRequirements, GetDeviceImageMemoryRequirements),
    NVVK_HOOK(GetDeviceImageMemoryRequirementsKHR, GetDeviceImageMemoryRequirements),
    NVVK_HOOK(GetAndroidHardwareBufferPropertiesANDROID, GetAndroidHardwareBufferPropertiesANDROID),
    NVVK_HOOK(GetMemoryAndroidHardwareBufferANDROID, GetMemoryAndroidHardwareBufferANDROID),
    NVVK_HOOK(GetSwapchainGrallocUsageANDROID, GetSwapchainGrallocUsageANDROID),
    NVVK_HOOK(GetSwapchainGrallocUsage2ANDROID, GetSwapchainGrallocUsage2ANDROID),
    NVVK_HOOK(GetSwapchainGrallocUsage3ANDROID, GetSwapchainGrallocUsage3ANDROID),
    NVVK_HOOK(GetSwapchainGrallocUsage4ANDROID, GetSwapchainGrallocUsage4ANDROID),
    NVVK_HOOK(AcquireImageANDROID, AcquireImageANDROID),
    NVVK_HOOK(QueueSignalReleaseImageANDROID, QueueSignalReleaseImageANDROID),
};

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL GetInstanceProcAddr(VkInstance instance, const char* name);

const Hook kInstanceHooks[] = {
    NVVK_HOOK(GetInstanceProcAddr, GetInstanceProcAddr),
    NVVK_HOOK(EnumerateInstanceExtensionProperties, EnumerateInstanceExtensionProperties),
    NVVK_HOOK(CreateInstance, CreateInstance),
    NVVK_HOOK(EnumerateDeviceExtensionProperties, EnumerateDeviceExtensionProperties),
    NVVK_HOOK(CreateDevice, CreateDevice),
    NVVK_HOOK(GetPhysicalDeviceProperties2, GetPhysicalDeviceProperties2),
    NVVK_HOOK(GetPhysicalDeviceProperties2KHR, GetPhysicalDeviceProperties2),
    NVVK_HOOK(GetPhysicalDeviceImageFormatProperties2, GetPhysicalDeviceImageFormatProperties2),
    NVVK_HOOK(GetPhysicalDeviceImageFormatProperties2KHR, GetPhysicalDeviceImageFormatProperties2),
    NVVK_HOOK(GetPhysicalDeviceExternalBufferProperties, GetPhysicalDeviceExternalBufferProperties),
    NVVK_HOOK(GetPhysicalDeviceExternalBufferPropertiesKHR,
              GetPhysicalDeviceExternalBufferProperties),
};

template <size_t N>
PFN_vkVoidFunction FindHook(const Hook (&hooks)[N], const char* name) {
    for (const Hook& h : hooks)
        if (!strcmp(h.name, name))
            return h.fn;
    return nullptr;
}

// The entry points below the ASTC emulation (astc/lib.rs): this module's own, or NVIDIA's.
PFN_vkVoidFunction NextDeviceProc(VkDevice device, const char* name) {
    if (PFN_vkVoidFunction fn = FindHook(kDeviceHooks, name))
        return fn;
    return g_inst.GetDeviceProcAddr(device, name);
}

PFN_vkVoidFunction NextInstanceProc(VkInstance instance, const char* name) {
    if (PFN_vkVoidFunction fn = FindHook(kInstanceHooks, name))
        return fn;
    if (instance)
        if (PFN_vkVoidFunction fn = FindHook(kDeviceHooks, name))
            return fn;
    if (!StartIcd())
        return nullptr;
    return g_icd.gipa(instance, name);
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL GetDeviceProcAddr(VkDevice device, const char* name) {
    if (PFN_vkVoidFunction fn = nvvk_astc_proc(name))
        return fn;
    return NextDeviceProc(device, name);
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL GetInstanceProcAddr(VkInstance instance, const char* name) {
    if (instance)
        if (PFN_vkVoidFunction fn = nvvk_astc_proc(name))
            return fn;
    return NextInstanceProc(instance, name);
}

// ---- the hwvulkan module ----

int CloseDevice(struct hw_device_t*) {
    return 0;
}

hwvulkan_device_t g_hal_device;

int OpenDevice(const hw_module_t* module, const char* id, hw_device_t** device) {
    if (strcmp(id, HWVULKAN_DEVICE_0) != 0)
        return -ENOENT;
    static const bool loaded = LoadIcd();
    if (!loaded)
        return -ENODEV;
    g_hal_device.common.tag = HARDWARE_DEVICE_TAG;
    g_hal_device.common.version = HWVULKAN_DEVICE_API_VERSION_0_1;
    g_hal_device.common.module = const_cast<hw_module_t*>(module);
    g_hal_device.common.close = CloseDevice;
    g_hal_device.EnumerateInstanceExtensionProperties = EnumerateInstanceExtensionProperties;
    g_hal_device.CreateInstance = CreateInstance;
    g_hal_device.GetInstanceProcAddr = GetInstanceProcAddr;
    *device = &g_hal_device.common;
    return 0;
}

hw_module_methods_t g_module_methods = {.open = OpenDevice};

}  // namespace

Device* GetDevice(VkDevice device) {
    std::lock_guard<std::mutex> guard(g_lock);
    auto it = g_devices.find(device);
    return it == g_devices.end() ? nullptr : it->second;
}

Device* GetDeviceForQueue(VkQueue queue) {
    std::lock_guard<std::mutex> guard(g_lock);
    auto it = g_queues.find(queue);
    return it == g_queues.end() ? nullptr : it->second;
}

}  // namespace nvvk

// ---- for the ASTC emulation (astc/lib.rs) ----

extern "C" const void* nvvk_next_device_proc(VkDevice device, const char* name) {
    return reinterpret_cast<const void*>(nvvk::NextDeviceProc(device, name));
}

extern "C" const void* nvvk_next_instance_proc(const char* name) {
    return reinterpret_cast<const void*>(nvvk::NextInstanceProc(nvvk::g_first_instance, name));
}

extern "C" VkDevice nvvk_queue_device(VkQueue queue) {
    nvvk::Device* dev = nvvk::GetDeviceForQueue(queue);
    return dev ? dev->handle : VK_NULL_HANDLE;
}

extern "C" bool nvvk_device_memory_properties(VkDevice device,
                                              VkPhysicalDeviceMemoryProperties* props) {
    nvvk::Device* dev = nvvk::GetDevice(device);
    if (!dev)
        return false;
    *props = dev->memory_props;
    return true;
}

extern "C" void nvvk_log(int level, const char* msg) {
    nvvk::Log(level == 0 ? nvvk::kLogError : level == 1 ? nvvk::kLogWarn : nvvk::kLogInfo, "%s",
              msg);
}

extern "C" {
__attribute__((visibility("default"))) hwvulkan_module_t HAL_MODULE_INFO_SYM = {
    .common =
        {
            .tag = HARDWARE_MODULE_TAG,
            .module_api_version = HWVULKAN_MODULE_API_VERSION_0_1,
            .hal_api_version = HARDWARE_HAL_API_VERSION,
            .id = HWVULKAN_HARDWARE_MODULE_ID,
            .name = "NVIDIA Vulkan (android-pc)",
            .author = "android-pc",
            .methods = &nvvk::g_module_methods,
        },
};
}
