# NVIDIA RTX 4070 (Phase G)

Running log for Phase G of `PLAN.md`: driving the PC's RTX 4070 (AD104, PCI `10de:2786`,
HDMI/DP audio function `10de:22bc`) from Android instead of the UEFI GOP framebuffer
(`pcfb`). One section per step.

## G1. nouveau + GSP on the GKI kernel

### What was added

| Piece | Where | What |
|---|---|---|
| Kernel config | `aosp-device/kernel/pc_x86_64.fragment` | `CONFIG_DRM_NOUVEAU=m` (CH7006/SIL164 TV encoders and WMI_BMOF off) |
| Vendor modules | `aosp-device/kernel/BUILD.bazel` (`_PC_MODULES`) | `nouveau.ko` plus what it selects that the GKI image lacks: `drm_display_helper`, `drm_exec`, `drm_gpuvm`, `drm_ttm_helper`, `gpu-sched`, `i2c-algo-bit`, ACPI `video`, `wmi`, `mxm-wmi` |
| Firmware | `tools/gki-build.sh` step `firmware` → `/vendor/firmware/nvidia/ad104/gsp/` | GSP-RM **570.144**: `booter_load`, `booter_unload`, `bootloader`, `gsp` (decompressed; 61 MiB for `gsp-570.144.bin`) |
| Module blocklist | `aosp-device/pc/modules.blocklist` (BoardConfig.mk) | Cuttlefish's list plus `nouveau.ko`: the boot-time load (`dlkm_loader`) skips it |
| Boot entry | `tools/mkusb-aosp17.sh --menu` | "Android 17, NVIDIA nouveau (test, no Android picture)" |
| Init | `aosp-device/pc/init.pc.rc`, `pcgpu.sh` | loads nouveau, gives minigbm nouveau's card node, keeps a mode on the monitor |
| Logs | `aosp-device/pc/pclog.sh` | GPU/HDA PCI functions, nouveau/GSP dmesg, `/sys/class/drm/*` status and modes, `modetest -c`, raw EDID files, full ELD of the `10de:22bc` codec |

The GKI image already has DRM, the KMS helpers, TTM, backlight, I2C, HMM, IOMMU and the
firmware loader built in, so only modules were needed. Kconfig also selects `FW_CACHE` and
`SND_HDA_COMPONENT`: `FW_CACHE` only changes code inside vmlinux (the firmware loader), so
the real kernel keeps it off and nothing in the modules depends on it; `SND_HDA_COMPONENT`
changes `snd-hda-core` and the HDMI codec drivers, which are our vendor modules anyway, and
is what lets `snd-hda-codec-nvhdmi` take the monitor's ELD from nouveau through the DRM
audio component.

Firmware: nouveau (`nvkm/subdev/gsp/ad102.c`) tries GSP-RM 570.144 first, then 535.113.01.
In linux-firmware `nvidia/ad104` is a symlink to `ad102`, and `gsp-570.144.bin` a symlink
to `ga102`'s; `zstd -d` follows both, so the image has plain files at the paths nouveau
asks for. Only the 570.144 set ships (the 535 set would add another 40 MiB). The GKI image
has no `FW_LOADER_COMPRESS`, so the files must be decompressed.

### The `nouveau` boot entry

The Limine menu (`mkusb-aosp17.sh --menu`) gets one more entry; the default entry stays
`pcfb`. Its command line adds:

- `pcfb.enable=0`: pcfb does nothing, so no driver touches the GOP framebuffer that
  nouveau takes over.
- `androidboot.android_pc.gpu=nouveau` (`ro.boot.android_pc.gpu`, the `androidpc.gpu`
  switch of `PLAN.md`): `init.pc.rc` runs `modprobe nouveau.ko` in `early-init` (modprobe
  with an explicit name ignores the blocklist; its dependencies come from `modules.dep`).
- `androidboot.hardware.hwcomposer.mode=noop` and `...display_finder_mode=noop`: the ranchu
  composer only drives a DRM device named `virtio*` (pcfb's disguise), so until G2 brings a
  KMS composer, Android runs on the composer's no-op display (720x1280, nothing on the
  monitor). `display_finder_mode` moved from the shared bootconfig to each entry's command
  line for this (a bootconfig value would win over the command line).
- `androidboot.selinux=permissive`.

