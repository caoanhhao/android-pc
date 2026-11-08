//! Shmem objects: the page cache of RAM-backed files.
//!
//! A [`Shmem`] is a sparse array of reference-counted frames plus a size
//! and memfd seals. It backs every tmpfs regular file (and memfd, which is
//! an unnamed tmpfs file) and every MAP_SHARED anonymous mapping. The
//! object holds one reference to each of its frames; every PTE mapping a
//! frame holds another (see `frame`), so a page stays alive while it is
//! mapped even after a truncate drops it from the cache.
//!
//! - `read`/`write` copy through the direct map; MAP_SHARED mappings map
//!   the cached frames themselves, so both views always agree.
//! - MAP_PRIVATE file mappings map cached frames read-only and copy them on
//!   write (the B3a COW rule: the cache's reference keeps the count > 1).
//! - Files unpacked from the initramfs keep their contents in the boot
//!   module (`backing`) until a page is first touched.
//! - The object remembers which address spaces map it (weak references) so
//!   that truncate and hole punching can unmap the dropped pages, and
//!   F_SEAL_WRITE can refuse while writable shared mappings exist.
//!
//! Lock order: address space -> shmem -> frame allocator. The object never
//! calls into an address space while holding its own lock.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::PhysFrame;

use super::paging::phys_to_virt;
use super::user::AddressSpace;
use super::{PAGE_SIZE, frame};
use crate::errno::*;

pub const F_SEAL_SEAL: u32 = 1;
pub const F_SEAL_SHRINK: u32 = 2;
pub const F_SEAL_GROW: u32 = 4;
pub const F_SEAL_WRITE: u32 = 8;
pub const F_SEAL_FUTURE_WRITE: u32 = 16;
pub const F_SEAL_EXEC: u32 = 32;
const ALL_SEALS: u32 = 63;
pub const WRITE_SEALS: u32 = F_SEAL_WRITE | F_SEAL_FUTURE_WRITE;

pub const FALLOC_FL_KEEP_SIZE: u32 = 1;
pub const FALLOC_FL_PUNCH_HOLE: u32 = 2;

/// Largest file size (Linux MAX_LFS_FILESIZE). Files are sparse, so only
/// the pages actually written use memory.
pub const MAX_SIZE: u64 = i64::MAX as u64;
/// `write` and `fallocate` fail with ENOSPC rather than take the last
/// 16 MiB of free frames.
const RESERVE_FRAMES: usize = 4096;

const PAGE: usize = PAGE_SIZE as usize;

fn page_ptr(f: PhysFrame) -> *mut u8 {
    phys_to_virt(f.start_address()).as_mut_ptr()
}

struct Inner {
    /// Page index -> frame (one reference each).
    pages: BTreeMap<u64, PhysFrame>,
    size: u64,
    seals: u32,
    /// Initramfs contents for the pages not yet in `pages` (never longer
    /// than `size`).
    backing: &'static [u8],
}

impl Inner {
    /// The cached frame of page `idx`, created (zeroed or filled from the
    /// initramfs) if missing. `reserve` keeps RESERVE_FRAMES free.
    fn page(&mut self, idx: u64, reserve: bool) -> KResult<PhysFrame> {
        if let Some(&f) = self.pages.get(&idx) {
            return Ok(f);
        }
        if reserve && frame::stats().1 < RESERVE_FRAMES {
            return Err(ENOSPC);
        }
        let f = frame::alloc_zeroed().ok_or(ENOSPC)?;
        let start = (idx * PAGE_SIZE) as usize;
        if start < self.backing.len() {
            let n = (self.backing.len() - start).min(PAGE);
            unsafe { core::ptr::copy_nonoverlapping(self.backing.as_ptr().add(start), page_ptr(f), n) };
        }
        self.pages.insert(idx, f);
        Ok(f)
    }

    /// Copy every initramfs page into frames (before punching holes).
    fn materialize(&mut self) -> KResult<()> {
        for idx in 0..(self.backing.len() as u64).div_ceil(PAGE_SIZE) {
            self.page(idx, false)?;
        }
        self.backing = &[];
        Ok(())
    }

