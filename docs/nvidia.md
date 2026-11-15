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
  `androidboot.lcd_density` (160 since H4, 240 before).
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

### Real-PC result and fix (Phase I1, 2026-11-15)

The first real-PC run (build `eng.caoanh.20261115.0441`, pclog `boot-7` "NVIDIA nouveau,
VTS run", `boot-11` NVK, `boot-8` modetest) showed no picture on the nouveau and NVK
entries, although nouveau and GSP-RM 570.144 came up fine (`NVIDIA AD104`, 12282 MiB VRAM)
and the modetest entry lit the monitor with its colour pattern.

Root cause: the card order. Cuttlefish's `init.vendor.rc` (installed as
`init.cutf_cvm.rc`) does `wait /dev/dri/card0` and then `modprobe vkms.ko` in `early-init`,
before `init.pc.rc`'s actions. On the nouveau entries there is no card yet (pcfb is off and
nouveau is loaded by `init.pc.rc` afterwards), so the wait times out after 5 s, vkms becomes
**card0** and nouveau **card1** (`[drm] Initialized nouveau 1.4.0 ... on minor 1`).
drm_hwcomposer opens `vendor.hwc.drm.device=/dev/dri/card0`, i.e. vkms:
`drmhwc: Found Backend 'generic' for 'Virtual-1' and driver 'vkms'`. Android ran on vkms's
virtual connector (`card0-Virtual-1` connected, every `card1-*` NVIDIA connector unused,
nothing ever set a mode there). In QEMU (G2's check) vkms is the only card, so the bug never
showed there; the modetest entry worked because `modetest -M nouveau` picks the card by driver.

Fix (`aosp-device/pc/init.pc.rc`, nouveau entries only): unload vkms, load nouveau (DRM gives
it the lowest free minor, card0), then load vkms again (card1, unused on the PC; card0 in
QEMU, where the entry keeps running on it as before). minigbm is unaffected (it takes the
first render node, nouveau's `renderD128`; vkms has none), and `pcgpu.sh` finds nouveau by
driver name.

The beeps on the modetest entry were pclog's audio check (a 5 s 440 Hz tone, H1's test,
whenever `audio-check.dex` is on the pclog partition), not the GPU. The modetest entry and the
audio check are passed tests left for I4 to remove (`PLAN.md`, Phase I).

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

## G4. GLES through ANGLE, GPU composition

### What changed

On Cuttlefish's graphics setup, Android's GLES is already ANGLE
(`androidboot.hardware.egl=angle`, shared bootconfig) and ANGLE runs on the system Vulkan
driver. So moving the Vulkan driver of an entry from SwiftShader (`pastel`) to NVK moves
everything that draws on that entry to the GPU:
- SurfaceFlinger's RenderEngine (Skia Ganesh on GL: the client composition that
  drm_hwcomposer gets, since overlay planes are off);
- HWUI in every app;
- Chromium/WebView's GPU process;
- the games.

G4 turns G3's test entry into the GPU entry, checks GLES on it, and measures it against
pcfb.

