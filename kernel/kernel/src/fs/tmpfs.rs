//! tmpfs: a read-write file system in kernel memory. Also the root file
//! system (the initramfs is unpacked into it), devtmpfs and sysfs.
//!
//! File data lives in a shmem object (`mm::shmem`, the page cache), which
//! is also what `mmap` maps; files unpacked from the initramfs keep pointing
//! into the boot module until a page is first touched. memfds are tmpfs
//! files with no name.
//!
//! Pseudo files ([`PseudoFile`]: sysfs `uevent`, selinuxfs nodes, ...) are
//! tmpfs inodes whose reads and writes go to kernel code. Every inode can
//! carry extended attributes (`security.selinux` labels from restorecon).

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::{Mutex, Once};

use super::vfs::*;
use crate::errno::*;
use crate::mm::shmem::Shmem;

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

    fn into_shmem(self) -> KResult<Arc<Shmem>> {
        match self {
            Data::Static(s) => Ok(Shmem::from_static(s)),
            Data::Owned(v) => {
                let obj = Shmem::new(0);
                obj.write(0, &v)?;
                Ok(obj)
            }
        }
    }
}

/// A file whose contents kernel code produces and whose writes it handles
/// (sysfs attributes, selinuxfs nodes).
pub trait PseudoFile: Send + Sync {
    /// The contents, taken when the file is opened.
    fn read(&self) -> Vec<u8>;

    /// A write of `data` at offset `off` (large writes come in chunks). A
    /// non-empty `reply` is what later reads of the same open file return
    /// (selinuxfs transaction files) and resets the file position to 0.
    fn write(&self, _off: u64, _data: &[u8], _reply: &mut Vec<u8>) -> KResult<usize> {
        Err(EINVAL)
    }
}

enum Kind {
    Dir(Mutex<BTreeMap<Vec<u8>, Arc<TmpInode>>>),
    File(Arc<Shmem>),
    Symlink(Vec<u8>),
    /// Device node, FIFO or socket: only metadata.
    Special,
    Pseudo(Arc<dyn PseudoFile>),
    /// A pinned kernel object (bpffs: a bpf map, program, link or BTF).
    Object(Arc<dyn Any + Send + Sync>),
}

pub struct TmpInode {
    meta: Meta,
    kind: Kind,
    /// memfd_create name (the file has no path).
    memfd_name: Option<Vec<u8>>,
    xattrs: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

pub const XATTR_CREATE: u32 = 1;
pub const XATTR_REPLACE: u32 = 2;

impl TmpInode {
    fn new(dev: u64, mode: u32, rdev: u64, kind: Kind) -> Arc<TmpInode> {
        Arc::new(TmpInode { meta: Meta::new(dev, mode, rdev), kind, memfd_name: None, xattrs: Mutex::new(Vec::new()) })
    }

    /// Add a pseudo file `name` (regular file mode bits `perm`).
    pub fn add_pseudo(&self, name: &[u8], perm: u32, obj: Arc<dyn PseudoFile>) -> KResult<Arc<TmpInode>> {
        let mut children = self.dir()?.lock();
        let node = Self::new(self.meta.dev, S_IFREG | (perm & 0o7777), 0, Kind::Pseudo(obj));
        if let Some(old) = children.insert(name.to_vec(), node.clone()) {
            old.meta.inc_nlink(-1);
        }
        Ok(node)
    }

    /// Add `name` holding the kernel object `obj` (bpffs pinning); the
    /// node is a regular file with mode `perm`. EEXIST if `name` exists.
    pub fn add_object(&self, name: &[u8], perm: u32, obj: Arc<dyn Any + Send + Sync>) -> KResult<Arc<TmpInode>> {
        if name.len() > NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        let mut children = self.dir()?.lock();
        if children.contains_key(name) {
            return Err(EEXIST);
        }
        let node = Self::new(self.meta.dev, S_IFREG | (perm & 0o7777), 0, Kind::Object(obj));
        children.insert(name.to_vec(), node.clone());
        self.meta.touch_mtime();
        Ok(node)
    }

    /// The object a bpffs node holds (see [`TmpInode::add_object`]).
    pub fn object(&self) -> Option<&Arc<dyn Any + Send + Sync>> {
        match &self.kind {
            Kind::Object(o) => Some(o),
            _ => None,
        }
    }

