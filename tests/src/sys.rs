//! Raw syscall layer: the `syscall!` macro, ABI constants and structs, and thin
//! wrappers. Wrappers return the raw kernel result (negative errno on error).

use crate::nr;
use core::arch::asm;

#[inline(always)]
pub unsafe fn syscall6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let r: isize;
    unsafe {
        asm!("syscall",
             inlateout("rax") n as isize => r,
             in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d, in("r8") e, in("r9") f,
             lateout("rcx") _, lateout("r11") _,
             options(nostack));
    }
    r
}

/// `syscall!(nr::FOO, a, b, ...)`: each argument is cast with `as usize`
/// (so negative ints sign-extend, e.g. AT_FDCWD).
#[macro_export]
macro_rules! syscall {
    ($n:expr) => { $crate::sys::syscall6($n, 0, 0, 0, 0, 0, 0) };
    ($n:expr, $a:expr) => { $crate::sys::syscall6($n, $a as usize, 0, 0, 0, 0, 0) };
    ($n:expr, $a:expr, $b:expr) => { $crate::sys::syscall6($n, $a as usize, $b as usize, 0, 0, 0, 0) };
    ($n:expr, $a:expr, $b:expr, $c:expr) => {
        $crate::sys::syscall6($n, $a as usize, $b as usize, $c as usize, 0, 0, 0)
    };
    ($n:expr, $a:expr, $b:expr, $c:expr, $d:expr) => {
        $crate::sys::syscall6($n, $a as usize, $b as usize, $c as usize, $d as usize, 0, 0)
    };
    ($n:expr, $a:expr, $b:expr, $c:expr, $d:expr, $e:expr) => {
        $crate::sys::syscall6($n, $a as usize, $b as usize, $c as usize, $d as usize, $e as usize, 0)
    };
    ($n:expr, $a:expr, $b:expr, $c:expr, $d:expr, $e:expr, $f:expr) => {
        $crate::sys::syscall6($n, $a as usize, $b as usize, $c as usize, $d as usize, $e as usize, $f as usize)
    };
}

// ---------------------------------------------------------------------------
// Constants.

pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ESRCH: i32 = 3;
pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const EBADF: i32 = 9;
pub const ECHILD: i32 = 10;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const EACCES: i32 = 13;
pub const EFAULT: i32 = 14;
pub const EBUSY: i32 = 16;
pub const EEXIST: i32 = 17;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EINVAL: i32 = 22;
pub const EMFILE: i32 = 24;
pub const ENOSPC: i32 = 28;
pub const ESPIPE: i32 = 29;
pub const EPIPE: i32 = 32;
pub const ERANGE: i32 = 34;
pub const ENOSYS: i32 = 38;
pub const ENOTEMPTY: i32 = 39;
pub const ENOTSOCK: i32 = 88;
pub const EMSGSIZE: i32 = 90;
pub const EOPNOTSUPP: i32 = 95;
pub const EADDRINUSE: i32 = 98;
pub const ENOTCONN: i32 = 107;
pub const ETIMEDOUT: i32 = 110;
pub const ECONNREFUSED: i32 = 111;

pub const O_RDONLY: i32 = 0;
pub const O_WRONLY: i32 = 1;
pub const O_RDWR: i32 = 2;
pub const O_CREAT: i32 = 0o100;
pub const O_EXCL: i32 = 0o200;
pub const O_TRUNC: i32 = 0o1000;
pub const O_APPEND: i32 = 0o2000;
pub const O_NONBLOCK: i32 = 0o4000;
pub const O_DIRECTORY: i32 = 0o200000;
pub const O_NOFOLLOW: i32 = 0o400000;
pub const O_CLOEXEC: i32 = 0o2000000;

pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: i32 = 0x100;
pub const AT_REMOVEDIR: i32 = 0x200;
pub const AT_EMPTY_PATH: i32 = 0x1000;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFSOCK: u32 = 0o140000;

pub const SEEK_SET: i32 = 0;
pub const SEEK_CUR: i32 = 1;
pub const SEEK_END: i32 = 2;

pub const F_DUPFD: i32 = 0;
pub const F_GETFD: i32 = 1;
pub const F_SETFD: i32 = 2;
pub const F_GETFL: i32 = 3;
pub const F_SETFL: i32 = 4;
pub const F_DUPFD_CLOEXEC: i32 = 1030;
pub const F_ADD_SEALS: i32 = 1033;
pub const F_GET_SEALS: i32 = 1034;
pub const FD_CLOEXEC: i32 = 1;
pub const F_SEAL_SEAL: i32 = 1;
pub const F_SEAL_SHRINK: i32 = 2;
pub const F_SEAL_GROW: i32 = 4;
pub const F_SEAL_WRITE: i32 = 8;

