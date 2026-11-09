# Android init on the Rust kernel (phases C3a, C3b, C3c, C3d)

Unmodified AOSP `/init` (android-16.0.0_r4, `aosp_cf_x86_64_only_phone`,
the build in `aosp/out/target/product/vsoc_x86_64_only/`) boots on the
Rust kernel through both stages: first stage mounts the dynamic partitions
of `super.img`, switches root to `/system`, loads the SELinux policy,
second stage starts the property service and ueventd, runs the rc files
through the `early-init`, `init` and `late-init` triggers (and the
`fs`/`post-fs`/`late-fs`/`post-fs-data` ones `late-init` queues), and
starts the `console` service: an interactive `sh` on the serial port in
which `getprop` and `setprop` work. Since C3b the core native daemons run
for real (ueventd, servicemanager, hwservicemanager, logd, lmkd, vold,
keystore2 with the keymint/gatekeeper HALs, apexd, tombstoned, ...):
`service list`, `logcat`, `dmesg` and `ps` work from that shell. Since
C3c the kernel has eBPF maps, programs (stored, not run) and bpffs:
`load-bpf-programs` succeeds (`bpf.progs_loaded=1`), init starts netd and
zygote and stays up. Since C3d there is a network (virtio-net, smoltcp's
TCP/IP, rtnetlink, interface ioctls, stored netfilter tables): netd
finishes its setup, and adbd listens on TCP port 5555, so `adb connect`
and `adb shell` from the host work (see
[How far the boot gets](#how-far-the-boot-gets-c3d)).

## Running it

```
tools/limited-build.sh tools/run-qemu.sh --aosp-init        # interactive; Ctrl-A X quits QEMU
tools/limited-build.sh tools/run-qemu.sh --aosp-init-test   # scripted check, exit 0 iff it passes
tools/limited-build.sh tools/run-qemu.sh --aosp-adb-test    # adb from the host (C3d), exit 0 iff it passes
```

With `--aosp-init` running, adb from the host (any adb; the build's is
`aosp/out/host/linux-x86/bin/adb`) reaches the guest through QEMU's
user network, which forwards host 127.0.0.1:15555 (`--adb-port PORT`
to change it) to the guest's port 5555:

```
adb connect 127.0.0.1:15555
adb -s 127.0.0.1:15555 shell
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
| `brd.rd_nr=2 brd.rd_size=4096` | two RAM disks, ram0 and ram1: the block devices first-stage init waits for before mounting the tmpfs /metadata and /data |

`--aosp-init-test` (`tools/aosp-init-test.py`) follows init's log on the
serial console (init writes it to /dev/kmsg, which the kernel prints as
`kmsg: init: ...`) and checks these milestones in order: first stage,
dm-linear `system_a`/`vendor_a`, `/system` mounted, switch_root,
`/vendor` mounted, `selinux_setup` opening the policy, the policy load,
second stage, the property service socket, ueventd's coldboot,
`early-init`, `init`, the `console` service, `late-init`,
`post-fs-data`, the switch to the default mount namespace and apexd's
activation. Then it types commands into the console shell: `getprop` of
build, boot and product properties, `getprop | grep -c` (hundreds of
properties), `setprop` + `getprop` of a new property, the shell's SELinux
context (`u:r:shell:s0`, from the service's `seclabel`), `/proc/mounts`
showing `/` and `/vendor` on dm devices, and (C3b) the daemons:
`ro.cold_boot_done` (Android 16 has no `/dev/.coldboot_done` file any
more) and `/dev/hw_random` with ueventd.rc's owner and mode; keystore2
registered and `service list` with servicemanager's own `manager`,
keystore2, keymint and vold; servicemanager, hwservicemanager, keystore2
and logd running; `logcat -d` with service logs, `logcat -b kernel` with
init's kmsg lines (logd's klogd), `dmesg`; `ps` showing logd,
servicemanager and keystore2 under their own users; tmpfs /metadata and
/data with `/data/misc/keystore`; and (C3c) the `load-bpf-programs` and
`bpf-progs-loaded` triggers with netbpfload's "success" as milestones,
then `bpf.progs_loaded` = 1, the bpfloader service stopped,
`/sys/fs/bpf/netd_shared/mainline_done`, bpffs mounted with netd's
pinned maps and programs (owner and mode as netbpfload set them), netd's
`libnetd_updatable_init success` in logcat (its bpf setup: map locks,
cgroup attach and query), and no reboot: `sys.powerctl` still empty and
the shell still answering 10 s later. 41 checks, done about 55 s into
the boot; the serial log is in
`kernel/target/x86_64-unknown-none/<profile>/qemu-kernel/aosp-init-test.log`.

## The ramdisk (`tools/mkaosp-init-initramfs.py`)

AOSP's own generic ramdisk, unchanged, taken out of `init_boot.img`
(AOSP's host `unpack_bootimg` and `lz4`): the static first-stage `/init`,
`/system/etc/ramdisk/build.prop`, snapuserd, the empty mount points. One
file is added: `/fstab.rustpc`, the first-stage fstab. It lists the
logical partitions (`system`, `vendor` required; `system_ext`, `product`,
`odm`, `*_dlkm` with `nofail`), all erofs, read-only, `slotselect`,
without AVB, and (C3b) tmpfs on `/metadata` and `/data`
(`/dev/block/ram0` and `ram1` as their nominal devices, which tmpfs
ignores; first-stage init waits for a block device per non-logical
entry). Both start empty on every boot: post-fs-data creates the
directory tree, keystore2, tombstoned, apexd and vold use it. Cuttlefish's `vendor_boot` ramdisk is not used: it adds
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

## C3b: the core daemons

What each daemon needed from the kernel:

| daemon | needed | where |
|--------|--------|-------|
| ueventd | uevents and the sysfs tree (C3a); per-thread credentials: its threads switch egid around each `mknod`, so nodes get ueventd.rc's group (`/dev/hw_random` prng_seeder, `/dev/block/dm-*` system) | `process::Creds`, `vfs::create` |
| prng_seeder | `/dev/hw_random` (misc 10:183, `devices/virtual/misc/hw_random`) | `fs/dev.rs`, `fs/sysfs.rs` |
| servicemanager | the vendor APEX VINTF fragments under `/bootstrap-apex`: init binds `/bootstrap-apex` on `/apex` in the bootstrap mount namespace, apexd-bootstrap mounts the APEXes there and they must propagate to `/bootstrap-apex` (shared peer groups); without them keymint/gatekeeper cannot register and keystore2 aborts | `vfs` propagation |
| init | real mount namespaces: `GetCurrentMountNamespace` compares `/proc/self/ns/mnt` with the ids it saved after `unshare(CLONE_NEWNS)`/`setns`; with one namespace it never sees itself in the default one, so `perform_apex_config` skips the APEX rc files (no derive_classpath, art_boot, ...) | `vfs::MntNs`, `unshare`, `setns`, `clone` |
| logd | `/proc/kmsg` (klogd: `logcat -b kernel` has init's and the kernel's lines), syslog(2) READ_ALL/SIZE_BUFFER, `/dev/kmsg` writes (C3a); its sockets' SO_PEERCRED and SCM_CREDENTIALS now carry the clients' uids | `fs/kmsg.rs`, `Creds` |
| lmkd | PSI triggers on `/proc/pressure/memory` that epoll accepts (it exited "does not support memory pressure events" and was restarted every 5 s; lmkd is `critical`) | `fs/procfs.rs` |
| keystore2, keymint, gatekeeper | servicemanager's VINTF (above), a writable `/data/misc/keystore` (tmpfs /data) | |
| tombstoned, odsign, vold's init_user0, apexd | a writable `/data` and `/metadata` | ramdisk fstab, `block/brd.rs` |
| ps, logd's client checks, binder | credentials: `ps` shows logd, system, keystore, ... and binder's `sender_euid`, `SO_PEERCRED`, `SCM_CREDENTIALS` are the callers' real ids | `Creds` |

Running after ~25 s: ueventd, prng_seeder, logd, lmkd, servicemanager
(about 20 services: keystore2's, keymint, gatekeeper, secureclock,
sharedsecret, vold, apexservice, suspend, ...), hwservicemanager (it
logs "HIDL is not supported on this device" and waits, as on Cuttlefish),
vold, keystore2, the keymint and gatekeeper HALs, system_suspend,
tombstoned, apexd, aconfigd's socket service, the console shell.

## C3c: eBPF

`load-bpf-programs` runs `bpfloader`, which the tethering APEX's rc file
turns into `/apex/com.android.tethering/bin/netbpfload`:

1. netbpfload checks the kernel: uname 6.12.0 (>= 5.10 for 25Q4, an LTS
   branch, 64-bit), writes `/proc/sys/kernel/unprivileged_bpf_disabled`
   and `/proc/sys/net/core/bpf_jit_{enable,kallsyms}` (stored sysctls),
   creates its bpffs directories (`tethering`, `netd_shared`,
   `netd_readonly`, `net_shared`, `net_private`, `loader`), checks that
   no program or map exists yet (PROG/MAP_GET_NEXT_ID say ENOENT), loads
   `offload.o` with its own ELF loader and `test.o`, `clatd.o`,
   `dscpPolicy.o`, `netd.o` with libbpf 1.4 (feature probes, BTF_LOAD,
   map creation with BTF, `.rodata` maps updated and frozen,
   PROG_LOAD), pins each map and program (create location, rename,
   chmod, chown) and checks it through OBJ_GET_INFO_BY_FD (type, sizes,
   flags, and a non-zero JIT length), then writes and pins
   `map_kernel_bugs` and creates `netd_shared/mainline_done`.
2. It execs `uprobestatsbpfload` (the uprobestats APEX: its uprobe
   programs and maps are pinned under `/sys/fs/bpf/uprobestats`), which
   execs the platform `/system/bin/bpfloader` (Rust, libbpf-rs): gpuMem,
   gpuWork, memevents (ring buffers) and the critical bpfRingbufProg
   load; timeInState fails (CO-RE: no `/sys/kernel/btf/vmlinux`) and
   fuseMedia fails (no fuse-bpf), both optional. Its legacy part loads
   `/vendor/etc/bpf/filterPowerSupplyEvents.o` (healthd's, critical).
3. It execs `netbpfload done`, which sets `bpf.progs_loaded=1`; init
   moves on to `bpf-progs-loaded` (start netd) and `zygote-start`.

What the kernel provides is described in
[kernel-architecture.md](kernel-architecture.md#ebpf-bpf-syscallbpfrs-c3c)
and [syscalls.md](syscalls.md#ebpf-c3c): real maps, programs that are
stored but never run, BTF blobs, links and recorded attachments, ids,
bpffs, and OFD locks (netd's `mapLockTest`).

Starting zygote and the many services of class `main` filled the 64 MiB
kernel heap within seconds (a kernel panic): erofs kept a decompression
buffer of up to 64 KiB per open compressed inode. That is now a small
global cache, and the heap stays at about 18 MB (`Slab` in
/proc/meminfo is the kernel heap in use).

## C3d: network and adbd

Every VM has a virtio-net card on QEMU's user-mode network (`-netdev
user`; the guest is 10.0.2.15, the gateway 10.0.2.2); for the AOSP boots
the host's 127.0.0.1:15555 is forwarded to the guest's port 5555. The
kernel side is described in
[kernel-architecture.md](kernel-architecture.md#network-netinet-c3d) and
[syscalls.md](syscalls.md#network-c3d). What the boot does with it:

- The card is `eth0`, configured by the kernel like Linux's `ip=`
  autoconfiguration: up, 10.0.2.15/24, default route via 10.0.2.2.
  Cuttlefish's vendor init runs `rename_netiface eth0 buried_eth0`
  (RTM_SETLINK: name plus "down"); as on Linux, renaming an up link is
  EBUSY and the request changes nothing, so eth0 stays up (Cuttlefish
  itself reaches adbd over vsock and has more cards).
- netd: `iptables-restore` / `ip6tables-restore` (legacy iptables,
  libiptc) read the built-in tables with IPT_SO_GET_INFO/GET_ENTRIES
  and install netd's chains with IPT_SO_SET_REPLACE; the kernel stores
  the rule blobs without enforcing them (`iptables -S` lists netd's
  `bw_*`, `fw_*`, `st_*`, `tetherctrl_*` chains). RouteController's
  policy rules and routes go through rtnetlink (stored; `ip rule`, `ip
  route show table all`), XfrmController's flushes get the netlink stub's
  acks. netd logs "Netd started" (about 2 s of setup) and stays up; it is
  only restarted when zygote aborts (zygote's `onrestart restart netd`).
- adbd: `persist.sys.usb.config=adb` in the build's props makes
  `init.usb.rc` start adbd at `boot` (`sys.usb.configfs` is 0, the
  /sys/class/android_usb writes fail harmlessly). There is no FunctionFS
  (`/dev/usb-ffs/adb/ep0` is absent), so adbd uses only the network:
  `persist.adb.tcp.port=5555` (also in the build) makes it listen on
  `tcp:5555` (an AF_INET6 dual-stack socket) and `vsock:5555` (refused:
  no AF_VSOCK). userdebug without `ro.adb.secure` needs no host key. The
  host's adb connects through the port forward; `adb shell` runs
  commands over the shell protocol (pipes, exit status), `adb push` /
  `pull` use the sync service.

`--aosp-adb-test` (`tools/aosp-adb-test.py`) boots like
`--aosp-init-test`, follows the serial log to `boot` and "started service
'adbd'", then runs the host adb with a server of its own (port 15037, a
throw-away HOME; other adb servers and Cuttlefish's cvd-1 are left
alone): `adb connect`, `adb devices -l` (product
`aosp_cf_x86_64_only_phone`), and through `adb shell`: `getprop
ro.build.version.sdk`, `ls /system`, `id`, adbd's properties, `ip addr`
/ `ip route` (rtnetlink from the shell), adbd's socket in
/proc/net/tcp6, netd's "Netd started" without "Failed to initialize
BandwidthController", netd's chains in `iptables -S` / `ip6tables -S`,
an exit status (`exit 3`), and `adb push` / `md5sum` / `adb pull` of a
300 kB file.

Fixed on the way: `cgroup.events` never notified pollers and kept the
content it had at open. libprocessgroup's KillProcessGroup re-reads it
with pread and polls it for POLLPRI until the killed group is empty, so
every `restart` of a service took the full 2.2 s timeout; with zygote
and the services restarting in a loop, init's action queue fell minutes
behind (adbd started about 4 minutes after `boot`). The file now reads
the current state at offset 0 and signals POLLPRI|POLLERR when a process
exits or changes group; adbd starts within about 10-60 s of `boot`.

## How far the boot gets (C3d)

Everything in [C3c](#how-far-the-boot-gets-c3c) below, plus: netd
completes its initialisation (bandwidth control, firewall, tethering,
strict mode and connmark iptables hooks, RouteController, XfrmController,
the NetdNativeService and MDnsService binder services); adbd starts at
`boot` and serves `adb shell`, `push` and `pull` over TCP. Remaining
failures: zygote still aborts (ART needs userfaultfd, phase D), and with
it its `onrestart` list (netd, audioserver, cameraserver, media,
surfaceflinger, wificond) restarts every few seconds; the graphics
allocator, hwcomposer and other HALs needing DRM or other devices keep
failing; `sys.boot_completed` is not reached. netd logs "cannot find
interface dummy0" (no dummy links) and "Unable to start HIDL
NetdHwService" (no hwservice for it), both harmless.

## How far the boot gets (C3c)

Everything in [C3b](#how-far-the-boot-gets-c3b) below, then
`load-bpf-programs` (about 20-35 s in, under 1 s of loading),
`bpf-progs-loaded`, `zygote-start`, `early-boot`, `boot` with the class
`main`/`hal` services. init stays up (checked for 2+ minutes). Running
or restarting at that point: netd, zygote, statsd, surfaceflinger,
audioserver, cameraserver, media, credstore, wificond and most vendor
HALs (graphics allocator, hwcomposer, health, camera provider, ...).

- **netd**: `libnetd_updatable_init` succeeds (bpf maps opened with
  OFD locks, cgroup programs attached to /sys/fs/cgroup and found by
  BPF_PROG_QUERY), then `iptables-restore`/`ip6tables-restore` fail
  (no netfilter: "Failed to initialize BandwidthController") and netd
  exits; init restarts it every few seconds, and its `onrestart`
  restarts zygote each time. Netfilter, the network stack and
  virtio-net are C3d (fixed there).
- **zygote** (`app_process64`) starts the runtime
  (`ZygoteInit` begins), but ART rejects `/system/framework/boot.art`
  ("read barrier state mismatch"): without userfaultfd (syscall 323) ART
  falls back from the CMC to the CC garbage collector, which the boot
  image was not built for. It is killed by netd's restarts before it
  gets further. Phase D.
- Failing as before: `vendor.boot-default` (no default fstab with
  `/misc`), the minigbm graphics allocator (no DRM device), odsign,
  crash_dump (no ptrace). `sys.boot_completed` is not reached.

## How far the boot gets (C3b)

`early-init`, `init`, `late-init`, `early-fs`, `fs`, `post-fs`, `late-fs`
(class `early_hal`), `post-fs-data`: the switch to the default mount
namespace, apexd's full activation (`apexd.status=activated`, every
APEX in /apex), `perform_apex_config` (the APEX rc files parsed),
`vdc cryptfs init_user0` (succeeds), the /data directory tree, then
`load-bpf-programs`. Until C3c netbpfload (from the tethering APEX)
stopped there for lack of eBPF (`/proc/sys/net/core/bpf_jit_enable`,
the bpf() syscall, maps, programs, bpffs pinning): it exited with status
24 and its `reboot_on_failure` made init reboot
("reboot,bpfloader-failed") about 30 s after boot.

Failing but harmless before that: `vendor.boot-default` (the boot HAL
finds no default fstab with `/misc`), `odsign` (needs ART's
`odrefresh` artifacts), `dlkm_loader` and modprobe (no modules),
`recovery-refresh`, aconfigd's platform init (no
`/metadata/aconfig` content), crash_dump (no ptrace).

## What is stubbed or missing (C3d and later)

- **eBPF** (C3c): programs are never run (no verifier, JIT or
  interpreter): socket filters, cgroup hooks, tracepoints and uprobes do
  nothing, ring buffers stay empty, traffic accounting maps stay as user
  space left them. No kernel BTF (CO-RE objects fail), no fuse-bpf.
- **Network** (C3d): IPv6 never leaves the machine (no neighbour
  discovery, no SLAAC; ::1 works), no IP forwarding, packet sockets,
  tun/tap, dummy/veth links or vsock; netfilter tables and policy rules
  are stored, never applied; raw and ICMP sockets carry no traffic; no
  DHCP client in the kernel (the address is static), and no IP
  fragmentation (datagrams must fit the 1500-byte MTU).
- **SELinux** is not enforced: there is no policy engine. The policy is
  accepted, every access check allows, `enforce=1` is stored only. There
  is no `class/` tree in selinuxfs, so libselinux logs "Unknown class"
  for its checks (and allows them, `deny_unknown` is 0). Domain
  transitions on exec are guessed from the program's `*_exec` label for
  the kernel and init domains only; `seclabel` (setexeccon) works.
- **/data and /metadata** are tmpfs: empty on every boot, no ext4 writes,
  f2fs, file-based or metadata encryption (`mount_all` still finds no
  default fstab: `ro.crypto.state` stays unset, so the
  `zygote-start && ro.crypto.state=...` triggers would not fire either).
- **Credentials** are tracked but never checked, and capabilities are not
  modelled: a daemon that dropped to its uid can do what root can.
- **ueventd** only sees the devices the kernel registers (block, mem,
  tty, misc incl. hw_random, RAM disks); there is no firmware loading,
  no modalias handling and no input/graphics/net/rtc devices (`wait
  /dev/dri/card0` times out after 5 s). `dlkm_loader` and modprobe fail:
  there are no loadable modules (`finit_module` is ENOSYS).
- **logd** gets the netlink stub for NETLINK_AUDIT (no audit messages).
- **cgroups** store limits but enforce nothing (no memory, cpu or cpuset
  control); PSI files exist but never trigger.
- **keys** have no permission checks, quotas or expiry; one set of
  special keyrings for the whole system.
- **pseudo-terminals** have no line discipline (no echo, canonical mode
  or signals on the slave); good enough for logwrapper, not yet for an
  interactive `adb shell`.
- **namespaces**: mount namespaces are real; the other kinds are one
  global instance each (unshare/setns accept them without effect); slave
  and unbindable mounts act as private; chroot only to the current root.
- `process_vm_readv` and `ptrace` are missing, so crash_dump cannot unwind
  crashing services (tombstoned gets nothing).
