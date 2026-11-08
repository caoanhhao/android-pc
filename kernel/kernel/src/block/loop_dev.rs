//! Loop devices (`/dev/block/loopN`, 7:N) and `/dev/loop-control`.
//!
//! A bound loop device reads and writes its backing file (any open file:
//! a regular file on any file system, or another block device) at
//! `offset`, limited to `sizelimit`, rounded down to whole sectors.
//! Supported: LOOP_SET_FD, LOOP_CONFIGURE, LOOP_CLR_FD (deferred to the
//! last close / unmount with AUTOCLEAR while in use), LOOP_{GET,SET}_STATUS64,
//! LOOP_SET_CAPACITY, LOOP_SET_BLOCK_SIZE, LOOP_SET_DIRECT_IO (no-op),
//! LOOP_CHANGE_FD; LO_FLAGS_PARTSCAN scans a GPT (loopNpM on 259:x).
//! loop-control: LOOP_CTL_GET_FREE (creating a device when all are busy),
//! LOOP_CTL_ADD, LOOP_CTL_REMOVE. Eight devices exist from boot.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::Ordering;
use spin::Mutex;

use super::{Backend, BlockDev, LOOP_MAJOR, SECTOR};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};

pub const LOOP_SET_FD: u32 = 0x4c00;
pub const LOOP_CLR_FD: u32 = 0x4c01;
pub const LOOP_SET_STATUS: u32 = 0x4c02;
pub const LOOP_GET_STATUS: u32 = 0x4c03;
pub const LOOP_SET_STATUS64: u32 = 0x4c04;
pub const LOOP_GET_STATUS64: u32 = 0x4c05;
pub const LOOP_CHANGE_FD: u32 = 0x4c06;
pub const LOOP_SET_CAPACITY: u32 = 0x4c07;
pub const LOOP_SET_DIRECT_IO: u32 = 0x4c08;
pub const LOOP_SET_BLOCK_SIZE: u32 = 0x4c09;
pub const LOOP_CONFIGURE: u32 = 0x4c0a;
pub const LOOP_CTL_ADD: u32 = 0x4c80;
pub const LOOP_CTL_REMOVE: u32 = 0x4c81;
pub const LOOP_CTL_GET_FREE: u32 = 0x4c82;

pub const LO_FLAGS_READ_ONLY: u32 = 1;
pub const LO_FLAGS_AUTOCLEAR: u32 = 4;
pub const LO_FLAGS_PARTSCAN: u32 = 8;
pub const LO_FLAGS_DIRECT_IO: u32 = 16;
const SETTABLE: u32 = LO_FLAGS_AUTOCLEAR | LO_FLAGS_PARTSCAN;
const CONFIGURE_SETTABLE: u32 = LO_FLAGS_READ_ONLY | LO_FLAGS_AUTOCLEAR | LO_FLAGS_PARTSCAN | LO_FLAGS_DIRECT_IO;

const INFO_SIZE: usize = 232;
const CONFIG_SIZE: usize = 304;

struct Bound {
    file: Arc<OpenFile>,
    offset: u64,
    sizelimit: u64,
    flags: u32,
    file_name: [u8; 64],
    crypt_name: [u8; 64],
    block_size: u32,
    /// Size in bytes (recomputed by SET_CAPACITY / SET_STATUS).
    size: u64,
}

impl Bound {
    fn compute_size(&mut self) {
        let fsize = self.file.ops.size(&self.file);
        let mut s = fsize.saturating_sub(self.offset);
        if self.sizelimit != 0 {
            s = s.min(self.sizelimit);
        }
        self.size = s / SECTOR * SECTOR;
    }
}

pub struct Loop {
    pub number: u32,
    state: Mutex<Option<Bound>>,
}

impl Loop {
    fn bound(&self) -> bool {
        self.state.lock().is_some()
    }
}