| Piece | Where | What |
|---|---|---|
| Boot entry | `tools/mkusb-aosp17.sh --menu` | G3's "NVIDIA nouveau, NVK Vulkan (test)" is now **"Android 17, NVIDIA GPU (NVK + ANGLE)"**: nouveau, drm_hwcomposer, `androidboot.hardware.vulkan=nouveau`. libvulkan loads only `vulkan.<ro.hardware.vulkan>` from the APEX, so SwiftShader is never loaded there. "Android 17, NVIDIA nouveau" keeps SwiftShader on KMS as the fallback; pcfb stays the default entry |
| dEQP-GLES3 | `aosp-device/vktest/pcvktest.sh` | after G3's dEQP-VK groups, the same once-per-build run does the GLES 3.0 smoke groups through `DeqpInstrumentation`, in a window with CTS's config (`rgba8888d24s8ms0`, window surface): `info`, `functional.prerequisite`, `functional.implementation_limits`, `functional.color_clear`, `functional.depth_stencil_clear`, `functional.buffer.write`, `functional.texture.format`, `functional.fbo.completeness`, `functional.draw.draw_arrays`, `functional.shaders.linkage`. Results go to pclog `boot-N/gles/` (`summary.txt`, `failures-*.txt`, `deqp-*.qpa`), plus `gles/egl.txt` (SurfaceFlinger's `GLES:` line, the ANGLE renderer string naming the Vulkan device) |
| Frame-rate benchmark | `aosp-device/vktest/pcgfxbench.sh`, `init.pcvktest.rc` | runs on **every** entry, 90 s after `sys.boot_completed` (on the GPU entry after pcvktest.sh), once per build, Vulkan driver and GPU entry. Runs the San Angeles demo (`angeles`, GLES 1 on screen, fps), then `hwuimacro` scenes `shadowgrid2 rectgrid listview recents savelayer2 backdropblur`, 200 frames each, off screen (throughput) and on screen (through SurfaceFlinger and the composer, vsync-bound). Writes `boot-N/bench/summary.txt` (the entry, the display mode, one fps line per result) and the raw files. Takes about 4 minutes on SwiftShader and draws test scenes over the desktop |
| Benchmark binaries | `aosp-device/vktest/Android.bp` | `angeles` and `hwuimacro` are native tests, which tradefed installs into `/data`. A stick update never writes userdata, so they ship as plain files in `/system_ext/etc/pcvktest/` (`prebuilt_etc` from the test modules), and the script copies them to `/data/local/tmp/pcbench` and makes them executable. `hwuimacro` writes no CSV, only JSON or a table: the script uses JSON |
| Game frame rates | `aosp-device/pc/pclog.sh` | SurfaceFlinger's timestats are switched on at every boot. Each state dump has `== timestats` (frames, missed frames, client-composition frames, and per-layer `averageFPS`). So a game played on both entries can be compared without a separate tool |
| Who renders | `pclog.sh` | `== gles`: the EGL/Vulkan/GLES-version properties and SurfaceFlinger's `GLES:` and RenderEngine lines. `== swiftshader users`: every process that maps a SwiftShader or `vulkan.pastel` library. It is empty on the GPU entry, and that is how "SwiftShader removed" is checked |
| Chromium/WebView | `aosp-device/pc/init.pc.chromium.rc` | on the GPU entry Chromium gets the same `chrome-command-line` as on pcfb (no Skia Graphite, no VulkanFromANGLE), so its GPU process uses Ganesh on the system EGL: ANGLE on NVK. Dawn/Graphite on NVK is untried, and the flags keep the one route known to work. The file lives in `/data` and is shared by all entries, so each entry writes its own |

Not changed, on purpose:
- **`ro.opengles.version` stays 3.1 (196609).** It is in the shared bootconfig, and ANGLE's
  `exposeES32ForTesting` stays off. Claiming 3.2 before dEQP-GLES31/32 runs on NVK could
  make apps ask for contexts that ANGLE cannot create. Raise it per entry after the real-PC
  results.
- **The RenderEngine backend.** SurfaceFlinger picks Skia Ganesh on GL, which here is ANGLE
  on NVK. `pclog`'s `== gles` shows the choice. SkiaVk directly on NVK is a later
  experiment.
- **Overlay planes stay off** (`ro.vendor.hwc.use_overlay_planes=0`, G2). Every layer is
  composed by SurfaceFlinger on the GPU into one buffer for the primary plane.

### QEMU check (2026-11-14)

Build `eng.caoanh.20261114.2006`, image `--menu --append console=ttyS0,115200`, each entry
booted with `WRITABLE=1 NO_WINDOW=1 AUDIO=none tools/run-qemu-aosp17-usb.sh` (E-cores 12-15,
4 vCPUs, 4 GiB). Each run used `default_entry` set in the test image's `limine.conf`. QEMU
has no NVIDIA GPU, so this checks the plumbing and the SwiftShader baseline, not NVK.

- **Default entry (pcfb)**: `sys.boot_completed=1`. `pcgfxbench.sh` ran at 115 s and finished
  in 145 s:

  | | pcfb (1280x800) | NVIDIA nouveau entry on vkms (1024x768) |
  |---|---|---|
  | angeles | 26.3 fps | 26.3 fps |
  | hwui off screen shadowgrid2 / rectgrid / listview / recents / backdropblur | 67.8 / 309.7 / 449.2 / 113.5 / 47.3 | 81.1 / 202.2 / 335.6 / 86.0 / 47.8 |
  | hwui on screen, same scenes | 56.7 / 59.9 / 59.9 / 59.8 / 39.7 | 46.3 / 59.7 / 59.8 / 54.6 / 34.3 |

  `savelayer2` off screen reports about 58 000 fps on both: the scene draws almost
  nothing off screen, so ignore it.
  - `== gles`: `GLES: ... ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 16.0.0)))`.
  - `== swiftshader users`: SurfaceFlinger, system_server, SystemUI, the launcher and the
    other apps, as expected on pcfb.
  - `== timestats`: per-layer FPS (e.g. the benchmark's layer at 26.5 fps during angeles).
  - The first build wrote CSV, which `hwuimacro` does not support ("Unknown format 'csv'").
    It was switched to JSON, and this table is from the rebuild.
- **"NVIDIA nouveau"** (drm_hwcomposer on vkms, SwiftShader): boots, and the benchmark runs
  under its own marker (`bench.pastel-nouveau`). Results are in the table above.
- **"NVIDIA GPU (NVK + ANGLE)"**: `ro.hardware.vulkan=nouveau`. Without an NVIDIA device,
  ANGLE finds no Vulkan GPU and SurfaceFlinger restarts in a loop, as in G3's QEMU run.
  Android never reaches `sys.boot_completed`, so neither dEQP nor the benchmark runs. On the
  PC this entry needs the 4070.

### Status

**Code done, real-PC test pending.** Release image `out/aosp17-usb-g4.img` (build
`eng.caoanh.20261114.2006`), which also carries G1-G3.

### Real-PC test

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g4.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g4.img /dev/sdX
```

(`/dev/sdX` is the stick: `lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`.)

The same steps also cover G3's checklist: the GPU entry runs the dEQP-VK groups before the
GLES ones.

1. **Default entry (pcfb)**. Let it sit at the desktop for about 6 minutes. At about
   1.5 minutes, test scenes appear over the desktop for about 4 minutes (the pcfb baseline
   of `pcgfxbench.sh`). Then play the same thing you will play on the GPU entry for a few
   minutes, for example Delta Force or Genshin Impact up to the first scene, and a YouTube
   video in Chromium.
2. **"Android 17, NVIDIA GPU (NVK + ANGLE)"**. The boot animation and the desktop at the
   monitor's native mode. Then leave it alone for about 45 minutes:
   - `pcvktest.sh` installs dEQP and runs the VK groups, then the GLES3 groups, each opening
     a test window;
   - then `pcgfxbench.sh` draws its scenes (the NVK numbers).

   `dmesg` (`pcvktest:` and `pcgfxbench:` lines) shows progress. If the desktop never comes
   up, wait 10 minutes (vkjson is written first), reboot, and use "NVIDIA nouveau" instead.
3. On the GPU entry, play the same games and video as in step 1:
   - Chromium/WebView: a page with video and WebGL (e.g. a YouTube video, then
     `chrome://gpu` and take a screenshot). It should show hardware acceleration on
     "ANGLE (... NVK ...)".
   - Delta Force, then Genshin Impact.

   Take a screenshot (Print Screen) of anything odd. Use it for 10-15 minutes, then reboot.
4. Optional: **"Android 17, NVIDIA nouveau"** once, for SwiftShader on KMS (third benchmark
   set).
5. On the build host: `tools/pclog-read.sh /dev/sdX`, then:
   - **dEQP-GLES3**: GPU boot `gles/summary.txt` (no `fail` in any group),
     `gles/failures-*.txt`, `gles/egl.txt` (the `GLES:` line must name ANGLE on Vulkan with
     the NVIDIA/NVK device, not SwiftShader);
   - **dEQP-VK** (G3): `vk/vkjson.json`, `vk/summary.txt`;
   - **frame rate**: `bench/summary.txt` of the pcfb boot against the GPU boot (the
     on-screen hwui numbers should reach the refresh rate where pcfb does not, and the
     off-screen and angeles numbers should be far higher). Also check `== timestats` in
     each boot's latest `state-*.txt` for the games' layers (`averageFPS`, `missedFrames`);
   - **SwiftShader gone**: `== swiftshader users` empty, `== gles` with
     `ro.hardware.vulkan=nouveau`;
   - **games**: `applogs/` (Unreal logs), `tombstones/`, and `logcat.txt` for `vulkan`,
     `ANGLE`, `nvk`, `MESA` errors;
   - **GPU faults**: `dmesg.txt` for `nouveau ... fault`, `gsp`, channel errors.

Done when:
- the dEQP-GLES3 smoke groups have no failures;
- Chromium/WebView and the F9 games run on the GPU (not in `== swiftshader users`, with the
  ANGLE/NVK renderer);
- the GPU boot's frame rates beat the pcfb boot's.

Fallbacks:
- If the GPU entry's desktop does not come up, the logcat of that boot (ANGLE/EGL errors in
  SurfaceFlinger) says whether ANGLE rejected NVK. Try `debug.angle.*` feature overrides
  next.
- If Chromium misbehaves on NVK, remove `/data/local/tmp/chrome-command-line` on that entry
  to try Graphite on Dawn.
- If a game picks GLES 3.2 features, raise `ro.opengles.version` per entry once
  dEQP-GLES31 passes.

## G5. Power, thermal, stability

### What nouveau gives on GSP-RM

On Ada, nouveau boots GSP-RM and creates no `therm`, `clk`, `volt` or `iccsense` subdev
(`nv194_chipset` in `nvkm/engine/device/base.c`; `gp100_therm_new` returns `-ENODEV` when
GSP-RM runs). GSP-RM runs the fans, clocks and thermal slowdown by itself, and
`nouveau_hwmon_init` registers no hwmon device. So "nouveau's hwmon" had nothing to feed
into the thermal HAL, and nothing in nouveau shows the clocks.

The RM control interface has calls for temperatures, fans and clocks. But NVIDIA's open
headers (`open-gpu-kernel-modules` 570.144, `ctrl2080thermal.h`, `ctrl2080fan.h`,
`ctrl2080clk.h`) are stubs: the structures and command numbers were removed. Guessing
layouts against the GSP firmware is not safe. What is public:
- `NV2080_CTRL_CMD_PERF_GET_CURRENT_PSTATE` (`0x20802068`, one `NvU32` P-state bit) in
  `ctrl2080perf.h`;
- the core sensor register `0x020460` that nouveau's own `gp100_temp_get()` reads (bit 29
  valid, bits 3-16 the temperature in 1/256 °C).

