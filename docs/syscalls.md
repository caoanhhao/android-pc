# Syscall table (Rust kernel)

Linux x86_64 syscalls the kernel in `kernel/` implements, by phase. The
dispatcher is `kernel/kernel/src/syscall/mod.rs`; every number not listed
returns `-ENOSYS` and is logged once (`unimplemented syscall N`). Boot with
`--append strace` to log every call. What AOSP needs overall is in
[syscall-trace.md](syscall-trace.md).

Status: **yes** = implemented with Linux semantics for what the B1 tests and
static programs use; **partial** = works for the common case, gaps listed;
**stub** = accepted, does nothing useful yet.

## Implemented (phases B1-B4b, B5 AOSP shell, C1 binder, C2 block devices, C3a/C3b/C3c Android init, daemons and eBPF)

| Nr | Name | Status | Notes |
|---:|------|--------|-------|
| 0 | read | yes | VFS files, devices, pipes/FIFOs, sockets (= recv 0), /proc, eventfd/timerfd/signalfd; regular files read in 64 KiB chunks until done |
| 1 | write | yes | VFS files (O_APPEND atomic), devices, pipes (EPIPE + SIGPIPE, atomic up to PIPE_BUF), sockets (= send 0) |
| 2 | open | yes | = openat(AT_FDCWD); block device nodes open the device (EROFS for writing a read-only one); writing a regular file on erofs/ext4 or a MS_RDONLY mount gives EROFS |
| 3 | close | yes | |
| 4 | stat | yes | = newfstatat(AT_FDCWD, 0) |
| 5 | fstat | yes | any fd (pipes S_IFIFO, sockets S_IFSOCK, devices S_IFCHR with rdev); uid/gid/times stored |
| 6 | lstat | yes | = newfstatat(AT_SYMLINK_NOFOLLOW) |
| 7 | poll | yes | POLLNVAL for bad fds, negative fds ignored, ERR/HUP always reported; timeout in ms (< 0 = forever); EINTR on a signal; EINVAL above 1024 fds |
| 8 | lseek | yes | SEEK_SET/CUR/END; directories (getdents index); ESPIPE on pipes; devices accept and stay at 0 |
| 9 | mmap | partial | anonymous memory lazily zero-filled; MAP_FIXED, MAP_FIXED_NOREPLACE, hint; MAP_SHARED anonymous gets its own shmem object (shared across fork; growing it with mremap gives SIGBUS past the original size). File mappings of tmpfs/memfd files, and of erofs/ext4 files (a read-only page cache filled from the disk on first touch), map the page cache: MAP_SHARED writes are seen by read() and every other mapping, MAP_PRIVATE is COW from the cache; pages past EOF raise SIGBUS (the tail of the last page reads zero). EACCES for a write-only fd, or PROT_WRITE + MAP_SHARED on a fd not open for writing (or O_APPEND); EPERM for PROT_WRITE + MAP_SHARED on a write-sealed memfd. /dev/zero: anonymous memory (shared or private). Binder devices: the binder buffer (see Binder below). Other files ENODEV; EBADF for a bad or O_PATH fd |
| 10 | mprotect | yes | splits VMAs, updates PTEs (COW-shared pages stay read-only), TLB shootdown; EACCES when adding PROT_WRITE to a shared mapping of a read-only fd or write-sealed memfd |
| 11 | munmap | yes | frees pages, TLB shootdown IPI to CPUs using the address space |
| 12 | brk | yes | lazy heap after the highest PT_LOAD |
| 13 | rt_sigaction | yes | SA_SIGINFO, SA_RESTORER (required), SA_ONSTACK, SA_RESTART, SA_NODEFER, SA_RESETHAND; setting SIG_IGN flushes pending |
| 14 | rt_sigprocmask | yes | SIGKILL/SIGSTOP never blocked |
| 15 | rt_sigreturn | yes | restores registers, mask, FPU state from the rt_sigframe |
| 16 | ioctl | partial | FIOCLEX/FIONCLEX/FIONBIO on any fd, FIONREAD on pipes, FIONREAD (SIOCINQ)/SIOCOUTQ on sockets, FIONREAD on netlink and inotify fds; console tty: TCGETS/TCSETS/TCSETSW/TCSETSF (F flushes input; switching ICANON keeps pending input), TIOCGWINSZ/TIOCSWINSZ, TIOCGPGRP/TIOCSPGRP (the foreground group ^C/^\/^Z signal), FIONREAD, TIOCOUTQ (0), TCFLSH, TIOCSCTTY (the caller's session and process group become the console's, so a shell started after setsid is in the foreground), TCSBRK/TCSBRKP/TCXONC/TIOCNOTTY (no-ops), TIOCGSID; pseudo-terminals: TIOCGPTN, TIOCSPTLCK/TIOCGPTLCK, TIOCGPTPEER, TCGETS/TCSETS*, TIOCGWINSZ/TIOCSWINSZ, TIOCGPGRP/TIOCSPGRP, FIONREAD; binder devices: the BINDER_* ioctls and BINDER_CTL_ADD (see Binder below); block devices, loop, device mapper: see Block devices below; everything else ENOTTY |
| 17 | pread64 | yes | ESPIPE on pipes/devices |
| 18 | pwrite64 | yes | |
| 19 | readv | yes | on a socket: one receive scattered over the iovecs (a datagram is not split over reads) |
| 20 | writev | yes | gathered into one write |
| 21 | access | yes | = faccessat; root: only X_OK can fail |
| 22 | pipe | yes | = pipe2(0) |
| 23 | select | yes | fd_sets up to 1024 fds; EBADF for a closed fd in a set; the timeval is updated with the time left |
| 24 | sched_yield | yes | |
| 25 | mremap | partial | anonymous, shared and file mappings: shrink, grow in place, MREMAP_MAYMOVE, MREMAP_FIXED, MREMAP_DONTUNMAP; pages move by moving PTEs, a file/shared VMA keeps its object and offset; old_size 0 on a shared mapping (with MAYMOVE) maps the same pages again, EINVAL otherwise; the old range must lie in one VMA (EFAULT) |
| 26 | msync | yes | argument checks only (EINVAL: unaligned address, unknown flags, MS_SYNC with MS_ASYNC; ENOMEM: unmapped range); every file is RAM-backed |
| 27 | mincore | yes | 1 per page that is mapped or in its object's page cache; EINVAL unaligned, ENOMEM unmapped |
| 28 | madvise | partial | MADV_DONTNEED / MADV_FREE drop the pages of the range (private anonymous memory comes back zero-filled, file and shared mappings refault from their object); MADV_REMOVE punches a hole in the object behind a shared mapping (EINVAL for private, EACCES if not writable); MADV_WIPEONFORK / MADV_KEEPONFORK on private anonymous memory (a fork child gets zero pages; EINVAL on shared or file mappings, used by bionic's arc4random); NORMAL, RANDOM, SEQUENTIAL, WILLNEED, DONTFORK..DODUMP, COLD, PAGEOUT accepted as no-ops; EINVAL for an unaligned address or unknown advice, ENOMEM for an unmapped range |
| 32 | dup | yes | |
| 33 | dup2 | yes | |
| 35 | nanosleep | yes | EINTR + remaining time on signal |
| 39 | getpid | yes | thread group id |
| 40 | sendfile | yes | any readable fd to any writable fd through a kernel buffer; with an offset pointer the input position is untouched and the pointer advanced |
| 41 | socket | yes | AF_UNIX: SOCK_STREAM, SOCK_DGRAM (SOCK_RAW = DGRAM), SOCK_SEQPACKET; SOCK_NONBLOCK, SOCK_CLOEXEC; protocol 0/1 else EPROTONOSUPPORT, other types ESOCKTNOSUPPORT, unknown flags EINVAL. AF_NETLINK: SOCK_RAW/SOCK_DGRAM, any protocol below 32 (see Netlink below). Every other family (AF_INET, AF_INET6, ...) EAFNOSUPPORT |
| 42 | connect | yes | stream/seqpacket: to a listener by path or abstract name (ECONNREFUSED if nothing listens, ENOENT for a missing path, EPROTOTYPE on type mismatch, EISCONN, EINVAL on a listener); queued on the listener at once (no accept needed to send), blocks (EAGAIN with O_NONBLOCK) while the backlog is full. dgram: sets the default destination (EPERM if the target is connected elsewhere), AF_UNSPEC disconnects |
| 43 | accept | yes | = accept4(0) |
| 44 | sendto | yes | see "Sockets" below; an address on a connected stream EISCONN, unconnected EOPNOTSUPP |
| 45 | recvfrom | yes | sender address (namelen 0 for an unnamed sender) |
| 46 | sendmsg | yes | iovecs gathered; SCM_RIGHTS (up to 253 fds, EBADF for a bad one), SCM_CREDENTIALS (taken as given: everything is root); other cmsgs EINVAL |
| 47 | recvmsg | yes | SCM_CREDENTIALS (with SO_PASSCRED) then SCM_RIGHTS; MSG_CTRUNC when the control buffer is short (fds that do not fit are closed), MSG_TRUNC for a cut datagram; MSG_CMSG_CLOEXEC |
| 48 | shutdown | yes | SHUT_RD/WR/RDWR, the peer gets the mirror image (EOF / EPIPE); ENOTSOCK on other files |
| 49 | bind | yes | path (creates an S_IFSOCK inode, mode 0777 & ~umask; EADDRINUSE if the path exists), abstract name (EADDRINUSE while bound), family-only length = autobind (abstract "\0xxxxx"); EINVAL if already bound |
| 50 | listen | yes | EINVAL unbound or connected, EOPNOTSUPP on dgram; backlog clamped to 4096; again on a listener updates the backlog |
| 51 | getsockname | yes | path + NUL, abstract name, or family only |
| 52 | getpeername | yes | ENOTCONN |
| 53 | socketpair | yes | AF_UNIX, all three types; SO_PEERCRED of both ends = the caller |
| 54 | setsockopt | partial | netlink: SO_PASSCRED, SO_RCVBUF(FORCE), SO_RCVTIMEO, NETLINK_ADD/DROP_MEMBERSHIP, others accepted. AF_UNIX SOL_SOCKET: SO_PASSCRED, SO_SNDBUF/RCVBUF(+FORCE) (stored doubled, min 4608/2304), SO_RCVTIMEO/SNDTIMEO (old and new; EDOM for bad usec), common flags (REUSEADDR, KEEPALIVE, ...) accepted; others ENOPROTOOPT, other levels EOPNOTSUPP |
| 55 | getsockopt | partial | SOL_SOCKET: SO_TYPE, SO_DOMAIN, SO_PROTOCOL, SO_ERROR, SO_SNDBUF/RCVBUF (212992 default), SO_PASSCRED, SO_PEERCRED (pid 0 uid/gid 65534 without peer), SO_ACCEPTCONN, SO_RCVTIMEO/SNDTIMEO, SO_RCVLOWAT/SNDLOWAT (1); SO_PEERSEC: the peer process's SELinux context (`/proc/<pid>/attr/current`); netlink: type, domain, protocol, buffers, SO_PASSCRED |
| 56 | clone | yes | fork-style and thread-style (CLONE_VM, THREAD, SIGHAND, FILES, VFORK, PARENT, SETTLS, PARENT_SETTID, CHILD_SETTID, CHILD_CLEARTID); CLONE_PIDFD -> EINVAL; CLONE_NEWNS gives the child a copy of the mount namespace, other namespace flags ignored; the new thread inherits the caller's credentials |
| 57 | fork | yes | copy-on-write: private pages shared read-only, copied on the first write (or made writable again when no longer shared) |
| 58 | vfork | yes | shares memory, parent sleeps until the child execs or exits |
| 59 | execve | yes | path resolved through the VFS (cwd-relative, symlinks); ELF ET_EXEC and ET_DYN (PIE at 0x555555554000), static or dynamic: the PT_INTERP program (bionic's linker64) is loaded at 0x7fc000000000 and entered with AT_BASE, AT_PHDR/AT_ENTRY of the program, AT_RANDOM, AT_UID/AT_EUID/AT_GID/AT_EGID, AT_SECURE (1 when a set-user/group-ID bit changed the ids), AT_EXECFN; `#!interp [arg]` scripts (up to 5 levels, else ELOOP); ELIBBAD for a bad interpreter; ENOENT/EACCES/ENOEXEC/E2BIG; kills other threads; CLOEXEC; signal reset; records /proc/<pid>/exe and cmdline |
| 60 | exit | yes | thread exit, CLONE_CHILD_CLEARTID + futex wake |
| 61 | wait4 | yes | pid > 0, -1, 0, < -1; WNOHANG; ECHILD; rusage zeroed |
| 103 | syslog | yes | on the kernel log buffer (see [Kernel log](#kernel-log-c3b)): READ (destructive, blocking, shared with /proc/kmsg), READ_ALL and READ_CLEAR (`<N>[sec.usec] text` lines since the last CLEAR, newest that fit), CLEAR, SIZE_UNREAD, SIZE_BUFFER; CONSOLE_OFF/ON/LEVEL accepted |
| 62 | kill | yes | pid > 0, 0 (group), -1, < -1; sig 0 = existence check |
| 63 | uname | yes | Linux / 6.12.0-android-pc / x86_64 (6.12: Android 16's GKI, an LTS netbpfload accepts; also /proc/version and kernel.osrelease) |
| 72 | fcntl | yes | F_DUPFD(_CLOEXEC), F_GETFD/SETFD, F_GETFL/SETFL (O_APPEND, O_NONBLOCK, O_DIRECT, O_NOATIME, O_ASYNC), F_GETPIPE_SZ/SETPIPE_SZ (64 KiB fixed), F_ADD_SEALS/F_GET_SEALS on tmpfs/memfd files (SEAL, SHRINK, GROW, WRITE, FUTURE_WRITE, EXEC; tmpfs files and memfds without MFD_ALLOW_SEALING start with F_SEAL_SEAL; EPERM for a read-only fd or after F_SEAL_SEAL, EBUSY for F_SEAL_WRITE while a shared mapping could write; EINVAL on other files); open file description locks (F_OFD_SETLK, F_OFD_SETLKW, F_OFD_GETLK: real byte-range read/write locks per inode, released when the description is closed; EAGAIN on conflict, EBADF for a read lock on a write-only fd or the reverse, l_pid must be 0); classic record locks (F_SETLK, F_SETLKW) always granted, F_GETLK says unlocked |
| 73 | flock | stub | 0 for a valid fd (no locking) |
| 74 | fsync | stub | 0 (everything is in memory) |
| 75 | fdatasync | stub | 0 |
| 76 | truncate | yes | |
| 77 | ftruncate | yes | EINVAL on read-only fds and non-regular files; files are sparse (up to 2^63-1 bytes); shrinking unmaps the dropped pages from every mapping (SIGBUS on access) and zeroes the tail of the last page; EPERM against F_SEAL_SHRINK/GROW |
| 79 | getcwd | yes | ENOENT when the cwd was removed, ERANGE |
| 80 | chdir | yes | |
| 81 | fchdir | yes | |
| 82 | rename | yes | = renameat2(0) |
| 83 | mkdir | yes | = mkdirat(AT_FDCWD) |
| 84 | rmdir | yes | ENOTEMPTY, ENOTDIR, EBUSY on mount points |
| 85 | creat | yes | |
| 86 | link | yes | |
| 87 | unlink | yes | EISDIR; open files stay readable (nlink 0) |
| 88 | symlink | yes | |
| 89 | readlink | yes | |
| 90 | chmod | yes | |
| 91 | fchmod | yes | |
| 92 | chown | partial | stored only (no permission checks: everything runs as root) |
| 93 | fchown | partial | stored only |
| 94 | lchown | partial | stored only |
| 95 | umask | yes | per process, inherited by fork |
| 96 | gettimeofday | yes | |
| 97 | getrlimit | yes | = prlimit64(0) |
| 98 | getrusage | stub | RUSAGE_SELF/CHILDREN/THREAD; all fields 0 (no CPU time accounting yet); EINVAL for other `who` |
| 99 | sysinfo | yes | uptime, totalram/freeram (frames), procs; loads and swap 0, mem_unit 1 |
| 100 | times | partial | CPU times 0; returns clock ticks (HZ 100) since boot |
| 102 | getuid | yes | the calling thread's real uid (see [Credentials](#credentials-c3b)) |
| 104 | getgid | yes | real gid |
| 105 | setuid | yes | Linux's rules: euid 0 sets real, effective and saved ids, others only the effective one to their real or saved id (EPERM otherwise); setgid (106) the same; setreuid (113) / setregid (114) and setresuid (117) / setresgid (119) with -1 = unchanged and the unprivileged limits; setgroups (116) for euid 0 only (EPERM), at most 65536 |
| 107 | geteuid | yes | effective uid |
| 108 | getegid | yes | effective gid |
| 109 | setpgid | partial | no permission checks |
| 110 | getppid | yes | 0 for init |
| 111 | getpgrp | yes | |
| 112 | setsid | partial | |
| 115 | getgroups | yes | the supplementary groups (size 0: the count; EINVAL if too small or negative) |
| 118 | getresuid | yes | real, effective, saved; getresgid (120) the same |
| 121 | getpgid | yes | |
| 122 | setfsuid | yes | returns the old fs uid, changes it when privileged or to one of the caller's ids (set*id reset it to the effective id); setfsgid (123) the same |
| 124 | getsid | yes | |
| 125 | capget | yes | versions 1-3 (EINVAL + preferred version written back otherwise); effective = permitted = all caps 0-40, inheritable 0 (capabilities are not modelled, even for non-root ids) |
| 126 | capset | stub | header/data checked, nothing enforced |
| 127 | rt_sigpending | yes | pending & blocked |
| 128 | rt_sigtimedwait | yes | EAGAIN on timeout, EINTR on another signal |
| 129 | rt_sigqueueinfo | yes | siginfo from the caller (si_code, pid, uid, si_value); EPERM for si_code >= 0 or SI_TKILL to another process |
| 130 | rt_sigsuspend | yes | old mask restored after the handler |
| 131 | sigaltstack | yes | SS_DISABLE, EPERM while on it, ENOMEM below MINSIGSTKSZ |
| 133 | mknod | yes | regular files, device nodes, FIFOs (open: readers wait for a writer and writers for a reader, O_NONBLOCK reader opens at once, O_NONBLOCK writer without reader ENXIO, O_RDWR never blocks), sockets (open ENXIO, connect works on a bound one) |
| 135 | personality | yes | stored per process (inherited by fork); 0xffffffff queries |
| 137 | statfs | partial | erofs/ext4: magic, block size and counts, free blocks/inodes from the superblock, ST_RDONLY; others: f_type from the mount (tmpfs/ramfs/proc/sysfs magic), block counts are RAM frames |
| 138 | fstatfs | partial | as statfs; pipes report PIPEFS_MAGIC |
| 140 | getpriority | yes | PRIO_PROCESS/PGRP/USER; returns 20 - nice like the raw syscall |
| 141 | setpriority | partial | nice stored (clamped to -20..19), the scheduler ignores it |
| 142 | sched_setparam | partial | = sched_setscheduler with the current policy |
| 143 | sched_getparam | yes | stored priority |
| 144 | sched_setscheduler | partial | OTHER/BATCH/IDLE (priority 0), FIFO/RR (1-99) checked and stored per process (SCHED_RESET_ON_FORK ignored); one round-robin scheduler runs everything |
| 145 | sched_getscheduler | yes | stored policy (default SCHED_OTHER) |
| 146 | sched_get_priority_max | yes | 99 for FIFO/RR, 0 otherwise; sched_get_priority_min (147): 1 / 0 |
| 149 | mlock | stub | all memory is resident: 0; also munlock (150), mlockall (151), munlockall (152), mlock2 (325) |
| 157 | prctl | partial | PR_SET_NAME, PR_GET_NAME, PR_GET/SET_DUMPABLE, PR_GET/SET_KEEPCAPS, PR_GET/SET_NO_NEW_PRIVS (stored, not enforced), PR_CAPBSET_READ (1 for caps 0-40), PR_CAPBSET_DROP and PR_CAP_AMBIENT (accepted: no capability model), PR_GET/SET_SECUREBITS, PR_GET/SET_PDEATHSIG, PR_GET/SET_TIMERSLACK, PR_GET/SET_CHILD_SUBREAPER (stored, not acted on), PR_GET/SET_SECCOMP and THP_DISABLE (accepted, filters not applied), PR_SET_VMA (anon names accepted, not kept); others EINVAL |
| 158 | arch_prctl | yes | ARCH_SET/GET_FS/GS (EPERM above TASK_SIZE_MAX); RDFSBASE/WRFSBASE enabled and advertised in AT_HWCAP2 |
| 160 | setrlimit | yes | = prlimit64(0) |
| 161 | chroot | partial | one root for everybody: chroot to the current root succeeds (Android's switch_root does `chroot(".")` after moving a mount onto `/`), anything else EPERM |
| 162 | sync | stub | 0 |
| 165 | mount | partial | tmpfs, ramfs (`mode=` option; uid=/gid= ignored), proc (`hidepid=` ignored), sysfs, devtmpfs, binder (binderfs; options like `stats=global` ignored), selinuxfs, devpts and sysfs (single shared instances), cgroup2 and cgroup (v1, controllers from the options; see cgroups below), bpf (a bpffs for pinning, see [eBPF](#ebpf-c3c)), tracefs/debugfs/pstore/configfs/fusectl/functionfs (empty instances); erofs, ext4 (also ext2/ext3) from a block device path or "major:minor", always read-only (MS_RDONLY is forced, with a warning when not asked for), ENOTBLK for a source that is not a block device (toybox mount then sets up a loop device), EINVAL for a bad superblock or unsupported features; MS_BIND (also files; MS_REC copies the mounts below the source; the new mount joins a shared source's peer group); MS_MOVE of a mount root (also onto `/`: the moved mount becomes the namespace's root; ELOOP below itself); MS_SHARED and MS_PRIVATE (MS_SLAVE and MS_UNBINDABLE act as private), with MS_REC for the whole subtree: see [Mount namespaces](#mount-namespaces-and-propagation-c3b); MS_REMOUNT accepted as a no-op; other types ENODEV. Every change wakes pollers of /proc/mounts (POLLPRI\|POLLERR once) |
| 166 | umount2 | yes | EBUSY with mounts below; EINVAL if not a mount root; UMOUNT_NOFOLLOW; propagates to the copies in peer mounts. Lazy: files still open keep the file system (and its device's holder count) alive |
| 169 | reboot | yes | POWER_OFF/HALT (ACPI PM1a / isa-debug-exit), RESTART, RESTART2 (the reason string is logged) |
| 170 | sethostname | yes | uname's nodename (default "android-pc"; also /proc/sys/kernel/hostname); setdomainname (171) likewise |
| 175 | init_module | no | ENOSYS (no loadable modules); also finit_module (313) |
| 186 | gettid | yes | |
| 188 | setxattr | partial | stored on tmpfs-based file systems (tmpfs, rootfs, devtmpfs, sysfs, devpts, ...): any name, XATTR_CREATE (EEXIST) / XATTR_REPLACE (ENODATA), values up to 64 KiB; accepted and dropped on procfs and cgroupfs (restorecon); EROFS on erofs/ext4, EOPNOTSUPP elsewhere; also lsetxattr (189), fsetxattr (190), removexattr (197-199, ENODATA when absent) |
| 191 | getxattr | partial | stored xattrs (above), erofs and ext4 xattrs (all name indexes: user., trusted., security., system.posix_acl_*; ext4 in-inode, xattr block and EA-inode values; erofs inline and shared); size 0 returns the length, ERANGE, ENODATA; `security.selinux` with nothing stored reads `u:object_r:unlabeled:s0`; also lgetxattr (192), fgetxattr (193) |
| 194 | listxattr | partial | as getxattr; also llistxattr (195), flistxattr (196) |
| 200 | tkill | yes | |
| 201 | time | yes | |
| 202 | futex | partial | WAIT, WAKE, WAIT_BITSET (absolute, FUTEX_CLOCK_REALTIME), WAKE_BITSET, REQUEUE, CMP_REQUEUE; in a MAP_SHARED mapping (without FUTEX_PRIVATE_FLAG) keyed by (shmem object, offset), so it works across processes and mappings; otherwise by (address space, address); PI futexes ENOSYS |
| 203 | sched_setaffinity | partial | accepted when the mask names an online CPU (else EINVAL); threads still run anywhere |
| 204 | sched_getaffinity | yes | all online CPUs; returns the mask size in bytes (EINVAL when `len` is too small or not a multiple of 8) |
| 213 | epoll_create | yes | size must be > 0 |
| 247 | waitid | yes | P_ALL, P_PID, P_PGID; WEXITED (WSTOPPED/WCONTINUED never report: no job control), WNOHANG (si_pid 0 when nothing is ready), WNOWAIT (peek, the child stays a zombie); siginfo: SIGCHLD, CLD_EXITED/KILLED/DUMPED, pid, status |
| 248 | add_key | partial | see Keys below |
| 249 | request_key | partial | searches the session then the user keyring; no upcall (ENOKEY) |
| 250 | keyctl | partial | see Keys below |
| 251 | ioprio_set | partial | IOPRIO_WHO_PROCESS stored per process (not applied); PGRP/USER accepted; ioprio_get (252) reports it (default BE/4) |
| 253 | inotify_init | yes | = inotify_init1(0); see inotify below |
| 254 | inotify_add_watch | yes | see inotify below |
| 255 | inotify_rm_watch | yes | IN_IGNORED queued; EINVAL for an unknown wd |
| 217 | getdents64 | yes | ".", "..", then the entries with real d_type; position = entry index (lseek 0 rewinds); EINVAL if the first record does not fit |
| 218 | set_tid_address | yes | |
| 221 | fadvise64 | stub | hints ignored (all files in RAM); EBADF, ESPIPE on pipes and sockets, EINVAL for unknown advice |
| 228 | clock_gettime | yes | REALTIME (Limine boot date + TSC), MONOTONIC/BOOTTIME/RAW/COARSE (TSC); CPU-time clocks report uptime |
| 229 | clock_getres | yes | 1 ns, coarse clocks 10 ms |
| 230 | clock_nanosleep | yes | TIMER_ABSTIME |
| 231 | exit_group | yes | kills every thread of the group |
| 232 | epoll_wait | yes | see epoll below; EINVAL for maxevents <= 0 or a non-epoll fd; EINTR on a signal |
| 233 | epoll_ctl | yes | ADD/MOD/DEL; EEXIST, ENOENT, EINVAL (self, bad op, EPOLLEXCLUSIVE with MOD or with other than IN/OUT/ERR/HUP/ET/WAKEUP), EPERM for files without poll support (regular files, memory devices), ELOOP for loops or nesting deeper than 4 |
| 234 | tgkill | yes | si_code SI_TKILL |
| 257 | openat | yes | O_CREAT/EXCL/TRUNC/APPEND/DIRECTORY/NOFOLLOW/CLOEXEC/NONBLOCK/PATH; dirfd-relative; mode & ~umask; creating through a dangling symlink; O_TMPFILE EOPNOTSUPP |
| 258 | mkdirat | yes | |
| 259 | mknodat | yes | see mknod |
| 260 | fchownat | partial | stored only; AT_SYMLINK_NOFOLLOW, AT_EMPTY_PATH |
| 262 | newfstatat | yes | AT_EMPTY_PATH, AT_SYMLINK_NOFOLLOW |
| 263 | unlinkat | yes | AT_REMOVEDIR |
| 264 | renameat | yes | |
| 265 | linkat | yes | AT_SYMLINK_FOLLOW, AT_EMPTY_PATH; EXDEV across mounts |
| 266 | symlinkat | yes | |
| 267 | readlinkat | yes | AT_EMPTY_PATH-style empty path on an O_PATH symlink fd |
| 268 | fchmodat | yes | |
| 269 | faccessat | yes | |
| 270 | pselect6 | yes | select with a timespec and the { sigset, size } argument (temporary signal mask) |
| 271 | ppoll | yes | poll with a timespec (updated) and a temporary signal mask: kept until the handler runs on EINTR, restored on any other return |
| 272 | unshare | partial | CLONE_FILES gives a private fd table; CLONE_NEWNS a copy of the mount namespace; the other namespace flags (NEWUTS, NEWIPC, NEWNET, NEWPID, NEWUSER, NEWCGROUP, NEWTIME), CLONE_FS, SYSVSEM accepted without effect (one namespace of each other kind); others EINVAL |
| 273 | set_robust_list | partial | stored; robust futexes are not walked at exit yet |
| 274 | get_robust_list | yes | |
| 280 | utimensat | yes | UTIME_NOW/UTIME_OMIT, NULL path = dirfd |
| 281 | epoll_pwait | yes | epoll_wait + temporary signal mask (sigsetsize must be 8) |
| 282 | signalfd | yes | = signalfd4(flags 0) |
| 283 | timerfd_create | yes | CLOCK_REALTIME, MONOTONIC, BOOTTIME (= MONOTONIC: no suspend), REALTIME_ALARM, BOOTTIME_ALARM; TFD_NONBLOCK, TFD_CLOEXEC |
| 284 | eventfd | yes | = eventfd2(flags 0) |
| 285 | fallocate | partial | tmpfs/memfd: mode 0 allocates pages and grows the file, FALLOC_FL_KEEP_SIZE, FALLOC_FL_PUNCH_HOLE (with KEEP_SIZE; partial pages zeroed, whole pages freed and unmapped); other modes EOPNOTSUPP; EBADF read-only fd, EINVAL bad offset/length, ESPIPE pipes, ENODEV other files, EPERM against seals, ENOSPC near the end of RAM |
| 286 | timerfd_settime | yes | relative or TFD_TIMER_ABSTIME (realtime converted with the boot epoch), interval, old value; zero disarms; TFD_TIMER_CANCEL_ON_SET accepted (the clock is never set); resets the expiration count; EINVAL for a non-timerfd fd |
| 287 | timerfd_gettime | yes | time left and interval |
| 288 | accept4 | yes | SOCK_NONBLOCK, SOCK_CLOEXEC (the new socket does not inherit O_NONBLOCK); peer address = the client's (family only when unnamed); EINVAL if not listening, EOPNOTSUPP on dgram |
| 289 | signalfd4 | yes | SFD_NONBLOCK, SFD_CLOEXEC; fd = -1 creates, else updates the mask of an existing signalfd (EINVAL otherwise); reads 128-byte signalfd_siginfo records of the reader's pending signals in the mask (several per read, EINVAL below 128 bytes); POLLIN while one is pending |
| 290 | eventfd2 | yes | EFD_SEMAPHORE, EFD_NONBLOCK, EFD_CLOEXEC; 8-byte reads/writes (EINVAL shorter or writing 2^64-1), write blocks/EAGAIN past 2^64-2 |
| 291 | epoll_create1 | yes | EPOLL_CLOEXEC |
| 292 | dup3 | yes | O_CLOEXEC |
| 293 | pipe2 | yes | O_CLOEXEC, O_NONBLOCK, O_DIRECT accepted (not packet mode); 64 KiB buffer |
| 295 | preadv | yes | |
| 296 | pwritev | yes | |
| 297 | rt_tgsigqueueinfo | yes | thread-directed rt_sigqueueinfo |
| 299 | recvmmsg | partial | recvmsg in a loop, MSG_WAITFORONE; the timeout is ignored |
| 294 | inotify_init1 | yes | IN_NONBLOCK, IN_CLOEXEC |
| 302 | prlimit64 | yes | per-process limits, inherited by fork; Linux defaults (STACK 8 MiB, NOFILE 1024, ...); EINVAL soft > hard, EPERM for RLIMIT_NOFILE above 32768 (the largest fd table; init raises it to 32768); not enforced except NOFILE by the fd table |
| 306 | syncfs | stub | 0 |
| 307 | sendmmsg | yes | sendmsg in a loop; count sent, the error only if none was |
| 308 | setns | partial | an open /proc/<pid>/ns/mnt: join that mount namespace (cwd moves to its root; nstype 0 or CLONE_NEWNS, else EINVAL); other namespace kinds: one of each, joining is a no-op (the fd must be open) |
| 309 | getcpu | yes | current CPU, node 0 |
| 316 | renameat2 | yes | RENAME_NOREPLACE, RENAME_EXCHANGE; EXDEV across mounts, EINVAL moving a directory below itself |
| 318 | getrandom | yes | RDRAND + xorshift, not a vetted CSPRNG yet |
| 319 | memfd_create | yes | MFD_CLOEXEC, MFD_ALLOW_SEALING, MFD_NOEXEC_SEAL (mode 0666 + F_SEAL_EXEC), MFD_EXEC; MFD_HUGETLB and unknown flags EINVAL; names up to 249 bytes; shown as `/memfd:NAME (deleted)` in /proc/<pid>/fd and maps |
| 321 | bpf | partial | maps, programs (stored, never run), BTF, links, pinning: see [eBPF](#ebpf-c3c) |
| 326 | copy_file_range | yes | regular files (EINVAL otherwise, EISDIR for directories), optional offset pointers, flags must be 0 |
| 332 | statx | yes | basic stats (STATX_BASIC_STATS, no btime) |
| 436 | close_range | yes | CLOSE_RANGE_CLOEXEC, CLOSE_RANGE_UNSHARE |
| 439 | faccessat2 | yes | AT_EACCESS, AT_SYMLINK_NOFOLLOW, AT_EMPTY_PATH |
| 441 | epoll_pwait2 | yes | epoll_pwait with a timespec timeout |

## Sockets (AF_UNIX)

- Stream: no message boundaries (a read coalesces queued writes, stops
  after a chunk that carried fds and, with SO_PASSCRED, between different
  writers); writes are split as buffer room allows (SO_SNDBUF of the
  sender against the peer's queue) and return a partial count when
  interrupted or nonblocking. Dgram/seqpacket: one message per read,
  the rest of a cut message is dropped (MSG_TRUNC returns the real size);
  EMSGSIZE above SO_SNDBUF. An unconnected receiver queues at most 600
  datagrams from senders it is not connected to.
- Flags: MSG_DONTWAIT, MSG_PEEK (fds are duplicated), MSG_WAITALL
  (stream), MSG_TRUNC, MSG_NOSIGNAL, MSG_CMSG_CLOEXEC; MSG_OOB EOPNOTSUPP.
- Errors: ENOTCONN (send on an unconnected stream/seqpacket or dgram
  without destination, recv on an unconnected seqpacket), EINVAL (recv on
  an unconnected or listening stream), EPIPE + SIGPIPE (unless
  MSG_NOSIGNAL) after our SHUT_WR or the peer's SHUT_RD/close,
  ECONNREFUSED (dgram target gone; a connected dgram socket is then
  disconnected), EPERM (dgram target connected to someone else), EAGAIN
  for nonblocking calls and SO_RCVTIMEO/SO_SNDTIMEO expiry, ENOTSOCK for
  socket calls on other files.
- SCM_RIGHTS files that are never received are closed when the receiving
  socket goes away; credentials are the sender's pid with uid/gid 0.
- /proc/net/unix lists every socket (Linux columns; `@name` for abstract
  names, flag 00010000 for listeners).

## Readiness: poll, select, epoll

- What files report: pipe/FIFO read end POLLIN with data, POLLHUP once
  every writer is gone (a FIFO reader only after a writer has opened
  since it did); write end POLLOUT while not full, POLLERR without
  readers; eventfd POLLIN when > 0, POLLOUT below 2^64-2; timerfd POLLIN
  with unread expirations; signalfd POLLIN while a masked signal is
  pending for the polling thread; epoll fd POLLIN while an item on its
  ready list has events; console POLLOUT, and POLLIN while a line
  (canonical) or a byte (raw) is readable; regular
  files and memory devices are always ready (and not epoll-able).
  Sockets: POLLIN with queued data, a listener POLLIN with a pending
  connection; POLLRDHUP | POLLIN after the peer shut down writing,
  POLLHUP when both directions are shut (peer closed) or for an
  unconnected stream/seqpacket socket; POLLERR with a pending SO_ERROR
  (ECONNRESET when the peer closed with unread data); POLLOUT while the
  peer's queue has room.
- epoll: level-triggered items are reported as long as they are ready,
  EPOLLET items once per wake-up of the file, EPOLLONESHOT items once
  until re-armed with EPOLL_CTL_MOD; EPOLLERR/EPOLLHUP are always
  reported; EPOLLEXCLUSIVE and EPOLLWAKEUP are accepted (no thundering-herd
  or wakelock effect). Items are keyed by (fd, open file) and dropped once
  the file's last reference is closed (a dup keeps it alive, as on
  Linux). Nested epolls work.
- /proc/<pid>/fd shows `anon_inode:[eventfd]`, `[eventpoll]`, `[timerfd]`,
  `[signalfd]` and `socket:[ino]`; /proc/<pid>/fdinfo/N has pos/flags/mnt_id/ino plus the
  epoll items (`tfd: ... events: ... data: ...`), eventfd count, timerfd
  clock/ticks/it_value/it_interval and signalfd mask.

## Binder (C1)

`drivers/binder/` implements Linux binder's user ABI (protocol version 8,
64-bit; `include/uapi/linux/android/binder.h`, `binderfs.h`). Design notes
are in [kernel-architecture.md](kernel-architecture.md#binder-driversbinder).

- **Devices**: `binder`, `hwbinder`, `vndbinder` (major 244, minors 0-2),
  each a separate context with its own context manager. They exist only
  inside binderfs: `mount -t binder binder /dev/binderfs` (Android's init.rc
  does this and symlinks `/dev/binder` etc. into it). binderfs also has
  `binder-control` (244:1023; BINDER_CTL_ADD creates a device in that mount,
  a new context, and returns its major/minor; EEXIST for a taken name,
  EINVAL for a bad one) and an empty `binder_logs/`. Every mount shows the
  three default devices (one IPC namespace).
- **open**: a new binder process per open. **close** (last reference, so
  also after munmap of the buffer and process exit) releases it: see
  semantics below.
- **mmap**: PROT_READ only (EPERM with PROT_WRITE, and mprotect cannot add
  it: EACCES), offset 0, once per open (EBUSY); at most 4 MiB of the
  mapping is used as buffer space. MAP_PRIVATE and MAP_SHARED behave the
  same (the pages are the kernel's).
- **poll/epoll**: POLLIN when the polling thread has work (its own, or the
  process's when it is an idle looper); first poll by a new thread is
  readable (as on Linux).
- **ioctls**: BINDER_WRITE_READ; BINDER_SET_MAX_THREADS;
  BINDER_SET_CONTEXT_MGR and _EXT (flags: FLAT_BINDER_FLAG_TXN_SECURITY_CTX;
  EBUSY if the context has one); BINDER_THREAD_EXIT; BINDER_VERSION (8);
  BINDER_GET_NODE_DEBUG_INFO; BINDER_GET_NODE_INFO_FOR_REF (context manager
  only, EPERM otherwise; strong = local + internal strong refs, weak = local
  weak refs); BINDER_GET_EXTENDED_ERROR (id, command, -errno of the
  thread's last transaction, reset by reading); BINDER_ENABLE_ONEWAY_SPAM_DETECTION
  (stored, never reports); BINDER_FREEZE, BINDER_SET_IDLE_TIMEOUT,
  BINDER_SET_IDLE_PRIORITY (no-ops); BINDER_GET_FROZEN_INFO (nothing is
  frozen). Others EINVAL.
- **Commands** (write buffer): BC_TRANSACTION(_SG), BC_REPLY(_SG),
  BC_FREE_BUFFER, BC_INCREFS/ACQUIRE/RELEASE/DECREFS (INCREFS/ACQUIRE on
  handle 0 create the ref to the context manager), BC_INCREFS_DONE /
  BC_ACQUIRE_DONE, BC_REGISTER_LOOPER, BC_ENTER_LOOPER, BC_EXIT_LOOPER,
  BC_REQUEST_DEATH_NOTIFICATION, BC_CLEAR_DEATH_NOTIFICATION,
  BC_DEAD_BINDER_DONE. BC_ATTEMPT_ACQUIRE, BC_ACQUIRE_RESULT, the freeze
  notification commands and unknown commands fail the ioctl with EINVAL
  (write_consumed stops before them). Bad handles/pointers in reference
  commands are logged and ignored, as on Linux.
- **Returns** (read buffer): BR_NOOP first, replaced by BR_SPAWN_LOOPER when
  the pool should grow; BR_TRANSACTION, BR_TRANSACTION_SEC_CTX (context
  string `u:r:kernel:s0` until SELinux exists), BR_REPLY,
  BR_TRANSACTION_COMPLETE, BR_DEAD_REPLY, BR_FAILED_REPLY,
  BR_INCREFS/ACQUIRE/RELEASE/DECREFS, BR_DEAD_BINDER,
  BR_CLEAR_DEATH_NOTIFICATION_DONE. A read stops after a transaction or a
  BR_DEAD_BINDER; it needs 76 free bytes per item; O_NONBLOCK gives EAGAIN
  instead of waiting; a signal gives EINTR.
- **Transactions**: data, offsets (multiple of 8, objects 4-aligned, in
  order, not overlapping) and scatter-gather buffers are copied into a
  buffer allocated in the receiver's mapping (best fit; oneway calls may use
  half of it; ENOSPC otherwise). Objects: BINDER/WEAK_BINDER become
  HANDLE/WEAK_HANDLE refs of the receiver (a node is created on first
  send, with ACCEPTS_FDS and TXN_SECURITY_CTX from its flags), handles
  become refs of the receiver or BINDER again for the node's owner; FD and
  FDA files are installed (O_CLOEXEC) in the receiver when it reads the
  transaction (EPERM unless the target node accepts fds, or the call had
  TF_ACCEPT_FDS for a reply); PTR buffers are copied and their address
  patched, with parent fixups; fds of fd arrays are closed by BC_FREE_BUFFER.
  sender_pid is the caller's pid for synchronous calls, 0 for oneway calls
  and replies; sender_euid is 0 (everything is root).
- **Failures** go to user space as returns, not ioctl errors: no context
  manager, a dead target node or process, or a dead caller (for a reply)
  give BR_DEAD_REPLY; a bad handle, bad object layout, unknown fd, missing
  mmap, no buffer space or calling oneself give BR_FAILED_REPLY. A failed
  reply gives the replier BR_TRANSACTION_COMPLETE and its caller the error.
  After an error the rest of the write buffer is not processed until the
  error has been read (as on Linux).

## Block devices (C2)

Code: `kernel/kernel/src/block/` (block layer, cache, GPT, loop, dm),
`drivers/pci.rs`, `drivers/virtio/` (virtio-blk), `fs/erofs.rs`, `fs/lz4.rs`,
`fs/ext4.rs`. Architecture in [kernel-architecture.md](kernel-architecture.md#block-devices-and-disk-file-systems-c2).

- **Devices**: virtio-blk disks vda, vdb, ... (254:16n, modern virtio-pci,
  or legacy I/O ports for transitional/legacy-only devices; read-only
  devices from VIRTIO_BLK_F_RO), GPT partitions vdaN (254:16n+N, past 15
  on 259:x), loop0..7 (7:N, more on demand), dm-N (253:N). Nodes in
  devtmpfs under `/dev/block/`, plus `/dev/block/by-name/<GPT name>`
  symlinks, `/dev/loop-control` (10:237), `/dev/device-mapper` and
  `/dev/mapper/control` (10:236). sysfs: `/sys/block/<disk>[/<part>]/`
  with dev, size, ro, removable, range, uevent (MAJOR, MINOR, DEVNAME,
  DEVTYPE, PARTN, PARTNAME), partition, start, queue/*; loop/ and dm/
  subdirectories; `/sys/class/block/<name>` and `/sys/dev/block/M:m`
  symlinks. `/proc/partitions`.
- **read/write/lseek** on a block device: byte granularity through the
  device's page cache (write-through), EOF at the end, ENOSPC writing past
  it, EPERM writing a read-only device, SEEK_END = size.
- **Block ioctls**: BLKGETSIZE64, BLKGETSIZE, BLKSSZGET, BLKPBSZGET,
  BLKIOMIN (512), BLKBSZGET (4096), BLKBSZSET (checked only), BLKIOOPT /
  BLKALIGNOFF / BLKDISCARDZEROES / BLKROTATIONAL (0), BLKROGET, BLKROSET
  (EACCES clearing it on read-only hardware), BLKFLSBUF (flush + drop the
  cache), BLKRAGET/BLKRASET, BLKDISCARD / BLKSECDISCARD (range checked,
  no-op), BLKZEROOUT (writes zeros), BLKRRPART (rescan; EBUSY while a
  partition is open or mounted), BLKGETDISKSEQ.
- **Loop** (`/dev/block/loopN`): LOOP_SET_FD, LOOP_CONFIGURE, LOOP_CLR_FD
  (deferred to the last close/umount with LO_FLAGS_AUTOCLEAR while in use),
  LOOP_GET_STATUS64 / LOOP_SET_STATUS64 (offset, size limit, file name;
  READ_ONLY from a read-only fd or LOOP_CONFIGURE, AUTOCLEAR, PARTSCAN),
  LOOP_SET_CAPACITY, LOOP_SET_BLOCK_SIZE, LOOP_SET_DIRECT_IO (no-op),
  LOOP_CHANGE_FD; ENXIO when unbound, EBUSY binding twice. The 32-bit
  LOOP_GET/SET_STATUS are EINVAL. loop-control: LOOP_CTL_GET_FREE (creates
  a device when all are bound), LOOP_CTL_ADD (EEXIST), LOOP_CTL_REMOVE
  (EBUSY while bound or open).
- **Device mapper** (ioctl version 4.48.0): VERSION, REMOVE_ALL,
  LIST_DEVICES (name list with event_nr and uuid), DEV_CREATE (EBUSY for a
  taken name or uuid; DM_PERSISTENT_DEV_FLAG), DEV_REMOVE (EBUSY while
  open or mounted), DEV_RENAME (name, or uuid with DM_UUID_FLAG),
  DEV_SUSPEND (suspend flag only; resume swaps in the inactive table),
  DEV_STATUS / DEV_WAIT / DEV_ARM_POLL / DEV_SET_GEOMETRY (status flags:
  SUSPEND, READONLY, ACTIVE_PRESENT, INACTIVE_PRESENT; QUERY_INACTIVE_TABLE),
  TABLE_LOAD (targets must be contiguous from sector 0; READONLY flag),
  TABLE_CLEAR, TABLE_DEPS, TABLE_STATUS (status lines, or the table with
  DM_STATUS_TABLE_FLAG; linear devices shown as major:minor),
  LIST_VERSIONS, GET_TARGET_VERSION; DM_BUFFER_FULL_FLAG when the output
  does not fit, DM_DATA_OUT_FLAG when data is returned; TARGET_MSG EINVAL.
  Targets: linear, zero, error, and verity accepted as a pass-through to
  its data device (no hash verification; dm-verity is "disabled"). I/O is
  not held back while suspended. AOSP's dmctl (libdm, and liblp's
  `create-from-super` reading the super partition metadata) works.

## Netlink (C3a)

- **NETLINK_KOBJECT_UEVENT**: sockets bound to group 1 (any mask with bit
  0) receive every uevent the kernel sends: on device registration and
  removal ("add"/"remove": block devices, partitions, loop and dm
  devices) and when an action ("add", "change", "remove", "move",
  "online", "offline", "bind", "unbind"; EINVAL otherwise) is written to
  a device's sysfs `uevent` file. Payload: `ACTION@DEVPATH`, then
  `ACTION=`, `DEVPATH=`, `SUBSYSTEM=`, the `uevent` file's lines
  (MAJOR, MINOR, DEVNAME, DEVTYPE, PARTN, PARTNAME, ...) and `SEQNUM=`,
  NUL-separated. The devices: block (virtio disks and partitions, loop,
  dm, RAM disks), mem, tty, misc (device-mapper, loop-control,
  hw_random). The sender address is port 0, group 1; with SO_PASSCRED
  an SCM_CREDENTIALS message (pid 0, uid 0, gid 0) is attached, as
  libcutils' `uevent_kernel_recv` requires. Messages beyond the receive
  buffer (SO_RCVBUF(FORCE)) are dropped and the next read gives ENOBUFS.
- bind with port 0 picks the caller's pid (or a unique number); explicit
  ports must be free (EADDRINUSE). getsockname reports (port, groups).
  NETLINK_ADD/DROP_MEMBERSHIP change the groups. Messages to another
  socket's port are delivered (ECONNREFUSED if none), to groups to every
  member.
- Every other protocol (ROUTE, SELINUX, AUDIT, ...) is a stub: requests
  to the kernel get NLMSG_DONE for dumps, an NLMSG_ERROR ack (0 with
  NLM_F_ACK, else -EOPNOTSUPP) otherwise; nothing is ever multicast.
  Protocols >= 32 EPROTONOSUPPORT, stream sockets ESOCKTNOSUPPORT.

## Kernel log (C3b)

`fs/kmsg.rs`: a ring of 1024 records (sequence number, microseconds
since boot, `facility << 3 | level`, up to 400 bytes of text), filled by
every kernel log line (facility 0, levels error 3 / warn 4 / info 6 /
debug 7) and every write to /dev/kmsg (`<N>` prefix parsed; LOG_USER
level 4 without one; facility 0 is not allowed from user space). Lines
written to /dev/kmsg are also printed on the serial console as before
(`kmsg: ...`).

- **/dev/kmsg** (1:11) reads one record per call as `prio,seq,usec,-;text\n`
  (non-printable bytes as `\xNN`); the file position is the next sequence
  number: SEEK_SET 0 = the oldest kept record, SEEK_END = after the newest;
  EINVAL when the buffer is too small for the record, EAGAIN (O_NONBLOCK)
  or blocking when caught up; poll: POLLIN when a newer record exists
  (POLLOUT always). Lost records are skipped silently (no EPIPE).
- **/proc/kmsg** (0400; init chmods it 0440): the syslog READ cursor,
  `<N>[    5.140900] text\n` lines, destructive, blocking; poll/epoll
  POLLIN while unread records exist. logd's klogd reads it (`logcat -b
  kernel`).
- Readers are woken from the timer tick (the logger may run in any
  context) or directly by /dev/kmsg writes.

## Credentials (C3b)

Every thread has real, effective, saved and fs user and group ids and
supplementary groups (`process::Creds` on the task: bionic's set*id
change only the calling thread, and ueventd's threads each switch egid
around a mknod). Inherited by clone and fork; execve of a set-user-ID
(set-group-ID with group exec) file sets the effective id, saved ids
follow the effective ones, and AT_UID/AT_EUID/AT_GID/AT_EGID/AT_SECURE
come from the result. They are reported by the get*id calls,
/proc/<pid>/status (Uid/Gid/Groups) and the owner of /proc/<pid>/* (ps's
USER column), SO_PEERCRED and SCM_CREDENTIALS (effective ids), binder's
`sender_euid`, the si_uid of kill/tgkill; new files, directories, nodes,
symlinks and sockets get the creator's fs uid and fs gid, or a
set-group-ID directory's group (subdirectories inherit the bit). They are
**not** used for permission checks, and capabilities are not modelled
(capget reports everything), so a daemon that dropped to its uid can
still do what root can.

## Mount namespaces and propagation (C3b)

- A namespace (`vfs::MntNs`) is its copy of the initial root mount; its
  mounts are those whose chain of mount points leads there, so lookups,
  `/` and /proc/mounts stay inside it. unshare(CLONE_NEWNS) and
  clone(CLONE_NEWNS) copy every mount of the caller's namespace (same
  roots and dentries; the caller's cwd moves to the copy); setns joins
  the one an open /proc/<pid>/ns/mnt names (`mnt:[4026531841]` for the
  initial one, 4026532000 and up for copies). Processes keep their
  namespace across fork and exec.
- Shared subtrees: MS_SHARED puts a mount in a peer group (`shared:N` in
  mountinfo), MS_PRIVATE takes it out; a bind of a shared mount and the
  copies of a namespace are peers of the original. A mount below one
  member of a group is copied to the same dentry below every other member
  that can see it (the copies form a new group), and unmounting removes
  the copies too. This is what makes init's /bootstrap-apex (bound on
  /apex in the bootstrap namespace) show the APEXes apexd-bootstrap
  mounts in the default namespace, where servicemanager reads their VINTF
  fragments.
- Not modelled: slave mounts (MS_SLAVE is private), unbindable mounts,
  propagation of MS_MOVE, mount namespace ownership by user namespaces.

## SELinux stub (C3a)

No policy engine: a policy is accepted and every check allows. What
libselinux and init see:

- **selinuxfs** (`mount -t selinuxfs`, one shared instance, statfs
  SELINUX_MAGIC): `enforce` (reads 0/1; writes stored and reported, the
  system stays permissive in effect), `load` (any policy; written in
  chunks, the write at offset 0 starts a load and bumps the load
  counter), `status` (mmap-able `selinux_kernel_status`: version 1, even
  sequence bumped on every change, enforcing, policyload, deny_unknown 0),
  transaction files that answer on the same fd: `context` (validity:
  "user:role:type[:level]", EINVAL otherwise), `access` (`ffffffff
  ffffffff 0 ffffffff <seqno> 0`), `create`/`relabel`/`member` (the
  target context), `user` (no contexts); `deny_unknown` and
  `reject_unknown` 0, `checkreqprot` 0, `mls` 1, `policyvers` 33, `null`
  (1:3), `initial_contexts/*`, empty `class/`, `booleans/`,
  `policy_capabilities/`, `avc/`.
- **Process attributes**: /proc/<pid>/attr/{current,prev,exec,fscreate,
  keycreate,sockcreate} (also /proc/<pid>/task/<tid>/attr and
  /proc/thread-self/attr; per process, writable only by the process
  itself, NUL-terminated on read, EINVAL for a malformed context, an
  empty write clears). Processes start as `u:r:kernel:s0`; fork copies
  all of them; execve sets `current` to `exec` when set, else, for the
  kernel and init domains, to the domain named by the program's
  `security.selinux` label (`u:object_r:foo_exec:s0` -> `u:r:foo:s0`),
  then clears exec/fscreate/keycreate/sockcreate. Files created while
  `fscreate` is set get it as their label (sockets: `sockcreate` first).
- `SO_PEERSEC` reports the peer's current context; KEYCTL_GET_SECURITY
  the caller's.

## cgroups (C3a)

- `mount -t cgroup2` gives the one v2 hierarchy; `mount -t cgroup -o
  <controllers>[,noprefix]` a v1 hierarchy for those controllers (cpuset,
  cpu, cpuacct, memory, blkio, freezer, devices, net_cls, pids,
  schedtune; at least one, EINVAL otherwise). The same controller set
  again is the same hierarchy; a controller already in another one gives
  EBUSY. /proc/mounts lists the controllers (and `noprefix`) as options,
  as libprocessgroup expects.
- Groups: mkdir creates one with the hierarchy's control files (v2:
  cgroup.procs/threads/controllers/subtree_control/events/type/freeze/
  kill/..., memory.*, cpu.*, cpuset.*, io.weight, pids.*, *.pressure; v1:
  cgroup.procs, tasks, notify_on_release, the controllers' files, cpuset
  files without the `cpuset.` prefix under `noprefix`). Values start as
  the parent's. Creating plain files is EPERM; rmdir is EBUSY while the
  group or a child group has live members, or has children.
- Membership is per process and hierarchy, inherited by fork.
  `cgroup.procs` and `tasks` list live members (pids / thread ids);
  writing a pid (0 = the caller) or a tid moves that whole process.
  `cgroup.events` reports `populated` (this group or below);
  `cgroup.kill` ("1") sends SIGKILL to every member at or below.
  /proc/<pid>/cgroup and /proc/cgroups are generated from it.
- Everything else (limits, cpus, uclamp, freeze, PSI triggers) is stored
  and read back but not enforced.

## inotify (C3a)

inotify_init1 (IN_NONBLOCK, IN_CLOEXEC), inotify_add_watch on any path
(IN_DONT_FOLLOW, IN_ONLYDIR -> ENOTDIR, IN_MASK_ADD, IN_MASK_CREATE ->
EEXIST, a zero event mask EINVAL; the same inode gives the same wd),
inotify_rm_watch (IN_IGNORED). Events: IN_CREATE (create, mkdir, mknod,
symlink, link), IN_DELETE (+ IN_DELETE_SELF and IN_IGNORED for a watched
inode whose last link or directory went), IN_MOVED_FROM/IN_MOVED_TO
(shared cookie) and IN_MOVE_SELF, IN_MODIFY (each successful write),
IN_CLOSE_WRITE / IN_CLOSE_NOWRITE (last close of a file opened by path),
IN_ATTRIB (chmod, chown, utimes, link count, xattrs). Events about a
child also go to its directory's watches with its name; IN_ISDIR for
directories; IN_ONESHOT; identical consecutive events coalesce; 16384
queued events, then IN_Q_OVERFLOW. read returns whole events (EINVAL if
the first does not fit, EAGAIN/blocking when empty), poll/epoll and
FIONREAD work. Not generated: IN_ACCESS, IN_OPEN, IN_UNMOUNT.

## Keys (C3a)

One system-wide table. add_key ("user", "logon", "keyring", "big_key",
"fscrypt-provisioning", "asymmetric"; ENODEV otherwise, EPERM for "."
types): the same type and description in the target keyring is updated
in place (same serial). keyctl: GET_KEYRING_ID (special keyrings thread,
process, session, user, user-session, group: one each for the system,
created on demand), JOIN_SESSION_KEYRING (named or the session one),
UPDATE, REVOKE / INVALIDATE (then EKEYREVOKED), CHOWN, SETPERM (stored),
DESCRIBE ("type;uid;gid;perm;description"), CLEAR, LINK, UNLINK (ENOENT
if not linked), SEARCH (depth-first through nested keyrings, ENOKEY),
READ (keyrings: serials; logon keys: EOPNOTSUPP), SET_TIMEOUT,
SET_REQKEY_KEYRING, GET_SECURITY, GET_PERSISTENT, SESSION_TO_PARENT,
RESTRICT_KEYRING; others EOPNOTSUPP. No permission checks, quotas or
expiry.

## Pseudo-terminals (C3a)

/dev/ptmx (5:2, also in devtmpfs and devpts) opens a new master and
creates /dev/pts/N (136:N, 0620) in the shared devpts instance; the
slave is locked until TIOCSPTLCK(0) (EIO before). TIOCGPTN, TIOCGPTLCK,
TIOCGPTPEER (opens the slave without a path). No line discipline: bytes
written to the master reach slave reads unchanged (no echo, canonical
mode or signals); slave output gets "\n" -> "\r\n" while OPOST|ONLCR
are set (the default termios). 64 KiB per direction (writers block).
Master closed: slave reads EOF, writes EIO, POLLHUP. Every slave fd
closed (after one was opened): master reads EIO, POLLHUP.

## /proc and /sys for init (C3a)

- /proc/sys: a fixed table of sysctls (kernel.*, vm.*, fs.*, net.core.*,
  ...) whose values are stored and read back (a re-read from offset 0 of
  an open file sees later writes); kernel/hostname and domainname are
  uname's; kernel/random/{boot_id,uuid} generated. Unknown names ENOENT
  (EACCES for O_CREAT).
- /proc/bootconfig: the `androidboot.*` command-line parameters as
  `key = "value"` lines. /proc/cgroups, devices, swaps, vmstat,
  interrupts, modules (empty), sysrq-trigger (logged), kmsg (see
  [Kernel log](#kernel-log-c3b)), pressure/{cpu,io,memory} (PSI triggers
  accepted and epoll-able, they never fire: lmkd runs on them).
- /proc/<pid>/: mountinfo, cgroup, environ (empty), oom_score_adj (and
  oom_adj, oom_score; stored), timerslack_ns, statm, schedstat, wchan,
  attr/, task/<tid>/ (attr/, comm, stat, status, cgroup), ns/ (cgroup,
  ipc, mnt, net, pid, time, user, uts as `mnt:[4026531841]`-style links;
  mnt names the process's own mount namespace; opening one gives a
  namespace file for setns). /proc/<pid> and its files belong to the
  process's effective uid and gid. fd/N, exe, cwd and ns/*
  are magic links: opening them reaches the file itself (a memfd can be
  re-opened through /proc/self/fd/N). A tid names its thread group's
  directory. /proc/mounts and mountinfo show mounts reachable from the
  root, with cgroup controllers as options, are regenerated when read
  from offset 0, and poll POLLPRI|POLLERR once after each mount change.
- /sys: devices under /sys/devices with `uevent` files and `subsystem`
  links, symlinks from /sys/class, /sys/block, /sys/dev, /sys/bus (see
  [android-init.md](android-init.md)); kernel/uevent_seqnum, fs/selinux,
  fs/cgroup and the other mount points.

## eBPF (C3c)

bpf(2) takes `union bpf_attr` up to its `size` (zero-extended). Commands:

- MAP_CREATE: HASH, ARRAY, PROG_ARRAY, PERF_EVENT_ARRAY, PERCPU_HASH,
  PERCPU_ARRAY, STACK_TRACE, CGROUP_ARRAY, LRU_HASH, LRU_PERCPU_HASH,
  LPM_TRIE (needs BPF_F_NO_PREALLOC), ARRAY_OF_MAPS / HASH_OF_MAPS (inner
  map fd checked), DEVMAP / DEVMAP_HASH (BPF_F_RDONLY_PROG added, as
  Linux does), SOCKMAP, CPUMAP, XSKMAP, SOCKHASH, CGROUP_STORAGE,
  SK/INODE/TASK/CGRP_STORAGE (max_entries 0, NO_PREALLOC), RINGBUF
  (key and value 0, max_entries a power-of-2 number of pages); other
  types and unknown flags EINVAL. Array keys are 4 bytes. Names: up to
  15 of [A-Za-z0-9_.]. btf_fd with key/value type ids must be a BTF fd.
- MAP_LOOKUP_ELEM, UPDATE_ELEM (BPF_ANY / NOEXIST -> EEXIST / EXIST ->
  ENOENT, BPF_F_LOCK accepted), DELETE_ELEM (EINVAL for arrays),
  LOOKUP_AND_DELETE_ELEM, GET_NEXT_KEY (NULL or a missing key -> first
  key; ENOENT after the last), MAP_FREEZE (then updates EPERM). Arrays:
  ENOENT / E2BIG past max_entries; a full hash E2BIG, a full LRU map
  evicts its least recently used key. Per-CPU values are
  round_up(value_size, 8) times the number of possible CPUs. Lookups need
  a readable fd, changes a writable one (EPERM). mmap: ARRAY with
  BPF_F_MMAPABLE (shared with lookup/update; no PROT_WRITE MAP_SHARED
  after freeze) and RINGBUF; other maps ENODEV.
- PROG_LOAD: types 1..32, 1..2^20 instructions, license (GPL-compatible
  ones set the gpl bit), name, prog_btf_fd; maps referenced by ld_imm64
  BPF_PSEUDO_MAP_FD / MAP_VALUE are held by the program (EBADF for a bad
  fd). No verification: every program loads, an empty log is returned.
- OBJ_PIN / OBJ_GET (file_flags BPF_F_RDONLY / WRONLY / BPF_F_PATH_FD):
  pinning only inside a bpffs (EPERM elsewhere).
- PROG_ATTACH / PROG_DETACH (flags ALLOW_OVERRIDE, ALLOW_MULTI, REPLACE)
  and PROG_QUERY (prog_cnt and ids; ENOSPC when the array is short) on
  any fd (cgroup directories, sockets); LINK_CREATE, LINK_DETACH,
  RAW_TRACEPOINT_OPEN: link fds. PROG_BIND_MAP. Nothing is ever run.
- PROG/MAP/BTF/LINK_GET_NEXT_ID and _GET_FD_BY_ID (MAP: open_flags
  RDONLY/WRONLY), ENOENT past the last id or for a freed object.
- OBJ_GET_INFO_BY_FD: bpf_map_info, bpf_prog_info (type, id, tag, the
  instructions as jited and xlated image with their lengths, load time,
  uid, map ids, name, gpl, btf id, verified_insns), bpf_btf_info (blob,
  size, id, empty name), bpf_link_info (type, id, prog id). A caller's
  struct longer than the kernel's must be zero past it (E2BIG); info_len
  is set to what was written.
- BTF_LOAD: magic 0xeb9f, version 1, header and section bounds checked;
  BTF_GET_FD_BY_ID, BTF_GET_NEXT_ID. ENABLE_STATS gives an fd (no stats).
- PROG_TEST_RUN EOPNOTSUPP; batch operations, iterators, tokens, struct_ops
  EINVAL.

Files show as `anon_inode:bpf-map` / `bpf-prog` / `btf` / `bpf_link` with
Linux-style fdinfo (map_type, key_size, ..., map_id; prog_type, prog_tag,
prog_id). bpffs nodes are regular files (mode 0600 & ~umask) handled like
tmpfs files by chmod, chown, rename and unlink; open(2) of one is EIO.
/proc/sys/net/core/bpf_jit_enable (1), bpf_jit_harden, bpf_jit_kallsyms,
bpf_jit_limit and kernel/unprivileged_bpf_disabled (2) are stored sysctls.

## Signals: what is delivered how

- Delivery happens on every return to user mode (syscall exit, interrupt,
  exception), through a Linux-compatible `rt_sigframe` (ucontext with
  sigcontext, 512-byte FXSAVE area, siginfo). `sa_restorer` is mandatory, as
  on Linux x86_64.
- Faults: #PF outside a VMA or against its protection -> SIGSEGV (si_addr =
  CR2, SEGV_MAPERR/ACCERR), #GP -> SIGSEGV, #UD -> SIGILL, #DE -> SIGFPE,
  #XM/#MF -> SIGFPE, `int3` -> SIGTRAP, #AC -> SIGBUS, #PF past the end of
  a file or shared mapping's object -> SIGBUS (BUS_ADRERR, si_addr = CR2). A blocked or ignored
  fault signal is forced to the default action.
- Default actions terminate (no core files), ignore, or, for stop/continue
  signals, do nothing (no job control yet).
- Interrupted syscalls return EINTR, or are restarted with SA_RESTART
  (wait4, blocking futex wait, pipe/FIFO read/write and open, eventfd,
  timerfd and signalfd reads, socket send/recv/connect/accept without
  SO_RCVTIMEO/SO_SNDTIMEO; with a timeout set they return EINTR, as on
  Linux). poll, select, epoll_wait and their variants
  always return EINTR, as on Linux.
- Real-time signals do not queue (one pending instance each), like the
  standard ones.

## Not implemented yet (selection, by phase)

- **B2 leftovers**: legacy getdents (78), pivot_root (EINVAL), chroot
  to anything but the root, permission checks (ids are tracked but
  never checked), read-only mounts (recorded only, except on erofs/ext4
  which refuse writes), slave and unbindable mounts.
- **C2 leftovers (block devices)**: ext4 writes (for /data), f2fs, the
  ext4 journal replay, erofs big pclusters / ztailpacking / fragments /
  other compressors, MBR partitions, dm-verity hash checking and the
  other dm targets AOSP uses on /data (default-key, bow), uevents for new
  O_DIRECT semantics, interrupt-driven
  virtio (requests are polled), blocking a dm device's I/O while
  suspended, umount EBUSY while files are open (umount is lazy).
- **B3 leftovers (memory)**: MAP_GROWSDOWN, mmap of files on file systems
  without a page cache (procfs, devices other than /dev/zero),
  process_vm_readv/writev.
- **B4 leftovers**: pidfd, TFD_IOC_SET_TICKS, timerfd clock-set
  cancellation, SIGIO/O_ASYNC. Sockets: AF_INET/AF_INET6 (no network
  stack), netlink protocols other than uevents, MSG_OOB, SO_PEEK_OFF,
  SO_PEERGROUPS, SCM_SECURITY, garbage collection of in-flight fd cycles
  (a socket sent over itself and never received leaks),
  /proc/sys/net/unix/max_dgram_qlen (fixed at 600).
- **B5 leftovers**: permission checks and capabilities (credentials are
  tracked since C3b, not enforced), CPU time accounting (getrusage/times
  report 0), scheduling classes/affinity/nice (stored only), SIGTTIN/SIGTTOU
  and stop/continue job control, PR_SET_PDEATHSIG (stored, never sent),
  ELF segments are copied (not mapped from the file's page cache),
  membarrier, ptrace.
- **C1 leftovers (binder)**: the freezer (BINDER_FREEZE is a no-op, no
  BR_FROZEN_REPLY / freeze notifications), oneway spam detection (never
  reports BR_ONEWAY_SPAM_SUSPECT), priority inheritance and
  FLAT_BINDER_FLAG_INHERIT_RT, real SELinux security contexts, the
  binder_logs/ files, per-mount binderfs device sets, TF_CLEAR_BUF and
  TF_UPDATE_TXN, BINDER_FLUSH on close of one of several fds.
- **C3a leftovers**: a SELinux policy engine (checks always allow),
  cgroup resource control, loadable modules (init_module/finit_module
  ENOSYS), a pty line discipline, key permissions and quotas, seccomp
  filters (accepted, not applied), a persistent /data (see
  [android-init.md](android-init.md)).
- **C3b leftovers**: NETLINK_AUDIT (logd's auditd
  gets the stub), lmkd's process_mrelease (448), pidfd_open (434),
  timer_create (222), settimeofday (164), /dev/kmsg EPIPE on lost
  records, the mq/ and slaves/ entries of dm devices.
- **C3c leftovers (eBPF)**: running programs (no verifier, JIT or
  interpreter: socket filters, cgroup hooks, tracepoints and uprobes
  never fire, ring buffers stay empty), kernel BTF
  (/sys/kernel/btf/vmlinux: CO-RE objects such as timeInState.bpf fail),
  fuse-bpf (/sys/fs/fuse/bpf_prog_type_fuse), map batch operations,
  bpf iterators and tokens, permission checks (CAP_BPF,
  unprivileged_bpf_disabled are not enforced). userfaultfd (323) is
  missing (ART's GC).
- Later: rseq, clone3, io_uring (not used by AOSP).
