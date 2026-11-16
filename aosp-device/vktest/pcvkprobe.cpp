// Phase J1 (docs/nvidia.md): picks the Vulkan driver of the NVK boot entry before anything
// draws. The entry passes androidboot.android_pc.vulkan=nouveau instead of
// androidboot.hardware.vulkan, so ro.hardware.vulkan is still unset when init.pc.rc runs this
// at post-fs-data (SurfaceFlinger and gpuservice start later, at boot). The probe loads NVK
// (vulkan.nouveau from the Vulkan APEX, as libvulkan does), creates an instance and a device
// with a graphics queue, and sets ro.hardware.vulkan to "nouveau" when that works and to
// "pastel" (SwiftShader) otherwise, so the entry still shows a picture when NVK cannot run.
// The Vulkan calls run in a child process with a time limit: a GPU that hangs costs at most
// kTimeoutSeconds of boot time. vendor.pc.vulkan_probe says what happened (pclog records it).

#define LOG_TAG "pcvkprobe"

#include <android-base/properties.h>
#include <android/dlext.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <hardware/hwvulkan.h>
#include <log/log.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/system_properties.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#include <algorithm>
#include <string>
#include <vector>

extern "C" android_namespace_t* android_get_exported_namespace(const char*);

namespace {

constexpr int kTimeoutSeconds = 20;

// Logs to logcat and to the kernel log (pclog's dmesg covers boots that never reach logcat).
void Log(const char* fmt, ...) {
    char msg[512];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);
    ALOGI("%s", msg);
    int kmsg = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (kmsg >= 0) {
        dprintf(kmsg, "<6>pcvkprobe: %s\n", msg);
        close(kmsg);
    }
}

// The child: returns 0 when NVK gives a device with a graphics queue. Writes one line (the
// device name or the failing step) to `out`.
int Probe(int out) {
    std::string apex = android::base::GetProperty("ro.vulkan.apex", "com.google.cf.vulkan");
    std::replace(apex.begin(), apex.end(), '.', '_');
    android_namespace_t* ns = android_get_exported_namespace(apex.c_str());
    if (!ns) {
        dprintf(out, "no linker namespace %s", apex.c_str());
        return 1;
    }
    const android_dlextinfo info = {
            .flags = ANDROID_DLEXT_USE_NAMESPACE,
            .library_namespace = ns,
    };
    void* so = android_dlopen_ext("vulkan.nouveau.so", RTLD_LOCAL | RTLD_NOW, &info);
    if (!so) {
        dprintf(out, "dlopen vulkan.nouveau.so: %s", dlerror());
        return 1;
    }
    auto* module = static_cast<hwvulkan_module_t*>(dlsym(so, HAL_MODULE_INFO_SYM_AS_STR));
    hwvulkan_device_t* hal = nullptr;
    if (!module || module->common.methods->open(&module->common, HWVULKAN_DEVICE_0,
                                                reinterpret_cast<hw_device_t**>(&hal)) != 0) {
        dprintf(out, "HAL module open failed");
        return 1;
    }

    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO};
    app.pApplicationName = "pcvkprobe";
    app.apiVersion = VK_API_VERSION_1_1;
    VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
    ici.pApplicationInfo = &app;
    VkInstance instance;
    VkResult r = hal->CreateInstance(&ici, nullptr, &instance);
    if (r != VK_SUCCESS) {
        dprintf(out, "vkCreateInstance %d", r);
        return 1;
    }
    auto gipa = hal->GetInstanceProcAddr;
#define GET(name) auto name = reinterpret_cast<PFN_##name>(gipa(instance, #name))
    GET(vkEnumeratePhysicalDevices);
    GET(vkGetPhysicalDeviceProperties);
    GET(vkGetPhysicalDeviceQueueFamilyProperties);
    GET(vkCreateDevice);
    GET(vkDestroyDevice);
    GET(vkDestroyInstance);
#undef GET

    uint32_t count = 0;
    r = vkEnumeratePhysicalDevices(instance, &count, nullptr);
    if (r != VK_SUCCESS || count == 0) {
        dprintf(out, "vkEnumeratePhysicalDevices %d, %u devices", r, count);
        return 1;
    }
    std::vector<VkPhysicalDevice> gpus(count);
    vkEnumeratePhysicalDevices(instance, &count, gpus.data());
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(gpus[0], &props);

    uint32_t nfam = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(gpus[0], &nfam, nullptr);
    std::vector<VkQueueFamilyProperties> fams(nfam);
    vkGetPhysicalDeviceQueueFamilyProperties(gpus[0], &nfam, fams.data());
    uint32_t fam = 0;
    while (fam < nfam && !(fams[fam].queueFlags & VK_QUEUE_GRAPHICS_BIT)) fam++;
    if (fam == nfam) {
        dprintf(out, "%s: no graphics queue", props.deviceName);
        return 1;
    }
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    qci.queueFamilyIndex = fam;
    qci.queueCount = 1;
    qci.pQueuePriorities = &priority;
    VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
    dci.queueCreateInfoCount = 1;
    dci.pQueueCreateInfos = &qci;
    VkDevice device;
    r = vkCreateDevice(gpus[0], &dci, nullptr, &device);
    if (r != VK_SUCCESS) {
        dprintf(out, "%s: vkCreateDevice %d", props.deviceName, r);
        return 1;
    }
    dprintf(out, "%s, Vulkan %u.%u.%u", props.deviceName, VK_VERSION_MAJOR(props.apiVersion),
            VK_VERSION_MINOR(props.apiVersion), VK_VERSION_PATCH(props.apiVersion));
    vkDestroyDevice(device, nullptr);
    vkDestroyInstance(instance, nullptr);
    return 0;
}

}  // namespace

int main() {
    int fds[2];
    if (pipe2(fds, O_CLOEXEC) != 0) return 1;
    pid_t pid = fork();
    if (pid == 0) {
        close(fds[0]);
        _exit(Probe(fds[1]));
    }
    close(fds[1]);

    bool ok = false;
    std::string why = "timed out";
    for (int i = 0; pid > 0 && i < kTimeoutSeconds * 10; i++) {
        int status;
        if (waitpid(pid, &status, WNOHANG) == pid) {
            ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
            why = WIFSIGNALED(status) ? "crashed (signal " + std::to_string(WTERMSIG(status)) + ")"
                                      : "";
            pid = 0;
            break;
        }
        usleep(100 * 1000);
    }
    if (pid > 0) kill(pid, SIGKILL);  // a hung GPU call: leave it, boot on SwiftShader

    char line[256] = "";
    if (pid == 0) {
        ssize_t n = read(fds[0], line, sizeof(line) - 1);
        line[std::max<ssize_t>(n, 0)] = 0;
    }
    if (why.empty()) why = line;

    const char* driver = ok ? "nouveau" : "pastel";
    std::string result = ok ? std::string("nouveau: ") + line : "pastel: NVK " + why;
    Log("ro.hardware.vulkan=%s (%s)", driver, result.c_str());
    android::base::SetProperty("vendor.pc.vulkan_probe", result.substr(0, PROP_VALUE_MAX - 1));
    android::base::SetProperty("ro.hardware.vulkan", driver);
    return 0;
}