    fn write(&mut self, off: u64, data: &[u8]) -> KResult<usize> {
        if self.seals & WRITE_SEALS != 0 {
            return Err(EPERM);
        }
        let end = off.checked_add(data.len() as u64).filter(|&e| e <= MAX_SIZE).ok_or(EFBIG)?;
        if end > self.size && self.seals & F_SEAL_GROW != 0 {
            return Err(EPERM);
        }
        let mut done = 0;
        while done < data.len() {
            let pos = off + done as u64;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE - in_page).min(data.len() - done);
            match self.page(pos / PAGE_SIZE, true) {
                Ok(f) => unsafe {
                    core::ptr::copy_nonoverlapping(data.as_ptr().add(done), page_ptr(f).add(in_page), n)
                },
                Err(e) if done == 0 => return Err(e),
                Err(_) => break,
            }
            done += n;
        }
        self.size = self.size.max(off + done as u64);
        Ok(done)
    }
}

pub struct Shmem {
    inner: Mutex<Inner>,
    /// Address spaces that have (or had) a VMA on this object.
    mappers: Mutex<Vec<Weak<AddressSpace>>>,
}

impl Shmem {
    /// An empty object of `size` bytes that cannot be sealed (the
    /// default for tmpfs files and shared anonymous memory).
    pub fn new(size: u64) -> Arc<Shmem> {
        Self::with_backing(size, &[])
    }

    /// An object whose contents are `data` (an initramfs file), copied
    /// page by page on first use.
    pub fn from_static(data: &'static [u8]) -> Arc<Shmem> {
        Self::with_backing(data.len() as u64, data)
    }

    fn with_backing(size: u64, backing: &'static [u8]) -> Arc<Shmem> {
        Arc::new(Shmem {
            inner: Mutex::new(Inner { pages: BTreeMap::new(), size, seals: F_SEAL_SEAL, backing }),
            mappers: Mutex::new(Vec::new()),
        })
    }

    pub fn size(&self) -> u64 {
        self.inner.lock().size
    }

