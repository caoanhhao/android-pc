// Phase L2 (docs/nvidia-native.md): shared between libnvglibc.so's sources.

#pragma once

#include <stdbool.h>
#include <sys/cdefs.h>

__BEGIN_DECLS

// Logs to logcat (tag nvglibc); warnings and errors, and everything with $NVGLIBC_DEBUG set, also
// go to stderr.
void nvg_log(int prio, const char* fmt, ...) __attribute__((format(printf, 2, 3)));

// After bionic's dlopen() of a library rewritten by tools/waydroid/nvtls-rewrite: places its
// initial-exec TLS in nvglibc's surplus and runs its constructors (tls_surplus.c). Does nothing
// for other libraries.
bool nvg_tls_fixup(const char* path);

__END_DECLS
