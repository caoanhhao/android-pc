// Minimal Turnip check: compute dispatch into host memory and into an AHardwareBuffer.
#include <android/hardware_buffer.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#define VK_USE_PLATFORM_ANDROID_KHR
#include <vulkan/vulkan.h>
#include "sq_spv.h"

#define CK(x) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { printf("FAIL %s = %d\n", #x, r_); exit(1); } } while (0)

static VkDevice dev;
static VkPhysicalDevice pd;
static VkQueue q;
static uint32_t qf;
static VkPipeline pipe;
static VkPipelineLayout pl;
static VkDescriptorSetLayout dsl;
static VkCommandPool pool;

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want) {
  VkPhysicalDeviceMemoryProperties mp;
  vkGetPhysicalDeviceMemoryProperties(pd, &mp);
  for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
    if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
  printf("FAIL no memory type bits=%x want=%x\n", bits, want);
  exit(1);
}

static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec / 1e9;
}

// Runs the shader `reps` times over buf (n uints); returns seconds.
static double dispatch(VkBuffer buf, uint32_t n, uint32_t iters, int reps) {
  VkDescriptorPoolSize ps = {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1};
  VkDescriptorPoolCreateInfo dpci = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, 0, 0, 1, 1, &ps};
  VkDescriptorPool dp;
  CK(vkCreateDescriptorPool(dev, &dpci, 0, &dp));
  VkDescriptorSetAllocateInfo dsai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, 0, dp, 1, &dsl};
  VkDescriptorSet ds;
  CK(vkAllocateDescriptorSets(dev, &dsai, &ds));
  VkDescriptorBufferInfo dbi = {buf, 0, VK_WHOLE_SIZE};
  VkWriteDescriptorSet w = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, 0, ds, 0, 0, 1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 0, &dbi, 0};
  vkUpdateDescriptorSets(dev, 1, &w, 0, 0);
  VkCommandBufferAllocateInfo cbai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, 0, pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1};
  VkCommandBuffer cb;
  CK(vkAllocateCommandBuffers(dev, &cbai, &cb));
  VkCommandBufferBeginInfo bi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
  CK(vkBeginCommandBuffer(cb, &bi));
  vkCmdBindPipeline(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
  vkCmdBindDescriptorSets(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pl, 0, 1, &ds, 0, 0);
  uint32_t pc[2] = {n, iters};
  vkCmdPushConstants(cb, pl, VK_SHADER_STAGE_COMPUTE_BIT, 0, 8, pc);
  vkCmdDispatch(cb, (n + 63) / 64, 1, 1);
  VkMemoryBarrier mb = {VK_STRUCTURE_TYPE_MEMORY_BARRIER, 0, VK_ACCESS_SHADER_WRITE_BIT, VK_ACCESS_HOST_READ_BIT};
  vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &mb, 0, 0, 0, 0);
  CK(vkEndCommandBuffer(cb));
  VkFenceCreateInfo fci = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
  VkFence f;
  CK(vkCreateFence(dev, &fci, 0, &f));
  double t0 = now();
  for (int r = 0; r < reps; r++) {
    VkSubmitInfo si = {VK_STRUCTURE_TYPE_SUBMIT_INFO, 0, 0, 0, 0, 1, &cb, 0, 0};
    CK(vkQueueSubmit(q, 1, &si, f));
    CK(vkWaitForFences(dev, 1, &f, VK_TRUE, 10ull * 1000 * 1000 * 1000));
    CK(vkResetFences(dev, 1, &f));
  }
  double t = now() - t0;
  vkDestroyFence(dev, f, 0);
  vkFreeCommandBuffers(dev, pool, 1, &cb);
  vkDestroyDescriptorPool(dev, dp, 0);
  return t;
}

static int verify(const uint32_t *v, uint32_t n) {
  uint32_t bad = 0;
  for (uint32_t i = 0; i < n; i++)
    if (v[i] != i * i) bad++;
  return bad;
}

