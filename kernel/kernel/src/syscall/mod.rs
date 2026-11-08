//! Linux x86_64 syscall layer.
//!
//! `arch::x86_64::entry` saves the user registers in a `TrapFrame` and
//! calls [`dispatch`], which decodes the Linux syscall number in RAX and
//! the arguments in RDI, RSI, RDX, R10, R8, R9, and stores the result (or
//! `-errno`) back in RAX. The implemented set is listed in
//! `docs/syscalls.md`; anything else returns ENOSYS (logged once).
//!
//! Booting with `strace` on the kernel command line logs every syscall.

pub mod fs;
pub mod keys;
pub mod net;
pub mod poll;
pub mod sys;
pub mod time;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::TrapFrame;
use crate::errno::*;
use alloc::sync::Arc;

use crate::fs::file::{O_APPEND, OpenFile};
use crate::mm::shmem::WRITE_SEALS;
use crate::mm::user::{Backing, PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::process::{TABLE, current, futex, lifecycle, signal};

pub static TRACE: AtomicBool = AtomicBool::new(false);
static WARNED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

pub mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const STAT: u64 = 4;
    pub const FSTAT: u64 = 5;
    pub const LSTAT: u64 = 6;
    pub const POLL: u64 = 7;
    pub const LSEEK: u64 = 8;
    pub const MMAP: u64 = 9;
    pub const MPROTECT: u64 = 10;
    pub const MUNMAP: u64 = 11;
    pub const BRK: u64 = 12;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const RT_SIGRETURN: u64 = 15;
    pub const IOCTL: u64 = 16;
    pub const PREAD64: u64 = 17;
    pub const PWRITE64: u64 = 18;
    pub const READV: u64 = 19;
    pub const WRITEV: u64 = 20;
    pub const ACCESS: u64 = 21;
    pub const PIPE: u64 = 22;
    pub const SELECT: u64 = 23;
    pub const SCHED_YIELD: u64 = 24;
    pub const MREMAP: u64 = 25;
    pub const MSYNC: u64 = 26;
    pub const MINCORE: u64 = 27;
    pub const MADVISE: u64 = 28;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const NANOSLEEP: u64 = 35;
    pub const GETPID: u64 = 39;
    pub const SENDFILE: u64 = 40;
    pub const SOCKET: u64 = 41;
    pub const CONNECT: u64 = 42;
    pub const ACCEPT: u64 = 43;
    pub const SENDTO: u64 = 44;
    pub const RECVFROM: u64 = 45;
    pub const SENDMSG: u64 = 46;
    pub const RECVMSG: u64 = 47;
    pub const SHUTDOWN: u64 = 48;
    pub const BIND: u64 = 49;
    pub const LISTEN: u64 = 50;
    pub const GETSOCKNAME: u64 = 51;
    pub const GETPEERNAME: u64 = 52;
    pub const SOCKETPAIR: u64 = 53;
    pub const SETSOCKOPT: u64 = 54;
    pub const GETSOCKOPT: u64 = 55;
    pub const CLONE: u64 = 56;
    pub const FORK: u64 = 57;
    pub const VFORK: u64 = 58;
    pub const EXECVE: u64 = 59;
    pub const EXIT: u64 = 60;
    pub const WAIT4: u64 = 61;
    pub const KILL: u64 = 62;
    pub const UNAME: u64 = 63;
    pub const FCNTL: u64 = 72;
    pub const FLOCK: u64 = 73;
    pub const FSYNC: u64 = 74;
    pub const FDATASYNC: u64 = 75;
    pub const TRUNCATE: u64 = 76;
    pub const FTRUNCATE: u64 = 77;
    pub const GETCWD: u64 = 79;
    pub const CHDIR: u64 = 80;
    pub const FCHDIR: u64 = 81;
    pub const RENAME: u64 = 82;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const CREAT: u64 = 85;
    pub const LINK: u64 = 86;
    pub const UNLINK: u64 = 87;
    pub const SYMLINK: u64 = 88;
    pub const READLINK: u64 = 89;
    pub const CHMOD: u64 = 90;
    pub const FCHMOD: u64 = 91;
    pub const CHOWN: u64 = 92;
    pub const FCHOWN: u64 = 93;
    pub const LCHOWN: u64 = 94;
    pub const UMASK: u64 = 95;
    pub const GETTIMEOFDAY: u64 = 96;
    pub const GETRLIMIT: u64 = 97;
    pub const GETRUSAGE: u64 = 98;
    pub const SYSINFO: u64 = 99;
    pub const TIMES: u64 = 100;
    pub const GETUID: u64 = 102;
    pub const GETGID: u64 = 104;
    pub const SETUID: u64 = 105;
    pub const SETGID: u64 = 106;
    pub const GETEUID: u64 = 107;
    pub const GETEGID: u64 = 108;
    pub const SETPGID: u64 = 109;
    pub const GETPPID: u64 = 110;
    pub const GETPGRP: u64 = 111;
    pub const SETSID: u64 = 112;
    pub const SETREUID: u64 = 113;
    pub const SETREGID: u64 = 114;
    pub const GETGROUPS: u64 = 115;
    pub const SETGROUPS: u64 = 116;
    pub const SETRESUID: u64 = 117;
    pub const GETRESUID: u64 = 118;
    pub const SETRESGID: u64 = 119;
    pub const GETRESGID: u64 = 120;
    pub const GETPGID: u64 = 121;
    pub const SETFSUID: u64 = 122;
    pub const SETFSGID: u64 = 123;
    pub const GETSID: u64 = 124;
    pub const CAPGET: u64 = 125;
    pub const CAPSET: u64 = 126;
    pub const RT_SIGPENDING: u64 = 127;
    pub const RT_SIGTIMEDWAIT: u64 = 128;
    pub const RT_SIGQUEUEINFO: u64 = 129;
    pub const RT_SIGSUSPEND: u64 = 130;
    pub const SIGALTSTACK: u64 = 131;
    pub const MKNOD: u64 = 133;
    pub const PERSONALITY: u64 = 135;
    pub const STATFS: u64 = 137;
    pub const FSTATFS: u64 = 138;
    pub const GETPRIORITY: u64 = 140;
    pub const SETPRIORITY: u64 = 141;
    pub const SCHED_SETPARAM: u64 = 142;
    pub const SCHED_GETPARAM: u64 = 143;
    pub const SCHED_SETSCHEDULER: u64 = 144;
    pub const SCHED_GETSCHEDULER: u64 = 145;
    pub const SCHED_GET_PRIORITY_MAX: u64 = 146;
    pub const SCHED_GET_PRIORITY_MIN: u64 = 147;
    pub const PIVOT_ROOT: u64 = 155;
    pub const MLOCK: u64 = 149;
    pub const MUNLOCK: u64 = 150;
    pub const MLOCKALL: u64 = 151;
    pub const MUNLOCKALL: u64 = 152;
    pub const PRCTL: u64 = 157;
    pub const ARCH_PRCTL: u64 = 158;
    pub const SYSLOG: u64 = 103;
    pub const SETRLIMIT: u64 = 160;
    pub const SYNC: u64 = 162;
    pub const CHROOT: u64 = 161;
    pub const MOUNT: u64 = 165;
    pub const UMOUNT2: u64 = 166;
    pub const REBOOT: u64 = 169;
    pub const SETHOSTNAME: u64 = 170;
    pub const SETDOMAINNAME: u64 = 171;
    pub const INIT_MODULE: u64 = 175;
    pub const GETTID: u64 = 186;
    pub const SETXATTR: u64 = 188;
    pub const LSETXATTR: u64 = 189;
    pub const FSETXATTR: u64 = 190;
    pub const GETXATTR: u64 = 191;
    pub const LGETXATTR: u64 = 192;
    pub const FGETXATTR: u64 = 193;
    pub const LISTXATTR: u64 = 194;
    pub const LLISTXATTR: u64 = 195;
    pub const FLISTXATTR: u64 = 196;
    pub const REMOVEXATTR: u64 = 197;
    pub const LREMOVEXATTR: u64 = 198;
    pub const FREMOVEXATTR: u64 = 199;
    pub const TKILL: u64 = 200;
    pub const TIME: u64 = 201;
    pub const FUTEX: u64 = 202;
    pub const SCHED_SETAFFINITY: u64 = 203;
    pub const SCHED_GETAFFINITY: u64 = 204;
    pub const EPOLL_CREATE: u64 = 213;
    pub const WAITID: u64 = 247;
    pub const ADD_KEY: u64 = 248;
    pub const REQUEST_KEY: u64 = 249;
    pub const KEYCTL: u64 = 250;
    pub const IOPRIO_SET: u64 = 251;
    pub const IOPRIO_GET: u64 = 252;
    pub const INOTIFY_INIT: u64 = 253;
    pub const INOTIFY_ADD_WATCH: u64 = 254;
    pub const INOTIFY_RM_WATCH: u64 = 255;
    pub const GETDENTS64: u64 = 217;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const FADVISE64: u64 = 221;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const CLOCK_GETRES: u64 = 229;
    pub const CLOCK_NANOSLEEP: u64 = 230;
    pub const EXIT_GROUP: u64 = 231;
    pub const EPOLL_WAIT: u64 = 232;
    pub const EPOLL_CTL: u64 = 233;
    pub const TGKILL: u64 = 234;
    pub const OPENAT: u64 = 257;
    pub const MKDIRAT: u64 = 258;
    pub const MKNODAT: u64 = 259;
    pub const FCHOWNAT: u64 = 260;
    pub const NEWFSTATAT: u64 = 262;
    pub const UNLINKAT: u64 = 263;
    pub const RENAMEAT: u64 = 264;
    pub const LINKAT: u64 = 265;
    pub const SYMLINKAT: u64 = 266;
    pub const READLINKAT: u64 = 267;
    pub const FCHMODAT: u64 = 268;
    pub const FACCESSAT: u64 = 269;
    pub const PSELECT6: u64 = 270;
    pub const PPOLL: u64 = 271;
    pub const UNSHARE: u64 = 272;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const GET_ROBUST_LIST: u64 = 274;
    pub const UTIMENSAT: u64 = 280;
    pub const EPOLL_PWAIT: u64 = 281;
    pub const SIGNALFD: u64 = 282;
    pub const TIMERFD_CREATE: u64 = 283;
    pub const EVENTFD: u64 = 284;
    pub const FALLOCATE: u64 = 285;
    pub const TIMERFD_SETTIME: u64 = 286;
    pub const TIMERFD_GETTIME: u64 = 287;
    pub const ACCEPT4: u64 = 288;
    pub const SIGNALFD4: u64 = 289;
    pub const EVENTFD2: u64 = 290;
    pub const EPOLL_CREATE1: u64 = 291;
    pub const DUP3: u64 = 292;
    pub const PIPE2: u64 = 293;
    pub const INOTIFY_INIT1: u64 = 294;
    pub const PREADV: u64 = 295;
    pub const PWRITEV: u64 = 296;
    pub const RT_TGSIGQUEUEINFO: u64 = 297;
    pub const RECVMMSG: u64 = 299;
    pub const PRLIMIT64: u64 = 302;
    pub const SYNCFS: u64 = 306;
    pub const SENDMMSG: u64 = 307;
    pub const SETNS: u64 = 308;
    pub const GETCPU: u64 = 309;
    pub const FINIT_MODULE: u64 = 313;
    pub const RENAMEAT2: u64 = 316;
    pub const GETRANDOM: u64 = 318;
    pub const MEMFD_CREATE: u64 = 319;
    pub const MLOCK2: u64 = 325;
    pub const COPY_FILE_RANGE: u64 = 326;
    pub const STATX: u64 = 332;
    pub const CLOSE_RANGE: u64 = 436;
    pub const FACCESSAT2: u64 = 439;
    pub const EPOLL_PWAIT2: u64 = 441;
}