### What changed

| Piece | Where | What |
|---|---|---|
| nouveau hwmon on GSP-RM | `tools/patches/gki-nouveau-gsp-thermal.patch` (`nouveau_hwmon.c`; `tools/gki-build.sh` applies it to `gki/common`) | With no therm subdev and GSP-RM running, registers hwmon `nouveau` with only `temp1_input`, read from `0x020460`. Reads that are not valid return `-ENODEV`: the valid bit is clear, the value is a PRI error (`0xbadXXXXX`, which has the valid bit set), or it is above 150 °C. If the first read at probe is not valid, there is no hwmon device. dmesg always logs `nouveau ...: drm: GSP-RM thermal sensor: 0x........` with the raw value |
| GSP-RM P-state | same patch (`nvkm/subdev/gsp/rm/r535/gsp.c`) | debugfs `/sys/kernel/debug/nouveau/<pci>/perf`, next to GSP-RM's `logrm`/`loginit`: `pstate P0 (0x1)` (from `PERF_GET_CURRENT_PSTATE` on the internal subdevice) and `tsensor 0x... temp N`. It is removed with the GSP, also when `keep_gsp_logging` keeps the directory |
| Thermal HAL | `aosp-device/hals/thermal/src/sensors.rs` | A GPU sensor whose driver reports no `max`/`crit`/`emergency` gets default thresholds: LIGHT 80, MODERATE 85, SEVERE 90 (NVIDIA's rating for desktop Ada), CRITICAL 95, EMERGENCY 100 °C, and no SHUTDOWN (the GPU's own thermal shutdown handles that). Driver limits win when present. Every `fanN_input` of a reported hwmon driver (CPU, GPU) becomes a `FAN` cooling device (`gpu-fan0`, ...), whose value is its RPM. Host test `android.hardware.thermal-service.pc_sensors_test`: 6/6 pass, with a new nouveau-on-GSP plus amdgpu-with-fans case |
| Suspend | `aosp-device/pc/init.pc.rc` | On `ro.boot.android_pc.gpu=nouveau`, at `sys.boot_completed`: `stop suspend_blocker` (Cuttlefish holds a wakelock forever with it), `start vendor.pcwake`. Android then suspends when the screen is off and nothing holds a wakelock. pcfb entries keep the blocker: the firmware does not set the GOP framebuffer up again after a resume |
| `pcwake.sh` | `aosp-device/pc/pcwake.sh` | Enables `power/wakeup` on USB keyboards (HID boot protocol 1), their hubs and the xHCI controller; mice stay off, so a bump on the desk does not wake the PC. After each resume that no alarm caused (the RTC's or alarmtimer's wakeup sources did not count up), it sends `KEYCODE_WAKEUP`. Without it the screen stays off, because the ACPI button driver swallows the press that woke the PC, and Android suspends again. While `pcvktest.sh` or `pcgfxbench.sh` run it holds the wakelock `pcwake`, so a screen timeout does not cut the unattended G3/G4 tests short. Logs (`pcwake:`) go to the kernel log |
| pclog | `aosp-device/pc/pclog.sh`, nouveau entries | `gpu-perf.txt`: uptime, P-state, temperature and load every 5 s for an hour. `suspend-test.txt`: once per build (marker `suspend-test.done` on the pclog partition, written first so a hang is not repeated), at ≥ 420 s with the G3/G4 tests idle: an `echo mem > /sys/power/state` with an RTC wake 20 s later, with the connectors, `perf`, `suspend_stats`, the display mode and the kernel log of the cycle. State dumps: `== gpu thermal` (hwmon, `perf`, thermalservice GPU lines), `== gpu faults` (count and last 60 lines of nouveau/GSP fault, timeout, error, hang, killed-channel lines), `== suspend` (`mem_sleep`, `suspend_stats`, last resume reason, blocker/pcwake state, PM lines) |

