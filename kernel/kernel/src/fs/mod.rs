//! File system layer for phase B1: the initramfs unpacked into a writable
//! in-memory tree (`ramfs`), open files, fd tables, pipes and the few
//! device nodes the tests need (console, null, zero, urandom).
//!
//! Planned (phase B2): VFS with dentries/inodes/mounts, tmpfs, devfs,
//! procfs, sysfs; later ext4/erofs and binderfs. Relative paths are
//! resolved from the root until processes get a cwd in B2.

pub mod file;
pub mod ramfs;

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Once;

use crate::errno::*;
use file::{FileKind, O_ACCMODE, O_CREAT, O_DIRECTORY, O_EXCL, O_RDONLY, O_TRUNC, OpenFile};
use ramfs::{Data, Inode, RamFs, S_IFMT, S_IFREG};

static FS: Once<RamFs> = Once::new();

/// Unpack the initramfs (if any) into the root file system.
pub fn init(initramfs: Option<&'static [u8]>) {
    let fs = FS.call_once(RamFs::new);
    match initramfs {
        Some(archive) => match fs.load_cpio(archive) {
            Ok(n) => log::info!("initramfs: {} entries, {} KiB", n, archive.len() / 1024),
            Err(e) => log::error!("initramfs: bad cpio archive ({e:?})"),
        },
        None => log::warn!("no initramfs module"),
    }
}

pub fn fs() -> &'static RamFs {
    FS.call_once(RamFs::new)
}

pub fn lookup(path: &[u8]) -> KResult<Arc<Inode>> {
    fs().lookup(&fs().root, path)
}

/// The file kind behind a character device node.
fn device(rdev: (u32, u32)) -> KResult<FileKind> {
    Ok(match rdev {
        (1, 3) => FileKind::Null,
        (1, 5) => FileKind::Zero,
        (1, 8) | (1, 9) => FileKind::Random,
        (5, 0) | (5, 1) | (4, _) => FileKind::Console,
        _ => return Err(ENODEV),
    })
}

pub fn open(path: &[u8], flags: u32, mode: u32) -> KResult<Arc<OpenFile>> {
    let fs = fs();
    let node = match fs.lookup(&fs.root, path) {
        Ok(n) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return Err(EEXIST);
            }
            n
        }
        Err(ENOENT) if flags & O_CREAT != 0 => {
            let (dir, name) = fs.lookup_parent(&fs.root, path)?;
            let n = Inode::new(S_IFREG | (mode & 0o7777), (0, 0), Data::Owned(Vec::new()));
            dir.link(name, n.clone())?;
            n
        }
        Err(e) => return Err(e),
    };
    if flags & O_DIRECTORY != 0 && !node.is_dir() {
        return Err(ENOTDIR);
    }
    if node.is_dir() && flags & O_ACCMODE != O_RDONLY {
        return Err(EISDIR);
    }
    let kind = if node.mode & S_IFMT == ramfs::S_IFCHR {
        device(node.rdev)?
    } else {
        if flags & O_TRUNC != 0 && node.is_reg() && flags & O_ACCMODE != O_RDONLY {
            node.data.lock().owned().clear();
        }
        FileKind::Inode(node)
    };
    Ok(OpenFile::new(kind, flags))
}

pub fn mkdir(path: &[u8], mode: u32) -> KResult<()> {
    let fs = fs();
    let (dir, name) = fs.lookup_parent(&fs.root, path)?;
    if name == b"." || name == b".." {
        return Err(EEXIST);
    }
    dir.link(name, Inode::new(ramfs::S_IFDIR | (mode & 0o7777), (0, 0), Data::Owned(Vec::new())))
}

pub fn unlink(path: &[u8], rmdir: bool) -> KResult<()> {
    let fs = fs();
    let (dir, name) = fs.lookup_parent(&fs.root, path)?;
    let mut children = dir.children.lock();
    let node = children.get(name).ok_or(ENOENT)?;
    match (rmdir, node.is_dir()) {
        (true, false) => return Err(ENOTDIR),
        (false, true) => return Err(EISDIR),
        (true, true) if !node.children.lock().is_empty() => return Err(ENOTEMPTY),
        _ => {}
    }
    children.remove(name);
    Ok(())
}

/// Console file (fds 0, 1, 2 of init).
pub fn console() -> Arc<OpenFile> {
    OpenFile::new(FileKind::Console, file::O_RDWR)
}