    /// Read at `off`; returns 0 at or past the end of the file.
    pub fn read(&self, off: u64, buf: &mut [u8]) -> usize {
        let inner = self.inner.lock();
        if off >= inner.size {
            return 0;
        }
        let len = (buf.len() as u64).min(inner.size - off) as usize;
        let mut done = 0;
        while done < len {
            let pos = off + done as u64;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE - in_page).min(len - done);
            let dst = &mut buf[done..done + n];
            if let Some(&f) = inner.pages.get(&(pos / PAGE_SIZE)) {
                unsafe { core::ptr::copy_nonoverlapping(page_ptr(f).add(in_page), dst.as_mut_ptr(), n) };
            } else {
                // A hole, or initramfs contents not copied yet.
                let b = inner.backing;
                let from_backing = b.len().saturating_sub(pos as usize).min(n);
                dst[..from_backing].copy_from_slice(&b[pos as usize..pos as usize + from_backing]);
                dst[from_backing..].fill(0);
            }
            done += n;
        }
        len
    }

    pub fn write(&self, off: u64, data: &[u8]) -> KResult<usize> {
        self.inner.lock().write(off, data)
    }

    /// Append atomically; returns (bytes written, new size).
    pub fn append(&self, data: &[u8]) -> KResult<(usize, u64)> {
        let mut inner = self.inner.lock();
        let off = inner.size;
        let n = inner.write(off, data)?;
        Ok((n, inner.size))
    }

    /// Set the size. Shrinking drops the pages past the end from the cache
    /// and from every mapping (later accesses raise SIGBUS) and zeroes the
    /// tail of the new last page.
    pub fn truncate(&self, size: u64) -> KResult<()> {
        if size > MAX_SIZE {
            return Err(EFBIG);
        }
        let (dropped, first) = {
            let mut inner = self.inner.lock();
            if size < inner.size && inner.seals & F_SEAL_SHRINK != 0
                || size > inner.size && inner.seals & F_SEAL_GROW != 0
            {
                return Err(EPERM);
            }
            let old = inner.size;
            inner.size = size;
            if size >= old {
                return Ok(());
            }
            let first = size.div_ceil(PAGE_SIZE);
            let dropped = inner.pages.split_off(&first);
            let tail = (size % PAGE_SIZE) as usize;
            if tail != 0
                && let Some(&f) = inner.pages.get(&(size / PAGE_SIZE))
            {
                unsafe { core::ptr::write_bytes(page_ptr(f).add(tail), 0, PAGE - tail) };
            }
            let keep = inner.backing.len().min(size as usize);
            inner.backing = &inner.backing[..keep];
            (dropped, first)
        };
        // Mapped frames keep their PTE's reference until unmapped, so the
        // cache's references can go first.
        for f in dropped.values() {
            frame::free(*f);
        }
        self.zap(first, u64::MAX, None);
        Ok(())
    }

    /// `fallocate(2)`: allocate pages (growing the file unless KEEP_SIZE)
    /// or punch a hole (whole pages are freed, partial ones zeroed).
    pub fn fallocate(&self, mode: u32, off: u64, len: u64) -> KResult<()> {
        if mode & !(FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE) != 0 {
            return Err(EOPNOTSUPP);
        }
        let end = off.checked_add(len).filter(|&e| e <= MAX_SIZE).ok_or(EFBIG)?;
        if mode & FALLOC_FL_PUNCH_HOLE != 0 {
            if mode & FALLOC_FL_KEEP_SIZE == 0 {
                return Err(EOPNOTSUPP);
            }
            return self.punch_hole(off, end);
        }
        let mut inner = self.inner.lock();
        if end > inner.size && mode & FALLOC_FL_KEEP_SIZE == 0 && inner.seals & F_SEAL_GROW != 0 {
            return Err(EPERM);
        }
        for idx in off / PAGE_SIZE..end.div_ceil(PAGE_SIZE) {
            inner.page(idx, true)?;
        }
        if mode & FALLOC_FL_KEEP_SIZE == 0 && end > inner.size {
            inner.size = end;
        }
        Ok(())
    }

    fn punch_hole(&self, off: u64, end: u64) -> KResult<()> {
        let (dropped, first, last) = {
            let mut inner = self.inner.lock();
            if inner.seals & WRITE_SEALS != 0 {
                return Err(EPERM);
            }
            if (inner.backing.len() as u64) > off {
                inner.materialize()?;
            }
            // Zero the partial pages at both ends.
            let zero = |from: u64, to: u64| {
                if let Some(&f) = inner.pages.get(&(from / PAGE_SIZE)) {
                    let s = (from % PAGE_SIZE) as usize;
                    unsafe { core::ptr::write_bytes(page_ptr(f).add(s), 0, (to - from) as usize) };
                }
            };
            let first = off.div_ceil(PAGE_SIZE);
            let last = end / PAGE_SIZE;
            if first > last {
                // Inside one page.
                zero(off, end);
                return Ok(());
            }
            if !off.is_multiple_of(PAGE_SIZE) {
                zero(off, first * PAGE_SIZE);
            }
            if !end.is_multiple_of(PAGE_SIZE) {
                zero(last * PAGE_SIZE, end);
            }
            let mut tail = inner.pages.split_off(&first);
            let mut rest = tail.split_off(&last);
            inner.pages.append(&mut rest);
            (tail, first, last)
        };
        if dropped.is_empty() {
            return Ok(());
        }
        // Private COW copies of the punched pages survive (only the
        // cache's own frames are unmapped).
        let frames: BTreeSet<u64> = dropped.values().map(|f| f.start_address().as_u64()).collect();
        for f in dropped.values() {
            frame::free(*f);
        }
        self.zap(first, last, Some(&frames));
        Ok(())
    }

    pub fn seals(&self) -> u32 {
        self.inner.lock().seals
    }

    /// Initial seals (memfd: 0 with MFD_ALLOW_SEALING, F_SEAL_EXEC with
    /// MFD_NOEXEC_SEAL).
    pub fn set_initial_seals(&self, seals: u32) {
        self.inner.lock().seals = seals;
    }

    /// F_ADD_SEALS.
    pub fn add_seals(&self, seals: u32) -> KResult<()> {
        if seals & !ALL_SEALS != 0 {
            return Err(EINVAL);
        }
        if self.inner.lock().seals & F_SEAL_SEAL != 0 {
            return Err(EPERM);
        }
        if seals & F_SEAL_WRITE != 0 && self.live_mappers().iter().any(|mm| mm.maps_writable(self)) {
            return Err(EBUSY);
        }
        let mut inner = self.inner.lock();
        if inner.seals & F_SEAL_SEAL != 0 {
            return Err(EPERM);
        }
        inner.seals |= seals;
        Ok(())
    }

    /// Whether page `idx` is in the cache (mincore).
    pub fn has_page(&self, idx: u64) -> bool {
        self.inner.lock().pages.contains_key(&idx)
    }

    /// The frame for page `idx` of a mapping, with a new reference for
    /// the caller's PTE. EFAULT past the end of the object (SIGBUS).
    pub fn fault_page(&self, idx: u64) -> KResult<PhysFrame> {
        let mut inner = self.inner.lock();
        if idx >= inner.size.div_ceil(PAGE_SIZE) {
            return Err(EFAULT);
        }
        let f = inner.page(idx, false).map_err(|_| ENOMEM)?;
        frame::get(f);
        Ok(f)
    }

    /// Record that `mm` maps this object.
    pub fn register(&self, mm: &Weak<AddressSpace>) {
        let mut m = self.mappers.lock();
        m.retain(|w| w.strong_count() > 0);
        if !m.iter().any(|w| w.ptr_eq(mm)) {
            m.push(mm.clone());
        }
    }

    fn live_mappers(&self) -> Vec<Arc<AddressSpace>> {
        self.mappers.lock().iter().filter_map(Weak::upgrade).collect()
    }

    /// Unmap pages `[first, last)` from every mapping (only the PTEs
    /// pointing at `only`, if given).
    fn zap(&self, first: u64, last: u64, only: Option<&BTreeSet<u64>>) {
        for mm in self.live_mappers() {
            mm.zap_object(self, first, last, only);
        }
    }
}

