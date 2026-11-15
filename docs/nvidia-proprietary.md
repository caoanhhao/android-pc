# NVIDIA's own driver on bare-metal Android (Phase K)

Running log for Phase K of `PLAN.md`: the RTX 4070 (AD104, `10de:2786`) driven by NVIDIA's
own driver instead of nouveau or Nova, with the design of `~/projects/waydroid-nvidia`
(Mesa Venus over a vtest socket to `virgl_test_server` on NVIDIA's Vulkan) moved onto the
stick. One section per step. NVIDIA's firmware and userspace binaries are used for this
personal stick only and are not redistributed.

Driver version: **620.70.06**, the build host's `nvidia-open` / `nvidia-utils` (PLAN.md and
waydroid-nvidia started from 615.78.08; the host has moved on). Kernel modules, GSP firmware
and the K2 userspace must all be this exact version.

## K1. nvidia-open kernel modules on GKI android17-6.18

### What was added

| Piece | Where | What |
|---|---|---|
| Source | `gki/nvidia-open` (not in git) | `git clone --depth 1 --branch 620.70.06 https://github.com/NVIDIA/open-gpu-kernel-modules gki/nvidia-open` |
| Build | `tools/nvidia-build.sh`, step `nvidia` of `tools/gki-build.sh` | `nvidia.ko`, `nvidia-modeset.ko`, `nvidia-drm.ko`, `nvidia-uvm.ko` against the GKI image, stripped of debug info, into `gki/out/nvidia/dist`; `gki-build.sh install` copies them into the vendor modules |
| GKI patch to nvidia-open | `tools/patches/nvidia-open-gki-ns.patch` | `MODULE_IMPORT_NS("ANDROID_GKI_VFS_internal_I_am_really_a_filesystem_and_am_NOT_a_driver")` in `nvidia` and `nvidia-modeset` |
| Firmware | `gki-build.sh` step `firmware` → `/vendor/firmware/nvidia/620.70.06/` | `gsp_ga10x.bin` (17 MiB) and `ucodes_ga10x.bin` (54 MiB) from the host's `/usr/lib/firmware/nvidia/620.70.06/` |
| Blocklist | `aosp-device/pc/modules.blocklist` | the four modules, so the boot-time module load skips them on every entry |
| Init | `aosp-device/pc/init.pc.rc`, `pcnvidia.sh` | on `ro.boot.android_pc.gpu=nvidia`: modprobe, device nodes, composer settings |
| gralloc (until K3) | `tools/patches/aosp17-minigbm-nvidia-dumb.patch` | minigbm's dumb backend for the DRM driver `nvidia-drm` |
| Boot entry | `tools/mkusb-aosp17.sh --menu` | **"Android 17, NVIDIA (proprietary, Venus)"** |
| Logs | `aosp-device/pc/pclog.sh` | `== nvidia modules`, `== nvidia dmesg`, `== /proc/driver/nvidia`, `== /dev/nvidia*`, `== drm cards`, `== modetest -c nvidia-drm` |

### Building against the GKI image

The modules are not a Kleaf target. NVIDIA's build has two stages: the OS-agnostic cores
(`src/nvidia` → `nv-kernel.o`, `src/nvidia-modeset` → `nv-modeset-kernel.o`) are built with
NVIDIA's own makefiles, then `kernel-open/` links them into the modules with kbuild.
`tools/nvidia-build.sh` therefore:

