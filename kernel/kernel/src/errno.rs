//! Linux errno values. Syscalls return `-errno` in RAX.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Errno(pub i32);

pub type KResult<T> = Result<T, Errno>;

pub const EPERM: Errno = Errno(1);
pub const ENOENT: Errno = Errno(2);
pub const ESRCH: Errno = Errno(3);
pub const EINTR: Errno = Errno(4);
pub const EIO: Errno = Errno(5);
pub const ENXIO: Errno = Errno(6);
pub const E2BIG: Errno = Errno(7);
pub const ENOEXEC: Errno = Errno(8);
pub const EBADF: Errno = Errno(9);
pub const ECHILD: Errno = Errno(10);
pub const EAGAIN: Errno = Errno(11);
pub const ENOMEM: Errno = Errno(12);
pub const EACCES: Errno = Errno(13);
pub const EFAULT: Errno = Errno(14);
pub const ENOTBLK: Errno = Errno(15);
pub const EBUSY: Errno = Errno(16);
pub const EEXIST: Errno = Errno(17);
pub const EXDEV: Errno = Errno(18);
pub const ENODEV: Errno = Errno(19);
pub const ENOTDIR: Errno = Errno(20);
pub const EISDIR: Errno = Errno(21);
pub const EINVAL: Errno = Errno(22);
pub const EMFILE: Errno = Errno(24);
pub const ENOTTY: Errno = Errno(25);
pub const EFBIG: Errno = Errno(27);
pub const ENOSPC: Errno = Errno(28);
pub const ESPIPE: Errno = Errno(29);
pub const EROFS: Errno = Errno(30);
pub const EPIPE: Errno = Errno(32);
pub const EDOM: Errno = Errno(33);
pub const ERANGE: Errno = Errno(34);
pub const EDEADLK: Errno = Errno(35);
pub const ENAMETOOLONG: Errno = Errno(36);
pub const ENOSYS: Errno = Errno(38);
pub const ENETDOWN: Errno = Errno(100);
pub const EBADFD: Errno = Errno(77);
pub const ENOTEMPTY: Errno = Errno(39);
pub const ELOOP: Errno = Errno(40);
pub const ENODATA: Errno = Errno(61);
pub const EOVERFLOW: Errno = Errno(75);
pub const EPROTO: Errno = Errno(71);
pub const ELIBBAD: Errno = Errno(80);
pub const ENOTSOCK: Errno = Errno(88);
pub const EMSGSIZE: Errno = Errno(90);
pub const EPROTOTYPE: Errno = Errno(91);
pub const ENOPROTOOPT: Errno = Errno(92);
pub const EDESTADDRREQ: Errno = Errno(89);
pub const EPROTONOSUPPORT: Errno = Errno(93);
pub const ESOCKTNOSUPPORT: Errno = Errno(94);
pub const EOPNOTSUPP: Errno = Errno(95);
pub const EAFNOSUPPORT: Errno = Errno(97);
pub const EADDRINUSE: Errno = Errno(98);
pub const EADDRNOTAVAIL: Errno = Errno(99);
pub const ENETUNREACH: Errno = Errno(101);
pub const ECONNRESET: Errno = Errno(104);
pub const ENOBUFS: Errno = Errno(105);
pub const EISCONN: Errno = Errno(106);
pub const ENOTCONN: Errno = Errno(107);
pub const ECONNREFUSED: Errno = Errno(111);
pub const EALREADY: Errno = Errno(114);
pub const EINPROGRESS: Errno = Errno(115);
pub const ETIMEDOUT: Errno = Errno(110);
/// EFSCORRUPTED in Linux file systems.
pub const EUCLEAN: Errno = Errno(117);
pub const ENOKEY: Errno = Errno(126);
pub const EKEYREVOKED: Errno = Errno(128);
/// Internal: restart the syscall if the signal handler has SA_RESTART
/// (otherwise the task sees EINTR). Never reaches user space.
pub const ERESTARTSYS: Errno = Errno(512);
