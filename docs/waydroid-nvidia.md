# NVIDIA's own Vulkan driver in Waydroid

Phase L (`docs/nvidia-native.md`) on the build host's Waydroid instead of the stick: Android apps in
Waydroid draw on the RTX 4070 through NVIDIA's own userspace driver (620.70.06, the host's
`nvidia-open`), loaded in-process by nvglibc behind `vulkan.nvidia`. No host-side render server
(the earlier attempt in `~/projects/waydroid-nvidia`, Venus over vtest, needed one).

```
App / HWUI (Skia on Vulkan)        SurfaceFlinger (SkiaGL) -- ANGLE (GLES)
  └─ Android 13's Vulkan loader ──────────┘
       └─ /vendor/lib64/hw/vulkan.nvidia.so        (hals/nvgpu, built as vulkan.nvidia.waydroid)
            └─ libnvglibc (hals/nvglibc) ─► /vendor/lib64/nvidia/libGLX_nvidia.so.0 (NVIDIA 620.70.06)
gralloc: Waydroid's minigbm_gbm_mesa ─► /vendor/lib64/libgbm_mesa_wrapper.so (aosp-device/waydroid/gralloc)
            ─► nvgr block-linear buffers: nvidia-drm video memory, or udmabuf system memory
hwcomposer.waydroid ─► dma-buf + NVIDIA block-linear modifier ─► Hyprland (on the RTX 4070)
```

NVIDIA's binaries are copied from the build host's own install and are never committed or
redistributed.

## The Waydroid image

| | Waydroid | android-pc |
|---|---|---|
| Android | LineageOS 20, Android 13 (API 33), `TQ3A.230901.001` | AOSP 17 |
| ABI | x86_64 (+ x86, arm via libhoudini) | x86_64 (+ arm64 via berberis) |
| Kernel | the host's (7.3, `nvidia-open` 620.70.06) | GKI + K1's modules |
| APEXes | flattened (`/system/apex/<name>/`) | images |
| gralloc | minigbm `gbm_mesa` backend (HIDL mapper/allocator 4.0) | minigbm "nvidia" (AIDL) |
| SurfaceFlinger RenderEngine | SkiaGL only (`skiavk` is not recognised) | Skia Vulkan |

The Phase L pieces are built in the AOSP 17 tree and run on Android 13 unchanged, except:

- **libc++.** `vulkan.nvidia` uses `std::__1::__hash_memory` and `__libcpp_verbose_abort`, which
  Android 13's libc++ lacks: the Waydroid module `vulkan.nvidia.waydroid` (`hals/nvgpu/Android.bp`,
  in no product) links libc++ statically. Everything else they import is in Android 13's
  bionic (symbol versions `LIBC`, `LIBC_P`, `LIBC_Q` only), liblog and libnativewindow.
- **bionic's static TLS** (below): Android 13 has no `aosp17-bionic-static-tls-surplus.patch`.
- **The gralloc** is Waydroid's own, with a new `libgbm_mesa_wrapper.so` (below).

Nothing in the image is rebuilt: everything goes into `/var/lib/waydroid/overlay`.

## Static TLS without the bionic patch

