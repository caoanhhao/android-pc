//! tmpfs: a read-write file system in kernel memory. Also the root file
//! system (the initramfs is unpacked into it), devtmpfs and sysfs.
//!
//! File data lives in a `Vec` on the kernel heap; files unpacked from the
//! initramfs keep pointing into the boot module until first written.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use super::vfs::*;
use crate::errno::*;

/// Largest tmpfs file (the kernel heap is small until B3's page cache).
pub const MAX_FILE_SIZE: u64 = 64 << 20;

pub enum Data {
    /// Contents still inside the initramfs module.
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl Data {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Data::Static(s) => s,
            Data::Owned(v) => v,
        }
    }

    /// Writable contents (copies a static file on first write).
    fn owned(&mut self) -> KResult<&mut Vec<u8>> {
        if let Data::Static(s) = self {
            let mut v = Vec::new();
            v.try_reserve_exact(s.len()).map_err(|_| ENOSPC)?;
            v.extend_from_slice(s);
            *self = Data::Owned(v);
        }
        match self {
            Data::Owned(v) => Ok(v),
            Data::Static(_) => unreachable!(),
        }
    }

    fn resize(&mut self, len: u64) -> KResult<()> {
        if len > MAX_FILE_SIZE {
            return Err(EFBIG);
        }
        let v = self.owned()?;
        let len = len as usize;
        if len > v.len() {
            v.try_reserve(len - v.len()).map_err(|_| ENOSPC)?;
        }
        v.resize(len, 0);
        if v.capacity() > 2 * v.len() + 4096 {
            v.shrink_to_fit();
        }
        Ok(())
    }

    fn write_at(&mut self, off: u64, data: &[u8]) -> KResult<usize> {
        let end = off.checked_add(data.len() as u64).ok_or(EFBIG)?;
        if end > self.bytes().len() as u64 {
            self.resize(end)?;
        }
        let v = self.owned()?;
        v[off as usize..end as usize].copy_from_slice(data);
        Ok(data.len())
    }
}

enum Kind {
    Dir(Mutex<BTreeMap<Vec<u8>, Arc<TmpInode>>>),
    File(Mutex<Data>),
    Symlink(Vec<u8>),
    /// Device node, FIFO or socket: only metadata.
    Special,
}

pub struct TmpInode {
    meta: Meta,
    kind: Kind,
}

impl TmpInode {
    fn new(dev: u64, mode: u32, rdev: u64, kind: Kind) -> Arc<TmpInode> {
        Arc::new(TmpInode { meta: Meta::new(dev, mode, rdev), kind })
    }

    /// A new, empty tmpfs; returns its root directory.
    pub fn new_fs(mode: u32) -> Arc<TmpInode> {
        Self::new(alloc_dev(), S_IFDIR | (mode & 0o7777), 0, Kind::Dir(Mutex::new(BTreeMap::new())))
    }

    fn dir(&self) -> KResult<&Mutex<BTreeMap<Vec<u8>, Arc<TmpInode>>>> {
        match &self.kind {
            Kind::Dir(d) => Ok(d),
            _ => Err(ENOTDIR),
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir(_))
    }

    fn is_empty_dir(&self) -> bool {
        matches!(&self.kind, Kind::Dir(d) if d.lock().is_empty())
    }

    /// Insert a new child (used by `create` and the initramfs unpacker).
    pub fn add(&self, name: &[u8], mode: u32, rdev: u64, data: Option<Data>) -> KResult<Arc<TmpInode>> {
        if name.len() > NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        let mut children = self.dir()?.lock();
        if children.contains_key(name) {
            return Err(EEXIST);
        }
        let kind = match mode & S_IFMT {
            S_IFDIR => Kind::Dir(Mutex::new(BTreeMap::new())),
            S_IFREG => Kind::File(Mutex::new(data.unwrap_or(Data::Owned(Vec::new())))),
            S_IFLNK => Kind::Symlink(data.map(|d| d.bytes().to_vec()).unwrap_or_default()),
            S_IFCHR | S_IFBLK | S_IFIFO | S_IFSOCK => Kind::Special,
            _ => return Err(EINVAL),
        };
        let node = Self::new(self.meta.dev, mode, rdev, kind);
        if node.is_dir() {
            self.meta.inc_nlink(1);
        }
        children.insert(name.to_vec(), node.clone());
        self.meta.touch_mtime();
        Ok(node)
    }