fn encode(r: KResult<u64>) -> u64 {
    match r {
        Ok(v) => v,
        Err(e) => (-(e.0 as i64)) as u64,
    }
}

fn warn_once(n: u64) {
    if n < 512 {
        let (w, b) = ((n / 64) as usize, 1u64 << (n % 64));
        if WARNED[w].fetch_or(b, Ordering::Relaxed) & b != 0 {
            return;
        }
    }
    log::warn!("unimplemented syscall {} (pid {})", n, current().pid());
}

pub fn dispatch(frame: &mut TrapFrame) {
    let n = frame.rax;
    let [a0, a1, a2, a3, a4, a5] = frame.args();
    let trace = TRACE.load(Ordering::Relaxed);
    if trace {
        log::info!(
            "[{}] syscall {} ({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x})",
            current().tid,
            n,
            a0,
            a1,
            a2,
            a3,
            a4,
            a5
        );
    }
    let (i0, i1) = (a0 as i32, a1 as i32);
    let r = match n {
        nr::READ => fs::sys_read(i0, a1, a2),
        nr::WRITE => fs::sys_write(i0, a1, a2),
        nr::OPEN => fs::sys_openat(fs::AT_FDCWD, a0, a1 as u32, a2 as u32),
        nr::CLOSE => fs::sys_close(i0),
        nr::STAT => fs::sys_stat(a0, a1, true),
        nr::LSTAT => fs::sys_stat(a0, a1, false),
        nr::FSTAT => fs::sys_fstat(i0, a1),
        nr::LSEEK => fs::sys_lseek(i0, a1 as i64, a2 as u32),
        nr::MMAP => sys_mmap(a0, a1, a2, a3, a4 as i32, a5),
        nr::MPROTECT => sys_mprotect(a0, a1, a2),
        nr::MUNMAP => current().mm().munmap(a0, a1).map(|_| 0),
        nr::BRK => Ok(current().mm().brk(a0)),
        nr::MREMAP => current().mm().mremap(a0, a1, a2, a3 as u32, a4),
        nr::MSYNC => current().mm().msync(a0, a1, a2 as u32).map(|_| 0),
        nr::MINCORE => sys_mincore(a0, a1, a2),
        nr::RT_SIGACTION => signal::sys_rt_sigaction(a0 as u32, a1, a2, a3),
        nr::RT_SIGPROCMASK => signal::sys_rt_sigprocmask(a0 as u32, a1, a2, a3),
        nr::RT_SIGRETURN => signal::sigreturn(frame),
        nr::IOCTL => fs::sys_ioctl(i0, a1 as u32, a2),
        nr::PREAD64 => fs::sys_pread64(i0, a1, a2, a3 as i64),
        nr::PWRITE64 => fs::sys_pwrite64(i0, a1, a2, a3 as i64),
        nr::READV => fs::sys_readv(i0, a1, a2),
        nr::WRITEV => fs::sys_writev(i0, a1, a2),
        nr::ACCESS => fs::sys_faccessat(fs::AT_FDCWD, a0, a1 as u32, 0),
        nr::PIPE => fs::sys_pipe2(a0, 0),
        nr::SCHED_YIELD => {
            crate::sched::yield_now();
            Ok(0)
        }
        nr::MADVISE => current().mm().madvise(a0, a1, a2 as u32).map(|_| 0),
        nr::DUP => fs::sys_dup(i0),
        nr::DUP2 => fs::sys_dup3(i0, i1, 0, true),
        nr::NANOSLEEP => time::sys_nanosleep(a0, a1),
        nr::GETPID => Ok(current().pid() as u64),
        nr::SOCKET => net::sys_socket(a0 as u32, a1 as u32, a2 as u32),
        nr::SOCKETPAIR => net::sys_socketpair(a0 as u32, a1 as u32, a2 as u32, a3),
        nr::CONNECT => net::sys_connect(i0, a1, a2 as u32),
        nr::ACCEPT => net::sys_accept4(i0, a1, a2, 0),
        nr::ACCEPT4 => net::sys_accept4(i0, a1, a2, a3 as u32),
        nr::SENDTO => net::sys_sendto(i0, a1, a2, a3 as u32, a4, a5 as u32),
        nr::RECVFROM => net::sys_recvfrom(i0, a1, a2, a3 as u32, a4, a5),
        nr::SENDMSG => net::sys_sendmsg(i0, a1, a2 as u32),
        nr::RECVMSG => net::sys_recvmsg(i0, a1, a2 as u32),
        nr::SENDMMSG => net::sys_sendmmsg(i0, a1, a2 as u32, a3 as u32),
        nr::RECVMMSG => net::sys_recvmmsg(i0, a1, a2 as u32, a3 as u32, a4),
        nr::SHUTDOWN => net::sys_shutdown(i0, a1 as u32),
        nr::BIND => net::sys_bind(i0, a1, a2 as u32),
        nr::LISTEN => net::sys_listen(i0, a1 as i32),
        nr::GETSOCKNAME => net::sys_getsockname(i0, a1, a2),
        nr::GETPEERNAME => net::sys_getpeername(i0, a1, a2),
        nr::SETSOCKOPT => net::sys_setsockopt(i0, a1 as i32, a2 as i32, a3, a4 as u32),
        nr::GETSOCKOPT => net::sys_getsockopt(i0, a1 as i32, a2 as i32, a3, a4),
        nr::CLONE => lifecycle::sys_clone(frame, a0, a1, a2, a3, a4),
        nr::FORK => lifecycle::sys_clone(frame, signal::SIGCHLD as u64, 0, 0, 0, 0),
        nr::VFORK => lifecycle::sys_clone(
            frame,
            lifecycle::CLONE_VM | lifecycle::CLONE_VFORK | signal::SIGCHLD as u64,
            0,
            0,
            0,
            0,
        ),
        nr::EXECVE => lifecycle::sys_execve(frame, a0, a1, a2),
        nr::EXIT => lifecycle::sys_exit(i0),
        nr::WAIT4 => lifecycle::sys_wait4(i0, a1, a2 as u32, a3),
        nr::WAITID => lifecycle::sys_waitid(a0 as u32, a1 as u32, a2, a3 as u32, a4),
        nr::KILL => signal::sys_kill(i0, a1 as u32),
        nr::UNAME => sys_uname(a0),
        nr::FCNTL => fs::sys_fcntl(i0, a1 as u32, a2),
        nr::FLOCK | nr::FSYNC | nr::FDATASYNC => fs::sys_fsync(i0),
        nr::TRUNCATE => fs::sys_truncate(a0, a1 as i64),
        nr::FTRUNCATE => fs::sys_ftruncate(i0, a1 as i64),
        nr::GETCWD => fs::sys_getcwd(a0, a1),
        nr::CHDIR => fs::sys_chdir(a0),
        nr::FCHDIR => fs::sys_fchdir(i0),
        nr::RENAME => fs::sys_renameat2(fs::AT_FDCWD, a0, fs::AT_FDCWD, a1, 0),
        nr::MKDIR => fs::sys_mkdirat(fs::AT_FDCWD, a0, a1 as u32),
        nr::RMDIR => fs::sys_unlinkat(fs::AT_FDCWD, a0, 0x200),
        nr::CREAT => fs::sys_openat(fs::AT_FDCWD, a0, 0o1101, a1 as u32),
        nr::LINK => fs::sys_linkat(fs::AT_FDCWD, a0, fs::AT_FDCWD, a1, 0),
        nr::UNLINK => fs::sys_unlinkat(fs::AT_FDCWD, a0, 0),
        nr::SYMLINK => fs::sys_symlinkat(a0, fs::AT_FDCWD, a1),
        nr::READLINK => fs::sys_readlinkat(fs::AT_FDCWD, a0, a1, a2 as i32),
        nr::CHMOD => fs::sys_fchmodat(fs::AT_FDCWD, a0, a1 as u32, 0),
        nr::FCHMOD => fs::sys_fchmod(i0, a1 as u32),
        nr::CHOWN => fs::sys_fchownat(fs::AT_FDCWD, a0, a1 as u32, a2 as u32, 0),
        nr::FCHOWN => fs::sys_fchown(i0, a1 as u32, a2 as u32),
        nr::LCHOWN => fs::sys_fchownat(fs::AT_FDCWD, a0, a1 as u32, a2 as u32, 0x100),
        nr::UMASK => fs::sys_umask(a0 as u32),
        nr::MKNOD => fs::sys_mknodat(fs::AT_FDCWD, a0, a1 as u32, a2),
        nr::STATFS => fs::sys_statfs(a0, a1),
        nr::GETXATTR | nr::LGETXATTR => fs::sys_getxattr(fs::XattrTarget::Path(a0, n == nr::GETXATTR), a1, a2, a3),
        nr::FGETXATTR => fs::sys_getxattr(fs::XattrTarget::Fd(i0), a1, a2, a3),
        nr::LISTXATTR | nr::LLISTXATTR => fs::sys_listxattr(fs::XattrTarget::Path(a0, n == nr::LISTXATTR), a1, a2),
        nr::FLISTXATTR => fs::sys_listxattr(fs::XattrTarget::Fd(i0), a1, a2),
        nr::SETXATTR | nr::LSETXATTR => {
            fs::sys_setxattr(fs::XattrTarget::Path(a0, n == nr::SETXATTR), a1, Some((a2, a3)), a4 as u32)
        }
        nr::FSETXATTR => fs::sys_setxattr(fs::XattrTarget::Fd(i0), a1, Some((a2, a3)), a4 as u32),
        nr::REMOVEXATTR | nr::LREMOVEXATTR => {
            fs::sys_setxattr(fs::XattrTarget::Path(a0, n == nr::REMOVEXATTR), a1, None, 0)
        }
        nr::FREMOVEXATTR => fs::sys_setxattr(fs::XattrTarget::Fd(i0), a1, None, 0),
        nr::FSTATFS => fs::sys_fstatfs(i0, a1),
        nr::SYNC => Ok(0),
        nr::SYNCFS => fs::sys_fsync(i0),
        nr::MOUNT => fs::sys_mount(a0, a1, a2, a3, a4),
        nr::CHROOT => fs::sys_chroot(a0),
        nr::UMOUNT2 => fs::sys_umount2(a0, a1 as u32),
        nr::GETDENTS64 => fs::sys_getdents64(i0, a1, a2),
        nr::GETTIMEOFDAY => time::sys_gettimeofday(a0, a1),
        nr::GETUID | nr::GETGID | nr::GETEUID | nr::GETEGID => sys::sys_getid(n),
        nr::SETPGID => sys_setpgid(a0 as u32, a1 as u32),
        nr::GETPPID => Ok(TABLE.lock().procs.get(&current().pid()).map_or(0, |n| n.parent) as u64),
        nr::GETPGRP => sys_getpgid(0),
        nr::SETSID => sys_setsid(),
        nr::GETPGID => sys_getpgid(a0 as u32),
        nr::GETSID => sys_getsid(a0 as u32),
        nr::RT_SIGPENDING => signal::sys_rt_sigpending(a0, a1),
        nr::RT_SIGTIMEDWAIT => signal::sys_rt_sigtimedwait(a0, a1, a2, a3),
        nr::RT_SIGSUSPEND => signal::sys_rt_sigsuspend(a0, a1),
        nr::SIGALTSTACK => signal::sys_sigaltstack(a0, a1),
        nr::PRCTL => sys_prctl(a0 as u32, a1, a2),
        nr::ARCH_PRCTL => sys_arch_prctl(a0 as u32, a1),
        nr::REBOOT => sys_reboot(a0 as u32, a1 as u32, a2 as u32, a3),
        nr::GETTID => Ok(current().tid as u64),
        nr::TKILL => signal::sys_tgkill(-1, i0, a1 as u32),
        nr::TIME => time::sys_time(a0),
        nr::FUTEX => futex::sys_futex(a0, a1 as u32, a2 as u32, a3, a4, a5 as u32),
        nr::SET_TID_ADDRESS => {
            let t = current();
            t.clear_child_tid.store(a0, Ordering::Relaxed);
            Ok(t.tid as u64)
        }
        nr::CLOCK_GETTIME => time::sys_clock_gettime(a0 as u32, a1),
        nr::CLOCK_GETRES => time::sys_clock_getres(a0 as u32, a1),
        nr::CLOCK_NANOSLEEP => time::sys_clock_nanosleep(a0 as u32, a1 as u32, a2, a3),
        nr::EXIT_GROUP => lifecycle::sys_exit_group(i0),
        nr::TGKILL => signal::sys_tgkill(i0, i1, a2 as u32),
        nr::OPENAT => fs::sys_openat(i0, a1, a2 as u32, a3 as u32),
        nr::MKDIRAT => fs::sys_mkdirat(i0, a1, a2 as u32),
        nr::NEWFSTATAT => fs::sys_newfstatat(i0, a1, a2, a3 as u32),
        nr::UNLINKAT => fs::sys_unlinkat(i0, a1, a2 as u32),
        nr::MKNODAT => fs::sys_mknodat(i0, a1, a2 as u32, a3),
        nr::FCHOWNAT => fs::sys_fchownat(i0, a1, a2 as u32, a3 as u32, a4 as u32),
        nr::RENAMEAT => fs::sys_renameat2(i0, a1, a2 as i32, a3, 0),
        nr::LINKAT => fs::sys_linkat(i0, a1, a2 as i32, a3, a4 as u32),
        nr::SYMLINKAT => fs::sys_symlinkat(a0, i1, a2),
        nr::READLINKAT => fs::sys_readlinkat(i0, a1, a2, a3 as i32),
        nr::FCHMODAT => fs::sys_fchmodat(i0, a1, a2 as u32, 0),
        nr::FACCESSAT => fs::sys_faccessat(i0, a1, a2 as u32, 0),
        nr::FACCESSAT2 => fs::sys_faccessat(i0, a1, a2 as u32, a3 as u32),
        nr::UTIMENSAT => fs::sys_utimensat(i0, a1, a2, a3 as u32),
        nr::FALLOCATE => fs::sys_fallocate(i0, a1 as u32, a2 as i64, a3 as i64),
        nr::MEMFD_CREATE => fs::sys_memfd_create(a0, a1 as u32),
        nr::PREADV => fs::sys_preadv(i0, a1, a2, a3 as i64),
        nr::PWRITEV => fs::sys_pwritev(i0, a1, a2, a3 as i64),
        nr::RENAMEAT2 => fs::sys_renameat2(i0, a1, a2 as i32, a3, a4 as u32),
        nr::STATX => fs::sys_statx(i0, a1, a2 as u32, a3 as u32, a4),
        nr::CLOSE_RANGE => fs::sys_close_range(a0 as u32, a1 as u32, a2 as u32),
        nr::SET_ROBUST_LIST => {
            if a1 != 24 {
                Err(EINVAL)
            } else {
                current().robust_list.store(a0, Ordering::Relaxed);
                Ok(0)
            }
        }
        nr::GET_ROBUST_LIST => sys_get_robust_list(i0, a1, a2),
        nr::DUP3 => fs::sys_dup3(i0, i1, a2 as u32, false),
        nr::PIPE2 => fs::sys_pipe2(a0, a1 as u32),
        nr::POLL => poll::sys_poll(a0, a1, a2 as i32),
        nr::PPOLL => poll::sys_ppoll(a0, a1, a2, a3, a4),
        nr::SELECT => poll::sys_select(i0, a1, a2, a3, a4),
        nr::PSELECT6 => poll::sys_pselect6(i0, a1, a2, a3, a4, a5),
        nr::EPOLL_CREATE => poll::sys_epoll_create(i0),
        nr::EPOLL_CREATE1 => poll::sys_epoll_create1(a0 as u32),
        nr::EPOLL_CTL => poll::sys_epoll_ctl(i0, a1 as u32, a2 as i32, a3),
        nr::EPOLL_WAIT => poll::sys_epoll_wait(i0, a1, a2 as i32, a3 as i32),
        nr::EPOLL_PWAIT => poll::sys_epoll_pwait(i0, a1, a2 as i32, a3 as i32, a4, a5),
        nr::EPOLL_PWAIT2 => poll::sys_epoll_pwait2(i0, a1, a2 as i32, a3, a4, a5),
        nr::EVENTFD => poll::sys_eventfd2(a0 as u32, 0),
        nr::EVENTFD2 => poll::sys_eventfd2(a0 as u32, a1 as u32),
        nr::TIMERFD_CREATE => poll::sys_timerfd_create(a0 as u32, a1 as u32),
        nr::TIMERFD_SETTIME => poll::sys_timerfd_settime(i0, a1 as u32, a2, a3),
        nr::TIMERFD_GETTIME => poll::sys_timerfd_gettime(i0, a1),
        nr::SIGNALFD => poll::sys_signalfd4(i0, a1, a2, 0),
        nr::SIGNALFD4 => poll::sys_signalfd4(i0, a1, a2, a3 as u32),
        nr::GETRANDOM => sys_getrandom(a0, a1, a2 as u32),
        nr::GETRLIMIT => sys::sys_getrlimit(a0 as u32, a1),
        nr::SETRLIMIT => sys::sys_setrlimit(a0 as u32, a1),
        nr::PRLIMIT64 => sys::sys_prlimit64(i0, a1 as u32, a2, a3),
        nr::GETRUSAGE => sys::sys_getrusage(i0, a1),
        nr::SYSINFO => sys::sys_sysinfo(a0),
        nr::TIMES => sys::sys_times(a0),
        nr::SETUID | nr::SETGID => sys::sys_setid1(n == nr::SETGID, a0 as u32),
        nr::SETREUID | nr::SETREGID => sys::sys_setresid(n == nr::SETREGID, a0 as i32, a1 as i32, -1, true),
        nr::SETRESUID | nr::SETRESGID => sys::sys_setresid(n == nr::SETRESGID, a0 as i32, a1 as i32, a2 as i32, false),
        nr::SETGROUPS => sys::sys_setgroups(i0, a1),
        nr::SETFSUID | nr::SETFSGID => sys::sys_setfsid(n == nr::SETFSGID, a0 as u32),
        nr::GETRESUID | nr::GETRESGID => sys::sys_getresid(n == nr::GETRESGID, a0, a1, a2),
        nr::GETGROUPS => sys::sys_getgroups(i0, a1),
        nr::CAPGET => sys::sys_capget(a0, a1),
        nr::CAPSET => sys::sys_capset(a0, a1),
        nr::RT_SIGQUEUEINFO => sys::sys_rt_sigqueueinfo(i0, -1, a1 as u32, a2),
        nr::RT_TGSIGQUEUEINFO => sys::sys_rt_sigqueueinfo(i0, i1, a2 as u32, a3),
        nr::PERSONALITY => sys::sys_personality(a0 as u32),
        nr::GETPRIORITY => sys::sys_getpriority(a0 as u32, i1),
        nr::SETPRIORITY => sys::sys_setpriority(a0 as u32, i1, a2 as i32),
        nr::SCHED_SETPARAM => sys::sys_sched_setparam(i0, a1),
        nr::SCHED_GETPARAM => sys::sys_sched_getparam(i0, a1),
        nr::SCHED_SETSCHEDULER => sys::sys_sched_setscheduler(i0, a1 as i32, a2),
        nr::SCHED_GETSCHEDULER => sys::sys_sched_getscheduler(i0),
        nr::SCHED_GET_PRIORITY_MAX => sys::sys_sched_get_priority_max(a0 as u32),
        nr::SCHED_GET_PRIORITY_MIN => sys::sys_sched_get_priority_min(a0 as u32),
        nr::SCHED_SETAFFINITY => sys::sys_sched_setaffinity(i0, a1, a2),
        nr::SCHED_GETAFFINITY => sys::sys_sched_getaffinity(i0, a1, a2),
        nr::GETCPU => sys::sys_getcpu(a0, a1),
        // All memory is resident: locking is a no-op.
        nr::MLOCK | nr::MUNLOCK | nr::MLOCK2 | nr::MLOCKALL | nr::MUNLOCKALL => Ok(0),
        nr::FADVISE64 => fs::sys_fadvise64(i0, a3 as u32),
        nr::SENDFILE => fs::sys_sendfile(i0, i1, a2, a3),
        nr::COPY_FILE_RANGE => fs::sys_copy_file_range(i0, a1, a2 as i32, a3, a4, a5 as u32),
        nr::INOTIFY_INIT => fs::sys_inotify_init1(0),
        nr::INOTIFY_INIT1 => fs::sys_inotify_init1(a0 as u32),
        nr::INOTIFY_ADD_WATCH => fs::sys_inotify_add_watch(i0, a1, a2 as u32),
        nr::INOTIFY_RM_WATCH => fs::sys_inotify_rm_watch(i0, i1),
        nr::ADD_KEY => keys::sys_add_key(a0, a1, a2, a3, a4 as i32),
        nr::REQUEST_KEY => keys::sys_request_key(a0, a1, a2, a3 as i32),
        nr::KEYCTL => keys::sys_keyctl(a0 as u32, a1, a2, a3, a4),
        nr::UNSHARE => sys::sys_unshare(a0),
        nr::SETNS => sys::sys_setns(i0, a1 as u32),
        nr::SYSLOG => sys::sys_syslog(i0, a1, a2 as i32),
        nr::SETHOSTNAME => sys::sys_sethostname(a0, a1, false),
        nr::SETDOMAINNAME => sys::sys_sethostname(a0, a1, true),
        nr::IOPRIO_SET => sys::sys_ioprio_set(i0, i1, a2 as u32),
        nr::IOPRIO_GET => sys::sys_ioprio_get(i0, i1),
        // No loadable modules: the drivers are built in.
        nr::INIT_MODULE | nr::FINIT_MODULE => Err(ENOSYS),
        nr::PIVOT_ROOT => Err(EINVAL),
        _ => {
            warn_once(n);
            Err(ENOSYS)
        }
    };
    frame.rax = encode(r);
    if trace {
        log::info!("[{}] syscall {} -> {:#x}", current().tid, n, frame.rax);
    }
}

