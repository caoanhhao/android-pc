/*
 * pcnvtest: a small Vulkan client for NVIDIA's own driver (Phase L1, docs/nvidia-proprietary.md).
 *
 * Built for glibc on the build host (tools/nvtest-build.sh) and run through the sidecar's glibc
 * root (/vendor/bin/nvsidecar: ld-linux, libvulkan, NVIDIA's ICD) by pcnvtest.sh. It isolates the
 * Xid 51 (BAD_TSG) seen on the real PC: each test creates its own VkDevice (so its own channels
 * and TSG), fills a buffer with vkCmdFillBuffer, copies it with vkCmdCopyBuffer, runs a compute
 * shader over it, waits on a fence with a timeout and checks the result.
 *
 *   pcnvtest info                  instance, physical device, queue families, global priority
 *   pcnvtest copy [PRIO]           one device; PRIO: default|low|medium|high|realtime
 *   pcnvtest pair [N]              two threads, each with its own device at once, N rounds
 *                                  (the pattern of microG's DroidGuard that preceded the Xid)
 *   pcnvtest many [N]              N devices created and destroyed one after another
 *
 * Exit status 0 when every check passed. One line per result, prefixed "pcnvtest:".
 */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <vulkan/vulkan.h>

#include "fill_comp.h" /* fill_comp_spv[]: glslc of fill.comp (tools/nvtest-build.sh) */

#define N_WORDS (4u << 20) /* 16 MiB per buffer */
#define FENCE_TIMEOUT_NS (5ull * 1000 * 1000 * 1000)

static VkInstance instance;
static VkPhysicalDevice phys;
static uint32_t qfam;
static int has_prio_ext;
static const char *prio_ext_name;

#define say(...) do { printf("pcnvtest: " __VA_ARGS__); printf("\n"); fflush(stdout); } while (0)
#define CHECK(call) do { VkResult r_ = (call); if (r_ != VK_SUCCESS) { \
    say("FAIL %s: %s = %d", tag, #call, r_); goto out; } } while (0)

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static int setup_instance(void) {
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "pcnvtest",
        .apiVersion = VK_API_VERSION_1_3,
    };
    VkInstanceCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &app,
    };
    VkResult r = vkCreateInstance(&ici, NULL, &instance);
    if (r != VK_SUCCESS) { say("FAIL vkCreateInstance = %d", r); return -1; }
    uint32_t n = 0;
    vkEnumeratePhysicalDevices(instance, &n, NULL);
    if (n == 0) { say("FAIL no physical device"); return -1; }
    VkPhysicalDevice devs[8];
    if (n > 8) n = 8;
    vkEnumeratePhysicalDevices(instance, &n, devs);
    phys = devs[0];
    uint32_t nq = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(phys, &nq, NULL);
    VkQueueFamilyProperties qp[16];
    if (nq > 16) nq = 16;
    vkGetPhysicalDeviceQueueFamilyProperties(phys, &nq, qp);
    qfam = UINT32_MAX;
    for (uint32_t i = 0; i < nq; i++) {
        if ((qp[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) && (qp[i].queueFlags & VK_QUEUE_COMPUTE_BIT)) {
            qfam = i;
            break;
        }
    }
    if (qfam == UINT32_MAX) { say("FAIL no graphics+compute queue family"); return -1; }
    uint32_t ne = 0;
    vkEnumerateDeviceExtensionProperties(phys, NULL, &ne, NULL);
    VkExtensionProperties *ext = calloc(ne, sizeof(*ext));
    vkEnumerateDeviceExtensionProperties(phys, NULL, &ne, ext);
    for (uint32_t i = 0; i < ne; i++) {
        if (!strcmp(ext[i].extensionName, VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME)) {
            has_prio_ext = 1; prio_ext_name = VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME;
        } else if (!has_prio_ext && !strcmp(ext[i].extensionName, VK_EXT_GLOBAL_PRIORITY_EXTENSION_NAME)) {
            has_prio_ext = 1; prio_ext_name = VK_EXT_GLOBAL_PRIORITY_EXTENSION_NAME;
        }
    }
    free(ext);
    return 0;
}

static int prio_value(const char *s, VkQueueGlobalPriorityKHR *out) {
    static const struct { const char *n; VkQueueGlobalPriorityKHR v; } t[] = {
        { "low", VK_QUEUE_GLOBAL_PRIORITY_LOW_KHR }, { "medium", VK_QUEUE_GLOBAL_PRIORITY_MEDIUM_KHR },
        { "high", VK_QUEUE_GLOBAL_PRIORITY_HIGH_KHR }, { "realtime", VK_QUEUE_GLOBAL_PRIORITY_REALTIME_KHR },
    };
    for (unsigned i = 0; i < sizeof(t) / sizeof(t[0]); i++)
        if (!strcmp(s, t[i].n)) { *out = t[i].v; return 1; }
    return 0;
}

