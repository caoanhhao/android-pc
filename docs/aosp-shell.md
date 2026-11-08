# AOSP shell on the Rust kernel (phase B milestone)

Unmodified AOSP userspace binaries from the build in `aosp/out/` (see
[aosp-build.md](aosp-build.md)): `/system/bin/sh` (mksh) and toybox,
dynamically linked against bionic through `/system/bin/linker64`, run on
the Rust kernel and give an interactive shell on the serial console.

## Running it

```
tools/limited-build.sh tools/run-qemu.sh --aosp           # interactive shell; Ctrl-A X quits QEMU
tools/limited-build.sh tools/run-qemu.sh --aosp-sh-test   # scripted check, exit 0 iff it passes
```

Both build the kernel, then `out/aosp-initramfs.cpio` with
`tools/mkaosp-initramfs.sh`, and boot it with `init=/init`. Other
`run-qemu.sh` options apply (`--smp N`, `--release`, `--append strace`,
...). The shell's `exit` (or ^D) ends init, and the kernel powers off.

`--aosp-sh-test` runs `tools/aosp-sh-test.py`. It starts QEMU with the
serial console on a pipe, waits for `aosp-init: ready`, then types each
command into the interactive mksh. Each command is followed by
`echo "@@END$((N))@@ rc=$?"`, and the driver waits for that marker
before typing the next command, so the check does not depend on timing.
The output must match regexes and the status must be 0. The checks:

| # | command | checks |
|---|---------|--------|
| 1 | `echo hello-$((6 * 7))` | shell arithmetic, output |
| 2 | `ls -1 /` | the initramfs layout |
| 3 | `cat /proc/self/maps` | toybox, linker64 and libc.so mappings named by path, `[stack]` |
| 4 | `ps -A` | sh is pid 1, ps is its child |
| 5 | `uname -a` | `Linux android-pc ... x86_64` |
| 6 | `id` | `uid=0(root) gid=0(root)` |
| 7 | `toybox seq 100 \| toybox wc -l` | a pipeline: `100` |
| 8 | `seq 1000 \| grep 7 \| sort -n \| tail -n 1` | a 4-stage pipeline: `997` |
| 9 | `sleep 1 & p=$!; echo started; wait $p; echo waited=$?` | a background job and `wait` |
| 10 | `(exit 3) & wait $!; echo status=$?` | exit status through `wait` |
| 11 | `echo android-pc > /data/f && toybox sha256sum /data/f` | sha256 of a file (tmpfs) |
| 12 | `readlink /proc/self/exe; cat /proc/self/cmdline \| tr '\0' ' '` | /proc links and cmdline |
| 13 | `mount \| grep -c ...` | proc, sysfs, devtmpfs, tmpfs mounted by /init |
| 14 | `echo ab-X<DEL>cd` | line editing over the serial line: `ab-cd` |
| 15 | `sleep 600; ...` then ^C | the tty sends SIGINT to the foreground job, and the shell survives |

The serial log of the last run is
`kernel/target/x86_64-unknown-none/<profile>/qemu-kernel/aosp-sh-test.log`.

## The initramfs (`tools/mkaosp-initramfs.sh`)

Nothing is rebuilt. The script reads
`aosp/out/target/product/vsoc_x86_64_only` (or `--product DIR`, or
`$AOSP_PRODUCT_OUT`) and finds `aosp/` from the main git worktree. In the
build, the runtime APEX (linker64, libc, libm, libdl) exists only as
`system/apex/com.android.runtime.apex`. The script unzips its
`apex_payload.img` (erofs) and extracts it with AOSP's host
`out/host/linux-x86/bin/fsck.erofs --extract`. No root is needed.

Layout (real Android paths):

```
/init                          mksh script (below)
/system/bin/sh, toybox         + a symlink per toybox applet (every system/bin -> toybox link in the build)
/system/bin/linker64        -> /apex/com.android.runtime/bin/linker64
/system/lib64/                 NEEDED closure of sh and toybox (readelf -d), searched like the
                               linker's default namespace: libcrypto, liblog, libselinux, libz,
                               libc++, libpcre2, libpackagelistparser, ld-android;
                               libc.so, libm.so, libdl.so -> /apex/com.android.runtime/lib64/bionic/
/apex/com.android.runtime/{bin,lib64/bionic}/
/linkerconfig/ld.config.txt    one non-isolated namespace searching /system/${LIB}
/bin, /sbin -> /system/bin; /etc -> /system/etc
/dev /proc /sys /tmp /data     mount points; /dev/console and /dev/null nodes
```

