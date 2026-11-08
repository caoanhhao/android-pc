# Syscall table (Rust kernel)

Linux x86_64 syscalls the kernel in `kernel/` implements, by phase. The
dispatcher is `kernel/kernel/src/syscall/mod.rs`; every number not listed
returns `-ENOSYS` and is logged once (`unimplemented syscall N`). Boot with
`--append strace` to log every call. What AOSP needs overall is in
[syscall-trace.md](syscall-trace.md).

Status: **yes** = implemented with Linux semantics for what the B1 tests and
static programs use; **partial** = works for the common case, gaps listed;
**stub** = accepted, does nothing useful yet.

## Implemented (phases B1-B4a)

| Nr | Name | Status | Notes |
|---:|------|--------|-------|
| 0 | read | yes | VFS files, devices, pipes/FIFOs, /proc, eventfd/timerfd/signalfd; regular files read in 64 KiB chunks until done |
| 1 | write | yes | VFS files (O_APPEND atomic), devices, pipes (EPIPE + SIGPIPE, atomic up to PIPE_BUF) |
| 2 | open | yes | = openat(AT_FDCWD) |
| 3 | close | yes | |
| 4 | stat | yes | = newfstatat(AT_FDCWD, 0) |
| 5 | fstat | yes | any fd (pipes S_IFIFO, devices S_IFCHR with rdev); uid/gid/times stored |
| 6 | lstat | yes | = newfstatat(AT_SYMLINK_NOFOLLOW) |
| 7 | poll | yes | POLLNVAL for bad fds, negative fds ignored, ERR/HUP always reported; timeout in ms (< 0 = forever); EINTR on a signal; EINVAL above 1024 fds |
| 8 | lseek | yes | SEEK_SET/CUR/END; directories (getdents index); ESPIPE on pipes; devices accept and stay at 0 |
| 9 | mmap | partial | anonymous memory lazily zero-filled; MAP_FIXED, MAP_FIXED_NOREPLACE, hint; MAP_SHARED anonymous gets its own shmem object (shared across fork; growing it with mremap gives SIGBUS past the original size). File mappings of tmpfs/memfd files map the page cache: MAP_SHARED writes are seen by read() and every other mapping, MAP_PRIVATE is COW from the cache; pages past EOF raise SIGBUS (the tail of the last page reads zero). EACCES for a write-only fd, or PROT_WRITE + MAP_SHARED on a fd not open for writing (or O_APPEND); EPERM for PROT_WRITE + MAP_SHARED on a write-sealed memfd. /dev/zero: anonymous memory (shared or private). Other files ENODEV; EBADF for a bad or O_PATH fd |
| 10 | mprotect | yes | splits VMAs, updates PTEs (COW-shared pages stay read-only), TLB shootdown; EACCES when adding PROT_WRITE to a shared mapping of a read-only fd or write-sealed memfd |
| 11 | munmap | yes | frees pages, TLB shootdown IPI to CPUs using the address space |
| 12 | brk | yes | lazy heap after the highest PT_LOAD |
| 13 | rt_sigaction | yes | SA_SIGINFO, SA_RESTORER (required), SA_ONSTACK, SA_RESTART, SA_NODEFER, SA_RESETHAND; setting SIG_IGN flushes pending |
| 14 | rt_sigprocmask | yes | SIGKILL/SIGSTOP never blocked |
| 15 | rt_sigreturn | yes | restores registers, mask, FPU state from the rt_sigframe |
| 16 | ioctl | partial | FIOCLEX/FIONCLEX/FIONBIO on any fd, FIONREAD on pipes; console: TCGETS/TCSETS*/TIOCGWINSZ/TIOCSWINSZ/TIOCGPGRP/TIOCSPGRP/TIOCSCTTY/TIOCGSID; everything else ENOTTY |
| 17 | pread64 | yes | ESPIPE on pipes/devices |
| 18 | pwrite64 | yes | |
| 19 | readv | yes | |
| 20 | writev | yes | gathered into one write |
| 21 | access | yes | = faccessat; root: only X_OK can fail |
| 22 | pipe | yes | = pipe2(0) |
| 23 | select | yes | fd_sets up to 1024 fds; EBADF for a closed fd in a set; the timeval is updated with the time left |
| 24 | sched_yield | yes | |
| 25 | mremap | partial | anonymous, shared and file mappings: shrink, grow in place, MREMAP_MAYMOVE, MREMAP_FIXED, MREMAP_DONTUNMAP; pages move by moving PTEs, a file/shared VMA keeps its object and offset; old_size 0 on a shared mapping (with MAYMOVE) maps the same pages again, EINVAL otherwise; the old range must lie in one VMA (EFAULT) |
| 26 | msync | yes | argument checks only (EINVAL: unaligned address, unknown flags, MS_SYNC with MS_ASYNC; ENOMEM: unmapped range); every file is RAM-backed |
| 27 | mincore | yes | 1 per page that is mapped or in its object's page cache; EINVAL unaligned, ENOMEM unmapped |
| 28 | madvise | partial | MADV_DONTNEED / MADV_FREE drop the pages of the range (private anonymous memory comes back zero-filled, file and shared mappings refault from their object); MADV_REMOVE punches a hole in the object behind a shared mapping (EINVAL for private, EACCES if not writable); NORMAL, RANDOM, SEQUENTIAL, WILLNEED, DONTFORK..DODUMP, COLD, PAGEOUT accepted as no-ops; EINVAL for an unaligned address or unknown advice, ENOMEM for an unmapped range |
| 32 | dup | yes | |
| 33 | dup2 | yes | |
| 35 | nanosleep | yes | EINTR + remaining time on signal |
| 39 | getpid | yes | thread group id |
| 56 | clone | yes | fork-style and thread-style (CLONE_VM, THREAD, SIGHAND, FILES, VFORK, PARENT, SETTLS, PARENT_SETTID, CHILD_SETTID, CHILD_CLEARTID); CLONE_PIDFD -> EINVAL; namespaces ignored |
| 57 | fork | yes | copy-on-write: private pages shared read-only, copied on the first write (or made writable again when no longer shared) |
| 58 | vfork | yes | shares memory, parent sleeps until the child execs or exits |
| 59 | execve | yes | path resolved through the VFS (cwd-relative, symlinks); static ELF (ET_EXEC, static-PIE); PT_INTERP -> ENOEXEC (dynamic linking later); ENOENT/EACCES/ENOEXEC/E2BIG; kills other threads; CLOEXEC; signal reset; records /proc/<pid>/exe and cmdline |
| 60 | exit | yes | thread exit, CLONE_CHILD_CLEARTID + futex wake |
| 61 | wait4 | yes | pid > 0, -1, 0, < -1; WNOHANG; ECHILD; rusage zeroed |
| 62 | kill | yes | pid > 0, 0 (group), -1, < -1; sig 0 = existence check |
| 63 | uname | yes | Linux / 6.6.0-android-pc / x86_64 |
| 72 | fcntl | yes | F_DUPFD(_CLOEXEC), F_GETFD/SETFD, F_GETFL/SETFL (O_APPEND, O_NONBLOCK, O_DIRECT, O_NOATIME, O_ASYNC), F_GETPIPE_SZ/SETPIPE_SZ (64 KiB fixed), F_ADD_SEALS/F_GET_SEALS on tmpfs/memfd files (SEAL, SHRINK, GROW, WRITE, FUTURE_WRITE, EXEC; tmpfs files and memfds without MFD_ALLOW_SEALING start with F_SEAL_SEAL; EPERM for a read-only fd or after F_SEAL_SEAL, EBUSY for F_SEAL_WRITE while a shared mapping could write; EINVAL on other files); record locks (F_SETLK*, F_OFD_*) always granted, F_GETLK says unlocked |
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
| 102 | getuid | yes | always 0 (credentials are later) |
| 104 | getgid | yes | 0 |
| 107 | geteuid | yes | 0 |
| 108 | getegid | yes | 0 |
| 109 | setpgid | partial | no permission checks |
| 110 | getppid | yes | 0 for init |
| 111 | getpgrp | yes | |
| 112 | setsid | partial | |
| 121 | getpgid | yes | |
| 124 | getsid | yes | |
| 127 | rt_sigpending | yes | pending & blocked |
| 128 | rt_sigtimedwait | yes | EAGAIN on timeout, EINTR on another signal |
| 130 | rt_sigsuspend | yes | old mask restored after the handler |
| 131 | sigaltstack | yes | SS_DISABLE, EPERM while on it, ENOMEM below MINSIGSTKSZ |
| 133 | mknod | yes | regular files, device nodes, FIFOs (open: readers wait for a writer and writers for a reader, O_NONBLOCK reader opens at once, O_NONBLOCK writer without reader ENXIO, O_RDWR never blocks), sockets (open ENXIO) |
| 137 | statfs | partial | f_type from the mount (tmpfs/ramfs/proc/sysfs magic); block counts are RAM frames |
| 138 | fstatfs | partial | pipes report PIPEFS_MAGIC |
| 157 | prctl | partial | PR_SET_NAME, PR_GET_NAME; others EINVAL |
| 158 | arch_prctl | yes | ARCH_SET/GET_FS/GS (EPERM above TASK_SIZE_MAX); RDFSBASE/WRFSBASE enabled and advertised in AT_HWCAP2 |
| 162 | sync | stub | 0 |
| 165 | mount | partial | tmpfs, ramfs (`mode=` option), proc, sysfs, devtmpfs; MS_BIND (also files); MS_REMOUNT and propagation-only calls accepted as no-ops; MS_MOVE EINVAL; other types ENODEV |
| 166 | umount2 | yes | EBUSY with mounts below; EINVAL if not a mount root; UMOUNT_NOFOLLOW |
| 169 | reboot | yes | POWER_OFF/HALT (ACPI PM1a / isa-debug-exit), RESTART |
| 186 | gettid | yes | |
| 200 | tkill | yes | |
| 201 | time | yes | |
| 202 | futex | partial | WAIT, WAKE, WAIT_BITSET (absolute, FUTEX_CLOCK_REALTIME), WAKE_BITSET, REQUEUE, CMP_REQUEUE; in a MAP_SHARED mapping (without FUTEX_PRIVATE_FLAG) keyed by (shmem object, offset), so it works across processes and mappings; otherwise by (address space, address); PI futexes ENOSYS |
| 213 | epoll_create | yes | size must be > 0 |
| 217 | getdents64 | yes | ".", "..", then the entries with real d_type; position = entry index (lseek 0 rewinds); EINVAL if the first record does not fit |
| 218 | set_tid_address | yes | |
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
| 289 | signalfd4 | yes | SFD_NONBLOCK, SFD_CLOEXEC; fd = -1 creates, else updates the mask of an existing signalfd (EINVAL otherwise); reads 128-byte signalfd_siginfo records of the reader's pending signals in the mask (several per read, EINVAL below 128 bytes); POLLIN while one is pending |
| 290 | eventfd2 | yes | EFD_SEMAPHORE, EFD_NONBLOCK, EFD_CLOEXEC; 8-byte reads/writes (EINVAL shorter or writing 2^64-1), write blocks/EAGAIN past 2^64-2 |
| 291 | epoll_create1 | yes | EPOLL_CLOEXEC |
| 292 | dup3 | yes | O_CLOEXEC |
| 293 | pipe2 | yes | O_CLOEXEC, O_NONBLOCK, O_DIRECT accepted (not packet mode); 64 KiB buffer |
| 295 | preadv | yes | |
| 296 | pwritev | yes | |
| 306 | syncfs | stub | 0 |
| 316 | renameat2 | yes | RENAME_NOREPLACE, RENAME_EXCHANGE; EXDEV across mounts, EINVAL moving a directory below itself |
| 318 | getrandom | yes | RDRAND + xorshift, not a vetted CSPRNG yet |
| 319 | memfd_create | yes | MFD_CLOEXEC, MFD_ALLOW_SEALING, MFD_NOEXEC_SEAL (mode 0666 + F_SEAL_EXEC), MFD_EXEC; MFD_HUGETLB and unknown flags EINVAL; names up to 249 bytes; shown as `/memfd:NAME (deleted)` in /proc/<pid>/fd and maps |
| 332 | statx | yes | basic stats (STATX_BASIC_STATS, no btime) |
| 436 | close_range | yes | CLOSE_RANGE_CLOEXEC, CLOSE_RANGE_UNSHARE |
| 439 | faccessat2 | yes | AT_EACCESS, AT_SYMLINK_NOFOLLOW, AT_EMPTY_PATH |
| 441 | epoll_pwait2 | yes | epoll_pwait with a timespec timeout |