So that the framework still boots (the audio test below needs `system_server`), minigbm
needs a device that allocates: nouveau's render node refuses dumb buffers (the ioctl is not
`DRM_RENDER_ALLOW`), so `init.pc.rc` sets `/dev/dri/renderD128` to mode 0000 after coldboot;
minigbm (user `system`) cannot open it and falls back to the card node, where its
`nouveau` backend allocates dumb buffers.

`pcgpu.sh` (service `vendor.pcgpu`, started at `post-fs-data`) runs `modetest -M nouveau -r`
with its stdin held open: the preferred mode on every connected connector, with a test
pattern. That lights the HDMI/DP link, so the NVIDIA HD Audio codec gets an ELD, the
codec's jack switches on, `WiredAccessoryManager` connects `HDMI Out`, and the F9h Rust
audio HAL plays to the monitor. If modetest exits (no monitor yet), it is retried every
10 s; its output goes to `/data/vendor/pcgpu/` and into pclog's state dumps.

What to expect on the real PC: the monitor shows the GOP picture, then a modetest colour
pattern once nouveau and `pcgpu.sh` are up (no Android UI on this entry). pclog's HDMI check
beeps on each HDMI PCM at 150 s, and its audio check plays 440 Hz through the audio HAL at
240 s and 480 s.

`modetest` comes from libdrm and lands in `/system/bin` (`pc.mk`): the generated vendor
partition takes a `PRODUCT_PACKAGES` name in all its variants, so the system variant is
installed and allowed past `generic_system.mk`'s artifact path requirement.

### QEMU check (2026-11-14)

QEMU has no NVIDIA GPU; the check is that both entries still boot and nouveau is harmless.
Image `out/aosp17-usb-g1-qemu.img` (`--menu --append console=ttyS0,115200`), booted with
`NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh`:

- Default entry: `sys.boot_completed=1`, lock screen drawn through pcfb, nouveau not
  loaded (blocklist works); its helper modules load and sit unused.
