/*
 * libpc_native_bridge: the native bridge ART loads (ro.dalvik.vm.native.bridge).
 *
 * It forwards every NativeBridgeCallbacks entry to Google's libndk_translation and adds one
 * SIGSEGV handler (getSignalHandler), for arm64 code that PLT-hooks a host (x86_64) library.
 * NetEase's packer (libunisec.so, Where Winds Meet) writes the address of its arm64 hook into
 * the GOT slot of `read` in the host libartbase.so and later calls the saved original, which
 * is the host libc `read`. Neither call goes through the bridge:
 *
 *  1. host -> guest: x86_64 code calls through the GOT into arm64 code, which is mapped
 *     non-executable, and faults with rip == fault address. The handler asks the translator
 *     for a host-callable wrapper of that guest function (WrapGuestFunctionImpl + RunGuestCall,
 *     what the bridge's own JNI trampolines use) and resumes at the wrapper.
 *  2. guest -> host: arm64 code jumps to the host `read`. The translator finds no guest code
 *     there and raises SIGSEGV (berberis_HandleNoExec, fault address = the host function).
 *     If the address is the exact start of an exported host function, the handler registers a
 *     guest-callable trampoline at that address (MakeTrampolineCallable, as the bridge does for
 *     host JNIEnv functions); the translator retries the jump and now calls the host function.
 *
 * Both directions pass a generic signature: 8 integer and 8 floating-point arguments, integer
 * result. That covers the libc-style functions such hooks wrap; floating-point results,
 * structs by value and more than 8 integer arguments are not carried over.
 * persist.sys.pc.nb_hooks=0 turns the handler off. docs/arm-translation.md, "H3".
 */

#include <dlfcn.h>
#include <stddef.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/system_properties.h>
#include <sys/ucontext.h>

#include <atomic>

#include <android/log.h>

#define LOG_TAG "pc_native_bridge"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

// android::NativeBridgeCallbacks, version 8 (art/libnativebridge/include/nativebridge/
// native_bridge.h). That header is visible to //art only (Soong forbids include_dirs into art/
// as well), and this library only copies the table and replaces getSignalHandler, so the
// layout is spelled out here; the other entries are opaque.
using NativeBridgeSignalHandlerFn = bool (*)(int, siginfo_t*, void*);
struct NativeBridgeCallbacks {
  uint32_t version;
  void* initialize;
  void* loadLibrary;
  void* getTrampoline;
  void* isSupported;
  void* getAppEnv;
  void* isCompatibleWith;
  NativeBridgeSignalHandlerFn (*getSignalHandler)(int signal);
  void* unloadLibrary;
  void* getError;
  void* isPathSupported;
  void* unused_initAnonymousNamespace;
  void* createNamespace;
  void* linkNamespaces;
  void* loadLibraryExt;
  void* getVendorNamespace;
  void* getExportedNamespace;
  void* preZygoteFork;
  void* getTrampolineWithJNICallType;
  void* getTrampolineForFunctionPointer;
  // libndk_translation's table (168 bytes) has one entry more than AOSP 17's header; copied
  // as is so nothing of it is lost.
  void* extra;
};
static_assert(sizeof(NativeBridgeCallbacks) == 0xa8, "NativeBridgeCallbacks v8 layout");
static_assert(offsetof(NativeBridgeCallbacks, getSignalHandler) == 0x38, "getSignalHandler");

extern "C" {
// What libnativebridge looks up with dlsym() after loading this library.
__attribute__((visibility("default"))) NativeBridgeCallbacks NativeBridgeItf;
}