    /// Child `name` as a tmpfs inode (initramfs unpacking).
    pub fn get(&self, name: &[u8]) -> Option<Arc<TmpInode>> {
        self.dir().ok()?.lock().get(name).cloned()
    }

    /// Replace (or create) `name` (initramfs entries may repeat).
    pub fn replace(&self, name: &[u8], mode: u32, rdev: u64, data: Option<Data>) -> KResult<Arc<TmpInode>> {
        let _ = Inode::unlink(self, name, false);
        self.add(name, mode, rdev, data)
    }

    pub fn set_owner(&self, uid: u32, gid: u32, mtime: i64) {
        self.meta.update(|a| {
            a.uid = uid;
            a.gid = gid;
            a.mtime = mtime;
            a.ctime = mtime;
            a.atime = mtime;
        });
    }

    fn data(&self) -> KResult<&Mutex<Data>> {
        match &self.kind {
            Kind::File(d) => Ok(d),
            Kind::Dir(_) => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }
}

/// Lock two directory maps in address order (rename between directories).
fn lock_two<'a, T>(a: &'a Mutex<T>, b: &'a Mutex<T>) -> (spin::MutexGuard<'a, T>, spin::MutexGuard<'a, T>) {
    if (a as *const _ as usize) < (b as *const _ as usize) {
        let ga = a.lock();
        (ga, b.lock())
    } else {
        let gb = b.lock();
        (a.lock(), gb)
    }
}

impl Inode for TmpInode {
    fn meta(&self) -> &Meta {
        &self.meta
    }

    fn size(&self) -> u64 {
        match &self.kind {
            Kind::File(d) => d.lock().bytes().len() as u64,
            Kind::Symlink(t) => t.len() as u64,
            Kind::Dir(_) => 4096,
            Kind::Special => 0,
        }
    }

    fn lookup(&self, name: &[u8]) -> KResult<Arc<dyn Inode>> {
        let c = self.dir()?.lock();
        c.get(name).cloned().map(|n| n as Arc<dyn Inode>).ok_or(ENOENT)
    }

    fn create(&self, name: &[u8], mode: u32, rdev: u64) -> KResult<Arc<dyn Inode>> {
        if mode & S_IFMT == S_IFLNK {
            return Err(EINVAL);
        }
        Ok(self.add(name, mode, rdev, None)?)
    }

    fn symlink(&self, name: &[u8], target: &[u8]) -> KResult<Arc<dyn Inode>> {
        Ok(self.add(name, S_IFLNK | 0o777, 0, Some(Data::Owned(target.to_vec())))?)
    }

    fn link(&self, name: &[u8], inode: &Arc<dyn Inode>) -> KResult<()> {
        let mut children = self.dir()?.lock();
        if children.contains_key(name) {
            return Err(EEXIST);
        }
        let any: Arc<dyn Any + Send + Sync> = inode.clone();
        let node = any.downcast::<TmpInode>().map_err(|_| EXDEV)?;
        if node.meta.dev != self.meta.dev {
            return Err(EXDEV);
        }
        node.meta.inc_nlink(1);
        children.insert(name.to_vec(), node);
        self.meta.touch_mtime();
        Ok(())
    }

    fn unlink(&self, name: &[u8], dir: bool) -> KResult<()> {
        let mut children = self.dir()?.lock();
        let node = children.get(name).ok_or(ENOENT)?;
        match (dir, node.is_dir()) {
            (true, false) => return Err(ENOTDIR),
            (false, true) => return Err(EISDIR),
            (true, true) if !node.is_empty_dir() => return Err(ENOTEMPTY),
            _ => {}
        }
        let node = children.remove(name).unwrap();
        if node.is_dir() {
            node.meta.update(|a| a.nlink = 0);
            self.meta.inc_nlink(-1);
        } else {
            node.meta.inc_nlink(-1);
        }
        self.meta.touch_mtime();
        Ok(())
    }

