//! Character devices and devtmpfs.
//!
//! Device nodes can live on any file system (devtmpfs, the initramfs);
//! opening one picks the driver by (major, minor) here:
//!
//! | node            | major:minor | behaviour                            |
//! |-----------------|-------------|--------------------------------------|
//! | /dev/null       | 1:3         | EOF / discards                       |
//! | /dev/zero       | 1:5         | zeros / discards                     |
//! | /dev/full       | 1:7         | zeros / ENOSPC                       |
//! | /dev/random     | 1:8         | random bytes (never blocks)          |
//! | /dev/urandom    | 1:9         | random bytes                         |
//! | /dev/kmsg       | 1:11        | writes go to the kernel log; reads EOF |
//! | /dev/tty        | 5:0         | the serial console                   |
//! | /dev/console    | 5:1         | the serial console                   |
//! | /dev/ttyS0      | 4:64        | the serial console                   |
//! | binderfs nodes  | 244:n       | binder devices (`drivers::binder`)   |
//!
//! The console is a tty with serial input and a line discipline (see
//! `fs::tty`). The memory devices have no poll method (always ready, not
//! epoll-able).

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::any::Any;

use super::file::{FileOps, OpenFile};
use super::poll::WaitQueue;
use super::tmpfs::TmpInode;
use super::vfs::{AnonInode, Inode, S_IFCHR, S_IFDIR, dev_major, dev_minor, encode_dev};
use crate::errno::*;

pub const CONSOLE_RDEV: (u32, u32) = (5, 1);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dev {
    Null,
    Zero,
    Full,
    Random,
    Kmsg,
    Console,
}

struct DevFile(Dev);

impl FileOps for DevFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        match self.0 {
            Dev::Console => super::tty::read(buf, f.nonblock()),
            Dev::Null | Dev::Kmsg => Ok(0),
            Dev::Zero | Dev::Full => {
                buf.fill(0);
                Ok(buf.len())
            }
            Dev::Random => {
                crate::random::fill(buf);
                Ok(buf.len())
            }
        }
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        match self.0 {
            Dev::Full => Err(ENOSPC),
            Dev::Console => Ok(super::tty::write(data)),
            Dev::Kmsg => {
                kmsg_write(data);
                Ok(data.len())
            }
            _ => Ok(data.len()),
        }
    }

    fn lseek_ok(&self) -> bool {
        true
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if self.0 == Dev::Console { super::tty::ioctl(cmd, arg) } else { Err(ENOTTY) }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        match self.0 {
            Dev::Console => super::tty::poll(),
            _ => super::poll::DEFAULT_POLLMASK,
        }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        (self.0 == Dev::Console).then(super::tty::wait_queue)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// /dev/kmsg write: "<prio>text" -> one kernel log line.
fn kmsg_write(data: &[u8]) {
    let mut s = data;
    if s.first() == Some(&b'<')
        && let Some(end) = s.iter().position(|&c| c == b'>')
    {
        s = &s[end + 1..];
    }
    while s.last() == Some(&b'\n') {
        s = &s[..s.len() - 1];
    }
    log::info!("kmsg: {}", core::str::from_utf8(s).unwrap_or("<binary>"));
}

// ---------------------------------------------------------------------------

/// File operations for opening a character device node.
pub fn open_chr(rdev: u64) -> KResult<Box<dyn FileOps>> {
    let d = match (dev_major(rdev), dev_minor(rdev)) {
        (1, 3) => Dev::Null,
        (1, 5) => Dev::Zero,
        (1, 7) => Dev::Full,
        (1, 8) | (1, 9) => Dev::Random,
        (1, 11) => Dev::Kmsg,
        (5, 0) | (5, 1) | (4, _) => Dev::Console,
        (crate::drivers::binder::BINDER_MAJOR, minor) => return crate::drivers::binder::open(minor),
        _ => return Err(ENXIO),
    };
    Ok(Box::new(DevFile(d)))
}

/// The boot console as an open file (fds 0-2 of init).
pub fn console() -> Arc<OpenFile> {
    let rdev = encode_dev(CONSOLE_RDEV.0, CONSOLE_RDEV.1);
    let inode: Arc<dyn Inode> = AnonInode::new(S_IFCHR | 0o600, rdev);
    OpenFile::new(inode, None, Box::new(DevFile(Dev::Console)), super::file::O_RDWR)
}

/// A new devtmpfs instance with the standard nodes.
pub fn new_devtmpfs() -> Arc<TmpInode> {
    let root = TmpInode::new_fs(0o755);
    let nodes: [(&[u8], u32, u32, u32); 9] = [
        (b"null", 0o666, 1, 3),
        (b"zero", 0o666, 1, 5),
        (b"full", 0o666, 1, 7),
        (b"random", 0o666, 1, 8),
        (b"urandom", 0o666, 1, 9),
        (b"kmsg", 0o644, 1, 11),
        (b"tty", 0o666, 5, 0),
        (b"console", 0o600, 5, 1),
        (b"ttyS0", 0o660, 4, 64),
    ];
    for (name, perm, major, minor) in nodes {
        let _ = root.add(name, S_IFCHR | perm, encode_dev(major, minor), None);
    }
    let _ = root.add(b"pts", S_IFDIR | 0o755, 0, None);
    let _ = root.add(b"shm", S_IFDIR | 0o1777, 0, None);
    root
}
