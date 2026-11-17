# NVIDIA's own driver in Waydroid

These scripts make Android apps in Waydroid draw on an NVIDIA GPU (tested on an RTX 4070) with
NVIDIA's own proprietary userspace driver. The driver is loaded inside each app's process, with no
render server on the host: `vulkan.nvidia` (this project's Vulkan HAL) reaches
`libGLX_nvidia.so.0` through nvglibc, a glibc-on-bionic shim. GLES (SurfaceFlinger, GL apps)
runs on ANGLE, on top of that Vulkan driver.

```
App / HWUI (Vulkan)                SurfaceFlinger, GLES apps -- ANGLE
  └─ Android 13's Vulkan loader ─────────┘
       └─ /vendor/lib64/hw/vulkan.nvidia.so
            └─ libnvglibc ─► /vendor/lib64/nvidia/libGLX_nvidia.so.0 (the host's NVIDIA version)
gralloc ─► libgbm_mesa_wrapper.so ─► NVIDIA block-linear buffers (nvidia-drm or udmabuf)
hwcomposer ─► dma-buf + NVIDIA modifier ─► the Wayland compositor (on the NVIDIA GPU)
```

The design, the problems found on the way and the test results are in
[docs/waydroid-nvidia.md](../../docs/waydroid-nvidia.md).

## Requirements

- **Waydroid** with its LineageOS 20 (Android 13, x86_64) image, initialised (`waydroid init`).
- **The host's NVIDIA driver:** `nvidia-open` loaded, with the GPU bound to `nvidia` and not to
  `vfio-pci` (`tools/run-qemu-aosp17-vfio.sh --release` gives it back after a VFIO VM). The
  build copies the host's own NVIDIA userspace libraries. Their version must match the kernel
  module, so rebuild after every driver update.
- **`/dev/udmabuf`** (`sudo modprobe udmabuf`).
- **A Wayland compositor running on the NVIDIA GPU** (Hyprland here). A compositor on an iGPU
  cannot show NVIDIA block-linear video memory.
- **To build:**
  - this repository's AOSP 17 tree in `aosp/` (`tools/aosp17-build.sh`);
  - the Android NDK in `/opt/android-ndk` (or set `NDK=`);
  - Rust/cargo;
  - `debugfs` (e2fsprogs).

NVIDIA's libraries are copied from the host into the overlay. They are never committed or
redistributed.

## Build

```bash
tools/waydroid/nvidia-build.sh
```

The script runs in this order:

1. builds libnvglibc, `vulkan.nvidia.waydroid` and `nvglibc_vktest` in the AOSP tree, through
   `tools/limited-build.sh`, one build at a time. Pass `--no-aosp` to reuse what `aosp/out`
   already has;
2. builds `nvtls-rewrite` with cargo;
3. rewrites NVIDIA's libraries for bionic's static TLS;
4. patches Waydroid's own `libvulkan.so` and `libEGL.so`, read from its `system.img`;
5. builds the gralloc wrapper, the composer wrapper and `libnvglibc_tls.so` with the NDK.

The result is `out/waydroid-nvidia/overlay/` (about 240 MB).

Optional host test (no container needed):

```bash
tools/waydroid/nvidia-hosttest.sh
```

It runs `nvglibc_vktest` on Waydroid's own Android 13 linker and libc, in bwrap. It should end
with `PASS`.

## Install

```bash
tools/waydroid/nvidia-install.sh
```

Run it as your user, not with `sudo`: it asks for the password itself. The script then:

- **backs up first.** `waydroid.cfg` and every overlay file it replaces go to
  `/var/lib/waydroid/nvidia-backup/<date>/`, with the list of files it adds;
- **copies the overlay** into `/var/lib/waydroid/overlay`. This includes the init script that
  creates `/dev/nvidia*`, `/dev/udmabuf` and the NVIDIA render node inside the container;
- **sets the properties** in `waydroid.cfg`: `ro.hardware.vulkan=nvidia`,
  `ro.hardware.egl=angle`, `gralloc.gbm.device=/dev/dri/renderD129`,
  `ro.zygote.disable_gl_preload=true`, `debug.hwui.renderer=skiavk` and
  `debug.nvvk.pipeline_cache_max=262144`;
- **removes the old Venus/vtest properties**, if any;
- **restarts the container**: it runs `waydroid upgrade --offline` and restarts
  `waydroid-container`.

Then start Waydroid as usual (`waydroid show-full-ui`).

Running the script again updates the files and keeps the first backup.

### Check

```bash
sudo waydroid shell -- cmd gpu vkjson < /dev/null | cat | grep -E 'deviceName|driverInfo'
```

This should show `NVIDIA GeForce RTX 4070` (your GPU) and the NVIDIA driver version.
`dumpsys gfxinfo <package>` of an app shows `Pipeline=Skia (Vulkan)`.

### Uninstall

```bash
tools/waydroid/nvidia-install.sh --uninstall
```

This restores the newest backup: it removes the files the install added, puts back the replaced
ones and `waydroid.cfg`, and restarts the container.

## Games: Where Winds Meet

Where Winds Meet (`com.netease.yyvn`, arm64 through libhoudini) renders on Vulkan only when its
`LocalData/vulkan.tag` file exists. Without it, the game uses ES3 through ANGLE and crashes about
two minutes in, with SIGFPE in ANGLE's `UtilsVk::copyImageBits`.

The overlay's init script creates the empty tag once user 0's storage is unlocked. This works only
if the game has already run once, so start the game once, then restart Waydroid. To check that the
game uses Vulkan:

```bash
sudo waydroid shell -- cat /sdcard/Android/data/com.netease.yyvn/files/LocalData/last_graphics_api.tag < /dev/null | cat
```

`vlk` means Vulkan; `es3` means the tag is still missing. To turn the tag off, set
`persist.vendor.android_pc.wwm_vulkan=0` and delete the tag.

## Logs and troubleshooting

- **Reading logs needs root:** `sudo waydroid logcat`. When you script `sudo waydroid shell`,
  give it `< /dev/null` and pipe its output through `cat`. Otherwise it makes the file used as
  its stdin or stdout root-owned.
- **Install says there is no NVIDIA render node:** the GPU is still on `vfio-pci`, or the
  `nvidia` module is not loaded.
- **Apps crash right after an NVIDIA driver update:** the overlay has the old userspace. Run
  `nvidia-build.sh --no-aosp`, then `nvidia-install.sh`.
- **`vulkan.nvidia: slow ASTC decode ...` warnings are expected.** The RTX 4070 has no ASTC, so
  ASTC textures are decoded on the CPU.

## Limits

- **Only 64-bit x86_64 code gets the NVIDIA driver,** because NVIDIA's userspace is x86_64 only.
  arm64 apps reach it through libhoudini's native bridge; 32-bit apps get no NVIDIA driver.
- **YUV buffers (video, camera) stay linear on the iGPU.**
