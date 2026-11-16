// Phase L2 (docs/nvidia-native.md): drives NVIDIA's Vulkan ICD directly, without a Vulkan
// loader, the way an Android Vulkan HAL would: vkCreateInstance, vkEnumeratePhysicalDevices,
// vkCreateDevice and a compute dispatch that fills a buffer, checked on the CPU.
//
// Built twice from this file: for bionic (Soong, `nvglibc_vktest`), where the glibc ICD is loaded
// through nvglibc, and for glibc on the build host (tools/nvglibc-hosttest.sh), as the baseline.
//
// Usage: nvglibc_vktest [icd]   (icd defaults to libGLX_nvidia.so.0)
// Exit status 0 only when the dispatch wrote the expected values.

#include <dlfcn.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>

#ifdef __BIONIC__
#include "nvglibc.h"
#define OPEN_ICD(name) nvglibc_dlopen(name, RTLD_NOW | RTLD_LOCAL)
#define OPEN_ERROR() nvglibc_dlerror()
#define ABI "bionic+nvglibc"
#else
#define OPEN_ICD(name) dlopen(name, RTLD_NOW | RTLD_LOCAL)
#define OPEN_ERROR() dlerror()
#define ABI "glibc"
#endif

#define TAG "nvglibc_vktest: "
#define N_ELEMS 65536u

static const uint32_t kFillSpv[] = {
#include "fill.spv.inc"
};

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

