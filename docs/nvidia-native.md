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

### L3c: AHB size and the first full boot on the RTX 4070 (VFIO, 2026-11-17)

In the VFIO VM (below) the NVIDIA entry got the GPU, the ICD started, and SurfaceFlinger then
aborted in a loop: `Failed to create a valid texture ... [24,24] format:5`, after
`E vulkan.nvidia: dma-buf import failed: -2 (size 15468, ...)`. minigbm's handle `total_size`
counts the metadata region too (`cros_gralloc_driver.cc`: `reserved_region_size` + the bo's
size), which lives in its own fd; the HAL took 15468 (4096 of buffer + 11372 of metadata) as
the dma-buf's size and NVIDIA refused the import. The host test goes through gbm, whose
buffers have no metadata region, so it never saw it. `ReadGrallocHandle` now subtracts
`reserved_region_size`. Build `eng.caoanh.20261117.0020` in the VFIO VM: `sys.boot_completed=1`,
SurfaceFlinger `RE Vulkan (Ganesh)`, SystemUI `Pipeline=Skia (Vulkan)`, the desktop in
`screencap`, no Xid (`out/vfio-l3c/`).

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

## L4. Games on the native driver: Where Winds Meet (VFIO, 2026-11-17)

Where Winds Meet (`com.netease.yyvn` 1.15.7, arm64 through the native bridge, NetEase's Messiah
engine) in the VFIO VM on the NVIDIA entry: the game picks GLES itself (`OpenGLRendor` in its
log; the arm64 Vulkan proxy is there but unused), so it draws through
`libproxy_arm64_to_x86_64_libGLESv2` → ANGLE → `vulkan.nvidia` → the RTX 4070.

| Step | What happened | Fix |
|---|---|---|
| Install | split APKs from the user's Waydroid install, `adb install-multiple` (3.7 GB), then 737 MB of resources and the shader warm-up in the game | — |
| Load after login | the game aborted itself (SIGABRT from its own arm64 code) while loading the world; the 4 GB guest had 600 MB left, zram 1.1 GB used, `am_low_memory` 12 | guest RAM 12 GB (`MEM=12288`, allowed by the user for this test; the 4 GB rule stays for other VMs) |
| World all white, no HUD icons, black loading screens | the engine ships ASTC textures (`TextureASTC`/`TextureNewASTC` in `Engine/Config/Built.key`); NVIDIA's desktop Vulkan has no ASTC (`textureCompressionASTC_LDR = VK_FALSE`), and AOSP's ANGLE is built without `third_party/astc-encoder`, so ANGLE could not decode ASTC on the CPU (its `AstcDecompressorNoOp`) | `tools/patches/aosp17-angle-astcenc.patch` (`git apply` in `aosp/external/angle`): ARM's astc-encoder decoder at ANGLE's DEPS revision `2319d9c` (Apache-2.0, `ASTCENC_DECOMPRESS_ONLY`) as `angle_astcenc`, `AstcDecompressor.cpp` instead of the no-op, `-DANGLE_HAS_ASTCENC`: ANGLE exposes `GL_KHR_texture_compression_astc_ldr` as emulated and decodes ASTC to RGBA8 on upload |
| SIGFPE in `UtilsVk::copyImageBits` | `glCopyImageSubData` between two `GL_RGBA4` images with different actual formats; that path only handles RGB emulated as RGBA and divides by `pixelBytes / channelCount`, 0 for packed formats | same patch: such copies fail with a warning (`glCopyImageSubData between emulated formats not supported`) instead of crashing |

Result: the world renders fully (bamboo forest, fog, shadows, the character, every HUD icon),
`out/vfio-l4/wwm-world.png`; ~30 fps at 2560x1080 (33.4 ms per frame from SurfaceFlinger's
latency, the game's default cap), no Xid. Open: the RGBA4 copies are skipped (no visible
effect seen yet); ASTC decode costs CPU on texture upload.
Build `eng.caoanh.20261117.0224` (the patch in the full image set) boots the NVIDIA entry in the
VFIO VM with the same `libGLESv2_angle.so` (md5 `99d86416…`) as the WWM run; QEMU image
`out/aosp17-usb-l4a-qemu-nv.img` (the WWM run itself used `out/aosp17-usb-l4-qemu-nv.img`).
Stick update image `out/aosp17-usb-l4a.img` (`tools/mkusb-aosp17.sh --menu --userdata 64`, same
build); on the real PC WWM runs on the "Android 17, NVIDIA (proprietary)" entry.