impl Drop for Shmem {
    fn drop(&mut self) {
        for f in self.inner.get_mut().pages.values() {
            frame::free(*f);
        }
    }
}

#[test_case]
fn shmem_read_write_truncate_without_leaks() {
    let (_, free_before) = frame::stats();
    {
        static INIT: [u8; 5000] = [7; 5000];
        let s = Shmem::from_static(&INIT);
        let mut b = [0u8; 8];
        assert_eq!(s.read(4996, &mut b), 4);
        assert_eq!(&b[..4], &[7; 4]);
        // Writing one page copies just that page out of the backing.
        s.write(4097, b"xy").unwrap();
        assert_eq!(s.read(4095, &mut b[..4]), 4);
        assert_eq!(&b[..4], &[7, 7, b'x', b'y']);
        assert!(!s.has_page(0) && s.has_page(1));
        // Shrink into the middle of a page: the tail reads back zero.
        s.truncate(4098).unwrap();
        s.truncate(3 * 4096).unwrap();
        assert_eq!(s.read(4096, &mut b[..4]), 4);
        assert_eq!(&b[..4], &[7, b'x', 0, 0]);
        assert_eq!(s.read(0, &mut b[..1]), 1);
        assert_eq!(b[0], 7);
        // Sparse growth uses no memory; seals are refused by default.
        s.truncate(1 << 40).unwrap();
        assert_eq!(s.add_seals(F_SEAL_GROW), Err(EPERM));
        let (n, size) = s.append(b"end").unwrap();
        assert_eq!((n, size), (3, (1 << 40) + 3));
        // Hole punch frees whole pages and zeroes partial ones.
        s.fallocate(FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 4097, 4096 * 4).unwrap();
        assert!(s.has_page(1) && !s.has_page(2));
        assert_eq!(s.read(4096, &mut b[..2]), 2);
        assert_eq!(&b[..2], &[7, 0]);
    }
    assert_eq!(frame::stats().1, free_before);
}

#[test_case]
fn shmem_seals() {
    let s = Shmem::new(4096);
    s.set_initial_seals(0);
    s.add_seals(F_SEAL_SHRINK | F_SEAL_GROW).unwrap();
    assert_eq!(s.truncate(100), Err(EPERM));
    assert_eq!(s.truncate(8192), Err(EPERM));
    assert_eq!(s.write(4095, b"ab"), Err(EPERM));
    assert_eq!(s.write(4094, b"ab"), Ok(2));
    assert_eq!(s.fallocate(0, 0, 8192), Err(EPERM));
    s.add_seals(F_SEAL_WRITE | F_SEAL_SEAL).unwrap();
    assert_eq!(s.write(0, b"a"), Err(EPERM));
    assert_eq!(s.add_seals(F_SEAL_EXEC), Err(EPERM));
    assert_eq!(s.seals(), F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL);
}
