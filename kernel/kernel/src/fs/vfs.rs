//! VFS core: inodes, dentries, mounts and path resolution.
//!
//! - [`Inode`] is what a file system implements (tmpfs, procfs, ...). Every
//!   inode carries a [`Meta`] (ino, dev, mode, owner, link count, times).
//! - [`Dentry`] is a name in the tree: (name, parent, inode) plus a cache of
//!   child dentries (weak refs). Unlink/rename go through the VFS, which keeps
//!   the cache in sync, so a dentry stays valid for open files and cwd.
//! - [`Mount`] attaches a file system root (or a bind-mounted dentry) on a
//!   (mount, dentry) point. There is one global mount namespace.
//! - [`PathRef`] = (mount, dentry), the unit everything resolves to.
//!
//! Path walk follows symlinks (at most [`MAX_SYMLINKS`]), handles `.` and
//! `..` (also across mount points) and crosses into mounts.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, Once};

use crate::errno::*;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

pub const DT_UNKNOWN: u8 = 0;
pub const DT_FIFO: u8 = 1;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_BLK: u8 = 6;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;
pub const DT_SOCK: u8 = 12;

pub const MAX_SYMLINKS: u32 = 40;
pub const NAME_MAX: usize = 255;
pub const PATH_MAX: usize = 4096;

pub const RENAME_NOREPLACE: u32 = 1;
pub const RENAME_EXCHANGE: u32 = 2;

pub fn dtype(mode: u32) -> u8 {
    match mode & S_IFMT {
        S_IFDIR => DT_DIR,
        S_IFREG => DT_REG,
        S_IFLNK => DT_LNK,
        S_IFCHR => DT_CHR,
        S_IFBLK => DT_BLK,
        S_IFIFO => DT_FIFO,
        S_IFSOCK => DT_SOCK,
        _ => DT_UNKNOWN,
    }
}

/// Linux `new_encode_dev`.
pub fn encode_dev(major: u32, minor: u32) -> u64 {
    ((minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)) as u64
}

pub fn dev_major(dev: u64) -> u32 {
    ((dev >> 8) & 0xfff) as u32
}

pub fn dev_minor(dev: u64) -> u32 {
    ((dev & 0xff) | ((dev >> 12) & 0xfff00)) as u32
}

static NEXT_INO: AtomicU64 = AtomicU64::new(1);
static NEXT_DEV: AtomicU64 = AtomicU64::new(1);

