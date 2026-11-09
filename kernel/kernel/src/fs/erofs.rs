//! erofs (read-only), the format of AOSP's system/vendor/product images.
//!
//! Supported: compact and extended inodes, FLAT_PLAIN and FLAT_INLINE
//! (tail-packed) data, LZ4 compressed files with full (legacy) or compact
//! (4B / 2B) lcluster indexes and 0-padding (one block per pcluster; plain
//! and interlaced uncompressed pclusters), directories, symlinks, device
//! nodes, inline and shared xattrs. Not supported (mount fails or the
//! file reads EOPNOTSUPP): big pclusters, ztailpacking, fragments, chunked
//! files, extra devices, other compressors, long xattr name prefixes.
//!
//! File data goes through a per-inode page cache (`Shmem::from_source`),
//! which `mmap` maps; metadata is read through the block device's cache.
//! The format follows the Linux headers (fs/erofs/erofs_fs.h) and zmap.c.

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

pub const EROFS_SUPER_MAGIC: u32 = 0xe0f5_e1e2;
const SB_OFFSET: u64 = 1024;
const INCOMPAT_ZERO_PADDING: u32 = 1;
const SUPPORTED_INCOMPAT: u32 = INCOMPAT_ZERO_PADDING;

const LAYOUT_FLAT_PLAIN: u16 = 0;
const LAYOUT_COMPRESSED_FULL: u16 = 1;
const LAYOUT_FLAT_INLINE: u16 = 2;
const LAYOUT_COMPRESSED_COMPACT: u16 = 3;

const LCLUSTER_PLAIN: u8 = 0;
const LCLUSTER_HEAD1: u8 = 1;
const LCLUSTER_NONHEAD: u8 = 2;
const LCLUSTER_HEAD2: u8 = 3;
const LI_D0_CBLKCNT: u32 = 1 << 11;

const ADVISE_COMPACTED_2B: u16 = 0x1;
const ADVISE_BIG_PCLUSTER_1: u16 = 0x2;
const ADVISE_BIG_PCLUSTER_2: u16 = 0x4;
const ADVISE_INLINE_PCLUSTER: u16 = 0x8;
const ADVISE_INTERLACED_PCLUSTER: u16 = 0x10;
const ADVISE_FRAGMENT_PCLUSTER: u16 = 0x20;

