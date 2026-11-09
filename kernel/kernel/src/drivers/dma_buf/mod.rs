//! dma-buf: shared buffers passed between processes and devices as fds,
//! the system dma-heap, and sync files (`sync_file`).
//!
//! A [`Buffer`] is a shmem object whose pages are all allocated up front
//! (devices get their physical addresses: virtio-gpu backing) plus the
//! anonymous inode its dma-buf files share. Exporting a buffer twice
//! while a dma-buf file for it is open returns that same file (as
//! Linux's dma_buf->file), so the fds share one description.
//!
//! A dma-buf file (`/dmabuf:NAME` in /proc/<pid>/fd) maps the buffer
//! (MAP_SHARED, the pages are shared with every other mapping and the
//! DRM driver), takes DMA_BUF_IOCTL_SYNC (no caches to manage: a no-op
//! after argument checks), DMA_BUF_SET_NAME and the sync-file export /
//! import ioctls (implicit fences are always signalled here), and always
//! polls ready. fdinfo has Linux's `size`, `count`, `exp_name` and
//! `name` lines.
//!
//! /dev/dma_heap/system (char [`HEAP_MAJOR`]:0): DMA_HEAP_IOCTL_ALLOC
//! returns a new dma-buf of the requested size.

pub mod sync_file;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;
use x86_64::PhysAddr;

use crate::errno::*;
use crate::fs::file::{FileOps, O_ACCMODE, O_CLOEXEC, O_RDWR, OpenFile};
use crate::fs::poll::{POLLIN, POLLOUT};
use crate::fs::vfs::{AnonInode, Inode};
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;
use crate::process::current;

/// Character major of /dev/dma_heap/* (Linux allocates one dynamically).
pub const HEAP_MAJOR: u32 = 249;

const DMA_BUF_IOCTL_SYNC: u32 = 0x4008_6200;
const DMA_BUF_SET_NAME_A: u32 = 0x4004_6201;
const DMA_BUF_SET_NAME_B: u32 = 0x4008_6201;
const DMA_BUF_IOCTL_EXPORT_SYNC_FILE: u32 = 0xc008_6202;
const DMA_BUF_IOCTL_IMPORT_SYNC_FILE: u32 = 0x4008_6203;
const DMA_HEAP_IOCTL_ALLOC: u32 = 0xc018_4800;
const DMA_BUF_NAME_LEN: usize = 32;

/// Largest buffer (1 GiB).
const MAX_SIZE: u64 = 1 << 30;

pub struct Buffer {
    pub shmem: Arc<Shmem>,
    /// Physical address of every page, in order.
    pub pages: Vec<PhysAddr>,
    pub size: u64,
    /// Exporter name (fdinfo `exp_name`).
    pub exporter: &'static str,
    name: Mutex<String>,
    inode: Arc<dyn Inode>,
    file: Mutex<Weak<OpenFile>>,
    /// Driver data (the DRM driver's fake mmap offset).
    pub drm_offset: Mutex<u64>,
}

impl Buffer {
    /// A zeroed buffer of `size` bytes (rounded up to whole pages).
    pub fn new(size: u64, exporter: &'static str) -> KResult<Arc<Buffer>> {
        if size == 0 || size > MAX_SIZE {
            return Err(EINVAL);
        }
        let size = size.next_multiple_of(PAGE_SIZE);
        let shmem = Shmem::new(size);
        let pages = shmem.populate().map_err(|_| ENOMEM)?.iter().map(|f| f.start_address()).collect();
        Ok(Arc::new(Buffer {
            shmem,
            pages,
            size,
            exporter,
            name: Mutex::new(String::new()),
            inode: AnonInode::new(0o600, 0),
            file: Mutex::new(Weak::new()),
            drm_offset: Mutex::new(0),
        }))
    }
}

pub struct DmaBufFile {
    pub buf: Arc<Buffer>,
}

impl FileOps for DmaBufFile {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        match cmd {
            DMA_BUF_IOCTL_SYNC => {
                let mut b = [0u8; 8];
                mm.copy_in(arg, &mut b)?;
                let flags = u64::from_le_bytes(b);
                if flags & !7 != 0 || flags & 3 == 0 {
                    return Err(EINVAL);
                }
                Ok(0)
            }
            DMA_BUF_SET_NAME_A | DMA_BUF_SET_NAME_B => {
                let s = mm.read_cstr(arg, DMA_BUF_NAME_LEN)?;
                *self.buf.name.lock() = String::from_utf8_lossy(&s).into_owned();
                Ok(0)
            }
            DMA_BUF_IOCTL_EXPORT_SYNC_FILE => {
                let mut b = [0u8; 8];
                mm.copy_in(arg, &mut b)?;
                let flags = u32::from_le_bytes(b[..4].try_into().unwrap());
                if flags & !3 != 0 || flags == 0 {
                    return Err(EINVAL);
                }
                let fd = sync_file::signalled_fd(sync_file::new_context(), 1, "dma_buf", "implicit")?;
                mm.copy_out(arg + 4, &fd.to_le_bytes())?;
                Ok(0)
            }
            DMA_BUF_IOCTL_IMPORT_SYNC_FILE => {
                let mut b = [0u8; 8];
                mm.copy_in(arg, &mut b)?;
                sync_file::get_fd(i32::from_le_bytes(b[4..].try_into().unwrap()))?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    /// lseek(SEEK_END) gives the size (how userspace learns it).
    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.buf.size
    }