fn prot_bits(prot: u64) -> KResult<u32> {
    if prot & !7 != 0 {
        return Err(EINVAL);
    }
    let mut p = 0;
    if prot & 1 != 0 {
        p |= PROT_READ;
    }
    if prot & 2 != 0 {
        p |= PROT_WRITE | PROT_READ;
    }
    if prot & 4 != 0 {
        p |= PROT_EXEC | PROT_READ;
    }
    Ok(p)
}

const MAP_SHARED: u64 = 1;
const MAP_PRIVATE: u64 = 2;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED_NOREPLACE: u64 = 0x100000;

fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: i32, off: u64) -> KResult<u64> {
    let p = prot_bits(prot)?;
    if flags & (MAP_SHARED | MAP_PRIVATE) == 0 || !off.is_multiple_of(crate::mm::PAGE_SIZE) {
        return Err(EINVAL);
    }
    let (shared, fixed, noreplace) =
        (flags & MAP_SHARED != 0, flags & MAP_FIXED != 0, flags & MAP_FIXED_NOREPLACE != 0);
    if flags & MAP_ANONYMOUS == 0 {
        let f = current().process.files().get(fd)?;
        return mmap_file(&f, addr, len, p, shared, fixed, noreplace, off);
    }
    current().mm().mmap(addr, len, p, shared, fixed, noreplace)
}

