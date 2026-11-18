# The PC image in Waydroid (Phase W)

The host's Waydroid (1.6.3) runs this tree's AOSP 17 instead of the LineageOS 20 image from
ota.waydro.id. It is **the PC build itself**: `aosp_pc_x86_64-cp2a-userdebug`, the same out
directory, the same system and vendor binaries as the USB stick, x86_64 only. From Waydroid
only the host side is used: its tools, LXC config, binderfs nodes, Wayland/PulseAudio sockets
and `waydroid.cfg`. Plan: PLAN.md, "Phase W". NVIDIA's driver on the LineageOS 20 image:
docs/waydroid-nvidia.md.

## How Waydroid runs an image

- `system.img` is loop-mounted read-only as the LXC rootfs (`/var/lib/waydroid/rootfs`): a
  system-as-root image whose `/init` is the container's first process. There is no kernel,
  ramdisk or boot image: the container shares the host's kernel. Only `system.img` and
  `vendor.img` are mounted: no product, system_ext, odm or *_dlkm partitions.
- `vendor.img` is mounted on `/vendor`. Waydroid bind-mounts its generated `waydroid.prop`
  (`waydroid_base.prop` + `[properties]` of `waydroid.cfg` + the session) on
  `/vendor/waydroid.prop`, and overlay-mounts `/var/lib/waydroid/overlay` (+ `overlay_rw`)
  on both.
- Binder: the host's binderfs nodes (`anbox-binder`, ...) are bind-mounted as
  `/dev/{binder,vndbinder,hwbinder}`. The host side (`waydroid session`) talks to the
  container's servicemanager through libgbinder; `binder_protocol`/`service_manager_protocol`
  are set at every container start from `ro.build.version.sdk` of the system image (API 36+:
  `aidl3`/`aidl6`).
- `/data` is a bind mount of `$XDG_DATA_HOME/waydroid/data` (`~/.local/share/waydroid/data`).
- No SELinux (the host has none), `/sys` read-only, cgroup v2 only, `/dev` a tmpfs with only
  what Waydroid binds (the first non-NVIDIA render node, binder, ...). Waydroid's composer
  turns the host's Wayland input into `/dev/input/wl_*_events` pipes.

## Why the PC build, not a Waydroid-style product

