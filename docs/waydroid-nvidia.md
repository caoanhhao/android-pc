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

## Where Winds Meet

The game (arm64, through libhoudini) picks its renderer from `LocalData/vulkan.tag` like on the
stick (`docs/nvidia-native.md` L4). Without the tag it takes ES3 through Waydroid's ANGLE on
`vulkan.nvidia`, and crashed about two minutes in with SIGFPE (2026-11-18). A kprobe on
`force_sig_fault` (`sig==8`, user stack trace on) caught it: `FPE_INTDIV` at
`libGLESv2_angle.so+0x4abcd2`, a `div` in `rx::UtilsVk::copyImageBits` (ANGLE 2.1.20440), whose
block-size arithmetic reaches zero for the ASTC formats ANGLE emulates because `vulkan.nvidia`
reports no ASTC. The game's own crash handler swallows the signal (an empty minidump, no tombstone).

`waydroid-nvidia.rc` runs `hals/nvgpu/pcwwmvk.sh` once user 0's storage is unlocked, which creates
the empty tag (the game must have run once; `persist.vendor.android_pc.wwm_vulkan=0` turns it off).
With it `last_graphics_api.tag` reads `vlk` and the game renders on `vulkan.nvidia`.

Striped/black ground patches (2026-11-18): the terrain's virtual-texture pages are rendered into
`B4G4R4A4` and `B5G6R5` color attachments, which NVIDIA cannot render to; `vulkan.nvidia` now
gives those images a stand-in format (`docs/nvidia-native.md`, *The striped ground: 16-bit render
targets*). The game's ETC2/EAC maps are emulated too; the purple HUD icons have not been seen
since.

### Where Winds Meet's quality

2026-11-19, AOSP 17 image in Waydroid, NVIDIA 620.70.06, the user's data: the best preset
("Tuyệt phẩm") still looks soft. The game's own choice, not the graphics stack:

- **Not the stack.** The game's swapchain is 2560x1068 RGBA8 (the Android display, the
  `persist.waydroid.width/height` pin), and SurfaceFlinger composes it 1:1 (`dumpsys
  SurfaceFlinger`: source crop = display frame, no scaling). The game runs Vulkan on
  `vulkan.nvidia` (`last_graphics_api.tag` = `vlk`, `VkPipeline.cache`, the process maps
  NVIDIA's libraries); the device has `samplerAnisotropy` (16x), BC, ASTC/ETC2 (decoded
  losslessly to RGBA8), Vulkan 1.4.
- **The game ranks the device "middle".** Its log (`LocalData/game_account.log`, every byte
  +1) records a `default_render_level` per login: `middle` for this device. It was `high` only
  on 2026-11-15 18:11-19:51, while the earlier Venus setup reported the GPU as "Adreno (TM)
  740" (`~/projects/waydroid-nvidia`, `venus/notes-wwm.md`); NVIDIA's own name gets `middle`.
  `LocalData/crash_extra`: `default level: middle`, `setting level: high`.
- **"Tuyệt phẩm" is the `high` level, render set `AndroidHigh`.** `Engine/Config/
  AllRenderOptions.bin` (Messiah's binary config: u16-length keys, a type byte, `p` = u32 length
  + blob) has `AndroidFastLow2` ... `AndroidHigh`, `AndroidHigh2`. `AndroidHigh` against
  `AndroidHigh2` (and `PCHigh`):

  | option | AndroidHigh | AndroidHigh2 | PCHigh |
  |---|---|---|---|
  | `ScreenScale` (3D render scale) | 0.7 | 0.85 | 1.0 |
  | `ScreenMinSize` / `ScreenSize` (render width clamp) | 1300 / 1920 | - | - |
  | anti-aliasing | SMAA | TSAA (TAA quality 3) | TSAA |
  | `EnableVirtualTexture565` (terrain pages in RGB565) | 1 | 0 | 0 |
  | `MaxTextureSize` | 8 | 7 | 7 |
  | realtime reflections, transparency scale | 0.5, 0.5 | 1.0, 1.0 | 1.0, 1.0 |
  | bloom downsample, CSM tile size | 4, 15 | 2, 100 | 2, 100 |

  So the 3D scene is drawn at about 1792x748 and stretched to 2560x1068 with RCAS sharpening;
  screenshots show it (soft rocks and grass, sharp HUD).
- **The resources are the "mid" pack.** `LocalData/patch_conf.json`: `"patch_level":"mid"`, and
  every downloaded list is `patchlist_mid_*`: the textures themselves are the mid-range phone
  set, chosen at the first start (2026-11-15 14:03, rank `middle`).
- The game also flags the device as an emulator (`LocalData/Patch/emulator.tag`); the earlier
  notes found that NetEase's Pharos reports `is_ad_simulator=1` whatever the properties say.

`libGame.so` and the scripts are encrypted, so which rank unlocks `AndroidHigh2` or the high
pack could not be read.

**Apps see an Adreno 850.** `vulkan.nvidia` gives app processes a Qualcomm Adreno's identity:
vendor `0x5143`, Qualcomm's chip ID, "Adreno (TM) <model>", integrated, driver ID
`QUALCOMM_PROPRIETARY` and its name and info (also in every `VkPhysicalDeviceDriverProperties`
and `VkPhysicalDeviceVulkan12Properties` of the chain); limits, features, extensions and formats
stay NVIDIA's. On by default (the stick and Waydroid, same image), first as an opt-in
experiment (31f9f35, b43affd):

