# Syscall table (Rust kernel)

Linux x86_64 syscalls the kernel in `kernel/` implements, by phase. The
dispatcher is `kernel/kernel/src/syscall/mod.rs`; every number not listed
returns `-ENOSYS` and is logged once (`unimplemented syscall N`). Boot with
`--append strace` to log every call. What AOSP needs overall is in
[syscall-trace.md](syscall-trace.md).

Status: **yes** = implemented with Linux semantics for what the B1 tests and
static programs use; **partial** = works for the common case, gaps listed;
**stub** = accepted, does nothing useful yet.

## Implemented (phase B1)

| Nr | Name | Status | Notes |
|---:|------|--------|-------|
| 0 | read | yes | ramfs files, console (EOF), /dev/null, /dev/zero, /dev/urandom, pipes |
| 1 | write | yes | console = serial (fd 0-2 of init), ramfs files, pipes (EPIPE + SIGPIPE) |
| 2 | open | partial | = openat(AT_FDCWD) |
| 3 | close | yes | |
| 4 | stat | partial | = newfstatat; no timestamps/uid |
| 5 | fstat | partial | ramfs inode, pipe (S_IFIFO), devices (S_IFCHR) |
| 6 | lstat | partial | no symlinks yet, same as stat |
| 8 | lseek | yes | SEEK_SET/CUR/END on ramfs files, ESPIPE on pipes |
| 9 | mmap | partial | anonymous only, lazily zero-filled; MAP_FIXED, MAP_FIXED_NOREPLACE, hint; MAP_SHARED behaves as private; file mappings -> ENODEV (B3) |
| 10 | mprotect | yes | splits VMAs, updates PTEs, TLB shootdown |
| 11 | munmap | yes | frees pages, TLB shootdown IPI to CPUs using the address space |
| 12 | brk | yes | lazy heap after the highest PT_LOAD |
| 13 | rt_sigaction | yes | SA_SIGINFO, SA_RESTORER (required), SA_ONSTACK, SA_RESTART, SA_NODEFER, SA_RESETHAND; setting SIG_IGN flushes pending |
| 14 | rt_sigprocmask | yes | SIGKILL/SIGSTOP never blocked |
| 15 | rt_sigreturn | yes | restores registers, mask, FPU state from the rt_sigframe |
| 16 | ioctl | stub | ENOTTY for every valid fd (no tty layer yet) |
| 19 | readv | yes | |
| 20 | writev | yes | gathered into one write |
| 22 | pipe | yes | = pipe2(0) |
| 24 | sched_yield | yes | |
| 28 | madvise | stub | returns 0 (MADV_DONTNEED zeroing is B3) |
| 32 | dup | yes | |
| 33 | dup2 | yes | |
| 35 | nanosleep | yes | EINTR + remaining time on signal |
| 39 | getpid | yes | thread group id |
| 56 | clone | yes | fork-style and thread-style (CLONE_VM, THREAD, SIGHAND, FILES, VFORK, PARENT, SETTLS, PARENT_SETTID, CHILD_SETTID, CHILD_CLEARTID); CLONE_PIDFD -> EINVAL; namespaces ignored |
| 57 | fork | yes | eager copy of the address space (COW is B3) |
| 58 | vfork | yes | shares memory, parent sleeps until the child execs or exits |
| 59 | execve | yes | static ELF (ET_EXEC, static-PIE); PT_INTERP -> ENOEXEC (dynamic linking later); ENOENT/EACCES/ENOEXEC/E2BIG; kills other threads; CLOEXEC; signal reset |
| 60 | exit | yes | thread exit, CLONE_CHILD_CLEARTID + futex wake |
| 61 | wait4 | yes | pid > 0, -1, 0, < -1; WNOHANG; ECHILD; rusage zeroed |
| 62 | kill | yes | pid > 0, 0 (group), -1, < -1; sig 0 = existence check |
| 63 | uname | yes | Linux / 6.6.0-android-pc / x86_64 |
| 72 | fcntl | partial | F_DUPFD(_CLOEXEC), F_GETFD/SETFD, F_GETFL/SETFL (O_APPEND, O_NONBLOCK) |
| 79 | getcwd | stub | always "/" (cwd is B2) |
| 83 | mkdir | yes | ramfs |
| 84 | rmdir | yes | ENOTEMPTY, ENOTDIR |
| 87 | unlink | yes | EISDIR |
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
| 157 | prctl | partial | PR_SET_NAME, PR_GET_NAME; others EINVAL |
| 158 | arch_prctl | yes | ARCH_SET/GET_FS/GS (EPERM above TASK_SIZE_MAX); RDFSBASE/WRFSBASE enabled and advertised in AT_HWCAP2 |
| 169 | reboot | yes | POWER_OFF/HALT (ACPI PM1a / isa-debug-exit), RESTART |
| 186 | gettid | yes | |
| 200 | tkill | yes | |
| 201 | time | yes | |
| 202 | futex | partial | WAIT, WAKE, WAIT_BITSET (absolute, FUTEX_CLOCK_REALTIME), WAKE_BITSET, REQUEUE, CMP_REQUEUE; keyed by (address space, address) so shared futexes across processes wait for MAP_SHARED (B3); PI futexes ENOSYS |
| 218 | set_tid_address | yes | |
| 228 | clock_gettime | yes | REALTIME (Limine boot date + TSC), MONOTONIC/BOOTTIME/RAW/COARSE (TSC); CPU-time clocks report uptime |
| 229 | clock_getres | yes | 1 ns, coarse clocks 10 ms |
| 230 | clock_nanosleep | yes | TIMER_ABSTIME |
| 231 | exit_group | yes | kills every thread of the group |
| 234 | tgkill | yes | si_code SI_TKILL |
| 257 | openat | partial | O_CREAT/EXCL/TRUNC/APPEND/DIRECTORY/CLOEXEC/NONBLOCK; dirfd must be AT_FDCWD or the path absolute (else ENOSYS until B2) |
| 258 | mkdirat | partial | same dirfd limit |
| 262 | newfstatat | partial | AT_EMPTY_PATH |
| 263 | unlinkat | partial | AT_REMOVEDIR |
| 273 | set_robust_list | partial | stored; robust futexes are not walked at exit yet |
| 274 | get_robust_list | yes | |
| 292 | dup3 | yes | O_CLOEXEC |
| 293 | pipe2 | yes | O_CLOEXEC, O_NONBLOCK; 64 KiB buffer |
| 318 | getrandom | yes | RDRAND + xorshift, not a vetted CSPRNG yet |

