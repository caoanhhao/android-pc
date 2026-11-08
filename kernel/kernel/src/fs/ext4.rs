//! ext4 (also ext2/ext3 images), read-only.
//!
//! Supported: 1-4 KiB blocks, 32/64-bit group descriptors (flex_bg),
//! extent trees and indirect block maps (holes and uninitialised extents
//! read as zeros), linear and htree directories (htree index blocks look
//! like empty entries, so a linear scan of every directory block finds
//! all names), fast and slow symlinks, device nodes, inline data, xattrs
//! in the inode body and in an xattr block (EA-inode values too), 64-bit
//! sizes and nanosecond times. The journal is not replayed (an image that
//! needs recovery is mounted with a warning); meta_bg, encryption and
//! block sizes over 4 KiB are refused. Writing (for /data) comes later.
//!
//! File data goes through a per-inode page cache (`Shmem::from_source`);
//! metadata through the block device's cache.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use spin::{Mutex, Once};

use super::vfs::*;
use crate::block::{BlockDev, Holder};
use crate::errno::*;
use crate::mm::shmem::{PageSource, Shmem};

pub const EXT4_SUPER_MAGIC: u16 = 0xef53;

const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_MMP: u32 = 0x100;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_EA_INODE: u32 = 0x400;
const INCOMPAT_CSUM_SEED: u32 = 0x2000;
const INCOMPAT_LARGEDIR: u32 = 0x4000;
const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;
const SUPPORTED_INCOMPAT: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_RECOVER
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_MMP
    | INCOMPAT_FLEX_BG
    | INCOMPAT_EA_INODE
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR
    | INCOMPAT_INLINE_DATA
    | INCOMPAT_CASEFOLD;
const RO_COMPAT_HUGE_FILE: u32 = 0x8;

const FL_HUGE_FILE: u32 = 0x4_0000;
const FL_EXTENTS: u32 = 0x8_0000;
const FL_INLINE_DATA: u32 = 0x1000_0000;

const XATTR_MAGIC: u32 = 0xea02_0000;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub struct Fs {
    dev: Arc<BlockDev>,
    _holder: Holder,
    devno: u64,
    bs: u64,
    inode_size: u64,
    inodes_per_group: u64,
    first_data_block: u64,
    desc_size: u64,
    group_count: u64,
    filetype: bool,
    huge_file: bool,
    blocks: u64,
    free_blocks: u64,
    inodes_count: u64,
    free_inodes: u64,
    inodes: Mutex<BTreeMap<u64, Weak<Ext4Inode>>>,
}