    fn rename(&self, old: &[u8], new_dir: &Arc<dyn Inode>, new: &[u8], flags: u32) -> KResult<()> {
        let Some(nd) = new_dir.as_any().downcast_ref::<TmpInode>() else { return Err(EXDEV) };
        if nd.meta.dev != self.meta.dev {
            return Err(EXDEV);
        }
        if new.len() > NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        if core::ptr::eq(self, nd) {
            return rename_in(&mut self.dir()?.lock(), old, new, flags, &self.meta);
        }
        let (mut src, mut dst) = lock_two(self.dir()?, nd.dir()?);
        let node = src.get(old).cloned().ok_or(ENOENT)?;
        let existing = dst.get(new).cloned();
        if flags & RENAME_EXCHANGE != 0 {
            let other = existing.ok_or(ENOENT)?;
            if node.is_dir() != other.is_dir() {
                let d = if node.is_dir() { 1 } else { -1 };
                self.meta.inc_nlink(-d);
                nd.meta.inc_nlink(d);
            }
            src.insert(old.to_vec(), other);
            dst.insert(new.to_vec(), node);
        } else {
            if let Some(e) = &existing {
                if flags & RENAME_NOREPLACE != 0 {
                    return Err(EEXIST);
                }
                check_replace(&node, e)?;
            }
            src.remove(old);
            if let Some(e) = dst.insert(new.to_vec(), node.clone()) {
                drop_replaced(&e, &nd.meta);
            }
            if node.is_dir() {
                self.meta.inc_nlink(-1);
                nd.meta.inc_nlink(1);
            }
        }
        self.meta.touch_mtime();
        nd.meta.touch_mtime();
        Ok(())
    }

    fn entries(&self) -> KResult<Vec<DirEntry>> {
        let c = self.dir()?.lock();
        Ok(c.iter()
            .map(|(name, n)| DirEntry { name: name.clone(), ino: n.meta.ino, dtype: dtype(n.meta.mode()) })
            .collect())
    }

    fn readlink(&self) -> KResult<Vec<u8>> {
        match &self.kind {
            Kind::Symlink(t) => Ok(t.clone()),
            _ => Err(EINVAL),
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let d = self.data()?.lock();
        let bytes = d.bytes();
        let start = (off.min(bytes.len() as u64)) as usize;
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        Ok(n)
    }

    fn write_at(&self, off: u64, data: &[u8]) -> KResult<usize> {
        let n = self.data()?.lock().write_at(off, data)?;
        self.meta.touch_mtime();
        Ok(n)
    }

    fn append(&self, data: &[u8]) -> KResult<(usize, u64)> {
        let mut d = self.data()?.lock();
        let off = d.bytes().len() as u64;
        let n = d.write_at(off, data)?;
        let size = d.bytes().len() as u64;
        drop(d);
        self.meta.touch_mtime();
        Ok((n, size))
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        self.data()?.lock().resize(size)?;
        self.meta.touch_mtime();
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn check_replace(node: &TmpInode, existing: &TmpInode) -> KResult<()> {
    match (node.is_dir(), existing.is_dir()) {
        (true, false) => Err(ENOTDIR),
        (false, true) => Err(EISDIR),
        (true, true) if !existing.is_empty_dir() => Err(ENOTEMPTY),
        _ => Ok(()),
    }
}

/// Account for an inode that lost its name to a rename.
fn drop_replaced(e: &TmpInode, dir_meta: &Meta) {
    if e.is_dir() {
        e.meta.update(|a| a.nlink = 0);
        dir_meta.inc_nlink(-1);
    } else {
        e.meta.inc_nlink(-1);
    }
}

/// Rename inside one directory.
fn rename_in(map: &mut BTreeMap<Vec<u8>, Arc<TmpInode>>, old: &[u8], new: &[u8], flags: u32, meta: &Meta) -> KResult<()> {
    let node = map.get(old).cloned().ok_or(ENOENT)?;
    if old == new {
        return Ok(());
    }
    let existing = map.get(new).cloned();
    if flags & RENAME_EXCHANGE != 0 {
        let other = existing.ok_or(ENOENT)?;
        map.insert(old.to_vec(), other);
        map.insert(new.to_vec(), node);
    } else {
        if let Some(e) = &existing {
            if flags & RENAME_NOREPLACE != 0 {
                return Err(EEXIST);
            }
            check_replace(&node, e)?;
        }
        map.remove(old);
        if let Some(e) = map.insert(new.to_vec(), node) {
            drop_replaced(&e, meta);
        }
    }
    meta.touch_mtime();
    Ok(())
}