- every app process (uid 10000 and up in its user) sees it; system processes (SurfaceFlinger,
  system_server's HWUI, gpuservice) see NVIDIA;
- `persist.vendor.nvvk.adreno=0` turns it off everywhere;
  `persist.vendor.nvvk.adreno_exclude` lists process names that see NVIDIA;
  `persist.vendor.nvvk.adreno_apps` lists process names that see the Adreno although they are
  not apps;
- `persist.vendor.nvvk.adreno_model` picks the model: `850` (the default: the Snapdragon 8
  Elite Gen 6 Pro's, the fastest Android GPU), `740`, `750`, `830`, `840` or any other number,
  with an optional chip ID after a colon (`850:0x44070041`). Chip IDs come from Mesa's
  `freedreno_devices.py` (740 `0x43050a01`, 750 `0x43051401`, 830 `0x44050001`, 840
  `0x44050a31`); the 850 is not there yet and takes the 840's.

```bash
setprop persist.vendor.nvvk.adreno 0                          # off for every app
setprop persist.vendor.nvvk.adreno_exclude com.example.app    # off for one app
setprop persist.vendor.nvvk.adreno_model 740
```

The SoC stays the PC's (`ro.soc.*`, `ro.board.platform`, `/proc/cpuinfo`). WWM's Java code
reads `SOC_MODEL`, `SOC_MANUFACTURER`, `ro.board.platform`, `ro.hardware` and `/proc/cpuinfo`,
but in NetEase's device-info and anti-cheat collectors (`DiInfo`, `BUILD_SOC_MODEL_SET`) and
Glide, not where the rank is logged (`device_model` = brand, model, CPU name, cores, clock);
the quality logic itself is in encrypted native code. A Snapdragon SoC on an x86_64 CPU with
x86 `ro.hardware` would be one more inconsistency for those collectors to report, and the
properties are system-wide (every app and the system would see them), so it was not done.

On the stick (2026-11-19 14:2x): build `eng.caoanh.20261119.1409` (commit 276bf5e;
`vendor.img` has the new `vulkan.nvidia`), image `out/aosp17-usb-adreno850.img`
(`tools/mkusb-aosp17.sh --userdata 64`; its ESP has the one NVIDIA entry,
`androidboot.hardware.vulkan=nvidia`, no ranchu), written with `tools/update-usb-aosp17.sh`
over `eng.caoanh.20261119.0009`: "Read back and compared: OK", Google extras written back.
Not yet booted on the PC.

Checked: with `/system/bin/gpuservice` in the list, `cmd gpu vkjson` reports "Adreno (TM) 740",
`0x5143`/`0x43050a01` and driver ID 8. In the game (2026-11-19 10:42): logcat has `com.netease.yyvn
sees the GPU as an Adreno 740`, and the game's rank goes from `middle` to `high`
(`game_account.log` `default_render_level: high`, `crash_extra` `default level: high`); it
loads, reaches the world and plays on Vulkan, no crash in the first minutes.

The graphics menu (Hình ảnh > Chi tiết hình ảnh) has the presets Tiết kiệm pin, Cân bằng, Chất
lượng, Tuyệt phẩm, **Nguyên bản** and Tùy chỉnh. "Nguyên bản", above "Tuyệt phẩm", stays greyed
out with the `high` rank; TAA is a separate switch (on). The resource pack is still `mid`. Open:
what unlocks "Nguyên bản" (a lock icon on it: the device, by GPU or model) and whether the game
offers the high pack now. Adreno 850 (11:04, the user's request): `cmd gpu vkjson` with
gpuservice listed reports "Adreno (TM) 850", `0x44050a31`, "Driver Build: 800.0"; the game logs
`sees the GPU as an Adreno 850`, ranks it `high` as with the 740, and "Nguyên bản" keeps its
lock. So the lock does not follow the GPU name or chip ID alone; the model (`SM-S918B`), the
emulator flag or a server-side list are left. Changing the model would show the account a new
device, so it was not tried.

Updating only the HAL in the overlay (a new inode in the lower dir is not seen while processes
hold the old one, so the container restarts):

```bash
sudo install -m644 aosp/out/target/product/pc_x86_64/vendor/lib64/waydroid/vulkan.nvidia.waydroid.so \
    /var/lib/waydroid/overlay/vendor/lib64/hw/vulkan.nvidia.so
sudo systemctl restart waydroid-container.service
```

`adb` reaches the container without root (`adb connect <IP>:5555`, the IP from `waydroid status`):
`adb logcat`, `adb shell setprop debug.nvvk.astc_trace 1` before starting the game. Files the
game writes under `/data` are on the host in `~/.local/share/waydroid/data` (owned by the app's
uid).

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
removes `waydroid.gralloc.vtest` and `mesa.vtest.socket.name` (the Venus setup's), then runs `waydroid upgrade --offline` and restarts
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
- 32-bit apps have no NVIDIA driver (NVIDIA's userspace here is x86_64 only); arm64 apps reach
  it through libhoudini's native bridge (Where Winds Meet, above).