pub const PROT_NONE: usize = 0;
pub const PROT_READ: usize = 1;
pub const PROT_WRITE: usize = 2;
pub const PROT_EXEC: usize = 4;
pub const MAP_SHARED: usize = 1;
pub const MAP_PRIVATE: usize = 2;
pub const MAP_FIXED: usize = 0x10;
pub const MAP_ANONYMOUS: usize = 0x20;
pub const MAP_FIXED_NOREPLACE: usize = 0x100000;
pub const MREMAP_MAYMOVE: usize = 1;
pub const MADV_DONTNEED: usize = 4;

pub const MFD_CLOEXEC: u32 = 1;
pub const MFD_ALLOW_SEALING: u32 = 2;

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGABRT: i32 = 6;
pub const SIGBUS: i32 = 7;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;

pub const SA_SIGINFO: u64 = 4;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;
pub const SIG_BLOCK: i32 = 0;
pub const SIG_UNBLOCK: i32 = 1;
pub const SIG_SETMASK: i32 = 2;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

pub const CLONE_VM: usize = 0x100;
pub const CLONE_FS: usize = 0x200;
pub const CLONE_FILES: usize = 0x400;
pub const CLONE_SIGHAND: usize = 0x800;
pub const CLONE_VFORK: usize = 0x4000;
pub const CLONE_THREAD: usize = 0x10000;
pub const CLONE_SYSVSEM: usize = 0x40000;
pub const CLONE_SETTLS: usize = 0x80000;
pub const CLONE_PARENT_SETTID: usize = 0x100000;
pub const CLONE_CHILD_CLEARTID: usize = 0x200000;
pub const CLONE_CHILD_SETTID: usize = 0x0100_0000;
/// Flags bionic's pthread_create uses.
pub const CLONE_THREAD_FLAGS: usize = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD
    | CLONE_SYSVSEM | CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID;

pub const FUTEX_WAIT: i32 = 0;
pub const FUTEX_WAKE: i32 = 1;
pub const FUTEX_PRIVATE_FLAG: i32 = 128;

pub const ARCH_SET_GS: i32 = 0x1001;
pub const ARCH_SET_FS: i32 = 0x1002;
pub const ARCH_GET_FS: i32 = 0x1003;
pub const ARCH_GET_GS: i32 = 0x1004;

pub const WNOHANG: i32 = 1;

pub const CLOCK_REALTIME: i32 = 0;
pub const CLOCK_MONOTONIC: i32 = 1;
pub const CLOCK_BOOTTIME: i32 = 7;

pub const EPOLLIN: u32 = 0x1;
pub const EPOLLPRI: u32 = 0x2;
pub const EPOLLOUT: u32 = 0x4;
pub const EPOLLERR: u32 = 0x8;
pub const EPOLLHUP: u32 = 0x10;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;
pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;
pub const EPOLL_CLOEXEC: i32 = O_CLOEXEC;

pub const EFD_SEMAPHORE: i32 = 1;
pub const EFD_NONBLOCK: i32 = O_NONBLOCK;
pub const EFD_CLOEXEC: i32 = O_CLOEXEC;
pub const TFD_NONBLOCK: i32 = O_NONBLOCK;
pub const TFD_CLOEXEC: i32 = O_CLOEXEC;
pub const TFD_TIMER_ABSTIME: i32 = 1;
pub const SFD_NONBLOCK: i32 = O_NONBLOCK;
pub const SFD_CLOEXEC: i32 = O_CLOEXEC;

pub const POLLIN: i16 = 0x1;
pub const POLLPRI: i16 = 0x2;
pub const POLLOUT: i16 = 0x4;
pub const POLLERR: i16 = 0x8;
pub const POLLHUP: i16 = 0x10;
pub const POLLNVAL: i16 = 0x20;

pub const AF_UNIX: i32 = 1;
pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_SEQPACKET: i32 = 5;
pub const SOCK_NONBLOCK: i32 = O_NONBLOCK;
pub const SOCK_CLOEXEC: i32 = O_CLOEXEC;
pub const SOL_SOCKET: i32 = 1;
pub const SO_TYPE: i32 = 3;
pub const SO_PASSCRED: i32 = 16;
pub const SO_PEERCRED: i32 = 17;
pub const SCM_RIGHTS: i32 = 1;
pub const SCM_CREDENTIALS: i32 = 2;
pub const MSG_PEEK: i32 = 2;
pub const MSG_CTRUNC: i32 = 8;
pub const MSG_TRUNC: i32 = 0x20;
pub const MSG_DONTWAIT: i32 = 0x40;
pub const MSG_NOSIGNAL: i32 = 0x4000;
pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;

