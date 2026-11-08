//! Block devices.
//!
//! A [`BlockDev`] is what `/dev/block/<name>` opens and what disk file
//! systems read from: a name, a device number, a read-only flag, a
//! [`Backend`] doing the actual I/O and a page cache ([`cache::Cache`]) in
//! front of it. Backends: virtio-blk disks (`drivers::virtio::blk`), GPT
//! partitions ([`gpt`]), loop devices ([`loop_dev`]) and device-mapper
//! devices ([`dm`]). Stacked devices (partition, dm, loop) call the lower
//! device's backend directly, so each layer has its own cache, as on
//! Linux.
//!
//! Registering a device creates its node in devtmpfs (`/dev/block/<name>`,
//! plus `/dev/block/by-name/<partname>` for named GPT partitions) and its
//! sysfs directory (`/sys/block/<disk>[/<part>]` with dev, size, ro, start,
//! partition, uevent, queue/*; `/sys/class/block/<name>` and
//! `/sys/dev/block/<maj>:<min>` symlinks).
//!
//! Device numbers: virtio disks 254:(16 * disk + part) with partitions past
//! 15 on 259:n (Linux's blkext), loop 7:n, device-mapper 253:n. Control
//! devices: /dev/loop-control 10:237, /dev/device-mapper (and
//! /dev/mapper/control) 10:236.

pub mod cache;
pub mod dm;
pub mod gpt;
pub mod loop_dev;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::tmpfs::{Data, TmpInode};
use crate::fs::vfs::{self, Inode, S_IFBLK, S_IFCHR, S_IFDIR, S_IFLNK, S_IFREG, encode_dev};

pub const SECTOR: u64 = 512;

pub const VIRTBLK_MAJOR: u32 = 254;
pub const DM_MAJOR: u32 = 253;
pub const BLKEXT_MAJOR: u32 = 259;
pub const LOOP_MAJOR: u32 = 7;
pub const MISC_MAJOR: u32 = 10;
pub const DM_CONTROL_MINOR: u32 = 236;
pub const LOOP_CONTROL_MINOR: u32 = 237;

/// What a block device does. Offsets and lengths are multiples of 512.
pub trait Backend: Send + Sync + Any {
    /// Size in bytes.
    fn size(&self) -> u64;
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()>;
    fn write(&self, off: u64, data: &[u8]) -> KResult<()>;
    fn flush(&self) -> KResult<()> {
        Ok(())
    }
    /// Read-only hardware (or backing file).
    fn read_only(&self) -> bool {
        false
    }
    /// Device-specific ioctls (loop, dm); ENOTTY otherwise.
    fn ioctl(&self, _dev: &Arc<BlockDev>, _cmd: u32, _arg: u64) -> KResult<u64> {
        Err(ENOTTY)
    }
    fn as_any(&self) -> &dyn Any;
}

pub struct BlockDev {
    pub name: String,
    pub major: u32,
    pub minor: u32,
    /// BLKROSET (in addition to the backend's own read-only state).
    ro: AtomicBool,
    pub backend: Arc<dyn Backend>,
    pub cache: cache::Cache,
    /// Partitions: the whole disk, partition number, first sector.
    pub parent: Option<Weak<BlockDev>>,
    pub partno: u32,
    pub start_sector: u64,
    /// GPT partition name (PARTNAME).
    pub partname: Option<String>,
    /// Open file descriptions (BLKRRPART / LOOP_CLR_FD / DM remove checks).
    pub opens: AtomicU32,
    /// Holders: mounted file systems and dm tables using this device.
    pub holders: AtomicU32,
}

impl BlockDev {
    pub fn new(name: String, major: u32, minor: u32, backend: Arc<dyn Backend>) -> BlockDev {
        BlockDev {
            name,
            major,
            minor,
            ro: AtomicBool::new(false),
            backend,
            cache: cache::Cache::new(),
            parent: None,
            partno: 0,
            start_sector: 0,
            partname: None,
            opens: AtomicU32::new(0),
            holders: AtomicU32::new(0),
        }
    }