### QEMU check (2026-11-14)

Build `eng.caoanh.20261114.2128`, image `--menu --append console=ttyS0,115200` with
`default_entry: 5` ("Android 17, NVIDIA nouveau", drm_hwcomposer on vkms in QEMU), run with
`WRITABLE=1 NO_WINDOW=1 AUDIO=none
QEMU_EXTRA="-global ICH9-LPC.disable_s3=0 -device vhost-vsock-pci,guest-cid=3"` (S3 on, adb
over vsock). QEMU has no NVIDIA GPU, so the hwmon and `perf` additions are not exercised.
That needs the real PC. Everything around suspend is exercised:

- Boot: `sys.boot_completed` at 140 s, `suspend_blocker` stopped, `vendor.pcwake` running,
  `mem_sleep` `s2idle [deep]`. pcwake enabled wakeup for the USB keyboard (`1-2:1.0`), held
  its wakelock while `pcgfxbench.sh` ran, and released it afterwards.
- Self-test at 420 s: `PM: suspend entry (deep)`, `ACPI: PM: Preparing to enter system
  sleep state S3`, RTC wake 20 s later, `Waking up from system sleep state S3`. xHCI
  reinitialised and the USB stick (root file systems and pclog) reset and carried on. Then
  `PM: suspend exit`, the connector still `connected enabled`, the display mode unchanged
  (1024x768 at 60 Hz), and `suspend_stats` showed success 1, all fail counters 0.
