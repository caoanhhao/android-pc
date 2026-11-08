//! In-memory file tree filled from the initramfs (cpio "newc" archive).
//!
//! Stopgap for phase B1 so that `execve` can find binaries and the B1 tests
//! can create scratch files under /tmp. It has no mounts, permissions,
//! links, symlink resolution or timestamps; the B2 VFS (dentries, inodes,
//! tmpfs, devfs, procfs) replaces it.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::errno::*;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFCHR: u32 = 0o020000;

pub enum Data {
    /// File contents still inside the initramfs module.
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
    pub fn owned(&mut self) -> &mut Vec<u8> {
        if let Data::Static(s) = self {
            *self = Data::Owned(s.to_vec());
        }
        match self {
            Data::Owned(v) => v,
            Data::Static(_) => unreachable!(),
        }
    }
}

pub struct Inode {
    pub ino: u64,
    pub mode: u32,
    /// (major, minor) for device nodes.
    pub rdev: (u32, u32),
    pub data: Mutex<Data>,
    pub children: Mutex<BTreeMap<Vec<u8>, Arc<Inode>>>,
    parent: Mutex<Weak<Inode>>,
}

static NEXT_INO: AtomicU64 = AtomicU64::new(1);

impl Inode {
    pub fn new(mode: u32, rdev: (u32, u32), data: Data) -> Arc<Inode> {
        Arc::new(Inode {
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
            mode,
            rdev,
            data: Mutex::new(data),
            children: Mutex::new(BTreeMap::new()),
            parent: Mutex::new(Weak::new()),
        })
    }

    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub fn size(&self) -> usize {
        self.data.lock().bytes().len()
    }

    fn parent(self: &Arc<Self>) -> Arc<Inode> {
        self.parent.lock().upgrade().unwrap_or_else(|| self.clone())
    }

    pub fn lookup_child(&self, name: &[u8]) -> Option<Arc<Inode>> {
        self.children.lock().get(name).cloned()
    }

    /// Add `name` -> `child`; fails with EEXIST if the name is taken.
    pub fn link(self: &Arc<Self>, name: &[u8], child: Arc<Inode>) -> KResult<()> {
        let mut c = self.children.lock();
        if c.contains_key(name) {
            return Err(EEXIST);
        }
        *child.parent.lock() = Arc::downgrade(self);
        c.insert(name.to_vec(), child);
        Ok(())
    }
}

pub struct RamFs {
    pub root: Arc<Inode>,
}

impl RamFs {
    pub fn new() -> Self {
        let root = Inode::new(S_IFDIR | 0o755, (0, 0), Data::Owned(Vec::new()));
        Self { root }
    }

    /// Resolve `path` from `cwd` (absolute paths start at the root).
    pub fn lookup(&self, cwd: &Arc<Inode>, path: &[u8]) -> KResult<Arc<Inode>> {
        if path.is_empty() {
            return Err(ENOENT);
        }
        let mut node = if path[0] == b'/' { self.root.clone() } else { cwd.clone() };
        for comp in path.split(|&c| c == b'/').filter(|c| !c.is_empty()) {
            if !node.is_dir() {
                return Err(ENOTDIR);
            }
            node = match comp {
                b"." => node,
                b".." => node.parent(),
                name => node.lookup_child(name).ok_or(ENOENT)?,
            };
        }
        Ok(node)
    }

    /// Resolve all but the last component: (directory, last name).
    pub fn lookup_parent<'p>(&self, cwd: &Arc<Inode>, path: &'p [u8]) -> KResult<(Arc<Inode>, &'p [u8])> {
        let trimmed = {
            let mut p = path;
            while p.len() > 1 && p.ends_with(b"/") {
                p = &p[..p.len() - 1];
            }
            p
        };
        let (dir, name) = match trimmed.iter().rposition(|&c| c == b'/') {
            Some(0) => (&b"/"[..], &trimmed[1..]),
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => (&b"."[..], trimmed),
        };
        if name.is_empty() {
            return Err(ENOENT);
        }
        if name.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        let d = self.lookup(cwd, dir)?;
        if !d.is_dir() {
            return Err(ENOTDIR);
        }
        Ok((d, name))
    }

    /// Create every missing directory of `path` (cpio entries may come
    /// before their parent directory).
    fn mkdir_p(&self, path: &[u8]) -> Arc<Inode> {
        let mut node = self.root.clone();
        for comp in path.split(|&c| c == b'/').filter(|c| !c.is_empty() && *c != b".") {
            let next = node.lookup_child(comp);
            node = match next {
                Some(n) => n,
                None => {
                    let d = Inode::new(S_IFDIR | 0o755, (0, 0), Data::Owned(Vec::new()));
                    let _ = node.link(comp, d.clone());
                    d
                }
            };
        }
        node
    }

    /// Unpack a "newc" cpio archive. Returns the number of entries.
    pub fn load_cpio(&self, archive: &'static [u8]) -> KResult<usize> {
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
            let filesize = field(6)? as usize;
            let (rmaj, rmin) = (field(9)?, field(10)?);
            let namesize = field(11)? as usize;
            let name_start = off + 110;
            let name = archive.get(name_start..name_start + namesize - 1).ok_or(EINVAL)?;
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
            let parent = self.mkdir_p(dir);
            if mode & S_IFMT == S_IFDIR {
                if parent.lookup_child(base).is_none() {
                    parent.link(base, Inode::new(mode, (0, 0), Data::Owned(Vec::new())))?;
                }
                continue;
            }
            let node = Inode::new(mode, (rmaj, rmin), Data::Static(data));
            parent.children.lock().remove(base);
            parent.link(base, node)?;
        }
    }
}

#[test_case]
fn cpio_unpack_and_lookup() {
    // Two entries + trailer, as written by tools/mkcpio.py.
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
    entry(&mut a, "TRAILER!!!", 0, b"");
    let a: &'static [u8] = alloc::boxed::Box::leak(a.into_boxed_slice());
    let fs = RamFs::new();
    assert_eq!(fs.load_cpio(a), Ok(2));
    let f = fs.lookup(&fs.root, b"/bin/../bin/./hello").unwrap();
    assert!(f.is_reg());
    assert_eq!(f.data.lock().bytes(), b"hi!");
    assert_eq!(fs.lookup(&fs.root, b"/bin/nope").err(), Some(ENOENT));
    assert_eq!(fs.lookup(&fs.root, b"/bin/hello/x").err(), Some(ENOTDIR));
}