/// mmap of an open file: tmpfs/memfd files map their shmem object,
/// /dev/zero gives anonymous memory.
#[allow(clippy::too_many_arguments)]
fn mmap_file(f: &Arc<OpenFile>, addr: u64, len: u64, p: u32, shared: bool, fixed: bool, noreplace: bool, off: u64) -> KResult<u64> {
    use crate::fs::vfs::{S_IFCHR, S_IFMT, S_IFREG, dev_major, dev_minor};
    if f.is_path_only() {
        return Err(EBADF);
    }
    if !f.readable() {
        return Err(EACCES);
    }
    let meta = f.inode.meta();
    let obj = match meta.mode() & S_IFMT {
        S_IFREG => f.inode.shmem().ok_or(ENODEV)?,
        S_IFCHR if (dev_major(meta.rdev), dev_minor(meta.rdev)) == (1, 5) => {
            return current().mm().mmap(addr, len, p, shared, fixed, noreplace);
        }
        S_IFCHR => return f.ops.mmap(f, addr, len, p, fixed, noreplace, off),
        _ => return Err(ENODEV),
    };
    let pgoff = off / crate::mm::PAGE_SIZE;
    if pgoff.checked_add(len.div_ceil(crate::mm::PAGE_SIZE)).is_none_or(|e| e > i64::MAX as u64 / crate::mm::PAGE_SIZE) {
        return Err(EOVERFLOW);
    }
    let writable = f.writable() && f.flags() & O_APPEND == 0;
    let sealed = obj.seals() & WRITE_SEALS != 0;
    if shared && p & PROT_WRITE != 0 {
        if !writable {
            return Err(EACCES);
        }
        if sealed {
            return Err(EPERM);
        }
    }
    let backing = Backing { obj, pgoff, file: Some(f.clone()) };
    current().mm().mmap_object(addr, len, p, shared, writable && !sealed, backing, fixed, noreplace)
}