impl Fs {
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        self.dev.read(off, buf)
    }

    fn read_block(&self, blk: u64) -> KResult<Vec<u8>> {
        let mut v = vec![0u8; self.bs as usize];
        self.read(blk * self.bs, &mut v)?;
        Ok(v)
    }

    fn inode_pos(&self, ino: u64) -> KResult<u64> {
        if ino == 0 || ino > self.inodes_count {
            return Err(EUCLEAN);
        }
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        if group >= self.group_count {
            return Err(EUCLEAN);
        }
        let mut d = [0u8; 64];
        let gd = (self.first_data_block + 1) * self.bs + group * self.desc_size;
        self.read(gd, &mut d[..self.desc_size as usize])?;
        let mut table = le32(&d, 8) as u64;
        if self.desc_size >= 64 {
            table |= (le32(&d, 0x28) as u64) << 32;
        }
        Ok(table * self.bs + index * self.inode_size)
    }

    fn inode(self: &Arc<Self>, ino: u64) -> KResult<Arc<Ext4Inode>> {
        if let Some(i) = self.inodes.lock().get(&ino).and_then(Weak::upgrade) {
            return Ok(i);
        }
        let data = Arc::new(self.load(ino)?);
        let meta = Meta::with_ino(ino, self.devno, data.attr.mode, data.rdev);
        meta.update(|m| *m = data.attr);
        let inode = Arc::new(Ext4Inode { meta, data, cache: Once::new() });
        let mut map = self.inodes.lock();
        if let Some(i) = map.get(&ino).and_then(Weak::upgrade) {
            return Ok(i);
        }
        if map.len() >= 256 && map.len().is_power_of_two() {
            map.retain(|_, w| w.strong_count() > 0);
        }
        map.insert(ino, Arc::downgrade(&inode));
        Ok(inode)
    }

    fn load(self: &Arc<Self>, ino: u64) -> KResult<IData> {
        let pos = self.inode_pos(ino)?;
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read(pos, &mut raw)?;
        let b = &raw;
        let mode = le16(b, 0) as u32;
        let extra = if self.inode_size > 128 { le16(b, 128) as usize } else { 0 };
        let time = |sec_off: usize, extra_off: usize| -> i64 {
            let mut s = le32(b, sec_off) as i32 as i64;
            let mut ns = 0;
            if extra_off + 4 <= 128 + extra {
                let x = le32(b, extra_off);
                s += ((x & 3) as i64) << 32;
                ns = (x >> 2) as i64;
            }
            s * 1_000_000_000 + ns
        };
        let attr = Attr {
            mode,
            uid: le16(b, 2) as u32 | (le16(b, 120) as u32) << 16,
            gid: le16(b, 24) as u32 | (le16(b, 122) as u32) << 16,
            nlink: le16(b, 26) as u32,
            atime: time(8, 140),
            ctime: time(12, 132),
            mtime: time(16, 136),
        };
        let size = le32(b, 4) as u64 | (le32(b, 108) as u64) << 32;
        let flags = le32(b, 32);
        let mut i_blocks = le32(b, 28) as u64 | (le16(b, 116) as u64) << 32;
        if self.huge_file && flags & FL_HUGE_FILE != 0 {
            i_blocks *= self.bs / 512;
        }
        let file_acl = le32(b, 104) as u64 | (le16(b, 118) as u64) << 32;
        let mut iblock = [0u8; 60];
        iblock.copy_from_slice(&b[40..100]);
        let rdev = match mode & S_IFMT {
            S_IFCHR | S_IFBLK => {
                let (old, new) = (le32(b, 40), le32(b, 44));
                if old != 0 {
                    encode_dev((old >> 8) & 0xff, old & 0xff)
                } else {
                    encode_dev((new & 0xfff00) >> 8, (new & 0xff) | ((new >> 12) & 0xfff00))
                }
            }
            _ => 0,
        };
        let ibody_xattr = if self.inode_size > 128 + extra as u64 + 4 && le32(b, 128 + extra) == XATTR_MAGIC {
            raw[128 + extra + 4..].to_vec()
        } else {
            Vec::new()
        };
        let ea_blocks = if file_acl != 0 { self.bs / 512 } else { 0 };
        // Linux's ext4_inode_is_fast_symlink.
        let fast_symlink =
            mode & S_IFMT == S_IFLNK && flags & FL_INLINE_DATA == 0 && i_blocks.saturating_sub(ea_blocks) == 0;
        Ok(IData {
            fs: self.clone(),
            size,
            flags,
            iblock,
            file_acl,
            rdev,
            attr,
            ibody_xattr,
            fast_symlink,
        })
    }
}

pub struct IData {
    fs: Arc<Fs>,
    size: u64,
    flags: u32,
    iblock: [u8; 60],
    file_acl: u64,
    rdev: u64,
    attr: Attr,
    /// The in-inode xattr area (after the magic), if any.
    ibody_xattr: Vec<u8>,
    fast_symlink: bool,
}

impl IData {
    /// Physical block of logical block `lblk` and how many following
    /// blocks are contiguous with it; `None` for a hole (zeros).
    fn bmap(&self, lblk: u64) -> KResult<(Option<u64>, u64)> {
        if self.flags & FL_EXTENTS != 0 {
            return self.extent_map(lblk);
        }
        self.indirect_map(lblk)
    }

    fn extent_map(&self, lblk: u64) -> KResult<(Option<u64>, u64)> {
        let mut node: Vec<u8> = self.iblock.to_vec();
        for _ in 0..8 {
            if le16(&node, 0) != 0xf30a {
                return Err(EUCLEAN);
            }
            let entries = le16(&node, 2) as usize;
            let depth = le16(&node, 6);
            if 12 + entries * 12 > node.len() {
                return Err(EUCLEAN);
            }
            // Last entry starting at or before lblk.
            let mut pick = None;
            for i in 0..entries {
                let e = 12 + i * 12;
                if le32(&node, e) as u64 <= lblk {
                    pick = Some(e);
                } else {
                    break;
                }
            }
            let Some(e) = pick else {
                // Hole before the first extent.
                let next = if entries > 0 { le32(&node, 12) as u64 - lblk } else { u64::MAX };
                return Ok((None, next.max(1)));
            };
            if depth == 0 {
                let first = le32(&node, e) as u64;
                let mut len = le16(&node, e + 4) as u64;
                let uninit = len > 32768;
                if uninit {
                    len -= 32768;
                }
                let start = (le16(&node, e + 6) as u64) << 32 | le32(&node, e + 8) as u64;
                if lblk >= first + len {
                    // Hole up to the next extent.
                    let next = if e + 12 < 12 + entries * 12 { le32(&node, e + 12) as u64 - lblk } else { u64::MAX };
                    return Ok((None, next.max(1)));
                }
                let left = first + len - lblk;
                return Ok((if uninit { None } else { Some(start + lblk - first) }, left));
            }
            let leaf = (le16(&node, e + 8) as u64) << 32 | le32(&node, e + 4) as u64;
            node = self.fs.read_block(leaf)?;
        }
        Err(EUCLEAN)
    }