**Native Vulkan (2026-11-17): the switch is a local tag file.** The game's launcher script
`checker.lua` (an encrypted patch file; read from a dump of the paused VM's memory, taken from
the host through the QEMU monitor, the game's pages located through `/proc/<pid>/pagemap`, the
chunk LZ4-decompressed and listed with `luac5.4 -l` after restoring its header; the opcodes are
shuffled, the constants and line structure are not) chooses the API in `post_initialize`:

```lua
-- checker.lua lines 46-90, reconstructed
local vk = false
if MPlatform.GetOsName() == "android" then
  if MPatch.FileExistInSystem("LocalData", "vulkan.tag") then vk = true
  else vk = MPatch.FileExistInSystem("LocalData", "EnableVulkan.tag") end
  -- LocalData/VulkanFail.tag counts Vulkan starts that did not finish; past the limit it is
  -- deleted and LocalData/VulkanBroken.tag is created
end
if vk and not MPatch.FileExistInSystem("LocalData", "VulkanBroken.tag") then
  log("checker choose Vulkan")   -- VulkanFail.tag + 1, ChooseDriver("vlk"), PrepareDevice, ...
else
  log("checker choose ES3")      -- ChooseDriver("es3")
end
```

With an empty `/sdcard/Android/data/com.netease.yyvn/files/LocalData/vulkan.tag` the log shows
`checker choose Vulkan`, `last_graphics_api.tag` becomes `vlk`, the server's package list carries a
`vlk` shader package instead of `es3` (`collect_known_fenbaos package=[..., vlk, ...] vlk`;
113 MB, then "Preparing shaders"), and the game process maps the arm64 `libvulkan` and
`libproxy_arm64_to_x86_64_libvulkan.so` (vulkan.nvidia behind it). First result: the launcher UI
draws its text but every image is black (the NVIDIA driver has no ASTC_LDR or ETC2 texture
compression, and the game ships ASTC textures), and it downloads the Vulkan variant of the core
resources again (5.4 GB).

**ASTC on vulkan.nvidia (Rust, `hals/nvgpu/vulkan/astc/lib.rs`).** The HAL now reports
`textureCompressionASTC_LDR` and emulates it: an ASTC image is created as R8G8B8A8 (UNORM/SRGB),
its format properties are those of R8G8B8A8 for sampling and transfers, and a
`vkCmdCopyBufferToImage(2)` into it is recorded as a copy from a HAL staging buffer that
`vkQueueSubmit(2)` fills by decoding the app's blocks on the CPU (the astc-decode crate, pure Rust,
vendored in `hals/nvgpu/third_party/astc-decode`). The C++ HAL asks the Rust module first in its
`vkGet*ProcAddr` and hands it the entry points below (`nvvk_next_device_proc` and friends).
`vkCreateDevice` clears `textureCompressionASTC_LDR` before NVIDIA sees it (otherwise NVIDIA
returns `VK_ERROR_FEATURE_NOT_PRESENT` and SurfaceFlinger's RenderEngine aborts).

Result in the VFIO VM (S26 identity, `vulkan.tag`): `checker choose Vulkan`, the HAL logs
`ASTC textures are decoded on the CPU`, the game process maps the arm64 `libvulkan` and
`libproxy_arm64_to_x86_64_libvulkan.so` with `vulkan.nvidia` behind them, and the world renders
with its textures (bamboo forest, grass, character, HUD): `out/vfio-l4/vk/wwm-vulkan-world.png`.
Open: 3 fps, a steady 333 ms per frame with the process nearly idle (0.9 s of CPU in 5 s) and
each frame ready on the GPU ~2 ms after it is queued, so the game waits on something with a
timeout every frame; and some iridescent noise patches on the ground.

The investigation that led here (kept for the record): The goal was WWM's own Vulkan
backend (arm64 `libvulkan` through the native bridge's proxy to vulkan.nvidia, no ANGLE in the
process). The game never loads the arm64 `libvulkan` (`/proc/<pid>/maps` has only the arm64
`libEGL`/`libGLESv2`/`libGLESv3` proxies; the x86 `libvulkan`/`vulkan.nvidia` there are ANGLE's), so
the proxy is not the blocker: nothing asks it for Vulkan.

What the game's start-up logs show (`LocalData/patch_log/*.txt` in
`/sdcard/Android/data/com.netease.yyvn/files`; the patcher and its checker are encrypted Lua):

1. The checker runs first and decides at once, with no Vulkan probe in between:
   ```
   enter check
   checker choose ES3
   checker:shader_prereg skip reason=disabled by toggle
   checker after check
   ```
   It writes `LocalData/last_graphics_api.tag` = `es3` (and `Patch/driver.tag` = `es3`).
2. The patcher then logs the inputs it reports, and its own driver choice:
   ```
   {..."device_model":"samsung#SM-S9480#0#4#2#","gpu":"Adreno (TM) 840",...}
   ------> on device checked...: valid=true reason=SUCC_adreno (tm) 840
   Driver:check_prefer_driver start
   Driver:check_prefer_driver res=nil
   StagePatchList:collect_known_fenbaos package=[kaifeng, es3, waiguan, ..., pkg, pvp] es3
   StagePatchList:fetch_patchlist res=ok from=fetched patchlist=patchlist_mid_es3.txt
   ```
   `device_model` is `ro.product.manufacturer#ro.product.model#?#CPU count#?`; `gpu` is GL_RENDERER
   from a GLES context (ANGLE, so gpuid.conf can change it). `check_prefer_driver res=nil`: no
   preferred driver for this device, so the default stays.
3. The package list comes from one file per platform and channel
   (`patchmd5_global_publish_android_formal_o_fenbao.txt`, not per device). Its graphics package is
   `es3` (shader cache `Patch/Shader/es3.shader.config`); there is no Vulkan (`vk`/`vulkan`) package.

Tried, each with the checker's cache cleared first (`last_graphics_api.tag`, `patch_conf.json*`,
`Patch/driver.tag`, `engine_features_*.txt`; account kept) and the result checked in a new log:

| Identity (ro.product.*, ro.soc.*) | GL_RENDERER (gpuid.conf) | Checker | Packages |
|---|---|---|---|
| android-pc x86_64, vsoc_x86_64_only | ANGLE (NVIDIA, Vulkan 1.4.357 ...) | ES3 | es3 |
| samsung SM-S9280 (Galaxy S24 Ultra), SM8650 | Adreno (TM) 750 | ES3 | es3 |
| samsung SM-S9480 (Galaxy S26 Ultra), SM8850, ro.board.platform canoe | Adreno (TM) 840 | ES3 | es3, fetched fresh from the server |

Then, on the S26 Ultra identity, the other device-side inputs were hidden one after another
(cumulative; tool: `out/vfio-l4/vk/wwm-check.sh LABEL`, which clears the cache, launches the game
and saves the new logs as `out/vfio-l4/vk/LABEL.log`):

| Step | Change | Checker |
|---|---|---|
| a | `ro.hardware` / `ro.boot.hardware` = `qcom` (bootconfig in `initrd.img`; the kernel command line loses to it) | ES3 |
| c1 | `ro.product.board` canoe, `ro.build.type` user, `ro.build.tags` release-keys, retail `ro.build.fingerprint` | ES3 |
| c2 | `ro.product.cpu.abilist(64)` = `arm64-v8a,x86_64` | ES3 |
| c3 | arm64 code's `/proc/cpuinfo` (berberis serves `/system/etc/berberis/cpuinfo.arm64.txt`): 8 Qualcomm cores, `Hardware: Qualcomm Technologies, Inc SM8850` | ES3 |
| c4 | `ro.hardware.virtual_device` 0; retail model/fingerprint on every partition, `ro.build.display.id/description/flavor` | ES3 |
| c5 | `ro.product.cpu.abi` arm64-v8a, `ro.bionic.arch` arm64 | ES3 |
| b | 8 vCPUs (one run, the user allowed it above the 4-vCPU cap; `device_model` becomes `samsung#SM-S9480#0#8#2#`) | ES3 |

The server side: the fenbao index the game fetches
(`https://h72naxx2gb-14.update.easebar.com/pl/patchmd5_global_publish_android_formal_o_fenbao.txt`,
copy in `out/vfio-l4/vk/server/`) is one file for all Android devices; it points at
`patchlist_dict.txt`, from which the game builds `known_list`, and that list has no `vk`/`vulkan`
package for any identity above (the S26 run fetched it fresh).