fn sys_mincore(addr: u64, len: u64, vec: u64) -> KResult<u64> {
    let mm = current().mm();
    let v = mm.mincore(addr, len)?;
    mm.copy_out(vec, &v)?;
    Ok(0)
}

fn sys_mprotect(addr: u64, len: u64, prot: u64) -> KResult<u64> {
    let p = prot_bits(prot)?;
    current().mm().mprotect(addr, len, p)?;
    Ok(0)
}

fn sys_uname(buf: u64) -> KResult<u64> {
    let mut b = [0u8; 65 * 6];
    let (host, domain) = sys::uts_names();
    let fields: [&[u8]; 6] = [
        b"Linux",
        &host,
        b"6.6.0-android-pc",
        concat!("#1 SMP android-pc ", env!("CARGO_PKG_VERSION")).as_bytes(),
        b"x86_64",
        &domain,
    ];
    for (i, f) in fields.iter().enumerate() {
        b[i * 65..i * 65 + f.len()].copy_from_slice(f);
    }
    current().mm().copy_out(buf, &b)?;
    Ok(0)
}

fn sys_setpgid(pid: u32, pgid: u32) -> KResult<u64> {
    let me = current().pid();
    let pid = if pid == 0 { me } else { pid };
    let mut t = TABLE.lock();
    let node = t.procs.get_mut(&pid).ok_or(ESRCH)?;
    node.pgid = if pgid == 0 { pid } else { pgid };
    Ok(0)
}

