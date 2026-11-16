# NVIDIA's driver as the device's native GPU driver (Phase L)

Running log for Phase L of `PLAN.md`: the RTX 4070 brought up the way a vendor brings up a GPU on
Android, with NVIDIA's own userspace driver (620.70.06, the version of K1's kernel modules) loaded
**in-process** by bionic code, instead of Phase K's glibc sidecar behind a vtest socket. One
section per step. NVIDIA's binaries are used for this personal stick only and are not
redistributed.

## L2. nvglibc: NVIDIA's glibc libraries inside bionic processes

NVIDIA ships its userspace only for glibc. nvglibc makes a bionic process able to `dlopen`
NVIDIA's Vulkan ICD (`libGLX_nvidia.so.0`) and everything it pulls in, and call it directly.

### Prior art

| Project | Direction | What it does | Why not as is |
|---|---|---|---|
| libhybris | bionic libraries in glibc processes | its own port of bionic's linker; a hook table maps each bionic symbol to a glibc wrapper | the opposite direction; the hook-table idea is reused here, inverted |
| box64 "wrapped libs" | x86_64 glibc programs on (ARM) bionic or glibc hosts | own ELF loader, `wrappedlibc` translates glibc calls to the host libc, incl. struct conversions | an emulator: its loader and wrappers live inside the dynarec context; nothing to emulate on x86_64 |
| Termux glibc (glibc-runner, glibc-repo) | glibc programs on Android | a whole glibc with its own `ld.so`, as a separate program | one libc per process: cannot be mixed with bionic in SurfaceFlinger or an app |
| gcompat (Alpine) | glibc binaries on musl | a shim library under glibc's sonames, loaded by musl's own linker, mapping glibc symbols onto musl | closest match: nvglibc is this design for bionic |
| glibc `rtld.optional_static_tls` | | 512 bytes of static TLS kept free for `dlopen()`ed libraries that use initial-exec TLS | what bionic was missing (see the bionic patch) |

Design picked: **gcompat's** — bionic's own linker loads the glibc libraries, glibc's sonames are
small trampoline libraries, and the few functions whose ABI differs are translated. No second ELF
loader: `dlopen`/`dlsym`/`dl_iterate_phdr`, unwinding, debuggerd and `/proc/<pid>/maps` all see
NVIDIA's libraries as ordinary libraries.

### How it works

```
bionic process (nvglibc_vktest now, the L3 Vulkan HAL next)
  └─ nvglibc_dlopen("libGLX_nvidia.so.0")                   libnvglibc.so (core, C)
       1. loads /vendor/lib64/nvglibc/*.so once:             glibc sonames, assembler only
            libc.so.6 = libnvglibc_c.so  (~470 glibc names: jump to bionic, or to nvg_* in core)
            libpthread.so.0, libdl.so.2, libm.so.6, librt.so.1, ld-linux-x86-64.so.2 (empty)
            libgcc_s.so.1 = libnvglibc_gcc_s.so (_Unwind_* -> LLVM libunwind in core)
            libX11.so.6 / libXext.so.6 (Xlib functions returning 0: no X display)
       2. reads the library's DT_NEEDED, loads them first (leaves first, recursively) by path
          from the driver directories, then the library: every DT_NEEDED is then already loaded
          and bionic matches it by soname (no linker namespace needed; libdl_android, which
          creates namespaces, is platform-only and the HAL is vendor code)
       3. dlopen()s by path with bionic's linker (RTLD_LOCAL, NODELETE)
```

- **Symbol binding.** The trampoline libraries have no symbol versions. bionic's linker accepts
  an unversioned definition for any requested version, so `memcpy@GLIBC_2.14` binds to
  `libc.so.6`'s `memcpy`; bionic's own `memcpy@LIBC` is skipped because `GLIBC_2.14` is not one
  of libc.so's version definitions. libnvglibc.so itself keeps calling bionic under the same
  names (its relocations are done before the trampolines are loaded).
- **Trampolines** (`src/trampolines.h`): `FWD(name, target)` is `jmp *slot`; the
  `libc.so.6` constructor points each slot at bionic's `target` (`dlsym` on libc/libm/libdl).
  A plain jump keeps the caller's frame, so variadic functions, `_setjmp` and `vfork` work. A
  missing target traps into `nvglibc_missing()`, which names the symbol and aborts.
  `WRAP(name)` jumps to `nvg_<name>` in libnvglibc.so. `DATA(name)` is a variable the core
  fills (`stdin`/`stdout`/`stderr`, `program_invocation_name`, ...). The symbol list
  (`src/libc_syms.def`) is the union of the GLIBC-versioned imports of libGLX_nvidia,
  libnvidia-{glcore,glsi,tls,gpucomp,glvkspirv,rtcore,allocator,eglcore,ml}, libEGL_nvidia and
  GLVND's libEGL/libGLdispatch: 386 forwards, 74 wrappers, 7 variables.
