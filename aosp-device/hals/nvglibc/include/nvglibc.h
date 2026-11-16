// Phase L2 (docs/nvidia-native.md): nvglibc runs NVIDIA's glibc-built userspace driver inside
// bionic processes. libnvglibc.so is the core; the glibc sonames (libc.so.6, libpthread.so.0,
// libdl.so.2, libm.so.6, librt.so.1, ld-linux-x86-64.so.2, libgcc_s.so.1) are thin trampoline
// libraries next to it in <dir of libnvglibc.so>/nvglibc/, loaded on first use.

#pragma once

#include <sys/cdefs.h>

__BEGIN_DECLS

// Loads a glibc library and the glibc libraries it needs (DT_NEEDED, loaded leaves first) into
// this process with bionic's linker. `name` is a path, or a file name looked up in the driver
// directories: $NVGLIBC_DRIVER_PATH (colon-separated), else NVGLIBC_DEFAULT_DRIVER_PATH.
// Returns a dlopen() handle (use dlsym()/dlclose()), or NULL with nvglibc_dlerror() set.
void* nvglibc_dlopen(const char* name, int flags);

// The last nvglibc_dlopen() error of this thread, or NULL.
const char* nvglibc_dlerror(void);

__END_DECLS

#define NVGLIBC_DEFAULT_DRIVER_PATH "/vendor/lib64/nvidia:/vendor/bin/nvsidecar/lib"