The first attempt (2026-11-18) made a new product `waydroid_x86_64` like upstream Waydroid's:
the GSI board, 32-bit, LineageOS 20's vendor image, then its own vendor. It was dropped: a new
product is a full build of its own (140,000 build steps in the shared out directory, and
Soong's analysis of the 32-bit variant did not fit in the 20 GB build cap), and it would not be
the AOSP 17 the PC runs. Now the Waydroid image is the PC image plus a few modules that only act
in the container, so a build is incremental and Waydroid and the stick run the same code.

## Container guard (host side)

**Incident, 2026-11-18 (twice):** the first boots of this image froze the host. In the container
init failed early (cgroup v1 `blkio` cannot be mounted, so `createProcessGroup()` failed for
every service; `exec_start apexd-bootstrap` failed with "Could not get process context"), so it
rebooted (`reboot,bootloader,bootstrap-apexd-failed`). The reboot path could not unmount `/data`
and ran `EXT4_IOC_SHUTDOWN` on it. In Waydroid `/data` is a bind mount of a directory on the
host's root filesystem and the container keeps `CAP_SYS_ADMIN`, so the kernel shut down the
host's `/` (`EXT4-fs (nvme0n1p2): shut down requested`). Android's init log ends up in the host
journal (`journalctl -b -1 -t init`), since the container writes the host's `/dev/kmsg`.

Waydroid's container is privileged: no user namespace, `sys_admin` and `mknod` kept, no device
cgroup rules, and on this host no AppArmor or SELinux. Its seccomp profile only blocks module
loading, kexec, `reboot`, clock setting, keyrings and swap. Whatever image runs inside could
shut down or freeze host filesystems and open the host's disks. `tools/waydroid/container-guard.sh`
puts that boundary back on the host side:

- **seccomp:** the filesystem shutdown and freeze ioctls fail with `EPERM`:
  `EXT4_IOC_SHUTDOWN` (the same number as `XFS_IOC_GOINGDOWN` and `FS_IOC_SHUTDOWN`),
  `F2FS_IOC_SHUTDOWN`, `FIFREEZE` and `FITHAW`. The request is matched on its low 32 bits,
  because bionic passes ioctl's int request sign-extended.
- **device cgroup:** the container cannot create or open the block devices of disks: IDE, sd,
  md, sr, nbd, mmc, xvd, blkext (NVMe, partitions) and the dynamic block majors 234-254
  (virtio, device-mapper, zram). Loop devices, which APEXes need, stay allowed.

`container-guard.sh install` adds the guard to Waydroid's templates
(`/usr/lib/waydroid/data/configs/{config_base,waydroid.seccomp}`, which every `waydroid
init`/`upgrade` copies) and to the current `/var/lib/waydroid/lxc/waydroid/`. It also installs a
pacman hook (`/etc/pacman.d/hooks/waydroid-container-guard.hook`) that adds the guard again after
each waydroid package update. `container-guard.sh check` reports whether every file has it, and
`aosp17-switch.sh` refuses to switch images without it.

`container-guard.sh test` starts a throwaway LXC container, not Waydroid. It has Waydroid's
capabilities and seccomp profile plus the guard, and runs only a probe
(`tools/waydroid/guard/guardtest.c`). The probe issues the ioctls on a tmpfs, so a broken guard
returns `ENOTTY` instead of touching a real filesystem. Results on 2026-11-18:

| Check | With the guard | Without (`NOGUARD=1`) |
|---|---|---|
| `EXT4_IOC_SHUTDOWN`, also sign-extended | EPERM | reaches the filesystem (ENOTTY on tmpfs) |
| `F2FS_IOC_SHUTDOWN`, `FIFREEZE`, `FITHAW` | EPERM | reach the filesystem |
| a harmless ioctl (`FIONREAD`) | works | works |
| NVMe 259:0, sd 8:0 | mknod refused | **opened**: the host's disks |
| loop 7:0 | opens | opens |

What the guard does not cover: a privileged container with `CAP_SYS_ADMIN` and no LSM can
still mount a new proc or sysfs and write the host kernel's settings or `/proc/sysrq-trigger`.
Closing that needs AppArmor/SELinux on the host, or a user namespace, which Waydroid does not
support. The image side therefore also keeps init away from those paths (below).

## What the PC image carries for Waydroid

Everything is dormant on the PC; it acts only when `/vendor/waydroid.prop` exists (code) or
`ro.boot.android_pc.waydroid=1` (init scripts). Both come from Waydroid's host side only: the
PC's `/vendor` is a read-only, verified image, and `ro.boot.*` comes from its boot loader.

- **Configuration as on the PC, through `waydroid.prop`.** The PC images are configured by
  `androidboot.*` from the boot entry, like Cuttlefish. In Waydroid the same `ro.boot.*` and
  `ro.hardware.*` come from `[properties]` of `waydroid.cfg` (`properties.aosp17`, written by
  `tools/waydroid/aosp17-switch.sh`): software KeyMint and gatekeeper, no composer APEX
  (`ro.boot.vendor.apex.com.android.hardware.graphics.composer=none`), minigbm, ANGLE,
  `ro.hardware.vulkan=nvidia`, Skia on Vulkan.
- **Composer** (`aosp-device/waydroid/waydroid.mk`): Waydroid's `hwcomposer.waydroid` (a Wayland
  client: one xdg toplevel for the full UI, dma-bufs with their DRM modifier through
  `zwp_linux_dmabuf_v1`, the mouse cursor as `wl_shm`, input to `/dev/input/wl_*_events`) in
  AOSP's HIDL composer 2.1 service, run as the host user (`user host`). Fetched from
  `waydroid/android_hardware_waydroid` (`dev/lineage-23.2`) into `hardware/waydroid` by
  `tools/waydroid/aosp17-setup.sh`; `external/wayland-protocols` gets fractional-scale-v1. Its
  VINTF declarations live in `/vendor/etc/waydroid/composer-hidl.xml`; init's Waydroid first
  stage bind-mounts them over the empty `/vendor/etc/vintf/manifest/waydroid.xml`, and
  `init.waydroid.vendor.rc` starts the service (disabled on the PC).
