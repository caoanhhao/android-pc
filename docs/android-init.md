# Android init on the Rust kernel (phase C3a)

Unmodified AOSP `/init` (android-16.0.0_r4, `aosp_cf_x86_64_only_phone`,
the build in `aosp/out/target/product/vsoc_x86_64_only/`) boots on the
Rust kernel through both stages: first stage mounts the dynamic partitions
of `super.img`, switches root to `/system`, loads the SELinux policy,
second stage starts the property service and ueventd, runs the rc files
through the `early-init`, `init` and `late-init` triggers (and the
`fs`/`post-fs`/`late-fs`/`post-fs-data` ones `late-init` queues), and
starts the `console` service: an interactive `sh` on the serial port in
which `getprop` and `setprop` work.

## Running it

```
tools/limited-build.sh tools/run-qemu.sh --aosp-init        # interactive; Ctrl-A X quits QEMU
tools/limited-build.sh tools/run-qemu.sh --aosp-init-test   # scripted check, exit 0 iff it passes
```

Both build the kernel and `out/aosp-init-initramfs.cpio`
(`tools/mkaosp-init-initramfs.py`), attach `super.img` read-only as the
virtio disk at PCI 00:04.0 (vda), and boot with these parameters
(`run-qemu.sh` puts them on the kernel command line; `/proc/bootconfig`
shows the `androidboot.*` ones in bootconfig syntax, so init reads them
from either place):

| parameter | why |
|-----------|-----|
| `androidboot.fstab_suffix=rustpc` | first stage reads `/fstab.rustpc` from the ramdisk |
| `androidboot.boot_devices=pci0000:00/0000:00:04.0` | the virtio disk is the boot device: `/dev/block/by-name/*` links |
| `androidboot.partition_map=vda,super` | the whole disk vda is the `super` partition (no GPT around super.img) |
| `androidboot.slot_suffix=_a` | `slotselect` partitions are `system_a`, ... |
| `androidboot.selinux=permissive` | userdebug build: SELinux permissive (the stub allows everything anyway) |
| `androidboot.hardware=cutf_cvm` | Cuttlefish's vendor rc files and HAL choices |
| `androidboot.vendor.apex.*=...` | which of Cuttlefish's alternative vendor APEXes apexd activates (the values cvd-1 uses); without them apexd-bootstrap aborts on duplicates |
| `androidboot.console=ttyS0`, `serialno`, `hardware.*`, `lcd_density`, ... | what Cuttlefish's bootconfig sets, so `${ro.boot.*}` expansions in the rc files work |

