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
//! | /dev/kmsg       | 1:11        | the kernel log buffer (`fs::kmsg`)   |
//! | /dev/tty        | 5:0         | the serial console                   |
//! | /dev/console    | 5:1         | the serial console                   |
//! | /dev/ttyS0      | 4:64        | the serial console                   |
//! | /dev/ptmx, /dev/pts/N | 5:2, 136:N | pseudo-terminals (`fs::pty`)    |
//! | binderfs nodes  | 244:n       | binder devices (`drivers::binder`)   |
//! | /dev/device-mapper, /dev/mapper/control | 10:236 | dm control (`block::dm`) |
//! | /dev/loop-control | 10:237    | loop control (`block::loop_dev`)     |
//! | /dev/hw_random  | 10:183      | random bytes (hw_random, for prng_seeder) |
//! | /dev/dri/card0, renderD128 | 226:0, 226:128 | DRM (`drivers::drm`) |
//! | /dev/dma_heap/system | 249:0  | system dma-heap (`drivers::dma_buf`) |
//! | /dev/input/eventN | 13:64+N   | evdev (`drivers::input`)             |
//! | /dev/uinput     | 10:223      | uinput (`drivers::input::uinput`)    |
//! | /dev/net/tun    | 10:200      | tun links without traffic (`net::tun`) |
//! | /dev/hvc1..17   | 229:N       | host-less virtio-console stand-ins (`drivers::hvc`) |
//! | /dev/ashmem<boot_id> | 10:126 | ashmem regions (`drivers::ashmem`) |
//! | /dev/rtc0       | 252:0       | the CMOS RTC (`drivers::platform::rtc`) |
//! | /dev/userfaultfd | 10:124     | USERFAULTFD_IOC_NEW (`mm::userfaultfd`) |
//! | /sys/kernel/debug/sync/sw_sync | 10:250 | sw_sync timelines (`drivers::dma_buf::sync_file`) |
//!
//! Block device nodes (/dev/block/*) open `block::open_blk`.
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
/// The misc minor of /dev/hw_random.
pub const HWRNG_MINOR: u32 = 183;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dev {
    Null,
    Zero,
    Full,
    Random,
    Console,
}

struct DevFile(Dev);

impl FileOps for DevFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        match self.0 {
            Dev::Console => super::tty::read(buf, f.nonblock()),
            Dev::Null => Ok(0),
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

// ---------------------------------------------------------------------------

/// File operations for opening a character device node.
pub fn open_chr(rdev: u64) -> KResult<Box<dyn FileOps>> {
    let d = match (dev_major(rdev), dev_minor(rdev)) {
        (1, 3) => Dev::Null,
        (1, 5) => Dev::Zero,
        (1, 7) => Dev::Full,
        (1, 8) | (1, 9) => Dev::Random,
        (1, 11) => return Ok(Box::new(super::kmsg::KmsgDev)),
        (5, 2) => return super::pty::open_master(),
        (super::pty::PTS_MAJOR, n) => return super::pty::open_slave(n),
        (5, 0) | (5, 1) | (4, _) => Dev::Console,
        (crate::drivers::binder::BINDER_MAJOR, minor) => return crate::drivers::binder::open(minor),
        (crate::block::MISC_MAJOR, HWRNG_MINOR) => Dev::Random,
        (crate::block::MISC_MAJOR, crate::drivers::input::uinput::UINPUT_MINOR) => return crate::drivers::input::uinput::open(),
        (crate::drivers::input::INPUT_MAJOR, minor) => return crate::drivers::input::open_evdev(minor),
        (crate::block::MISC_MAJOR, crate::drivers::dma_buf::sync_file::SW_SYNC_MINOR) => {
            return crate::drivers::dma_buf::sync_file::open_sw_sync();
        }
        (crate::drivers::hvc::HVC_MAJOR, minor) => return crate::drivers::hvc::open(minor),
        (crate::drivers::platform::rtc::RTC_MAJOR, 0) => return crate::drivers::platform::rtc::open(),
        (crate::drivers::drm::DRM_MAJOR, minor) => return crate::drivers::drm::open(minor),
        (crate::drivers::dma_buf::HEAP_MAJOR, minor) => return crate::drivers::dma_buf::open_heap(minor),
        (crate::block::MISC_MAJOR, crate::net::tun::TUN_MINOR) => return crate::net::tun::open(),
        (crate::block::MISC_MAJOR, crate::drivers::ashmem::ASHMEM_MINOR) => return crate::drivers::ashmem::open(),
        (crate::block::MISC_MAJOR, crate::mm::userfaultfd::USERFAULTFD_MINOR) => return crate::mm::userfaultfd::open_dev(),
        (crate::block::MISC_MAJOR, minor) => return crate::block::open_misc(minor),
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
    let nodes: [(&[u8], u32, u32, u32); 14] = [
        (b"null", 0o666, 1, 3),
        (b"zero", 0o666, 1, 5),
        (b"full", 0o666, 1, 7),
        (b"random", 0o666, 1, 8),
        (b"urandom", 0o666, 1, 9),
        (b"kmsg", 0o644, 1, 11),
        (b"tty", 0o666, 5, 0),
        (b"console", 0o600, 5, 1),
        (b"ttyS0", 0o660, 4, 64),
        (b"ptmx", 0o666, 5, 2),
        (b"uinput", 0o660, 10, 223),
        (b"userfaultfd", 0o600, 10, 124),
        (b"ashmem", 0o666, 10, 126),
        (b"hvc1", 0o600, 229, 1),
    ];
    for (name, perm, major, minor) in nodes {
        let _ = root.add(name, S_IFCHR | perm, encode_dev(major, minor), None);
    }
    let _ = root.add(b"pts", S_IFDIR | 0o755, 0, None);
    if let Ok(net) = root.add(b"net", S_IFDIR | 0o755, 0, None)
        && let Some(net) = net.as_any().downcast_ref::<TmpInode>()
    {
        let _ = net.add(b"tun", S_IFCHR | 0o666, encode_dev(10, 200), None);
    }
    let _ = root.add(b"shm", S_IFDIR | 0o1777, 0, None);
    root
}