pub struct Fs {
    dev: Arc<BlockDev>,
    _holder: Holder,
    /// st_dev of every inode: the block device's number.
    devno: u64,
    blkbits: u32,
    meta_blkaddr: u64,
    xattr_blkaddr: u64,
    root_nid: u64,
    build_time: i64,
    blocks: u64,
    inos: u64,
    /// LZ4_0PADDING: compressed data ends at the end of its block.
    zero_padding: bool,
    inodes: Mutex<BTreeMap<u64, Weak<ErofsInode>>>,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Fs {
    fn blksz(&self) -> u64 {
        1 << self.blkbits
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        self.dev.read(off, buf)
    }

    fn read_vec(&self, off: u64, len: usize) -> KResult<Vec<u8>> {
        let mut v = vec![0u8; len];
        self.read(off, &mut v)?;
        Ok(v)
    }

    fn iloc(&self, nid: u64) -> u64 {
        self.meta_blkaddr * self.blksz() + nid * 32
    }

    fn inode(self: &Arc<Self>, nid: u64) -> KResult<Arc<ErofsInode>> {
        if let Some(i) = self.inodes.lock().get(&nid).and_then(Weak::upgrade) {
            return Ok(i);
        }
        let data = Arc::new(self.load(nid)?);
        let a = data.attr;
        let meta = Meta::with_ino(nid, self.devno, a.mode, data.rdev);
        meta.update(|m| *m = a);
        let inode = Arc::new(ErofsInode { meta, data, cache: Once::new() });
        let mut map = self.inodes.lock();
        if let Some(i) = map.get(&nid).and_then(Weak::upgrade) {
            return Ok(i);
        }
        if map.len() >= 256 && map.len().is_power_of_two() {
            map.retain(|_, w| w.strong_count() > 0);
        }
        map.insert(nid, Arc::downgrade(&inode));
        Ok(inode)
    }

    fn load(self: &Arc<Self>, nid: u64) -> KResult<IData> {
        let pos = self.iloc(nid);
        let b = self.read_vec(pos, 64)?;
        let format = le16(&b, 0);
        let extended = format & 1 != 0;
        let layout = (format >> 1) & 7;
        let xattr_icount = le16(&b, 2);
        let mode = le16(&b, 4) as u32;
        let (isize, size, iu, uid, gid, mtime, nlink) = if extended {
            let t = le64(&b, 32) as i64 * 1_000_000_000 + le32(&b, 40) as i64;
            (64u64, le64(&b, 8), le32(&b, 16), le32(&b, 24), le32(&b, 28), t, le32(&b, 44))
        } else {
            (32, le32(&b, 8) as u64, le32(&b, 16), le16(&b, 24) as u32, le16(&b, 26) as u32, self.build_time, le16(&b, 6) as u32)
        };
        let xattr_isize = if xattr_icount == 0 { 0 } else { 12 + 4 * (xattr_icount as u64 - 1) };
        let ty = mode & S_IFMT;
        let rdev = match ty {
            S_IFCHR | S_IFBLK => {
                // Linux's new_decode_dev of the 32-bit i_u.
                let (major, minor) = ((iu & 0xfff00) >> 8, (iu & 0xff) | ((iu >> 12) & 0xfff00));
                encode_dev(major, minor)
            }
            _ => 0,
        };
        let mut d = IData {
            fs: self.clone(),
            nid,
            pos,
            isize,
            xattr_isize,
            layout,
            size,
            raw_blkaddr: iu as u64,
            rdev,
            attr: Attr { mode, uid, gid, nlink, atime: mtime, mtime, ctime: mtime },
            z: None,
        };
        if matches!(layout, LAYOUT_COMPRESSED_FULL | LAYOUT_COMPRESSED_COMPACT) && ty == S_IFREG {
            let h = self.read_vec((pos + isize + xattr_isize).next_multiple_of(8), 8)?;
            let advise = le16(&h, 4);
            let algs = h[6];
            let lbits = self.blkbits + (h[7] & 7) as u32;
            let bad = ADVISE_BIG_PCLUSTER_1 | ADVISE_BIG_PCLUSTER_2 | ADVISE_INLINE_PCLUSTER | ADVISE_FRAGMENT_PCLUSTER;
            let ok = advise & bad == 0 && algs & 0x0f == 0 && (algs >> 4 == 0) && lbits <= 14
                && (layout == LAYOUT_COMPRESSED_FULL || lbits == self.blkbits);
            d.z = Some(ZInfo { advise, lbits, ok });
        } else if !matches!(layout, LAYOUT_FLAT_PLAIN | LAYOUT_FLAT_INLINE) && ty != S_IFCHR && ty != S_IFBLK
            && ty != S_IFIFO && ty != S_IFSOCK
        {
            log::warn!("erofs: inode {nid}: unsupported data layout {layout}");
        }
        Ok(d)
    }
}

struct ZInfo {
    advise: u16,
    lbits: u32,
    /// Features we can decode.
    ok: bool,
}

/// One lcluster index, decoded (Linux's z_erofs_maprecorder).
#[derive(Default, Clone, Copy)]
struct Lcluster {
    ty: u8,
    clusterofs: u32,
    pblk: u64,
    delta0: u32,
}

/// An extent of a compressed file: logical start/length, the physical
/// block and its type.
struct Extent {
    la: u64,
    llen: u64,
    pblk: u64,
    ty: u8,
}

/// Inode data needed to read the file (shared with its page cache).
pub struct IData {
    fs: Arc<Fs>,
    nid: u64,
    pos: u64,
    isize: u64,
    xattr_isize: u64,
    layout: u16,
    size: u64,
    raw_blkaddr: u64,
    rdev: u64,
    attr: Attr,
    z: Option<ZInfo>,
}

/// A decompressed pcluster: (file system, physical block, logical
/// start of its extent) -> output from that start.
type ZKey = (usize, u64, u64);

/// Recently decompressed pclusters of all erofs inodes (sequential reads
/// hit the same pcluster many times). Global and small: a cache per
/// inode filled the kernel heap once zygote touched hundreds of files.
static ZCACHE: Mutex<Vec<(ZKey, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());
const ZCACHE_ENTRIES: usize = 32;

fn zcache_get(key: ZKey, need: usize) -> Option<Arc<Vec<u8>>> {
    let mut c = ZCACHE.lock();
    let i = c.iter().position(|(k, out)| *k == key && out.len() >= need)?;
    let e = c.remove(i);
    let out = e.1.clone();
    c.push(e);
    Some(out)
}

impl Drop for Fs {
    fn drop(&mut self) {
        // A later file system may get the same address.
        let me = self as *const Fs as usize;
        ZCACHE.lock().retain(|((fs, _, _), _)| *fs != me);
    }
}

fn zcache_put(key: ZKey, out: Arc<Vec<u8>>) {
    let mut c = ZCACHE.lock();
    c.retain(|(k, _)| *k != key);
    if c.len() >= ZCACHE_ENTRIES {
        c.remove(0);
    }
    c.push((key, out));
}

impl IData {
    fn inline_off(&self) -> u64 {
        self.pos + self.isize + self.xattr_isize
    }