#define CHECK(call)                                                       \
    do {                                                                  \
        VkResult r_ = (call);                                             \
        if (r_ != VK_SUCCESS) {                                           \
            fprintf(stderr, TAG "FAIL %s = %d (line %d)\n", #call, r_, __LINE__); \
            return 1;                                                     \
        }                                                                 \
    } while (0)

#define LOAD_I(inst, fn)                                                  \
    PFN_##fn fn = (PFN_##fn)gipa(inst, #fn);                              \
    if (!fn) {                                                            \
        fprintf(stderr, TAG "FAIL no %s\n", #fn);                         \
        return 1;                                                         \
    }

#define LOAD_D(dev, fn)                                                   \
    PFN_##fn fn = (PFN_##fn)vkGetDeviceProcAddr(dev, #fn);                \
    if (!fn) {                                                            \
        fprintf(stderr, TAG "FAIL no %s\n", #fn);                         \
        return 1;                                                         \
    }

int main(int argc, char** argv) {
    const char* icd = argc > 1 ? argv[1] : "libGLX_nvidia.so.0";
    double t0 = now_ms();

    printf(TAG "ABI %s, loading %s\n", ABI, icd);
    fflush(stdout);
    void* h = OPEN_ICD(icd);
    if (!h) {
        fprintf(stderr, TAG "FAIL load %s: %s\n", icd, OPEN_ERROR());
        return 1;
    }
    double t_load = now_ms();

    typedef VkResult (*PFN_negotiate)(uint32_t*);
    PFN_negotiate negotiate = (PFN_negotiate)dlsym(h, "vk_icdNegotiateLoaderICDInterfaceVersion");
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(h, "vk_icdGetInstanceProcAddr");
    if (!negotiate || !gipa) {
        fprintf(stderr, TAG "FAIL %s has no ICD entry points\n", icd);
        return 1;
    }
    uint32_t icd_version = 5;
    CHECK(negotiate(&icd_version));

    LOAD_I(NULL, vkCreateInstance);
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "nvglibc_vktest",
        .apiVersion = VK_API_VERSION_1_3,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkInstance inst;
    CHECK(vkCreateInstance(&ici, NULL, &inst));

    LOAD_I(inst, vkEnumeratePhysicalDevices);
    LOAD_I(inst, vkGetPhysicalDeviceProperties2);
    LOAD_I(inst, vkGetPhysicalDeviceQueueFamilyProperties);
    LOAD_I(inst, vkGetPhysicalDeviceMemoryProperties);
    LOAD_I(inst, vkCreateDevice);
    LOAD_I(inst, vkGetDeviceProcAddr);
    LOAD_I(inst, vkDestroyInstance);

    uint32_t n_phys = 8;
    VkPhysicalDevice phys[8];
    CHECK(vkEnumeratePhysicalDevices(inst, &n_phys, phys));
    if (n_phys == 0) {
        fprintf(stderr, TAG "FAIL no physical device\n");
        return 1;
    }
    for (uint32_t i = 0; i < n_phys; i++) {
        VkPhysicalDeviceDriverProperties drv = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES,
        };
        VkPhysicalDeviceProperties2 p2 = {
            .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
            .pNext = &drv,
        };
        vkGetPhysicalDeviceProperties2(phys[i], &p2);
        const VkPhysicalDeviceProperties* p = &p2.properties;
        printf(TAG "device %u: %s (%04x:%04x) api %u.%u.%u driver %s %s (0x%08x)\n", i,
               p->deviceName, p->vendorID, p->deviceID, VK_API_VERSION_MAJOR(p->apiVersion),
               VK_API_VERSION_MINOR(p->apiVersion), VK_API_VERSION_PATCH(p->apiVersion),
               drv.driverName, drv.driverInfo, p->driverVersion);
    }
    VkPhysicalDevice pd = phys[0];

    uint32_t n_qf = 16;
    VkQueueFamilyProperties qf[16];
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &n_qf, qf);
    uint32_t qfi = UINT32_MAX;
    for (uint32_t i = 0; i < n_qf; i++) {
        if (qf[i].queueFlags & VK_QUEUE_COMPUTE_BIT) {
            qfi = i;
            break;
        }
    }
    if (qfi == UINT32_MAX) {
        fprintf(stderr, TAG "FAIL no compute queue\n");
        return 1;
    }

    float prio = 1.0f;
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qfi,
        .queueCount = 1,
        .pQueuePriorities = &prio,
    };
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &qci,
    };
    VkDevice dev;
    CHECK(vkCreateDevice(pd, &dci, NULL, &dev));
    printf(TAG "vkCreateDevice OK (queue family %u)\n", qfi);

    LOAD_D(dev, vkGetDeviceQueue);
    LOAD_D(dev, vkCreateBuffer);
    LOAD_D(dev, vkGetBufferMemoryRequirements);
    LOAD_D(dev, vkAllocateMemory);
    LOAD_D(dev, vkBindBufferMemory);
    LOAD_D(dev, vkMapMemory);
    LOAD_D(dev, vkCreateShaderModule);
    LOAD_D(dev, vkCreateDescriptorSetLayout);
    LOAD_D(dev, vkCreatePipelineLayout);
    LOAD_D(dev, vkCreateComputePipelines);
    LOAD_D(dev, vkCreateDescriptorPool);
    LOAD_D(dev, vkAllocateDescriptorSets);
    LOAD_D(dev, vkUpdateDescriptorSets);
    LOAD_D(dev, vkCreateCommandPool);
    LOAD_D(dev, vkAllocateCommandBuffers);
    LOAD_D(dev, vkBeginCommandBuffer);
    LOAD_D(dev, vkCmdBindPipeline);
    LOAD_D(dev, vkCmdBindDescriptorSets);
    LOAD_D(dev, vkCmdDispatch);
    LOAD_D(dev, vkEndCommandBuffer);
    LOAD_D(dev, vkCreateFence);
    LOAD_D(dev, vkQueueSubmit);
    LOAD_D(dev, vkWaitForFences);
    LOAD_D(dev, vkDeviceWaitIdle);
    LOAD_D(dev, vkDestroyDevice);

    VkQueue queue;
    vkGetDeviceQueue(dev, qfi, 0, &queue);

    const VkDeviceSize size = N_ELEMS * sizeof(uint32_t);
    VkBufferCreateInfo bci = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size = size,
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bci, NULL, &buf));
    VkMemoryRequirements mr;
    vkGetBufferMemoryRequirements(dev, buf, &mr);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    const VkMemoryPropertyFlags want =
        VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    uint32_t mti = UINT32_MAX;
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((mr.memoryTypeBits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) {
            mti = i;
            break;
        }
    }
    if (mti == UINT32_MAX) {
        fprintf(stderr, TAG "FAIL no host-visible memory type\n");
        return 1;
    }
    VkMemoryAllocateInfo mai = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = mr.size,
        .memoryTypeIndex = mti,
    };
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mai, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    uint32_t* data;
    CHECK(vkMapMemory(dev, mem, 0, size, 0, (void**)&data));
    memset(data, 0xff, size);

    VkShaderModuleCreateInfo smci = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = sizeof(kFillSpv),
        .pCode = kFillSpv,
    };
    VkShaderModule sm;
    CHECK(vkCreateShaderModule(dev, &smci, NULL, &sm));

    VkDescriptorSetLayoutBinding dslb = {
        .binding = 0,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1,
        .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
    };
    VkDescriptorSetLayoutCreateInfo dslci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 1,
        .pBindings = &dslb,
    };
    VkDescriptorSetLayout dsl;
    CHECK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl));
    VkPipelineLayoutCreateInfo plci = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1,
        .pSetLayouts = &dsl,
    };
    VkPipelineLayout pl;
    CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &pl));
    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {
            .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
            .stage = VK_SHADER_STAGE_COMPUTE_BIT,
            .module = sm,
            .pName = "main",
        },
        .layout = pl,
    };
    VkPipeline pipe;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe));

    VkDescriptorPoolSize dps = {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1};
    VkDescriptorPoolCreateInfo dpci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 1,
        .poolSizeCount = 1,
        .pPoolSizes = &dps,
    };
    VkDescriptorPool dp;
    CHECK(vkCreateDescriptorPool(dev, &dpci, NULL, &dp));
    VkDescriptorSetAllocateInfo dsai = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = dp,
        .descriptorSetCount = 1,
        .pSetLayouts = &dsl,
    };
    VkDescriptorSet ds;
    CHECK(vkAllocateDescriptorSets(dev, &dsai, &ds));
    VkDescriptorBufferInfo dbi = {buf, 0, VK_WHOLE_SIZE};
    VkWriteDescriptorSet wds = {
        .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
        .dstSet = ds,
        .descriptorCount = 1,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .pBufferInfo = &dbi,
    };
    vkUpdateDescriptorSets(dev, 1, &wds, 0, NULL);

    VkCommandPoolCreateInfo cpi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = qfi,
    };
    VkCommandPool cp;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cp));
    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = cp,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    VkCommandBuffer cb;
    CHECK(vkAllocateCommandBuffers(dev, &cbai, &cb));
    VkCommandBufferBeginInfo cbbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    CHECK(vkBeginCommandBuffer(cb, &cbbi));
    vkCmdBindPipeline(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
    vkCmdBindDescriptorSets(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1, &ds, 0, NULL);
    vkCmdDispatch(cb, N_ELEMS / 64, 1, 1);
    CHECK(vkEndCommandBuffer(cb));

    VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    CHECK(vkCreateFence(dev, &fci, NULL, &fence));
    VkSubmitInfo si = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers = &cb,
    };
    double t_submit = now_ms();
    CHECK(vkQueueSubmit(queue, 1, &si, fence));
    CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, 5ull * 1000 * 1000 * 1000));
    double t_done = now_ms();

    uint32_t ok = 0;
    for (uint32_t i = 0; i < N_ELEMS; i++) {
        if (data[i] == i * i + 7u) ok++;
    }
    printf(TAG "compute dispatch: %u/%u values correct (gpu %.2f ms)\n", ok, N_ELEMS,
           t_done - t_submit);

    vkDeviceWaitIdle(dev);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    printf(TAG "%s (load %.0f ms, total %.0f ms)\n", ok == N_ELEMS ? "PASS" : "FAIL",
           t_load - t0, now_ms() - t0);
    return ok == N_ELEMS ? 0 : 1;
}