`--aosp-init-test` (`tools/aosp-init-test.py`) follows init's log on the
serial console (init writes it to /dev/kmsg, which the kernel prints as
`kmsg: init: ...`) and checks these milestones in order: first stage,
dm-linear `system_a`/`vendor_a`, `/system` mounted, switch_root,
`/vendor` mounted, `selinux_setup` opening the policy, the policy load,
second stage, the property service socket, ueventd's coldboot,
`early-init`, `init`, the `console` service, `late-init`. Then it types
commands into the console shell: `getprop` of build, boot and product
properties, `getprop | grep -c` (hundreds of properties), `setprop` +
`getprop` of a new property, the shell's SELinux context
(`u:r:shell:s0`, from the service's `seclabel`), and `/proc/mounts`
showing `/` and `/vendor` on dm devices. 22 checks; the serial log is in
`kernel/target/x86_64-unknown-none/<profile>/qemu-kernel/aosp-init-test.log`.

## The ramdisk (`tools/mkaosp-init-initramfs.py`)

AOSP's own generic ramdisk, unchanged, taken out of `init_boot.img`
(AOSP's host `unpack_bootimg` and `lz4`): the static first-stage `/init`,
`/system/etc/ramdisk/build.prop`, snapuserd, the empty mount points. One
file is added: `/fstab.rustpc`, the first-stage fstab. It lists the
logical partitions (`system`, `vendor` required; `system_ext`, `product`,
`odm`, `*_dlkm` with `nofail`), all erofs, read-only, `slotselect`,
without AVB. Cuttlefish's `vendor_boot` ramdisk is not used: it adds
kernel modules (no module loader here) and an fstab whose `/data`,
`/metadata` and AVB entries cannot be served yet.

## What happens, and what the kernel provides for it

**First stage** (`/init` from the ramdisk, static):
- mounts tmpfs on /dev, devpts, proc (`hidepid=` ignored), sysfs,
  selinuxfs, tmpfs on /mnt, /debug_ramdisk, /second_stage_resources;
  `mknod`s /dev/kmsg, null, random, urandom, ptmx in the tmpfs /dev;
  copies the ramdisk build.prop (`sendfile`).
- block devices: writes "add" to the `uevent` files under /sys/class,
  /sys/block, /sys/devices and reads the uevents from a
  NETLINK_KOBJECT_UEVENT socket (`net::netlink`, `fs::sysfs`); creates
  /dev/block/vda and /dev/block/by-name/super (from `partition_map`
  and the PCI path in the uevent's DEVPATH) and /dev/device-mapper (the
  `misc` uevent of `devices/virtual/misc/device-mapper`).
- reads the super metadata (liblp) and creates one dm-linear device per
  logical partition (libdm ioctls, C2), waits for each dm-N's uevent,
  mounts `/system` (erofs), moves every mount below it and the root
  itself (`MS_MOVE` onto `/`, then `chroot(".")`), mounts vendor,
  product, ... and frees the ramdisk (unlinking every file).
- execs `/system/bin/init selinux_setup`.

**SELinux setup** (dynamically linked, from erofs): reads the
precompiled policy and writes it to /sys/fs/selinux/load (in 64 KiB
chunks; one load), keeps enforce at 0 (permissive), labels
`/system/bin/init` (already labelled in the image), execs the second
stage.

**Second stage**: `/dev/__properties__` (property area files created,
`fsetxattr` labelled, mapped shared), the property service on
`/dev/socket/property_service` (AF_UNIX stream, SO_PEERCRED, SO_PEERSEC,
the per-context permission checks go through libselinux and are
allowed), restorecon of /dev and friends (`security.selinux` xattrs on
tmpfs), signalfd/eventfd/epoll (including epoll on /proc/mounts for
POLLPRI, `MountHandler`), mount namespaces (`unshare(CLONE_NEWNS)`,
`setns`, /proc/self/ns/mnt), the `vendor_init` subcontext, all rc files
of system, system_ext, vendor, product and the bootstrap APEXes.
`SetupCgroups` mounts cgroup2 on /sys/fs/cgroup and the cpuset
(`noprefix`), cpu and blkio v1 hierarchies under /dev; every service gets
`/sys/fs/cgroup/{system,apps}/uid_N/pid_M`, which is removed when it
exits (`cgroup.procs`, `cgroup.kill`, rmdir). `SetMmapRndBits` and
`SetKptrRestrict` write /proc/sys and read the values back. Services are
reaped with `waitid(P_ALL, WNOWAIT)` + `waitpid`.

Started along the way: ueventd (coldboot over the same uevent mechanism,
`ro.cold_boot_done` set), apexd-bootstrap (10 bootstrap APEXes on loop
devices, 97 loop devices and 87 dm devices pre-allocated), linkerconfig
under logwrapper (a pseudo-terminal: /dev/ptmx and devpts), logd,
servicemanager and hwservicemanager (binder, C1), vold, keystore2, the
early HALs, tombstoned, `console`.

## Kernel pieces added in C3a

| piece | where | what |
|-------|-------|------|
| uevents | `net/netlink.rs`, `fs/sysfs.rs` | AF_NETLINK sockets; NETLINK_KOBJECT_UEVENT broadcast to group 1 with sender port 0 and SCM_CREDENTIALS; writing an action to a sysfs `uevent` file sends it; device registration/removal sends add/remove; other protocols answer dumps with NLMSG_DONE |
| sysfs device tree | `fs/sysfs.rs`, `block/mod.rs`, `drivers/virtio/mod.rs` | /sys/devices/pci0000:00/0000:00:BB.F/virtioN/block/vda[/vda1], devices/virtual/{block,mem,tty,misc}/..., devices/platform/serial8250/tty/ttyS0; `subsystem` links; /sys/class, /sys/block, /sys/dev/{block,char} symlinks; queue/*, holders/, slaves/ |
| selinuxfs | `fs/selinuxfs.rs`, `security.rs` | see [syscalls.md](syscalls.md#selinux-stub-c3a) |
| per-process contexts | `security.rs`, `fs/procfs.rs` | /proc/<pid>/attr/{current,prev,exec,fscreate,keycreate,sockcreate} and task/<tid>/attr; exec applies `exec` or the `*_exec` -> domain guess; new files labelled from `fscreate` (sockets from `sockcreate`); SO_PEERSEC |
| xattrs | `fs/tmpfs.rs`, `syscall/fs.rs` | set/get/list/remove on tmpfs (and devtmpfs, sysfs, ...), XATTR_CREATE/REPLACE; `security.selinux` reads `u:object_r:unlabeled:s0` when none is stored |
| cgroups | `fs/cgroupfs.rs` | cgroup2 and v1 hierarchies, membership per process, cgroup.procs/tasks/kill/events, stored control values |
| /proc | `fs/procfs.rs`, `fs/sysctl.rs` | /proc/sys (a writable table), bootconfig, cgroups, mountinfo, pid/{cgroup,attr,task,ns,oom_score_adj,...}, pollable mounts, magic links (fd/N, ns/*, exe, cwd) |
| mounts | `fs/vfs.rs` | MS_MOVE (also onto `/`, which then is the root), chroot to the current root, mount generation for poll; new types selinuxfs, cgroup, cgroup2, devpts (shared, with the pty nodes), tracefs/debugfs/pstore/bpf/configfs/fusectl/functionfs (empty) |
| pseudo-terminals | `fs/pty.rs` | /dev/ptmx, devpts /dev/pts/N, TIOCGPTN/TIOCSPTLCK/TIOCGPTPEER, termios stored, ONLCR on slave output, hang-up both ways |
| inotify | `fs/inotify.rs` | see [syscalls.md](syscalls.md) |
| keys | `syscall/keys.rs` | add_key, request_key, keyctl (session keyring, "fscrypt" keyring, logon keys) |
| other syscalls | `syscall/*.rs`, `process/lifecycle.rs` | waitid (WNOWAIT), sendfile, copy_file_range, unshare/setns, syslog, sethostname, ioprio, reboot RESTART2 (with the reason), more prctl options; console tty: TIOCSCTTY makes the caller's group the foreground one |

## What is stubbed or missing (C3b, C3c and later)

- **SELinux** is not enforced: there is no policy engine. The policy is
  accepted, every access check allows, `enforce=1` is stored only. There
  is no `class/` tree in selinuxfs, so libselinux logs "Unknown class"
  for its checks (and allows them, `deny_unknown` is 0). Domain
  transitions on exec are guessed from the program's `*_exec` label for
  the kernel and init domains only; `seclabel` (setexeccon) works.
- **/data and /metadata** do not exist (the read-only system image's
  empty directories): nothing provides a writable userdata partition yet
  (ext4 writes, f2fs, FBE), and `mount_all` finds no default fstab after
  the ramdisk is freed. aconfigd, keystore2, tombstoned, vold's
  checkpointing and the keymint/gatekeeper HALs fail; keystore2 is a
  `critical` service, so after its fourth crash (about 20 s after
  `late-init`) init reboots to the bootloader, which ends the QEMU run.
  The test's checks finish well before that.
- **ueventd** only sees the devices the kernel registers (block, mem,
  tty, misc); there is no firmware loading, no modalias handling and no
  input/graphics/net devices (`wait /dev/dri/card0` times out after 5 s).
  `dlkm_loader` and modprobe fail: there are no loadable modules
  (`finit_module` is ENOSYS).
- **logd** runs without /proc/kmsg and audit netlink; **adbd** is not
  reached (C3c).
- **cgroups** store limits but enforce nothing (no memory, cpu or cpuset
  control); PSI files exist but never trigger.
- **keys** have no permission checks, quotas or expiry; one set of
  special keyrings for the whole system.
- **pseudo-terminals** have no line discipline (no echo, canonical mode
  or signals on the slave); good enough for logwrapper, not yet for an
  interactive `adb shell`.
- **namespaces**: one mount namespace; `unshare`/`setns` succeed without
  effect, chroot only to the current root.
- `prng_seeder` waits forever for /dev/hw_random; `process_vm_readv` and
  `ptrace` are missing, so crash_dump cannot unwind crashing services.