Android declares Vulkan in all three (`android.hardware.vulkan.version` 1.3 = 4206592, level 1,
compute, `android.software.vulkan.deqp.level`, from the cf.vulkan APEX). Inputs not changed:
`ro.hardware` (`cutf_cvm`, from the kernel command line), the 4 vCPUs (the VM cap; a real S26 has
8 cores), the ABI list (`x86_64,arm64-v8a`: the game runs translated). The engine's configs allow
Vulkan (`Engine/Config/Platform.bin`: `EnableVulkan` default True, with a windows-only rule;
GlobalOptions has Vulkan PSO options), so the choice is the checker's.

When the checker decides, and what it could read (2026-11-17, S26 identity):
- It runs in the launcher stage, before the engine has a GL context: its log (`enter check`,
  `checker choose ES3`, then `PatchPatcher:start`) is written before the patcher's
  `patch main init` / `waiting for device check...` / `on device checked...` (the GL_RENDERER
  check). So GL strings cannot be among its inputs; a faked `GL_VERSION` with an Adreno driver
  version (`debug.angle.gl_version` = `OpenGL ES 3.2 V@0846.21 ...`) changed nothing either.
- The game never calls Vulkan: the only Vulkan instance in its process is HWUI's (skiavk, the
  app's UI; one `vulkan: searching for layers ...` / `vulkan.nvidia: device NVIDIA GeForce RTX 4070`
  pair at start-up), no arm64 `libvulkan` is loaded, so no Vulkan property (vendorID,
  driverVersion, extensions, features) reaches it.
- Static read: the Java code (8 dex files) has no render-API selection (its only Vulkan strings are
  a video-capture SDK's); `libGame.so` (149 MB, the engine) is packed (no readable dynamic section,
  encrypted strings: only `Vulkan(MT)`), and the other native libraries reference Vulkan only for
  video/live streaming (`libnative-mvd-render`, `libshared-texture`).
- Tracing the process with strace makes it exit at once (anti-debug); not used further.
- Its own state files do not steer it: with `LocalData/last_graphics_api.tag` and
  `Patch/driver.tag` pre-seeded (no newline, like its `es3`) with `vk`, `vulkan`, `Vulkan`,
  `Vulkan(MT)`, `vk_mt` and `VK` in turn, every run logs `checker choose ES3` and writes `es3` back
  into `last_graphics_api.tag`; the patcher then logs `check_prefer_driver res=nil` and writes
  `es3` into `driver.tag` too. The checker is itself a patch file from the server
  (`PatchPatcher:check_file file=LuaText/checker.lua pass=true`, encrypted).
- The code around it is protected: the launcher activity `com.netease.game.MessiahNativeActivity`
  is not in the APK's eight dex files (the game's own classes load from an encrypted container at
  run time), so its intent extras or engine arguments cannot be read; the server toggles it uses
  (`shader_prereg skip reason=disabled by toggle`) sit in the encrypted
  `engine_features_remote_cache.txt` / `engine_features_grey_info.txt`.
- Without the game's GLES shader caches (`LocalData/Cache/GLES.cache`, `ShaderProfiles`,
  `Patch/Shader/*.shader.config` moved away) it still picks ES3, so it is not keeping the API it
  has caches for.