- **The driver directories**: `$NVGLIBC_DRIVER_PATH`, default
  `/vendor/lib64/nvidia:/vendor/bin/nvsidecar/lib` (K2's glibc root, until L3 gives the
  libraries their own directory). Only NVIDIA's libraries, GLVND's `libEGL.so.1` and
  `libGLdispatch.so.0` (and `$NVGLIBC_ALLOW`) are loaded from there; anything else the driver
  `dlopen()`s optionally (libxcb-*, libdbus, `libnvidia-egl-{wayland,gbm,xcb}`, libnvtegrahv) is
  reported missing, which the driver tolerates. `libdrm.so.2` maps to bionic's `libdrm.so`
  (plain C ABI).
- **EGL.** Without an X display NVIDIA's Vulkan ICD starts through EGL (GLVND `libEGL.so.1` →
  `libEGL_nvidia.so.0` → `libnvidia-eglcore`), as K2 found for the sidecar. nvglibc sets
  `__EGL_VENDOR_LIBRARY_FILENAMES` (if unset) to `<driver dir>/../share/glvnd/egl_vendor.d/10_nvidia.json`.

### ABI differences handled (x86_64)

Most of glibc's x86_64 ABI is the kernel's, which bionic shares: `struct stat`/`stat64`,
`dirent`, `statfs`, `msghdr`, `rusage`, `tm`, `passwd`, `utsname`, `mbstate_t`, `cpu_set_t`,
`pthread_attr_t`/`rwlock_t`/`once_t`/`key_t` sizes, errno values, `open`/`mmap`/`fcntl` flags,
the `%fs:0x28` stack guard and `%fs:0` TCB self-pointer. Those go straight to bionic. Translated:

| glibc | Difference | nvglibc |
|---|---|---|
| `pthread_mutex_*`, `pthread_cond_*` | same sizes, other layouts; glibc's static `PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP` sets `__kind` at offset 16, which bionic reads as a normal mutex | own futex mutex (normal/recursive/errorcheck/adaptive, timedlock, pshared) and sequence-counter condition variable on glibc's layout (`src/pthread.c`) |
| `pthread_mutexattr_t`, `pthread_condattr_t` | 4 bytes (bionic writes 8: stack overflow) | own 4-byte encoding |
| `pthread_attr_setinheritsched` | `PTHREAD_INHERIT_SCHED`/`EXPLICIT_SCHED` are 0/1, swapped in bionic | swapped back |
| `pthread_create` default stack | glibc 8 MiB (RLIMIT_STACK), bionic 1 MiB; NVIDIA's LLVM shader compiler recurses deeply | 8 MiB unless the caller set a size or stack |
| `pthread_{get,set}affinity_np` | not in bionic | `sched_{get,set}affinity(pthread_gettid_np())` |
| `sigaction` | 128-byte `sigset_t` in the struct | converted both ways (plain `sigset_t` functions: bionic reads the first 8 bytes, which hold signals 1-64 in both) |
| `sysconf` | different `_SC_*` numbers | table generated from glibc's `<bits/confname.h>` and bionic's `<bits/sysconf.h>` (`src/sysconf.inc`), cache line sizes 64 |
| `mallinfo` | `int` fields (bionic `size_t`) | converted |
| `__xstat`/`__fxstat`/`__lxstat`/`__fxstatat`/`__xmknod` | pre-2.33 versioned stat entry points | `stat`/`fstat`/`lstat`/`fstatat`/`mknod` |
| `__newlocale`/`__uselocale`/`__ctype_b_loc`/... | libstdc++ (linked into NVIDIA's libraries) reads the ctype table pointers of glibc's `struct __locale_struct` | one "C" locale with glibc-encoded ctype/tolower/toupper tables; `__nl_langinfo_l` answers glibc's item numbers for "C"; `localeconv` in glibc's `struct lconv` order |
| `strtold_l` | 80-bit x87 `long double` in `%st(0)`; bionic's is binary128 | assembler: `strtod`, then `fldl` |
| `strerror_r`, `basename` | GNU variants | bionic's `__gnu_strerror_r` |
| `__isoc99_*scanf`, `__vfprintf_chk`, `__fread_chk`, `__assert_fail`, `gnu_get_libc_version` ("2.38"), `dgettext`, `__popcountdi2` | glibc/libgcc-only | small wrappers |
| `dlopen` | glibc names, `RTLD_DEEPBIND` | the loader above; `RTLD_NOLOAD` honoured, `RTLD_DEEPBIND` dropped |
| `dlsym(RTLD_DEFAULT/RTLD_NEXT/dlopen(NULL), ...)` | glibc code expects glibc's ABI | `libc.so.6`'s names first, then bionic; a handle of an empty soname library (libpthread.so.0, ...) looks in `libc.so.6` |
| `dladdr1(RTLD_DL_LINKMAP)`, `dlinfo(RTLD_DI_LINKMAP/ORIGIN)` | `struct link_map` chain | glibc's public `link_map` for every loaded object, chained in `dl_iterate_phdr` order: the driver walks `l_prev`/`l_next` from its own entry to find its other libraries and fails `vk_icdNegotiateLoaderICDInterfaceVersion` with `VK_ERROR_INITIALIZATION_FAILED` without the chain |
| `_Unwind_*@GCC_3.0` (libgcc_s.so.1) | | LLVM libunwind, linked into libnvglibc.so, behind `nvg__Unwind_*` |
| initial-exec TLS (`libnvidia-tls`, `libGLdispatch`: `DF_STATIC_TLS`, `R_X86_64_TPOFF64`) | bionic refuses IE TLS in `dlopen()`ed libraries | bionic patch below |

Long double functions other than `strtold_l` are not imported by the driver; adding one needs
the same x87 conversion.

### The bionic patch: static TLS surplus

`tools/patches/aosp17-bionic-static-tls-surplus.patch` (apply with
`git -C aosp/bionic apply ../../tools/patches/aosp17-bionic-static-tls-surplus.patch`; it ships
in the runtime APEX's linker/libc and in the bootstrap linker/libc). Without it:
`dlopen failed: TLS symbol "_nv005tls" in dlopened "libnvidia-tls.so.620.70.06" ... using IE
access model`.

- `linker_finalize_static_tls()` reserves 512 bytes (64-byte aligned) at the end of every
  thread's static TLS, like glibc's `rtld.optional_static_tls`.
- `register_soinfo_tls()`: a library `dlopen()`ed after startup with `DF_STATIC_TLS` gets its TLS
  segment there (`static_offset` set, so `R_*_TLS_TPREL` relocations and `__tls_get_addr` work as
  for startup libraries), and becomes NODELETE. NVIDIA needs 56 bytes (libnvidia-tls) + 8
  (libGLdispatch).
- A non-zero TLS image (libGLdispatch's) is copied into every thread after relocation and before
  the library's constructors (`linker_init_static_tls_surplus()` → libc's
  `__pthread_internal_init_static_tls` through `libc_shared_globals::static_tls_init_callback`,
  under `g_thread_creation_lock` for writing), and into new threads by `pthread_create`
  (`__init_static_tls_surplus`, under the same lock for reading), so no thread is missed.
- `__libc_iterate_dynamic_tls` skips surplus modules (their memory is not the allocator's).

Cost: 512 bytes per thread in every dynamically linked process.

### Files

| Piece | Where |
|---|---|
| Core | `aosp-device/hals/nvglibc/src/core.c` (loader, dl*, bind), `pthread.c`, `libc.c`, `include/nvglibc.h` → `/vendor/lib64/libnvglibc.so` |
| Soname libraries | `src/trampolines.h`, `src/libc_so_6.S` + `src/libc_syms.def`, `src/gcc_s_so_1.S`, `src/x11_so_6.S`, `src/empty.S` → `/vendor/lib64/nvglibc/libnvglibc_*.so` (soname via `ldflags`, which Soong puts after its own `-soname`) |
| Test | `test/nvglibc_vktest.c` (+ `fill.comp`, `fill.spv.inc` from `glslc -O -mfmt=num`) → `/vendor/bin/nvglibc_vktest` |
| Packages | `aosp-device/hals/nvglibc/hal.mk` |
| bionic | `tools/patches/aosp17-bionic-static-tls-surplus.patch` |
| Host test | `tools/nvglibc-hosttest.sh` |
| Real PC | `aosp-device/pc/pclog.sh`: on the `nvidia`/`nvidia-l1` entries, 30 s after boot completed, `nvglibc_vktest` into `boot-N/nvglibc-vktest.txt` (exit status and any `NVRM: Xid` lines appended) |

`nvglibc_vktest` drives the ICD without a Vulkan loader, as a hwvulkan HAL does:
`vk_icdNegotiateLoaderICDInterfaceVersion` (5), `vkCreateInstance` (1.3),
`vkEnumeratePhysicalDevices` with driver properties, `vkCreateDevice` on a compute queue, and a
compute shader writing `i*i+7` into 65536 words of host-visible memory, checked on the CPU.

### Host test

The device binaries run on the build host in a bwrap sandbox that looks like the device:
`/system` from the product out directory with the patched **bootstrap** linker and libc, the
Soong-built `/vendor/lib64` and `/vendor/bin/nvglibc_vktest`, K2's glibc root at
`/vendor/bin/nvsidecar` (default driver directory and EGL JSON), and the host's `/dev/nvidia*`.
No `linux_bionic` build was needed: the device's own bionic runs on the host kernel (the property
area is missing, which bionic tolerates). The host's driver and the user's desktop are only read.

```
m libnvglibc libnvglibc_{c,dl,gcc_s,ld,m,pthread,rt,x11,xext} nvglibc_vktest \
  $P/system/bin/bootstrap/linker64 $P/system/lib64/bootstrap/libc.so
tools/nvglibc-hosttest.sh           # NVGLIBC_DEBUG=1 for the loader's log, HOSTLIB=1 for /usr/lib
```

Result (2026-11-16, build host RTX 4070, host driver 620.70.06):

```
== bionic: /vendor/bin/nvglibc_vktest through nvglibc
nvglibc_vktest: ABI bionic+nvglibc, loading libGLX_nvidia.so.0
nvglibc_vktest: device 0: NVIDIA GeForce RTX 4070 (10de:2786) api 1.4.357 driver NVIDIA 620.70.06 (0x9b118180)
nvglibc_vktest: vkCreateDevice OK (queue family 0)
nvglibc_vktest: compute dispatch: 65536/65536 values correct (gpu 0.13 ms)
nvglibc_vktest: PASS (load 18 ms, total 279 ms)
== glibc baseline (same source, host gcc)
nvglibc_vktest: ABI glibc, loading libGLX_nvidia.so.0
nvglibc_vktest: device 0: NVIDIA GeForce RTX 4070 (10de:2786) api 1.4.357 driver NVIDIA 620.70.06 (0x9b118180)
nvglibc_vktest: vkCreateDevice OK (queue family 0)
nvglibc_vktest: compute dispatch: 65536/65536 values correct (gpu 0.11 ms)
nvglibc_vktest: PASS (load 12 ms, total 146 ms)
== both passed
```

Loaded into the bionic process (NVGLIBC_DEBUG=1): the 9 soname libraries, then
libnvidia-glsi, libnvidia-tls, libnvidia-gpucomp, libnvidia-glcore, libGLX_nvidia,
libGLdispatch, libEGL, libEGL_nvidia, libnvidia-eglcore, libnvidia-allocator,
libnvidia-glvkspirv, libnvidia-rtcore. Missing and tolerated: libnvtegrahv.so, libdbus-1.so.3.
5 runs in a row passed.

### Findings

- Without an X display the ICD starts through GLVND EGL; without the EGL vendor JSON it fails
  negotiation (K2 saw the same as "Found no drivers").
- The driver reads `/proc/self/maps`, then walks the `link_map` chain from `dladdr1()` to find
  `libGLX_nvidia` and `libnvidia-glcore` and re-reads their ELF headers from disk.
- A soname library must never enter a global lookup group (LD_PRELOAD, RTLD_GLOBAL): its
  unversioned names would then interpose on bionic for every library, bionic's libc included
  (`dlopen` from libc's netd client init landed in nvglibc's `dlopen` during an experiment).
  nvglibc loads them RTLD_LOCAL only.
- For L3: in SurfaceFlinger the global group (the executable and its DT_NEEDED) is searched first
  for NVIDIA's imports too. bionic's versioned libraries cannot satisfy `GLIBC_2.x` imports, but
  an *unversioned* platform library exporting a glibc name would. None seen so far.
- For L3: the HAL runs in the `sphal` namespace; its permitted paths must cover the driver
  directory and `/vendor/lib64/nvglibc` (vendor `default`, where nvglibc_vktest runs, permits
  all of `/vendor`).

### Status

**2026-11-16: host test passes** (above); packaged for the vendor partition and in pclog on the
NVIDIA entries. Real-PC run pending: it needs a stick image with the bionic patch (runtime APEX
and bootstrap linker/libc, i.e. the system image) and the vendor image, built by L1 or L3.