- **gralloc, Vulkan, GLES:** the PC's own: minigbm with the "nvidia" backend, `vulkan.nvidia`
  + nvglibc + NVIDIA's userspace (620.70.06, the host module's version), ANGLE. In the
  container `/dev/dri/renderD128` is the host's iGPU (minigbm has no backend for it) and
  `renderD129` NVIDIA's, made with `/dev/nvidia*` and `/dev/udmabuf` by
  `waydroid-nvidia-nodes.sh` at early-init.
- **waydroid-preinit** (GPL-3.0, `waydroid/android_vendor_waydroid_init`), fetched into
  `external/waydroid-preinit` at a pinned commit and built with `tools/waydroid/preinit/Android.bp`:
  binder nodes if missing, `/dev` permissions, a writable cgroup2 mount, read-only `/sys` and
  `/proc/sys`.

### Changes to AOSP

Upstream Waydroid patches AOSP in ~150 places (`waydroid-patches/base-patches-36` of
`waydroid/android_vendor_waydroid`). Here only what the container cannot boot without, each a
run-time branch (`tools/waydroid/aosp17-patches/`, applied by `aosp17-setup.sh`):

| Project | Change | Why it is needed |
|---|---|---|
| system/core init | `waydroid_first_stage.cpp`: when `/vendor/waydroid.prop` exists, `/init` (second-stage init) runs a short first stage itself: waydroid-preinit, `/dev` and `/proc` remounted instead of mounted, nodes made only if missing, then `second_stage` | Waydroid starts `/system/bin/init` with no argument and no ramdisk; it aborts without a first stage, and the PC's first stage mounts partitions and loads SELinux policy, which the container has neither of |
| system/core init | `mount_all` mounts nothing in the container, "not encryptable" | every fstab entry fails (Waydroid mounted everything), and a failing `mount_all` ends in recovery |
| system/core init | `/vendor/waydroid.prop` read last, without `ro.vndk.lite`; the first stage bind-mounts the composer's VINTF fragment | Waydroid's whole configuration (session, `[properties]`) is in that file; Waydroid sets `ro.vndk.lite` for vendor images like ours and linkerconfig refuses VNDK-Lite; a bind made from init.rc stays in the bootstrap mount namespace, which hwservicemanager does not see |
| system/core init | reboot/shutdown in the container: stop every process and exit (the container has no `CAP_SYS_BOOT`), with no unmounting, `EXT4_IOC_SHUTDOWN`/`F2FS_IOC_SHUTDOWN` or bootloader message; no `/proc/sysrq-trigger` crash on a fatal init error; the first stage only remounts `/proc` (a new proc mount would cover LXC's read-only `/proc/sys` and `sysrq-trigger`) and drops `CAP_SYS_BOOT/MODULE/RAWIO/TIME/MAC_*` from the bounding set, with no_new_privs (as upstream's "Drop harmful capabilities") | every mount in the container belongs to the host: the reboot path shut down the host's `/` (see "Container guard") |
| system/core init | services start without an SELinux context; a failing `createProcessGroup()` is a warning | no SELinux on the host (`security_compute_create` fails: "Could not get process context"); the container gets only the cgroup controllers the host delegates |
| system/core libprocessgroup | `SetupCgroups` in the container: no cgroup v1 controllers, no cgroup2 mount (waydroid-preinit remounted the container's own), processes moved from the cgroup root to `/init` so controllers can be enabled below it (as upstream's "Move all processes away from root cgroup") | the host has only cgroup v2; `blkio` (v1) failed first and stopped the setup, so `/sys/fs/cgroup/system` never existed |
| system/core init | `SetMmapRndBits` does nothing in the container; the first stage makes `/dev/loop-control` and `/dev/block` | ASLR entropy is the host kernel's (`/proc/sys` read-only), and init aborts when it cannot set it; apexd needs loop-control to mount APEXes, and ueventd's cold boot cannot run on the read-only `/sys` |
| system/core init | user `host` = `waydroid.host.uid` | the composer runs as the desktop user to reach its Wayland socket |
| external/selinux | libselinux: context calls (`getcon`, `getpeercon`, `getfilecon`, `setexeccon`, ...) succeed in the container | init (property service, services), servicemanager, zygote and installd abort when the kernel has no SELinux; this one switch replaces upstream's patches to each of them |
| bionic | property area accepted without its SELinux label | `__system_property_area_init()` fails without the xattr and init aborts |
| frameworks/native servicemanager | no caller context lookup (`getpidcon`) in the container | binder hands over no SELinux context, and servicemanager aborts rather than look one up from a binder thread |
| system/hwservicemanager | no caller context lookup in the container | as servicemanager |
| system/netd | iptables and bandwidth-control failures are not fatal in the container; adding an interface to a network goes on without the incoming-packet mark | the host may have only nftables (no legacy iptables, or its modules not loaded); without this `networkAddInterface(eth0)` failed with EREMOTEIO and no per-network ip rules or routes were installed |
| frameworks/native inputflinger | EventHub opens Waydroid's input FIFOs (`/dev/input/wl_{touch,keyboard,pointer,tablet}_events`) as touchscreen, keyboard, mouse and stylus, with no evdev ioctls; `CursorInputMapper` passes the pointer FIFO's absolute position (upstream Waydroid's "EventHub: Add wayland inputs support", "absolute event support for cursor" and keyboard DPAD class, Android 16 versions) | the hwcomposer writes the Wayland seat's events into these FIFOs; stock EventHub never opens them, so the window took no mouse or keyboard input |
| packages/modules/Connectivity | NetBpfLoad: host sysctls and pre-existing host programs are not errors, no uprobestats/platform loaders; netd's map self-test skipped; ClatCoordinator skips SELinux xattr checks | see "Status": host kernel, no CAP_BPF for netd, no SELinux |
| system/security keystore2 | placeholder calling sid in the container | binder hands over no sid; libselinux allows the check |
| system/memory libmeminfo | no abort when the GPU-memory map is missing (container only) | the tracing program behind it is not loaded |
| frameworks/native libbinder, system/libhwbinder | no `FLAT_BINDER_FLAG_TXN_SECURITY_CTX` in the container | the host kernel has no LSM context to hand over, so binder fails every transaction to servicemanager |
| hardware/interfaces hwc2on1adapter | a hotplug of the primary display re-reads its configs and reaches SurfaceFlinger as a reconnect (upstream Waydroid's "hwc2on1: Handle primary hotplugs") | the hwcomposer reports a host window resize as a primary hotplug; the stock adapter drops it ("non-external display"), so Android kept the size the window had at boot (half the screen) |
| (ours) aosp-device/hals/audio | the Rust audio HAL plays to the desktop session's PulseAudio server (`src/pulse.rs`, the native protocol over the socket Waydroid binds at `/run/xdg/pulse/native`) when an output has no ALSA PCM and that socket exists | the container has no `/dev/snd`, so every stream went to the HAL's null device and Waydroid had no sound; capture stays silent |
| system/core ueventd | in the container, input devices get `/dev/input` nodes only if they are gamepads or joysticks (a key in BTN_JOYSTICK..BTN_THUMBR in `/sys/.../capabilities/key`), and those present before the container started are added from `/sys/class/input` | the container receives every host uevent; Android has the desktop keyboard and mouse through the Wayland seat, so their raw devices would type and click twice. padctl's virtual pad ("Xbox Elite Series 2", 045e:0b00, AOSP's `Vendor_045e_Product_0b00.kl`) must reach Android, also when it existed before Waydroid started (cold boot cannot regenerate uevents on the host's read-only `/sys`) |
| system/core init keychords | only character devices in /dev/input are opened | init watches /dev/input for keychords and opens new nodes blocking; the composer's FIFOs there (wl_*_events) hung init (stuck after `vdc keymaster earlyBootEnded`, a zombie vdc never reaped) once /dev/input existed early in boot |
| system/libvintf | the runtime check skips the kernel SELinux policy version in the container when it reads 0 | the host kernel has no SELinux; `VintfObject.verifyBuildAtBoot()` failed ("kernelSepolicyVersion = 0 but required >= 30") and `Build.isBuildConsistent()` made system_server show "There's an internal problem with your device" at every boot (on Android 17 this check is VINTF, not the partition fingerprints) |
| external/wayland-protocols | fractional-scale-v1 added to the client protocols library | hwcomposer.waydroid uses it |

Left out on purpose, until a boot shows they are needed: upstream's cgroup v2 rewrite of the
task profiles (JSON and `task_profiles.cpp`), its apexd change that opens a detached writable
sysfs to configure loop devices (it writes the host's sysfs), the vold/netd/bpf/apexd/lmkd/logd/adb/Connectivity relaxations, the
ashmem/memfd checks in libcutils (Waydroid sets `sys.use_memfd=true` when the host has no
ashmem, which AOSP 17 honours), the SELinux removals in installd/servicemanager/hwservicemanager
(covered by libselinux), the alarm/`WAKE_ALARM` and suspend changes, the multi-window task
streams and everything for host vendors (`vendor_extra`). Each one added later gets a line
in the table with the log that needed it.

## Build

```bash
tools/waydroid/aosp17-setup.sh                   # fetch hwcomposer + preinit, apply the patches
STEPS="system.img vendor.img" tools/aosp17-build.sh   # the PC images, incremental
tools/waydroid/aosp17-pack.sh                    # -> out/waydroid-aosp17/images/
```

`aosp17-pack.sh` (asks for the sudo password: loop mounts keep owners, modes and file
capabilities) puts system_ext, product and odm inside a new EROFS `system.img`, at the root
where the PC mounts them; `vendor.img` is the PC's.

## Install, test, switch back

`tools/waydroid/aosp17-switch.sh` (run as your user; it asks for the sudo password):

**Never start Waydroid without the user's explicit OK for that run** (CLAUDE.md, "Waydroid
rule"). Before any start, `tools/waydroid/container-guard.sh check` must report the guard
present, and the image must carry the init changes above.

```bash
tools/waydroid/container-guard.sh install
tools/waydroid/aosp17-switch.sh backup
tools/waydroid/aosp17-switch.sh install out/waydroid-aosp17/images
# first boot on an empty data directory, the user's data untouched:
XDG_DATA_HOME=$HOME/.local/share/waydroid-test waydroid show-full-ui
# then on the real data (see "Userdata"):
waydroid show-full-ui
```

- `backup` stops Waydroid and copies `~/.local/share/waydroid/data` to
  `~/.local/share/waydroid-backup/lineage20-<date>/data` (shared storage `media/`, 50+ GB of
  game downloads, as hard links), and `/var/lib/waydroid/{images,overlay,overlay_rw,
  waydroid.cfg,waydroid_base.prop,waydroid.prop}` to `.../var/`.
- `install` copies the two images to `/var/lib/waydroid/images-aosp17/`, starts an empty
  overlay for them (the LineageOS 20 NVIDIA overlay stays with that image), writes
  `properties.aosp17` (the user's own properties without LineageOS 20's libhoudini, gralloc and
  ANGLE ones, plus the PC configuration above), then switches.
- Switching moves `overlay`, `overlay_rw`, `overlay_work` to `<dir>.<image>`, sets
  `images_path` and `[properties]`, and runs `waydroid upgrade --offline`. It then checks the
  container guard and does not start anything. For AOSP 17 the overlay provides what Waydroid
  expects in an image and the PC image lacks: an empty `vendor/waydroid.prop`, which Waydroid
  bind-mounts over (its presence is what tells the image it runs in Waydroid), and the
  `mnt_extra`, `var` and `run` mount points of its LXC config. A run that stopped half-way can
  be repeated. `/var/lib/waydroid/images` (LineageOS 20) is never written.
- `waydroid upgrade` (and other `waydroid` commands) D-Bus-activate `waydroid-container`
  even when the unit is disabled; the switch script stops it again afterwards. The service on
  its own runs no LXC container: a session start does.

Back to LineageOS 20 (AOSP 17 upgrades the userdata; Android cannot run an older system on
it, so the backup comes back too):

```bash
tools/waydroid/aosp17-switch.sh restore ~/.local/share/waydroid-backup/lineage20-<date>
tools/waydroid/aosp17-switch.sh use lineage20
```

`restore` keeps the AOSP 17 userdata as `~/.local/share/waydroid/data.aosp17-<date>`.

## Userdata

The data of LineageOS 20 (Android 13) can boot on AOSP 17 (a forward upgrade), but with a new
vendor image and a new system:

- **Keystore keys are lost:** LineageOS 20's software keymaster and AOSP 17's software KeyMint
  do not share their key-encryption key, so every key an app kept in the Android keystore is
  unusable: logins and tokens stored that way (many apps sign in again), the lock-screen
  credential if one was set.
- **Google apps:** the data came from LineageOS 20 GAPPS (Google Play services signed by
  Google); the PC image has microG under the same package name with another signature, so
  Android clears Play services' data; Google accounts are added again in microG.
- **arm apps:** LineageOS 20 ran them with libhoudini, the PC with libndk_translation
  (arm64 only); their compiled code is regenerated. 32-bit apps no longer run.

## Status

2026-11-18:
- The PC build with the Waydroid pieces is installed as `images-aosp17`; `waydroid.cfg` points to
  it, and the LineageOS 20 overlay is kept in `overlay.lineage20`. The backup is in
  `~/.local/share/waydroid-backup/lineage20-20261118-1222`.
- The real userdata was never mounted. Two test starts on an empty data directory froze the host
  (see "Container guard").
- The container guard is installed and tested. The init/libprocessgroup changes for the reboot
  path and cgroups are built into a new image.
- `waydroid-container.service` is disabled and stopped.
- Test boot 3 (15:22, empty data, with the user's OK): the host was not affected. init got past
  cgroups and SELinux contexts, and `apexd-bootstrap` ran. With no `/dev/loop-control` in the
  container it could not mount any APEX, so dynamic binaries (no runtime linker) failed and
  `boringssl_self_test64` asked for a reboot. The reboot path logged "Waydroid container:
  stopping all processes; no unmount or filesystem shutdown" and the container stopped
  cleanly. The first stage now creates `/dev/loop-control` and `/dev/block`; apexd makes the
  loop nodes itself (`mount_before_data`). The loop devices are the host kernel's, so `losetup`
  on the host shows them while the container runs.
- Test boot 4 (15:29): APEXes mounted and init got to `SetMmapRndBits`, which aborts when it
  cannot write `/proc/sys/vm/mmap_rnd_bits`. That is the host kernel's setting, read-only in the
  container. The fatal-error path ended the container cleanly. In the container this action
  now does nothing.
- Test boot 5 (15:37): init finished early-init/init/late-init and started the services and
  zygote. servicemanager aborted ("Did not get context from PID"): binder hands over no
  security context, and it refuses to look one up from a binder thread. As a critical service
  it then triggered the reboot path, which stopped the container cleanly. In the container it
  now uses an empty context, as upstream Waydroid's servicemanager does. The host's
  systemd-coredump collects the cores of crashing container processes (`coredumpctl`).
- Test boot 6 (15:48): servicemanager ran, and the host session connected to it
  ("Service manager /dev/anbox-binder has appeared"). hwcomposer.waydroid reached the host's
  Wayland display, but could not register: "must be in VINTF manifest". The early-init bind
  mount of `composer-hidl.xml` was made in init's bootstrap mount namespace, which
  hwservicemanager, in the default namespace, does not see. The first stage now makes it,
  before init creates the namespaces. Also, linkerconfig refused to run ("no longer supports
  VNDK-Lite configuration"), because Waydroid puts `ro.vndk.lite=true` in `waydroid_base.prop`
  for "mainline" vendor images. With no linker namespaces, keystore2 and many services exited
  at once; keystore2 is critical, so init took the reboot path, and the container stopped
  cleanly. init now drops `ro.vndk.lite` from `waydroid.prop`, as upstream Waydroid's init
  does.
- Boots 7-15 (15:54-16:20, automatic, empty data):
  - hwservicemanager stayed idle ("HIDL is not supported on this device"), because the PC
    declares no HIDL. The Waydroid fragment now declares `android.hidl.manager@1.2`.
    hwservicemanager then aborted on the missing caller context, like servicemanager; it now
    uses an empty context in the container.
  - The boot-control HAL aborts without a misc partition, and vold's checkpoint step waits for
    it forever, which stalled post-fs-data. `properties.aosp17` disables its APEX
    (`ro.boot.vendor.apex.com.android.hardware.boot = none`).
  - NetBpfLoad stopped the boot (`reboot,bpfloader-failed`) on the read-only host sysctls. In the
    container it now loads no BPF programs into the host kernel. netd's one-shot setup, iptables
    rules and bandwidth control fail without BPF and without legacy iptables on the host
    (nftables only); in the container netd continues.
  - Boot 15: the composer, the allocator, SurfaceFlinger, the boot animation, zygote, netd and
    system_server run. netd still aborts in DnsResolver's `ADnsHelper_init`, which needs
    the BPF maps, and takes zygote/system_server with it every 5 s.
- Boots 16-25 (16:24-17:12; the user chose (a) below): NetBpfLoad loads the network programs.
  The host sysctls are left alone, and the "no programs yet" check is skipped (the host kernel
  has its own). The uprobestats and platform loaders are not run: their tracing programs attach
  kernel-wide, to the host's processes too. netd skips its map-creation self-test: the host keeps
  `unprivileged_bpf_disabled=2`, and netd has no `CAP_BPF` there. More fixes:
  - ClatCoordinator skips its SELinux xattr checks of the pinned maps.
  - libmeminfo returns "no data" for the missing GPU-memory map instead of aborting.
  - keystore2 uses a placeholder calling sid.
  - The first stage reads `/vendor/etc/waydroid/bind-mounts` (composer VINTF; the PC's
    sub-HAL-less `hals.pc.conf`, since SensorService blocked system_server) and puts a tmpfs
    on `/metadata` (aconfig flag storage).
  - `properties.aosp17` turns off the Cuttlefish oemlock, UWB and Thread APEXes and
    `ro.frp.pst`.
- **Boot 25: `sys.boot_completed=1`.** The launcher and taskbar show in the Waydroid window, and
  SurfaceFlinger renders with Vulkan ("RE Vulkan (Ganesh)", `ro.hardware.vulkan=nvidia`,
  `renderD129`). Play Store (`com.android.vending`) and `com.google.android.gms` are installed.
- **Real userdata, 17:45 (user's OK after the losses were listed):** it boots
  (`sys.boot_completed=1`) and keeps the apps (Steam, TCBS, `com.netease.yyvn`, ...). The data is
  not file-based-encrypted, so files are readable. The user had a lock-screen credential: user 0
  stays `RUNNING_LOCKED` behind the keyguard, with `FallbackHome` as home. The credential's
  synthetic-password blob (`/data/system_de/0/spblob`) is protected by LineageOS 20's
  gatekeeper/keystore, so entering it on AOSP 17 is expected to fail. Removing the credential
  (`spblob/` and `locksettings.db`, both in the backup) needs the user's OK. DeskClock crashes
  on LineageOS's newer clock database ("Can't downgrade database from version 12 to 8"); clearing
  its data fixes that.
- **Decided: networking BPF (a).** Android 17's network stack (netd, DnsResolver,
  ConnectivityService, NetworkStats) depends on BPF programs and maps. The options were: Android 17's network stack (netd, DnsResolver,
  ConnectivityService, NetworkStats) depends on BPF programs and maps. Either:
  - (a) like upstream Waydroid, let NetBpfLoad load its programs. They go into the host kernel
    and attach to the container's own cgroup subtree. The host-side guard does not block
    `bpf()`, and a container with `CAP_SYS_ADMIN` could already load any BPF program. Or:
  - (b) keep BPF out and patch the remaining users in the container. That means DnsResolver
    and several Java services in system_server, more and riskier changes.
  The user chose (a), limited to the network programs, which attach to the container's cgroup
  and network namespace.
- **20:30, real userdata:** system_server crash-looped on "Failed to initialize Keystore super
  keys for user 0" (keystore2 could not decrypt the existing CE super key). Fixed by moving
  `system_de/0/spblob`, `system/locksettings.db*` and `misc/keystore/persistent.sqlite` to
  `~/.local/share/waydroid-backup/keystore-reset-20261118-2032`; the next boot completes with no
  lock credential.
- `waydroid show-full-ui` waited forever for the `waydroidplatform` service (Waydroid's
  `IPlatform`, W4). Workaround on older images:
  `sudo waydroid shell -- sh -c 'setprop waydroid.active_apps Waydroid; input keyevent KEYCODE_WAKEUP'`.
  The image now has it (below).
- **IPlatform:** `WaydroidPlatform` (aosp-device/waydroid/platform), a persistent system-uid app
  on system_ext, registers `waydroidplatform` (`lineageos.waydroid.IPlatform`, the host's
  transaction codes in `tools/interfaces/IPlatform.py`) when `/vendor/waydroid.prop` exists; on
  the PC it does nothing. It is Java, not Rust: everything it does (app list, launch intents,
  PackageInstaller, Settings) is framework API. With no SELinux in the container it accepts only
  uid 0, 1000 (the host's desktop user maps to it) and 2000. Settings modes: 0 system, 1 secure,
  2 global (`policy_control`).
- **Network on the real data (21:00):** eth0 came up as `buried_eth0` (Cuttlefish's
  `rename_eth0`; init.waydroid.vendor.rc now sets `ro.vendor.disable_rename_eth0=1`), netd
  installed no routes (see the netd row above), and `restricted_networking_mode` was 1 in the
  LineageOS data (nothing in this image sets it), which dropped every app uid without the
  restricted bit. Fixed by hand at runtime (`settings put global restricted_networking_mode 0`
  persists); the first two are fixed in the image.
- **21:00, real userdata, image with IPlatform, input and network fixes:** boots in ~25 s;
  `waydroidplatform` registered; `waydroid app list`, `show-full-ui` and
  `waydroid app launch com.android.settings` work (Settings resumed, `waydroid.active_apps` and
  `policy_control` set). `dumpsys input` lists wayland_touch (TOUCH_MT), wayland_keyboard
  (KEYBOARD, ALPHAKEY, DPAD), wayland_pointer (CURSOR, source MOUSE, axes sized to the
  1260x1058 window) and wayland_tablet; mouse and key events reach the dispatcher. eth0 keeps
  its name, netd installs the `eth0` routing table, and the container reaches the internet.
- **21:17, hwc2on1 primary hotplug:** a host resize now reaches SurfaceFlinger ("host resized
  Waydroid#0 ...", then "Reconnecting display 0"), and the display follows. The user data sets
  `persist.waydroid.width/height` (2560x1068), which the hwcomposer uses as the display size.
- **21:41, sound:** with the PulseAudio output in the audio HAL, an app's AAudio stream
  (Chromium) shows up on the host as sink input "Android" (application.id `android-pc`,
  s16le 2ch 48 kHz) on the default sink.
- **21:55, gamepads (padctl):** tested with a uinput pad shaped like padctl's output (045e:0b00,
  "Xbox Elite Series 2"), since the Vader 5 Pro was unplugged. Created before Waydroid started:
  `/dev/input/event30` and `js0` exist after boot, EventHub classes `KEYBOARD | GAMEPAD |
  JOYSTICK | EXTERNAL` with `Vendor_045e_Product_0b00.kl`, and `getevent` shows its BTN_GAMEPAD
  presses. Removed and created again: the nodes go and come back (padctl's profile switches
  recreate the device the same way). A uinput keyboard hotplugged at the same time gets no node.
  The user then confirmed the real Vader 5 Pro (through padctl) works in a game.
- **22:00, keyboard and mouse dead, then no boot:** with a pad present at boot, ueventd made
  /dev/input as root:root 0755 before the composer, which could then not create its FIFOs
  (keyboard and mouse dead). init.waydroid.vendor.rc now makes /dev/input system-owned in
  early-init; with the directory there that early, init's keychords watcher opened a FIFO and
  hung init. With keychords limited to character devices (22:09) it boots in ~10 s with the
  four wl_*_events FIFOs, the wayland devices and the real pad.
- **21:03, Where Winds Meet reaches the game world** on the real data (image of commits 124b5a9 +
  eb48bbf): arm64 through `libpc_native_bridge`, `checker choose Vulkan` on `vulkan.nvidia`
  (RTX 4070), the character and HUD in the world (`out/waydroid-aosp17-wwm-world.png`). What it
  needed besides the image fixes:
  - `settings put global restricted_networking_mode 0`: the old data had it on, so the BPF owner
    match dropped every app uid without `RESTRICTED_MATCH` (the game showed "Failed to connect to
    update server" while root could reach the Internet).
  - `waydroid prop set persist.waydroid.suspend false`: with the Waydroid window on a hidden
    workspace Android went to sleep and paused the game.
  - Earlier runs during the image swap died with SIGILL in `libnvidia-gpucomp.so` and SIGSEGV
    (the game's own handler swallows it, empty minidump); on the new image neither came back.