    /// An unnamed file for memfd_create, with seals `seals` (F_SEAL_SEAL
    /// unless sealing is allowed).
    pub fn new_memfd(name: &[u8], mode: u32, seals: u32) -> Arc<TmpInode> {
        static MEMFD_DEV: Once<u64> = Once::new();
        let obj = Shmem::new(0);
        obj.set_initial_seals(seals);
        Arc::new(TmpInode {
            meta: Meta::new(*MEMFD_DEV.call_once(alloc_dev), S_IFREG | mode, 0),
            kind: Kind::File(obj),
            memfd_name: Some(name.to_vec()),
            xattrs: Mutex::new(Vec::new()),
        })
    }

    pub fn memfd_name(&self) -> Option<&[u8]> {
        self.memfd_name.as_deref()
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
            S_IFREG => Kind::File(data.map_or_else(|| Ok(Shmem::new(0)), Data::into_shmem)?),
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

    fn data(&self) -> KResult<&Arc<Shmem>> {
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
            Kind::File(d) => d.size(),
            Kind::Symlink(t) => t.len() as u64,
            Kind::Dir(_) | Kind::Pseudo(_) => 4096,
            Kind::Special | Kind::Object(_) => 0,
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
        Ok(self.data()?.read(off, buf))
    }

    fn write_at(&self, off: u64, data: &[u8]) -> KResult<usize> {
        let n = self.data()?.write(off, data)?;
        self.meta.touch_mtime();
        Ok(n)
    }

    fn append(&self, data: &[u8]) -> KResult<(usize, u64)> {
        let r = self.data()?.append(data)?;
        self.meta.touch_mtime();
        Ok(r)
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        if matches!(self.kind, Kind::Pseudo(_)) {
            // O_TRUNC on an attribute file: nothing to drop.
            return Ok(());
        }
        self.data()?.truncate(size)?;
        self.meta.touch_mtime();
        Ok(())
    }

    fn shmem(&self) -> Option<Arc<Shmem>> {
        self.data().ok().cloned()
    }

    fn xattrs(&self) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self.xattrs.lock().clone())
    }

    fn set_xattr(&self, name: &[u8], value: Option<&[u8]>, flags: u32) -> KResult<()> {
        let mut x = self.xattrs.lock();
        let at = x.iter().position(|(n, _)| n == name);
        match (value, at) {
            (Some(_), Some(_)) if flags & XATTR_CREATE != 0 => return Err(EEXIST),
            (Some(_), None) if flags & XATTR_REPLACE != 0 => return Err(ENODATA),
            (Some(v), Some(i)) => x[i].1 = v.to_vec(),
            (Some(v), None) => x.push((name.to_vec(), v.to_vec())),
            (None, Some(i)) => {
                x.remove(i);
            }
            (None, None) => return Err(ENODATA),
        }
        drop(x);
        self.meta.update(|a| a.ctime = now());
        Ok(())
    }

    fn open(&self, _flags: u32) -> KResult<Option<alloc::boxed::Box<dyn super::file::FileOps>>> {
        match &self.kind {
            Kind::Pseudo(obj) => Ok(Some(alloc::boxed::Box::new(PseudoOps { obj: obj.clone(), buf: Mutex::new(obj.read()) }))),
            // Pinned bpf objects are opened with BPF_OBJ_GET, not open(2).
            Kind::Object(_) => Err(EIO),
            _ => Ok(None),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Open pseudo file: reads come from a buffer filled at open (or by the
/// last write's reply).
struct PseudoOps {
    obj: Arc<dyn PseudoFile>,
    buf: Mutex<Vec<u8>>,
}

impl super::file::FileOps for PseudoOps {
    fn read(&self, _: &super::file::OpenFile, pos: &mut u64, out: &mut [u8]) -> KResult<usize> {
        let b = self.buf.lock();
        let start = (*pos as usize).min(b.len());
        let n = out.len().min(b.len() - start);
        out[..n].copy_from_slice(&b[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, _: &super::file::OpenFile, pos: &mut u64, data: &[u8]) -> KResult<usize> {
        let mut reply = Vec::new();
        let n = self.obj.write(*pos, data, &mut reply)?;
        if reply.is_empty() {
            *pos += n as u64;
        } else {
            *self.buf.lock() = reply;
            *pos = 0;
        }
        Ok(n)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &super::file::OpenFile) -> u64 {
        self.buf.lock().len() as u64
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