static uint32_t find_memory(uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(phys, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
    return UINT32_MAX;
}

/* One device: fill A with a pattern, copy A to B, compute over B (b[i] = b[i] * 3 + i),
 * read B back. tag names the test in the output. */
static int run_device(const char *tag, const char *prio) {
    int ok = 0;
    double t0 = now_s();
    VkDevice dev = VK_NULL_HANDLE;
    VkBuffer buf[2] = { 0 };
    VkDeviceMemory mem[2] = { 0 };
    VkCommandPool pool = 0;
    VkFence fence = 0;
    VkShaderModule sm = 0;
    VkDescriptorSetLayout dsl = 0;
    VkPipelineLayout pl = 0;
    VkPipeline pipe = 0;
    VkDescriptorPool dp = 0;

    float qprio = 1.0f;
    VkDeviceQueueGlobalPriorityCreateInfoKHR gp = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_GLOBAL_PRIORITY_CREATE_INFO_KHR,
    };
    VkDeviceQueueCreateInfo qci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qfam, .queueCount = 1, .pQueuePriorities = &qprio,
    };
    const char *exts[1];
    uint32_t next = 0;
    if (prio && strcmp(prio, "default")) {
        if (!prio_value(prio, &gp.globalPriority)) { say("FAIL %s: unknown priority %s", tag, prio); return 0; }
        if (!has_prio_ext) { say("SKIP %s: no global priority extension", tag); return 1; }
        qci.pNext = &gp;
        exts[next++] = prio_ext_name;
    }
    VkDeviceCreateInfo dci = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci,
        .enabledExtensionCount = next, .ppEnabledExtensionNames = exts,
    };
    CHECK(vkCreateDevice(phys, &dci, NULL, &dev));
    VkQueue q;
    vkGetDeviceQueue(dev, qfam, 0, &q);

    for (int i = 0; i < 2; i++) {
        VkBufferCreateInfo bci = {
            .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = N_WORDS * 4,
            .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT |
                     VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        };
        CHECK(vkCreateBuffer(dev, &bci, NULL, &buf[i]));
        VkMemoryRequirements mr;
        vkGetBufferMemoryRequirements(dev, buf[i], &mr);
        /* A in device memory (VRAM), B host-visible so the CPU can check it. */
        VkMemoryPropertyFlags want = i == 0 ? VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT
            : VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        uint32_t mt = find_memory(mr.memoryTypeBits, want);
        if (mt == UINT32_MAX) { say("FAIL %s: no memory type for buffer %d", tag, i); goto out; }
        VkMemoryAllocateInfo mai = {
            .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mr.size, .memoryTypeIndex = mt,
        };
        CHECK(vkAllocateMemory(dev, &mai, NULL, &mem[i]));
        CHECK(vkBindBufferMemory(dev, buf[i], mem[i], 0));
    }

    VkShaderModuleCreateInfo smci = {
        .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = sizeof(fill_comp_spv), .pCode = (const uint32_t *)fill_comp_spv,
    };
    CHECK(vkCreateShaderModule(dev, &smci, NULL, &sm));
    VkDescriptorSetLayoutBinding b = {
        .binding = 0, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 1,
        .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
    };
    VkDescriptorSetLayoutCreateInfo dslci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &b,
    };
    CHECK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl));
    VkPipelineLayoutCreateInfo plci = {
        .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &dsl,
    };
    CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &pl));
    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                   .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "main" },
        .layout = pl,
    };
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe));
    VkDescriptorPoolSize ps = { VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1 };
    VkDescriptorPoolCreateInfo dpci = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &ps,
    };
    CHECK(vkCreateDescriptorPool(dev, &dpci, NULL, &dp));
    VkDescriptorSet ds;
    VkDescriptorSetAllocateInfo dsai = {
        .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dp,
        .descriptorSetCount = 1, .pSetLayouts = &dsl,
    };
    CHECK(vkAllocateDescriptorSets(dev, &dsai, &ds));
    VkDescriptorBufferInfo dbi = { buf[1], 0, VK_WHOLE_SIZE };
    VkWriteDescriptorSet w = {
        .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = ds, .descriptorCount = 1,
        .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi,
    };
    vkUpdateDescriptorSets(dev, 1, &w, 0, NULL);

    VkCommandPoolCreateInfo cpi = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfam };
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &pool));
    VkCommandBuffer cb;
    VkCommandBufferAllocateInfo cbai = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1,
    };
    CHECK(vkAllocateCommandBuffers(dev, &cbai, &cb));
    VkCommandBufferBeginInfo cbbi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    CHECK(vkBeginCommandBuffer(cb, &cbbi));
    vkCmdFillBuffer(cb, buf[0], 0, VK_WHOLE_SIZE, 0x01020304u);
    VkMemoryBarrier mb = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_SHADER_READ_BIT | VK_ACCESS_SHADER_WRITE_BIT,
    };
    vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    VkBufferCopy region = { 0, 0, N_WORDS * 4 };
    vkCmdCopyBuffer(cb, buf[0], buf[1], 1, &region);
    vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    vkCmdBindPipeline(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
    vkCmdBindDescriptorSets(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1, &ds, 0, NULL);
    vkCmdDispatch(cb, N_WORDS / 256, 1, 1);
    VkMemoryBarrier hb = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT, .dstAccessMask = VK_ACCESS_HOST_READ_BIT,
    };
    vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &hb, 0, NULL, 0, NULL);
    CHECK(vkEndCommandBuffer(cb));

    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    CHECK(vkCreateFence(dev, &fci, NULL, &fence));
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb };
    CHECK(vkQueueSubmit(q, 1, &si, fence));
    CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, FENCE_TIMEOUT_NS));

    uint32_t *p;
    CHECK(vkMapMemory(dev, mem[1], 0, VK_WHOLE_SIZE, 0, (void **)&p));
    uint32_t bad = 0, first = UINT32_MAX;
    for (uint32_t i = 0; i < N_WORDS; i++) {
        if (p[i] != 0x01020304u * 3u + i) { if (!bad++) first = i; }
    }
    vkUnmapMemory(dev, mem[1]);
    if (bad) { say("FAIL %s: %u of %u words wrong (first at %u)", tag, bad, N_WORDS, first); goto out; }
    ok = 1;
    say("OK %s (%.0f ms)", tag, (now_s() - t0) * 1000);