- nouveau entry (the test copy's `limine.conf` set to `default_entry: 5`): command line has
  `pcfb.enable=0 androidboot.android_pc.gpu=nouveau ...mode=noop ...display_finder_mode=noop`
  after the base `pcfb.enable=1` (the later one wins); `nouveau.ko` loaded by `init.pc.rc`
  and idle (no device to bind), pcfb loaded but inert, `ro.vendor.hwcomposer.mode=noop`,
  SurfaceFlinger on the composer's `EMU_display_0`, `sys.boot_completed=1`,
  `vendor.pcgpu` running (retrying modetest, no card). In QEMU minigbm uses vkms's card
  node; on the PC it gets nouveau's.
- Rerun after the host reboots (builds `eng.caoanh.20261114.1255` and `.1305`): same result, with
  `/system/bin/modetest` present. It showed that `pcgpu.sh`'s first version (`sleep | modetest`)
  never retried, because the pipeline waits for the `sleep`. The script now holds modetest's
  stdin through a FIFO and kills the holder when modetest exits. In QEMU it retries every
  10 s (`modetest exited (255)`, no nouveau card). On `.1305` the default entry
  still uses pcfb and does not load nouveau. Release image `out/aosp17-usb-g1.img` is
  build `.1305`.

### Status

**Code done, real-PC test pending** (G1–G5 are tested on the PC together).

### Real-PC test

Release image for the stick (same `mkusb` options as the stick, tiny `userdata`, which
the update never copies):

```bash
tools/mkusb-aosp17.sh --menu --userdata 64 out/aosp17-usb-g1.img
```

Find the stick (`lsblk -o NAME,SIZE,MODEL,TRAN`, a ~57 GB SanDisk on `usb`), check, then
update it in place (keeps apps and data; docs/usb-boot-aosp17.md §10):

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g1.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g1.img /dev/sdX
```

On the PC (monitor on the RTX 4070's HDMI or DP port, speakers on in the monitor):

1. Boot the default entry once: Android as before (pcfb), nouveau not in `lsmod`.
2. Reboot, pick **"Android 17, NVIDIA nouveau (test, no Android picture)"** in the Limine
   menu. Expected: the GOP picture, then a black screen while nouveau boots GSP, then a
   modetest colour pattern at the monitor's native mode. At about 150 s a beep on each
   HDMI PCM (pclog `hdmi-check.txt`), at 240 s and 480 s a 440 Hz tone through the Rust
   audio HAL (`audio-check.txt`). Note what was seen and heard.
3. Leave it running about 10 minutes (the 600 s pclog dump), then reboot into the default
   entry and plug the stick into the build host.
4. Read the logs: `tools/pclog-read.sh /dev/sdX`, then in the nouveau boot's directory:
   - `dmesg.txt`: `nouveau ... gsp: RM version 570.144`-style lines, no `gsp` errors or
     timeouts, `pcgpu:` lines;
   - `state-*.txt` sections `== pci display`, `== nouveau dmesg`, `== drm connectors`
     (the monitor's connector `connected`, its native mode first), `== modetest -c`,
     `== eld card1 (10de:22bc)` (`monitor_present 1`, `eld_valid 1`, the monitor's name),
     `== audio devices` (HDMI connected), `== pcgpu`;
   - `edid-card0-*.bin` (`edid-decode` on the host);
   - `hdmi-check.txt` and `audio-check.txt` (`play` must report device type 9, HDMI).

Done when GSP boots, the connector is connected at its native mode, and the 440 Hz tone
from the audio check plays on the monitor.

Fallbacks if it fails: firmware not found → check the path in dmesg against
`/vendor/firmware/nvidia/ad104/gsp/`; GSP boot timeout → try the 535.113.01 set (add it to
`tools/gki-build.sh`); framework does not boot on this entry → the test pattern and the
ALSA beep in `hdmi-check.txt` still show whether the link and ELD work.

## G2. Android display on KMS

### What changed

| Piece | Where | What |
|---|---|---|
| Composer choice | `tools/mkusb-aosp17.sh` | `androidboot.vendor.apex.com.android.hardware.graphics.composer` moved from the shared bootconfig to each entry's command line (a bootconfig value wins over the command line: init reads bootconfig first and `ro.*` cannot change). `ranchu` unless the entry names another APEX |
| Boot entries | `tools/mkusb-aosp17.sh --menu` | **"Android 17, NVIDIA nouveau"** now runs Android on drm_hwcomposer; **"…, VTS run"** the same plus the graphics VTS; **"…, modetest only (no Android picture)"** is G1's test (ranchu no-op display, `pcgpu.sh`) |
| drm_hwcomposer | `aosp-device/pc/init.pc.rc` | the APEX Cuttlefish already ships (`com.android.hardware.graphics.composer.drm_hwcomposer`), on `/dev/dri/card0` (Cuttlefish's `vendor.hwc.drm.device`; nouveau is card0 there: pcfb off, vkms blocklisted), `ro.vendor.hwc.use_overlay_planes=0` |
| Gralloc | minigbm (unchanged choice) | its `nouveau` dumb backend; on nouveau's render node, which G1 had closed (`chmod 0000`). `DRM_IOCTL_MODE_CREATE_DUMB` and `MAP_DUMB` are `DRM_RENDER_ALLOW` in android17-6.18, so the render node allocates; the chmod is gone (G3's NVK needs the render node too) |
| minigbm patch | `tools/patches/aosp17-minigbm-drop-master.patch` (`git apply` in `aosp/external/minigbm`, after `aosp17-minigbm-rw-lock.patch`) | the allocator drops DRM master after opening a node (see QEMU check) |
| `pcgpu.sh` | `init.pc.rc` | starts only with `androidboot.android_pc.kms=modetest`: modetest holds DRM master, which drm_hwcomposer needs |
| VTS | `mkusb-aosp17.sh`, `pclog.sh` | `VtsHalGraphicsComposer3_TargetTest`, `VtsHalGraphicsAllocatorAidl_TargetTest`, `VtsHalGraphicsMapperStableC_TargetTest`, when built, go into the ESP's `android/vts/` (the ESP is rewritten by every update, unlike pclog). On the "VTS run" entry `pclog.sh` copies them to `/data/local/tmp/vts` at 200 s and runs allocator, mapper, then composer (SurfaceFlinger stopped for it, started again after), each into `boot-N/vts-NAME.txt`, with a summary in `boot-N/vts.txt` |
| pclog | `pclog.sh` | `== composer` (APEX choice, process, drm_hwcomposer's own dumpsys), `== displays` (DisplayManager modes) |

How the pieces meet the G2 goals:

- **Native mode and refresh rate**: drm_hwcomposer offers every connector mode and marks
  the EDID preferred one; SurfaceFlinger starts on it. The density stays at
  `androidboot.lcd_density` (240).
- **Hotplug, second monitor**: drm_hwcomposer listens for DRM uevents and reports
  connectors coming and going; the first connected connector is the internal display, a
  second one an external display (desktop mode extends onto it).
- **Client composition on SwiftShader**: overlay planes off, so SurfaceFlinger composes
  every layer itself (SwiftShader through ANGLE) into one buffer on the primary plane.
- **Audio follows hotplug**: drm_hwcomposer's modeset lights the HDMI/DP link, the
  NVIDIA codec gets the ELD, its jack switches on, and `WiredAccessoryManager` connects
  `HDMI Out`; at each connect the F9h Rust audio HAL picks the HDMI PCM whose jack is on
  (`Endpoint::connect_hdmi`). A screen that Android turns off disables the CRTC, so the
  monitor's sound stops with it, as on any KMS Linux desktop.

Known risk for the real PC: dumb buffers live in VRAM (`nouveau_display_dumb_create`), so
the CPU (SwiftShader) writes and reads them through the BAR1 write-combined mapping.
Writing is fine; reading back (SurfaceFlinger sampling app buffers) is slow. If the
desktop is sluggish, the next step is a minigbm `nouveau` backend that allocates GART
buffers with `DRM_NOUVEAU_GEM_NEW` for buffers that are not scanned out, which G3 needs anyway
for NVK interop.

### QEMU check (2026-11-14)

Build `eng.caoanh.20261114.1410`, image made with `--menu --append console=ttyS0,115200`,
each entry booted with `WRITABLE=1 NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh` (E-cores 12-15,
4 vCPUs, 4 GiB) after setting `default_entry` in a copy's `limine.conf`. QEMU has no NVIDIA
GPU, but Cuttlefish's `init.cutf_cvm.rc` loads vkms when no `card0` appears, so the nouveau
entries ran drm_hwcomposer and minigbm on vkms: the same composer, gralloc and VTS path,
on another KMS driver.

- **Default entry (pcfb)**: unchanged. `sys.boot_completed=1`, ranchu composer, 1280x800
  GOP mode, nouveau not loaded.
- **"NVIDIA nouveau"**: first build (`.1305`-based) booted, but drm_hwcomposer logged
  `DRM/KMS master access required` and fell back to its headless null display. The
  minigbm allocator had opened `card0` first (vkms has no render node) and so became DRM
  master. Fixed with `aosp17-minigbm-drop-master.patch`. On nouveau, minigbm uses the render
  node and never takes master, but the patch keeps the KMS composer safe on any card. After
  the fix: `drmhwc: Found Backend 'generic' for 'Virtual-1' and driver 'vkms'`, the connector
  is `connected enabled`, SurfaceFlinger runs on HWC display 0 at the connector's preferred
  mode (1024x768, 60 Hz), and `sys.boot_completed=1`. The only errors are composer calls
  drm_hwcomposer does not support (display identification without EDID, decoration,
  per-frame metadata). The only tombstones are Bluetooth's (no controller in QEMU, as before).
- **"NVIDIA nouveau, VTS run"**: all three suites pass on vkms + minigbm:

  | Suite | Passed | Skipped | Failed |
  |---|---|---|---|
  | `VtsHalGraphicsAllocatorAidl_TargetTest` | 11 | 2 (RAW10/RAW12) | 0 |
  | `VtsHalGraphicsMapperStableC_TargetTest` | 63 | 18 (mapper v6 calls, RGB formats minigbm lacks) | 0 |
  | `VtsHalGraphicsComposer3_TargetTest` | 163 | 10 (LUTs, mixed colour spaces, multi-display config) | 0 |

  The composer suite took 210 s. SurfaceFlinger came back afterwards (desktop at 600 s and
  900 s).
- The "modetest only" entry was not booted again. Its only change is the trigger
  (`androidboot.android_pc.kms=modetest` instead of the gpu property).

The host froze once (13:25) right when a QEMU run started after a build; that was
before `tools/run-qemu-aosp17-usb.sh` pinned QEMU to the E-cores (commit 5114656). Every
run since then has been pinned with 4 vCPUs, and none froze.

### Status

**Code done, real-PC test pending.** Release image `out/aosp17-usb-g2.img` (build
`eng.caoanh.20261114.1410`), which also carries G1.

### Real-PC test (G1 + G2)

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g2.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g2.img /dev/sdX
```

(`/dev/sdX` is the stick: `lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`.)

Checklist, monitor on the RTX 4070 with its speakers on, a second monitor at hand:

1. **Default entry**: Android as before on pcfb (no regression).
2. **"Android 17, NVIDIA nouveau"**: the GOP picture, a short black screen while GSP
   boots, then the boot animation and the desktop **at the monitor's native mode**
   (e.g. 2560x1080 at its top refresh rate, not the GOP mode). Check that the mouse,
   keyboard and windows work and that it feels as fast as pcfb (VRAM readback risk above).
3. **Sound**: play a video or let pclog's audio check run (240 s and 480 s, 440 Hz). It
   should come out of the monitor, and Settings → Sound should show HDMI.
4. **Hotplug**: unplug the monitor cable for 10 s and plug it back in. The picture and
   sound must come back.
5. **Second monitor**: plug it into another port of the 4070. It should show as an
   external display (desktop mode can move windows there). Unplug it again.
6. Note the times of steps 4-5, use it for about 10 minutes, then reboot.
7. **"…, VTS run"** (optional, about 10 minutes): at 200 s the screen goes black for about
   4 minutes during the composer test, then the desktop comes back.
8. If step 2 shows no picture: **"…, modetest only"** is G1's test (colour pattern and
   beep), which tells a nouveau problem from a composer problem.