int main(int argc, char **argv) {
  int load_secs = argc > 1 ? atoi(argv[1]) : 0;
  VkApplicationInfo ai = {VK_STRUCTURE_TYPE_APPLICATION_INFO, 0, "vkprobe", 1, 0, 0, VK_API_VERSION_1_1};
  VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, 0, 0, &ai};
  VkInstance inst;
  CK(vkCreateInstance(&ici, 0, &inst));
  uint32_t n = 1;
  CK(vkEnumeratePhysicalDevices(inst, &n, &pd));
  VkPhysicalDeviceDriverProperties drv = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
  VkPhysicalDeviceProperties2 p2 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, &drv};
  vkGetPhysicalDeviceProperties2(pd, &p2);
  printf("device: %s, api %u.%u.%u, driver: %s (%s)\n", p2.properties.deviceName,
         VK_VERSION_MAJOR(p2.properties.apiVersion), VK_VERSION_MINOR(p2.properties.apiVersion),
         VK_VERSION_PATCH(p2.properties.apiVersion), drv.driverName, drv.driverInfo);

  uint32_t nq = 8;
  VkQueueFamilyProperties qp[8];
  vkGetPhysicalDeviceQueueFamilyProperties(pd, &nq, qp);
  for (qf = 0; qf < nq && !(qp[qf].queueFlags & VK_QUEUE_COMPUTE_BIT); qf++) {}
  float prio = 1;
  VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, 0, 0, qf, 1, &prio};
  const char *exts[] = {"VK_ANDROID_external_memory_android_hardware_buffer", "VK_KHR_external_memory",
                        "VK_EXT_queue_family_foreign", "VK_KHR_dedicated_allocation"};
  VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, 0, 0, 1, &qci, 0, 0, 4, exts};
  CK(vkCreateDevice(pd, &dci, 0, &dev));
  vkGetDeviceQueue(dev, qf, 0, &q);

  VkShaderModuleCreateInfo smci = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, 0, 0, sq_spv_len, (const uint32_t *)sq_spv};
  VkShaderModule sm;
  CK(vkCreateShaderModule(dev, &smci, 0, &sm));
  VkDescriptorSetLayoutBinding b = {0, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT, 0};
  VkDescriptorSetLayoutCreateInfo dslci = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, 0, 0, 1, &b};
  CK(vkCreateDescriptorSetLayout(dev, &dslci, 0, &dsl));
  VkPushConstantRange pcr = {VK_SHADER_STAGE_COMPUTE_BIT, 0, 8};
  VkPipelineLayoutCreateInfo plci = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, 0, 0, 1, &dsl, 1, &pcr};
  CK(vkCreatePipelineLayout(dev, &plci, 0, &pl));
  VkComputePipelineCreateInfo cpci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, 0, 0,
      {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, 0, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm, "main", 0}, pl};
  CK(vkCreateComputePipelines(dev, 0, 1, &cpci, 0, &pipe));
  VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, 0, 0, qf};
  CK(vkCreateCommandPool(dev, &cpi, 0, &pool));

  // Test 1: host-visible VkBuffer.
  const uint32_t N = 1 << 20;
  VkBufferCreateInfo bci = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, 0, 0, N * 4, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
  VkBuffer buf;
  CK(vkCreateBuffer(dev, &bci, 0, &buf));
  VkMemoryRequirements mr;
  vkGetBufferMemoryRequirements(dev, buf, &mr);
  VkMemoryAllocateInfo mai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, 0, mr.size,
      memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)};
  VkDeviceMemory mem;
  CK(vkAllocateMemory(dev, &mai, 0, &mem));
  CK(vkBindBufferMemory(dev, buf, mem, 0));
  uint32_t *v;
  CK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&v));
  memset(v, 0xff, N * 4);
  double t = dispatch(buf, N, 0, 1);
  printf("test1 compute into VkBuffer: %u elements, %u wrong, %.2f ms\n", N, verify(v, N), t * 1e3);

  // Test 2: BLOB AHardwareBuffer from the gralloc allocator, written by the GPU, read by the CPU.
  AHardwareBuffer_Desc d = {N * 4, 1, 1, AHARDWAREBUFFER_FORMAT_BLOB,
      AHARDWAREBUFFER_USAGE_GPU_DATA_BUFFER | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN | AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN};
  AHardwareBuffer *ahb = 0;
  int ar = AHardwareBuffer_allocate(&d, &ahb);
  printf("test2 AHardwareBuffer_allocate(BLOB %u bytes) = %d\n", N * 4, ar);
  if (ar == 0) {
    uint32_t *cv;
    AHardwareBuffer_lock(ahb, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, 0, (void **)&cv);
    memset(cv, 0xff, N * 4);
    AHardwareBuffer_unlock(ahb, 0);
    VkAndroidHardwareBufferPropertiesANDROID hp = {VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID};
    PFN_vkGetAndroidHardwareBufferPropertiesANDROID gp =
        (PFN_vkGetAndroidHardwareBufferPropertiesANDROID)vkGetDeviceProcAddr(dev, "vkGetAndroidHardwareBufferPropertiesANDROID");
    CK(gp(dev, ahb, &hp));
    VkExternalMemoryBufferCreateInfo emb = {VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO, 0,
        VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID};
    VkBufferCreateInfo bci2 = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, &emb, 0, N * 4, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer buf2;
    CK(vkCreateBuffer(dev, &bci2, 0, &buf2));
    VkImportAndroidHardwareBufferInfoANDROID imp = {VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, 0, ahb};
    VkMemoryDedicatedAllocateInfo ded = {VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO, &imp, 0, buf2};
    VkMemoryAllocateInfo mai2 = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, &ded, hp.allocationSize, memtype(hp.memoryTypeBits, 0)};
    VkDeviceMemory mem2;
    VkResult ir = vkAllocateMemory(dev, &mai2, 0, &mem2);
    if (ir != VK_SUCCESS) {  // a non-dedicated import is also valid for buffers
      mai2.pNext = &imp;
      ir = vkAllocateMemory(dev, &mai2, 0, &mem2);
    }
    printf("test2 import AHB into Vulkan: %d (allocationSize %llu)\n", ir, (unsigned long long)hp.allocationSize);
    if (ir == VK_SUCCESS) {
      CK(vkBindBufferMemory(dev, buf2, mem2, 0));
      t = dispatch(buf2, N, 0, 1);
      AHardwareBuffer_lock(ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, 0, (void **)&cv);
      printf("test2 compute into AHardwareBuffer, CPU lock read: %u elements, %u wrong, %.2f ms\n", N, verify(cv, N), t * 1e3);
      AHardwareBuffer_unlock(ahb, 0);
    }
  }

  // Optional load: keep the GPU busy so devfreq/GPU stats can be read.
  if (load_secs > 0) {
    double t0 = now();
    int reps = 0;
    while (now() - t0 < load_secs) {
      dispatch(buf, N, 2000, 1);
      reps++;
    }
    printf("load: %d dispatches of %u x 2000 iterations in %.1f s, result %u wrong\n", reps, N, now() - t0, verify(v, N));
  }
  printf("vkprobe done\n");
  return 0;
}