namespace {

using GuestAddr = uintptr_t;
struct GuestArgumentBuffer;
struct ThreadState;
using GuestRunnerFunc = void (*)(GuestAddr, GuestArgumentBuffer*);
using TrampolineFunc = void (*)(const void*, ThreadState*);

// berberis entry points libndk_translation exports (its proxy libraries link against them).
struct Berberis {
  void* (*wrap_guest_function)(GuestAddr, const char*, GuestRunnerFunc, const char*);
  GuestRunnerFunc run_guest_call;
  void (*make_trampoline_callable)(GuestAddr, bool, TrampolineFunc, const void*, const char*);
  void (*invalidate_guest_range)(GuestAddr, GuestAddr);
  void* (*guest_map_shadow)();
  bool (*is_executable)(const void* shadow, GuestAddr, size_t);  // const member: `this` first
  size_t x_offset[8];  // ThreadState offsets of x0..x7
  size_t v_offset[8];  // and of v0..v7 (d0..d7 are their low halves)
  bool ok;
};

Berberis g_bb;
NativeBridgeSignalHandlerFn (*g_ndk_get_signal_handler)(int);
NativeBridgeSignalHandlerFn g_ndk_sigsegv;

// Result + 8 integer + 8 floating-point arguments (berberis wrapper signature letters).
constexpr char kSignature[] = "llllllllldddddddd";

std::atomic<int> g_log_budget{64};

#define LOG_LIMITED(...)                                                  \
  do {                                                                    \
    if (g_log_budget.fetch_sub(1, std::memory_order_relaxed) > 0) LOGI(__VA_ARGS__); \
  } while (0)

// Host functions already given a guest trampoline. A second NoExec fault at the same address
// means the trampoline did not help (or the fault is not the translator's): let it crash.
std::atomic<GuestAddr> g_wrapped_host[512];

bool RememberHostFunction(GuestAddr addr) {
  size_t n = sizeof(g_wrapped_host) / sizeof(g_wrapped_host[0]);
  for (size_t i = (addr >> 4) % n, tries = 0; tries < n; i = (i + 1) % n, ++tries) {
    GuestAddr expected = 0;
    if (g_wrapped_host[i].compare_exchange_strong(expected, addr)) return true;
    if (expected == addr) return false;
  }
  return false;  // table full
}

bool IsGuestCode(GuestAddr addr) {
  return g_bb.is_executable(g_bb.guest_map_shadow(), addr, 1);
}

// Guest -> host: called by the translator's trampoline with the guest thread's state.
void CallHostFunction(const void* callee, ThreadState* state) {
  char* s = reinterpret_cast<char*>(state);
  uint64_t x[8];
  double d[8];
  for (int i = 0; i < 8; ++i) {
    memcpy(&x[i], s + g_bb.x_offset[i], sizeof(x[i]));
    memcpy(&d[i], s + g_bb.v_offset[i], sizeof(d[i]));
  }
  // Variadic type: the call passes the integers in rdi..r9 and on the stack and the doubles in
  // xmm0..7, as for a plain prototype, and also sets al for variadic callees such as open().
  auto fn = reinterpret_cast<uint64_t (*)(uint64_t, ...)>(const_cast<void*>(callee));
  uint64_t result = fn(x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7],
                       d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]);
  memcpy(s + g_bb.x_offset[0], &result, sizeof(result));
}

bool HostCallsGuest(GuestAddr addr, greg_t* rip) {
  if (!IsGuestCode(addr)) return false;
  void* wrapper = g_bb.wrap_guest_function(addr, kSignature, g_bb.run_guest_call,
                                           "pc_native_bridge host->guest");
  if (wrapper == nullptr) return false;
  LOG_LIMITED("host code called guest code at %p: running it through wrapper %p",
              reinterpret_cast<void*>(addr), wrapper);
  // The call already pushed its return address, so the wrapper starts as if called directly.
  *rip = reinterpret_cast<greg_t>(wrapper);
  return true;
}

bool GuestCallsHost(GuestAddr addr) {
  if (addr == 0 || IsGuestCode(addr)) return false;
  Dl_info info;
  if (dladdr(reinterpret_cast<void*>(addr), &info) == 0 ||
      info.dli_saddr != reinterpret_cast<void*>(addr) || info.dli_sname == nullptr) {
    return false;  // only exact starts of exported host functions
  }
  if (!RememberHostFunction(addr)) return false;
  // The failed jump left a "no exec" entry for this address in the translation cache.
  g_bb.invalidate_guest_range(addr, addr + 1);
  g_bb.make_trampoline_callable(addr, true, CallHostFunction, reinterpret_cast<void*>(addr),
                                info.dli_sname);
  LOG_LIMITED("guest code called host %s (%s) at %p: trampoline added", info.dli_sname,
              info.dli_fname, reinterpret_cast<void*>(addr));
  return true;
}