    /// Read uncompressed data (any layout but compressed) at `off`.
    fn read_flat(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if off >= self.size {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.size - off) as usize;
        let bs = self.fs.blksz();
        let nblocks = self.size.div_ceil(bs);
        let lastblk = if self.layout == LAYOUT_FLAT_INLINE { nblocks - 1 } else { nblocks };
        let mut done = 0;
        while done < n {
            let pos = off + done as u64;
            let blk = pos / bs;
            let in_blk = pos % bs;
            let m = ((bs - in_blk) as usize).min(n - done);
            let src = if blk < lastblk {
                (self.raw_blkaddr + blk) * bs + in_blk
            } else {
                self.inline_off() + (pos - lastblk * bs)
            };
            // Whole aligned blocks bypass the device cache.
            if blk < lastblk && in_blk == 0 && m as u64 == bs {
                self.fs.dev.read_direct(src, &mut buf[done..done + m])?;
            } else {
                self.fs.read(src, &mut buf[done..done + m])?;
            }
            done += m;
        }
        Ok(n)
    }

    fn zinfo(&self) -> KResult<&ZInfo> {
        match &self.z {
            Some(z) if z.ok => Ok(z),
            _ => Err(EOPNOTSUPP),
        }
    }

    fn total_lclusters(&self, z: &ZInfo) -> u64 {
        self.size.div_ceil(1 << z.lbits)
    }

    fn index_base(&self) -> u64 {
        self.inline_off().next_multiple_of(8) + 8
    }

    fn load_lcluster(&self, z: &ZInfo, lcn: u64) -> KResult<Lcluster> {
        if lcn >= self.total_lclusters(z) {
            return Err(EUCLEAN);
        }
        if self.layout == LAYOUT_COMPRESSED_FULL {
            let pos = self.index_base() + 8 + lcn * 8;
            let mut b = [0u8; 8];
            self.fs.read(pos, &mut b)?;
            let ty = (le16(&b, 0) & 3) as u8;
            if ty == LCLUSTER_NONHEAD {
                let d0 = le16(&b, 4) as u32;
                if d0 & LI_D0_CBLKCNT != 0 {
                    return Err(EUCLEAN);
                }
                return Ok(Lcluster { ty, clusterofs: 1 << z.lbits, pblk: 0, delta0: d0 });
            }
            let clusterofs = le16(&b, 2) as u32;
            if clusterofs >= 1 << z.lbits {
                return Err(EUCLEAN);
            }
            return Ok(Lcluster { ty, clusterofs, pblk: le32(&b, 4) as u64, delta0: 0 });
        }
        self.load_compact(z, lcn)
    }

