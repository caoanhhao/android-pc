//! Sockets. Only AF_UNIX exists so far (`unix`); every other address
//! family is refused with EAFNOSUPPORT (AF_NETLINK for uevents comes in C3).
//!
//! A socket is an open file whose `FileOps` is a [`unix::SocketFile`] and
//! whose inode is an anonymous S_IFSOCK inode (`socket:[ino]` in
//! /proc/<pid>/fd). The syscalls are in `syscall/net.rs`.

pub mod unix;

pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX: u16 = 1;
/// Highest address family number + 1 (Linux AF_MAX).
pub const AF_MAX: u32 = 46;

pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_RAW: u32 = 3;
pub const SOCK_SEQPACKET: u32 = 5;
/// Highest socket type + 1 (Linux SOCK_MAX).
pub const SOCK_MAX: u32 = 11;
pub const SOCK_TYPE_MASK: u32 = 0xf;
pub const SOCK_NONBLOCK: u32 = 0o4000;
pub const SOCK_CLOEXEC: u32 = 0o2000000;

pub const MSG_OOB: u32 = 1;
pub const MSG_PEEK: u32 = 2;
pub const MSG_CTRUNC: u32 = 8;
pub const MSG_TRUNC: u32 = 0x20;
pub const MSG_DONTWAIT: u32 = 0x40;
pub const MSG_WAITALL: u32 = 0x100;
pub const MSG_NOSIGNAL: u32 = 0x4000;
pub const MSG_WAITFORONE: u32 = 0x10000;
pub const MSG_CMSG_CLOEXEC: u32 = 0x4000_0000;

pub const SOL_SOCKET: i32 = 1;
pub const SCM_RIGHTS: i32 = 1;
pub const SCM_CREDENTIALS: i32 = 2;

/// Process credentials as seen by SO_PEERCRED / SCM_CREDENTIALS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ucred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// What Linux reports for a socket with no peer credentials (overflow ids).
pub const NO_CRED: Ucred = Ucred { pid: 0, uid: 65534, gid: 65534 };

impl Ucred {
    /// The calling process (everything runs as root for now).
    pub fn current() -> Ucred {
        Ucred { pid: crate::process::current().pid(), uid: 0, gid: 0 }
    }

    pub fn to_bytes(self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[..4].copy_from_slice(&self.pid.to_le_bytes());
        b[4..8].copy_from_slice(&self.uid.to_le_bytes());
        b[8..].copy_from_slice(&self.gid.to_le_bytes());
        b
    }
}
