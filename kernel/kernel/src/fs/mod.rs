//! File systems.
//!
//! - `vfs`: inodes, dentries, mounts, path resolution (one mount namespace).
//! - `file`: open files, the generic file operations, fd tables.
//! - `tmpfs`: in-memory read-write file system; the root file system is a
//!   tmpfs filled from the initramfs (cpio "newc").
//! - `dev`: character devices (null, zero, random, console, kmsg, ...) and
//!   devtmpfs. `procfs`, `sysfs` (device tree with uevent files), `pipe`.
//! - `selinuxfs` (SELinux stub interface), `cgroupfs` (cgroup v1 and v2),
//!   `inotify`, `pty` (/dev/ptmx and devpts), `sysctl` (/proc/sys).
//!
//! File data of tmpfs (and memfd) lives in `mm::shmem` objects, the page
//! cache that `mmap` maps.
//!
//! Disk file systems (read-only for now): `erofs` (with `lz4`) and `ext4`,
//! mounted from block devices (`crate::block`).
//!
//! binderfs lives with the binder driver (`drivers::binder`).
//!
//! Planned: ext4 writes.

pub mod cgroupfs;
pub mod dev;
pub mod epoll;
pub mod erofs;
pub mod eventfd;
pub mod ext4;
pub mod lz4;
pub mod file;
pub mod inotify;
pub mod kmsg;
pub mod pipe;
pub mod poll;
pub mod signalfd;
pub mod timerfd;
pub mod procfs;
pub mod pty;
pub mod selinuxfs;
pub mod sysctl;
pub mod sysfs;
pub mod tmpfs;
pub mod tty;
pub mod vfs;

use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Once;

use crate::errno::*;
use file::*;
use tmpfs::{Data, TmpInode};
use vfs::{Dentry, Inode, PathRef, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK};

pub use dev::console;

/// Unpack the initramfs (if any) into a tmpfs and make it the root.
pub fn init(initramfs: Option<&'static [u8]>) {
    let root = TmpInode::new_fs(0o755);
    match initramfs {
        Some(archive) => match load_cpio(&root, archive) {
            Ok(n) => log::info!("initramfs: {} entries, {} KiB", n, archive.len() / 1024),
            Err(e) => log::error!("initramfs: bad cpio archive ({e:?})"),
        },
        None => log::warn!("no initramfs module"),
    }
    vfs::set_root(root, "rootfs");
    sysfs::init();
}

/// Create every missing directory of `path` below `root`.
fn mkdir_p(root: &Arc<TmpInode>, path: &[u8]) -> KResult<Arc<TmpInode>> {
    let mut node = root.clone();
    for comp in path.split(|&c| c == b'/').filter(|c| !c.is_empty() && *c != b".") {
        node = match node.get(comp) {
            Some(n) => n,
            None => node.add(comp, S_IFDIR | 0o755, 0, None)?,
        };
    }
    Ok(node)
}

/// Unpack a "newc" cpio archive into `root`. Returns the number of entries.
pub fn load_cpio(root: &Arc<TmpInode>, archive: &'static [u8]) -> KResult<usize> {
    let hex = |b: &[u8]| -> KResult<u32> {
        let s = core::str::from_utf8(b).map_err(|_| EINVAL)?;
        u32::from_str_radix(s, 16).map_err(|_| EINVAL)
    };
    let mut off = 0usize;
    let mut count = 0;
    loop {
        let h = archive.get(off..off + 110).ok_or(EINVAL)?;
        if &h[..6] != b"070701" && &h[..6] != b"070702" {
            return Err(EINVAL);
        }
        let field = |i: usize| hex(&h[6 + i * 8..14 + i * 8]);
        let mode = field(1)?;
        let (uid, gid, mtime) = (field(2)?, field(3)?, field(5)?);
        let filesize = field(6)? as usize;
        let (rmaj, rmin) = (field(9)?, field(10)?);
        let namesize = field(11)? as usize;
        let name_start = off + 110;
        let name = archive.get(name_start..name_start + namesize.max(1) - 1).ok_or(EINVAL)?;
        let data_start = (name_start + namesize).next_multiple_of(4);
        let data = archive.get(data_start..data_start + filesize).ok_or(EINVAL)?;
        off = (data_start + filesize).next_multiple_of(4);
        if name == b"TRAILER!!!" {
            return Ok(count);
        }
        count += 1;
        let path = name.strip_prefix(b"./").unwrap_or(name);
        if path == b"." || path.is_empty() {
            continue;
        }
        let (dir, base) = match path.iter().rposition(|&c| c == b'/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => (&b""[..], path),
        };
        let parent = mkdir_p(root, dir)?;
        let node = match mode & S_IFMT {
            S_IFDIR => match parent.get(base) {
                Some(d) => d,
                None => parent.add(base, mode, 0, None)?,
            },
            S_IFREG | S_IFLNK => parent.replace(base, mode, 0, Some(Data::Static(data)))?,
            S_IFCHR | vfs::S_IFBLK | S_IFIFO | S_IFSOCK => {
                parent.replace(base, mode, vfs::encode_dev(rmaj, rmin), None)?
            }
            _ => continue,
        };
        node.set_owner(uid, gid, mtime as i64 * 1_000_000_000);
    }
}