    pub fn rdev(&self) -> u64 {
        encode_dev(self.major, self.minor)
    }

    pub fn size(&self) -> u64 {
        self.backend.size()
    }

    pub fn read_only(&self) -> bool {
        self.ro.load(Ordering::Relaxed) || self.backend.read_only()
    }

    pub fn set_read_only(&self, ro: bool) {
        self.ro.store(ro, Ordering::Relaxed);
        sysfs_set(&self.sysfs_path(), "ro", &format!("{}\n", self.read_only() as u8));
    }

    /// Cached read of any byte range inside the device.
    pub fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        self.cache.read(&*self.backend, off, buf)
    }

    /// Uncached read (file data headed for a file's own page cache);
    /// `off` and `buf.len()` must be multiples of 512.
    pub fn read_direct(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        if off % SECTOR != 0 || buf.len() as u64 % SECTOR != 0 {
            return self.read(off, buf);
        }
        let end = off.checked_add(buf.len() as u64).ok_or(EINVAL)?;
        if end > self.size() {
            return Err(EIO);
        }
        self.backend.read(off, buf)
    }

    /// Write-through to the backend, updating cached pages.
    pub fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        if self.read_only() {
            return Err(EPERM);
        }
        self.cache.write(&*self.backend, off, data)
    }

    /// The directory of this device below /sys (without the /sys).
    pub fn sysfs_path(&self) -> String {
        match self.parent.as_ref().and_then(Weak::upgrade) {
            Some(p) => format!("block/{}/{}", p.name, self.name),
            None => format!("block/{}", self.name),
        }
    }

    fn uevent(&self) -> String {
        let mut s = format!(
            "MAJOR={}\nMINOR={}\nDEVNAME={}\nDEVTYPE={}\n",
            self.major,
            self.minor,
            self.name,
            if self.parent.is_some() { "partition" } else { "disk" }
        );
        if self.parent.is_some() {
            s += &format!("PARTN={}\n", self.partno);
            if let Some(n) = &self.partname {
                s += &format!("PARTNAME={n}\n");
            }
        }
        s
    }

    /// Count a holder (a mounted file system) until the guard is dropped.
    pub fn hold(self: &Arc<Self>) -> Holder {
        self.holders.fetch_add(1, Ordering::Relaxed);
        Holder(self.clone())
    }

    /// Rewrite the sysfs "size" file (loop devices change size).
    pub fn update_sysfs_size(&self) {
        sysfs_set(&self.sysfs_path(), "size", &format!("{}\n", self.size() / SECTOR));
    }
}

/// A file system's claim on its block device (see `BlockDev::hold`).
pub struct Holder(Arc<BlockDev>);

