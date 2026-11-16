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
| Real PC | `aosp-device/pc/pclog.sh`: `nvglibc_vktest` into `boot-N/nvglibc-vktest.txt` (exit status and any `NVRM: Xid` lines appended), 30 s after boot completed on `gpu=nvidia`; on `gpu=nvidia-l1` only after L1's pcnvtest has finished, so that it stays the only GPU client |

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

## L3. Android Vulkan HAL and gralloc on NVIDIA's own driver

Goal: SurfaceFlinger, HWUI and every app draw with NVIDIA's own Vulkan driver, loaded in-process,
on the "Android 17, NVIDIA (proprietary)" boot entry. No CPU rendering path on that entry: if
the driver fails, SurfaceFlinger fails and logcat says why.

```
App / HWUI / SurfaceFlinger (Skia on Vulkan), ANGLE (GLES)
  └─ Android Vulkan loader (sphal namespace; ro.vulkan.apex emptied on the entry)
       └─ /vendor/lib64/hw/vulkan.nvidia.so  (hwvulkan module, hals/nvgpu/vulkan)
            ├─ VK_ANDROID_native_buffer, VK_ANDROID_external_memory_android_hardware_buffer
            │    on VK_EXT_external_memory_dma_buf + VK_EXT_image_drm_format_modifier + sync fds
            └─ libnvglibc (L2) ─► /vendor/lib64/nvidia/libGLX_nvidia.so.0 (NVIDIA 620.70.06 ICD)
gralloc: minigbm "nvidia" backend (block-linear, nvgr_layout.h) ─► nvidia-drm / udmabuf dma-bufs
composer: drm_hwcomposer on nvidia-drm (K4), client composition, IN_FENCE_FD / OUT_FENCE_PTR
```

### Pieces

| Piece | Where | What |
|---|---|---|
| Vulkan HAL | `aosp-device/hals/nvgpu/vulkan/` (`vulkan.nvidia`) | NVIDIA's dispatchable objects already start with the loader magic `0x01CDC0DE` (= `HWVULKAN_DISPATCH_MAGIC`), so every handle is NVIDIA's and almost every entry point is NVIDIA's function itself (no wrapping, no per-call cost). Hooked: instance/device creation (extension lists: window-system and swapchain extensions hidden, the two Android ones added; the NVIDIA extensions they need enabled on every device), `vkGetPhysicalDevice{Properties2,ImageFormatProperties2,ExternalBufferProperties}`, `vkCreateImage`/`vkDestroyImage`/`vkBindImageMemory2`/`vkCreateImageView`, `vkCreateBuffer`, `vkAllocateMemory`/`vkFreeMemory`, `vkCreateSamplerYcbcrConversion`, `vkGetDeviceImageMemoryRequirements`, the AHB queries, the gralloc-usage queries, `vkAcquireImageANDROID`, `vkQueueSignalReleaseImageANDROID`. |
| Buffer layout | `aosp-device/hals/nvgpu/include/nvgr/nvgr_layout.h` | shared by the HAL and the gralloc (below) |
| gralloc | `tools/patches/aosp17-minigbm-nvidia.patch` → `aosp/external/minigbm/nvidia.c` | replaces K1's nvidia-drm dumb backend; with `ro.hardware.vulkan=nvidia` it allocates as below, otherwise it is still K1's dumb backend (the L1 test entries) |
| Init | `aosp-device/hals/nvgpu/init.nvgpu.rc` | on `androidboot.hardware.vulkan=nvidia`: `ro.vulkan.apex` empty, `debug.renderengine.backend=skiavkthreaded` |
| NVIDIA userspace | `aosp-device/hals/nvgpu/hal.mk`, `hals/nvsidecar/hal.mk` | NVIDIA's ICD, GLVND libEGL/libGLdispatch, libEGL_nvidia and libnvidia-{glsi,tls,gpucomp,glcore,eglcore,glvkspirv,rtcore,allocator} (the set nvglibc loads, L2) from K2's glibc root to `/vendor/lib64/nvidia` (nvglibc's first driver directory; the sphal namespace reaches `/vendor/lib64` only), with an EGL vendor JSON naming `libEGL_nvidia.so.0` bare; vulkan.nvidia sets `__EGL_VENDOR_LIBRARY_FILENAMES` to it. The rest of the root stays at `/vendor/bin/nvsidecar` for L1's glibc test client, with bare-name ICD and EGL JSONs. |
| SELinux | `aosp-device/hals/nvgpu/sepolicy/file_contexts`, K2's `file_contexts` | the HAL, libnvglibc, `/vendor/lib64/nvglibc/`, `/vendor/lib64/nvidia/` as `same_process_hal_file` (apps may map only those executable); `/dev/nvidia*` as `gpu_device` (AOSP's GPU rules then cover every client). The entry stays SELinux-permissive. |
| Entry | `tools/mkusb-aosp17.sh` | "Android 17, NVIDIA (proprietary)": K1's `$nvp` + `androidboot.hardware.vulkan=nvidia androidboot.hardware.guest_hwui_renderer=skiavk` + `$drm`. K3's `$venus` is gone; K2's server starts only with `androidboot.android_pc.vulkan=venus`, which no entry has. |
| Host test | `tools/nvgpu-hosttest.sh`, `aosp-device/hals/nvgpu/hosttest/` | the HAL and the gralloc backend built with gcc for the build host and run on its RTX 4070 (below) |