A device generates `/linkerconfig/ld.config.txt` with `linkerconfig` at
boot. Without the file, the linker prints a warning on every exec, so the
script writes a minimal one.

`/init` (run by the kernel through its `#!/system/bin/sh` line):

```sh
export PATH=/system/bin HOME=/ TMPDIR=/tmp ANDROID_ROOT=/system
/system/bin/mount -t proc proc /proc     # full path, see below
mount -t sysfs sysfs /sys; mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp; mount -t tmpfs tmpfs /data
echo "aosp-init: ready ($(uname -sr))"
exec sh -i
```

The first `mount` uses a full path. Bionic's linker finds the executable
by `stat("/proc/self/exe")`, or by `stat(argv[0])` when /proc is missing
(`linker_main.cpp`, `get_executable_info`). A bare `mount` aborts with
"unable to stat either /proc/self/exe or mount", and it would on Linux
too. On a device, first-stage init is static and mounts /proc first.

mksh runs interactive with its emacs line editor (the tty in raw mode).
Job control (`set -o monitor`) stays off, because pid 1 has no
controlling-tty session. Background jobs, `wait` and ^C still work.

## Kernel changes for this milestone

- **Dynamic ELF**: PT_INTERP loads the interpreter at 0x7fc000000000 and
  enters it with AT_BASE. AT_PHDR, AT_PHNUM and AT_ENTRY describe the
  program, as in Linux's binfmt_elf. AT_RANDOM (16 bytes), AT_SECURE (0),
  AT_EXECFN, AT_HWCAP2 and AT_MINSIGSTKSZ were already there.
- **`#!` scripts** in execve.
- **/proc/<pid>/maps** names the linker's mappings by its path.
- **MADV_WIPEONFORK / MADV_KEEPONFORK**: a VMA flag, and a fork child
  gets zero pages. Bionic's arc4random aborts without it.
- **Console tty with input**: `fs/tty.rs`. The UART is polled from the
  tick into an N_TTY-style line discipline: canonical editing, echo,
  VMIN/VTIME, and ^C/^\/^Z signals to the foreground group. Before this,
  console reads returned EOF.
- **`tools/limited-build.sh`** passes stdin through. Bash gives a
  background job `/dev/null`, so piped or typed input never reached QEMU.

### Syscalls added

The details are in [syscalls.md](syscalls.md).

| Nr | Name | Notes |
|---:|------|-------|
| 97 / 160 / 302 | getrlimit / setrlimit / prlimit64 | per-process limits, inherited by fork, Linux defaults |
| 98 | getrusage | zeros (no CPU accounting) |
| 99 | sysinfo | uptime, RAM, process count |
| 100 | times | zero CPU times, ticks since boot |
| 105, 106, 113, 114, 116, 117, 119 | setuid, setgid, setreuid, setregid, setgroups, setresuid, setresgid | accepted, ids stay 0 (everyone is root) |
| 115 | getgroups | no supplementary groups |
| 118 / 120 | getresuid / getresgid | 0 |
| 122 / 123 | setfsuid / setfsgid | 0 |
| 125 / 126 | capget / capset | all capabilities; capset not enforced |
| 129 / 297 | rt_sigqueueinfo / rt_tgsigqueueinfo | with si_value. Bionic's `abort()` uses it, and so does the linker's fatal-error path |
| 135 | personality | stored |
| 140 / 141 | getpriority / setpriority | nice stored |
| 142-147 | sched_setparam, sched_getparam, sched_setscheduler, sched_getscheduler, sched_get_priority_max/min | policy and priority checked and stored. Bionic's `pthread_create` calls sched_getscheduler |
| 149-152, 325 | mlock, munlock, mlockall, munlockall, mlock2 | no-ops (all memory resident) |
| 203 / 204 | sched_setaffinity / sched_getaffinity | all online CPUs |
| 221 | fadvise64 | hints ignored |
| 309 | getcpu | current CPU |

prctl gained PR_GET/SET_DUMPABLE, PR_GET/SET_KEEPCAPS,
PR_GET/SET_NO_NEW_PRIVS, PR_CAPBSET_READ and PR_SET_VMA (bionic names its
anonymous mappings with it; the names are accepted but not kept).
madvise gained WIPEONFORK/KEEPONFORK. The tty ioctls gained FIONREAD with
real counts, TCSETSF flushing, TCFLSH, TCSBRK/TCSBRKP, TCXONC and
TIOCOUTQ.

## Limitations

- ELF segments are copied into anonymous memory. They are not mapped from
  the page cache, so text is not shared between processes.
- There is no credentials model, so everything runs as root. Scheduling
  policy, nice and affinity are stored only.