- Autosuspend: `input keyevent KEYCODE_SLEEP` turned the screen off, and QEMU went to
  `paused (suspended)` by itself. A wake from the QEMU monitor (`system_wakeup`, like a
  key press) resumed it, pcwake logged `not an alarm, turning the screen on`, and
  PowerManager woke (`WAKE_REASON_WAKE_KEY`, `mWakefulness=Awake`).
- That run showed that the RTC's wakeup source is named after its PNP device (`00:04`), not
  `rtc`, so pcwake also treated the RTC wake of the self-test as a user wake. pcwake now
  counts every wakeup source whose device has an `rtc/` child as an alarm. That fix and
  quieter pclog lines without hwmon are in the release build below.
- Rerun on the release build `eng.caoanh.20261114.2154`. The nouveau entry logged
  `alarm wakeup sources: 00:04 alarmtimer.0.auto rtc-test.1 rtc-test.2`, and the
  self-test's RTC wake no longer turned the screen on. On the default pcfb entry:
  `sys.boot_completed=1`, `suspend_blocker` running, no pcwake, nouveau not loaded, GLES
  on SwiftShader as before.

### Status

**Code done, real-PC soak pending.** Release image `out/aosp17-usb-g5.img` (build
`eng.caoanh.20261114.2154`), which also carries G1-G4. The default Limine entry stays pcfb
until the soak below passes. Then `tools/mkusb-aosp17.sh --menu` gets the nouveau entry as
`default_entry`, with pcfb kept as an entry.