### Buffer layout (measured on the RTX 4070, NVIDIA 620.70.06)

The AHB extension creates a `VkImage` before it sees the buffer (and Android's swapchain may
create images before their buffers exist, `VK_SWAPCHAIN_CREATE_DEFERRED_MEMORY_ALLOCATION_BIT_EXT`).
Mesa's drivers defer the image's layout to bind time; a thin layer over NVIDIA's own handles
cannot. So the layout of every buffer of an image format depends only on its format and size,
and the HAL creates the image with exactly that layout (`VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`,
explicit plane offsets and pitches), then checks the buffer it imports against it (a mismatch is
an error in logcat, never a silent fallback).

- **Block-linear, always**: NVIDIA lists `DRM_FORMAT_MOD_LINEAR` only as sampled/storage, never
  as a color attachment, so linear buffers could not be rendered to. Modifier
  `DRM_FORMAT_MOD_NVIDIA_BLOCK_LINEAR_2D(0, 1, 2, 6, h)` (what `DRM_NVIDIA_GET_DEV_INFO` and
  the primary planes' `IN_FORMATS` report: no compression, desktop sector layout, kind 6),
  `h` = the smallest block (8·2^h rows) that holds the height, at most 4.
- Pitch = row bytes aligned to 64 (a GOB is 64 bytes × 8 rows), rows aligned to the block height.
- GOB swizzle (offset of byte x, row y inside a GOB):
  `(x&32)<<3 | (y&4)<<5 | (x&16)<<2 | (y&3)<<4 | (x&15)`, i.e. 16-byte sectors, four rows of the
  left 32 bytes, then the right 32 (checked byte by byte against images NVIDIA wrote).
- Multi-planar formats (NV12/NV21, YV12, P010): planes packed one after another. NVIDIA's own
  implicit layouts start planes at 64 KiB, but for an explicit layout it sizes the image, and maps
  it on the GPU, as the sum of the plane sizes: a 64 KiB-aligned last plane faulted (Xid 39 on the
  test's channel). Vulkan also requires even sizes for 4:2:0 images (NVIDIA faults on odd ones).
- **Memory**: buffers only the GPU touches live in video memory
  (`DRM_IOCTL_NVIDIA_GEM_ALLOC_NVKMS_MEMORY` on nvidia-drm's render node, scanout-capable when
  asked, exported as a dma-buf). Buffers the CPU, camera, video or sensors touch live in system
  memory: a sealed memfd turned into a dma-buf by `/dev/udmabuf`. BAR1 is only 256 MiB on this
  card and nvidia-drm keeps a CPU mapping of video memory in BAR1 until the buffer is freed, so
  CPU-mapped buffers must not be video memory.
- **CPU access** goes through a linear shadow in the mapper (Android's own plane layout, YV12 as
  Y, V, U): lock detiles, unlock tiles back (a write-only lock of the whole buffer skips the
  detile).
- **Cache coherency**: NVIDIA reads and writes imported system memory without snooping the CPU
  caches (measured: lost and stale cache lines). The allocator flushes a new buffer's zeroed lines,
  and the mapper flushes (`clflush`) around every CPU read and write of system memory.
- Formats Vulkan never images (blobs, RGB888, RAW16, ...) are linear system memory, pitch 256.

### WSI

`vkGetSwapchainGrallocUsage*ANDROID` and the AHB usage query return GPU usages only, so window
buffers are video memory. `vkAcquireImageANDROID` imports the dequeue fence as a temporary
`SYNC_FD` payload of the semaphore and/or fence (-1 = signalled). `vkQueueSignalReleaseImageANDROID`
submits the waits plus a signal of a per-queue exportable semaphore and exports it as the
release `sync_file`, which SurfaceFlinger and drm_hwcomposer (`IN_FENCE_FD`) wait on.
`VkPhysicalDevicePresentationPropertiesANDROID.sharedImage` is false (no front-buffer rendering).
Queue global priorities above MEDIUM (SurfaceFlinger and HWUI ask for REALTIME/HIGH) are capped
at MEDIUM because Phase K's Xid 51 came right after a high-priority channel started;
`androidboot.android_pc.nvprio=1` lifts the cap.

### Checked

- Host test (`tools/nvgpu-hosttest.sh`, RTX 4070, 620.70.06): the hwvulkan module as Android's
  loader drives it (extension lists, the loader magic, presentation properties, device with a
  REALTIME queue); a 3-buffer swapchain with one buffer bound late, 6 frames
  acquire → clear → release, each frame read back through an AHB import as SurfaceFlinger does
  after waiting on the release fence; AHB export; a 64 KiB blob AHB as a `VkBuffer`; CPU → GPU
  for RGBA8888 (333×77, 2560×1080), RGB565, RGBA_FP16, RGBA_1010102, R8, NV12 (1920×1080,
  176×144), YV12, P010; GPU → CPU for RGBA8888 in system and video memory, NV12, YV12. Every
  byte compared: 0 differences.
- QEMU: build `eng.caoanh.20261116.1459` (system, vendor, super on top of the L1 and L2 commits).
  - NVIDIA entry (`out/aosp17-usb-l3-qemu-nv.img`: the entry's words through `--append`, QEMU has
    no NVIDIA GPU): `ro.hardware.vulkan=nvidia`, `ro.vulkan.apex` empty (the APEX's own setprop
    is refused, as intended), `debug.renderengine.backend=skiavkthreaded`,
    `debug.hwui.renderer=skiavk`, nvsidecar not started. The loader opens
    `/vendor/lib64/hw/vulkan.nvidia.so` in the sphal namespace, nvglibc loads NVIDIA's ICD from
    `/vendor/lib64/nvidia` (`same_process_hal_file`), and the ICD refuses to start without a GPU:
    `E vulkan.nvidia: libGLX_nvidia.so.0: ICD interface negotiation failed (-3, version 5)`,
    `E vulkan: failed to open Vulkan HAL device: No such device`, then SurfaceFlinger aborts
    with `Could not initialize Vulkan RenderEngine!` and restarts in a loop. That is the intended
    behaviour without NVIDIA hardware: no SwiftShader, the failure is in logcat. No SELinux
    denial for the HAL, nvglibc or NVIDIA's files.
  - Default entry (`out/aosp17-usb-l3-qemu.img`, `--menu`, entry 1): boots
    (`sys.boot_completed=1` after ~80 s), SwiftShader (`pastel`) from the Cuttlefish Vulkan APEX
    as before, minigbm on vkms. Two crashes (`dlkm_loader`: a vendor_dlkm
    module list check; `com.android.bluetooth`: no Bluetooth controller in QEMU) touch nothing
    of L3.

### L3b: zygote must not hold NVIDIA's device fds (2026-11-16)

First real-PC boot of the entry (build `eng.caoanh.20261116.1950`, out/pclog-l1b/boot-28):
`vulkan.nvidia: device NVIDIA GeForce RTX 4070, driver NVIDIA 620.70.06`, SurfaceFlinger's
GaneshVkRenderEngine up on the GPU, no Xid in 615 s, but zygote64 crash-looped (100 tombstones,
`JNI FatalError called: (system_server) Not allowlisted (61): /dev/nvidiactl` from
`FileDescriptorInfo::CreateFromFd` ← `ForkCommon` ← `nativeForkSystemServer`), so system_server
never started. Zygote preloads the graphics driver (`ZygoteInit.preloadGraphicsDriver`), Android's
loader opens the HAL and enumerates its instance extensions, and the HAL used to negotiate the ICD
interface right there. Measured on the build host: `dlopen()` of NVIDIA's ICD opens no device;
`vk_icdNegotiateLoaderICDInterfaceVersion` opens `/dev/nvidiactl`, `/dev/nvidia0` (several),
`/dev/nvidia-modeset` and the render node and keeps them for the life of the process (the
tombstone lists the same set in zygote). zygote's fd allowlist (`fd_utils.cpp`) aborts any fork
that holds them; adding them there would share one NVIDIA client between every app.

Fix, both:
- `vulkan.nvidia` only loads the libraries when the module is opened and negotiates at the first
  call that needs the driver (`vkCreateInstance`, or a `vkGetInstanceProcAddr` of a function it
  does not implement itself), in the process that renders. Until then
  `vkEnumerateInstanceExtensionProperties` answers from the list NVIDIA 620 has (checked against
  the driver's real list at `vkCreateInstance`). If loading the module ever opened a
  `/dev/nvidia*` fd, the HAL logs it as an error. The host test checks that no `/dev/nvidia*` fd is
  open after opening the module and enumerating the instance extensions (12 after
  `vkCreateInstance`).
- The entry sets `androidboot.hardware.guest_disable_renderer_preload=true`
  (`ro.zygote.disable_gl_preload`): zygote does not load the graphics driver at all, every app
  loads it itself.

QEMU (build `eng.caoanh.20261116.2040`, the entry's words without the preload switch, so zygote
does preload the driver): with SurfaceFlinger stopped (it aborts without a GPU and takes zygote
with it), a restarted zygote maps `vulkan.nvidia.so`, libnvglibc, nvglibc's soname libraries and
`libGLX_nvidia.so.0`, holds no `/dev/nvidia*` fd, and forks system_server; no `Not allowlisted`
abort. Release image `out/aosp17-usb-l3b.img` (with the preload switch).

Left as they are (nvglibc log lines, harmless): `undefined symbol: ErrorF` (an X server function
NVIDIA's GLX code looks up optionally), `libnvtegrahv.so` and `libdbus-1.so.3` not provided (the
driver goes on without them), and `Could not initialize protected content Vulkan RenderEngine`
(NVIDIA has no protected memory; SurfaceFlinger runs without a protected context).

## VFIO in QEMU: the NVIDIA entries on the real card in a VM

`tools/run-qemu-aosp17-vfio.sh [IMAGE]` boots an image like `tools/run-qemu-aosp17-usb.sh`
(same lock, E-cores, 4 vCPUs, 4 GB) with the RTX 4070's PCI functions (GPU and HDMI audio)
passed through, so the NVIDIA entries can be tested without writing the stick and rebooting
the PC. Pick the NVIDIA entry in the QEMU window's menu; Android's picture then comes out of
the RTX 4070's own ports, the QEMU window keeps the std VGA.

The host must leave the card alone first:

1. VT-d on in the firmware; `intel_iommu=on iommu=pt` added to `GRUB_CMDLINE_LINUX_DEFAULT`
   in `/etc/default/grub`, `grub-mkconfig -o /boot/grub/grub.cfg`, reboot.
2. Hyprland on the iGPU: `~/.config/hypr/scripts/gpu-mode igpu` (points
   `config/custom/gpu.lua` at `~/.config/hypr/gpu/igpu.lua` instead of `gpu/nvidia.lua`:
   `AQ_DRM_DEVICES` = the iGPU only, Mesa EGL/GLX/GBM/Vulkan/VA-API for Hyprland, its apps and
   the systemd user services, so nothing opens `/dev/nvidia*`), monitor on a motherboard
   port, reboot (this board's iGPU ports are only probed at boot). No display manager: SDDM
   keeps its greeter's root Xorg running for the whole session and that Xorg holds the card,
   so login is a getty autologin on tty1 (`~/.config/hypr/system/getty-autologin.conf` in
   `/etc/systemd/system/getty@tty1.service.d/`, SDDM disabled) and `~/.zprofile` starts
   Hyprland, which locks the screen with hyprlock. Without a
   monitor on the iGPU, `igpu.lua` falls back to `nvidia.lua`. `gpu-mode nvidia` undoes it.
3. The launcher refuses to start while the IOMMU is off, while the card's IOMMU group holds
   another device that is not a bridge, or while a process holds `/dev/nvidia*` or the card's
   DRM nodes (it lists them). It asks for the password, unloads the nvidia modules, binds the
   functions to `vfio-pci` (`driver_override`), raises its locked-memory limit (VFIO pins all
   guest RAM), and gives the card back to `nvidia` / `snd_hda_intel` when QEMU exits.

A stick image made with `--disk` does not boot in QEMU: its userdata partition is sized for
the stick, so inside the shorter file `/data` is 62 MB and system_server loops on ENOSPC.
Use an image made without `--disk` (default `out/aosp17-usb.img`, 4 GB userdata).