impl Drop for Holder {
    fn drop(&mut self) {
        self.0.holders.fetch_sub(1, Ordering::Relaxed);
        loop_dev::maybe_release(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Registry

static DEVICES: Mutex<BTreeMap<(u32, u32), Arc<BlockDev>>> = Mutex::new(BTreeMap::new());
static NEXT_EXT_MINOR: AtomicU32 = AtomicU32::new(0);

pub fn alloc_ext_minor() -> u32 {
    NEXT_EXT_MINOR.fetch_add(1, Ordering::Relaxed)
}

pub fn lookup_dev(major: u32, minor: u32) -> Option<Arc<BlockDev>> {
    DEVICES.lock().get(&(major, minor)).cloned()
}

pub fn lookup_rdev(rdev: u64) -> Option<Arc<BlockDev>> {
    lookup_dev(vfs::dev_major(rdev), vfs::dev_minor(rdev))
}

pub fn all() -> Vec<Arc<BlockDev>> {
    DEVICES.lock().values().cloned().collect()
}

/// Make `dev` visible: device table, /dev/block, /sys/block.
pub fn register(dev: Arc<BlockDev>) {
    DEVICES.lock().insert((dev.major, dev.minor), dev.clone());
    let dir = devtmpfs_dir(b"block");
    if let Some(d) = &dir {
        let _ = d.replace(dev.name.as_bytes(), S_IFBLK | 0o600, dev.rdev(), None);
        if let Some(n) = &dev.partname
            && let Some(byname) = mkdir(d, b"by-name")
            && byname.get(n.as_bytes()).is_none()
        {
            let target = format!("/dev/block/{}", dev.name);
            let _ = byname.add(n.as_bytes(), S_IFLNK | 0o777, 0, Some(Data::Owned(target.into_bytes())));
        }
    }
    let path = dev.sysfs_path();
    sysfs_set(&path, "dev", &format!("{}:{}\n", dev.major, dev.minor));
    sysfs_set(&path, "size", &format!("{}\n", dev.size() / SECTOR));
    sysfs_set(&path, "ro", &format!("{}\n", dev.read_only() as u8));
    sysfs_set(&path, "uevent", &dev.uevent());
    if dev.parent.is_some() {
        sysfs_set(&path, "partition", &format!("{}\n", dev.partno));
        sysfs_set(&path, "start", &format!("{}\n", dev.start_sector));
    } else {
        sysfs_set(&path, "removable", "0\n");
        sysfs_set(&path, "range", "16\n");
        let q = format!("{path}/queue");
        for (f, v) in [
            ("logical_block_size", "512"),
            ("physical_block_size", "512"),
            ("hw_sector_size", "512"),
            ("minimum_io_size", "512"),
            ("rotational", "0"),
            ("read_ahead_kb", "128"),
            ("discard_granularity", "0"),
            ("discard_max_bytes", "0"),
        ] {
            sysfs_set(&q, f, &format!("{v}\n"));
        }
    }
    let rel = format!("../../{path}");
    sysfs_symlink("class/block", &dev.name, &rel);
    sysfs_symlink("dev/block", &format!("{}:{}", dev.major, dev.minor), &rel);
    log::info!(
        "block: {} {}:{} {} MiB{}{}",
        dev.name,
        dev.major,
        dev.minor,
        dev.size() >> 20,
        if dev.read_only() { " ro" } else { "" },
        dev.partname.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
    );
}

/// Remove `dev` from the table, /dev and /sys.
pub fn unregister(dev: &BlockDev) {
    DEVICES.lock().remove(&(dev.major, dev.minor));
    if let Some(d) = devtmpfs_dir(b"block") {
        let _ = d.unlink(dev.name.as_bytes(), false);
        if let Some(n) = &dev.partname
            && let Some(byname) = d.get(b"by-name")
        {
            let _ = byname.unlink(n.as_bytes(), false);
        }
    }
    sysfs_remove(&dev.sysfs_path());
    sysfs_remove(&format!("class/block/{}", dev.name));
    sysfs_remove(&format!("dev/block/{}:{}", dev.major, dev.minor));
}

/// The partitions registered on `disk`.
pub fn partitions_of(disk: &Arc<BlockDev>) -> Vec<Arc<BlockDev>> {
    all()
        .into_iter()
        .filter(|d| d.parent.as_ref().and_then(Weak::upgrade).is_some_and(|p| Arc::ptr_eq(&p, disk)))
        .collect()
}

/// Register a whole disk and its GPT partitions.
pub fn add_disk(disk: Arc<BlockDev>) {
    register(disk.clone());
    scan_partitions(&disk);
}

/// Read the partition table of `disk` and register what it lists (minor
/// numbers follow the disk's: +1..15, then blkext).
pub fn scan_partitions(disk: &Arc<BlockDev>) {
    let parts = match gpt::parse(disk) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("block: {}: bad partition table ({e:?})", disk.name);
            return;
        }
    };
    for p in parts {
        let (major, minor) = if p.number < 16 && disk.major == VIRTBLK_MAJOR {
            (disk.major, disk.minor + p.number)
        } else {
            (BLKEXT_MAJOR, alloc_ext_minor())
        };
        let name = if disk.name.ends_with(|c: char| c.is_ascii_digit()) {
            format!("{}p{}", disk.name, p.number)
        } else {
            format!("{}{}", disk.name, p.number)
        };
        let backend = Arc::new(gpt::Partition {
            disk: disk.backend.clone(),
            start: p.first_lba * SECTOR,
            len: (p.last_lba + 1 - p.first_lba) * SECTOR,
        });
        let mut d = BlockDev::new(name, major, minor, backend);
        d.parent = Some(Arc::downgrade(disk));
        d.partno = p.number;
        d.start_sector = p.first_lba;
        d.partname = (!p.name.is_empty()).then_some(p.name);
        d.ro.store(disk.read_only(), Ordering::Relaxed);
        register(Arc::new(d));
    }
}

// ---------------------------------------------------------------------------
// devtmpfs and sysfs helpers

fn mkdir(dir: &Arc<TmpInode>, name: &[u8]) -> Option<Arc<TmpInode>> {
    match dir.get(name) {
        Some(d) => Some(d),
        None => dir.add(name, S_IFDIR | 0o755, 0, None).ok(),
    }
}

/// Walk (creating) `path` below `root`.
fn mkdir_p(root: &Arc<TmpInode>, path: &str) -> Option<Arc<TmpInode>> {
    let mut d = root.clone();
    for c in path.split('/').filter(|c| !c.is_empty()) {
        d = mkdir(&d, c.as_bytes())?;
    }
    Some(d)
}

/// A directory in devtmpfs (created if needed).
fn devtmpfs_dir(name: &[u8]) -> Option<Arc<TmpInode>> {
    mkdir(&crate::fs::devtmpfs_root(), name)
}

/// Create the block-related nodes of a new devtmpfs.
pub fn populate_devtmpfs(root: &Arc<TmpInode>) {
    let ctl = encode_dev(MISC_MAJOR, DM_CONTROL_MINOR);
    let _ = root.add(b"device-mapper", S_IFCHR | 0o600, ctl, None);
    if let Some(m) = mkdir(root, b"mapper") {
        let _ = m.add(b"control", S_IFCHR | 0o600, ctl, None);
    }
    let _ = root.add(b"loop-control", S_IFCHR | 0o660, encode_dev(MISC_MAJOR, LOOP_CONTROL_MINOR), None);
    let _ = mkdir(root, b"block");
}

/// Write the sysfs file `<dir>/<name>` (creating directories).
pub fn sysfs_set(dir: &str, name: &str, contents: &str) {
    if let Some(d) = mkdir_p(&crate::fs::sysfs_root(), dir) {
        let _ = d.replace(name.as_bytes(), S_IFREG | 0o444, 0, Some(Data::Owned(contents.as_bytes().to_vec())));
    }
}

fn sysfs_symlink(dir: &str, name: &str, target: &str) {
    if let Some(d) = mkdir_p(&crate::fs::sysfs_root(), dir) {
        let _ = d.replace(name.as_bytes(), S_IFLNK | 0o777, 0, Some(Data::Owned(target.as_bytes().to_vec())));
    }
}

/// Remove a sysfs file or directory tree.
pub fn sysfs_remove(path: &str) {
    let root = crate::fs::sysfs_root();
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (mkdir_p(&root, d), n),
        None => (Some(root), path),
    };
    let Some(dir) = dir else { return };
    remove_tree(&dir, name.as_bytes());
}