out:
    if (dev) {
        vkDeviceWaitIdle(dev);
        if (fence) vkDestroyFence(dev, fence, NULL);
        if (pool) vkDestroyCommandPool(dev, pool, NULL);
        if (dp) vkDestroyDescriptorPool(dev, dp, NULL);
        if (pipe) vkDestroyPipeline(dev, pipe, NULL);
        if (pl) vkDestroyPipelineLayout(dev, pl, NULL);
        if (dsl) vkDestroyDescriptorSetLayout(dev, dsl, NULL);
        if (sm) vkDestroyShaderModule(dev, sm, NULL);
        for (int i = 0; i < 2; i++) {
            if (buf[i]) vkDestroyBuffer(dev, buf[i], NULL);
            if (mem[i]) vkFreeMemory(dev, mem[i], NULL);
        }
        vkDestroyDevice(dev, NULL);
    }
    return ok;
}

struct pair_arg { char tag[48]; int ok; };

static void *pair_thread(void *a) {
    struct pair_arg *pa = a;
    pa->ok = run_device(pa->tag, NULL);
    return NULL;
}

int main(int argc, char **argv) {
    const char *test = argc > 1 ? argv[1] : "info";
    if (setup_instance()) return 2;
    VkPhysicalDeviceProperties pp;
    vkGetPhysicalDeviceProperties(phys, &pp);
    if (!strcmp(test, "info")) {
        say("device %s, driver %u.%u.%u, API %u.%u, queue family %u, global priority: %s",
            pp.deviceName, pp.driverVersion >> 22, (pp.driverVersion >> 14) & 0xff,
            (pp.driverVersion >> 6) & 0xff, VK_API_VERSION_MAJOR(pp.apiVersion),
            VK_API_VERSION_MINOR(pp.apiVersion), qfam, has_prio_ext ? prio_ext_name : "none");
        return 0;
    }
    int ok = 1;
    if (!strcmp(test, "copy")) {
        char tag[48];
        const char *prio = argc > 2 ? argv[2] : "default";
        snprintf(tag, sizeof(tag), "copy/%s", prio);
        ok = run_device(tag, prio);
    } else if (!strcmp(test, "pair")) {
        int n = argc > 2 ? atoi(argv[2]) : 5;
        for (int r = 0; r < n && ok; r++) {
            struct pair_arg a[2];
            pthread_t t[2];
            for (int i = 0; i < 2; i++) {
                snprintf(a[i].tag, sizeof(a[i].tag), "pair %d/%d thread %d", r + 1, n, i);
                pthread_create(&t[i], NULL, pair_thread, &a[i]);
            }
            for (int i = 0; i < 2; i++) { pthread_join(t[i], NULL); ok &= a[i].ok; }
        }
    } else if (!strcmp(test, "many")) {
        int n = argc > 2 ? atoi(argv[2]) : 16;
        for (int i = 0; i < n && ok; i++) {
            char tag[32];
            snprintf(tag, sizeof(tag), "many %d/%d", i + 1, n);
            ok = run_device(tag, NULL);
        }
    } else {
        say("usage: pcnvtest info|copy [PRIO]|pair [N]|many [N]");
        return 2;
    }
    vkDestroyInstance(instance, NULL);
    return ok ? 0 : 1;
}