- There is no job control: no SIGTTIN/SIGTTOU, and stop signals with the
  default action are ignored.
- UART input is polled at 100 Hz (up to 64 bytes per tick). This is fine
  for typing and for scripted input, since QEMU applies back-pressure.
- The kernel prints its log lines on the same serial port as the shell.

## Binder: AOSP servicemanager (C1 stretch goal)

`tools/mkaosp-initramfs.sh --binder` builds `out/aosp-binder-initramfs.cpio`:
the same shell plus `/system/bin/servicemanager` and `/system/bin/service`
with their library closure (libbinder, libvintf, libselinux, libbase,
libperfetto_c, ...) and the bootstrap linker (`/system/bin/bootstrap/linker64`,
servicemanager's PT_INTERP). Its /init also does what init.rc does for
binder: `mount -t binder binder /dev/binderfs -o stats=global`, symlinks
`/dev/{binder,hwbinder,vndbinder}` into it and chmods them 0666.

```
tools/mkaosp-initramfs.sh --binder
tools/limited-build.sh tools/run-qemu.sh --initrd out/aosp-binder-initramfs.cpio
# in the shell:
ls -l /dev/binder /dev/binderfs
servicemanager &
```

How far it gets (2026-11-08): binderfs and the symlinks look as on a
device; unmodified servicemanager runs, prints "Starting sm instance on
/dev/binder", and its binder setup succeeds (open, BINDER_VERSION,
BINDER_SET_MAX_THREADS, BINDER_ENABLE_ONEWAY_SPAM_DETECTION, the 1 MiB
read-only mmap). libbinder also needs `/proc/sys/kernel/random/boot_id`
(`Bad boot_id` is fatal), which procfs now provides. It then aborts in
`Access::Access()` with `Check failed: selinux_status_open(true) >= 0`:
there is no selinuxfs, and the fallback is an AF_NETLINK socket.
`service list` therefore has no service manager to talk to (libbinder
keeps waiting for it). Getting further needs the SELinux stub planned for
C3: a selinuxfs with the `status` page (mmap) and `access`/`enforce`
files, `/proc/<pid>/attr/current` for getcon(), and the service_contexts
files; servicemanager also sets the `servicemanager.ready` property, which
needs init's property service (an error, not fatal). The binder protocol
paths servicemanager relies on (context manager with security contexts,
handle 0 lookups, BINDER_GET_NODE_INFO_FOR_REF, death notifications,
epoll on the binder fd) are covered by `tests/src/bin/c1_binder.rs`.
Since C3a this is resolved under real AOSP init: with the selinuxfs stub,
/proc/<pid>/attr and the property service, servicemanager (and
hwservicemanager, vold's binder service) run there; see
[android-init.md](android-init.md).

## AOSP images on a disk (C2)

```
tools/limited-build.sh tools/run-qemu.sh --aosp-disk-test
```

Boots `out/aosp-disk-initramfs.cpio` (`mkaosp-initramfs.sh --disk`: the
shell initramfs plus AOSP's `dmctl` and md5 manifests of every file in
system.img and vendor.img, made from a host `fsck.erofs --extract` and
cached in `out/aosp-disk/`) with `aosp/out/.../super.img` attached as a
read-only virtio disk (vda; QEMU opens it with `file.locking=off`, so a
running Cuttlefish is not disturbed). `aosp-sh-test.py --suite disk` then
types, in AOSP's mksh:

1. `/sys/block/vda/ro` and `toybox blockdev` (BLKROGET, BLKGETSIZE64 equal
   to the image size, BLKSSZGET);
2. `ln -s /dev/block/vda /dev/block/by-name/super` and
   `dmctl create-from-super system_a ...` / `vendor_a`: liblp reads the
   dynamic partition metadata from the super device and libdm creates
   dm-0 / dm-1 with linear tables (as first-stage init does);
3. `dmctl list devices`, `dmctl table`, `dmctl getpath`, sysfs `dm/name`;
4. `mount -t erofs -o ro` of both, `/proc/mounts`;
5. `/mnt/system/system/bin/toybox ls /mnt/system/system` (a dynamically
   linked binary straight from erofs);
6. `build.prop` and `vendor/etc/init` contents;
7. and 8. `toybox md5sum -c` of every file of each image against the host
   manifest (2349 files in system);
9. umount and `dmctl delete` (the devices must not be busy any more).

All 9 pass. Other images can be attached with `--disk FILE[,rw|,snapshot]
[,legacy]`, and `--sh-arg --run --sh-arg "CMD"` runs ad-hoc commands
instead of a suite.