bool HandleSigsegv(int sig, siginfo_t* info, void* context) {
  if (info->si_code == SEGV_ACCERR) {
    auto addr = reinterpret_cast<GuestAddr>(info->si_addr);
    greg_t* rip = &static_cast<ucontext_t*>(context)->uc_mcontext.gregs[REG_RIP];
    if (static_cast<GuestAddr>(*rip) == addr ? HostCallsGuest(addr, rip) : GuestCallsHost(addr)) {
      return true;
    }
  }
  return g_ndk_sigsegv != nullptr && g_ndk_sigsegv(sig, info, context);
}

bool HooksEnabled() {
  char value[PROP_VALUE_MAX] = "";
  __system_property_get("persist.sys.pc.nb_hooks", value);
  return strcmp(value, "0") != 0;
}

NativeBridgeSignalHandlerFn GetSignalHandler(int signal) {
  NativeBridgeSignalHandlerFn ndk =
      g_ndk_get_signal_handler != nullptr ? g_ndk_get_signal_handler(signal) : nullptr;
  if (signal != SIGSEGV || !g_bb.ok || !HooksEnabled()) return ndk;
  g_ndk_sigsegv = ndk;
  return HandleSigsegv;
}

template <typename T>
bool Resolve(void* handle, const char* name, T* out) {
  *out = reinterpret_cast<T>(dlsym(handle, name));
  if (*out == nullptr) LOGE("libndk_translation has no %s", name);
  return *out != nullptr;
}

void ResolveBerberis(void* h) {
  size_t (*reg_offset)(int) = nullptr;
  size_t (*simd_offset)(int) = nullptr;
  bool ok = Resolve(h, "_ZN8berberis21WrapGuestFunctionImplEmPKcPFvmPNS_19GuestArgumentBufferEES1_",
                    &g_bb.wrap_guest_function);
  ok &= Resolve(h, "_ZN8berberis12RunGuestCallEmPNS_19GuestArgumentBufferE", &g_bb.run_guest_call);
  ok &= Resolve(h, "_ZN8berberis22MakeTrampolineCallableEmbPFvPKvPNS_11ThreadStateEES1_PKc",
                &g_bb.make_trampoline_callable);
  ok &= Resolve(h, "_ZN8berberis20InvalidateGuestRangeEmm", &g_bb.invalidate_guest_range);
  ok &= Resolve(h, "_ZN8berberis14GuestMapShadow11GetInstanceEv", &g_bb.guest_map_shadow);
  ok &= Resolve(h, "_ZNK8berberis14GuestMapShadow12IsExecutableEmm", &g_bb.is_executable);
  ok &= Resolve(h, "_ZN8berberis23GetThreadStateRegOffsetEi", &reg_offset);
  ok &= Resolve(h, "_ZN8berberis27GetThreadStateSimdRegOffsetEi", &simd_offset);
  if (!ok) return;
  for (int i = 0; i < 8; ++i) {
    g_bb.x_offset[i] = reg_offset(i);
    g_bb.v_offset[i] = simd_offset(i);
  }
  g_bb.ok = true;
}

__attribute__((constructor)) void Init() {
  // On failure NativeBridgeItf keeps version 0 and libnativebridge leaves the bridge off.
  void* h = dlopen("libndk_translation.so", RTLD_LAZY);
  if (h == nullptr) {
    LOGE("cannot load libndk_translation.so: %s", dlerror());
    return;
  }
  auto* ndk = static_cast<const NativeBridgeCallbacks*>(dlsym(h, "NativeBridgeItf"));
  if (ndk == nullptr) {
    LOGE("libndk_translation.so has no NativeBridgeItf");
    return;
  }
  if (ndk->version != 8) {  // the table layout above
    LOGE("libndk_translation.so NativeBridgeItf version %u, expected 8", ndk->version);
    return;
  }
  ResolveBerberis(h);
  g_ndk_get_signal_handler = ndk->getSignalHandler;
  NativeBridgeItf = *ndk;
  NativeBridgeItf.getSignalHandler = GetSignalHandler;
}

}  // namespace