    fn indirect_map(&self, lblk: u64) -> KResult<(Option<u64>, u64)> {
        let per = self.fs.bs / 4;
        let direct = |i: usize| le32(&self.iblock, i * 4) as u64;
        let (mut blk, mut idx) = if lblk < 12 {
            let b = direct(lblk as usize);
            return Ok((if b == 0 { None } else { Some(b) }, 1));
        } else if lblk < 12 + per {
            (direct(12), vec![lblk - 12])
        } else if lblk < 12 + per + per * per {
            let r = lblk - 12 - per;
            (direct(13), vec![r / per, r % per])
        } else {
            let r = lblk - 12 - per - per * per;
            (direct(14), vec![r / (per * per), (r / per) % per, r % per])
        };
        for i in idx.drain(..) {
            if blk == 0 {
                return Ok((None, 1));
            }
            let mut e = [0u8; 4];
            self.fs.read(blk * self.fs.bs + i * 4, &mut e)?;
            blk = u32::from_le_bytes(e) as u64;
        }
        Ok((if blk == 0 { None } else { Some(blk) }, 1))
    }

    fn inline_data(&self) -> KResult<Vec<u8>> {
        let mut v = self.iblock.to_vec();
        if let Some((_, extra)) = self.xattrs()?.into_iter().find(|(n, _)| n == b"system.data") {
            v.extend_from_slice(&extra);
        }
        v.truncate(self.size as usize);
        Ok(v)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if off >= self.size {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.size - off) as usize;
        if self.flags & FL_INLINE_DATA != 0 || self.fast_symlink {
            let data = if self.fast_symlink { self.iblock[..(self.size as usize).min(60)].to_vec() } else { self.inline_data()? };
            let avail = data.len().saturating_sub(off as usize).min(n);
            buf[..avail].copy_from_slice(&data[off as usize..off as usize + avail]);
            buf[avail..n].fill(0);
            return Ok(n);
        }
        let bs = self.fs.bs;
        let mut done = 0;
        while done < n {
            let pos = off + done as u64;
            let lblk = pos / bs;
            let in_blk = pos % bs;
            let (phys, run) = self.bmap(lblk)?;
            // Read as much of the contiguous run as the request covers.
            let max = (run.min(1 << 20) * bs - in_blk) as usize;
            let m = max.min(n - done);
            let dst = &mut buf[done..done + m];
            match phys {
                None => dst.fill(0),
                Some(p) => {
                    let src = p * bs + in_blk;
                    if src % 512 == 0 && m % 512 == 0 {
                        self.fs.dev.read_direct(src, dst)?;
                    } else {
                        self.fs.read(src, dst)?;
                    }
                }
            }
            done += m;
        }
        Ok(n)
    }

    fn read_all(&self) -> KResult<Vec<u8>> {
        let mut v = vec![0u8; self.size as usize];
        let n = self.read_at(0, &mut v)?;
        v.truncate(n);
        Ok(v)
    }

    fn entries(&self) -> KResult<Vec<(Vec<u8>, u64, u8)>> {
        let mut out = Vec::new();
        let mut parse = |blk: &[u8]| -> KResult<()> {
            let mut p = 0;
            while p + 8 <= blk.len() {
                let ino = le32(blk, p) as u64;
                let rec = le16(blk, p + 4) as usize;
                let (nlen, ft) = if self.fs.filetype { (blk[p + 6] as usize, blk[p + 7]) } else { (le16(blk, p + 6) as usize, 0) };
                if rec < 8 || p + rec > blk.len() || 8 + nlen > rec {
                    return Err(EUCLEAN);
                }
                if ino != 0 && nlen > 0 {
                    let dt = match ft {
                        1 => DT_REG,
                        2 => DT_DIR,
                        3 => DT_CHR,
                        4 => DT_BLK,
                        5 => DT_FIFO,
                        6 => DT_SOCK,
                        7 => DT_LNK,
                        _ => DT_UNKNOWN,
                    };
                    out.push((blk[p + 8..p + 8 + nlen].to_vec(), ino, dt));
                }
                p += rec;
            }
            Ok(())
        };
        if self.flags & FL_INLINE_DATA != 0 {
            // 4-byte parent inode, then entries in i_block and system.data.
            let data = self.inline_data_raw()?;
            // (No "." / ".." on disk; the VFS adds them.)
            parse(&data[4..60.min(data.len())])?;
            if data.len() > 60 {
                parse(&data[60..])?;
            }
            return Ok(out);
        }
        let data = self.read_all()?;
        for blk in data.chunks(self.fs.bs as usize) {
            parse(blk)?;
        }
        Ok(out)
    }