    /// Linux's z_erofs_load_compact_lcluster + unpack_compacted_index.
    fn load_compact(&self, z: &ZInfo, lcn: u64) -> KResult<Lcluster> {
        let ebase = self.index_base();
        let totalidx = self.total_lclusters(z);
        let mut initial4b = (32 - ebase % 32) / 4;
        if initial4b == 8 {
            initial4b = 0;
        }
        let compacted_2b = if z.advise & ADVISE_COMPACTED_2B != 0 && initial4b < totalidx {
            (totalidx - initial4b) / 16 * 16
        } else {
            0
        };
        let mut pos = ebase;
        let mut lcn = lcn;
        let shift;
        if lcn < initial4b {
            shift = 2;
        } else {
            pos += initial4b * 4;
            lcn -= initial4b;
            if lcn < compacted_2b {
                shift = 1;
            } else {
                pos += compacted_2b * 2;
                lcn -= compacted_2b;
                shift = 2;
            }
        }
        pos += lcn << shift;
        let vcnt: u64 = match shift {
            2 if z.lbits <= 14 => 2,
            1 if z.lbits <= 12 => 16,
            _ => return Err(EOPNOTSUPP),
        };
        let pack = vcnt << shift;
        let lobits = z.lbits.max(12);
        let encodebits = ((pack - 4) * 8 / vcnt) as u32;
        let bytes = pos & (pack - 1);
        let mut inb = vec![0u8; pack as usize + 4];
        self.fs.read(pos - bytes, &mut inb[..pack as usize])?;
        let decode = |i: i64| -> (u32, u8) {
            let bit = encodebits as usize * i as usize;
            let v = u32::from_le_bytes(inb[bit / 8..bit / 8 + 4].try_into().unwrap()) >> (bit & 7);
            (v & ((1 << lobits) - 1), ((v >> lobits) & 3) as u8)
        };
        let mut i = (bytes >> shift) as i64;
        let (lo, ty) = decode(i);
        if ty == LCLUSTER_NONHEAD {
            let clusterofs = 1 << z.lbits;
            if lo & LI_D0_CBLKCNT != 0 {
                return Err(EUCLEAN);
            }
            if i + 1 != vcnt as i64 {
                return Ok(Lcluster { ty, clusterofs, pblk: 0, delta0: lo });
            }
            // The last lcluster of a pack stores delta[1]; derive delta[0]
            // from the previous one.
            let (plo, pty) = decode(i - 1);
            let plo = if pty != LCLUSTER_NONHEAD {
                0
            } else if plo & LI_D0_CBLKCNT != 0 {
                1
            } else {
                plo
            };
            return Ok(Lcluster { ty, clusterofs, pblk: 0, delta0: plo + 1 });
        }
        // HEAD/PLAIN: count the heads before it in this pack.
        let mut nblk: u64 = 1;
        while i > 0 {
            i -= 1;
            let (lo, ty) = decode(i);
            if ty == LCLUSTER_NONHEAD {
                i -= lo as i64;
            }
            if i >= 0 {
                nblk += 1;
            }
        }
        let base = le32(&inb, pack as usize - 4) as u64;
        Ok(Lcluster { ty, clusterofs: lo, pblk: base + nblk, delta0: 0 })
    }

    /// Linux's z_erofs_do_map_blocks: the extent containing `ofs`,
    /// mapped up to the end of `ofs`'s lcluster at most.
    fn map(&self, z: &ZInfo, ofs: u64) -> KResult<Extent> {
        let lb = z.lbits;
        let initial = ofs >> lb;
        let endoff = (ofs & ((1 << lb) - 1)) as u32;
        let mut m = self.load_lcluster(z, initial)?;
        let mut lcn = initial;
        let mut end = (initial + 1) << lb;
        let mut lookback = match m.ty {
            LCLUSTER_PLAIN | LCLUSTER_HEAD1 | LCLUSTER_HEAD2 => {
                if endoff >= m.clusterofs {
                    let la = (lcn << lb) | m.clusterofs as u64;
                    return Ok(Extent { la, llen: end - la, pblk: m.pblk, ty: m.ty });
                }
                if lcn == 0 {
                    return Err(EUCLEAN);
                }
                end = (lcn << lb) | m.clusterofs as u64;
                1
            }
            _ => m.delta0,
        };
        // z_erofs_extent_lookback
        loop {
            if lookback == 0 || lcn < lookback as u64 {
                return Err(EUCLEAN);
            }
            lcn -= lookback as u64;
            m = self.load_lcluster(z, lcn)?;
            match m.ty {
                LCLUSTER_NONHEAD => lookback = m.delta0,
                _ => {
                    let la = (lcn << lb) | m.clusterofs as u64;
                    return Ok(Extent { la, llen: end - la, pblk: m.pblk, ty: m.ty });
                }
            }
        }
    }