### Real-PC soak

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g5.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g5.img /dev/sdX
```

(`/dev/sdX` is the stick: `lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`.)

On the PC, after the G4 checklist (or instead of it):

1. Boot **"Android 17, NVIDIA GPU (NVK + ANGLE)"**. On the first boot of this build, leave
   it alone until the dEQP and benchmark runs are over (`pcvktest:` / `pcgfxbench:` in
   dmesg, about 45 minutes). At about 7 minutes after they finish, the screen goes black
   for about 20 s (the suspend self-test) and the desktop must come back by itself.
2. **Soak, one hour**: a game (Delta Force or Genshin Impact) with a YouTube video in
   Chromium on the side or between rounds, without rebooting. Note the start time and
   anything odd (freezes, black screens, artefacts, the fans getting loud), and take a
   screenshot of it (Print Screen).
3. **Suspend by hand**: press the PC's power button briefly (the screen goes off and Android
   suspends: fans stop, the power LED blinks or dims). Wait 30 s, then press a key on the
   keyboard. The desktop must come back with sound on the monitor. Do it once more with the
   power button to wake.
4. Reboot into the default entry and plug the stick into the build host.
5. On the build host: `tools/pclog-read.sh /dev/sdX`, then in the GPU boot's directory:
   - `gpu-perf.txt`: P8 (or P5) at the desktop, **P0** during the game and the dEQP and
     benchmark runs. The temperature stays below about 85 °C under load and drops back
     when idle;
   - `dmesg.txt`: the `GSP-RM thermal sensor: 0x...` line (bit 29 set = sensor valid);
   - the latest `state-*.txt`: `== gpu faults` count 0 (or only lines from boot), and the
     `== gpu thermal` hwmon value plus `GPU0` in thermalservice with its thresholds;
   - `suspend-test.txt`: `state write exit 0`, connectors `connected enabled` after resume,
     `suspend_stats` success ≥ 1 and fail 0;
   - `== suspend` in a dump taken after step 3: success ≥ 3, and `pcwake: ... turning the
     screen on` lines;
   - `logcat.txt`: no `nouveau`/`nvk`/`MESA` channel errors, and no SurfaceFlinger
     restarts (`tombstones/` empty of SurfaceFlinger or composer crashes).

Done when the hour shows no GPU faults or hangs, the temperatures stay sane, and suspend
comes back. Then the nouveau entry becomes the default.

Fallbacks:
- No `nouveau` hwmon and the dmesg value without bit 29: the register is not the Ada sensor.
  The thermal HAL then just has no GPU sensor (no harm), and the next try is RM's thermal
  call with a layout taken from a running proprietary driver.
- `perf` says `pstate error`: GSP-RM refused the control from nouveau's internal client.
  Then `gpu-perf.txt` has temperatures only.
- Resume fails (black screen, PC on): hold the power button, boot the pcfb entry and read
  pclog: the cycle's kernel log is in `dmesg.txt` up to the hang. `suspend-test.done` keeps
  the self-test from running again on that build. To keep suspend off on the nouveau entry
  until a fix, `adb shell start suspend_blocker` after each boot.

## G6. Nova (Rust) experiment

NVIDIA's Rust driver Nova is two modules in the 6.18 tree: `nova-core`
(`drivers/gpu/nova-core`, binds the GPU's PCI function, talks to the falcons and boots
GSP-RM) and `nova` (`drivers/gpu/drm/nova`, the DRM driver, which binds to an auxiliary
device that nova-core creates). Both are marked "work in progress and may not be
functional". G6 builds them for the GKI kernel, loads them on a boot entry of their own and
records how far they get on the RTX 4070, next to nouveau.

### What Nova 6.18 does on Ada

Read from the source (`nova-core/gpu.rs` `Gpu::new`, `gsp/boot.rs` `Gsp::boot`), in order:

1. Map BAR0 (16 MiB of registers only: no BAR1/BAR2, so it never touches the VRAM aperture
   the UEFI GOP framebuffer lives in), read `NV_PMC_BOOT_0`, print the chipset
   (`NVIDIA (Chipset: AD104, Architecture: Ada, Revision: ...)`).
2. Wait for the GPU's own firmware boot (GFW_BOOT) to finish.
3. Register the sysmem flush page, set up the GSP and SEC2 falcons.
4. Parse the VBIOS (from the PROM through BAR0), load the GSP-RM 570.144 firmware
   (`gsp`, `bootloader`; the same files as nouveau, already in `/vendor/firmware`) and build
   its radix page tables in DMA memory.
5. Compute the framebuffer layout: VRAM size (`vidmem_size`), VGA workspace, the FRTS
   region at the top of VRAM.
6. Run FWSEC-FRTS (the FWSEC ucode from the VBIOS, on the GSP falcon), which creates the
   WPR2 write-protected region; check that WPR2 is where it was asked to be.
7. Load and patch the `booter_load` firmware for SEC2.
8. Stop. The 6.18 tree does not run booter, so GSP-RM is never started: no RPC, no
   memory manager, no channels, no display. Then nova-core registers the `nova-drm`
   auxiliary device and `nova` binds to it: a DRM device with only GEM objects and a
   `GETPARAM` ioctl (`NOVA_GETPARAM_VRAM_BAR_SIZE`). It has no KMS, no render node use for
   Mesa, and no command submission.

So the most Nova can show on this kernel is steps 1-7: the chipset, a VBIOS that parses,
the VRAM size, and FWSEC-FRTS creating WPR2. "GSP boot" in the sense nouveau means
(GSP-RM running and answering RPCs) is not in this version.

### What changed

| Piece | Where | What |
|---|---|---|
| GKI image config | `aosp-device/kernel/gki_rust_defconfig`, `tools/gki-build.sh` (`--defconfig_fragment=//android-pc:gki_rust_defconfig` on every Bazel step) | `CONFIG_RUST_FW_LOADER_ABSTRACTIONS=y`. `NOVA_CORE` selects it, and it is a bool that adds `kernel::firmware` to the Rust `kernel` crate, which is linked into vmlinux. With it only in the vendor config, modpost reports `kernel::firmware::Firmware::request` and its `Drop` as undefined. It is the first change to vmlinux itself; it only adds code. All three Bazel steps must pass the same flag, otherwise the vendor builds would rebuild their base kernel without it |
| Export | `tools/patches/gki-vrealloc-export.patch` (`mm/vmalloc.c`) | `EXPORT_SYMBOL(vrealloc_node_align_noprof)`. The Android kernel builds Rust with `RUST_INLINE_HELPERS`, so `rust_helper_vrealloc_node_align` is inlined into the module and nova-core (which uses `KVVec`/`VVec`) calls the unexported function directly. Upstream builds without inline helpers call the exported helper instead, so this is only needed here. The patch matches the existing `kvrealloc`/`krealloc` exports and has an upstream-style header |
| Boot log | `tools/patches/gki-nova-boot-log.patch` (`nova-core/gsp/boot.rs`) | `dev_dbg!` is compiled out of the kernel crate unless `RUST_DEBUG_ASSERTIONS` is set (`Device::pr_dbg` checks `cfg!(debug_assertions)`), and GKI does not set it. So on GKI a working nova-core prints nothing after the chipset line. The patch adds three `dev_info!` lines: `VRAM: N MiB, FRTS: a-b`, `WPR2: a-b`, and `Booter loader firmware ready; GSP-RM boot not implemented yet`. It has an upstream-style header |
| Vendor modules | `aosp-device/kernel/pc_x86_64.fragment` (`CONFIG_NOVA_CORE=m`, `CONFIG_DRM_NOVA=m`), `BUILD.bazel` (`_PC_MODULES`) | `nova_core.ko` (246 KiB) and `nova.ko` (32 KiB) in `vendor_dlkm` |
| Blocklist | `aosp-device/pc/modules.blocklist` | `nova.ko`, `nova_core.ko`: the boot-time load skips them on every entry. nova-core's PCI aliases (`10de`, display classes 0300/0302) match the same GPU as nouveau |
| Boot entry | `tools/mkusb-aosp17.sh --menu`, the last entry | **"Android 17, NVIDIA Nova (Rust driver experiment)"**: `androidboot.selinux=permissive androidboot.android_pc.gpu=nova`. pcfb stays on (Nova has no display and does not take the GOP framebuffer), Android runs as on the default entry, nouveau is never loaded (`init.pc.rc` loads it only for `gpu=nouveau`). The default entry stays pcfb |
| Loader | `aosp-device/pc/init.pc.rc`, `pcnova.sh` (service `vendor.pcnova`) | At `sys.boot_completed` on `gpu=nova`, waits 30 s, then `modprobe nova_core.ko` and `modprobe nova.ko`. Both are needed: the two modules meet on the auxiliary bus, so `modules.dep` has no link between them. Loading late means pclog is already streaming dmesg to its partition if FWSEC or the GPU hangs the machine. It writes `/data/vendor/pcgpu/nova.txt`: modprobe output, `lsmod`, the NVIDIA PCI functions with their driver, auxiliary devices, DRM nodes and drivers, Nova's dmesg lines, `/proc/iomem` framebuffer ranges. It logs `pcnova:` lines to the kernel log |
| pclog | `aosp-device/pc/pclog.sh` | On the nova entry, state dumps get `== nova` (pcnova's report) and `== nova dmesg` |

### Building it

`tools/gki-build.sh` (all steps; the GKI image is rebuilt because of the defconfig fragment,
about 13 minutes on the E-cores), then the usual image steps. The AOSP analysis hit the
20 GB cap (`soong_build` OOM-killed at 20 GB RSS) with the default settings, so the build
ran with `SOONG_INCREMENTAL_ANALYSIS=false SOONG_GOMEMLIMIT=14GiB`, as G3 did. Build
`eng.caoanh.20261114.2338`.

### QEMU check (2026-11-14)

QEMU has no NVIDIA GPU, so the check is that Nova loads, stays idle, and harms nothing.
Image `out/aosp17-usb-g6-qemu.img` (`--menu --append console=ttyS0,115200`, `default_entry`
set in the copy's `limine.conf`), `WRITABLE=1 NO_WINDOW=1 AUDIO=none
tools/run-qemu-aosp17-usb.sh` (E-cores 12-15, 4 vCPUs, 4 GiB), adb over vsock:

- Nova entry (`default_entry: 9`): boot-time load logs `dlkm_loader: module nova is
  blocklisted` and the same for `nova_core` (blocklist works). `sys.boot_completed` at 33 s,
  `vendor.pcnova` started, at 63 s `pcnova: modprobe exited (0 0)`; `lsmod` shows `nova` and
  `nova_core` with refcount 0 (no NVIDIA PCI function to bind, no auxiliary device), nouveau
  not loaded, Android on pcfb (`card0`/`renderD128` = `efi-framebuffer`, i.e. pcfb's device)
  as on the default entry.
- Default entry (`default_entry: 1`): `sys.boot_completed=1`, `ro.boot.android_pc.gpu`
  empty, neither Nova module loaded (both reported blocklisted by `dlkm_loader`), pcfb
  drives the display as before.

### Status

**Code done, real-PC test pending.** Release image `out/aosp17-usb-g6.img` (build
`eng.caoanh.20261114.2338`), which also carries G1-G5. The Nova entry is never the
default; it stays an experiment until Nova can drive a display.

### Real-PC test

```bash
sudo tools/update-usb-aosp17.sh --dry-run out/aosp17-usb-g6.img /dev/sdX
```

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-g6.img /dev/sdX
```

(`/dev/sdX` is the stick: `lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`.)

On the PC:

1. Power the PC off fully, then boot **"Android 17, NVIDIA Nova (Rust driver
   experiment)"** (the last entry). A cold boot matters: if an earlier boot left the WPR2
   region set (a warm reboot from the nouveau entries may not reset the GPU), nova-core stops
   with `WPR2 region already exists - GPU needs to be reset`.
2. Android comes up on pcfb as on the default entry. About 30 s after the lock screen,
   pcnova loads Nova. The screen may flicker or stay as it is; note anything odd (black
   screen, freeze, fans). Leave it running about 5 minutes (the 300 s pclog dump).
3. Reboot into the default entry, plug the stick into the build host, then
   `tools/pclog-read.sh /dev/sdX` and look at the Nova boot's directory:
   - `dmesg.txt`: `pcnova:` lines and `nova_core <GPU PCI address>:` lines;
   - `state-300.txt`: `== nova` (`nova_core` bound to the `10de:2786` function, a
     `nova_core.nova-drm.0` auxiliary device, a DRM node driven by `nova`) and
     `== nova dmesg`.

What each result means:

| dmesg | Meaning |
|---|---|
| `NVIDIA (Chipset: AD104, Architecture: Ada, Revision: ...)` only, then a probe error | BAR0 works; the error line says which step failed (GFW boot, VBIOS, firmware) |
| `VRAM: N MiB, FRTS: ...` | VBIOS parsed, GSP firmware loaded, layout known (the 4070 has 12 GiB, so N is about 12288) |
| `WPR2: ...` | FWSEC-FRTS ran on the GSP falcon and created WPR2: the deepest step Nova 6.18 has |
| `Booter loader firmware ready; GSP-RM boot not implemented yet` | everything Nova 6.18 can do on Ada worked; `nova` then binds and `/dev/dri/card1` (driver `nova`) appears |
| `WPR2 region already exists` | not a cold boot; power off and retry |
| `FWSEC-FRTS returned with error code ...` | FWSEC failed; the code is what to report upstream |

### Nova compared with nouveau

| | nouveau (G1-G5) | Nova (6.18) |
|---|---|---|
| Language, size | C, 6.8 MiB `nouveau.ko` (stripped) plus 9 helper modules | Rust, 246 KiB `nova_core.ko` plus 32 KiB `nova.ko` |
| GSP-RM | boots it (570.144), RPC, power and clocks through it | firmware loaded and FWSEC-FRTS run, GSP-RM not started |
| VRAM | memory manager, TTM, VM_BIND for NVK | size read and printed only |
| Display | KMS: modes, hotplug, HDMI audio ELD, drm_hwcomposer | none (no KMS objects); Android stays on pcfb |
| Rendering | NVK Vulkan, ANGLE GLES | none (no channels, no command submission) |
| GKI changes | modules only, plus the G5 hwmon/perf patch | one vmlinux bool (`RUST_FW_LOADER_ABSTRACTIONS`) and one export (`vrealloc_node_align_noprof`), both from the Android kernel's Rust setup |
| Use on this PC | the GPU stack for G2-G5 | an experiment to follow upstream: worth retrying when the GKI branch moves to a release where nova-core runs booter and starts GSP-RM (the work continues upstream after 6.18) |

The Real-PC results go here once the stick has been booted on the Nova entry.
