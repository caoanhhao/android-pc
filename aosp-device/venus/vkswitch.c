/*
 * Phase K3 (docs/nvidia-proprietary.md): the Vulkan driver of the Venus entry
 * (ro.hardware.vulkan=venus). It is a switch, not a driver: each process that opens it gets
 * Venus (vulkan.virtio, the NVIDIA GPU through K2's sidecar) or, once vendor.venus.state is
 * "off", SwiftShader (vulkan.pastel). ro.hardware.vulkan cannot change after boot, so this is
 * how pcvenuswatch.sh takes new processes off a GPU that stopped creating devices (an Xid that
 * needs a GPU reset): it sets the property and restarts the graphics stack, and the new
 * processes come up on SwiftShader instead of aborting in vkCreateDevice.
 */

#define LOG_TAG "vkswitch"

#include <dlfcn.h>
#include <errno.h>
#include <hardware/hwvulkan.h>
#include <log/log.h>
#include <string.h>
#include <sys/system_properties.h>

static const struct hw_module_t *load_driver(const char *name) {
    /* the same linker namespace (the Vulkan APEX) that loaded this module */
    void *so = dlopen(name, RTLD_NOW | RTLD_LOCAL);
    if (!so) {
        ALOGE("dlopen %s: %s", name, dlerror());
        return NULL;
    }
    const struct hw_module_t *module = dlsym(so, HAL_MODULE_INFO_SYM_AS_STR);
    if (!module || strcmp(module->id, HWVULKAN_HARDWARE_MODULE_ID) != 0) {
        ALOGE("%s is not a Vulkan HAL module", name);
        dlclose(so);
        return NULL;
    }
    return module;
}

static int vkswitch_open(const struct hw_module_t *self, const char *id,
                         struct hw_device_t **device) {
    char state[PROP_VALUE_MAX] = "";
    __system_property_get("vendor.venus.state", state);

    const struct hw_module_t *module = NULL;
    if (strcmp(state, "off") != 0) module = load_driver("vulkan.virtio.so");
    if (!module) {
        ALOGI("Venus off (vendor.venus.state=%s): SwiftShader", state);
        module = load_driver("vulkan.pastel.so");
    }
    if (!module) return -ENOENT;
    /* the device's common.module is the real driver's, so the loader closes it there */
    return module->methods->open(module, id, device);
}

static struct hw_module_methods_t vkswitch_methods = {
        .open = vkswitch_open,
};

__attribute__((visibility("default"))) hwvulkan_module_t HAL_MODULE_INFO_SYM = {
        .common =
                {
                        .tag = HARDWARE_MODULE_TAG,
                        .module_api_version = HWVULKAN_MODULE_API_VERSION_0_1,
                        .hal_api_version = HARDWARE_HAL_API_VERSION,
                        .id = HWVULKAN_HARDWARE_MODULE_ID,
                        .name = "android-pc Venus/SwiftShader switch",
                        .author = "android-pc",
                        .methods = &vkswitch_methods,
                },
};