fn remove_tree(dir: &Arc<TmpInode>, name: &[u8]) {
    if let Some(child) = dir.get(name)
        && child.meta().is_dir()
        && let Ok(entries) = child.entries()
    {
        for e in entries {
            remove_tree(&child, &e.name);
        }
    }
    let is_dir = dir.get(name).is_some_and(|c| c.meta().is_dir());
    let _ = dir.unlink(name, is_dir);
}

/// /proc/partitions.
pub fn proc_partitions() -> String {
    let mut s = String::from("major minor  #blocks  name\n\n");
    for d in all() {
        s += &format!("{:4} {:7} {:10} {}\n", d.major, d.minor, d.size() / 1024, d.name);
    }
    s
}

// ---------------------------------------------------------------------------
// Opening /dev/block nodes

pub const BLKROSET: u32 = 0x125d;
pub const BLKROGET: u32 = 0x125e;
pub const BLKRRPART: u32 = 0x125f;
pub const BLKGETSIZE: u32 = 0x1260;
pub const BLKFLSBUF: u32 = 0x1261;
pub const BLKRASET: u32 = 0x1262;
pub const BLKRAGET: u32 = 0x1263;
pub const BLKSSZGET: u32 = 0x1268;
pub const BLKBSZGET: u32 = 0x8008_1270;
pub const BLKBSZSET: u32 = 0x4008_1271;
pub const BLKGETSIZE64: u32 = 0x8008_1272;
pub const BLKDISCARD: u32 = 0x1277;
pub const BLKIOMIN: u32 = 0x1278;
pub const BLKIOOPT: u32 = 0x1279;
pub const BLKALIGNOFF: u32 = 0x127a;
pub const BLKPBSZGET: u32 = 0x127b;
pub const BLKDISCARDZEROES: u32 = 0x127c;
pub const BLKSECDISCARD: u32 = 0x127d;
pub const BLKROTATIONAL: u32 = 0x127e;
pub const BLKZEROOUT: u32 = 0x127f;
pub const BLKGETDISKSEQ: u32 = 0x8008_1280;

