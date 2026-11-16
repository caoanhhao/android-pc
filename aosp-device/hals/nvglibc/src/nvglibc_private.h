// Phase L2 (docs/nvidia-native.md): shared between libnvglibc.so's sources.

#pragma once

#include <sys/cdefs.h>

__BEGIN_DECLS

// Logs to logcat (tag nvglibc); warnings and errors, and everything with $NVGLIBC_DEBUG set, also
// go to stderr.
void nvg_log(int prio, const char* fmt, ...) __attribute__((format(printf, 2, 3)));

__END_DECLS