pub const PR_SET_NAME: i32 = 15;
pub const PR_GET_NAME: i32 = 16;

// ---------------------------------------------------------------------------
// ABI structs.

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Timeval {
    pub sec: i64,
    pub usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Itimerspec {
    pub interval: Timespec,
    pub value: Timespec,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub _pad0: i32,
    pub rdev: u64,
    pub size: i64,
    pub blksize: i64,
    pub blocks: i64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub _unused: [i64; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Sigaction {
    pub handler: usize,
    pub flags: u64,
    pub restorer: usize,
    pub mask: u64,
}

#[repr(C)]
pub struct Stack {
    pub sp: usize,
    pub flags: i32,
    pub size: usize,
}

/// The fields of siginfo_t the tests read.
#[repr(C)]
pub struct Siginfo {
    pub signo: i32,
    pub errno: i32,
    pub code: i32,
    _pad: i32,
    /// si_pid (kill) / si_addr low half (faults): union at offset 16.
    pub u0: u64,
    pub u1: u64,
    _rest: [u64; 12],
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Iovec {
    pub base: *mut u8,
    pub len: usize,
}

#[repr(C)]
pub struct Msghdr {
    pub name: *mut u8,
    pub namelen: u32,
    pub iov: *mut Iovec,
    pub iovlen: usize,
    pub control: *mut u8,
    pub controllen: usize,
    pub flags: i32,
}

#[repr(C)]
pub struct Cmsghdr {
    pub len: usize,
    pub level: i32,
    pub ty: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrUn {
    pub family: u16,
    pub path: [u8; 108],
}

impl SockaddrUn {
    /// Build an address; a leading NUL byte in `path` selects the abstract
    /// namespace. Returns the address and its length for bind/connect.
    pub fn new(path: &[u8]) -> (SockaddrUn, u32) {
        let mut a = SockaddrUn { family: AF_UNIX as u16, path: [0; 108] };
        a.path[..path.len()].copy_from_slice(path);
        let len = if path.first() == Some(&0) { 2 + path.len() } else { 2 + path.len() + 1 };
        (a, len as u32)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Ucred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

#[repr(C)]
pub struct Utsname {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

/// struct signalfd_siginfo (128 bytes); only the leading fields are named.
#[repr(C)]
pub struct SignalfdSiginfo {
    pub signo: u32,
    pub errno: i32,
    pub code: i32,
    pub pid: u32,
    pub uid: u32,
    _rest: [u8; 108],
}

impl SignalfdSiginfo {
    pub fn zeroed() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// Bytes of a NUL-terminated fixed array.
pub fn cfield(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

// ---------------------------------------------------------------------------
// Wrappers. Pointers are passed raw; callers own their validity.

pub fn read(fd: i32, b: &mut [u8]) -> isize {
    unsafe { syscall!(nr::READ, fd, b.as_mut_ptr(), b.len()) }
}
pub fn write(fd: i32, b: &[u8]) -> isize {
    unsafe { syscall!(nr::WRITE, fd, b.as_ptr(), b.len()) }
}
pub fn pread(fd: i32, b: &mut [u8], off: i64) -> isize {
    unsafe { syscall!(nr::PREAD64, fd, b.as_mut_ptr(), b.len(), off) }
}
pub fn pwrite(fd: i32, b: &[u8], off: i64) -> isize {
    unsafe { syscall!(nr::PWRITE64, fd, b.as_ptr(), b.len(), off) }
}
pub fn close(fd: i32) -> isize {
    unsafe { syscall!(nr::CLOSE, fd) }
}
pub fn openat(dirfd: i32, path: *const u8, flags: i32, mode: u32) -> isize {
    unsafe { syscall!(nr::OPENAT, dirfd, path, flags, mode) }
}
pub fn open(path: *const u8, flags: i32, mode: u32) -> isize {
    openat(AT_FDCWD, path, flags, mode)
}
pub fn lseek(fd: i32, off: i64, whence: i32) -> isize {
    unsafe { syscall!(nr::LSEEK, fd, off, whence) }
}
pub fn fstat(fd: i32, st: &mut Stat) -> isize {
    unsafe { syscall!(nr::FSTAT, fd, st as *mut Stat) }
}
pub fn fstatat(dirfd: i32, path: *const u8, st: &mut Stat, flags: i32) -> isize {
    unsafe { syscall!(nr::NEWFSTATAT, dirfd, path, st as *mut Stat, flags) }
}
pub fn mkdirat(dirfd: i32, path: *const u8, mode: u32) -> isize {
    unsafe { syscall!(nr::MKDIRAT, dirfd, path, mode) }
}
pub fn unlinkat(dirfd: i32, path: *const u8, flags: i32) -> isize {
    unsafe { syscall!(nr::UNLINKAT, dirfd, path, flags) }
}
pub fn ftruncate(fd: i32, len: i64) -> isize {
    unsafe { syscall!(nr::FTRUNCATE, fd, len) }
}
pub fn dup(fd: i32) -> isize {
    unsafe { syscall!(nr::DUP, fd) }
}
pub fn dup3(old: i32, new: i32, flags: i32) -> isize {
    unsafe { syscall!(nr::DUP3, old, new, flags) }
}
pub fn fcntl(fd: i32, cmd: i32, arg: usize) -> isize {
    unsafe { syscall!(nr::FCNTL, fd, cmd, arg) }
}
pub fn pipe2(fds: &mut [i32; 2], flags: i32) -> isize {
    unsafe { syscall!(nr::PIPE2, fds.as_mut_ptr(), flags) }
}
pub fn getdents64(fd: i32, b: &mut [u8]) -> isize {
    unsafe { syscall!(nr::GETDENTS64, fd, b.as_mut_ptr(), b.len()) }
}

pub fn exit(code: i32) -> ! {
    unsafe {
        syscall!(nr::EXIT_GROUP, code);
        core::hint::unreachable_unchecked()
    }
}
/// Thread exit (`exit`, not `exit_group`).
pub fn exit_thread(code: i32) -> ! {
    unsafe {
        syscall!(nr::EXIT, code);
        core::hint::unreachable_unchecked()
    }
}
pub fn getpid() -> i32 {
    unsafe { syscall!(nr::GETPID) as i32 }
}
pub fn getppid() -> i32 {
    unsafe { syscall!(nr::GETPPID) as i32 }
}
pub fn gettid() -> i32 {
    unsafe { syscall!(nr::GETTID) as i32 }
}
/// fork as bionic/musl do it: clone(SIGCHLD).
pub fn fork() -> isize {
    unsafe { syscall!(nr::CLONE, SIGCHLD, 0, 0, 0, 0) }
}
pub fn wait4(pid: i32, status: &mut i32, options: i32) -> isize {
    unsafe { syscall!(nr::WAIT4, pid, status as *mut i32, options, 0) }
}
pub fn kill(pid: i32, sig: i32) -> isize {
    unsafe { syscall!(nr::KILL, pid, sig) }
}
pub fn tgkill(tgid: i32, tid: i32, sig: i32) -> isize {
    unsafe { syscall!(nr::TGKILL, tgid, tid, sig) }
}
pub fn execve(path: *const u8, argv: *const *const u8, envp: *const *const u8) -> isize {
    unsafe { syscall!(nr::EXECVE, path, argv, envp) }
}

pub fn sigaction(sig: i32, act: Option<&Sigaction>, old: Option<&mut Sigaction>) -> isize {
    let a = act.map_or(core::ptr::null(), |a| a as *const Sigaction);
    let o = old.map_or(core::ptr::null_mut(), |o| o as *mut Sigaction);
    unsafe { syscall!(nr::RT_SIGACTION, sig, a, o, 8) }
}
/// Install a plain `fn(sig)` handler with SA_RESTORER set.
pub fn signal(sig: i32, handler: extern "C" fn(i32), flags: u64) -> isize {
    let act = Sigaction { handler: handler as *const () as usize, flags: flags | SA_RESTORER, restorer: crate::restorer(), mask: 0 };
    sigaction(sig, Some(&act), None)
}
pub fn sigprocmask(how: i32, set: Option<u64>, old: Option<&mut u64>) -> isize {
    let s = set.as_ref().map_or(core::ptr::null(), |s| s as *const u64);
    let o = old.map_or(core::ptr::null_mut(), |o| o as *mut u64);
    unsafe { syscall!(nr::RT_SIGPROCMASK, how, s, o, 8) }
}
pub const fn sigbit(sig: i32) -> u64 {
    1 << (sig - 1)
}

pub fn mmap(addr: usize, len: usize, prot: usize, flags: usize, fd: i32, off: i64) -> isize {
    unsafe { syscall!(nr::MMAP, addr, len, prot, flags, fd, off) }
}
pub fn munmap(addr: *mut u8, len: usize) -> isize {
    unsafe { syscall!(nr::MUNMAP, addr, len) }
}
pub fn mprotect(addr: *mut u8, len: usize, prot: usize) -> isize {
    unsafe { syscall!(nr::MPROTECT, addr, len, prot) }
}
pub fn brk(addr: usize) -> usize {
    unsafe { syscall!(nr::BRK, addr) as usize }
}

pub fn futex(uaddr: *const i32, op: i32, val: i32, timeout: *const Timespec) -> isize {
    unsafe { syscall!(nr::FUTEX, uaddr, op, val, timeout, 0, 0) }
}

pub fn clock_gettime(clk: i32, ts: &mut Timespec) -> isize {
    unsafe { syscall!(nr::CLOCK_GETTIME, clk, ts as *mut Timespec) }
}
pub fn nanosleep(req: &Timespec, rem: *mut Timespec) -> isize {
    unsafe { syscall!(nr::NANOSLEEP, req as *const Timespec, rem) }
}

pub fn epoll_create1(flags: i32) -> isize {
    unsafe { syscall!(nr::EPOLL_CREATE1, flags) }
}
pub fn epoll_ctl(epfd: i32, op: i32, fd: i32, ev: u32, data: u64) -> isize {
    let e = EpollEvent { events: ev, data };
    unsafe { syscall!(nr::EPOLL_CTL, epfd, op, fd, &e as *const EpollEvent) }
}
pub fn epoll_wait(epfd: i32, evs: &mut [EpollEvent], timeout_ms: i32) -> isize {
    unsafe { syscall!(nr::EPOLL_WAIT, epfd, evs.as_mut_ptr(), evs.len(), timeout_ms) }
}
pub fn poll(fds: &mut [Pollfd], timeout_ms: i32) -> isize {
    unsafe { syscall!(nr::POLL, fds.as_mut_ptr(), fds.len(), timeout_ms) }
}

pub fn socketpair(domain: i32, ty: i32, fds: &mut [i32; 2]) -> isize {
    unsafe { syscall!(nr::SOCKETPAIR, domain, ty, 0, fds.as_mut_ptr()) }
}
pub fn socket(domain: i32, ty: i32) -> isize {
    unsafe { syscall!(nr::SOCKET, domain, ty, 0) }
}
pub fn bind(fd: i32, a: &SockaddrUn, len: u32) -> isize {
    unsafe { syscall!(nr::BIND, fd, a as *const SockaddrUn, len) }
}
pub fn listen(fd: i32, backlog: i32) -> isize {
    unsafe { syscall!(nr::LISTEN, fd, backlog) }
}
pub fn connect(fd: i32, a: &SockaddrUn, len: u32) -> isize {
    unsafe { syscall!(nr::CONNECT, fd, a as *const SockaddrUn, len) }
}
pub fn accept4(fd: i32, flags: i32) -> isize {
    unsafe { syscall!(nr::ACCEPT4, fd, 0, 0, flags) }
}
pub fn sendmsg(fd: i32, m: &Msghdr, flags: i32) -> isize {
    unsafe { syscall!(nr::SENDMSG, fd, m as *const Msghdr, flags) }
}
pub fn recvmsg(fd: i32, m: &mut Msghdr, flags: i32) -> isize {
    unsafe { syscall!(nr::RECVMSG, fd, m as *mut Msghdr, flags) }
}
pub fn send(fd: i32, b: &[u8], flags: i32) -> isize {
    unsafe { syscall!(nr::SENDTO, fd, b.as_ptr(), b.len(), flags, 0, 0) }
}
pub fn recv(fd: i32, b: &mut [u8], flags: i32) -> isize {
    unsafe { syscall!(nr::RECVFROM, fd, b.as_mut_ptr(), b.len(), flags, 0, 0) }
}
pub fn getsockopt(fd: i32, level: i32, name: i32, val: *mut u8, len: &mut u32) -> isize {
    unsafe { syscall!(nr::GETSOCKOPT, fd, level, name, val, len as *mut u32) }
}
pub fn setsockopt(fd: i32, level: i32, name: i32, val: *const u8, len: u32) -> isize {
    unsafe { syscall!(nr::SETSOCKOPT, fd, level, name, val, len) }
}
