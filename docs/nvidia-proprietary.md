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

## K2. glibc sidecar: NVIDIA's userspace and the vtest server

NVIDIA's userspace is built for glibc and Android is bionic, so the Vulkan driver runs in its
own small glibc world inside Android: a read-only root with glibc, NVIDIA's userspace at the
kernel modules' version (620.70.06) and waydroid-nvidia's patched `virgl_test_server`. Venus
in Android (K3) and minigbm's vtest gralloc talk to it over the unix socket
`/dev/socket/vtest`, as Waydroid talks to the host's server.

### What was added

| Piece | Where | What |
|---|---|---|
| Root builder | `tools/nvsidecar-build.sh [NV_VERSION]` | builds virglrenderer, assembles the root in `aosp/device/android-pc/nvsidecar/` (not in git) |
| vtest patches | `tools/patches/nvsidecar-vrend-000{1,2,3}-*.patch` | waydroid-nvidia's `vrend-0001..0003`: `RESOURCE_IMPORT_BLOB` (v5), `ALLOC_DMABUF_IMAGE` (v6), `SYNC_EXPORT_FD` (v7) on virglrenderer 1.3.0 |
| Host test | `tools/nvsidecar-hosttest.sh` | runs the root in a bwrap sandbox on the host's own RTX 4070 |
| Install | `aosp-device/hals/nvsidecar/hal.mk` | copies the root to `/vendor/bin/nvsidecar/` (`PRODUCT_COPY_FILES`) |
| Service | `aosp-device/hals/nvsidecar/{nvsidecar.sh,init.nvsidecar.rc}` | `vendor.nvsidecar`, only on `ro.boot.android_pc.gpu=nvidia` |
| SELinux | `aosp-device/hals/nvsidecar/sepolicy/` | domain `nvsidecar`; types `nvsidecar_file`, `nvidia_device`, `vtest_socket`, `nvsidecar_data_file` |
| Logs | `aosp-device/pc/pclog.sh` | `== nvsidecar`, `== nvidia-smi -q (sidecar)`, `== avc nvsidecar` |