## Signals: what is delivered how

- Delivery happens on every return to user mode (syscall exit, interrupt,
  exception), through a Linux-compatible `rt_sigframe` (ucontext with
  sigcontext, 512-byte FXSAVE area, siginfo). `sa_restorer` is mandatory, as
  on Linux x86_64.
- Faults: #PF outside a VMA or against its protection -> SIGSEGV (si_addr =
  CR2, SEGV_MAPERR/ACCERR), #GP -> SIGSEGV, #UD -> SIGILL, #DE -> SIGFPE,
  #XM/#MF -> SIGFPE, `int3` -> SIGTRAP, #AC -> SIGBUS. A blocked or ignored
  fault signal is forced to the default action.
- Default actions terminate (no core files), ignore, or, for stop/continue
  signals, do nothing (no job control yet).
- Interrupted syscalls return EINTR, or are restarted with SA_RESTART
  (wait4, blocking futex wait, pipe read/write).
- Real-time signals do not queue (one pending instance each), like the
  standard ones.

## Not implemented yet (selection, by phase)

- **B2 (VFS)**: pread64/pwrite64, getdents64, chdir/fchdir, rename*, link*,
  symlink*/readlink*, access/faccessat, ftruncate, umask, close_range,
  statfs, mount/umount2, *at with a real directory fd, /proc, /sys, /dev.
- **B3 (memory)**: file and shared mappings, mremap, madvise, msync,
  memfd_create, COW fork.
- **B4 (IPC / multiplexing)**: poll/ppoll/select, epoll_*, eventfd2,
  timerfd_*, signalfd4, sockets (AF_UNIX), pidfd.
- Later: credentials (setuid family, capabilities), rlimits, sched_*
  affinity/priority, rseq, clone3, io_uring (not used by AOSP).