`libnvidia-tls`, `libnvidia-glcore`, `libGLX_nvidia` and GLVND's `libGLdispatch` use initial-exec
TLS (`R_X86_64_TPOFF64`). Android 13's linker refuses those in `dlopen()`ed libraries (`TLS symbol
"_nv005tls" in dlopened ... using IE access model`) and gives static TLS only to libraries loaded at
process start. Three pieces replace the patch:

1. **`libnvglibc_tls.so`** (`aosp-device/waydroid/tls`): 513 bytes of `.tdata`, 64-byte aligned. It
   is made a `DT_NEEDED` of Waydroid's `/system/lib64/libvulkan.so` and `libEGL.so`, so every
   process that links either at start (zygote and its apps, SurfaceFlinger, gpuservice, the
   composer) has the block in every thread's static TLS.
2. **`tools/waydroid/nvtls-rewrite`** (Rust), on copies of NVIDIA's libraries:
   - `nvtls-rewrite tls OUT LIB...`: every `R_X86_64_TPOFF64` becomes `R_X86_64_NONE` with its
     symbol and addend kept; `DT_INIT`, `DT_INIT_ARRAY`, `DT_INIT_ARRAYSZ` are retagged
     `DT_NVG_INIT*` (`0x6e766701`-`03`), so bionic runs no constructor; `DF_STATIC_TLS` is
     cleared (`DF_SYMBOLIC` stays). 4 relocations each in libnvidia-tls, libnvidia-glcore and
     libGLX_nvidia, 1 in libGLdispatch. The other libraries' TLS is general-dynamic, which bionic
     handles.
   - `nvtls-rewrite add-needed IN OUT NAME`: adds a `DT_NEEDED` without growing the file: the
     string of a `DT_NEEDED` library of the same length, with no version needs and no string
     shared with another name, becomes `NAME`, and `NAME` needs that library itself. In
     libvulkan.so and libEGL.so `libnvglibc_tls.so` stands in for `libgraphicsenv.so` (17 bytes
     each), and `libnvglibc_tls.so` is linked against `libgraphicsenv.so`.
3. **nvglibc** (`hals/nvglibc/src/tls_surplus.c`), after bionic's `dlopen()` of a library carrying
   `DT_NVG_INIT`: finds `libnvglibc_tls.so`'s block through `dl_iterate_phdr()`
   (`dlpi_tls_data`) and checks it is static (the same offset from the thread pointer in a second
   thread; x86_64 bionic puts it *above* the thread pointer, +12288 here); places the library's
   `PT_TLS` in it (libnvidia-tls at +0, 56 bytes; libGLdispatch at +56, 8 bytes); writes each
   rewritten GOT word (`tpoff + st_value + addend`, the symbol found in the library itself or
   in a library placed earlier, through `DT_GNU_HASH`/`DT_HASH`), under a temporary `mprotect`
   of the RELRO page; copies the TLS image into the calling thread and into
   `libnvglibc_tls.so`'s own image, which bionic copies into each new thread; runs `DT_INIT`
   and `DT_INIT_ARRAY`. Libraries without `DT_NVG_INIT` (all of them on android-pc) are left
   alone.

Threads that existed before the driver was loaded see zeros in the block; only libGLdispatch has a
non-zero image (GLVND's current dispatch table, set again by `eglMakeCurrent`).

Android 13's linker (a userdebug build) logs every retagged entry: `Warning: "... libnvidia-tls ..."
unused DT entry: unknown OS-specific (type 0x6e766701 ...) (ignoring)`. Harmless.

## gralloc: `libgbm_mesa_wrapper.so`

Waydroid's minigbm `gbm_mesa` backend (`android_external_minigbm`, lineage-18.1) allocates,
imports and maps through `struct gbm_ops` of `libgbm_mesa_wrapper.so`, which the overlay replaces
(`aosp-device/waydroid/gralloc/nvgbm_wrapper.c`, NDK, API 33). The vendor image's own wrapper
stays as `libgbm_mesa_wrapper_intel.so` (already there from the Venus setup) for what this one
does not take.

- Single-plane formats of `nvgr_layout.h` (RGBA/RGBX/BGRA 8888, RGB565, 1010102, FP16, R8, GR88,
  R16, GR1616) are NVIDIA block-linear with the layout `vulkan.nvidia` computes from format and
  size, as android-pc's minigbm "nvidia" backend makes them. GPU-only buffers are video memory
  (`DRM_IOCTL_NVIDIA_GEM_ALLOC_NVKMS_MEMORY` on `gralloc.gbm.device`, the RTX 4070's render node,
  exported as a dma-buf); buffers the CPU touches (the backend asks for a map stride) are system
  memory (a sealed memfd made a dma-buf by `/dev/udmabuf`).
- The backend sizes the buffer as `out_stride x height` after `alloc()`, so `alloc()` raises
  `args->height` until that covers the block-linear layout (rows aligned to the block height).
- The mapper maps a linear shadow: `map()` detiles, `unmap()` tiles back (the backend does not say
  whether a lock reads or writes, so both always happen), with `clflush` around CPU access to
  system memory (NVIDIA does not snoop it).
- YUV, blobs (R8 asked linear) and other formats go to the Intel wrapper on `renderD128`, linear.
  `vulkan.nvidia` does not image linear YUV, so video frames sampled by the GPU are a gap.

Waydroid's `cros_gralloc_handle` has a 32-bit `usage` (AOSP 17's is 64-bit): the fields after it
move by 4 bytes and the handle is one int shorter. `vulkan.nvidia`'s `ReadGrallocHandle()` tells
the two apart by the handle's size (`numFds + numInts`).

## vulkan.nvidia changes found in Waydroid

All in `hals/nvgpu/vulkan`, and harmless on android-pc:

| Symptom | Cause | Change |
|---|---|---|
| every app: `Assertion failed: mCreateDevice(...)` on HWUI's RenderThread, `NVIDIA vkCreateDevice failed: -3` | Android 13's HWUI enables every device extension listed; NVIDIA's ray tracing, CUDA kernel launch, optical flow and binary import extensions need `libcuda.so.1`, which nvglibc does not load (`libcuda.so.1: not provided`), and each fails `vkCreateDevice` alone | those extensions are hidden unless `libcuda.so.1` is in the driver directory (`kCudaDeviceExtensions`); a failed `vkCreateDevice` logs the extensions, `pNext` chain and queues |
| SurfaceFlinger: `Could not create EGL image, err = (0x300c)`, `Unable to generate SkSurface` (a SystemUI screenshot buffer, CPU-readable) | ANGLE binds an AHB image only to a `DEVICE_LOCAL` memory type; NVIDIA imports udmabuf system memory only into type 0, which is not | `vkGetAndroidHardwareBufferPropertiesANDROID` also offers the device-local image types for such buffers, and the import takes a type NVIDIA accepts for the dma-buf |
| `cmd gpu vkjson`: `"devices": [{}]` | Android 13's vkjson serialises a device only for the API versions it knows; NVIDIA reports 1.4 | the reported `apiVersion` is clamped to 1.3 below Android 16 (API 36), whose loader is the first with 1.4 (`vkGetPhysicalDeviceProperties(2)`) |
| SystemUI: `SIGFPE` in `BlobCache::clean()` from `ShaderCache::store` ← `storePipelineCacheData` | Android 13's HWUI blob cache breaks on a pipeline cache larger than half of it (AOSP fixed it later: `static_assert(maxKeySize + maxValueSize < maxTotalSize)`); NVIDIA's is | `debug.nvvk.pipeline_cache_max` (bytes, 0 = no cap; 262144 in Waydroid): a larger pipeline cache is reported empty, so HWUI stores none. Caches already on disk must go (`com.android.skia.shaders_cache` under `/data/user_de/0/*/code_cache`) |

## Device nodes

Waydroid rewrites its LXC configuration (`config`, `config_nodes`, `config_session`) at every
container and session start, so a bind mount added there does not stay. The nodes are made inside
the container instead: `/vendor/etc/init/waydroid-nvidia.rc` runs
`/vendor/bin/waydroid-nvidia-nodes.sh` at `early-init`, which reads the host's numbers from
`/proc/devices` and `/sys/class` and makes `/dev/nvidia0`, `nvidiactl`, `nvidia-modeset`,
`nvidia-uvm`, `/dev/udmabuf` and the RTX 4070's render node as `/dev/dri/renderD129` (mode 0666) in
the container's `/dev` tmpfs. The container may (`CAP_MKNOD`, no device cgroup rules); the host's
own nodes and permissions are untouched.

## The composer and the mouse cursor

Waydroid's hwcomposer hands the mouse cursor to the compositor as a `wl_shm` buffer
(`persist.waydroid.cursor_force_shm=false`, the default; the name is inverted), and copies each
cursor image into it with its own EGL (`egl_render_to_pixels`, `hardware/waydroid/hwcomposer`),
which is ANGLE on `vulkan.nvidia` here. The composer gets libEGL only with `hwcomposer.waydroid.so`,
through `dlopen()`, so `libnvglibc_tls.so` came in as dynamic TLS, nvglibc refused NVIDIA's
libraries (`libnvglibc_tls.so is not loaded in this process`), the composer's
`eglChooseConfig`/`eglCreateContext` returned `EGL_NOT_INITIALIZED`, and Waydroid showed no cursor.

`aosp-device/waydroid/composer/composer-preload-wrapper.c` is installed over
`/vendor/bin/hw/android.hardware.graphics.composer@2.1-service` (the vendor image's binary goes
beside it as `...-service.real`, read from `vendor.img` by `nvidia-build.sh`) and execs it with
`LD_PRELOAD=libnativewindow.so:libEGL.so`: libEGL (and so `libnvglibc_tls.so`) is loaded at
start. libnativewindow is the earlier Venus project's fix (`~/projects/waydroid-nvidia`,
`venus/notes-cursor.md`: Android 13's ANGLE looks up `AHardwareBuffer_*` with
`dlsym(dlopen(NULL))`). An init `override` of the service is not applied in Waydroid, hence the
wrapper. Checked 2026-11-18: the composer's maps have `libnvglibc_tls.so` and `libGLX_nvidia.so.0`,
its EGL calls all return `EGL_SUCCESS`, and the Android pointer shows over the Waydroid window.

## Build and install

```bash
tools/waydroid/nvidia-build.sh
```

`--no-aosp` skips the AOSP 17 step (libnvglibc and its soname libraries, `vulkan.nvidia.waydroid`,
`nvglibc_vktest`, through `tools/aosp17-build.sh`). Then cargo builds `nvtls-rewrite`, NVIDIA's
libraries are rewritten, Waydroid's libvulkan.so and libEGL.so are read out of its `system.img`
(`debugfs`, no root) and patched, and the NDK builds `libnvglibc_tls.so`, the gralloc wrapper and
`out/waydroid-nvidia/ahb_vktest`. Output: `out/waydroid-nvidia/overlay` (240 MB).

```bash
tools/waydroid/nvidia-install.sh
```

Run as the user (it asks for sudo itself). The RTX 4070 must be on `nvidia`, not `vfio-pci`
(`tools/run-qemu-aosp17-vfio.sh --release`). The first run backs up `waydroid.cfg` and every
overlay file it replaces into `/var/lib/waydroid/nvidia-backup/<date>/`, with the list of files it
adds; later runs update the files and keep that backup; `--uninstall` restores it. It sets in
`waydroid.cfg`: `ro.hardware.vulkan=nvidia`, `ro.hardware.egl=angle`,
`gralloc.gbm.device=/dev/dri/renderD129`, `ro.zygote.disable_gl_preload=true` (Phase L3b: zygote
must not hold NVIDIA's fds), `debug.hwui.renderer=skiavk`, `debug.nvvk.pipeline_cache_max=262144`,
removes `waydroid.gralloc.vtest`, then runs `waydroid upgrade --offline` and restarts
`waydroid-container`.

Reading Android's logs needs root: `sudo waydroid logcat`, or a script piped into
`sudo waydroid shell` (it makes a file given as its stdin or stdout root-owned: pipe through `cat`).

## Tests

- **`tools/waydroid/nvidia-hosttest.sh`**: `nvglibc_vktest` on Waydroid's own Android 13 linker and
  libc (from its `system.img`) with the overlay on top, in bwrap on the host's RTX 4070,
  `libnvglibc_tls.so` through `LD_PRELOAD`. 2026-11-17:

  ```
  nvglibc: TLS surplus: /vendor/lib64/nvidia/libnvidia-tls.so.620.70.06 at +0 (56 bytes)
  nvglibc: TLS surplus: /vendor/lib64/nvidia/libGLdispatch.so.0 at +56 (8 bytes)
  nvglibc_vktest: device 0: NVIDIA GeForce RTX 4070 (10de:2786) api 1.4.357 driver NVIDIA 620.70.06 (0x9b118180)
  nvglibc_vktest: vkCreateDevice OK (queue family 0)
  nvglibc_vktest: compute dispatch: 65536/65536 values correct (gpu 0.06 ms)
  nvglibc_vktest: PASS (load 18 ms, total 132 ms)
  ```

- **`ahb_vktest`** (`aosp-device/waydroid/test`), run in the container as `/data/local/tmp/ahb_vktest`:
  for RGBA8888 buffers GPU-only (144x27), GPU + CPU read, a 2560x1080 composer buffer, a
  screenshot-style buffer (GPU output + CPU read, no sampling) and GPU + CPU read/write (333x77):
  allocate through Waydroid's gralloc, `vkGetAndroidHardwareBufferPropertiesANDROID`, an image
  bound to the imported AHB, a GPU clear, CPU read-back through `AHardwareBuffer_lock`, and an
  EGLImage through ANGLE. All pass (`0 of 3888 pixels wrong`, `EGLImage ok`, ...): 267 device
  extensions, RGBA8 with features `0x8001dd83`.

## Result (2026-11-18, build host, Hyprland on the iGPU)

After `tools/waydroid/nvidia-install.sh` and a session start, Android boots and stays up (no
tombstone in 5+ minutes; `sys.boot_completed=1`):

- `cmd gpu vkjson`: `"deviceName" : "NVIDIA GeForce RTX 4070"`, `"deviceID" : 10118` (0x2786),
  `"driverName" : "NVIDIA"`, `"driverInfo" : "620.70.06"`, `"apiVersion" : 4206949` (1.3.357).
- `dumpsys SurfaceFlinger`: `GLES: Google Inc. (NVIDIA), ANGLE (NVIDIA, Vulkan 1.4.357 (NVIDIA NVIDIA
  GeForce RTX 4070 (0x00002786)), NVIDIA-620.70.6.0), OpenGL ES 3.2.0 (ANGLE 2.1.20440 ...)`;
  `/proc/<surfaceflinger>/maps` has `vulkan.nvidia.so`.
- `dumpsys gfxinfo` of SystemUI, Launcher3 and Settings: `Pipeline=Skia (Vulkan)` (HWUI on
  `vulkan.nvidia`); Launcher3's maps have `vulkan.nvidia.so`.
- `screencap`: the lock screen, drawn by SystemUI on Vulkan and composed by SurfaceFlinger on ANGLE,
  both on the RTX 4070 (`out/waydroid-nvidia/check14/screen.png`).

Open:

- **On screen** (2026-11-18, Hyprland back on the RTX 4070 with `gpu-mode nvidia`):
  `waydroid show-full-ui` shows Android, and the mouse cursor after the composer fix above. An
  iGPU compositor cannot show NVIDIA block-linear video memory.
- **YUV** (video frames, camera) stays linear on the Intel iGPU, which `vulkan.nvidia` does not
  image.
- 32-bit and arm (libhoudini) apps have no NVIDIA driver (NVIDIA's userspace here is x86_64 only).
