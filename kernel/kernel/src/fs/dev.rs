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
//!
//! The console is a minimal tty: termios and window size ioctls work (so
//! isatty() is true); there is no serial input yet, reads return EOF.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::any::Any;
use spin::Mutex;

use super::file::{FileOps, OpenFile};
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
    fn read(&self, _: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        match self.0 {
            Dev::Null | Dev::Kmsg | Dev::Console => Ok(0),
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
            Dev::Console => {
                crate::drivers::serial::write_bytes(data);
                Ok(data.len())
            }
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
        if self.0 == Dev::Console { tty_ioctl(cmd, arg) } else { Err(ENOTTY) }
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
// Console tty ioctls

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TCFLSH: u32 = 0x540b;
const TIOCSCTTY: u32 = 0x540e;
const TIOCGPGRP: u32 = 0x540f;
const TIOCSPGRP: u32 = 0x5410;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const FIONREAD: u32 = 0x541b;
const TIOCNOTTY: u32 = 0x5422;
const TIOCGSID: u32 = 0x5429;

/// Kernel `struct termios` (36 bytes): iflag, oflag, cflag, lflag, line, cc[19].
fn default_termios() -> [u8; 36] {
    let mut t = [0u8; 36];
    let iflag: u32 = 0o400 | 0o2000; // ICRNL | IXON
    let oflag: u32 = 0o1 | 0o4; // OPOST | ONLCR
    let cflag: u32 = 0o17 | 0o60 | 0o200 | 0o2000; // B38400 | CS8 | CREAD | HUPCL
    let lflag: u32 = 0o1 | 0o2 | 0o10 | 0o20 | 0o40 | 0o1000 | 0o4000 | 0o100000;
    for (i, v) in [iflag, oflag, cflag, lflag].iter().enumerate() {
        t[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    // VINTR VQUIT VERASE VKILL VEOF VTIME VMIN VSWTC VSTART VSTOP VSUSP
    let cc = [3u8, 0x1c, 0x7f, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1a, 0, 0x12, 0x0f, 0x17, 0x16];
    t[17..17 + cc.len()].copy_from_slice(&cc);
    t
}

struct TtyState {
    termios: [u8; 36],
    winsize: [u8; 8],
    pgrp: i32,
}

static TTY: Mutex<Option<TtyState>> = Mutex::new(None);

fn with_tty<R>(f: impl FnOnce(&mut TtyState) -> R) -> R {
    let mut g = TTY.lock();
    let s = g.get_or_insert_with(|| {
        let mut winsize = [0u8; 8];
        winsize[0..2].copy_from_slice(&25u16.to_le_bytes());
        winsize[2..4].copy_from_slice(&80u16.to_le_bytes());
        TtyState { termios: default_termios(), winsize, pgrp: 1 }
    });
    f(s)
}

fn tty_ioctl(cmd: u32, arg: u64) -> KResult<u64> {
    let mm = crate::process::current().mm();
    match cmd {
        TCGETS => {
            let t = with_tty(|s| s.termios);
            mm.copy_out(arg, &t)?;
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut t = [0u8; 36];
            mm.copy_in(arg, &mut t)?;
            with_tty(|s| s.termios = t);
        }
        TIOCGWINSZ => {
            let w = with_tty(|s| s.winsize);
            mm.copy_out(arg, &w)?;
        }
        TIOCSWINSZ => {
            let mut w = [0u8; 8];
            mm.copy_in(arg, &mut w)?;
            with_tty(|s| s.winsize = w);
        }
        TIOCGPGRP => {
            let p = with_tty(|s| s.pgrp);
            mm.copy_out(arg, &p.to_le_bytes())?;
        }
        TIOCSPGRP => {
            let mut b = [0u8; 4];
            mm.copy_in(arg, &mut b)?;
            with_tty(|s| s.pgrp = i32::from_le_bytes(b));
        }
        TIOCGSID => mm.copy_out(arg, &1i32.to_le_bytes())?,
        FIONREAD => mm.copy_out(arg, &0i32.to_le_bytes())?,
        TCFLSH | TIOCSCTTY | TIOCNOTTY => {}
        _ => return Err(ENOTTY),
    }
    Ok(0)
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
