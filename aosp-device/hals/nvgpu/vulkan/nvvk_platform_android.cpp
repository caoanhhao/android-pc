/*
 * Copyright 2026 The Android PC Authors. All rights reserved.
 * Use of this source code is governed by a BSD-style license that can be
 * found in the LICENSE file.
 */

// vulkan.nvidia on Android: log, properties, AHardwareBuffer, and NVIDIA's ICD through nvglibc.

#include <dlfcn.h>
#include <stdarg.h>
#include <stdlib.h>
#include <sys/system_properties.h>

#include <android/hardware_buffer.h>
#include <log/log.h>
#include <nvglibc.h>
#include <vndk/hardware_buffer.h>

#include "nvvk_priv.h"

namespace nvvk {

void Log(LogLevel level, const char* fmt, ...) {
    static const android_LogPriority kPriority[] = {ANDROID_LOG_ERROR, ANDROID_LOG_WARN,
                                                    ANDROID_LOG_INFO, ANDROID_LOG_DEBUG};
    va_list ap;
    va_start(ap, fmt);
    __android_log_vprint(kPriority[level], NVVK_TAG, fmt, ap);
    va_end(ap);
}

int GetPropertyInt(const char* name, int def) {
    char value[PROP_VALUE_MAX] = "";
    if (__system_property_get(name, value) <= 0)
        return def;
    char* end = nullptr;
    const long v = strtol(value, &end, 0);
    return end != value ? static_cast<int>(v) : def;
}

void* IcdOpen(const char* name, char* err, size_t err_size) {
    // NVIDIA's Vulkan driver initialises through GLVND's libEGL, which finds NVIDIA's EGL driver
    // through this file (hals/nvgpu/hal.mk installs both). Overwrites what nvglibc's constructor
    // guessed when this module was loaded: K2's glibc root still carries a JSON naming the old
    // absolute path of libEGL_nvidia.
    setenv("__EGL_VENDOR_LIBRARY_FILENAMES", "/vendor/lib64/nvidia/10_nvidia.json", 1);
    void* lib = nvglibc_dlopen(name, RTLD_NOW | RTLD_LOCAL);
    if (!lib) {
        const char* e = nvglibc_dlerror();
        snprintf(err, err_size, "nvglibc: %s", e ? e : "unknown error");
    }
    return lib;
}

void* IcdSym(void* lib, const char* name) {
    return dlsym(lib, name);  // nvglibc_dlopen() returns a bionic dlopen() handle
}

const native_handle* AhbHandle(const AHardwareBuffer* ahb) {
    return AHardwareBuffer_getNativeHandle(ahb);
}

void AhbDescribe(const AHardwareBuffer* ahb, AhbDesc* desc) {
    AHardwareBuffer_Desc d = {};
    AHardwareBuffer_describe(ahb, &d);
    *desc = {d.width, d.height, d.layers, d.format, d.usage};
}

int AhbAllocate(const AhbDesc& desc, AHardwareBuffer** out) {
    AHardwareBuffer_Desc d = {};
    d.width = desc.width;
    d.height = desc.height;
    d.layers = desc.layers;
    d.format = desc.format;
    d.usage = desc.usage;
    return AHardwareBuffer_allocate(&d, out);
}

void AhbAcquire(AHardwareBuffer* ahb) {
    AHardwareBuffer_acquire(ahb);
}

void AhbRelease(AHardwareBuffer* ahb) {
    AHardwareBuffer_release(ahb);
}

}  // namespace nvvk