9. On the build host: `tools/pclog-read.sh /dev/sdX`, then per boot:
   - `state-*.txt`: `== composer` (drm_hwcomposer process and its dumpsys: connectors,
     modes), `== displays` (active mode = native), `== drm connectors`, `== eld …`,
     `== audio devices` (HDMI connected), `== nouveau dmesg`;
   - `logcat.txt`: `drmhwc` lines (`Attaching connector`, hotplug), no
     `master access required`;
   - `vts.txt` and `vts-*.txt` from the VTS boot;
   - `audio-check.txt` (`play` on device type 9, HDMI) and `hdmi-check.txt`.

Done when the desktop runs on KMS at the monitor's native mode with sound, and the
default (pcfb) entry still works.

## G3. NVK Vulkan on Android

### What changed

| Piece | Where | What |
|---|---|---|
| Soong build of NVK, NAK, NIL | `tools/patches/aosp17-mesa-nvk.patch` (`git apply` in `aosp/external/mesa3d`) | `src/compiler/rust/Android.bp` (the Rust compiler helpers NAK uses) and `src/nouveau/Android.bp`, generated by `src/nouveau/android_bp_gen.py` from the `meson.build` files of `src/nouveau` (rerun it after a Mesa update). The tree's own `Android.bp` files come from meson2hermetic, whose AOSP config has no nouveau; running that tool (a personal GitHub fork) was refused, so these are hand-written in the same style |
| Class headers, Rust class modules | `src/nouveau/headers/android_gen.py` | one generator call per genrule instead of meson's one custom_target per class; the class list and "previous class" rule are read from `headers/meson.build` |
| `nil.h` | `src/nouveau/nil/android/nil.h` | meson makes it with cbindgen, which Soong lacks and which could not be run here; hand-written from the `#[repr(C)]` types and `#[no_mangle]` functions. `nil_layout_check.c` and `layout_check.rs` (`--cfg mesa_soong`) assert the same sizes and offsets on both sides, so a mismatch fails the build |
| NVK's internal kernels | `src/nouveau/vulkan/cl/android/nvkcl.spv`, `tools/mesa-nvkcl-spv.sh` | meson compiles `vulkan/cl/*.cl` with `mesa_clc` (clang + SPIRV-LLVM-Translator + libclc), which neither Soong nor the host has. The script uses the host clang's own SPIR-V backend (LLVM 23) and `spirv-link`, with `mesa_clc`'s flags; `vtn_bindgen2` (built as a host tool in Soong) turns the result into `nvkcl.cpp/.h`. The LLVM backend writes no parameter names, so `vtn_bindgen2` now names them `argN`. Rerun the script when the kernels change |
| Gralloc interop | `src/util/u_gralloc/u_gralloc_minigbm.c` | a u_gralloc backend that reads minigbm's `cros_gralloc_handle` directly (format, modifier, plane fds/offsets/strides), first in the list when `ro.hardware.gralloc=minigbm`. The CrOS backend needs the gralloc0 module and a buffer imported through it, the IMapper backends need libui (not available to vendor code), and the fallback reports no modifier, which NVK cannot import |
| Modifiers | minigbm's `nouveau` (dumb) backend, NVK | minigbm allocates `DRM_FORMAT_MOD_LINEAR` buffers (dumb BOs, pitch aligned to 256 B); NVK imports them as linear images. NVK renders to a linear image only without a depth attachment and with a 128 B aligned pitch; otherwise it renders into a tiled shadow and copies. That shadow was only made for `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` images, so linear swapchain images (`VkNativeBufferANDROID`) now get one too |
| `AHardwareBuffer` | `nvk_image.c`, `nvk_device_memory.c`, `nvk_buffer.c`, `nvk_physical_device.c` | `VK_ANDROID_external_memory_android_hardware_buffer`: image/buffer format properties through the runtime's AHB helpers (AHB images: 1 level, 1 layer, 1 sample); AHB import and export in `vkAllocateMemory` import the AHB's dma-buf (the fd stays the AHB's); a dedicated AHB image gets the buffer's layout at allocation (`nvk_image_init_from_ahb`, an explicit-modifier re-init, like the ANB path) |
| System Vulkan HAL | `tools/patches/aosp17-cf-vulkan-nouveau.patch` (`aosp/device/google/cuttlefish`) | Cuttlefish's `com.google.cf.vulkan` APEX sets `ro.vulkan.apex`, so libvulkan loads `vulkan.<ro.hardware.vulkan>.so` only from that APEX; `vulkan.nouveau` (64-bit only: NAK is a Rust staticlib) goes into it next to ranchu and pastel |
| Vulkan driver per entry | `tools/mkusb-aosp17.sh` | `androidboot.hardware.vulkan` left the shared bootconfig for each entry's command line (like G2's composer); `pastel` (SwiftShader) unless the entry names another |
| Boot entry | `mkusb-aosp17.sh --menu` | **"Android 17, NVIDIA nouveau, NVK Vulkan (test)"**: G2's nouveau entry plus `androidboot.hardware.vulkan=nouveau`. Android's GLES is ANGLE, which runs on the system Vulkan driver, so on this entry SurfaceFlinger and apps compose on NVK as well (G4's subject); "Android 17, NVIDIA nouveau" keeps SwiftShader as the safe entry |
| On-device checks | `aosp-device/vktest/` (`vktest.mk`) | `pcvktest.sh` starts when gpuservice runs on the NVK entry and writes `boot-N/vk/` in pclog: `props.txt`, `vkjson.json` (`cmd gpu vkjson`, Android's vulkaninfo). After `sys.boot_completed`, once per build, it installs the dEQP APK from `/system_ext/etc/pcvktest/` and runs the smoke groups through `DeqpInstrumentation` (so `wsi.android` gets a window): `api.smoke`, `api.info`, `api.device_init`, `api.external.memory.android_hardware_buffer`, `memory.allocation.basic`, `memory.requirements`, `memory.mapping.suballocation.full`, `wsi.android`, one hour cap each; `summary.txt`, `deqp-GROUP.qpa`, `failures-GROUP.txt` |
| dEQP APK | `tools/patches/aosp17-deqp-apk-64bit.patch` (`aosp/external/deqp`) | `com.drawelements.deqp` built 64-bit only (99 MB in system_ext) |

### Building it

- Apply the three patches above (the tree in this checkout has them applied).
- Soong analysis with all of Mesa's NVK modules OOMs at the 20 GB build cap with
  incremental analysis on: build with `SOONG_INCREMENTAL_ANALYSIS=false` and
  `SOONG_GOMEMLIMIT=14GiB` (analysis then peaks under the cap).
- `m vulkan.nouveau` gives `vendor/lib64/hw/vulkan.nouveau.so`, 14 MB, with NAK and NIL
  linked in; `m com.google.cf.vulkan pcvktest.sh pc_deqp_apk`, then the usual image steps.

### QEMU check (2026-11-14)

Build `eng.caoanh.20261114.1832`, image `--menu --append console=ttyS0,115200`, booted with
`WRITABLE=1 NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh` (E-cores 12-15, 4 vCPUs, 4 GiB).
QEMU has no NVIDIA GPU, so this checks packaging and that nothing else broke, not NVK.

- **"NVK Vulkan (test)"**: `ro.hardware.vulkan=nouveau`, libvulkan loads `vulkan.nouveau`
  from the APEX without a linker or HAL error; with no nouveau device it reports none
  (`vkjson.json` is `{}`), ANGLE then fails `VK_ERROR_INITIALIZATION_FAILED`, and
  SurfaceFlinger aborts in a loop ("no suitable EGLConfig"), as expected without the GPU.
  No crash inside `vulkan.nouveau`. `pcvktest.sh` started at 14 s and wrote `boot-1/vk/`,
  then gave up on dEQP after 10 minutes without `sys.boot_completed`.
- **"NVIDIA nouveau"** (vkms in QEMU): unchanged from G2 with the Vulkan driver now set on
  its command line: `ro.hardware.vulkan=pastel`, drm_hwcomposer up (`hwc3-drm starting
  up`), `sys.boot_completed=1`. Only Bluetooth tombstones (no controller in QEMU).
- **Default entry (pcfb)**: `ro.hardware.vulkan=pastel`, `sys.boot_completed=1`. Only the
  known Bluetooth and `dlkm_loader` aborts (coretemp has no device in QEMU, see
  `docs/usb-boot-aosp17.md`).

### Status

**Code done, real-PC test pending.** Release image `out/aosp17-usb-g3.img` (build
`eng.caoanh.20261114.1832`), which also carries G1 and G2.

### Real-PC test

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g3.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g3.img /dev/sdX
```

(`/dev/sdX` is the stick: `lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`.)

Checklist:

1. **"Android 17, NVIDIA nouveau"** first: the G2 checks (desktop at the native mode,
   SwiftShader). This is the fallback if the NVK entry fails.
2. **"Android 17, NVIDIA nouveau, NVK Vulkan (test)"**: the boot animation and the desktop
   should come up as on entry 1, now drawn by ANGLE on NVK (a first taste of G4). Leave
   it alone for about 30 minutes after the desktop appears: from about a minute after
   boot, `pcvktest.sh` installs the dEQP app and runs the smoke groups, which open and
   close a test window several times. `dmesg` (`pcvktest:` lines) shows its progress. If
   the desktop never comes up, wait 10 minutes anyway (vkjson is written first), then
   reboot.
3. Reboot once more into the NVK entry. dEQP does not run again for the same build, but
   another `boot-N/vk/vkjson.json` is written.
4. On the build host: `tools/pclog-read.sh /dev/sdX`, then for the NVK boot:
   - `vk/vkjson.json`: `deviceName` "NVIDIA GeForce RTX 4070" (NVK names it from the chip:
     "NVIDIA AD104" or similar), `driverName` "NVK", API version 1.4;
   - `vk/summary.txt`: pass / fail / not supported per group; `vk/failures-*.txt`
     lists the failing cases; `vk/deqp-*.qpa` are the full logs;
   - `vk/props.txt`: `ro.hardware.vulkan=nouveau` and libvulkan's load line;
   - `logcat.txt`: `MESA`/`nvk` lines, `Using minigbm native handles`, ANGLE errors;
     `tombstones/` for crashes in `vulkan.nouveau`;
   - `dmesg.txt`: nouveau channel errors or GSP faults (`nouveau ... fault`, `gsp`).

Done when `vkjson.json` shows the RTX 4070 and the `api`, `memory` and `wsi.android`
smoke groups have no failures.

Known risks:

- `nvkcl.spv` comes from LLVM's SPIR-V backend, not SPIRV-LLVM-Translator. A bad
  translation would show up only in query copies (`vkCmdCopyQueryPoolResults`) and
  indirect copies. The `api` group does not use them much.
- `nil.h` is hand-written. Its layout is checked at build time, but enum values are not.
  The enum values were copied from the Rust order.
- NVK reads dumb buffers in VRAM through BAR1, which can be slow (see G2).