/// An open block device node.
pub struct BlockFile {
    pub dev: Arc<BlockDev>,
}

impl Drop for BlockFile {
    fn drop(&mut self) {
        self.dev.opens.fetch_sub(1, Ordering::Relaxed);
        loop_dev::maybe_release(&self.dev);
    }
}

/// File operations for opening block device `rdev`.
pub fn open_blk(rdev: u64, flags: u32) -> KResult<Box<dyn FileOps>> {
    let dev = lookup_rdev(rdev).ok_or(ENXIO)?;
    if flags & crate::fs::file::O_ACCMODE != crate::fs::file::O_RDONLY && dev.read_only() {
        return Err(EROFS);
    }
    dev.opens.fetch_add(1, Ordering::Relaxed);
    Ok(Box::new(BlockFile { dev }))
}

fn put_user<T: Copy>(arg: u64, v: T) -> KResult<u64> {
    let bytes = unsafe { core::slice::from_raw_parts(&v as *const T as *const u8, core::mem::size_of::<T>()) };
    crate::process::current().mm().copy_out(arg, bytes)?;
    Ok(0)
}

fn get_user<T: Copy + Default>(arg: u64) -> KResult<T> {
    let mut v = T::default();
    let bytes = unsafe { core::slice::from_raw_parts_mut(&mut v as *mut T as *mut u8, core::mem::size_of::<T>()) };
    crate::process::current().mm().copy_in(arg, bytes)?;
    Ok(v)
}