    /// Fill `buf` with the decompressed bytes [ofs, ofs + buf.len()).
    fn read_compressed(&self, ofs: u64, buf: &mut [u8]) -> KResult<()> {
        let z = self.zinfo()?;
        let bs = self.fs.blksz();
        let mut done = 0usize;
        while done < buf.len() {
            let pos = ofs + done as u64;
            let e = self.map(z, pos)?;
            let e_end = e.la + e.llen;
            if e_end <= pos || e.la > pos {
                return Err(EUCLEAN);
            }
            let n = ((e_end - pos) as usize).min(buf.len() - done);
            let need = (pos - e.la) as usize + n;
            let dst = &mut buf[done..done + n];
            match e.ty {
                LCLUSTER_PLAIN => {
                    if e.llen > bs {
                        return Err(EUCLEAN);
                    }
                    let mut blk = vec![0u8; bs as usize];
                    self.fs.read(e.pblk * bs, &mut blk)?;
                    let shift = if z.advise & ADVISE_INTERLACED_PCLUSTER != 0 { e.la % bs } else { 0 };
                    for (k, b) in dst.iter_mut().enumerate() {
                        *b = blk[((shift + (pos - e.la) + k as u64) % bs) as usize];
                    }
                }
                _ => {
                    let key = (Arc::as_ptr(&self.fs) as usize, e.pblk, e.la);
                    let out = match zcache_get(key, need) {
                        Some(out) => out,
                        None => {
                            let mut src = vec![0u8; bs as usize];
                            self.fs.read(e.pblk * bs, &mut src)?;
                            // The first token has literals, so it is never 0.
                            let start = src.iter().position(|&b| b != 0).ok_or(EUCLEAN)?;
                            // Without 0-padding the stream end is unknown: stop at
                            // exactly what is needed.
                            let want = if self.fs.zero_padding { need.next_multiple_of(64 * 1024) } else { need };
                            let mut out = vec![0u8; want];
                            let got = super::lz4::decompress(&src[start..], &mut out).ok_or(EUCLEAN)?;
                            if got < need {
                                log::warn!("erofs: nid {}: pcluster {} gave {got} of {need} bytes", self.nid, e.pblk);
                                return Err(EUCLEAN);
                            }
                            out.truncate(got);
                            let out = Arc::new(out);
                            zcache_put(key, out.clone());
                            out
                        }
                    };
                    let from = (pos - e.la) as usize;
                    dst.copy_from_slice(&out[from..from + n]);
                }
            }
            done += n;
        }
        Ok(())
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if self.z.is_some() {
            if off >= self.size {
                return Ok(0);
            }
            let n = (buf.len() as u64).min(self.size - off) as usize;
            self.read_compressed(off, &mut buf[..n])?;
            return Ok(n);
        }
        self.read_flat(off, buf)
    }

    fn read_all(&self) -> KResult<Vec<u8>> {
        let mut v = vec![0u8; self.size as usize];
        let n = self.read_at(0, &mut v)?;
        v.truncate(n);
        Ok(v)
    }

    fn entries(&self) -> KResult<Vec<(Vec<u8>, u64, u8)>> {
        let data = self.read_all()?;
        let bs = self.fs.blksz() as usize;
        let mut out = Vec::new();
        for chunk in data.chunks(bs) {
            if chunk.len() < 12 {
                break;
            }
            let count = le16(chunk, 8) as usize / 12;
            if count == 0 || count * 12 > chunk.len() {
                return Err(EUCLEAN);
            }
            for i in 0..count {
                let d = &chunk[i * 12..i * 12 + 12];
                let nid = le64(d, 0);
                let start = le16(d, 8) as usize;
                let end = if i + 1 < count { le16(chunk, (i + 1) * 12 + 8) as usize } else { chunk.len() };
                if start > end || end > chunk.len() {
                    return Err(EUCLEAN);
                }
                let mut name = &chunk[start..end];
                if i + 1 == count {
                    name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
                }
                let dt = match d[10] {
                    1 => DT_REG,
                    2 => DT_DIR,
                    3 => DT_CHR,
                    4 => DT_BLK,
                    5 => DT_FIFO,
                    6 => DT_SOCK,
                    7 => DT_LNK,
                    _ => DT_UNKNOWN,
                };
                out.push((name.to_vec(), nid, dt));
            }
        }
        Ok(out)
    }