/// Read a whole regular file (execve).
pub fn read_all(inode: &Arc<dyn Inode>) -> KResult<Vec<u8>> {
    let size = inode.size() as usize;
    let mut v = Vec::new();
    v.try_reserve_exact(size).map_err(|_| ENOMEM)?;
    v.resize(size, 0);
    let mut done = 0;
    while done < size {
        let n = inode.read_at(done as u64, &mut v[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    v.truncate(done);
    Ok(v)
}

/// Open `path` (relative to `base`). `mode` is already masked by umask.
pub fn open(base: &PathRef, path: &[u8], flags: u32, mode: u32) -> KResult<Arc<OpenFile>> {
    if flags & O_TMPFILE == O_TMPFILE {
        return Err(EOPNOTSUPP);
    }
    let follow = flags & O_NOFOLLOW == 0;
    let p = if flags & O_CREAT != 0 && flags & O_PATH == 0 {
        open_create(base, path, flags, mode)?
    } else {
        vfs::lookup(base, path, follow)?
    };
    open_path(p, flags)
}

fn open_create(base: &PathRef, path: &[u8], flags: u32, mode: u32) -> KResult<PathRef> {
    let (dir, name, trailing) = vfs::lookup_parent(base, path)?;
    if name == b"." || name == b".." {
        return Err(EISDIR);
    }
    match vfs::child_of(&dir, &name) {
        Ok(c) => {
            if flags & O_EXCL != 0 {
                return Err(EEXIST);
            }
            if c.inode().meta().file_type() != S_IFLNK {
                return Ok(c);
            }
            if flags & O_NOFOLLOW != 0 {
                return Err(ELOOP);
            }
            match vfs::lookup(base, path, true) {
                // A dangling symlink: create its target (one level).
                Err(ENOENT) => {
                    let target = c.inode().readlink()?;
                    let (tdir, tname, _) = vfs::lookup_parent(&dir, &target)?;
                    vfs::create(&tdir, &tname, S_IFREG | (mode & 0o7777), 0)
                }
                r => r,
            }
        }
        Err(ENOENT) => {
            if trailing {
                return Err(EISDIR);
            }
            vfs::create(&dir, &name, S_IFREG | (mode & 0o7777), 0)
        }
        Err(e) => Err(e),
    }
}

/// Open an already resolved path.
pub fn open_path(p: PathRef, flags: u32) -> KResult<Arc<OpenFile>> {
    let inode = p.inode().clone();
    let ty = inode.meta().file_type();
    if flags & O_DIRECTORY != 0 && ty != S_IFDIR {
        return Err(ENOTDIR);
    }
    if flags & O_PATH != 0 {
        let keep = O_PATH | (flags & (O_DIRECTORY | O_NOFOLLOW));
        return Ok(OpenFile::new(inode, Some(p), alloc::boxed::Box::new(NoIo), keep));
    }
    let acc = flags & O_ACCMODE;
    if acc == 3 {
        return Err(EINVAL);
    }
    let ops = match ty {
        S_IFLNK => return Err(ELOOP),
        S_IFDIR => {
            if acc != O_RDONLY || flags & O_CREAT != 0 {
                return Err(EISDIR);
            }
            inode.open(flags)?.unwrap_or_else(|| default_ops(ty))
        }
        S_IFCHR => dev::open_chr(inode.meta().rdev)?,
        S_IFIFO => pipe::open_fifo(&inode, flags)?,
        vfs::S_IFBLK => crate::block::open_blk(inode.meta().rdev, flags)?,
        S_IFSOCK => return Err(ENXIO),
        _ => {
            if acc != O_RDONLY && (inode.read_only_fs() || p.mnt.flags & MS_RDONLY != 0) {
                return Err(EROFS);
            }
            if flags & O_TRUNC != 0 && acc != O_RDONLY {
                inode.truncate(0)?;
            }
            inode.open(flags)?.unwrap_or_else(|| default_ops(ty))
        }
    };
    Ok(OpenFile::new(inode, Some(p), ops, flags))
}

/// The inode shared by every anonymous-inode file (eventfd, epoll,
/// timerfd, signalfd): mode 0600 with no file type, like Linux's.
pub fn anon_inode() -> Arc<dyn Inode> {
    static ANON: Once<Arc<dyn Inode>> = Once::new();
    ANON.call_once(|| vfs::AnonInode::new(0o600, 0)).clone()
}

/// The name /proc/<pid>/fd/N shows for an open file.
pub fn file_path(f: &OpenFile) -> Vec<u8> {
    if let Some(p) = &f.path {
        return vfs::path_of(p);
    }
    if let Some(name) = f.ops.anon_name() {
        return format!("anon_inode:{name}").into_bytes();
    }
    if let Some(name) = f.inode.as_any().downcast_ref::<TmpInode>().and_then(|t| t.memfd_name()) {
        let mut v = b"/memfd:".to_vec();
        v.extend_from_slice(name);
        v.extend_from_slice(b" (deleted)");
        return v;
    }
    let meta = f.inode.meta();
    match meta.file_type() {
        S_IFIFO => format!("pipe:[{}]", meta.ino).into_bytes(),
        S_IFSOCK => format!("socket:[{}]", meta.ino).into_bytes(),
        S_IFCHR => b"/dev/console".to_vec(),
        vfs::S_IFBLK => match crate::block::lookup_rdev(meta.rdev) {
            Some(d) => format!("/dev/block/{}", d.name).into_bytes(),
            None => b"/dev/block/unknown".to_vec(),
        },
        _ => format!("anon_inode:[{}]", meta.ino).into_bytes(),
    }
}

// ---------------------------------------------------------------------------
// Mountable file systems

pub const TMPFS_MAGIC: u64 = 0x0102_1994;
pub const RAMFS_MAGIC: u64 = 0x8584_58f6;
pub const PROC_SUPER_MAGIC: u64 = 0x9fa0;
pub const SYSFS_MAGIC: u64 = 0x6265_6572;

pub const MS_RDONLY: u64 = 1;

pub fn fs_magic(fstype: &str) -> u64 {
    match fstype {
        "erofs" => erofs::EROFS_SUPER_MAGIC as u64,
        "ext4" | "ext3" | "ext2" => ext4::EXT4_SUPER_MAGIC as u64,
        "proc" => PROC_SUPER_MAGIC,
        "sysfs" => SYSFS_MAGIC,
        "ramfs" | "rootfs" => RAMFS_MAGIC,
        "binder" => crate::drivers::binder::BINDERFS_SUPER_MAGIC,
        "selinuxfs" => selinuxfs::SELINUX_MAGIC,
        "cgroup" => cgroupfs::CGROUP_SUPER_MAGIC,
        "cgroup2" => cgroupfs::CGROUP2_SUPER_MAGIC,
        "devpts" => 0x1cd1,
        "tracefs" => 0x7472_6163,
        "debugfs" => 0x6462_6720,
        "pstore" => 0x6165_676c,
        "bpf" => 0xcafe_4a11,
        "configfs" => 0x6265_6570,
        "fusectl" => 0x6573_5546,
        _ => TMPFS_MAGIC,
    }
}

static DEVTMPFS: Once<Arc<Dentry>> = Once::new();
static DEVTMPFS_ROOT: Once<Arc<TmpInode>> = Once::new();
static SYSFS: Once<Arc<Dentry>> = Once::new();
static DEVPTS: Once<Arc<Dentry>> = Once::new();

/// The shared devtmpfs instance (drivers add their nodes to it).
pub fn devtmpfs_root() -> Arc<TmpInode> {
    DEVTMPFS_ROOT
        .call_once(|| {
            let root = dev::new_devtmpfs();
            crate::block::populate_devtmpfs(&root);
            root
        })
        .clone()
}

/// The sysfs tree (see `sysfs`).
pub fn sysfs_root() -> Arc<TmpInode> {
    sysfs::root()
}

/// Mount a disk file system of type `fstype` from the block device (or
/// "major:minor") `source`. Read-only file systems force MS_RDONLY.
/// ENOTBLK when `source` is not a block device (toybox mount then sets up
/// a loop device), ENODEV for unknown types.
pub fn new_block_fs(fstype: &[u8], source: &[u8], flags: u64) -> KResult<Option<(Arc<Dentry>, &'static str, u64)>> {
    let name = match fstype {
        b"erofs" => "erofs",
        b"ext4" => "ext4",
        b"ext3" => "ext3",
        b"ext2" => "ext2",
        _ => return Ok(None),
    };
    let dev = crate::block::resolve(source)?;
    if dev.size() == 0 {
        return Err(ENXIO);
    }
    let root = match name {
        "erofs" => erofs::mount(dev)?,
        _ => ext4::mount(dev)?,
    };
    if flags & MS_RDONLY == 0 {
        log::warn!("mount: {name} is read-only for now; mounting {} ro", alloc::string::String::from_utf8_lossy(source));
    }
    Ok(Some((Dentry::new_root(root), name, flags | MS_RDONLY)))
}

/// A new instance of `fstype` (devtmpfs and sysfs are single shared
/// instances, like on Linux). Returns its root dentry and the type name.
#[cfg(test)]
pub fn new_fs(fstype: &[u8], mode: Option<u32>) -> KResult<(Arc<Dentry>, &'static str)> {
    new_fs_opts(fstype, mode, b"")
}

/// [`new_fs`] with the mount's data string (cgroup v1 controller lists).
pub fn new_fs_opts(fstype: &[u8], mode: Option<u32>, data: &[u8]) -> KResult<(Arc<Dentry>, &'static str)> {
    let fresh = |i: Arc<dyn Inode>| Dentry::new_root(i);
    let (root, name) = match fstype {
        b"selinuxfs" => (selinuxfs::mount_root(), "selinuxfs"),
        b"cgroup2" => (cgroupfs::mount(None)?, "cgroup2"),
        b"cgroup" => (cgroupfs::mount(Some(data))?, "cgroup"),
        // Pseudo file systems with nothing behind them yet: empty
        // instances, so init's mounts succeed.
        b"devpts" => (DEVPTS.call_once(|| Dentry::new_root(pty::devpts_root())).clone(), "devpts"),
        b"tracefs" => (fresh(TmpInode::new_fs(0o700)), "tracefs"),
        b"debugfs" => (fresh(TmpInode::new_fs(0o700)), "debugfs"),
        b"pstore" => (fresh(TmpInode::new_fs(0o750)), "pstore"),
        b"bpf" => (fresh(TmpInode::new_fs(0o700)), "bpf"),
        b"configfs" => (fresh(TmpInode::new_fs(0o755)), "configfs"),
        b"fusectl" => (fresh(TmpInode::new_fs(0o755)), "fusectl"),
        b"functionfs" => (fresh(TmpInode::new_fs(0o755)), "functionfs"),
        b"tmpfs" => (fresh(TmpInode::new_fs(mode.unwrap_or(0o1777))), "tmpfs"),
        b"ramfs" => (fresh(TmpInode::new_fs(mode.unwrap_or(0o755))), "ramfs"),
        b"devtmpfs" => (DEVTMPFS.call_once(|| Dentry::new_root(devtmpfs_root())).clone(), "devtmpfs"),
        b"proc" => (fresh(procfs::new_procfs()), "proc"),
        b"sysfs" => (SYSFS.call_once(|| Dentry::new_root(sysfs_root())).clone(), "sysfs"),
        b"binder" => (fresh(crate::drivers::binder::new_binderfs(mode)), "binder"),
        _ => return Err(ENODEV),
    };
    if let Some(m) = mode
        && name != "proc"
    {
        root.inode.meta().update(|a| a.mode = S_IFDIR | (m & 0o7777));
    }
    Ok((root, name))
}

#[cfg(test)]
fn test_cpio() -> &'static [u8] {
    // Entries + trailer, as written by tools/mkcpio.py.
    fn entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let fields = [1, mode, 0, 0, 1, 0, data.len() as u32, 0, 0, 0, 0, name.len() as u32 + 1, 0];
        out.extend_from_slice(b"070701");
        for f in fields {
            out.extend_from_slice(alloc::format!("{f:08X}").as_bytes());
        }
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    let mut a = Vec::new();
    entry(&mut a, "bin", S_IFDIR | 0o755, b"");
    entry(&mut a, "bin/hello", S_IFREG | 0o755, b"hi!");
    entry(&mut a, "lib/link", S_IFLNK | 0o777, b"../bin/hello");
    entry(&mut a, "mnt", S_IFDIR | 0o755, b"");
    entry(&mut a, "TRAILER!!!", 0, b"");
    alloc::boxed::Box::leak(a.into_boxed_slice())
}

#[test_case]
fn vfs_cpio_paths_and_mounts() {
    let root = TmpInode::new_fs(0o755);
    assert_eq!(load_cpio(&root, test_cpio()), Ok(4));
    vfs::set_root(root, "rootfs");
    let r = vfs::root();
    let f = vfs::lookup(&r, b"/bin/../bin/./hello", true).unwrap();
    assert_eq!(f.inode().meta().file_type(), S_IFREG);
    assert_eq!(read_all(f.inode()).unwrap(), b"hi!");
    assert_eq!(vfs::lookup(&r, b"/bin/nope", true).err(), Some(ENOENT));
    assert_eq!(vfs::lookup(&r, b"/bin/hello/x", true).err(), Some(ENOTDIR));
    // Symlinks: followed or not, relative to the link's directory.
    let l = vfs::lookup(&r, b"/lib/link", true).unwrap();
    assert!(Arc::ptr_eq(l.inode(), f.inode()));
    let l = vfs::lookup(&r, b"/lib/link", false).unwrap();
    assert_eq!(l.inode().meta().file_type(), S_IFLNK);
    // Mount a tmpfs, create in it, ".." leads back out of it.
    let mnt = vfs::lookup(&r, b"/mnt", true).unwrap();
    let (troot, name) = new_fs(b"tmpfs", None).unwrap();
    vfs::mount(&mnt, troot, name, b"tmpfs", 0).unwrap();
    let m = vfs::lookup(&r, b"/mnt", true).unwrap();
    assert_eq!(m.mnt.fstype, "tmpfs");
    let d = vfs::create(&m, b"dir", S_IFDIR | 0o755, 0).unwrap();
    assert_eq!(vfs::path_of(&d), b"/mnt/dir");
    let up = vfs::lookup(&d, b"../../bin/hello", true).unwrap();
    assert!(Arc::ptr_eq(up.inode(), f.inode()));
    // Rename keeps the dentry (and its path) in sync; unlink marks it.
    vfs::rename(&m, b"dir", &m, b"moved", 0).unwrap();
    assert_eq!(vfs::path_of(&d), b"/mnt/moved");
    assert_eq!(vfs::unlink(&m, b"moved", false).err(), Some(EISDIR));
    vfs::unlink(&m, b"moved", true).unwrap();
    assert!(d.dentry.is_deleted());
    assert_eq!(vfs::umount(&m), Ok(()));
    assert_eq!(vfs::lookup(&r, b"/mnt", true).unwrap().mnt.fstype, "rootfs");
}