impl FileOps for BlockFile {
    fn read(&self, _: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let size = self.dev.size();
        if *pos >= size {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(size - *pos) as usize;
        // Bounded chunks keep kernel buffers small for huge reads.
        let mut done = 0;
        while done < n {
            let m = (n - done).min(1 << 20);
            self.dev.read(*pos + done as u64, &mut buf[done..done + m])?;
            done += m;
        }
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, _: &OpenFile, pos: &mut u64, data: &[u8]) -> KResult<usize> {
        let size = self.dev.size();
        if *pos >= size {
            return if data.is_empty() { Ok(0) } else { Err(ENOSPC) };
        }
        let n = (data.len() as u64).min(size - *pos) as usize;
        self.dev.write(*pos, &data[..n])?;
        *pos += n as u64;
        Ok(n)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.dev.size()
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let d = &self.dev;
        match cmd {
            BLKGETSIZE64 => put_user(arg, d.size()),
            BLKGETSIZE => put_user(arg, d.size() / SECTOR),
            BLKSSZGET | BLKPBSZGET | BLKIOMIN => put_user(arg, SECTOR as u32),
            BLKBSZGET => put_user(arg, 4096u32),
            BLKBSZSET => {
                let v: u32 = get_user(arg)?;
                if !v.is_power_of_two() || !(512..=4096).contains(&v) { Err(EINVAL) } else { Ok(0) }
            }
            BLKIOOPT | BLKALIGNOFF | BLKDISCARDZEROES | BLKROTATIONAL => put_user(arg, 0u32),
            BLKROGET => put_user(arg, d.read_only() as i32),
            BLKROSET => {
                let v: i32 = get_user(arg)?;
                if v == 0 && d.backend.read_only() {
                    return Err(EACCES);
                }
                d.set_read_only(v != 0);
                Ok(0)
            }
            BLKFLSBUF => {
                d.backend.flush()?;
                d.cache.invalidate();
                Ok(0)
            }
            BLKRAGET => put_user(arg, 256u64),
            BLKRASET => Ok(0),
            BLKDISCARD | BLKSECDISCARD | BLKZEROOUT => {
                let r: [u64; 2] = get_user(arg)?;
                if r[0] % SECTOR != 0 || r[1] % SECTOR != 0 || r[0].checked_add(r[1]).is_none_or(|e| e > d.size()) {
                    return Err(EINVAL);
                }
                if d.read_only() {
                    return Err(EPERM);
                }
                if cmd == BLKZEROOUT {
                    let zeros = alloc::vec![0u8; 64 * 1024];
                    let mut off = r[0];
                    while off < r[0] + r[1] {
                        let n = (r[0] + r[1] - off).min(zeros.len() as u64) as usize;
                        d.write(off, &zeros[..n])?;
                        off += n as u64;
                    }
                }
                Ok(0)
            }
            BLKRRPART => {
                if d.parent.is_some() {
                    return Err(EINVAL);
                }
                let parts = partitions_of(d);
                if parts.iter().any(|p| p.opens.load(Ordering::Relaxed) > 0 || p.holders.load(Ordering::Relaxed) > 0) {
                    return Err(EBUSY);
                }
                for p in parts {
                    unregister(&p);
                }
                d.cache.invalidate();
                scan_partitions(d);
                Ok(0)
            }
            BLKGETDISKSEQ => put_user(arg, ((d.major as u64) << 20) | d.minor as u64),
            _ => d.backend.ioctl(d, cmd, arg),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Resolve a mount source / dm table device: a path to a block device
/// node, or "major:minor".
pub fn resolve(spec: &[u8]) -> KResult<Arc<BlockDev>> {
    if let Some((ma, mi)) = core::str::from_utf8(spec).ok().and_then(|s| s.split_once(':'))
        && let (Ok(ma), Ok(mi)) = (ma.parse::<u32>(), mi.parse::<u32>())
    {
        return lookup_dev(ma, mi).ok_or(ENXIO);
    }
    let cwd = crate::process::current().process.cwd();
    let p = vfs::lookup(&cwd, spec, true)?;
    let meta = p.inode().meta();
    if meta.file_type() != S_IFBLK {
        return Err(ENOTBLK);
    }
    lookup_rdev(meta.rdev).ok_or(ENXIO)
}

/// Device mapper and loop control character devices.
pub fn open_misc(minor: u32) -> KResult<Box<dyn FileOps>> {
    match minor {
        DM_CONTROL_MINOR => Ok(Box::new(dm::Control)),
        LOOP_CONTROL_MINOR => Ok(Box::new(loop_dev::Control)),
        _ => Err(ENXIO),
    }
}

/// Boot-time setup: the loop devices exist from the start (like Linux's
/// max_loop default of 8).
pub fn init() {
    loop_dev::init();
}

// ---------------------------------------------------------------------------
// A RAM disk for the in-kernel tests.

#[cfg(test)]
pub struct RamDisk {
    pub data: Mutex<Vec<u8>>,
}

#[cfg(test)]
impl Backend for RamDisk {
    fn size(&self) -> u64 {
        self.data.lock().len() as u64
    }
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let d = self.data.lock();
        buf.copy_from_slice(d.get(off as usize..off as usize + buf.len()).ok_or(EIO)?);
        Ok(())
    }
    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        let mut d = self.data.lock();
        d.get_mut(off as usize..off as usize + data.len()).ok_or(EIO)?.copy_from_slice(data);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