1. prepares a plain kernel output directory (`gki/out/nvidia/kbuild`) from the GKI image's
   `.config` (`gki/out/kernel_x86_64/dist/kernel_x86_64_dot_config`) with Kleaf's toolchain
   (clang r584948c, rust 1.91.1.p3, bindgen, and Kleaf's own `pahole` v1.31) and
   `make olddefconfig modules_prepare`, then checks that the config did not change. Without
   pahole, `DEBUG_INFO_BTF` and with it `SCHED_CLASS_EXT` drop out, which changes
   `struct task_struct`. `Module.symvers` is the GKI build's
   (`kernel_x86_64_Module.symvers`: vmlinux plus the system_dlkm modules), so the modules
   carry the GKI's symbol CRCs (`MODVERSIONS` with `GENDWARFKSYMS`);
2. builds the two cores with clang and the code generation flags of the GKI's own modules
   (taken from a kbuild `.o.cmd`): kCFI with normalized integers, `-fpatchable-function-entry=11,11`
   (FineIBT/call-depth padding), `-fcf-protection=branch` (IBT), retpolines and return
   thunks, plus the kernel's semantics flags (`-fno-delete-null-pointer-checks`,
   `-fno-strict-overflow`, `-ftrivial-auto-var-init=zero`). Without the last three, clang
   treated paths after `nvAssertFailedNoLog()` as unreachable in 17 functions (objtool: "falls
   through to next function");
3. runs kbuild on `kernel-open/` against that directory. With IBT, objtool runs on each linked
   module, so it processes the cores too (ENDBR sealing, `.cfi_sites`, return sites).

The vermagic says `6.18.21-gb81607b966dd-dirty` instead of the GKI's
`6.18.21-android17-5-maybe-dirty`; with `MODVERSIONS` the kernel compares only the part after
the release (`same_magic()`), and the CRCs decide.

What the GKI image lacked, and the minimum change:

- **Symbol namespaces.** `filp_open()` and `close_fd()` are exported only in the GKI namespace
  `ANDROID_GKI_VFS_internal_I_am_really_a_filesystem_and_am_NOT_a_driver`; modpost refused
  `nvidia` and `nvidia-modeset`. Fixed with the namespace import (`nvidia-open-gki-ns.patch`,
  guarded by `CONFIG_ANDROID_VENDOR_HOOKS`). No GKI symbol list or kernel change was needed:
  every other symbol the modules use is exported by the `--notrim` GKI image or its
  system_dlkm modules (modpost resolved them all against the GKI `Module.symvers`).
- **No fbdev emulation.** The GKI image has no `DRM_FBDEV_EMULATION`/DRM client library, so
  nvidia-drm 620's conftest drops its `fbdev` option; `fbdev=1` is not passed. Android does
  not use an fbdev console.
- **kCFI/FineIBT and C++.** nvidia-modeset's DisplayPort library is C++. clang gives its
  methods kCFI preambles but does not check C++ virtual calls (objtool lists 484 "no-cfi
  indirect call"). Under FineIBT, which `cfi=auto` picks on CPUs with IBT such as the
  i7-13700K, the kernel moves every checked target's ENDBR to the FineIBT preamble and
  removes the one at the function entry, so an unchecked virtual call would take a #CP fault
  in DisplayPort code (link training on DP-1). The entry boots with **`cfi=kcfi`**: plain
  kCFI keeps the ENDBR at each function entry, and every checked call (the KAPI table
  nvidia-drm calls, the timer callbacks, the kernel's file operations) still checks its kCFI
  hash. The remaining objtool warnings are 75 "naked return" in C++ thunks (clang ignores
  `-mfunction-return=thunk-extern` there: a missing return thunk, not a functional problem).

### The `nvidia` boot entry

`mkusb-aosp17.sh --menu` adds **"Android 17, NVIDIA (proprietary, Venus)"**, command line:

- `pcfb.enable=0`: nothing touches the GOP framebuffer nvidia-drm takes over.
- `androidboot.android_pc.gpu=nvidia` (`ro.boot.android_pc.gpu`).
- `module_blacklist=nouveau,nova_core,nova`: the kernel refuses those modules on this entry
  (they are in `modules.blocklist` anyway).
- `cfi=kcfi` (above).
- drm_hwcomposer as the composer APEX, `androidboot.selinux=permissive`.

`init.pc.rc`, `early-init` on `gpu=nvidia`:

1. `modprobe -r vkms`, `modprobe nvidia-drm.ko modeset=1` (pulls in `nvidia-modeset` and
   `nvidia`), `modprobe nvidia-uvm.ko`, `modprobe vkms`: as on the nouveau entry (Phase I1),
   so that nvidia-drm gets `card0` and vkms comes back as `card1`. nvidia-drm with
   `modeset=1` brings the GPU up (GSP-RM boot) at load time; the firmware comes from
   `/vendor/firmware` (`firmware_class.path`). In QEMU nvidia.ko refuses to load (no GPU) and
   vkms is `card0`.
2. `pcnvidia.sh` creates the device nodes, which the modules register without creating
   (desktop Linux runs `nvidia-modprobe`): `/dev/nvidiactl` (195:255), `/dev/nvidia-modeset`
   (195:254), `/dev/nvidiaN` (195:N, from `/proc/driver/nvidia/gpus/*/information`),
   `/dev/nvidia-uvm` and `/dev/nvidia-uvm-tools` (dynamic major from `/proc/devices`),
   `/dev/nvidia-caps/nvidia-capN`; mode 0666, group graphics. It logs `pcnvidia:` lines with
   the nodes, the driver version and which driver owns each DRM card.

Display until K3/K4 replace it: drm_hwcomposer on `/dev/dri/card0` (nvidia-drm), overlay
planes off, SurfaceFlinger composing on SwiftShader, minigbm allocating nvidia-drm dumb
buffers on its render node (`aosp17-minigbm-nvidia-dumb.patch`; `git apply` in
`aosp/external/minigbm` after the nouveau-gart and vtest patches). This is what G2 did on
nouveau. nvidia-drm's dumb buffers are in VRAM, so expect the same slow CPU readback J2
fixed for nouveau; K3's vtest gralloc and K4 replace this path.

### QEMU check (2026-11-15)

Build `eng.caoanh.20261115.1540`, image `out/aosp17-usb-k1-qemu.img` (`--menu --userdata 2048
--append console=ttyS0,115200`), every entry booted once with `NO_WINDOW=1
tools/run-qemu-aosp17-usb.sh` (E-cores 12-15, 4 vCPUs, 4 GiB) after setting `default_entry`
in the image's `limine.conf` (mtools), and read over adb. QEMU has no NVIDIA GPU, so the
check is that the modules load into the GKI kernel and fail cleanly, and that no other entry
changed.

| Entry | `sys.boot_completed` | Notes |
|---|---|---|
| 1 SELinux permissive | 1 (40 s) | ranchu on pcfb, 1280x800 |
| 2 SELinux enforcing | 1 (40 s) | same, enforcing |
| 3 kernel log on screen | 1 (40 s) | same |
| 4 no pcfb display driver | 0 | SurfaceFlinger crash loop: ranchu has no `virtio*` card in QEMU. **Same on the J2 image** (`out/aosp17-usb-j2-qemu.img`, rerun today), so not a K1 regression; a debug entry for the real PC |
| 5 NVIDIA nouveau | 1 (40 s) | drm_hwcomposer on vkms (card0), nouveau loaded and idle |
| 6 NVIDIA GPU (NVK + ANGLE) | 1 (40 s) | same |
| 7 NVIDIA nouveau, VTS run | 1 (40 s) | same (stopped before the VTS run) |
| 8 NVIDIA Nova | 1 (40 s) | ranchu on pcfb |
| 9 NVIDIA (proprietary, Venus) | 1 (40 s) | see below |

On the nvidia entry the kernel command line ends with `pcfb.enable=0
androidboot.android_pc.gpu=nvidia cfi=kcfi module_blacklist=nouveau,nova_core,nova`; the boot-time
load skips the four modules (`dlkm_loader: module nvidia_drm is blocklisted`, ...); `init.pc.rc`
runs `modprobe nvidia-drm.ko modeset=1`, which loads `nvidia.ko` into the GKI kernel (no
unknown symbols, CRC or vermagic complaints) and runs its init, which ends cleanly:

```
nvidia-nvlink: Nvlink Core is being initialized, major device number 502
NVRM: No NVIDIA GPU found.
nvidia-nvlink: Unregistered Nvlink Core, major device number 502
```

modprobe exits 1, the same for `nvidia-uvm.ko`; vkms comes back as `card0`, `pcnvidia.sh` logs
that nvidia.ko is not loaded, drm_hwcomposer runs on vkms (1024x768) and SurfaceFlinger draws
with minigbm's vkms backend. Nothing of nouveau or Nova is loaded. (QEMU's CPU has no IBT, so
the kernel uses kCFI there anyway: `SMP alternatives: CFI: Using rehashed retpoline kCFI`.)

The host was rebooted (a clean shutdown, not a hang) during the first round of these runs;
the whole round was repeated afterwards.

### Status

**Code done, QEMU check passed, real-PC test pending.** Release image `out/aosp17-usb-k1.img`
(`--menu --userdata 64`, build `eng.caoanh.20261115.1540`, security patch 2026-06-05). It was
built from the shared checkout, so it also carries the K2-K4 work in progress at that time,
all inert on this entry (K3's Venus only with `androidboot.hardware.vulkan=virtio`, K4's
`pcnvdisplay` only confirms nvidia-drm's card), and K3's move of `androidboot.opengles.version`
from the bootconfig into each entry (same value, ES 3.1).

### Real-PC test

Find the stick (`lsblk -o NAME,SIZE,MODEL,TRAN`, the ~57 GB SanDisk on `usb`), unmount its
FAT partitions if the desktop mounted them (`udisksctl unmount -b /dev/sda2`, `udisksctl
unmount -b /dev/sda5`; do not open the stick in a file manager), then update it in place
(keeps apps and data; type the full word `yes`):

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-k1.img /dev/sda
```

On the PC (monitor on DP-1):

1. Pick **"Android 17, NVIDIA (proprietary, Venus)"** in the Limine menu. Expected: the GOP
   picture, a short black screen while nvidia-drm boots GSP-RM, then the Android desktop at
   the monitor's native mode (2560x1080) drawn by SwiftShader (slow, as on the first nouveau
   KMS entry). Note what was seen.
2. Leave it about 10 minutes (pclog's 600 s dump), reboot into the default entry once, plug
   the stick into the build host and read the logs (`tools/pclog-read.sh /dev/sda`). In the
   nvidia boot's directory:
   - `dmesg.txt`: `NVRM: loading NVIDIA UNIX Open Kernel Module for x86_64 620.70.06`, no
     `NVRM: GSP` or `RmInitAdapter failed` errors, no `CFI failure`, no `#CP`/`control
     protection` oops, `[drm] Initialized nvidia-drm`, `pcnvidia:` lines;
   - `state-*.txt` sections `== nvidia modules` (all four loaded, no nouveau/nova),
     `== /proc/driver/nvidia` (version 620.70.06, the RTX 4070 in `gpus/*/information`),
     `== /dev/nvidia*` (`nvidiactl`, `nvidia0`, `nvidia-modeset`, `nvidia-uvm`),
     `== drm cards` (`card0: nvidia`), `== drm connectors` / `== modetest -c nvidia-drm`
     (DP-1 `connected`, 2560x1080 first).

Done when the modules load, GSP boots, `/dev/nvidia*` exist and `card0` (nvidia-drm) shows
DP-1 connected. If the desktop does not show but those hold, K1 is still done (the display
path is K4's); if nvidia.ko fails, the `NVRM:` lines say why (firmware path:
`/vendor/firmware/nvidia/620.70.06/`).

## K3. Android side: Venus, gralloc, ANGLE, features

The Android half of waydroid-nvidia's design. It acts only on a boot entry with
`androidboot.android_pc.vulkan=venus` (`$venus` in `tools/mkusb-aosp17.sh`, for the NVIDIA
entry's command line), and only when the vtest socket is there at post-fs-data. Vulkan is Mesa
**Venus** (`vulkan.virtio`) over the vtest socket of K2's sidecar; GLES is **ANGLE** on Venus;
gralloc is minigbm's new **vtest** backend.

```
apps, SurfaceFlinger ── GLES ── ANGLE (system) ─┐
                     ── Vulkan ─────────────────┴─ Venus (vulkan.virtio, Cuttlefish Vulkan APEX)
gralloc: minigbm "vtest" backend                          │ vendor.mesa.vtest.socket.name
   GPU-only buffers ── VCMD_ALLOC_DMABUF_IMAGE ───────────┤ = /dev/socket/vtest (0666)
   CPU/video/cursor buffers ── udmabuf (linear), imported by Venus (VCMD_RESOURCE_IMPORT_BLOB)
                                                          ▼
                                    K2: virgl_test_server --venus (vtest protocol 6)
```

### What was added

| Piece | Where | What |
|---|---|---|
| Venus | `tools/venus-android-build.sh` → `aosp-device/venus/prebuilt/x86_64/vulkan.virtio.so` (`VERSION` next to it) | Mesa `mesa-26.2.6` + waydroid-nvidia's patches `0001`, `0002`, `1001`–`1004`, `1007`–`1011` (copied to `tools/patches/venus/`) + `2001` (below), cross-built with NDK r30 (API 33, libc++ static) with waydroid's meson options. The script clones Mesa into `out/venus/mesa` (from `~/projects/waydroid-nvidia/src/mesa` when it exists, else upstream), branch `pc-venus`, and starts again from the tag when the patch list changes. Stripped, 1.9 MB; the unstripped library stays in `out/venus/mesa/build-android-x86_64` for symbolizing tombstones. NEEDED: libdrm, libhardware, liblog, libnativewindow, libsync. Built with `--build-id=sha1` (meson's default) |
| Venus in the APEX | `aosp-device/venus/Android.bp` (`cc_prebuilt_library_shared` `vulkan.virtio`), `tools/patches/aosp17-cf-vulkan-venus.patch` (`patch -p1` in `aosp/device/google/cuttlefish`, after `aosp17-cf-vulkan-nouveau.patch`) | Cuttlefish loads Vulkan drivers only from `com.google.cf.vulkan` (`ro.vulkan.apex`), so Venus goes into that APEX (`lib64/hw`) next to SwiftShader and NVK, picked by `ro.hardware.vulkan=virtio` |
| Gralloc interop | `tools/patches/venus/2001-util-u_gralloc-read-minigbm-native-handles.patch` | the same `u_gralloc_minigbm.c` as AOSP's Mesa 26.1 (Phase G3): Venus reads minigbm's `cros_gralloc_handle` (format, modifier, plane fds, offsets, strides). Properties through libc, so the NDK build needs no libcutils |
| Gralloc | `tools/patches/aosp17-minigbm-vtest.patch` (`git apply` in `aosp/external/minigbm`, after the rw-lock, drop-master and nouveau-gart patches; K1's nvidia-dumb patch goes on top) | a new backend `vtest.c`, chosen before the DRM node scan when `ro.vendor.minigbm.vtest=true`; it opens no DRM node, so the composer is alone on nvidia-drm. GPU-only buffers of the formats the server maps to a VkFormat (AB24, XB24, AR24, XR24, RG16, AB30, XB30, AB4H) come from `VCMD_ALLOC_DMABUF_IMAGE` with the GPU's own modifier (NVIDIA block-linear); scanout buffers get the `SCANOUT` flag and, when K4 has set them from the KMS plane's `IN_FORMATS`, only the modifiers in `vendor.gralloc.vtest.modifiers.<fourcc hex>.<n>` (hex; read again until the list is there). Everything else (CPU access, cursor, video, camera, blobs, other formats, any server failure) is linear system memory: a sealed memfd turned into a dma-buf by `/dev/udmabuf` (the x86 GKI image has udmabuf but no system dma-heap), pitch aligned to 256 B. `bo->handle` is the dma-buf fd, as in `dma_heap.c`; mapping is `mmap` of the dma-buf plus `DMA_BUF_IOCTL_SYNC`. Socket: `ro.vendor.minigbm.vtest.socket`, else `vendor.mesa.vtest.socket.name`, else `/dev/socket/vtest`; one connection per allocator process, opened at the first allocation and reconnected once when the server went away |
| GLES | the system ANGLE (`androidboot.hardware.egl=angle`, as on every entry) | AOSP 17's ANGLE exposes ES 3.2 by itself when every extension 3.2 needs is there (`CanSupportGLES32`, `vk_caps_utils.cpp`); waydroid's Android 13 ANGLE needed `exposeNonConformantExtensionsAndVersions`, this one does not (checked below) |
| gpuid | `tools/patches/aosp17-angle-gpuid.patch` (`git apply` in `aosp/external/angle`), `aosp-device/venus/gpuid*.conf` | waydroid's gpuid is a vendor shim with ANGLE as DT_NEEDED; here ANGLE is in `/system/lib64` and the vendor (sphal) namespace cannot link it, so the same lookup is in ANGLE's `Context::initRendererString`/`initVendorString`: a process listed in `/vendor/etc/gpuid.conf` (`<process>|<GL_VENDOR>|<GL_RENDERER>`) gets those strings. The file is empty; `pcvenusprobe.sh` mounts `gpuid.nvidia.conf` (Genshin Impact → `Qualcomm` / `Adreno (TM) 750`) over it when Venus runs |
| Features | `pcvenusprobe.sh`, `aosp-device/venus/*.xml`, `tools/mkusb-aosp17.sh` | Vulkan 1.3, level 1 and compute are already declared by the Cuttlefish Vulkan APEX. `android.hardware.opengles.aep`: an empty `/vendor/etc/permissions/android.hardware.opengles.aep.pc.xml`, over which the probe mounts the real declaration when Venus runs (before system_server reads the features). `reqGlEsVersion`: `androidboot.opengles.version` moved from the shared bootconfig (which wins over the command line) to each entry's command line, 196609 (3.1) by default; the Venus entry gets none and the probe sets 196610 (3.2) or 196609 |
| Entry | `tools/mkusb-aosp17.sh`: `venus="androidboot.android_pc.vulkan=venus androidboot.hardware.guest_hwui_renderer=skiavk"` | the graphics half of the NVIDIA entry's command line (K4 adds it to K1's entry); HWUI draws with Skia on Vulkan. Like the NVK entry, `entry()` then adds neither `androidboot.hardware.vulkan` nor `androidboot.opengles.version`, so both properties stay unset in early-init. `entry()` also reads `--append` words for these overrides |
| Init, probe | `aosp-device/venus/init.venus.rc`, `pcvenusprobe.sh` | on `android_pc.vulkan=venus`, post-fs-data: `wait /dev/socket/vtest 20` (K2's sidecar starts earlier), then the probe. **With the socket**: `ro.vendor.minigbm.vtest=true`, `vendor.mesa.vn.debug=vtest` (no virtio-gpu probing), `vendor.mesa.vtest.socket.name=/dev/socket/vtest` (Mesa reads `MESA_*`/`VN_*` options from `debug.mesa.*`, `vendor.mesa.*` and `mesa.*`; the vendor prefix keeps them in the vendor namespace), `ro.hardware.vulkan=virtio`, `ro.opengles.version=196610`, and the gpuid and AEP bind mounts. **Without it** (QEMU, or a sidecar that did not start): `ro.hardware.vulkan=pastel`, `ro.opengles.version=196609` and minigbm's DRM scan, so the entry boots on SwiftShader. The choice is in `vendor.venus.probe` (`venus` or `swiftshader`), a `pcvenusprobe:` kernel log line and `/data/vendor/pcgpu/venus.txt`. All of this runs before the allocator (class hal), SurfaceFlinger and system_server start in `on boot`. Properties: `vendor_venus_prop` (vendor public, readable by every domain: apps load Venus and the mapper) |

Not ported because not needed here: waydroid's `hwc-yuv` (a hwcomposer shim for Hyprland's
format list) and `mapper-yv12` (a gralloc4 mapper shim for the gbm_mesa minigbm fork's one-plane
YV12): minigbm's own YV12 has three planes, and K4 composes with drm_hwcomposer. No 32-bit Venus:
the product is x86_64-only, and arm64 apps reach Vulkan and GLES through libndk_translation's
proxy libraries, which call the x86_64 loader and drivers.

### The vtest socket (for K2)

`/dev/socket/vtest`, mode 0666 (apps connect with their own uid), created by K2's
`virgl_test_server --venus --no-virgl --no-fork --multi-clients --socket-path /dev/socket/vtest`
(only `nvidia_icd.json` in `VK_DRIVER_FILES`, `RENDER_SERVER_EXEC_PATH` set) before
SurfaceFlinger starts: start the sidecar at post-fs-data or earlier; `init.venus.rc` waits up
to 20 s for the socket. The server needs vtest protocol 6 (`vrend-0001` + `vrend-0002`); Venus
here does not ask for protocol 7. No `--rendernode` is needed: the server's allocator takes the
Vulkan device with dma-buf and modifier export.

### SELinux

The NVIDIA entry boots permissive, like the other GPU entries. For enforcing these are missing:
the allocator (`hal_graphics_allocator`) opening `/dev/udmabuf` (`udmabuf_device` is a private
platform type, so vendor policy has to relabel the device), every client domain connecting to
the sidecar's socket (K2's types), the probe (runs as `su`) and the bind mounts (init may
already mount on `vendor_configs_file`).

### K3 test

No NVIDIA GPU is needed, but a server on the build host cannot serve a VM: vtest hands
dma-bufs and shared memory over the socket as file descriptors (SCM_RIGHTS). So the test runs
the server inside the guest, on lavapipe:

- `tools/venus-test.sh build`: lavapipe (glibc, from the same Mesa checkout, in its own git
  worktree `out/venus/mesa-lvp`, 813 steps) and a private glibc root `out/venus/test-root`
  (269 MB: the host's ld.so and libraries, waydroid-nvidia's patched `virgl_test_server` and
  `virgl_render_server` (vtest protocol 6), the Vulkan loader, lavapipe with
  `VK_EXT_external_memory_dma_buf` and `VK_EXT_image_drm_format_modifier`). The render server
  starts through a wrapper that runs it with the private ld.so (the binaries' PT_INTERP does not
  exist on Android). lavapipe gets a test-only patch
  (`tools/patches/venus-test/lavapipe-two-queues.patch`): HWUI aborts unless the graphics
  family has two queues (`VulkanManager.cpp`, `kRequestedQueueCount`), and lavapipe has one;
  the patch reports two and hands out the same queue for both. The RTX 4070 has 16.
- `tools/venus-test.sh push vsock:3:5555` copies it to `/data/local/tmp/vtest`. An entry with
  `androidboot.android_pc.venus_test=1` runs its `run.sh` from `init.venus.rc` at post-fs-data,
  before the probe waits for the socket.
- Image: `tools/mkusb-aosp17.sh --append "androidboot.android_pc.vulkan=venus
  androidboot.hardware.guest_hwui_renderer=skiavk androidboot.android_pc.venus_test=1
  androidboot.hardware.hwcomposer.mode=client" out/venus/k3-test.img`, booted with
  `QEMU_EXTRA="-device vhost-vsock-pci,guest-cid=3" tools/run-qemu-aosp17-usb.sh` (adb over
  vsock). `hwcomposer.mode=client` is for QEMU only: the ranchu composer uses its guest
  composer only with `ro.hardware.vulkan=pastel` and otherwise wants a gfxstream host
  (`Can't create AidlComposerClient`); the NVIDIA entry runs drm_hwcomposer (K4).

Results (2026-11-15, QEMU, 4 vCPUs, 4 GB):

| Check | First boot (nothing in /data, no socket) | After `push` and a reboot (lavapipe server) |
|---|---|---|
| `vendor.venus.probe` | `swiftshader` (`wait /dev/socket/vtest` timed out after 20 s) | `venus` |
| `ro.hardware.vulkan`, `ro.opengles.version` | `pastel`, 196609 | `virtio`, 196610 |
| boot | completes on SwiftShader | completes, SurfaceFlinger on ANGLE on Venus |
| `dumpsys gfxinfo` (systemui, launcher3) | | `Pipeline=Skia (Vulkan)`, frames rendered, none janky |
| `cmd gpu vkjson` | | `driverName` `venus`, `driverInfo` `Mesa 26.2.6`, `deviceName` `llvmpipe (LLVM 23.1.5, 256 bits)` (patch 1010 shows the host GPU's own name; on the PC, the RTX 4070) |
| GLES (`dumpsys SurfaceFlinger`) | | `ANGLE (Mesa, Vulkan 1.3.354 (llvmpipe ...), venus-26.2.6), OpenGL ES 3.2`, without `exposeNonConformantExtensionsAndVersions` (ASTC comes from Venus's emulation, patch 1008) |
| features (`pm list features`) | | `android.hardware.opengles.aep`, `vulkan.version` 1.3, `vulkan.level` 1, `vulkan.compute` |
| gralloc | | the server logs `vtest_vk_alloc: allocating window buffers on llvmpipe`; the allocator calls `UDMABUF_CREATE` for linear buffers; Venus reads the handles (`Using minigbm native handles`) |
| screen | | the desktop (wallpaper, taskbar) on the QEMU display through the composer |

Not checked here: gpuid (it needs Genshin Impact, which K5 runs on the PC), NVIDIA's own
block-linear buffers and the scanout modifiers (K2 and K4, on the PC).

Build note: `tools/aosp-device-link.sh` keeps mtimes, so a file edited before the last build's
outputs were written is not copied again by ninja; the first test image had an old
`init.venus.rc`. `touch` the edited files under `aosp-device/` before building.
