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
  VINTF declarations live in `/vendor/etc/waydroid/composer-hidl.xml`;
  `init.waydroid.vendor.rc` bind-mounts them over the empty
  `/vendor/etc/vintf/manifest/waydroid.xml` at early-init, and starts the service (disabled
  on the PC).
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
| system/core init | `/vendor/waydroid.prop` read last | Waydroid's whole configuration (session, `[properties]`) is in that file |
| system/core init | reboot/shutdown in the container: stop every process and exit (the container has no `CAP_SYS_BOOT`), with no unmounting, `EXT4_IOC_SHUTDOWN`/`F2FS_IOC_SHUTDOWN` or bootloader message; no `/proc/sysrq-trigger` crash on a fatal init error; the first stage only remounts `/proc` (a new proc mount would cover LXC's read-only `/proc/sys` and `sysrq-trigger`) and drops `CAP_SYS_BOOT/MODULE/RAWIO/TIME/MAC_*` from the bounding set, with no_new_privs (as upstream's "Drop harmful capabilities") | every mount in the container belongs to the host: the reboot path shut down the host's `/` (see "Container guard") |
| system/core init | services start without an SELinux context; a failing `createProcessGroup()` is a warning | no SELinux on the host (`security_compute_create` fails: "Could not get process context"); the container gets only the cgroup controllers the host delegates |
| system/core libprocessgroup | `SetupCgroups` in the container: no cgroup v1 controllers, no cgroup2 mount (waydroid-preinit remounted the container's own), processes moved from the cgroup root to `/init` so controllers can be enabled below it (as upstream's "Move all processes away from root cgroup") | the host has only cgroup v2; `blkio` (v1) failed first and stopped the setup, so `/sys/fs/cgroup/system` never existed |
| system/core init | user `host` = `waydroid.host.uid` | the composer runs as the desktop user to reach its Wayland socket |
| external/selinux | libselinux: context calls (`getcon`, `getpeercon`, `getfilecon`, `setexeccon`, ...) succeed in the container | init (property service, services), servicemanager, zygote and installd abort when the kernel has no SELinux; this one switch replaces upstream's patches to each of them |
| bionic | property area accepted without its SELinux label | `__system_property_area_init()` fails without the xattr and init aborts |
| frameworks/native libbinder, system/libhwbinder | no `FLAT_BINDER_FLAG_TXN_SECURITY_CTX` in the container | the host kernel has no LSM context to hand over, so binder fails every transaction to servicemanager |
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
- `waydroid-container.service` is disabled and stopped. The next start, on test data, waits for
  the user's OK.