    fn xattrs(&self) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        if self.xattr_isize == 0 {
            return Ok(out);
        }
        let ibody = self.fs.read_vec(self.pos + self.isize, self.xattr_isize as usize)?;
        let shared = ibody[4] as usize;
        let mut p = 12 + 4 * shared;
        for s in 0..shared {
            if 12 + 4 * s + 4 > ibody.len() {
                return Err(EUCLEAN);
            }
            let id = le32(&ibody, 12 + 4 * s) as u64;
            let at = self.fs.xattr_blkaddr * self.fs.blksz() + id * 4;
            let hdr = self.fs.read_vec(at, 4)?;
            let body = self.fs.read_vec(at + 4, hdr[0] as usize + le16(&hdr, 2) as usize)?;
            push_xattr(&mut out, hdr[1], &body[..hdr[0] as usize], &body[hdr[0] as usize..]);
        }
        while p + 4 <= ibody.len() {
            let (nlen, idx, vlen) = (ibody[p] as usize, ibody[p + 1], le16(&ibody, p + 2) as usize);
            let end = p + 4 + nlen + vlen;
            if end > ibody.len() {
                break;
            }
            push_xattr(&mut out, idx, &ibody[p + 4..p + 4 + nlen], &ibody[p + 4 + nlen..end]);
            p = end.next_multiple_of(4);
        }
        Ok(out)
    }
}

/// Name prefix of an xattr name index (Linux's erofs_xattr_handler).
pub fn xattr_prefix(idx: u8) -> Option<&'static [u8]> {
    Some(match idx {
        0 => b"",
        1 => b"user.",
        2 => b"system.posix_acl_access",
        3 => b"system.posix_acl_default",
        4 => b"trusted.",
        6 => b"security.",
        7 => b"system.",
        _ => return None,
    })
}

fn push_xattr(out: &mut Vec<(Vec<u8>, Vec<u8>)>, idx: u8, name: &[u8], value: &[u8]) {
    if let Some(p) = xattr_prefix(idx) {
        let mut n = p.to_vec();
        n.extend_from_slice(name);
        out.push((n, value.to_vec()));
    }
}

impl PageSource for IData {
    fn read_page(&self, idx: u64, buf: &mut [u8]) -> KResult<()> {
        let off = idx * buf.len() as u64;
        self.read_at(off, buf).map(|_| ())
    }
}

pub struct ErofsInode {
    meta: Meta,
    data: Arc<IData>,
    cache: Once<Arc<Shmem>>,
}

impl ErofsInode {
    fn page_cache(&self) -> Arc<Shmem> {
        self.cache.call_once(|| Shmem::from_source(self.data.size, self.data.clone())).clone()
    }
}

impl Inode for ErofsInode {
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
        for (n, nid, _) in self.data.entries()? {
            if n == name && n != b"." && n != b".." {
                return Ok(self.data.fs.inode(nid)?);
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
        self.data.xattrs()
    }

    fn statfs(&self) -> Option<StatFs> {
        let fs = &self.data.fs;
        Some(StatFs {
            magic: EROFS_SUPER_MAGIC as u64,
            bsize: fs.blksz(),
            blocks: fs.blocks,
            bfree: 0,
            bavail: 0,
            files: fs.inos,
            ffree: 0,
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

/// Mount the erofs image on `dev`; returns the root inode.
pub fn mount(dev: Arc<BlockDev>) -> KResult<Arc<dyn Inode>> {
    let mut sb = [0u8; 128];
    if dev.size() < SB_OFFSET + 128 {
        return Err(EINVAL);
    }
    dev.read(SB_OFFSET, &mut sb)?;
    if le32(&sb, 0) != EROFS_SUPER_MAGIC {
        return Err(EINVAL);
    }
    let blkbits = sb[12] as u32;
    let incompat = le32(&sb, 80);
    if !(9..=12).contains(&blkbits) || incompat & !SUPPORTED_INCOMPAT != 0 || le16(&sb, 86) != 0 {
        log::warn!("erofs: {}: unsupported image (blkbits {blkbits}, incompat {incompat:#x})", dev.name);
        return Err(EINVAL);
    }
    let fs = Arc::new(Fs {
        devno: dev.rdev(),
        _holder: dev.hold(),
        dev,
        blkbits,
        meta_blkaddr: le32(&sb, 40) as u64,
        xattr_blkaddr: le32(&sb, 44) as u64,
        root_nid: le16(&sb, 14) as u64,
        build_time: le64(&sb, 24) as i64 * 1_000_000_000 + le32(&sb, 32) as i64,
        blocks: le32(&sb, 36) as u64,
        inos: le64(&sb, 16),
        zero_padding: incompat & INCOMPAT_ZERO_PADDING != 0,
        inodes: Mutex::new(BTreeMap::new()),
    });
    let root = fs.inode(fs.root_nid)?;
    if !root.meta.is_dir() {
        return Err(EINVAL);
    }
    log::info!("erofs: mounted {} ({} blocks, {} inodes)", fs.dev.name, fs.blocks, fs.inos);
    Ok(root)
}