fn sys_getpgid(pid: u32) -> KResult<u64> {
    let pid = if pid == 0 { current().pid() } else { pid };
    TABLE.lock().procs.get(&pid).map(|n| n.pgid as u64).ok_or(ESRCH)
}

fn sys_getsid(pid: u32) -> KResult<u64> {
    let pid = if pid == 0 { current().pid() } else { pid };
    TABLE.lock().procs.get(&pid).map(|n| n.sid as u64).ok_or(ESRCH)
}

fn sys_setsid() -> KResult<u64> {
    let me = current().pid();
    let mut t = TABLE.lock();
    if t.procs.values().any(|n| n.pgid == me) && t.procs[&me].pgid == me && me != 1 {
        // Already a group leader.
        return Err(EPERM);
    }
    let node = t.node(me);
    node.pgid = me;
    node.sid = me;
    Ok(me as u64)
}

const PR_GET_DUMPABLE: u32 = 3;
const PR_SET_DUMPABLE: u32 = 4;
const PR_GET_KEEPCAPS: u32 = 7;
const PR_SET_KEEPCAPS: u32 = 8;
const PR_SET_PDEATHSIG: u32 = 1;
const PR_GET_PDEATHSIG: u32 = 2;
const PR_SET_NAME: u32 = 15;
const PR_GET_SECCOMP: u32 = 21;
const PR_SET_SECCOMP: u32 = 22;
const PR_GET_SECUREBITS: u32 = 27;
const PR_SET_SECUREBITS: u32 = 28;
const PR_SET_TIMERSLACK: u32 = 29;
const PR_GET_TIMERSLACK: u32 = 30;
const PR_SET_CHILD_SUBREAPER: u32 = 36;
const PR_GET_CHILD_SUBREAPER: u32 = 37;
const PR_SET_THP_DISABLE: u32 = 41;
const PR_GET_THP_DISABLE: u32 = 42;
const PR_CAP_AMBIENT: u32 = 47;
const PR_GET_NAME: u32 = 16;
const PR_CAPBSET_READ: u32 = 23;
const PR_SET_NO_NEW_PRIVS: u32 = 38;
const PR_GET_NO_NEW_PRIVS: u32 = 39;
/// Android's anonymous VMA naming (PR_SET_VMA_ANON_NAME).
const PR_SET_VMA: u32 = 0x53564d41;
/// CAP_CHECKPOINT_RESTORE, the highest capability.
const CAP_LAST_CAP: u64 = 40;