pub fn alloc_ino() -> u64 {
    NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

/// A new anonymous device number (major 0) for a file system instance.
pub fn alloc_dev() -> u64 {
    encode_dev(0, NEXT_DEV.fetch_add(1, Ordering::Relaxed) as u32)
}

/// Time in nanoseconds since the epoch.
pub fn now() -> i64 {
    crate::time::realtime_ns() as i64
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Attr {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// Inode metadata common to every file system.
pub struct Meta {
    pub ino: u64,
    pub dev: u64,
    /// Device number for character/block device nodes.
    pub rdev: u64,
    attr: Mutex<Attr>,
}

impl Meta {
    pub fn new(dev: u64, mode: u32, rdev: u64) -> Meta {
        Meta::with_ino(alloc_ino(), dev, mode, rdev)
    }

    pub fn with_ino(ino: u64, dev: u64, mode: u32, rdev: u64) -> Meta {
        let t = now();
        Meta {
            ino,
            dev,
            rdev,
            attr: Mutex::new(Attr {
                mode,
                uid: 0,
                gid: 0,
                nlink: if mode & S_IFMT == S_IFDIR { 2 } else { 1 },
                atime: t,
                mtime: t,
                ctime: t,
            }),
        }
    }

    pub fn attr(&self) -> Attr {
        *self.attr.lock()
    }

    pub fn update(&self, f: impl FnOnce(&mut Attr)) {
        f(&mut self.attr.lock())
    }

    pub fn mode(&self) -> u32 {
        self.attr.lock().mode
    }

    pub fn file_type(&self) -> u32 {
        self.mode() & S_IFMT
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == S_IFDIR
    }

    pub fn inc_nlink(&self, by: i32) {
        let mut a = self.attr.lock();
        a.nlink = (a.nlink as i32 + by).max(0) as u32;
        a.ctime = now();
    }

    pub fn touch_mtime(&self) {
        let t = now();
        let mut a = self.attr.lock();
        a.mtime = t;
        a.ctime = t;
    }
}

/// statfs(2) numbers of a disk file system (others are derived from
/// free memory).
pub struct StatFs {
    pub magic: u64,
    pub bsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub namelen: u64,
    pub flags: u64,
}

pub const ST_RDONLY: u64 = 1;

pub struct DirEntry {
    pub name: Vec<u8>,
    pub ino: u64,
    pub dtype: u8,
}

/// What a file system implements for each of its inodes. Defaults are
/// the answers for an inode that does not support the operation.
pub trait Inode: Send + Sync + Any {
    fn meta(&self) -> &Meta;

    /// File size in bytes (regular files, symlinks).
    fn size(&self) -> u64 {
        0
    }

    /// Directory: find `name` (never "." or "..").
    fn lookup(&self, _name: &[u8]) -> KResult<Arc<dyn Inode>> {
        Err(ENOTDIR)
    }

    /// Directory: create a regular file, directory, device node or FIFO
    /// (type from `mode`). EEXIST if the name is taken.
    fn create(&self, _name: &[u8], _mode: u32, _rdev: u64) -> KResult<Arc<dyn Inode>> {
        Err(self.not_dir_or(EACCES))
    }

    fn symlink(&self, _name: &[u8], _target: &[u8]) -> KResult<Arc<dyn Inode>> {
        Err(self.not_dir_or(EACCES))
    }

    /// Directory: add a hard link `name` -> `inode` (same file system).
    fn link(&self, _name: &[u8], _inode: &Arc<dyn Inode>) -> KResult<()> {
        Err(self.not_dir_or(EPERM))
    }

    /// Directory: remove `name` (a directory iff `dir`, which must be empty).
    fn unlink(&self, _name: &[u8], _dir: bool) -> KResult<()> {
        Err(self.not_dir_or(EACCES))
    }

    /// Directory: move `old` to `new` in `new_dir` (same file system).
    fn rename(&self, _old: &[u8], _new_dir: &Arc<dyn Inode>, _new: &[u8], _flags: u32) -> KResult<()> {
        Err(self.not_dir_or(EXDEV))
    }

    /// Directory: the entries, without "." and "..".
    fn entries(&self) -> KResult<Vec<DirEntry>> {
        Err(ENOTDIR)
    }

    fn readlink(&self) -> KResult<Vec<u8>> {
        Err(EINVAL)
    }

    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> KResult<usize> {
        Err(EINVAL)
    }

    fn write_at(&self, _off: u64, _data: &[u8]) -> KResult<usize> {
        Err(EINVAL)
    }

    /// Append atomically; returns (bytes written, new size).
    fn append(&self, data: &[u8]) -> KResult<(usize, u64)> {
        let n = self.write_at(self.size(), data)?;
        Ok((n, self.size()))
    }

    fn truncate(&self, _size: u64) -> KResult<()> {
        Err(EINVAL)
    }

    /// The page cache object holding this file's data, for regular files
    /// that can be mapped (tmpfs, memfd).
    fn shmem(&self) -> Option<Arc<crate::mm::shmem::Shmem>> {
        None
    }

    /// Extended attributes (full names such as "security.selinux").
    fn xattrs(&self) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(Vec::new())
    }

    /// Set (`Some`) or remove (`None`) an extended attribute; `flags` are
    /// XATTR_CREATE / XATTR_REPLACE.
    fn set_xattr(&self, _name: &[u8], _value: Option<&[u8]>, _flags: u32) -> KResult<()> {
        Err(if self.read_only_fs() { EROFS } else { EOPNOTSUPP })
    }

    /// Procfs "magic links" (/proc/<pid>/fd/N, ns/*, exe, cwd): following
    /// them reaches this target instead of resolving `readlink`'s text.
    fn magic_link(&self) -> Option<KResult<MagicLink>> {
        None
    }

    /// Extra mount options for /proc/mounts, asked of a mount's root inode
    /// (cgroup controllers).
    fn mount_options(&self) -> Option<alloc::string::String> {
        None
    }

    /// statfs numbers, asked of a mount's root inode (disk file systems).
    fn statfs(&self) -> Option<StatFs> {
        None
    }

    /// Inodes of read-only file systems (erofs, ext4 for now): opening a
    /// regular file for writing fails with EROFS.
    fn read_only_fs(&self) -> bool {
        false
    }

    /// File operations for an open of this inode when they are not the
    /// generic ones (pseudo files); `None` = use the VFS default.
    fn open(&self, _flags: u32) -> KResult<Option<alloc::boxed::Box<dyn super::file::FileOps>>> {
        Ok(None)
    }

    /// Whether lookups of this directory's children may be cached in
    /// dentries (false for dynamic directories like /proc).
    fn cacheable(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any;

    fn not_dir_or(&self, e: Errno) -> Errno {
        if self.meta().is_dir() { e } else { ENOTDIR }
    }
}

/// Where a magic link leads: a path, or an inode without one (memfd,
/// pipe, socket, namespace file), which gets a fresh dentry.
pub enum MagicLink {
    Path(PathRef),
    Inode(Arc<dyn Inode>),
}

/// An inode with no file system behind it (pipes, the boot console).
pub struct AnonInode {
    meta: Meta,
}

impl AnonInode {
    pub fn new(mode: u32, rdev: u64) -> Arc<dyn Inode> {
        static ANON_DEV: Once<u64> = Once::new();
        let dev = *ANON_DEV.call_once(alloc_dev);
        Arc::new(AnonInode { meta: Meta::new(dev, mode, rdev) })
    }
}

impl Inode for AnonInode {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Dentries

struct DentryInner {
    name: Vec<u8>,
    parent: Option<Arc<Dentry>>,
    children: BTreeMap<Vec<u8>, Weak<Dentry>>,
    deleted: bool,
}

pub struct Dentry {
    pub inode: Arc<dyn Inode>,
    inner: Mutex<DentryInner>,
}

impl Dentry {
    pub fn new_root(inode: Arc<dyn Inode>) -> Arc<Dentry> {
        Self::new(inode, b"/", None)
    }

    pub fn new(inode: Arc<dyn Inode>, name: &[u8], parent: Option<Arc<Dentry>>) -> Arc<Dentry> {
        Arc::new(Dentry {
            inode,
            inner: Mutex::new(DentryInner {
                name: name.to_vec(),
                parent,
                children: BTreeMap::new(),
                deleted: false,
            }),
        })
    }

    pub fn name(&self) -> Vec<u8> {
        self.inner.lock().name.clone()
    }

    pub fn parent(&self) -> Option<Arc<Dentry>> {
        self.inner.lock().parent.clone()
    }

    pub fn is_deleted(&self) -> bool {
        self.inner.lock().deleted
    }

    pub fn is_dir(&self) -> bool {
        self.inode.meta().is_dir()
    }

    fn cached(&self, name: &[u8]) -> Option<Arc<Dentry>> {
        self.inner.lock().children.get(name).and_then(Weak::upgrade)
    }

    /// The child dentry `name` (from the cache or the file system).
    pub fn child(self: &Arc<Self>, name: &[u8]) -> KResult<Arc<Dentry>> {
        if let Some(d) = self.cached(name) {
            return Ok(d);
        }
        let inode = self.inode.lookup(name)?;
        Ok(self.attach(name, inode))
    }

    /// A dentry for a freshly created (or looked up) child inode.
    pub fn attach(self: &Arc<Self>, name: &[u8], inode: Arc<dyn Inode>) -> Arc<Dentry> {
        if !self.inode.cacheable() {
            return Dentry::new(inode, name, Some(self.clone()));
        }
        let mut inner = self.inner.lock();
        if let Some(d) = inner.children.get(name).and_then(Weak::upgrade)
            && Arc::ptr_eq(&d.inode, &inode)
        {
            return d;
        }
        let d = Dentry::new(inode, name, Some(self.clone()));
        if inner.children.len() >= 64 && inner.children.len().is_power_of_two() {
            inner.children.retain(|_, w| w.strong_count() > 0);
        }
        inner.children.insert(name.to_vec(), Arc::downgrade(&d));
        d
    }

    /// Drop the cached child `name` (it was unlinked or replaced).
    pub fn forget(&self, name: &[u8]) {
        let old = self.inner.lock().children.remove(name).and_then(|w| w.upgrade());
        if let Some(d) = old {
            d.inner.lock().deleted = true;
        }
    }

    /// Move the cached child `old` of `self` to `new` in `new_parent`.
    fn moved(self: &Arc<Self>, old: &[u8], new_parent: &Arc<Dentry>, new: &[u8]) {
        let d = self.inner.lock().children.remove(old).and_then(|w| w.upgrade());
        if let Some(d) = d {
            {
                let mut i = d.inner.lock();
                i.name = new.to_vec();
                i.parent = Some(new_parent.clone());
            }
            new_parent.inner.lock().children.insert(new.to_vec(), Arc::downgrade(&d));
        }
    }

    /// True if `self` is `other` or below it in the dentry tree.
    pub fn is_descendant_of(self: &Arc<Self>, other: &Arc<Dentry>) -> bool {
        let mut cur = Some(self.clone());
        while let Some(d) = cur {
            if Arc::ptr_eq(&d, other) {
                return true;
            }
            cur = d.parent();
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Mounts

pub struct Mount {
    /// Root of what is mounted (a file system root, or a dentry for binds).
    pub root: Arc<Dentry>,
    /// Where it is mounted; `None` for the root mount. Changes with
    /// MS_MOVE (see [`move_mount`]).
    parent: Mutex<Option<PathRef>>,
    pub fstype: &'static str,
    pub source: Vec<u8>,
    /// MS_RDONLY (recorded, not enforced yet).
    pub flags: u64,
    /// Peer group (MS_SHARED), 0 when private. Mounts and unmounts below
    /// one member of a group propagate to the others, which is how a bind
    /// mount of a shared mount sees what is mounted later below its source
    /// (Android's /bootstrap-apex and /apex).
    group: AtomicU64,
}

static MOUNTS: Mutex<Vec<Arc<Mount>>> = Mutex::new(Vec::new());
/// Bumped on every change of the mount table (poll on /proc/mounts).
static MOUNT_GEN: AtomicU64 = AtomicU64::new(0);
static MOUNT_WQ: Once<Arc<crate::fs::poll::WaitQueue>> = Once::new();

pub fn mount_generation() -> u64 {
    MOUNT_GEN.load(Ordering::Relaxed)
}

pub fn mount_wait_queue() -> Arc<crate::fs::poll::WaitQueue> {
    MOUNT_WQ.call_once(crate::fs::poll::WaitQueue::new).clone()
}

fn mounts_changed() {
    MOUNT_GEN.fetch_add(1, Ordering::Relaxed);
    if let Some(q) = MOUNT_WQ.get() {
        q.wake(crate::fs::poll::POLLPRI | crate::fs::poll::POLLERR);
    }
}

/// `p` is visible from `root` (its chain of mount points leads there).
pub fn reachable(p: &PathRef, root: &PathRef) -> bool {
    let mut m = p.mnt.clone();
    loop {
        if Arc::ptr_eq(&m, &root.mnt) {
            return true;
        }
        match m.parent() {
            Some(pp) => m = pp.mnt,
            None => return false,
        }
    }
}

/// A mount namespace: its copy of the initial root mount. Its mounts are
/// those whose chain of mount points leads there (see [`reachable`]).
/// unshare(CLONE_NEWNS) copies every mount of the caller's namespace;
/// copies of shared mounts are peers of the originals, so later mounts
/// below them propagate both ways ([`mount_in_group`]).
pub struct MntNs {
    /// The nsfs inode number (/proc/<pid>/ns/mnt reads "mnt:[id]").
    pub id: u64,
    root: Arc<Mount>,
}

/// The initial namespace keeps Linux's well-known id.
const INIT_MNT_NS_ID: u64 = 4026531841;
static NEXT_NS_ID: AtomicU64 = AtomicU64::new(4026532000);
static INIT_NS: Once<Arc<MntNs>> = Once::new();

/// The namespace the calling process lives in (the initial one in kernel
/// context).
pub fn current_ns() -> Arc<MntNs> {
    if let Some(t) = crate::sched::try_current()
        && let Some(ns) = t.process.mnt_ns.lock().clone()
    {
        return ns;
    }
    INIT_NS.get().expect("VFS root not mounted").clone()
}

/// The initial mount namespace.
pub fn initial_ns() -> Arc<MntNs> {
    INIT_NS.get().expect("VFS root not mounted").clone()
}

/// Copy the namespace `ns` (unshare/clone with CLONE_NEWNS). Returns the
/// new namespace and the function mapping a path of the old one to the
/// same place in the new one.
pub fn copy_ns(ns: &Arc<MntNs>) -> (Arc<MntNs>, impl Fn(&PathRef) -> PathRef + use<>) {
    let mut mounts = MOUNTS.lock();
    let olds: Vec<Arc<Mount>> = mounts.iter().filter(|m| mount_reaches(m, &ns.root)).cloned().collect();
    let news: Vec<Arc<Mount>> = olds
        .iter()
        .map(|m| {
            Arc::new(Mount {
                root: m.root.clone(),
                parent: Mutex::new(None),
                fstype: m.fstype,
                source: m.source.clone(),
                flags: m.flags,
                group: AtomicU64::new(m.group()),
            })
        })
        .collect();
    let find = move |olds: &[Arc<Mount>], news: &[Arc<Mount>], m: &Arc<Mount>| {
        olds.iter().position(|o| Arc::ptr_eq(o, m)).map(|i| news[i].clone())
    };
    for (o, n) in olds.iter().zip(&news) {
        if let Some(p) = o.parent()
            && let Some(pm) = find(&olds, &news, &p.mnt)
        {
            *n.parent.lock() = Some(PathRef { mnt: pm, dentry: p.dentry });
        }
    }
    mounts.extend(news.iter().cloned());
    drop(mounts);
    let root = find(&olds, &news, &ns.root).expect("namespace root");
    let new = Arc::new(MntNs { id: NEXT_NS_ID.fetch_add(1, Ordering::Relaxed), root });
    let map = move |p: &PathRef| match find(&olds, &news, &p.mnt) {
        Some(mnt) => PathRef { mnt, dentry: p.dentry.clone() },
        None => p.clone(),
    };
    (new, map)
}

/// The root directory of namespace `ns`.
pub fn ns_root(ns: &MntNs) -> PathRef {
    cross_mounts(PathRef { dentry: ns.root.root.clone(), mnt: ns.root.clone() })
}

/// `m`'s chain of mount points ends at `root`.
fn mount_reaches(m: &Arc<Mount>, root: &Arc<Mount>) -> bool {
    let mut m = m.clone();
    loop {
        if Arc::ptr_eq(&m, root) {
            return true;
        }
        match m.parent() {
            Some(p) => m = p.mnt,
            None => return false,
        }
    }
}

#[derive(Clone)]
pub struct PathRef {
    pub mnt: Arc<Mount>,
    pub dentry: Arc<Dentry>,
}

impl PathRef {
    pub fn inode(&self) -> &Arc<dyn Inode> {
        &self.dentry.inode
    }

    pub fn is_dir(&self) -> bool {
        self.dentry.is_dir()
    }

    pub fn same(&self, other: &PathRef) -> bool {
        Arc::ptr_eq(&self.mnt, &other.mnt) && Arc::ptr_eq(&self.dentry, &other.dentry)
    }

    pub fn is_mount_root(&self) -> bool {
        Arc::ptr_eq(&self.dentry, &self.mnt.root)
    }
}

impl Mount {
    /// The point this mount is attached to (`None` for the initial root).
    pub fn parent(&self) -> Option<PathRef> {
        self.parent.lock().clone()
    }
}

/// Install the root file system (once, at boot).
pub fn set_root(inode: Arc<dyn Inode>, fstype: &'static str) {
    let m = Arc::new(Mount {
        root: Dentry::new_root(inode),
        parent: Mutex::new(None),
        fstype,
        source: b"rootfs".to_vec(),
        flags: 0,
        group: AtomicU64::new(0),
    });
    MOUNTS.lock().push(m.clone());
    INIT_NS.call_once(|| Arc::new(MntNs { id: INIT_MNT_NS_ID, root: m }));
}

/// The root directory of the caller's mount namespace: the root file
/// system, or what was mounted (moved) on top of it, as after Android's
/// switch_root.
pub fn root() -> PathRef {
    ns_root(&current_ns())
}

static NEXT_GROUP: AtomicU64 = AtomicU64::new(1);

impl Mount {
    /// The peer group id (0: private).
    pub fn group(&self) -> u64 {
        self.group.load(Ordering::Relaxed)
    }
}

/// MS_SHARED / MS_PRIVATE (MS_SLAVE and MS_UNBINDABLE are treated as
/// private: nothing propagates into them) on the mount at `p`, and with
/// `rec` on every mount below it.
pub fn set_propagation(p: &PathRef, shared: bool, rec: bool) -> KResult<()> {
    if !p.is_mount_root() {
        return Err(EINVAL);
    }
    let mounts = MOUNTS.lock().clone();
    let mut targets = alloc::vec![p.mnt.clone()];
    if rec {
        let mut i = 0;
        while i < targets.len() {
            let t = targets[i].clone();
            targets.extend(mounts.iter().filter(|m| m.parent().is_some_and(|q| Arc::ptr_eq(&q.mnt, &t))).cloned());
            i += 1;
        }
    }
    for m in targets {
        if !shared {
            m.group.store(0, Ordering::Relaxed);
        } else if m.group() == 0 {
            m.group.store(NEXT_GROUP.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        }
    }
    Ok(())
}

fn new_mount(at: PathRef, root: Arc<Dentry>, fstype: &'static str, source: &[u8], flags: u64, group: u64) -> Arc<Mount> {
    Arc::new(Mount { root, parent: Mutex::new(Some(at)), fstype, source: source.to_vec(), flags, group: AtomicU64::new(group) })
}

/// The other members of `m`'s peer group that can see `d` (it is their
/// root or below it), as mount points for a propagated copy.
fn peer_points(mounts: &[Arc<Mount>], m: &Arc<Mount>, d: &Arc<Dentry>) -> Vec<PathRef> {
    let g = m.group();
    if g == 0 {
        return Vec::new();
    }
    mounts
        .iter()
        .filter(|p| !Arc::ptr_eq(p, m) && p.group() == g && d.is_descendant_of(&p.root))
        .map(|p| PathRef { mnt: p.clone(), dentry: d.clone() })
        .collect()
}

/// Mount `root` on `at`. When `at` is in a shared mount, a copy is mounted
/// at the same place in each of its peers (the copies form a new peer
/// group with the original).
pub fn mount(at: &PathRef, root: Arc<Dentry>, fstype: &'static str, source: &[u8], flags: u64) -> KResult<()> {
    mount_in_group(at, root, fstype, source, flags, 0).map(|_| ())
}

/// [`mount`], joining peer group `group` (a bind of a shared mount) unless
/// it is 0. Returns the new mount.
pub fn mount_in_group(at: &PathRef, root: Arc<Dentry>, fstype: &'static str, source: &[u8], flags: u64, group: u64) -> KResult<Arc<Mount>> {
    if !at.is_dir() && root.inode.meta().file_type() != at.inode().meta().file_type() {
        return Err(ENOTDIR);
    }
    let mut mounts = MOUNTS.lock();
    let peers = peer_points(&mounts, &at.mnt, &at.dentry);
    let group = if group == 0 && !peers.is_empty() { NEXT_GROUP.fetch_add(1, Ordering::Relaxed) } else { group };
    let m = new_mount(at.clone(), root.clone(), fstype, source, flags, group);
    mounts.push(m.clone());
    for p in peers {
        mounts.push(new_mount(p, root.clone(), fstype, source, flags, group));
    }
    drop(mounts);
    mounts_changed();
    Ok(m)
}

/// MS_BIND: mount what `src` shows on `at`; with `rec` also the mounts
/// below `src` (MS_REC). The new mount joins `src`'s peer group.
pub fn bind_mount(src: &PathRef, at: &PathRef, rec: bool) -> KResult<()> {
    let top = mount_in_group(at, src.dentry.clone(), src.mnt.fstype, &src.mnt.source, src.mnt.flags, src.mnt.group())?;
    if !rec {
        return Ok(());
    }
    // (source mount, copy) pairs whose submounts still need copying.
    let mut todo = alloc::vec![(src.mnt.clone(), top)];
    while let Some((orig, copy)) = todo.pop() {
        let subs: Vec<Arc<Mount>> = MOUNTS
            .lock()
            .iter()
            .filter(|m| m.parent().is_some_and(|q| Arc::ptr_eq(&q.mnt, &orig) && q.dentry.is_descendant_of(&copy.root)))
            .cloned()
            .collect();
        for sub in subs {
            let point = PathRef { mnt: copy.clone(), dentry: sub.parent().unwrap().dentry };
            let c = new_mount(point, sub.root.clone(), sub.fstype, &sub.source, sub.flags, sub.group());
            MOUNTS.lock().push(c.clone());
            todo.push((sub, c));
        }
    }
    mounts_changed();
    Ok(())
}

/// Unmount the mount whose root is `at`, and the copies propagated to the
/// peers of the mount it is attached to.
pub fn umount(at: &PathRef) -> KResult<()> {
    if !at.is_mount_root() {
        return Err(EINVAL);
    }
    let mut mounts = MOUNTS.lock();
    let Some(point) = at.mnt.parent() else {
        return Err(EBUSY);
    };
    let has_children = |mounts: &Vec<Arc<Mount>>, x: &Arc<Mount>| {
        mounts.iter().any(|m| m.parent().is_some_and(|p| Arc::ptr_eq(&p.mnt, x)))
    };
    if has_children(&mounts, &at.mnt) {
        return Err(EBUSY);
    }
    let copies: Vec<Arc<Mount>> = peer_points(&mounts, &point.mnt, &point.dentry)
        .iter()
        .filter_map(|pp| {
            mounts
                .iter()
                .rev()
                .find(|m| m.parent().is_some_and(|q| q.same(pp)) && Arc::ptr_eq(&m.root, &at.mnt.root))
                .cloned()
        })
        .filter(|c| !has_children(&mounts, c))
        .collect();
    mounts.retain(|m| !Arc::ptr_eq(m, &at.mnt) && !copies.iter().any(|c| Arc::ptr_eq(c, m)));
    drop(mounts);
    mounts_changed();
    Ok(())
}

/// MS_MOVE: attach the mount whose root is `from` at `to` instead. The
/// mount keeps its identity (open files, cwds and mounts below it follow).
/// Moving onto the root directory makes it the new root.
pub fn move_mount(from: &PathRef, to: &PathRef) -> KResult<()> {
    if !from.is_mount_root() || from.mnt.parent().is_none() {
        return Err(EINVAL);
    }
    if !to.is_dir() {
        return Err(ENOTDIR);
    }
    // Not below itself.
    let mut cur = Some(to.clone());
    while let Some(p) = cur {
        if Arc::ptr_eq(&p.mnt, &from.mnt) {
            return Err(ELOOP);
        }
        cur = p.mnt.parent();
    }
    {
        let _g = MOUNTS.lock();
        *from.mnt.parent.lock() = Some(to.clone());
    }
    mounts_changed();
    Ok(())
}

/// Every mount, oldest first.
pub fn mounts() -> Vec<Arc<Mount>> {
    MOUNTS.lock().clone()
}

/// The mount stacked on `p`, if any (the most recent one).
fn mounted_on(p: &PathRef) -> Option<Arc<Mount>> {
    MOUNTS
        .lock()
        .iter()
        .rev()
        .find(|m| m.parent().is_some_and(|q| q.same(p)))
        .cloned()
}

/// Follow mounts stacked on `p`.
fn cross_mounts(mut p: PathRef) -> PathRef {
    while let Some(m) = mounted_on(&p) {
        p = PathRef { dentry: m.root.clone(), mnt: m };
    }
    p
}

/// `..` of `p`, never above `root`.
pub fn parent_of(root: &PathRef, p: &PathRef) -> PathRef {
    let mut cur = p.clone();
    loop {
        if cur.same(root) {
            return cur;
        }
        if !cur.is_mount_root() {
            break;
        }
        match cur.mnt.parent() {
            Some(mp) => cur = mp,
            None => return cur,
        }
    }
    match cur.dentry.parent() {
        Some(d) => PathRef { mnt: cur.mnt.clone(), dentry: d },
        None => cur,
    }
}

/// Child `name` of directory `dir`, crossing into mounts.
pub fn child_of(dir: &PathRef, name: &[u8]) -> KResult<PathRef> {
    let d = dir.dentry.child(name)?;
    Ok(cross_mounts(PathRef { mnt: dir.mnt.clone(), dentry: d }))
}

/// Absolute path of `p` as seen from the global root. Deleted entries get
/// " (deleted)" appended, like Linux's d_path.
pub fn path_of(p: &PathRef) -> Vec<u8> {
    let root = root();
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut cur = p.clone();
    while !cur.same(&root) {
        if cur.is_mount_root() {
            match cur.mnt.parent() {
                Some(mp) => {
                    cur = mp;
                    continue;
                }
                None => break,
            }
        }
        let Some(parent) = cur.dentry.parent() else { break };
        names.push(cur.dentry.name());
        cur = PathRef { mnt: cur.mnt.clone(), dentry: parent };
    }
    let mut out = Vec::new();
    for n in names.iter().rev() {
        out.push(b'/');
        out.extend_from_slice(n);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    if p.dentry.is_deleted() {
        out.extend_from_slice(b" (deleted)");
    }
    out
}

pub fn path_string(p: &PathRef) -> String {
    String::from_utf8_lossy(&path_of(p)).into_owned()
}

// ---------------------------------------------------------------------------
// Path resolution

fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|&c| c == b'/').filter(|c| !c.is_empty())
}

/// Resolve `path` relative to `start` (absolute paths from the root).
/// The last component is followed if it is a symlink only when `follow`.
pub fn lookup(start: &PathRef, path: &[u8], follow: bool) -> KResult<PathRef> {
    let mut depth = 0;
    walk(&root(), start, path, follow, &mut depth)
}

fn walk(root: &PathRef, start: &PathRef, path: &[u8], follow: bool, depth: &mut u32) -> KResult<PathRef> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    if path.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let mut cur = if path[0] == b'/' { root.clone() } else { start.clone() };
    let trailing_slash = path.ends_with(b"/");
    let comps: Vec<&[u8]> = components(path).collect();
    for (i, &c) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        if !cur.is_dir() {
            return Err(ENOTDIR);
        }
        match c {
            b"." => {}
            b".." => cur = parent_of(root, &cur),
            name => {
                if name.len() > NAME_MAX {
                    return Err(ENAMETOOLONG);
                }
                let next = child_of(&cur, name)?;
                if next.inode().meta().file_type() == S_IFLNK
                    && (!last || follow || trailing_slash)
                    && let Some(m) = next.inode().magic_link()
                {
                    cur = match m? {
                        MagicLink::Path(p) => p,
                        MagicLink::Inode(i) => PathRef { mnt: next.mnt.clone(), dentry: Dentry::new(i, name, Some(cur.dentry.clone())) },
                    };
                } else if next.inode().meta().file_type() == S_IFLNK && (!last || follow || trailing_slash) {
                    *depth += 1;
                    if *depth > MAX_SYMLINKS {
                        return Err(ELOOP);
                    }
                    let target = next.inode().readlink()?;
                    cur = walk(root, &cur, &target, true, depth)?;
                } else {
                    cur = next;
                }
            }
        }
    }
    if trailing_slash && !cur.is_dir() {
        return Err(ENOTDIR);
    }
    Ok(cur)
}

/// Resolve all but the last component: (directory, last name). The last
/// name is "." or ".." for paths like "/" or "a/..", which callers reject
/// as appropriate. A trailing slash is reported in the third field.
pub fn lookup_parent(start: &PathRef, path: &[u8]) -> KResult<(PathRef, Vec<u8>, bool)> {
    if path.is_empty() {
        return Err(ENOENT);
    }
    if path.len() >= PATH_MAX {
        return Err(ENAMETOOLONG);
    }
    let trailing = path.ends_with(b"/");
    let mut end = path.len();
    while end > 0 && path[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 {
        return Ok((root(), b".".to_vec(), true));
    }
    let trimmed = &path[..end];
    let (dir, name) = match trimmed.iter().rposition(|&c| c == b'/') {
        Some(i) => (&path[..i + 1], &trimmed[i + 1..]),
        None => (&b"."[..], trimmed),
    };
    if name.len() > NAME_MAX {
        return Err(ENAMETOOLONG);
    }
    let d = if dir == b"." { start.clone() } else { lookup(start, dir, true)? };
    if !d.is_dir() {
        return Err(ENOTDIR);
    }
    Ok((d, name.to_vec(), trailing))
}

// ---------------------------------------------------------------------------
// Namespace operations (keep the dentry cache in sync)

fn check_name(name: &[u8]) -> KResult<()> {
    if name == b"." || name == b".." {
        Err(EEXIST)
    } else {
        Ok(())
    }
}

/// A new file belongs to the caller's fs uid and fs gid, or to the
/// directory's group when it is set-group-ID (new directories inherit
/// the bit).
fn set_new_owner(dir: &dyn Inode, inode: &dyn Inode) {
    let Some(t) = crate::sched::try_current() else {
        return;
    };
    let (uid, mut gid) = {
        let c = t.creds.lock();
        (c.fsuid, c.fsgid)
    };
    let d = dir.meta().attr();
    let sgid_dir = d.mode & crate::process::S_ISGID != 0;
    if sgid_dir {
        gid = d.gid;
    }
    if uid == 0 && gid == 0 {
        return;
    }
    let is_dir = inode.meta().is_dir();
    inode.meta().update(|a| {
        a.uid = uid;
        a.gid = gid;
        if sgid_dir && is_dir {
            a.mode |= crate::process::S_ISGID;
        }
    });
}

pub fn create(dir: &PathRef, name: &[u8], mode: u32, rdev: u64) -> KResult<PathRef> {
    check_name(name)?;
    let inode = dir.inode().create(name, mode, rdev)?;
    set_new_owner(&**dir.inode(), &*inode);
    crate::security::label_new_inode(&*inode, mode & S_IFMT == S_IFSOCK);
    super::inotify::created(&**dir.inode(), name, &*inode);
    Ok(PathRef { mnt: dir.mnt.clone(), dentry: dir.dentry.attach(name, inode) })
}

pub fn symlink(dir: &PathRef, name: &[u8], target: &[u8]) -> KResult<()> {
    check_name(name)?;
    if target.is_empty() {
        return Err(ENOENT);
    }
    let inode = dir.inode().symlink(name, target)?;
    set_new_owner(&**dir.inode(), &*inode);
    crate::security::label_new_inode(&*inode, false);
    super::inotify::created(&**dir.inode(), name, &*inode);
    dir.dentry.attach(name, inode);
    Ok(())
}

pub fn link(old: &PathRef, dir: &PathRef, name: &[u8]) -> KResult<()> {
    check_name(name)?;
    if !Arc::ptr_eq(&old.mnt, &dir.mnt) {
        return Err(EXDEV);
    }
    if old.is_dir() {
        return Err(EPERM);
    }
    dir.inode().link(name, old.inode())?;
    super::inotify::created(&**dir.inode(), name, &**old.inode());
    super::inotify::touched(&**old.inode(), None, super::inotify::IN_ATTRIB);
    dir.dentry.attach(name, old.inode().clone());
    Ok(())
}

pub fn unlink(dir: &PathRef, name: &[u8], rmdir: bool) -> KResult<()> {
    match name {
        b"." => return Err(EINVAL),
        b".." => return Err(ENOTEMPTY),
        _ => {}
    }
    let target = child_of(dir, name)?;
    if !Arc::ptr_eq(&target.mnt, &dir.mnt) || mounted_on(&target).is_some() {
        return Err(EBUSY);
    }
    dir.inode().unlink(name, rmdir)?;
    dir.dentry.forget(name);
    super::inotify::deleted(&**dir.inode(), name, &**target.inode());
    Ok(())
}

pub fn rename(old_dir: &PathRef, old: &[u8], new_dir: &PathRef, new: &[u8], flags: u32) -> KResult<()> {
    if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0
        || flags & RENAME_NOREPLACE != 0 && flags & RENAME_EXCHANGE != 0
    {
        return Err(EINVAL);
    }
    if matches!(old, b"." | b"..") || matches!(new, b"." | b"..") {
        return Err(EBUSY);
    }
    if !Arc::ptr_eq(&old_dir.mnt, &new_dir.mnt) {
        return Err(EXDEV);
    }
    let src = child_of(old_dir, old)?;
    if !Arc::ptr_eq(&src.mnt, &old_dir.mnt) || mounted_on(&src).is_some() {
        return Err(EBUSY);
    }
    let dst = match child_of(new_dir, new) {
        Ok(d) => Some(d),
        Err(ENOENT) => None,
        Err(e) => return Err(e),
    };
    if let Some(d) = &dst {
        if Arc::ptr_eq(d.inode(), src.inode()) {
            return Ok(());
        }
        if !Arc::ptr_eq(&d.mnt, &new_dir.mnt) || mounted_on(d).is_some() {
            return Err(EBUSY);
        }
    }
    // A directory cannot move below itself (nor, when exchanging, the other).
    if src.is_dir() && new_dir.dentry.is_descendant_of(&src.dentry) {
        return Err(EINVAL);
    }
    if flags & RENAME_EXCHANGE != 0
        && let Some(d) = &dst
        && d.is_dir()
        && old_dir.dentry.is_descendant_of(&d.dentry)
    {
        return Err(EINVAL);
    }
    old_dir.inode().rename(old, new_dir.inode(), new, flags)?;
    if let Some(d) = &dst
        && flags & RENAME_EXCHANGE == 0
    {
        super::inotify::deleted(&**new_dir.inode(), new, &**d.inode());
    }
    super::inotify::moved(&**old_dir.inode(), old, &**new_dir.inode(), new, &**src.inode());
    if flags & RENAME_EXCHANGE != 0 {
        // Swap the two cached dentries through a temporary name.
        let tmp = b"\0rename-exchange";
        new_dir.dentry.moved(new, &new_dir.dentry, tmp);
        old_dir.dentry.moved(old, &new_dir.dentry, new);
        new_dir.dentry.moved(tmp, &old_dir.dentry, old);
    } else {
        new_dir.dentry.forget(new);
        old_dir.dentry.moved(old, &new_dir.dentry, new);
    }
    Ok(())
}
