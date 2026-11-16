// Phase L2 (docs/nvidia-native.md): libnvglibc_gcc_s, soname libgcc_s.so.1: the unwinder
// (LLVM libunwind, linked into libnvglibc.so) under libgcc_s's names, for the C++ exceptions of
// libstdc++ that NVIDIA links statically (libnvidia-rtcore).
#include "trampolines.h"
WRAP(_Unwind_DeleteException)
WRAP(_Unwind_GetDataRelBase)
WRAP(_Unwind_GetIPInfo)
WRAP(_Unwind_GetLanguageSpecificData)
WRAP(_Unwind_GetRegionStart)
WRAP(_Unwind_GetTextRelBase)
WRAP(_Unwind_RaiseException)
WRAP(_Unwind_Resume)
WRAP(_Unwind_Resume_or_Rethrow)
WRAP(_Unwind_SetGR)
WRAP(_Unwind_SetIP)
WRAP(_Unwind_GetGR)
WRAP(_Unwind_GetIP)
WRAP(_Unwind_GetCFA)
WRAP(_Unwind_Backtrace)
WRAP(_Unwind_FindEnclosingFunction)
WRAP(_Unwind_ForcedUnwind)
.section .note.GNU-stack, "", @progbits