fn sys_prctl(option: u32, arg: u64, arg3: u64) -> KResult<u64> {
    let t = current();
    match option {
        PR_GET_DUMPABLE => Ok(t.process.attrs.lock().dumpable as u64),
        PR_SET_DUMPABLE if arg <= 1 => {
            t.process.attrs.lock().dumpable = arg == 1;
            Ok(0)
        }
        PR_GET_KEEPCAPS => Ok(t.process.attrs.lock().keepcaps as u64),
        PR_SET_KEEPCAPS if arg <= 1 => {
            t.process.attrs.lock().keepcaps = arg == 1;
            Ok(0)
        }
        PR_CAPBSET_READ if arg <= CAP_LAST_CAP => Ok(1),
        // Capabilities are not modelled (everything may do anything): dropping
        // from the bounding set and ambient changes succeed.
        24 /* PR_CAPBSET_DROP */ if arg <= CAP_LAST_CAP => Ok(0),
        PR_CAP_AMBIENT => match arg {
            1 => Ok(0),     // IS_SET
            2..=4 => Ok(0), // RAISE, LOWER, CLEAR_ALL
            _ => Err(EINVAL),
        },
        PR_GET_SECUREBITS => Ok(t.process.attrs.lock().securebits as u64),
        PR_SET_SECUREBITS => {
            t.process.attrs.lock().securebits = arg as u32;
            Ok(0)
        }
        PR_GET_PDEATHSIG => {
            let v = t.process.attrs.lock().pdeathsig;
            t.mm().copy_out(arg, &v.to_le_bytes())?;
            Ok(0)
        }
        PR_SET_PDEATHSIG if arg <= 64 => {
            t.process.attrs.lock().pdeathsig = arg as i32;
            Ok(0)
        }
        PR_GET_TIMERSLACK => Ok(t.process.attrs.lock().timerslack),
        PR_SET_TIMERSLACK => {
            t.process.attrs.lock().timerslack = if arg == 0 { 50_000 } else { arg };
            Ok(0)
        }
        PR_GET_CHILD_SUBREAPER => {
            let v = t.process.attrs.lock().subreaper as i32;
            t.mm().copy_out(arg, &v.to_le_bytes())?;
            Ok(0)
        }
        PR_SET_CHILD_SUBREAPER => {
            t.process.attrs.lock().subreaper = arg != 0;
            Ok(0)
        }
        // seccomp filters are accepted but not applied.
        PR_GET_SECCOMP => Ok(0),
        PR_SET_SECCOMP => Ok(0),
        PR_GET_THP_DISABLE => Ok(0),
        PR_SET_THP_DISABLE => Ok(0),
        PR_GET_NO_NEW_PRIVS => Ok(t.process.attrs.lock().no_new_privs as u64),
        // Cannot be unset once set.
        PR_SET_NO_NEW_PRIVS if arg == 1 && arg3 == 0 => {
            t.process.attrs.lock().no_new_privs = true;
            Ok(0)
        }
        // PR_SET_VMA_ANON_NAME: names are not kept (maps shows the
        // region unnamed).
        PR_SET_VMA if arg == 0 => Ok(0),
        PR_SET_NAME => {
            let name = t.mm().read_cstr(arg, 4096)?;
            t.process.set_comm(&name);
            Ok(0)
        }
        PR_GET_NAME => {
            let c = *t.process.comm.lock();
            t.mm().copy_out(arg, &c)?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

const ARCH_SET_GS: u32 = 0x1001;
const ARCH_SET_FS: u32 = 0x1002;
const ARCH_GET_FS: u32 = 0x1003;
const ARCH_GET_GS: u32 = 0x1004;
/// Highest user address + 1 accepted as an FS/GS base (Linux TASK_SIZE_MAX).
const TASK_SIZE_MAX: u64 = 0x7fff_ffff_f000;

fn sys_arch_prctl(code: u32, addr: u64) -> KResult<u64> {
    use x86_64::VirtAddr;
    use x86_64::registers::model_specific::{FsBase, KernelGsBase};
    let t = current();
    match code {
        ARCH_SET_FS | ARCH_SET_GS => {
            if addr >= TASK_SIZE_MAX {
                return Err(EPERM);
            }
            if code == ARCH_SET_FS {
                t.fs_base.store(addr, Ordering::Relaxed);
                FsBase::write(VirtAddr::new(addr));
            } else {
                // In the kernel the user GS base sits in KERNEL_GS_BASE.
                t.gs_base.store(addr, Ordering::Relaxed);
                KernelGsBase::write(VirtAddr::new(addr));
            }
            Ok(0)
        }
        ARCH_GET_FS | ARCH_GET_GS => {
            // Live values: user code may have changed them with WRFSBASE.
            let v = if code == ARCH_GET_FS { FsBase::read() } else { KernelGsBase::read() };
            t.mm().copy_out(addr, &v.as_u64().to_le_bytes())?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

fn sys_get_robust_list(pid: i32, head: u64, len: u64) -> KResult<u64> {
    let t = if pid == 0 {
        current()
    } else {
        TABLE.lock().tasks.get(&(pid as u32)).cloned().ok_or(ESRCH)?
    };
    let mm = current().mm();
    mm.copy_out(head, &t.robust_list.load(Ordering::Relaxed).to_le_bytes())?;
    mm.copy_out(len, &24u64.to_le_bytes())?;
    Ok(0)
}

fn sys_getrandom(buf: u64, len: u64, flags: u32) -> KResult<u64> {
    if flags & !7 != 0 {
        return Err(EINVAL);
    }
    let n = (len as usize).min(1 << 20);
    let mut b = alloc::vec![0u8; n];
    crate::random::fill(&mut b);
    current().mm().copy_out(buf, &b)?;
    Ok(n as u64)
}

const REBOOT_MAGIC1: u32 = 0xfee1_dead;
const REBOOT_MAGIC2: [u32; 4] = [672274793, 85072278, 369367448, 537993216];
const CMD_RESTART: u32 = 0x0123_4567;
const CMD_HALT: u32 = 0xcdef_0123;
const CMD_POWER_OFF: u32 = 0x4321_fedc;
const CMD_RESTART2: u32 = 0xa1b2_c3d4;
const CMD_CAD_ON: u32 = 0x89ab_cdef;
const CMD_CAD_OFF: u32 = 0;

fn sys_reboot(magic1: u32, magic2: u32, cmd: u32, arg: u64) -> KResult<u64> {
    if magic1 != REBOOT_MAGIC1 || !REBOOT_MAGIC2.contains(&magic2) {
        return Err(EINVAL);
    }
    match cmd {
        CMD_POWER_OFF | CMD_HALT => {
            log::info!("reboot(2): power off requested by pid {}", current().pid());
            crate::arch::power_off()
        }
        CMD_RESTART => {
            log::info!("reboot(2): restart requested by pid {}", current().pid());
            crate::arch::reboot()
        }
        CMD_RESTART2 => {
            let reason = current().mm().read_cstr(arg, 256).unwrap_or_default();
            log::info!(
                "reboot(2): restart ({}) requested by pid {}",
                core::str::from_utf8(&reason).unwrap_or("?"),
                current().pid()
            );
            crate::arch::reboot()
        }
        CMD_CAD_ON | CMD_CAD_OFF => Ok(0),
        _ => Err(EINVAL),
    }
}