## Readiness: poll, select, epoll

- What files report: pipe/FIFO read end POLLIN with data, POLLHUP once
  every writer is gone (a FIFO reader only after a writer has opened
  since it did); write end POLLOUT while not full, POLLERR without
  readers; eventfd POLLIN when > 0, POLLOUT below 2^64-2; timerfd POLLIN
  with unread expirations; signalfd POLLIN while a masked signal is
  pending for the polling thread; epoll fd POLLIN while an item on its
  ready list has events; console POLLOUT only (no input yet); regular
  files and memory devices are always ready (and not epoll-able).
- epoll: level-triggered items are reported as long as they are ready,
  EPOLLET items once per wake-up of the file, EPOLLONESHOT items once
  until re-armed with EPOLL_CTL_MOD; EPOLLERR/EPOLLHUP are always
  reported; EPOLLEXCLUSIVE and EPOLLWAKEUP are accepted (no thundering-herd
  or wakelock effect). Items are keyed by (fd, open file) and dropped once
  the file's last reference is closed (a dup keeps it alive, as on
  Linux). Nested epolls work.
- /proc/<pid>/fd shows `anon_inode:[eventfd]`, `[eventpoll]`, `[timerfd]`,
  `[signalfd]`; /proc/<pid>/fdinfo/N has pos/flags/mnt_id/ino plus the
  epoll items (`tfd: ... events: ... data: ...`), eventfd count, timerfd
  clock/ticks/it_value/it_interval and signalfd mask.

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
  timerfd and signalfd reads). poll, select, epoll_wait and their variants
  always return EINTR, as on Linux.
- Real-time signals do not queue (one pending instance each), like the
  standard ones.

## Not implemented yet (selection, by phase)

- **B2 leftovers**: legacy getdents (78), chroot/pivot_root, sendfile,
  copy_file_range, fadvise, inotify, xattrs, devpts, console input (reads EOF),
  permission checks (everything is root), read-only mounts (recorded only).
- **B3 leftovers (memory)**: MAP_GROWSDOWN, mmap of files on file systems
  without a page cache (procfs, devices other than /dev/zero), mlock family,
  re-opening a memfd through /proc/<pid>/fd/N (not a magic link).
- **B4b**: sockets (AF_UNIX: stream/dgram/seqpacket, SCM_RIGHTS,
  SO_PEERCRED). Also pidfd, TFD_IOC_SET_TICKS, timerfd clock-set
  cancellation, SIGIO/O_ASYNC.
- Later: credentials (setuid family, capabilities), rlimits, sched_*
  affinity/priority, rseq, clone3, io_uring (not used by AOSP).
