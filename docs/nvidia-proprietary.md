# NVIDIA's own driver on bare-metal Android (Phase K)

Running log for Phase K of `PLAN.md`: the RTX 4070 (AD104, `10de:2786`) driven by NVIDIA's
own driver instead of nouveau or Nova, with the design of `~/projects/waydroid-nvidia`
(Mesa Venus over a vtest socket to `virgl_test_server` on NVIDIA's Vulkan) moved onto the
stick. One section per step. NVIDIA's firmware and userspace binaries are used for this
personal stick only and are not redistributed.

Driver version: **620.70.06**, the build host's `nvidia-open` / `nvidia-utils` (PLAN.md and
waydroid-nvidia started from 615.78.08; the host has moved on). Kernel modules, GSP firmware
and the K2 userspace must all be this exact version.

**On branch `main` this document covers only the kernel side that Phase L (NVIDIA's driver as
the device's own driver, `docs/nvidia-native.md`) runs on: K1 (nvidia-open modules, kCFI fixes)
and K4 (display and sound on nvidia-drm).** Phase K's Android half — the glibc sidecar with the
vtest server (K2), Mesa Venus, the minigbm `vtest` gralloc and ANGLE's gpuid (K3) — and the L1
test entries are on branch `gpu-alternatives`, with this document's full version. The sections
below are a dated log: where they mention Venus, the sidecar, the "NVIDIA (proprietary, Venus)"
entry name, or the nouveau and Nova entries, they describe the stick at that date; today the
entry is "Android 17, NVIDIA (proprietary)" with Phase L's graphics stack.

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
| gralloc | `tools/patches/aosp17-minigbm-nvidia.patch` (Phase L3; K1 used dumb buffers) | minigbm's `nvidia` backend for the DRM driver `nvidia-drm` |
| Boot entry | `tools/mkusb-aosp17.sh --menu` | **"Android 17, NVIDIA (proprietary)"** (K1 named it "..., Venus") |
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

### K1b. First real-PC boot hung: diagnosis and changes (2026-11-15)

**Result of the combined K image** (`out/aosp17-usb-k.img`, build `eng.caoanh.20261115.2013`):
on "Android 17, NVIDIA (proprietary, Venus)" the screen went black after the firmware logo and
the PC hung; pclog has no directory for that boot (`out/pclog-k`: boot-18 nouveau and boot-19
NVK on the same build, both fine).

What that says, offline:

- pclog starts at `on init`. K1 loaded the modules with `exec` in **early-init**, which blocks
  init, so anything that went wrong in `modprobe nvidia-drm.ko modeset=1` (or in nvidia.ko's
  probe: GSP-RM boot, nvidia-drm's KMS takeover) happens before pclog runs: no boot
  directory, exactly as seen. The black screen itself is normal for the entry (pcfb off,
  `console=ttynull`), so it does not tell where it stopped.
- QEMU already showed that the modules link and initialise on the GKI kernel; what QEMU cannot
  reach is the probe of a real GPU, i.e. the whole of NVIDIA's resource manager.
- The most likely kernel-side cause is **kCFI**: distribution kernels build nvidia-open with gcc,
  so NVIDIA's code (NVOC-generated method tables, callbacks cast between function types, the
  kernel-open/RM boundary) has never run with indirect-call type checks. One mismatch is a
  `CFI failure` BUG, and with `PANIC_ON_OOPS` a panic. `cfi=kcfi` only removed the FineIBT
  problem of the C++ virtual calls, not this. Other candidates are a GSP-RM boot failure that
  hangs the GPU/PCIe link and the modeset takeover of the GOP framebuffer.
- A panic should reboot after 10 s (`panic=10`); a hang that stays is either a hard lockup or
  the panic path itself stuck (this board's firmware on the EFI reset or the pstore write).
  The GKI image has `CONFIG_EFI_VARS_PSTORE=y` (enabled), so a panic may already be in the
  board's EFI variables; nothing copied `/sys/fs/pstore` until now.

Changes:

| Piece | Change |
|---|---|
| `init.pc.rc`, `pcnvidia.sh load` | modules loaded at **post-fs** instead of early-init (still before K2's sidecar at late-fs and K4's pcnvdisplay at post-fs-data); the script waits until pclog has mounted its partition, then loads `nvidia`, `nvidia-modeset`, `nvidia-drm modeset=1`, `nvidia-uvm` one at a time, each with a `pcnvidia: step:` kernel log line and a `sync` before and after |
| entry command line (`nvdiag`) | `cfi=kcfi,warn` (a kCFI mismatch prints `CFI failure at ...` with a backtrace and execution continues), `softlockup_panic=1 nmi_watchdog=panic` (a kernel lockup becomes a panic, which efi-pstore saves and `panic=10` reboots) |
| `pclog.sh` | every boot copies `/sys/fs/pstore/*` (efi-pstore records of an earlier crash) into `boot-N/pstore/` and removes them (they would fill the NVRAM); the K1 dump block also runs on the debug entry and includes `pcnvidia.sh debug`'s report |
| new entry **"Android 17, NVIDIA debug (kernel log on screen, driver loads after boot)"** | `androidboot.android_pc.gpu=nvidia-debug`, pcfb on (Android on the GOP framebuffer as usual), `earlycon=efifb keep_bootcon loglevel=7` (the kernel log scrolls over the screen), the same `nvdiag` words. After `sys.boot_completed`, `pcnvidia.sh debug` waits 30 s, then loads `nvidia`, `nvidia-uvm`, `nvidia-modeset`, `nvidia-drm modeset=0` with 10 s pauses (makes `/dev/nvidia*`), writes `/data/vendor/pcgpu/nvidia.txt`, and last reloads `nvidia-drm modeset=1`, which takes the monitor over from pcfb. K2-K4's services do not run there (they key on `gpu=nvidia`) |

QEMU check (build `eng.caoanh.20261115.2202`, `out/aosp17-usb-k1b-qemu.img`):

- NVIDIA entry: `pcnvidia: step: mp -r vkms.ko` at 9.0 s (post-fs), `step: mp nvidia.ko`,
  `NVRM: No NVIDIA GPU found.`, the later steps skip or fail cleanly, vkms back as card0,
  `sys.boot_completed=1` (70 s: K3's wait for the vtest socket on this entry, no GPU here);
  the command line carries `cfi=kcfi,warn` (`SMP alternatives: CFI: mismatch non-fatal!`).
- Debug entry: Android on pcfb, `vendor.pcnvidia_debug` starts at boot completion and runs
  each step 10 s apart (`step: mp nvidia.ko` at 61 s, `mp nvidia-uvm.ko` at 71 s, ...), all
  failing cleanly without the GPU; `nvidia.txt` written, Android keeps running.
- pstore: on the default entry `echo c > /proc/sysrq-trigger` panicked the kernel; it rebooted
  after 10 s (`panic=10`), and on the next boot pclog had copied 17
  `dmesg-efi_pstore-*` records (the panic's kernel log, plain text) into `boot-2/pstore/` and
  emptied `/sys/fs/pstore`. efi-pstore (`pstore: Registered efi_pstore as persistent store
  backend`) is what the PC uses too.

What the next real-PC boot can show: on the debug entry, the last `pcnvidia: step:` line on
the screen (or a `CFI failure`/`NVRM` message, or an oops) names the module and the stage; on
the main entry, `boot-N/dmesg.*` now exists up to the step that died, and `boot-N/pstore/` on
the boot after a panic holds the panic's kernel log.

### K1c. kCFI fixes, Xid 51, safe debug entry (2026-11-15)

**K1b real-PC result** (build `eng.caoanh.20261115.2202`, logs in `out/pclog-k1b`):

- The original hang was kCFI. `boot-20/pstore` held the earlier boot's panic:
  `CFI failure at kgspAllocateScrubberUcodeImage_IMPL+0x3a/0xe0 [nvidia] (target:
  ksec2GetBinArchiveSecurescrubUcode_AD10X+0x0/0x20 [nvidia]; ...)`, `Oops: invalid opcode`,
  `Kernel panic - not syncing: Fatal exception`.
- Main entry with `cfi=kcfi,warn` (`boot-21`): that one CFI failure (logged, not fatal), then all
  four modules load, `[drm] Initialized nvidia-drm 0.0.0 for 0000:01:00.0`, DP-1 connected,
  K2's server allocates on the RTX 4070 from 27.6 s, `sys.boot_completed` at 42.6 s. At 45.09 s
  GSP reports `Xid 51, BAD_TSG encountered on runlist: 0 with error code: 4` and `Xid 154 ...
  (PF FLR)`; every later GPFIFO channel allocation (class `c56f`) fails with
  `NV_ERR_RESET_REQUIRED`.
- Debug entry (`boot-20`): `watchdog: BUG: soft lockup - CPU#6 stuck for 22s! [init:1]` in
  `console_flush_all`, then `softlockup_panic` at 590 s: the earlycon framebuffer console at
  `loglevel=7` was far too slow.

**kCFI mismatches, fixed** (`tools/patches/nvidia-open-kcfi.patch`):

- `ksec2GetBinArchiveSecurescrubUcode_AD10X` and `ksec2GetBinArchiveBlUcode_TU102`: the generated
  BINDATA getters are defined as `(struct KernelSec2 *)`, but KernelSec2's NVOC method table
  declares and calls them as `(struct OBJGPU *, struct KernelSec2 *)`. The bindata files do not
  include the NVOC header, so no compiler sees it; with gcc it works because the getter ignores
  its argument. Now defined with both parameters (a script compared all 64 BINDATA getter
  definitions with their NVOC declarations; these two are the only differences).
- `__iom_dtor_OBJRPC` and `__iom_dtor_OBJRPCSTRUCTURECOPY`: defined as `(POBJRPC)` and cast to
  `NVOC_DYNAMIC_DTOR` (`void (*)(Dynamic *)`), which `objDelete` calls. Now take `Dynamic *`
  (empty bodies). They would have trapped at teardown.
- Search: the cores rebuilt with `-Wcast-function-type-strict` give 720 casts. 710 are NVOC's
  exported-method tables, stored as `void (*)(void)` and cast back to the same `(void *, void *)`
  or `(void *)` type before the call (`rs_resource.c`, `deferred_api.c`, `gpu.c`), which kCFI
  accepts; the timer wrapper in `timer.c` swaps types but always calls through the real one; the
  two IOM destructors are fixed. nvidia-modeset's C code has none.
- Check on the binary: the scrubber call site now checks hash `-0x74827754`, and the target's
  preamble carries `0x8b7d88ac`, the same value.

`cfi=kcfi,warn` stays on the entry until a real-PC boot shows no `CFI failure` line at all
(the search covers casts and the generated getters, not every NVOC path). After that the
entry can use plain `cfi=kcfi`.

**Xid 51.** It is not kCFI: the warn-mode boot logged one CFI failure in 45 s, at 17 s, in the
scrubber getter, which returns the right archive whatever its argument. So the scrubber image was
correct, and GSP and the display ran for 28 s before the Xid. The `BAD_TSG` message and its error
code come from the GSP firmware (not in the open sources). It came about 2 s after boot
completed, when apps start and K2's server creates their Vulkan devices, so it is a
runlist/channel failure under the first real load. The remaining difference from the build
host, where 620.70.06 runs Android games through the same server, is the kernel environment.
The clear one is the **IOMMU**: the GKI image has `CONFIG_INTEL_IOMMU_DEFAULT_ON=y`, so the RTX
4070's DMA (instance blocks, USERD, GPFIFOs in system memory) goes through VT-d remapping; the
Arch host has it off. Both NVIDIA entries now boot with `intel_iommu=off`. That is a hypothesis
for the next real-PC run, not a proven cause. If the Xid stays, the next A/B is K2's: the
sidecar keeps `CAP_SYS_NICE`, so NVIDIA honours the HIGH/REALTIME queue priorities HWUI and
SurfaceFlinger ask for and builds high-priority channels/TSGs, which the host (no SYS_NICE)
never does; after that, the clang code generation of the RM cores.

**Debug entry made safe:** `loglevel=5` (errors, warnings and pcnvidia's markers, now logged at
warning level; not the info/audit flood) and no `softlockup_panic`/`nmi_watchdog=panic`;
`keep_bootcon` stays (without it the screen console stops when `ttynull` registers).

QEMU check (build `eng.caoanh.20261115.2345`): the NVIDIA entry boots on vkms without the GPU
(command line has `cfi=kcfi,warn intel_iommu=off`, `sys.boot_completed` at 90 s, K3's socket
wait included); the debug entry runs every step to the last one (`mp nvidia-drm.ko modeset=1`
at 109 s, `NVRM: No NVIDIA GPU found.`) and Android keeps running on pcfb; the default entry
is unchanged.

**Real-PC test (K1c).** Image `out/aosp17-usb-k1c.img` (`--menu --userdata 64`). Update the
stick in place (keeps apps and data; the partitions are usually not mounted, else
`udisksctl unmount -b /dev/sdaN` first; type the full word `yes`):

```bash
sudo tools/update-usb-aosp17.sh out/aosp17-usb-k1c.img /dev/sda
```

On the PC (Limine remembers the last entry: pick each one by hand):

1. **"Android 17, NVIDIA (proprietary, Venus)"**: wait for the desktop, open two or three apps
   (Settings, Chrome), leave it 5 minutes. Note whether the desktop appears and whether it
   freezes or goes black later. If it reboots by itself, that was a panic (pstore keeps it).
2. **"Android 17, NVIDIA debug (kernel log on screen, driver loads after boot)"**: Android on
   the UEFI framebuffer with warnings/errors scrolling over it; about 30 s after boot the
   `pcnvidia: step: ...` lines appear, 10 s apart. Photograph the screen if it stops, or note
   the last step reached (the screen goes dark at the last step, `nvidia-drm.ko modeset=1`).
3. Boot the default entry once (pclog copies any pstore record), plug the stick into the build
   host, `tools/pclog-read.sh /dev/sda`. In the NVIDIA boot: `dmesg.*` must have **no**
   `CFI failure` and, if the IOMMU was the cause, **no** `Xid`; check `== nvidia modules`,
   `== /proc/driver/nvidia`, `== drm cards`; in later boots `pstore/` if one rebooted itself.

### K1d. Xid 51 trigger, A/B entries, debug entry removed (2026-11-16)

**K1c real-PC result** (build `eng.caoanh.20261115.2345`, `out/pclog-k1c/boot-22`): no
`CFI failure` any more, all modules load, sound from the monitor works; but Xid 51 (`BAD_TSG ...
error code: 4`) and Xid 154 (PF FLR) are still there at 57 s with `intel_iommu=off`, so the
IOMMU was not the cause. After it every GPFIFO allocation fails with `NV_ERR_RESET_REQUIRED`,
every `vkCreateDevice` fails, and 100 apps abort in HWUI's `VulkanManager::initialize`. The
debug entry froze hard (no pclog directory at all).

**The trigger.** logcat carries the kernel lines with their own timestamps, so the Xid lines up
exactly: at 06:09:29.609, 0.07 s after microG's DroidGuard
(`com.google.android.gms.unstable`) started a second ANGLE/Vulkan display on another thread while
the first one (29.394) was still alive, to fingerprint the GPU for the Play Store and Gmail. The
K1b boot's Xid lines up the same way (DroidGuard `dcs_get_verdict` for Gmail, 2 s after boot).
SurfaceFlinger, SystemUI (which asks for a HIGH-priority GPU context) and the launcher had
been drawing through Venus on the RTX 4070 for 20-30 s before. So the GPU dies on a
particular client pattern, not at the first channel. The `BAD_TSG` message and its code come
from GSP firmware.

**Offline checks (2026-11-16).**

- Data layout: four RM translation units that hold the RPC, channel, TSG and GSP code
  (`rpc.c`, `kernel_channel.c`, `kernel_channel_group.c`, `kernel_gsp.c`) were compiled with
  clang and with the host's gcc 16, both with full debug info, and compared with pahole. All
  3,300+ structures have the same size and all ~14,000 member offsets are the same. So the
  kernel RM sends GSP exactly the layout a gcc build sends; an ABI mismatch is ruled out.
- kCFI: K1c's boot logged no `CFI failure`, so it is no longer on the path.
- Queue priority: waydroid-nvidia's notes (`venus/notes-vkcreatedevice.md`) say that on the build
  host `virgl_render_server` has no `CAP_SYS_NICE`, so NVIDIA refuses HIGH/REALTIME global
  priority. Venus patch 0002 then drops the request (Launcher3, SystemUI and SurfaceFlinger all
  ask for it). So the host, where this driver version runs Android with the Play Store (and so
  DroidGuard) and games, has **never created a high-priority channel or TSG**. On the stick, K2's
  server keeps `CAP_SYS_NICE`, so NVIDIA grants the priorities. This is the combination that
  is new here, and the strongest candidate for the `BAD_TSG`.

The K1d A/B entries planned here were not built; Phase L1 (next section) replaced them with
test entries that need no Phase K client at all.

**Debug entry removed.** Both of its freezes came from writing to the UEFI framebuffer after
nvidia.ko owned the GPU: pcfb keeps drawing Android there and earlycon keeps printing the
kernel log there. In K1b the CPU sat for 22 s in `console_flush_all`, each framebuffer write
stalled. Showing the log beside the UI would need the log off that framebuffer (an Android
window instead of the kernel console), which does not help with a freeze. The main entry's
pclog and efi-pstore now cover what the debug entry was for.

### L1, K2, K3

The L1 test entries (log strip, pcnvtest, gcc-built RM), the glibc sidecar (K2) and Venus,
the vtest gralloc and gpuid (K3) are on branch `gpu-alternatives` (this document's full
version there). Phase L replaced them (`docs/nvidia-native.md`).

## K4. Display and sound on nvidia-drm

### What changed

| Piece | Where | What |
|---|---|---|
| Composer | the entry's command line (`$drm`, K1) | Cuttlefish's drm_hwcomposer APEX, unchanged, on nvidia-drm (`modeset=1`): atomic commits, the monitor's preferred mode, hotplug, vblank, fences |
| Card and modifiers | `aosp-device/hals/nvdisplay/pcnvdisplay.cpp` (`/vendor/bin/pcnvdisplay`), `init.nvdisplay.rc` | runs at `post-fs-data` on `androidboot.android_pc.gpu=nvidia`, before class `hal` (composer, allocator). Finds the `nvidia-drm` card by driver name, sets `vendor.hwc.drm.device` to it and logs the modifiers every primary plane scans out (`IN_FORMATS`). In K4 it also published them as `vendor.gralloc.vtest.modifiers.*` for K3's vtest gralloc; since Phase L minigbm's `nvidia` backend picks the scanout layout itself. `vendor.pc.nvdisplay` and `pcnvdisplay:` kernel log lines say what it found |
| Overlay planes | `init.pc.rc` (K1's block) | `ro.vendor.hwc.use_overlay_planes=0` |
| Entry | `tools/mkusb-aosp17.sh` | K1's entry, with K3's `$venus` words in K4 (Phase L's `$nvl3` today) |
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

The K4 real-PC test ran on the Venus entry; on `main` the NVIDIA entry runs Phase L's stack, and
its real-PC checks are in `docs/nvidia-native.md`. For the display part: `== nvdisplay`
(`vendor.pc.nvdisplay` naming the nvidia-drm card, `vendor.hwc.drm.device` = that card) and
`== modetest nvidia-drm` (the DP connector `connected`, a CRTC with the monitor's mode) in the
NVIDIA boot's `state-*.txt`, the eld and audio device sections for the monitor's sound.

If the screen stays black: `vendor.pc.nvdisplay` and the `pcnvdisplay:` lines say whether the
card was found; drm_hwcomposer's errors are in logcat (`hwc`), an import failure there means a
buffer without a scanout modifier.