    /// Inline directory data without truncating to i_size.
    fn inline_data_raw(&self) -> KResult<Vec<u8>> {
        let mut v = self.iblock.to_vec();
        if let Some((_, extra)) = self.xattrs()?.into_iter().find(|(n, _)| n == b"system.data") {
            v.extend_from_slice(&extra);
        }
        Ok(v)
    }

    fn xattrs(&self) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        if !self.ibody_xattr.is_empty() {
            let area = &self.ibody_xattr;
            self.parse_xattrs(area, 0, area, &mut out)?;
        }
        if self.file_acl != 0 {
            let blk = self.fs.read_block(self.file_acl)?;
            if le32(&blk, 0) == XATTR_MAGIC {
                self.parse_xattrs(&blk, 32, &blk, &mut out)?;
            }
        }
        Ok(out)
    }

    /// Entries start at `entries[start..]`; values are at offsets into
    /// `values` (the area after the magic, or the whole block).
    fn parse_xattrs(&self, entries: &[u8], start: usize, values: &[u8], out: &mut Vec<(Vec<u8>, Vec<u8>)>) -> KResult<()> {
        let mut p = start;
        while p + 16 <= entries.len() && le32(entries, p) != 0 {
            let nlen = entries[p] as usize;
            let idx = entries[p + 1];
            let voff = le16(entries, p + 2) as usize;
            let vinum = le32(entries, p + 4) as u64;
            let vsize = le32(entries, p + 8) as usize;
            if p + 16 + nlen > entries.len() {
                break;
            }
            let name = &entries[p + 16..p + 16 + nlen];
            let value = if vinum != 0 {
                let mut v = self.fs.inode(vinum)?.data.read_all()?;
                v.truncate(vsize);
                v
            } else {
                values.get(voff..voff + vsize).ok_or(EUCLEAN)?.to_vec()
            };
            let prefix: &[u8] = match idx {
                8 => b"system.richacl",
                i => super::erofs::xattr_prefix(i).unwrap_or(b"?"),
            };
            let mut full = prefix.to_vec();
            full.extend_from_slice(name);
            out.push((full, value));
            p += (16 + nlen).next_multiple_of(4);
        }
        Ok(())
    }
}

impl PageSource for IData {
    fn read_page(&self, idx: u64, buf: &mut [u8]) -> KResult<()> {
        self.read_at(idx * buf.len() as u64, buf).map(|_| ())
    }
}

pub struct Ext4Inode {
    meta: Meta,
    data: Arc<IData>,
    cache: Once<Arc<Shmem>>,
}

impl Ext4Inode {
    fn page_cache(&self) -> Arc<Shmem> {
        self.cache.call_once(|| Shmem::from_source(self.data.size, self.data.clone())).clone()
    }
}

impl Inode for Ext4Inode {
    fn meta(&self) -> &Meta {
        &self.meta
    }

    fn size(&self) -> u64 {
        self.data.size
    }

    fn lookup(&self, name: &[u8]) -> KResult<Arc<dyn Inode>> {
        if !self.meta.is_dir() {
            return Err(ENOTDIR);
        }
        for (n, ino, _) in self.data.entries()? {
            if n == name && n != b"." && n != b".." {
                return Ok(self.data.fs.inode(ino)?);
            }
        }
        Err(ENOENT)
    }

    fn create(&self, _: &[u8], _: u32, _: u64) -> KResult<Arc<dyn Inode>> {
        Err(self.not_dir_or(EROFS))
    }
    fn symlink(&self, _: &[u8], _: &[u8]) -> KResult<Arc<dyn Inode>> {
        Err(self.not_dir_or(EROFS))
    }
    fn link(&self, _: &[u8], _: &Arc<dyn Inode>) -> KResult<()> {
        Err(self.not_dir_or(EROFS))
    }
    fn unlink(&self, _: &[u8], _: bool) -> KResult<()> {
        Err(self.not_dir_or(EROFS))
    }
    fn rename(&self, _: &[u8], _: &Arc<dyn Inode>, _: &[u8], _: u32) -> KResult<()> {
        Err(self.not_dir_or(EROFS))
    }