impl Backend for Loop {
    fn size(&self) -> u64 {
        self.state.lock().as_ref().map_or(0, |b| b.size)
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let st = self.state.lock();
        let b = st.as_ref().ok_or(ENXIO)?;
        if off + buf.len() as u64 > b.size {
            return Err(EIO);
        }
        let mut done = 0;
        while done < buf.len() {
            let n = b.file.read(&mut buf[done..], Some(b.offset + off + done as u64))?;
            if n == 0 {
                buf[done..].fill(0);
                break;
            }
            done += n;
        }
        Ok(())
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        let st = self.state.lock();
        let b = st.as_ref().ok_or(ENXIO)?;
        if b.flags & LO_FLAGS_READ_ONLY != 0 {
            return Err(EPERM);
        }
        if off + data.len() as u64 > b.size {
            return Err(ENOSPC);
        }
        let mut done = 0;
        while done < data.len() {
            let n = b.file.write(&data[done..], Some(b.offset + off + done as u64))?;
            if n == 0 {
                return Err(EIO);
            }
            done += n;
        }
        Ok(())
    }

    fn read_only(&self) -> bool {
        self.state.lock().as_ref().is_some_and(|b| b.flags & LO_FLAGS_READ_ONLY != 0)
    }

    fn ioctl(&self, dev: &Arc<BlockDev>, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            LOOP_SET_FD => {
                let file = crate::process::current().process.files().get(arg as i32)?;
                self.bind(dev, file, 0, None)?;
                Ok(0)
            }
            LOOP_CONFIGURE => {
                let mut c = [0u8; CONFIG_SIZE];
                crate::process::current().mm().copy_in(arg, &mut c)?;
                let fd = u32::from_le_bytes(c[0..4].try_into().unwrap());
                let bs = u32::from_le_bytes(c[4..8].try_into().unwrap());
                if bs != 0 && (!bs.is_power_of_two() || !(512..=4096).contains(&bs)) {
                    return Err(EINVAL);
                }
                let info: [u8; INFO_SIZE] = c[8..8 + INFO_SIZE].try_into().unwrap();
                let file = crate::process::current().process.files().get(fd as i32)?;
                self.bind(dev, file, bs, Some(&info))?;
                Ok(0)
            }
            LOOP_CHANGE_FD => {
                let file = crate::process::current().process.files().get(arg as i32)?;
                let mut st = self.state.lock();
                let b = st.as_mut().ok_or(ENXIO)?;
                if b.flags & LO_FLAGS_READ_ONLY == 0 || file.ops.size(&file) != b.file.ops.size(&b.file) {
                    return Err(EINVAL);
                }
                b.file = file;
                drop(st);
                dev.cache.invalidate();
                Ok(0)
            }
            LOOP_CLR_FD => {
                if !self.bound() {
                    return Err(ENXIO);
                }
                if dev.opens.load(Ordering::Relaxed) > 1 || dev.holders.load(Ordering::Relaxed) > 0 {
                    // Linux defers the teardown to the last close.
                    if let Some(b) = self.state.lock().as_mut() {
                        b.flags |= LO_FLAGS_AUTOCLEAR;
                    }
                    return Ok(0);
                }
                self.unbind(dev);
                Ok(0)
            }
            LOOP_GET_STATUS64 => {
                let info = self.info()?;
                crate::process::current().mm().copy_out(arg, &info)?;
                Ok(0)
            }
            LOOP_SET_STATUS64 => {
                let mut info = [0u8; INFO_SIZE];
                crate::process::current().mm().copy_in(arg, &mut info)?;
                self.set_status(dev, &info, SETTABLE)?;
                Ok(0)
            }
            LOOP_GET_STATUS | LOOP_SET_STATUS => Err(EINVAL),
            LOOP_SET_CAPACITY => {
                let mut st = self.state.lock();
                st.as_mut().ok_or(ENXIO)?.compute_size();
                drop(st);
                dev.cache.invalidate();
                dev.update_sysfs_size();
                Ok(0)
            }
            LOOP_SET_DIRECT_IO => {
                self.state.lock().as_ref().ok_or(ENXIO)?;
                Ok(0)
            }
            LOOP_SET_BLOCK_SIZE => {
                if !arg.is_power_of_two() || !(512..=4096).contains(&arg) {
                    return Err(EINVAL);
                }
                self.state.lock().as_mut().ok_or(ENXIO)?.block_size = arg as u32;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn cstr(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

impl Loop {
    fn bind(&self, dev: &Arc<BlockDev>, file: Arc<OpenFile>, block_size: u32, info: Option<&[u8; INFO_SIZE]>) -> KResult<()> {
        if file.is_path_only() || !file.readable() {
            return Err(EBADF);
        }
        let ty = file.inode.meta().file_type();
        if ty != crate::fs::vfs::S_IFREG && ty != crate::fs::vfs::S_IFBLK {
            return Err(EINVAL);
        }
        if file.ops.as_any().downcast_ref::<super::BlockFile>().is_some_and(|b| Arc::ptr_eq(&b.dev, dev)) {
            return Err(EINVAL);
        }
        let mut name = [0u8; 64];
        let path = crate::fs::file_path(&file);
        let n = path.len().min(63);
        name[..n].copy_from_slice(&path[..n]);
        {
            let mut st = self.state.lock();
            if st.is_some() {
                return Err(EBUSY);
            }
            let ro = !file.writable();
            let mut b = Bound {
                file,
                offset: 0,
                sizelimit: 0,
                flags: if ro { LO_FLAGS_READ_ONLY } else { 0 },
                file_name: name,
                crypt_name: [0; 64],
                block_size: if block_size == 0 { 512 } else { block_size },
                size: 0,
            };
            b.compute_size();
            *st = Some(b);
        }
        if let Some(info) = info
            && let Err(e) = self.set_status(dev, info, CONFIGURE_SETTABLE)
        {
            *self.state.lock() = None;
            return Err(e);
        }
        dev.cache.invalidate();
        self.after_change(dev);
        Ok(())
    }

    fn set_status(&self, dev: &Arc<BlockDev>, info: &[u8; INFO_SIZE], settable: u32) -> KResult<()> {
        let u64_at = |o: usize| u64::from_le_bytes(info[o..o + 8].try_into().unwrap());
        let flags = u32::from_le_bytes(info[52..56].try_into().unwrap());
        let encrypt = u32::from_le_bytes(info[44..48].try_into().unwrap());
        if encrypt != 0 {
            return Err(EINVAL);
        }
        {
            let mut st = self.state.lock();
            let b = st.as_mut().ok_or(ENXIO)?;
            b.offset = u64_at(24);
            b.sizelimit = u64_at(32);
            // READ_ONLY only through LOOP_CONFIGURE; PARTSCAN cannot be cleared.
            let change = settable & !LO_FLAGS_READ_ONLY;
            let keep = b.flags & (LO_FLAGS_READ_ONLY | LO_FLAGS_PARTSCAN);
            b.flags = (b.flags & !change) | (flags & change) | keep;
            if settable & LO_FLAGS_READ_ONLY != 0 {
                b.flags |= flags & LO_FLAGS_READ_ONLY;
            }
            b.file_name.copy_from_slice(&info[56..120]);
            b.file_name[63] = 0;
            b.crypt_name.copy_from_slice(&info[120..184]);
            b.compute_size();
        }
        dev.cache.invalidate();
        self.after_change(dev);
        Ok(())
    }

    fn info(&self) -> KResult<[u8; INFO_SIZE]> {
        let st = self.state.lock();
        let b = st.as_ref().ok_or(ENXIO)?;
        let mut out = [0u8; INFO_SIZE];
        let put = |o: &mut [u8; INFO_SIZE], at: usize, v: u64| o[at..at + 8].copy_from_slice(&v.to_le_bytes());
        let meta = b.file.inode.meta();
        put(&mut out, 0, meta.dev);
        put(&mut out, 8, meta.ino);
        put(&mut out, 16, meta.rdev);
        put(&mut out, 24, b.offset);
        put(&mut out, 32, b.sizelimit);
        out[40..44].copy_from_slice(&self.number.to_le_bytes());
        out[52..56].copy_from_slice(&b.flags.to_le_bytes());
        out[56..120].copy_from_slice(&b.file_name);
        out[120..184].copy_from_slice(&b.crypt_name);
        Ok(out)
    }

    /// sysfs and partitions after a bind or status change.
    fn after_change(&self, dev: &Arc<BlockDev>) {
        let (offset, sizelimit, flags, name) = match self.state.lock().as_ref() {
            Some(b) => (b.offset, b.sizelimit, b.flags, String::from_utf8_lossy(cstr(&b.file_name)).into_owned()),
            None => return,
        };
        let dir = format!("{}/loop", dev.sysfs_path());
        super::sysfs_set(&dir, "backing_file", &format!("{name}\n"));
        super::sysfs_set(&dir, "offset", &format!("{offset}\n"));
        super::sysfs_set(&dir, "sizelimit", &format!("{sizelimit}\n"));
        super::sysfs_set(&dir, "autoclear", &format!("{}\n", (flags & LO_FLAGS_AUTOCLEAR != 0) as u8));
        super::sysfs_set(&dir, "partscan", &format!("{}\n", (flags & LO_FLAGS_PARTSCAN != 0) as u8));
        super::sysfs_set(&dir, "dio", "0\n");
        super::sysfs_set(&dev.sysfs_path(), "ro", &format!("{}\n", dev.read_only() as u8));
        dev.update_sysfs_size();
        if flags & LO_FLAGS_PARTSCAN != 0 && super::partitions_of(dev).is_empty() {
            super::scan_partitions(dev);
        }
    }

    fn unbind(&self, dev: &Arc<BlockDev>) {
        for p in super::partitions_of(dev) {
            super::unregister(&p);
        }
        let old = self.state.lock().take();
        dev.cache.invalidate();
        super::sysfs_remove(&format!("{}/loop", dev.sysfs_path()));
        dev.update_sysfs_size();
        super::sysfs_set(&dev.sysfs_path(), "ro", "0\n");
        drop(old);
    }

    /// Last close or unmount: tear down an AUTOCLEAR binding.
    pub fn release(&self, dev: &Arc<BlockDev>) {
        let autoclear = self.state.lock().as_ref().is_some_and(|b| b.flags & LO_FLAGS_AUTOCLEAR != 0);
        if autoclear && dev.opens.load(Ordering::Relaxed) == 0 && dev.holders.load(Ordering::Relaxed) == 0 {
            self.unbind(dev);
        }
    }
}

/// Called when an open of `dev` or a holder goes away.
pub fn maybe_release(dev: &Arc<BlockDev>) {
    if let Some(l) = dev.backend.as_any().downcast_ref::<Loop>() {
        l.release(dev);
    }
}

static LOOPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn add(n: u32) -> KResult<Arc<BlockDev>> {
    let mut loops = LOOPS.lock();
    if loops.contains(&n) {
        return Err(EEXIST);
    }
    loops.push(n);
    drop(loops);
    let dev = Arc::new(BlockDev::new(format!("loop{n}"), LOOP_MAJOR, n, Arc::new(Loop { number: n, state: Mutex::new(None) })));
    super::register(dev.clone());
    Ok(dev)
}

pub fn init() {
    for n in 0..8 {
        let _ = add(n);
    }
}

fn loop_of(dev: &BlockDev) -> Option<&Loop> {
    dev.backend.as_any().downcast_ref::<Loop>()
}

/// /dev/loop-control.
pub struct Control;

impl FileOps for Control {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            LOOP_CTL_GET_FREE => {
                let mut nums = LOOPS.lock().clone();
                nums.sort_unstable();
                for &n in &nums {
                    if let Some(d) = super::lookup_dev(LOOP_MAJOR, n)
                        && loop_of(&d).is_some_and(|l| !l.bound())
                    {
                        return Ok(n as u64);
                    }
                }
                let n = (0..).find(|n| !nums.contains(n)).unwrap();
                add(n)?;
                Ok(n as u64)
            }
            LOOP_CTL_ADD => {
                let n = if (arg as i64) < 0 {
                    let nums = LOOPS.lock().clone();
                    (0..).find(|n| !nums.contains(n)).unwrap()
                } else {
                    u32::try_from(arg).map_err(|_| EINVAL)?
                };
                if n >= 1 << 20 {
                    return Err(EINVAL);
                }
                add(n)?;
                Ok(n as u64)
            }
            LOOP_CTL_REMOVE => {
                let n = u32::try_from(arg).map_err(|_| EINVAL)?;
                let d = super::lookup_dev(LOOP_MAJOR, n).ok_or(ENODEV)?;
                let l = loop_of(&d).ok_or(ENODEV)?;
                if l.bound() || d.opens.load(Ordering::Relaxed) > 0 {
                    return Err(EBUSY);
                }
                super::unregister(&d);
                LOOPS.lock().retain(|&x| x != n);
                Ok(n as u64)
            }
            _ => Err(ENOTTY),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

