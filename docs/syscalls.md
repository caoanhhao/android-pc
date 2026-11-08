# Syscall table (Rust kernel)

Linux x86_64 syscalls the kernel in `kernel/` implements, by phase. The
dispatcher is `kernel/kernel/src/syscall/mod.rs`; every number not listed
returns `-ENOSYS` and is logged once (`unimplemented syscall N`). Boot with
`--append strace` to log every call. What AOSP needs overall is in
[syscall-trace.md](syscall-trace.md).

Status: **yes** = implemented with Linux semantics for what the B1 tests and
static programs use; **partial** = works for the common case, gaps listed;
**stub** = accepted, does nothing useful yet.

## Implemented (phases B1-B4b, B5 AOSP shell)

| Nr | Name | Status | Notes |
|---:|------|--------|-------|
| 0 | read | yes | VFS files, devices, pipes/FIFOs, sockets (= recv 0), /proc, eventfd/timerfd/signalfd; regular files read in 64 KiB chunks until done |
| 1 | write | yes | VFS files (O_APPEND atomic), devices, pipes (EPIPE + SIGPIPE, atomic up to PIPE_BUF), sockets (= send 0) |
| 2 | open | yes | = openat(AT_FDCWD) |
| 3 | close | yes | |
| 4 | stat | yes | = newfstatat(AT_FDCWD, 0) |
| 5 | fstat | yes | any fd (pipes S_IFIFO, sockets S_IFSOCK, devices S_IFCHR with rdev); uid/gid/times stored |
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
| 16 | ioctl | partial | FIOCLEX/FIONCLEX/FIONBIO on any fd, FIONREAD on pipes, FIONREAD (SIOCINQ)/SIOCOUTQ on sockets; console tty: TCGETS/TCSETS/TCSETSW/TCSETSF (F flushes input; switching ICANON keeps pending input), TIOCGWINSZ/TIOCSWINSZ, TIOCGPGRP/TIOCSPGRP (the foreground group ^C/^\/^Z signal), FIONREAD, TIOCOUTQ (0), TCFLSH, TCSBRK/TCSBRKP/TCXONC/TIOCSCTTY/TIOCNOTTY (no-ops), TIOCGSID; everything else ENOTTY |
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
| 41 | socket | yes | AF_UNIX: SOCK_STREAM, SOCK_DGRAM (SOCK_RAW = DGRAM), SOCK_SEQPACKET; SOCK_NONBLOCK, SOCK_CLOEXEC; protocol 0/1 else EPROTONOSUPPORT, other types ESOCKTNOSUPPORT, unknown flags EINVAL; every other family (AF_INET, AF_INET6, AF_NETLINK, ...) EAFNOSUPPORT |
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
| 54 | setsockopt | partial | SOL_SOCKET: SO_PASSCRED, SO_SNDBUF/RCVBUF(+FORCE) (stored doubled, min 4608/2304), SO_RCVTIMEO/SNDTIMEO (old and new; EDOM for bad usec), common flags (REUSEADDR, KEEPALIVE, ...) accepted; others ENOPROTOOPT, other levels EOPNOTSUPP |
| 55 | getsockopt | partial | SOL_SOCKET: SO_TYPE, SO_DOMAIN, SO_PROTOCOL, SO_ERROR, SO_SNDBUF/RCVBUF (212992 default), SO_PASSCRED, SO_PEERCRED (pid 0 uid/gid 65534 without peer), SO_ACCEPTCONN, SO_RCVTIMEO/SNDTIMEO, SO_RCVLOWAT/SNDLOWAT (1); SO_PEERSEC ENOPROTOOPT |
| 56 | clone | yes | fork-style and thread-style (CLONE_VM, THREAD, SIGHAND, FILES, VFORK, PARENT, SETTLS, PARENT_SETTID, CHILD_SETTID, CHILD_CLEARTID); CLONE_PIDFD -> EINVAL; namespaces ignored |
| 57 | fork | yes | copy-on-write: private pages shared read-only, copied on the first write (or made writable again when no longer shared) |
| 58 | vfork | yes | shares memory, parent sleeps until the child execs or exits |
| 59 | execve | yes | path resolved through the VFS (cwd-relative, symlinks); ELF ET_EXEC and ET_DYN (PIE at 0x555555554000), static or dynamic: the PT_INTERP program (bionic's linker64) is loaded at 0x7fc000000000 and entered with AT_BASE, AT_PHDR/AT_ENTRY of the program, AT_RANDOM, AT_SECURE 0, AT_EXECFN; `#!interp [arg]` scripts (up to 5 levels, else ELOOP); ELIBBAD for a bad interpreter; ENOENT/EACCES/ENOEXEC/E2BIG; kills other threads; CLOEXEC; signal reset; records /proc/<pid>/exe and cmdline |
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
| 97 | getrlimit | yes | = prlimit64(0) |
| 98 | getrusage | stub | RUSAGE_SELF/CHILDREN/THREAD; all fields 0 (no CPU time accounting yet); EINVAL for other `who` |
| 99 | sysinfo | yes | uptime, totalram/freeram (frames), procs; loads and swap 0, mem_unit 1 |
| 100 | times | partial | CPU times 0; returns clock ticks (HZ 100) since boot |
| 102 | getuid | yes | always 0 (credentials are later) |
| 104 | getgid | yes | 0 |
| 105 | setuid | stub | everyone is root: any id is accepted, ids stay 0 (no credentials model yet); same for setgid (106), setreuid (113), setregid (114), setgroups (116), setresuid (117), setresgid (119) |
| 107 | geteuid | yes | 0 |
| 108 | getegid | yes | 0 |
| 109 | setpgid | partial | no permission checks |
| 110 | getppid | yes | 0 for init |
| 111 | getpgrp | yes | |
| 112 | setsid | partial | |
| 115 | getgroups | yes | no supplementary groups: 0; EINVAL for a negative size |
| 118 | getresuid | yes | 0, 0, 0; getresgid (120) the same |
| 121 | getpgid | yes | |
| 122 | setfsuid | stub | returns 0 (the old fs id); setfsgid (123) the same |
| 124 | getsid | yes | |
| 125 | capget | yes | versions 1-3 (EINVAL + preferred version written back otherwise); effective = permitted = all caps 0-40, inheritable 0 |
| 126 | capset | stub | header/data checked, nothing enforced |
| 127 | rt_sigpending | yes | pending & blocked |
| 128 | rt_sigtimedwait | yes | EAGAIN on timeout, EINTR on another signal |
| 129 | rt_sigqueueinfo | yes | siginfo from the caller (si_code, pid, uid, si_value); EPERM for si_code >= 0 or SI_TKILL to another process |
| 130 | rt_sigsuspend | yes | old mask restored after the handler |
| 131 | sigaltstack | yes | SS_DISABLE, EPERM while on it, ENOMEM below MINSIGSTKSZ |
| 133 | mknod | yes | regular files, device nodes, FIFOs (open: readers wait for a writer and writers for a reader, O_NONBLOCK reader opens at once, O_NONBLOCK writer without reader ENXIO, O_RDWR never blocks), sockets (open ENXIO, connect works on a bound one) |
| 135 | personality | yes | stored per process (inherited by fork); 0xffffffff queries |
| 137 | statfs | partial | f_type from the mount (tmpfs/ramfs/proc/sysfs magic); block counts are RAM frames |
| 138 | fstatfs | partial | pipes report PIPEFS_MAGIC |
| 140 | getpriority | yes | PRIO_PROCESS/PGRP/USER; returns 20 - nice like the raw syscall |
| 141 | setpriority | partial | nice stored (clamped to -20..19), the scheduler ignores it |
| 142 | sched_setparam | partial | = sched_setscheduler with the current policy |
| 143 | sched_getparam | yes | stored priority |
| 144 | sched_setscheduler | partial | OTHER/BATCH/IDLE (priority 0), FIFO/RR (1-99) checked and stored per process (SCHED_RESET_ON_FORK ignored); one round-robin scheduler runs everything |
| 145 | sched_getscheduler | yes | stored policy (default SCHED_OTHER) |
| 146 | sched_get_priority_max | yes | 99 for FIFO/RR, 0 otherwise; sched_get_priority_min (147): 1 / 0 |
| 149 | mlock | stub | all memory is resident: 0; also munlock (150), mlockall (151), munlockall (152), mlock2 (325) |
| 157 | prctl | partial | PR_SET_NAME, PR_GET_NAME, PR_GET/SET_DUMPABLE, PR_GET/SET_KEEPCAPS, PR_GET/SET_NO_NEW_PRIVS (stored, not enforced), PR_CAPBSET_READ (1 for caps 0-40), PR_SET_VMA (anon names accepted, not kept); others EINVAL |
| 158 | arch_prctl | yes | ARCH_SET/GET_FS/GS (EPERM above TASK_SIZE_MAX); RDFSBASE/WRFSBASE enabled and advertised in AT_HWCAP2 |
| 160 | setrlimit | yes | = prlimit64(0) |
| 162 | sync | stub | 0 |
| 165 | mount | partial | tmpfs, ramfs (`mode=` option), proc, sysfs, devtmpfs; MS_BIND (also files); MS_REMOUNT and propagation-only calls accepted as no-ops; MS_MOVE EINVAL; other types ENODEV |
| 166 | umount2 | yes | EBUSY with mounts below; EINVAL if not a mount root; UMOUNT_NOFOLLOW |
| 169 | reboot | yes | POWER_OFF/HALT (ACPI PM1a / isa-debug-exit), RESTART |
| 186 | gettid | yes | |
| 200 | tkill | yes | |
| 201 | time | yes | |
| 202 | futex | partial | WAIT, WAKE, WAIT_BITSET (absolute, FUTEX_CLOCK_REALTIME), WAKE_BITSET, REQUEUE, CMP_REQUEUE; in a MAP_SHARED mapping (without FUTEX_PRIVATE_FLAG) keyed by (shmem object, offset), so it works across processes and mappings; otherwise by (address space, address); PI futexes ENOSYS |
| 203 | sched_setaffinity | partial | accepted when the mask names an online CPU (else EINVAL); threads still run anywhere |
| 204 | sched_getaffinity | yes | all online CPUs; returns the mask size in bytes (EINVAL when `len` is too small or not a multiple of 8) |
| 213 | epoll_create | yes | size must be > 0 |
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
| 302 | prlimit64 | yes | per-process limits, inherited by fork; Linux defaults (STACK 8 MiB, NOFILE 1024, ...); EINVAL soft > hard, EPERM for RLIMIT_NOFILE above 1024 (the fd table size); not enforced except NOFILE by the fd table |
| 306 | syncfs | stub | 0 |
| 307 | sendmmsg | yes | sendmsg in a loop; count sent, the error only if none was |
| 309 | getcpu | yes | current CPU, node 0 |
| 316 | renameat2 | yes | RENAME_NOREPLACE, RENAME_EXCHANGE; EXDEV across mounts, EINVAL moving a directory below itself |
| 318 | getrandom | yes | RDRAND + xorshift, not a vetted CSPRNG yet |
| 319 | memfd_create | yes | MFD_CLOEXEC, MFD_ALLOW_SEALING, MFD_NOEXEC_SEAL (mode 0666 + F_SEAL_EXEC), MFD_EXEC; MFD_HUGETLB and unknown flags EINVAL; names up to 249 bytes; shown as `/memfd:NAME (deleted)` in /proc/<pid>/fd and maps |
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

- **B2 leftovers**: legacy getdents (78), chroot/pivot_root, sendfile,
  copy_file_range, inotify, xattrs, devpts,
  permission checks (everything is root), read-only mounts (recorded only).
- **B3 leftovers (memory)**: MAP_GROWSDOWN, mmap of files on file systems
  without a page cache (procfs, devices other than /dev/zero),
  re-opening a memfd through /proc/<pid>/fd/N (not a magic link).
- **B4 leftovers**: pidfd, TFD_IOC_SET_TICKS, timerfd clock-set
  cancellation, SIGIO/O_ASYNC. Sockets: AF_INET/AF_INET6 (no network
  stack), AF_NETLINK (uevents in C3), MSG_OOB, SO_PEEK_OFF,
  SO_PEERGROUPS, SCM_SECURITY, garbage collection of in-flight fd cycles
  (a socket sent over itself and never received leaks),
  /proc/sys/net/unix/max_dgram_qlen (fixed at 600).
- **B5 leftovers**: a credentials model (the set*id calls change nothing,
  capabilities are not checked), CPU time accounting (getrusage/times
  report 0), scheduling classes/affinity/nice (stored only), SIGTTIN/SIGTTOU
  and stop/continue job control, PR_SET_PDEATHSIG, ELF segments are
  copied (not mapped from the file's page cache), waitid, membarrier.
- Later: rseq, clone3, io_uring (not used by AOSP).