`hals/nvsidecar` is picked up by the `hals/*/hal.mk` and `hals/*/sepolicy` globs, so no shared
makefile changed. It is not a HAL; it only borrows that wiring (as K4's `hals/nvdisplay` does).

### The root

`tools/nvsidecar-build.sh` (about 256 MiB):

```
/vendor/bin/nvsidecar/
  bin/virgl_test_server, bin/virgl_render_server   virglrenderer 1.3.0 + vrend-0001..0003
  bin/nvidia-smi                                   NVIDIA's binary
  lib/ld-linux-x86-64.so.2, libc.so.6, ...         the build host's glibc (2.44) and deps
  lib/libvirglrenderer.so.1, libvulkan.so.1        + GLVND libEGL.so.1, libGLdispatch.so.0
  lib/libGLX_nvidia.so.0, libEGL_nvidia.so.0, libnvidia-{glcore,eglcore,glsi,tls,glvkspirv,
      gpucomp,rtcore,allocator}.so.*, libnvidia-ml.so.1          NVIDIA 620.70.06
  share/vulkan/icd.d/nvidia_icd.json               library_path = absolute path of libGLX_nvidia
  share/glvnd/egl_vendor.d/10_nvidia.json          library_path = absolute path of libEGL_nvidia
  VERSION
```

- **The dynamic loader.** virglrenderer is linked with
  `--dynamic-linker=/vendor/bin/nvsidecar/lib/ld-linux-x86-64.so.2` and a `RUNPATH` of
  `/vendor/bin/nvsidecar/lib`, so the kernel starts it with the root's own glibc loader and
  `virgl_test_server` can exec `virgl_render_server` directly. NVIDIA's `nvidia-smi` keeps
  `/lib64/ld-linux-x86-64.so.2` and is run through the loader explicitly
  (`.../lib/ld-linux-x86-64.so.2 .../bin/nvidia-smi`). The service also sets
  `LD_LIBRARY_PATH` to the root's `lib/` for what NVIDIA and the loaders `dlopen`.
  (Meson's compiler sanity check cannot run a program with that interpreter on the build host,
  so the link arguments are set with `meson configure` after the first setup.)
- **Why `/vendor/bin`.** `fs_config` makes only `vendor/bin/*` mode 0755; the loader and the
  programs must be executable (`execve` of an interpreter needs its x bit), and the board has
  no `config.fs`. The whole root therefore sits below `/vendor/bin`.
- **NVIDIA's files** come from the host's `/usr/lib` when the host runs `NV_VERSION`, otherwise
  from `NVIDIA-Linux-x86_64-<ver>.run` (downloaded to `out/downloads/`, `--extract-only`).
  Each library is stored under its soname (no symlinks, which `PRODUCT_COPY_FILES` would
  duplicate). glibc and the other dependencies are the `ldd` closure of the root.
- **What NVIDIA's Vulkan driver needs** was found by running the root in the sandbox with
  `LD_DEBUG=files`. In 620.70.06 the Vulkan entry points (`libGLX_nvidia.so`) bring the
  driver up through EGL: GLVND's `libEGL.so.1` → `libEGL_nvidia.so` →
  `libnvidia-eglcore.so`. Without them `vk_icdGetInstanceProcAddr` returns no
  `vkCreateInstance` and the loader reports "Found no drivers" (never seen on the host, whose
  `/usr/lib` has them). `libGLX_nvidia.so` also `dlopen`s X11/xcb libraries (`libX11-xcb`,
  `libxcb-{dri3,glx,present,randr,sync,xfixes}`); they are in the root, no X server is
  involved. `libnvidia-rtcore` (ray tracing) is loaded at device creation.
  Not shipped: CUDA, OptiX, NVENC/NVDEC, NGX, the OpenGL front ends, `libdbus` (optional).
- **Size.** vendor.img was 345 MiB; the Cuttlefish vendor group (`odm vendor vendor_dlkm
  odm_dlkm`) has 1404 MiB, so the root fits the existing `super` layout: no partition or group
  change, and `tools/update-usb-aosp17.sh` updates the stick in place (it rewrites `super` as a
  whole and keeps `userdata`).

### The service

`init.nvsidecar.rc`, on `ro.boot.android_pc.gpu=nvidia` only:

- at `late-fs`: relabel K1's `/dev/nvidia*` nodes (`pcnvidia.sh` makes them with `mknod` as su
  in early-init, so they carry `/dev`'s `device` label) to `nvidia_device`, then
  `start vendor.nvsidecar`. late-fs comes before K3's `init.venus.rc` waits for the socket at
  post-fs-data, and before `class_start core/hal` at boot;
- at `post-fs-data`: `/data/vendor/nvsidecar{,/cache}` (HOME, NVIDIA's shader cache; used only
  once clients connect);
- `vendor.nvsidecar`: `user root` (only to create its socket in `/dev/socket`, which is
  root:root 0755) with every capability dropped but `SYS_NICE` (NVIDIA's queue priorities),
  `group graphics`, `stdio_to_kmsg`, `oneshot`, `disabled`.

`nvsidecar.sh` (domain `nvsidecar`):

1. prints the root's `VERSION`;
2. exits without a socket when there is no `/dev/nvidiactl` or `nvidia-smi -L` sees no GPU
   (QEMU, a module that did not load, a version mismatch): K3's `pcvenusprobe.sh` then finds
   no `/dev/socket/vtest` and boots on SwiftShader;
3. otherwise, with umask 0 (apps connect with their own uids), runs
   `virgl_test_server --venus --no-virgl --no-fork --multi-clients --socket-path /dev/socket/vtest`
   with `VK_DRIVER_FILES` = the root's `nvidia_icd.json` only,
   `__EGL_VENDOR_LIBRARY_FILENAMES` = the root's NVIDIA EGL vendor file only and
   `RENDER_SERVER_EXEC_PATH` = the root's `virgl_render_server`, and starts it again 2 s after
   it exits. Its output (and the server's) reaches the kernel log through `stdio_to_kmsg`,
   tag `nvsidecar` in `logcat -b all`. These are waydroid-vtest.service's flags without
   `--rendernode`: with NVIDIA's ICD alone, vrend-0002's dma-buf allocator takes the only
   device with the export extensions.

### SELinux

`nvsidecar` is an init daemon domain (`nvsidecar_exec` = `/vendor/bin/nvsidecar.sh`); the glibc
programs run from `nvsidecar_file` without a transition. It may use the vendor shell and toybox,
write the kernel log, read and execute its root, open `nvidia_device` and the DRM render nodes
(`gpu_device`; cards only `getattr`), read `proc` and `sysfs` (`/proc/driver/nvidia`, PCI and DRM
devices), `execmem` (NVIDIA's shader compiler), `sys_nice`, its data directory, create
`vtest_socket` in `/dev/socket` (type transition on the name `vtest`) and use unix stream and
seqpacket sockets (both `unix_stream_socket` to SELinux). Clients (`untrusted_app_all`,
`platform_app`, `priv_app`, `system_app`, the other core domains, the allocator, composer and
camera HAL servers; not `app_zygote` and the SDK sandbox, which the platform forbids unix
sockets) may write the socket, `connectto` the server and exchange fds with it. Core processes
connecting to a vendor socket is outside Treble's default, so the domain carries
`socket_between_core_and_vendor_violators`; the wire format is vtest, not a system API, and the
daemon only runs on the NVIDIA entry. The entry boots with `androidboot.selinux=permissive`
(K1), so missing rules show up in pclog's `== avc nvsidecar` instead of breaking the boot; they
are to be added from the first real-PC log.

### Tests

**Build host (bwrap).** `tools/nvsidecar-hosttest.sh` runs the root in a sandbox that has only
`/vendor/bin/nvsidecar`, `/dev/nvidia*`, `/dev/dri`, `/proc`, `/sys` and an empty `/data` (no
`/usr`, no `/lib64`), with the service's flags and environment on a private socket under
`$XDG_RUNTIME_DIR` (Waydroid's `waydroid-vtest` service and socket are not touched), and the
host's Mesa Venus as the client:

```
== nvidia-smi -L (sandbox)
GPU 0: NVIDIA GeForce RTX 4070 (UUID: GPU-735380de-...)
== virgl_test_server (sandbox), vulkaninfo through Venus (host)
	apiVersion         = 1.4.334
	deviceName         = Virtio-GPU Venus (NVIDIA GeForce RTX 4070)
	driverName         = venus
	conformanceVersion = 1.4.0.0
== vtest-sync-fd (waydroid-nvidia's patched Venus, protocol 7)
  real sync_file fds: 10 of 10
PASS (0 failures)
```

**QEMU (no NVIDIA GPU).** vendor and super built on top of the K1 and K3 commits (vendor.img
543 MiB, was 345 MiB; SELinux policy with every neverallow check passing), image
`out/aosp17-usb-k2-qemu.img` (`--menu --userdata 2048 --append "console=ttyS0,115200
androidboot.android_pc.vulkan=venus androidboot.hardware.guest_hwui_renderer=skiavk"`),
`default_entry` set to "NVIDIA (proprietary, Venus)" in its `limine.conf`, booted with
`NO_WINDOW=1 AUDIO=none tools/run-qemu-aosp17-usb.sh`, read over adb:

| Check | Result |
|---|---|
| boot | `sys.boot_completed=1` |
| service | started at late-fs, `nvsidecar: nvidia 620.70.06 ...` then `nvsidecar: no /dev/nvidiactl (no NVIDIA GPU, or nvidia.ko did not load): not starting`, exit 0 after 43 ms; `init.svc.vendor.nvsidecar=stopped`, no `/dev/socket/vtest` |
| fallback | `vendor.venus.probe=swiftshader`, `ro.hardware.vulkan=pastel` (K3's probe found no socket) |
| labels | `/vendor/bin/nvsidecar.sh` `nvsidecar_exec`, the root `nvsidecar_file` (mode 0755 from fs_config), `/data/vendor/nvsidecar{,/cache}` `nvsidecar_data_file` root:graphics 0770 |
| second start | a stray `/dev/nvidiactl` (root:root, no driver behind it) appeared 30 s after boot; `start vendor.nvsidecar` then got past the first check and stopped at the second: `nvidia-smi sees no GPU: not starting` |
| glibc on Android | `ld-linux-x86-64.so.2 nvidia-smi -L` runs (NVIDIA's "couldn't communicate with the NVIDIA driver", exit 9); `virgl_test_server` with the service's environment starts on Android's kernel and binds its socket |
| SELinux | no `avc: denied` for `nvsidecar`, `vtest_socket` or `nvidia_device` |

The service's output goes to the kernel log, but on this boot the dmesg ring had wrapped
before adb came up; logd's copy (`logcat -b all`, tag `nvsidecar`) has it, so pclog reads it
from there. `virgl_test_server` run by hand without `LD_LIBRARY_PATH` stops at `libexpat.so.1`
(a dependency of `libgbm`, which has no run path of its own): the root needs the environment
the service sets.

**Real PC (pending, with K4's image).** In pclog's `boot-N/state*`: `== nvsidecar` shows
`init.svc: running`, `virgl_test_server` in `u:r:nvsidecar:s0` and `/dev/socket/vtest` as
`vtest_socket`; `== nvidia-smi -q (sidecar)` lists the RTX 4070 with driver 620.70.06;
`getprop vendor.venus.probe` is `venus`; `== avc nvsidecar` lists the rules still missing.

### Changing the driver version

Kernel modules and userspace must match exactly. After K1's `tools/nvidia-build.sh` builds a
new version, run `tools/nvsidecar-build.sh <version>` (no argument: the host's version), then
`tools/nvsidecar-hosttest.sh` if the host runs that version, then the vendor image.

### Status

**Code done, checked on the build host (bwrap, the RTX 4070) and in QEMU (starts, fails
cleanly), real-PC test pending.** No release image: K4 builds the combined one. Done when, on
the real PC, `nvidia-smi` from the sidecar lists the RTX 4070 and `virgl_test_server` runs
(pclog `== nvsidecar`, `== nvidia-smi -q (sidecar)`).

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

## K4. Display and sound on nvidia-drm

### What changed

| Piece | Where | What |
|---|---|---|
| Composer | the entry's command line (`$drm`, K1) | Cuttlefish's drm_hwcomposer APEX, unchanged, on nvidia-drm (`modeset=1`): atomic commits, the monitor's preferred mode, hotplug, vblank, fences |
| Card and modifiers | `aosp-device/hals/nvdisplay/pcnvdisplay.cpp` (`/vendor/bin/pcnvdisplay`), `init.nvdisplay.rc` | runs at `post-fs-data` on `androidboot.android_pc.gpu=nvidia`, before class `hal` (composer, allocator). Finds the `nvidia-drm` card by driver name, sets `vendor.hwc.drm.device` to it, reads `IN_FORMATS` of every primary plane and writes the modifiers they all scan out as `vendor.gralloc.vtest.modifiers.<fourcc hex>.<n>` for K3's minigbm vtest backend. `vendor.pc.nvdisplay` and `pcnvdisplay:` kernel log lines say what it found |
| Overlay planes | `init.pc.rc` (K1's block) | `ro.vendor.hwc.use_overlay_planes=0`, as on the nouveau entries |
| Entry | `tools/mkusb-aosp17.sh` | K1's "Android 17, NVIDIA (proprietary, Venus)" gets K3's `$venus` words |
| Logs | `aosp-device/pc/pclog.sh` | `== nvdisplay` (props, `pcnvdisplay`/`nvidia-drm` dmesg), `== modetest nvidia-drm` (connectors, encoders, planes with types and AB24 modifiers) |

### How the pieces fit

- **Which card.** Cuttlefish's `init.cutf_cvm.rc` loads vkms in `early-init` when no
  `card0` exists after 5 s (I1). K1 unloads vkms, loads nvidia-drm and loads vkms again, so
  nvidia-drm is `card0`; `pcnvdisplay` does not rely on it and points drm_hwcomposer
  (`vendor.hwc.drm.device`) at the card whose `drmGetVersion` name is `nvidia-drm`. It drops
  DRM master right after each open (the first opener of a primary node becomes master, and
  drm_hwcomposer needs it). It waits up to 30 s for that card only when the PCI bus has an
  NVIDIA display controller (`10de`, class `03`); without one (QEMU) it returns at once and
  changes nothing, so the composer stays on `card0` (vkms).
- **Scanout-able buffers.** On this RTX 4070 with nvidia-open 620.70.06 (checked with the
  same code on the build host, whose monitor is on the same card), every primary plane's
  `IN_FORMATS` has, for AB24/XB24/AR24/XR24/RG16/AB30/XB30/AB4H/XB4H, `LINEAR` and
  `NVIDIA_BLOCK_LINEAR_2D` kind 6, GEN 2, sector layout 1, uncompressed, block heights 0-5
  (`0x0300000000606010`-`0x0300000000606015`). The vtest server allocates window buffers with
  NVIDIA Vulkan and picks, from the list gralloc passes, a modifier with color-attachment
  features that is dma-buf exportable; for NVIDIA that is `…606010`-`…606015` (LINEAR has no
  color-attachment support), usually `…606014` (waydroid-nvidia `venus/notes-dmabuf.md`). So
  every buffer drm_hwcomposer is handed for scanout is one the primary plane takes, and the
  dma-buf exported by the sidecar's VkDevice is imported by `drmPrimeFDToHandle` +
  `drmModeAddFB2WithModifiers` without a copy.
- **Composition.** Overlay planes are off: SurfaceFlinger composes every layer itself
  (RenderEngine on ANGLE → Venus → NVIDIA Vulkan) into its client target, the only buffer
  scanned out. That target is allocated with `HW_COMPOSER` usage (`BO_USE_SCANOUT`, the
  vtest server's SCANOUT flag) and, through the list above, always gets a scanout modifier:
  this is the "client composition into a scanout-capable buffer" path, taken for every
  frame. nvidia-drm has 4 CRTCs, each with a primary, an overlay (same `IN_FORMATS`) and a
  cursor plane; the overlays and cursors stay unused. Enabling overlays is a K5 experiment
  (`ro.vendor.hwc.use_overlay_planes=1`); drm_hwcomposer's test-only commit already falls back
  to client composition for a layer the planes refuse.
- **CPU buffers.** drm_hwcomposer imports every layer's buffer into the card when
  SurfaceFlinger hands it over, also for layers it then composes on the client. On the Venus
  entry minigbm makes CPU, cursor and video buffers as LINEAR udmabufs (K3). nvidia-drm
  620.70.06 imports those too: on the build host a 64x64 udmabuf goes through
  `drmPrimeFDToHandle` and `drmModeAddFB2WithModifiers(ARGB8888, LINEAR)` on the NVIDIA card
  without error, so they cost no failed imports or log noise.
- **Mode, hotplug, vsync, fences.** nvidia-drm exposes `OUT_FENCE_PTR` and `VRR_ENABLED` on
  its CRTCs, `IN_FENCE_FD` on every plane, `DRM_CAP_ADDFB2_MODIFIERS`, vblank events and
  hotplug uevents (`modeset=1`). drm_hwcomposer offers the connector's modes with the EDID's
  preferred one first (the LG ULTRAGEAR's 2560x1080 at 144 Hz), gets present fences from
  `OUT_FENCE_PTR`, and passes the client target's acquire fence as `IN_FENCE_FD`; nvidia-drm
  waits for it before the flip. That fence is a real NVIDIA sync_file when Venus and the
  server use vtest protocol 7 (`VCMD_SYNC_EXPORT_FD`, waydroid-nvidia `venus/notes-fence.md`),
  else `-1` after Venus's CPU wait; either is correct, as NVIDIA attaches no implicit fences
  to these dma-bufs. Connector names follow nvidia-drm's numbering (on the build host the DP
  monitor is `DP-2`; nouveau called the same port `DP-1`).
- **Sound on the monitor.** The NVIDIA driver registers no DRM audio component; the
  `10de:22bc` codec driver (`snd-hda-codec-nvhdmi`, `nvhdmi.c` in 6.18) then stays on
  unsolicited jack events, and the GPU writes the ELD to the codec when the DP link carries a
  mode. The build host shows this with the same driver, card and monitor:
  `/proc/asound/card2/eld#0.0` `monitor_present 1`, `eld_valid 1`, `monitor_name LG
  ULTRAGEAR`. From there the chain is J1's: jack on → `WiredAccessoryManager` → AudioService
  connects `HDMI Out` → the F9h Rust audio HAL's `Endpoint::connect_hdmi` picks the HDMI PCM
  whose jack is on. No code change; drm_hwcomposer's first commit on the connector lights the
  link. As on the nouveau entries, a screen Android turns off disables the CRTC and the sound
  moves to the onboard codec until it comes back.

### QEMU check (2026-11-15)

Build `eng.caoanh.20261115.2013` (K1-K4), image `out/aosp17-usb-k4-qemu.img` (`--menu
--userdata 2048 --append console=ttyS0,115200`), entries 1 and 5-9 booted once each with
`NO_WINDOW=1 tools/run-qemu-aosp17-usb.sh` and a vsock adb (`default_entry` set in the image's
`limine.conf` with mtools). QEMU has no NVIDIA GPU, so on the NVIDIA entry the check is that
every K piece steps aside and the entry still boots.

| Entry | `sys.boot_completed` | Notes |
|---|---|---|
| 1 SELinux permissive | 1 (31 s) | ranchu on pcfb, `Pipeline=Skia (OpenGL)` |
| 5 NVIDIA nouveau | 1 (35 s) | drm_hwcomposer on vkms (`card0`) |
| 6 NVIDIA GPU (NVK + ANGLE) | 1 (35 s) | same, NVK falls back to `pastel` |
| 7 NVIDIA nouveau, VTS run | 1 (35 s) | same (stopped before the VTS run) |
| 8 NVIDIA Nova | 1 (30 s) | ranchu on pcfb |
| 9 NVIDIA (proprietary, Venus) | 1 (66 s) | see below |

On the NVIDIA entry the command line now carries `androidboot.android_pc.vulkan=venus
androidboot.hardware.guest_hwui_renderer=skiavk` next to K1's words. `pcnvdisplay` finds no
`10de` display controller and returns in 4 ms (`vendor.pc.nvdisplay=none: no NVIDIA GPU`,
`pcnvdisplay: no NVIDIA GPU, composer stays on /dev/dri/card0`), so drm_hwcomposer stays on
vkms and no `vendor.gralloc.vtest.modifiers.*` is set; `vendor.nvsidecar` is `stopped` (K2),
`vendor.venus.probe=swiftshader` after K3's 20 s socket wait (the longer boot), so
`ro.hardware.vulkan=pastel` and ES 3.1. The only K4 denial is init executing
`/vendor/bin/pcnvdisplay` (`vendor_file`, `permissive=1`), the same as K1's `pcnvidia.sh`; the
entry is permissive. Bluetooth's crash loop (no controller in QEMU) and a `dlkm_loader`
abort on the pcfb entries show on the other entries as well and are not from K4.

### Status

**Code done, QEMU check passed, real-PC test pending.** Release image
`out/aosp17-usb-k.img` (`--menu --userdata 64`, build `eng.caoanh.20261115.2013`, security
patch 2026-06-05) carries K1-K4 together.

### Real-PC test

Unmount the stick's FAT partitions if the desktop mounted them (`udisksctl unmount -b
/dev/sda2`, `udisksctl unmount -b /dev/sda5`; do not open the stick in a file manager), then
update it in place (keeps apps and data; type the full word `yes`):

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-k.img /dev/sda
```

On the PC (monitor on the RTX 4070's DisplayPort), entry **"Android 17, NVIDIA (proprietary,
Venus)"**:

1. Picture: the GOP picture, a short black screen while GSP boots, then the lock screen at
   2560x1080 on the monitor. Unlock and use the desktop for a minute (launcher, settings,
   a scroll in Chrome).
2. Sound on the monitor: play a video; the output device should be "HDMI Out" and the sound
   should come from the monitor.
3. `adb shell dumpsys gfxinfo com.android.systemui | grep Pipeline` → `Skia (Vulkan)`;
   `adb shell getprop vendor.venus.probe` → `venus`.
4. Where Winds Meet and Genshin Impact: frame rate (in-game counter or `dumpsys gfxinfo`)
   against the same scene on the Nova entry.
5. Back on the build host, `tools/pclog-read.sh /dev/sda`; in the nvidia boot's
   `state-*.txt`: `== nvdisplay` (`vendor.pc.nvdisplay` naming the nvidia-drm card and
   `AB24:7 ...`, `vendor.hwc.drm.device` = that card, the modifier props), `== modetest
   nvidia-drm` (the DP connector `connected`, a CRTC with the 2560x1080 mode), the eld and
   audio device sections (`monitor_present 1`, `HDMI Out` connected), and K2/K3's sections
   (`vendor.nvsidecar` running, `vendor.venus.probe=venus`).

If the screen stays black: `vendor.pc.nvdisplay` and the `pcnvdisplay:` lines say whether the
card was found; drm_hwcomposer's errors are in logcat (`hwc`), an import failure there means a
buffer without a scanout modifier.
