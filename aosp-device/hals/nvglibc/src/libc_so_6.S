// Phase L2 (docs/nvidia-native.md): libnvglibc_c, soname libc.so.6: glibc's C library names as
// trampolines to bionic or to libnvglibc.so (trampolines.h, libc_syms.def).
#include "trampolines.h"
#include "libc_syms.def"
NVGLIBC_BIND_INIT