    fn mmap_shmem(&self, _: &OpenFile, _shared_write: bool, pgoff: u64) -> KResult<Option<(Arc<Shmem>, u64)>> {
        Ok(Some((self.buf.shmem.clone(), pgoff)))
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        POLLIN | POLLOUT
    }

    fn link_name(&self) -> Option<Vec<u8>> {
        Some(format!("/dmabuf:{}", self.buf.name.lock()).into_bytes())
    }

    fn fdinfo(&self, f: &OpenFile) -> String {
        // count: references to the description (fd table slots, users).
        let _ = f;
        format!("size:\t{}\ncount:\t1\nexp_name:\t{}\nname:\t{}\n", self.buf.size, self.buf.exporter, self.buf.name.lock())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The dma-buf file of `buf` (the open one if any, else a new one).
pub fn export(buf: &Arc<Buffer>) -> Arc<OpenFile> {
    let mut w = buf.file.lock();
    if let Some(f) = w.upgrade() {
        return f;
    }
    let f = OpenFile::new(buf.inode.clone(), None, Box::new(DmaBufFile { buf: buf.clone() }), O_RDWR);
    *w = Arc::downgrade(&f);
    f
}

/// Export `buf` as a new fd of the current process; `flags` may hold
/// O_CLOEXEC (and an access mode, ignored: dma-bufs are always O_RDWR
/// here).
pub fn export_fd(buf: &Arc<Buffer>, flags: u32) -> KResult<i32> {
    if flags & !(O_CLOEXEC | O_ACCMODE) != 0 {
        return Err(EINVAL);
    }
    current().process.files().alloc(export(buf), flags & O_CLOEXEC != 0, 0)
}

/// The buffer behind dma-buf fd `fd` of the current process.
pub fn get_fd(fd: i32) -> KResult<Arc<Buffer>> {
    let f = current().process.files().get(fd)?;
    f.ops.as_any().downcast_ref::<DmaBufFile>().map(|d| d.buf.clone()).ok_or(EINVAL)
}

// ---------------------------------------------------------------------------
// dma-heap

struct SystemHeap;

impl FileOps for SystemHeap {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd != DMA_HEAP_IOCTL_ALLOC {
            return Err(ENOTTY);
        }
        let mm = current().mm();
        let mut b = [0u8; 24];
        mm.copy_in(arg, &mut b)?;
        let len = u64::from_le_bytes(b[..8].try_into().unwrap());
        let fd_flags = u32::from_le_bytes(b[12..16].try_into().unwrap());
        let heap_flags = u64::from_le_bytes(b[16..24].try_into().unwrap());
        if heap_flags != 0 || fd_flags & !(O_ACCMODE | O_CLOEXEC) != 0 {
            return Err(EINVAL);
        }
        let buf = Buffer::new(len, "system")?;
        let fd = export_fd(&buf, fd_flags)?;
        mm.copy_out(arg + 8, &(fd as u32).to_le_bytes())?;
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Open /dev/dma_heap/<minor>: 0 is the system heap.
pub fn open_heap(minor: u32) -> KResult<Box<dyn FileOps>> {
    match minor {
        0 => Ok(Box::new(SystemHeap)),
        _ => Err(ENXIO),
    }
}

/// Register the heap and sw_sync device nodes.
pub fn init() {
    use crate::fs::sysfs;
    use crate::fs::vfs::{S_IFCHR, S_IFDIR, encode_dev};
    sysfs::add_char_device("devices/virtual/dma_heap/system", "dma_heap", "system", "dma_heap/system", HEAP_MAJOR, 0);
    let root = crate::fs::devtmpfs_root();
    if let Ok(d) = root.add(b"dma_heap", S_IFDIR | 0o755, 0, None) {
        let _ = d.add(b"system", S_IFCHR | 0o444, encode_dev(HEAP_MAJOR, 0), None);
    }
    // sw_sync lives in debugfs on Linux; here a node in /sys/kernel/debug.
    if let Some(d) = sysfs::dir("kernel/debug/sync") {
        let rdev = encode_dev(crate::block::MISC_MAJOR, sync_file::SW_SYNC_MINOR);
        let _ = d.add(b"sw_sync", S_IFCHR | 0o600, rdev, None);
    }
}