- Vulkan-side gaps of this device versus a phone (the NVIDIA driver reports no ETC2/ASTC_LDR
  texture compression, vendorID 0x10DE, a discrete deviceType, NVIDIA's driverVersion packing)
  cannot be what the checker rejects: nothing in the game queries Vulkan before the choice, so
  emulating ASTC/ETC2 or reporting Qualcomm values in the HAL would not reach it.
- The game's own settings (lobby and in-world, searched for "Vulkan", "OpenGL", "Render") have no
  render-API option.
- No patcher log, for any identity, names a Vulkan/SPIR-V file or package.
The checker indeed does not look at the device's Vulkan: it looks for the tag files above.

Those device-side changes could not matter: the checker reads no device property, only the
`LocalData` tag files, and the server's `vlk` package is there once the game asks for it.

The GLES path (ANGLE on vulkan.nvidia) stays. The real-PC crash on it (pclog boot-30
tombstone_14: SIGSEGV at 0x1cd in libnvidia-eglcore under ANGLE
`UpdateDescriptorSetsBuilder::flushDescriptorSetUpdates` <- `glDrawArrays`, after 4 min; the next run
went 35+ min) is the thing to fix there. The QEMU image `out/aosp17-usb-l4-qemu-nv.img` keeps the S26
Ultra identity (/product/etc/build.prop overlay, gpuid.conf `Adreno (TM) 840`) at the user's
request; `/data/local/tmp/product-build.prop.orig` and `cpuinfo.arm64.txt.orig` there, and the
saved `out/vfio-l4/vk/initrd.img.pc-orig` (bootconfig with cutf_cvm), restore the PC identity.

## VFIO in QEMU: the NVIDIA entries on the real card in a VM

`tools/run-qemu-aosp17-vfio.sh [IMAGE]` boots an image like `tools/run-qemu-aosp17-usb.sh`
(same lock, E-cores, 4 vCPUs, 4 GB) with the RTX 4070's PCI functions (GPU and HDMI audio)
passed through, so the NVIDIA entries can be tested without writing the stick and rebooting
the PC. An image made without `--menu` has a single entry, so the NVIDIA entry's words go in
with `--append` (the entry's own words override the defaults):

```
tools/mkusb-aosp17.sh --append "androidboot.selinux=permissive pcfb.enable=0 androidboot.android_pc.gpu=nvidia cfi=kcfi,warn intel_iommu=off module_blacklist=nouveau,nova_core,nova softlockup_panic=1 nmi_watchdog=panic androidboot.hardware.vulkan=nvidia androidboot.hardware.guest_hwui_renderer=skiavk androidboot.hardware.guest_disable_renderer_preload=true androidboot.vendor.apex.com.android.hardware.graphics.composer=com.android.hardware.graphics.composer.drm_hwcomposer" out/aosp17-usb-qemu-nv.img
POINTER=usb-mouse QEMU_EXTRA="-device vhost-vsock-pci,guest-cid=3" tools/run-qemu-aosp17-vfio.sh out/aosp17-usb-qemu-nv.img
```

Android's picture then comes out of the RTX 4070's own ports (a cable from the card to the
monitor's other input), the QEMU window keeps the std VGA; adb is `vsock:3:5555`. The card
stays on `vfio-pci` after QEMU exits: given back to `nvidia`, its new DRM node is opened by
Hyprland's hotplug handling (`AQ_DRM_DEVICES` only filters the devices present at its start),
and it could not be passed again until the next login. `run-qemu-aosp17-vfio.sh --release`
(or a reboot) gives it back.

The host must leave the card alone first:

1. VT-d on in the firmware; `intel_iommu=on iommu=pt` added to `GRUB_CMDLINE_LINUX_DEFAULT`
   in `/etc/default/grub`, `grub-mkconfig -o /boot/grub/grub.cfg`, reboot.
2. Hyprland on the iGPU: `~/.config/hypr/scripts/gpu-mode igpu` (points
   `config/custom/gpu.lua` at `~/.config/hypr/gpu/igpu.lua` instead of `gpu/nvidia.lua`:
   `AQ_DRM_DEVICES` = the iGPU only, Mesa EGL/GLX/GBM/Vulkan/VA-API for Hyprland, its apps and
   the systemd user services, so nothing opens `/dev/nvidia*`), monitor on a motherboard
   port, reboot (this board's iGPU ports are only probed at boot). SDDM on Wayland: on X11 it
   keeps its greeter's root Xorg running for the whole session and that Xorg holds the card;
   `~/.config/hypr/system/sddm-wayland.conf` in `/etc/sddm.conf.d/` (`DisplayServer=wayland`,
   greeter in weston, which runs only while the greeter is shown). Without a
   monitor on the iGPU, `igpu.lua` falls back to `nvidia.lua`. `gpu-mode nvidia` undoes it.
3. The launcher refuses to start while the IOMMU is off, while the card's IOMMU group holds
   another device that is not a bridge, or while a process holds `/dev/nvidia*` or the card's
   DRM nodes (it lists them). It asks for the password, unloads the nvidia modules, binds the
   functions to `vfio-pci` (`driver_override`), raises its locked-memory limit (VFIO pins all
   guest RAM), and gives the card back to `nvidia` / `snd_hda_intel` when QEMU exits.

A stick image made with `--disk` does not boot in QEMU: its userdata partition is sized for
the stick, so inside the shorter file `/data` is 62 MB and system_server loops on ENOSPC.
Use an image made without `--disk` (default `out/aosp17-usb.img`, 4 GB userdata).