    fn entries(&self) -> KResult<Vec<DirEntry>> {
        if !self.meta.is_dir() {
            return Err(ENOTDIR);
        }
        Ok(self
            .data
            .entries()?
            .into_iter()
            .filter(|(n, _, _)| n != b"." && n != b"..")
            .map(|(name, ino, dtype)| DirEntry { name, ino, dtype })
            .collect())
    }

    fn readlink(&self) -> KResult<Vec<u8>> {
        if self.meta.file_type() != S_IFLNK {
            return Err(EINVAL);
        }
        self.data.read_all()
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        match self.meta.file_type() {
            S_IFDIR => Err(EISDIR),
            S_IFREG => self.page_cache().try_read(off, buf),
            _ => Err(EINVAL),
        }
    }

    fn write_at(&self, _: u64, _: &[u8]) -> KResult<usize> {
        Err(EROFS)
    }

    fn truncate(&self, _: u64) -> KResult<()> {
        Err(EROFS)
    }

    fn shmem(&self) -> Option<Arc<Shmem>> {
        (self.meta.file_type() == S_IFREG).then(|| self.page_cache())
    }

    fn xattrs(&self) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self.data.xattrs()?.into_iter().filter(|(n, _)| n != b"system.data").collect())
    }

    fn statfs(&self) -> Option<StatFs> {
        let fs = &self.data.fs;
        Some(StatFs {
            magic: EXT4_SUPER_MAGIC as u64,
            bsize: fs.bs,
            blocks: fs.blocks,
            bfree: fs.free_blocks,
            bavail: fs.free_blocks,
            files: fs.inodes_count,
            ffree: fs.free_inodes,
            namelen: NAME_MAX as u64,
            flags: ST_RDONLY,
        })
    }

    fn read_only_fs(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Mount the ext2/3/4 file system on `dev` (read-only); returns the root.
pub fn mount(dev: Arc<BlockDev>) -> KResult<Arc<dyn Inode>> {
    if dev.size() < 2048 {
        return Err(EINVAL);
    }
    let mut sb = [0u8; 1024];
    dev.read(1024, &mut sb)?;
    if le16(&sb, 56) != EXT4_SUPER_MAGIC {
        return Err(EINVAL);
    }
    let log_bs = le32(&sb, 24);
    let incompat = le32(&sb, 96);
    let rev = le32(&sb, 76);
    if log_bs > 2 || incompat & !SUPPORTED_INCOMPAT != 0 {
        log::warn!("ext4: {}: unsupported (block size {}, incompat {:#x})", dev.name, 1024 << log_bs, incompat);
        return Err(EINVAL);
    }
    if incompat & INCOMPAT_META_BG != 0 {
        return Err(EINVAL);
    }
    if incompat & INCOMPAT_RECOVER != 0 {
        log::warn!("ext4: {}: journal needs recovery; mounting without replay", dev.name);
    }
    let bs = 1024u64 << log_bs;
    let is64 = incompat & INCOMPAT_64BIT != 0;
    let desc_size = if is64 { (le16(&sb, 254) as u64).max(32) } else { 32 };
    let blocks = le32(&sb, 4) as u64 | if is64 { (le32(&sb, 336) as u64) << 32 } else { 0 };
    let free_blocks = le32(&sb, 12) as u64 | if is64 { (le32(&sb, 344) as u64) << 32 } else { 0 };
    let blocks_per_group = le32(&sb, 32) as u64;
    let first_data_block = le32(&sb, 20) as u64;
    let inodes_per_group = le32(&sb, 40) as u64;
    if blocks_per_group == 0 || inodes_per_group == 0 || desc_size > 64 {
        return Err(EINVAL);
    }
    let fs = Arc::new(Fs {
        devno: dev.rdev(),
        _holder: dev.hold(),
        dev,
        bs,
        inode_size: if rev == 0 { 128 } else { le16(&sb, 88) as u64 },
        inodes_per_group,
        first_data_block,
        desc_size,
        group_count: (blocks - first_data_block).div_ceil(blocks_per_group),
        filetype: incompat & INCOMPAT_FILETYPE != 0,
        huge_file: le32(&sb, 100) & RO_COMPAT_HUGE_FILE != 0,
        blocks,
        free_blocks,
        inodes_count: le32(&sb, 0) as u64,
        free_inodes: le32(&sb, 16) as u64,
        inodes: Mutex::new(BTreeMap::new()),
    });
    let root = fs.inode(2)?;
    if !root.meta.is_dir() {
        return Err(EUCLEAN);
    }
    log::info!("ext4: mounted {} ({} KiB blocks, {} blocks)", fs.dev.name, bs / 1024, blocks);
    Ok(root)
}
