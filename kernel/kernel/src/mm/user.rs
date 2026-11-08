//! User address spaces.
//!
//! Each process has its own PML4: entries 0..256 (the lower half) map user
//! memory, entries 256..512 are copied from the kernel PML4 (whose PDPTs
//! are all pre-allocated, so the kernel half stays in sync).
//!
//! Memory is described by VMAs (start, end, protection, shared). Anonymous
//! pages are allocated zero-filled on first touch (page fault or kernel
//! access) and ELF segments are copied in eagerly.
//!
//! `fork` is copy-on-write: every present private page is shared by parent
//! and child with its PTE made read-only in both (frames are reference
//! counted, see `frame`). A write to such a page — by user code (page
//! fault) or by the kernel (`copy_out`) — copies it, or just makes it
//! writable again when this address space holds the only reference. The
//! rule is "PTE read-only while the VMA is writable and private" means
//! COW; there is no separate PTE bit for it.
//!
//! MAP_SHARED anonymous memory is a stop-gap until shmem objects (B3b):
//! fork populates every page of a shared VMA and maps the same frames,
//! writable, in both processes. File-backed mappings are B3b as well.
//!
//! The kernel never dereferences user pointers: `copy_in`/`copy_out` walk
//! the page tables under the address-space lock and go through the direct
//! map, so a bad pointer is an `EFAULT`, never a kernel page fault.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{PageTable, PageTableFlags as F, PhysFrame};

use super::paging::{kernel_pml4, phys_to_virt};
use super::{PAGE_SIZE, frame};
use crate::errno::*;

/// First address above user space.
pub const USER_END: u64 = 0x0000_8000_0000_0000;
pub const STACK_TOP: u64 = 0x7fff_ffff_f000;
pub const STACK_SIZE: u64 = 8 << 20;
/// `mmap` without a hint allocates top-down below this address.
const MMAP_TOP: u64 = 0x7f00_0000_0000;
const MMAP_BOTTOM: u64 = 0x1000_0000;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

#[derive(Clone, Copy, Debug)]
pub struct Vma {
    pub end: u64,
    pub prot: u32,
    /// MAP_SHARED: pages stay shared (never COW) across fork.
    pub shared: bool,
}

pub const MADV_NORMAL: u32 = 0;
pub const MADV_RANDOM: u32 = 1;
pub const MADV_SEQUENTIAL: u32 = 2;
pub const MADV_WILLNEED: u32 = 3;
pub const MADV_DONTNEED: u32 = 4;
pub const MADV_FREE: u32 = 8;

pub const MREMAP_MAYMOVE: u32 = 1;
pub const MREMAP_FIXED: u32 = 2;
pub const MREMAP_DONTUNMAP: u32 = 4;

struct Inner {
    /// start -> VMA, non-overlapping, page aligned.
    vmas: BTreeMap<u64, Vma>,
    brk_start: u64,
    brk: u64,
}

pub struct AddressSpace {
    pml4: PhysAddr,
    inner: Mutex<Inner>,
    /// CPUs (bit = index) that may have this address space loaded.
    pub active_cpus: AtomicU64,
}

fn table(phys: PhysAddr) -> &'static mut PageTable {
    unsafe { &mut *phys_to_virt(phys).as_mut_ptr::<PageTable>() }
}

fn page_down(a: u64) -> u64 {
    a & !(PAGE_SIZE - 1)
}

fn page_up(a: u64) -> Option<u64> {
    a.checked_add(PAGE_SIZE - 1).map(page_down)
}

fn pte_flags(prot: u32) -> F {
    let mut f = F::PRESENT | F::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        f |= F::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        f |= F::NO_EXECUTE;
    }
    f
}

/// PTE flags for a page of `vma` backed by `frame`: a private page that is
/// still shared with another address space stays read-only (COW).
fn leaf_flags(vma: &Vma, frame: PhysAddr) -> F {
    let f = pte_flags(vma.prot);
    if !vma.shared && frame::refcount(PhysFrame::containing_address(frame)) > 1 {
        f - F::WRITABLE
    } else {
        f
    }
}

/// True for a present PTE or one parked by PROT_NONE (both own a frame).
fn has_frame(e: &PageTableEntry) -> bool {
    e.flags().contains(F::PRESENT) || e.flags().contains(F::BIT_9)
}

/// A PROT_NONE page keeps its frame in a not-present PTE marked with BIT_9.
const PARKED_FLAGS: F = F::USER_ACCESSIBLE.union(F::NO_EXECUTE).union(F::BIT_9);

const TABLE_FLAGS: F = F::PRESENT.union(F::WRITABLE).union(F::USER_ACCESSIBLE);

impl Inner {
    fn find(&self, va: u64) -> Option<(u64, Vma)> {
        self.vmas
            .range(..=va)
            .next_back()
            .filter(|(_, v)| va < v.end)
            .map(|(&s, &v)| (s, v))
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.vmas
            .range(..end)
            .next_back()
            .is_some_and(|(_, v)| v.end > start)
    }

    /// Split VMAs so that `at` is a boundary.
    fn split_at(&mut self, at: u64) {
        if let Some((s, v)) = self.find(at)
            && s != at
        {
            self.vmas.insert(s, Vma { end: at, ..v });
            self.vmas.insert(at, v);
        }
    }

    /// Insert a VMA, merging with adjacent ones of the same kind.
    fn insert(&mut self, mut start: u64, end: u64, prot: u32, shared: bool) {
        if let Some((&ps, pv)) = self.vmas.range(..start).next_back()
            && pv.end == start
            && pv.prot == prot
            && pv.shared == shared
        {
            start = ps;
        }
        let end = match self.vmas.get(&end) {
            Some(nv) if nv.prot == prot && nv.shared == shared => {
                let e = nv.end;
                self.vmas.remove(&end);
                e
            }
            _ => end,
        };
        self.vmas.insert(start, Vma { end, prot, shared });
    }

    /// Fail with `err` unless every page of `[start, end)` is in a VMA.
    fn require_mapped(&self, start: u64, end: u64, err: Errno) -> KResult<()> {
        let mut a = start;
        while a < end {
            let (_, v) = self.find(a).ok_or(err)?;
            a = v.end;
        }
        Ok(())
    }

    /// Top-down search for a free, page-aligned range of `len` bytes.
    fn find_free(&self, len: u64) -> Option<u64> {
        let mut top = MMAP_TOP;
        for (&s, v) in self.vmas.range(..MMAP_TOP).rev() {
            if v.end <= top && top - v.end >= len {
                return Some(top - len);
            }
            top = top.min(s);
        }
        (top >= MMAP_BOTTOM + len).then(|| top - len)
    }
}

impl AddressSpace {
    pub fn new() -> Option<Arc<Self>> {
        let pml4 = frame::alloc_zeroed()?.start_address();
        let k = table(kernel_pml4());
        let t = table(pml4);
        for i in 256..512 {
            t[i] = k[i].clone();
        }
        Some(Arc::new(Self {
            pml4,
            inner: Mutex::new(Inner {
                vmas: BTreeMap::new(),
                brk_start: 0,
                brk: 0,
            }),
            active_cpus: AtomicU64::new(0),
        }))
    }

    /// Load this address space on the executing CPU.
    pub fn activate(&self) {
        let (cur, _) = Cr3::read();
        if cur.start_address() != self.pml4 {
            unsafe { Cr3::write(PhysFrame::containing_address(self.pml4), Cr3Flags::empty()) };
        }
    }

    /// The level-1 entry for `va`, creating intermediate tables if asked.
    fn walk(&self, va: u64, create: bool) -> Option<&'static mut PageTableEntry> {
        let mut t = table(self.pml4);
        for shift in [39, 30, 21] {
            let e = &mut t[((va >> shift) & 511) as usize];
            if e.is_unused() {
                if !create {
                    return None;
                }
                let f = frame::alloc_zeroed()?;
                e.set_frame(f, TABLE_FLAGS);
            }
            t = table(e.addr());
        }
        Some(&mut t[((va >> 12) & 511) as usize])
    }

    /// Map a fresh zeroed page at `va` (must be unmapped).
    fn map_zero(&self, va: u64, prot: u32) -> Option<PhysAddr> {
        let e = self.walk(va, true)?;
        let f = frame::alloc_zeroed()?;
        let flags = if prot == 0 { PARKED_FLAGS } else { pte_flags(prot) };
        e.set_frame(f, flags);
        Some(f.start_address())
    }

    /// Make the present private page `e` of `vma` writable (as far as the
    /// VMA allows), copying it if another address space shares the frame.
    /// Returns the old frame, to be released after a TLB flush, if it was
    /// replaced.
    fn break_cow(&self, e: &mut PageTableEntry, vma: &Vma) -> KResult<Option<PhysFrame>> {
        let old = PhysFrame::containing_address(e.addr());
        if frame::refcount(old) == 1 {
            e.set_addr(old.start_address(), pte_flags(vma.prot));
            return Ok(None);
        }
        let new = frame::alloc().ok_or(ENOMEM)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(old.start_address()).as_ptr::<u8>(),
                phys_to_virt(new.start_address()).as_mut_ptr::<u8>(),
                PAGE_SIZE as usize,
            )
        };
        e.set_addr(new.start_address(), pte_flags(vma.prot));
        Ok(Some(old))
    }

    /// Physical address backing user page `va`, faulting it in when the VMA
    /// allows. `write` requires a writable mapping unless `force`; a COW
    /// page is copied first, its old frame pushed to `release`.
    fn user_page(&self, inner: &Inner, va: u64, write: bool, force: bool, release: &mut Vec<PhysFrame>) -> KResult<PhysAddr> {
        if va >= USER_END {
            return Err(EFAULT);
        }
        let page = page_down(va);
        if let Some(e) = self.walk(page, false)
            && e.flags().contains(F::PRESENT)
        {
            if !write || e.flags().contains(F::WRITABLE) {
                return Ok(e.addr());
            }
            let (_, vma) = inner.find(page).ok_or(EFAULT)?;
            if vma.shared || (force && frame::refcount(PhysFrame::containing_address(e.addr())) == 1) {
                return if force { Ok(e.addr()) } else { Err(EFAULT) };
            }
            if !force && vma.prot & PROT_WRITE == 0 {
                return Err(EFAULT);
            }
            if let Some(old) = self.break_cow(e, &vma)? {
                release.push(old);
            }
            return Ok(e.addr());
        }
        let (_, vma) = inner.find(page).ok_or(EFAULT)?;
        if !force && (vma.prot == 0 || (write && vma.prot & PROT_WRITE == 0)) {
            return Err(EFAULT);
        }
        self.map_zero(page, vma.prot).ok_or(ENOMEM)
    }

    fn copy(&self, va: u64, len: usize, write: bool, force: bool, mut f: impl FnMut(*mut u8, usize, usize)) -> KResult<()> {
        let end = va.checked_add(len as u64).ok_or(EFAULT)?;
        if end > USER_END {
            return Err(EFAULT);
        }
        let inner = self.inner.lock();
        let mut release = Vec::new();
        let mut done = 0usize;
        let mut result = Ok(());
        while done < len {
            let a = va + done as u64;
            let off = (a % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - off).min(len - done);
            match self.user_page(&inner, a, write, force, &mut release) {
                Ok(phys) => f(phys_to_virt(phys + off as u64).as_mut_ptr::<u8>(), done, n),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
            done += n;
        }
        drop(inner);
        self.finish_unmap(release);
        result
    }

    /// Read user memory into `buf`.
    pub fn copy_in(&self, va: u64, buf: &mut [u8]) -> KResult<()> {
        let dst = buf.as_mut_ptr();
        self.copy(va, buf.len(), false, false, |p, done, n| unsafe {
            core::ptr::copy_nonoverlapping(p, dst.add(done), n)
        })
    }

    /// Write `data` to user memory.
    pub fn copy_out(&self, va: u64, data: &[u8]) -> KResult<()> {
        self.copy(va, data.len(), true, false, |p, done, n| unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr().add(done), p, n)
        })
    }

    /// Write `data` ignoring page protections (ELF loading, initial stack).
    pub fn copy_out_force(&self, va: u64, data: &[u8]) -> KResult<()> {
        self.copy(va, data.len(), true, true, |p, done, n| unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr().add(done), p, n)
        })
    }

    /// Read a NUL-terminated string of at most `max` bytes (without NUL).
    pub fn read_cstr(&self, va: u64, max: usize) -> KResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut a = va;
        loop {
            let chunk = (PAGE_SIZE - a % PAGE_SIZE) as usize;
            let mut buf = [0u8; 256];
            let n = chunk.min(buf.len());
            self.copy_in(a, &mut buf[..n])?;
            if let Some(z) = buf[..n].iter().position(|&c| c == 0) {
                out.extend_from_slice(&buf[..z]);
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > max {
                return Err(ENAMETOOLONG);
            }
            a += n as u64;
        }
    }

    /// Resolve a user page fault: demand-zero a missing page, or copy a COW
    /// page on write. False means SIGSEGV.
    pub fn handle_fault(&self, va: u64, write: bool, exec: bool) -> bool {
        if va >= USER_END {
            return false;
        }
        let inner = self.inner.lock();
        let Some((_, vma)) = inner.find(va) else {
            return false;
        };
        let allowed = match (write, exec) {
            (true, _) => vma.prot & PROT_WRITE != 0,
            (_, true) => vma.prot & PROT_EXEC != 0,
            _ => vma.prot != 0,
        };
        if !allowed {
            return false;
        }
        let page = page_down(va);
        let Some(e) = self.walk(page, false).filter(|e| e.flags().contains(F::PRESENT)) else {
            return self.map_zero(page, vma.prot).is_some();
        };
        // Present: either another thread or CPU got there first (stale TLB
        // entry), or this is a write to a COW page.
        if exec && e.flags().contains(F::NO_EXECUTE) {
            return false;
        }
        if !write || e.flags().contains(F::WRITABLE) {
            return true;
        }
        if vma.shared {
            return false;
        }
        match self.break_cow(e, &vma) {
            Ok(old) => {
                drop(inner);
                self.finish_unmap(old.into_iter().collect());
                true
            }
            Err(_) => false,
        }
    }

    /// Add a VMA for `[start, end)`; the range must be free.
    pub fn add_vma(&self, start: u64, end: u64, prot: u32) -> KResult<()> {
        let mut inner = self.inner.lock();
        if inner.overlaps(start, end) {
            return Err(EINVAL);
        }
        inner.insert(start, end, prot, false);
        Ok(())
    }

    /// Map `[start, end)` (page aligned) for an ELF segment. A page shared
    /// with the previous segment keeps one mapping with combined rights.
    pub fn map_segment(&self, start: u64, end: u64, prot: u32) -> KResult<()> {
        let mut inner = self.inner.lock();
        let mut s = start;
        if let Some((_, v)) = inner.find(s) {
            // Overlap with the previous segment's last page.
            let shared_end = v.end.min(end);
            let combined = Vma { prot: v.prot | prot, ..v };
            for p in (s..shared_end).step_by(PAGE_SIZE as usize) {
                if let Some(e) = self.walk(p, false)
                    && e.flags().contains(F::PRESENT)
                {
                    let addr = e.addr();
                    e.set_addr(addr, leaf_flags(&combined, addr));
                }
            }
            inner.split_at(s);
            inner.split_at(shared_end);
            if let Some(vv) = inner.vmas.get_mut(&s) {
                vv.prot = combined.prot;
            }
            s = shared_end;
        }
        if s < end {
            if inner.overlaps(s, end) {
                return Err(EINVAL);
            }
            inner.insert(s, end, prot, false);
        }
        Ok(())
    }

    /// Every VMA as (start, end, prot), for /proc/<pid>/maps.
    pub fn vma_list(&self) -> Vec<(u64, u64, u32)> {
        let inner = self.inner.lock();
        inner.vmas.iter().map(|(&s, v)| (s, v.end, v.prot)).collect()
    }

    /// (start, current) of the brk heap.
    pub fn brk_range(&self) -> (u64, u64) {
        let inner = self.inner.lock();
        (inner.brk_start, inner.brk)
    }

    pub fn set_brk_start(&self, start: u64) {
        let mut inner = self.inner.lock();
        inner.brk_start = start;
        inner.brk = start;
    }

    /// `brk(2)`: returns the new (or unchanged) break.
    pub fn brk(&self, new: u64) -> u64 {
        let mut inner = self.inner.lock();
        let (base, cur) = (inner.brk_start, inner.brk);
        if new < base || new >= MMAP_BOTTOM.max(base + (1 << 40)) {
            return cur;
        }
        let (old_end, new_end) = (page_up(cur).unwrap(), page_up(new).unwrap());
        if new_end > old_end {
            if inner.overlaps(old_end, new_end) {
                return cur;
            }
            inner.insert(old_end, new_end, PROT_READ | PROT_WRITE, false);
        } else if new_end < old_end {
            let frames = self.unmap_locked(&mut inner, new_end, old_end);
            drop(inner);
            self.finish_unmap(frames);
            let mut inner = self.inner.lock();
            inner.brk = new;
            return new;
        }
        inner.brk = new;
        new
    }

    /// Anonymous mapping. `fixed` replaces whatever is there.
    pub fn mmap(&self, addr: u64, len: u64, prot: u32, shared: bool, fixed: bool, noreplace: bool) -> KResult<u64> {
        let len = page_up(len).filter(|&l| l > 0 && l < USER_END).ok_or(EINVAL)?;
        let mut inner = self.inner.lock();
        let start = if fixed || noreplace {
            if !addr.is_multiple_of(PAGE_SIZE) || addr + len > USER_END || addr == 0 {
                return Err(EINVAL);
            }
            if inner.overlaps(addr, addr + len) {
                if noreplace {
                    return Err(EEXIST);
                }
                let frames = self.unmap_locked(&mut inner, addr, addr + len);
                drop(inner);
                self.finish_unmap(frames);
                inner = self.inner.lock();
            }
            addr
        } else {
            let hint = page_down(addr);
            if hint >= MMAP_BOTTOM && hint + len <= MMAP_TOP && !inner.overlaps(hint, hint + len) {
                hint
            } else {
                inner.find_free(len).ok_or(ENOMEM)?
            }
        };
        inner.insert(start, start + len, prot, shared);
        Ok(start)
    }

    pub fn munmap(&self, addr: u64, len: u64) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
            return Err(EINVAL);
        }
        let end = page_up(addr.checked_add(len).ok_or(EINVAL)?).ok_or(EINVAL)?;
        if end > USER_END {
            return Err(EINVAL);
        }
        let mut inner = self.inner.lock();
        let frames = self.unmap_locked(&mut inner, addr, end);
        drop(inner);
        self.finish_unmap(frames);
        Ok(())
    }

    pub fn mprotect(&self, addr: u64, len: u64, prot: u32) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(EINVAL);
        }
        let end = page_up(addr.checked_add(len).ok_or(ENOMEM)?).ok_or(ENOMEM)?;
        let mut inner = self.inner.lock();
        inner.require_mapped(addr, end, ENOMEM)?;
        inner.split_at(addr);
        inner.split_at(end);
        let starts: Vec<u64> = inner.vmas.range(addr..end).map(|(&s, _)| s).collect();
        for s in starts {
            inner.vmas.get_mut(&s).unwrap().prot = prot;
        }
        for p in (addr..end).step_by(PAGE_SIZE as usize) {
            if let Some(e) = self.walk(p, false)
                && has_frame(e)
            {
                let f = e.addr();
                if prot == 0 {
                    // Keep the frame reachable for a later mprotect.
                    e.set_addr(f, PARKED_FLAGS);
                } else {
                    let (_, vma) = inner.find(p).unwrap();
                    e.set_addr(f, leaf_flags(&vma, f));
                }
            }
        }
        drop(inner);
        self.flush_tlb();
        Ok(())
    }

    /// `madvise(2)`. DONTNEED and FREE drop the private pages of the range
    /// (the next touch maps a zeroed page); the other accepted hints are
    /// no-ops.
    pub fn madvise(&self, addr: u64, len: u64, advice: u32) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(EINVAL);
        }
        let drop_pages = match advice {
            MADV_DONTNEED | MADV_FREE => true,
            MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED => false,
            // DONTFORK..DODUMP, COLD, PAGEOUT: accepted, ignored.
            10..=17 | 20 | 21 => false,
            _ => return Err(EINVAL),
        };
        let end = page_up(addr.checked_add(len).ok_or(EINVAL)?).ok_or(EINVAL)?;
        if end > USER_END {
            return Err(ENOMEM);
        }
        let inner = self.inner.lock();
        inner.require_mapped(addr, end, ENOMEM)?;
        if !drop_pages {
            return Ok(());
        }
        let mut frames = Vec::new();
        for p in (addr..end).step_by(PAGE_SIZE as usize) {
            // Shared pages keep their contents (they would come back from
            // the shmem object on Linux).
            if inner.find(p).is_some_and(|(_, v)| v.shared) {
                continue;
            }
            if let Some(e) = self.walk(p, false)
                && has_frame(e)
            {
                frames.push(PhysFrame::containing_address(e.addr()));
                e.set_unused();
            }
        }
        drop(inner);
        self.finish_unmap(frames);
        Ok(())
    }

    /// `mremap(2)` for anonymous mappings. Pages move by moving their PTEs.
    pub fn mremap(&self, old: u64, old_len: u64, new_len: u64, flags: u32, new_addr: u64) -> KResult<u64> {
        let fixed = flags & MREMAP_FIXED != 0;
        let may_move = flags & MREMAP_MAYMOVE != 0;
        let dontunmap = flags & MREMAP_DONTUNMAP != 0;
        if flags & !(MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) != 0
            || !old.is_multiple_of(PAGE_SIZE)
            || (fixed || dontunmap) && !may_move
        {
            return Err(EINVAL);
        }
        let old_len = page_up(old_len).ok_or(EINVAL)?;
        let new_len = page_up(new_len).filter(|&l| l > 0 && l < USER_END).ok_or(EINVAL)?;
        // old_len == 0 duplicates a shared mapping on Linux: not supported.
        if old_len == 0 || (dontunmap && old_len != new_len) {
            return Err(EINVAL);
        }
        let old_end = old.checked_add(old_len).filter(|&e| e <= USER_END).ok_or(EFAULT)?;
        let mut inner = self.inner.lock();
        let (_, vma) = inner.find(old).filter(|(_, v)| v.end >= old_end).ok_or(EFAULT)?;
        let mut release = Vec::new();
        let result = if fixed {
            let new_end = new_addr.checked_add(new_len).filter(|&e| e <= USER_END).ok_or(EINVAL)?;
            if !new_addr.is_multiple_of(PAGE_SIZE) || new_addr == 0 || (new_addr < old_end && old < new_end) {
                return Err(EINVAL);
            }
            release = self.unmap_locked(&mut inner, new_addr, new_end);
            self.move_locked(&mut inner, old, old_len, new_addr, new_len, &vma, dontunmap, &mut release)
                .map(|_| new_addr)
        } else if new_len <= old_len && !dontunmap {
            release = self.unmap_locked(&mut inner, old + new_len, old_end);
            Ok(old)
        } else if !dontunmap
            && old + new_len <= USER_END
            && vma.end == old_end
            && !inner.overlaps(old_end, old + new_len)
        {
            inner.insert(old_end, old + new_len, vma.prot, vma.shared);
            Ok(old)
        } else if !may_move {
            Err(ENOMEM)
        } else {
            match inner.find_free(new_len) {
                Some(dest) => self
                    .move_locked(&mut inner, old, old_len, dest, new_len, &vma, dontunmap, &mut release)
                    .map(|_| dest),
                None => Err(ENOMEM),
            }
        };
        drop(inner);
        self.flush_tlb();
        for f in release {
            frame::free(f);
        }
        result
    }

    /// Move the pages of `[old, old + old_len)` to `dest` and give `dest`
    /// a VMA of `new_len` bytes. The old range is unmapped unless
    /// `dontunmap` (then it stays mapped and reads back as zeroes); frames
    /// past `new_len` are pushed to `release`.
    #[allow(clippy::too_many_arguments)]
    fn move_locked(&self, inner: &mut Inner, old: u64, old_len: u64, dest: u64, new_len: u64, vma: &Vma, dontunmap: bool, release: &mut Vec<PhysFrame>) -> KResult<()> {
        let moved = old_len.min(new_len);
        // Allocate every destination page table first so nothing fails
        // half-way.
        for off in (0..moved).step_by(PAGE_SIZE as usize) {
            if self.walk(old + off, false).is_some_and(|e| has_frame(e)) {
                self.walk(dest + off, true).ok_or(ENOMEM)?;
            }
        }
        for off in (0..moved).step_by(PAGE_SIZE as usize) {
            if let Some(e) = self.walk(old + off, false)
                && has_frame(e)
            {
                *self.walk(dest + off, true).unwrap() = e.clone();
                e.set_unused();
            }
        }
        if !dontunmap {
            release.extend(self.unmap_locked(inner, old, old + old_len));
        }
        inner.insert(dest, dest + new_len, vma.prot, vma.shared);
        Ok(())
    }

    /// Remove `[start, end)` from the VMAs and page tables; returns the
    /// frames to free once no CPU can still reach them.
    fn unmap_locked(&self, inner: &mut Inner, start: u64, end: u64) -> Vec<PhysFrame> {
        inner.split_at(start);
        inner.split_at(end);
        let starts: Vec<u64> = inner.vmas.range(start..end).map(|(&s, _)| s).collect();
        let mut frames = Vec::new();
        for s in starts {
            let v = inner.vmas.remove(&s).unwrap();
            for p in (s..v.end).step_by(PAGE_SIZE as usize) {
                if let Some(e) = self.walk(p, false)
                    && has_frame(e)
                {
                    frames.push(PhysFrame::containing_address(e.addr()));
                    e.set_unused();
                }
            }
        }
        frames
    }

    /// Flush this address space's TLB entries on every CPU using it.
    fn flush_tlb(&self) {
        crate::arch::x86_64::tlb::flush_local();
        crate::arch::x86_64::tlb::shootdown(self.active_cpus.load(Ordering::SeqCst));
    }

    /// Flush the TLBs, then drop a reference to each of `frames`.
    fn finish_unmap(&self, frames: Vec<PhysFrame>) {
        if frames.is_empty() {
            return;
        }
        self.flush_tlb();
        for f in frames {
            frame::free(f);
        }
    }

    /// Copy-on-write copy of the address space (`fork` without CLONE_VM).
    pub fn fork(&self) -> Option<Arc<AddressSpace>> {
        let child = AddressSpace::new()?;
        let inner = self.inner.lock();
        {
            let mut ci = child.inner.lock();
            ci.vmas = inner.vmas.clone();
            ci.brk_start = inner.brk_start;
            ci.brk = inner.brk;
        }
        // Shared VMAs have no backing object yet (B3b): populate them so
        // both processes map the same frames.
        for (&s, v) in inner.vmas.iter().filter(|(_, v)| v.shared) {
            for p in (s..v.end).step_by(PAGE_SIZE as usize) {
                if !self.walk(p, false).is_some_and(|e| has_frame(e)) {
                    self.map_zero(p, v.prot)?;
                }
            }
        }
        let mut ok = true;
        for_each_leaf(self.pml4, |va, e| {
            if !ok {
                return;
            }
            let Some(ce) = child.walk(va, true) else {
                ok = false;
                return;
            };
            let shared = inner.find(va).is_some_and(|(_, v)| v.shared);
            frame::get(PhysFrame::containing_address(e.addr()));
            if !shared && e.flags().contains(F::WRITABLE) {
                e.set_flags(e.flags() - F::WRITABLE);
            }
            *ce = e.clone();
        });
        drop(inner);
        // Parent PTEs went read-only: other CPUs running its threads must
        // not keep writable entries.
        self.flush_tlb();
        ok.then_some(child)
    }
}

/// Call `f(va, entry)` for every present (or PROT_NONE-parked) user page.
fn for_each_leaf(pml4: PhysAddr, mut f: impl FnMut(u64, &mut PageTableEntry)) {
    let l4 = table(pml4);
    for i4 in 0..256 {
        if l4[i4].is_unused() {
            continue;
        }
        let l3 = table(l4[i4].addr());
        for i3 in 0..512 {
            if l3[i3].is_unused() {
                continue;
            }
            let l2 = table(l3[i3].addr());
            for i2 in 0..512 {
                if l2[i2].is_unused() {
                    continue;
                }
                let l1 = table(l2[i2].addr());
                for i1 in 0..512 {
                    let e = &mut l1[i1];
                    if has_frame(e) {
                        let va = ((i4 as u64) << 39) | ((i3 as u64) << 30) | ((i2 as u64) << 21) | ((i1 as u64) << 12);
                        f(va, e);
                    }
                }
            }
        }
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // No CPU has this PML4 loaded any more (the last task using it has
        // switched away), so frames can be freed right away.
        for_each_leaf(self.pml4, |_, e| frame::free(PhysFrame::containing_address(e.addr())));
        let l4 = table(self.pml4);
        for i4 in 0..256 {
            if l4[i4].is_unused() {
                continue;
            }
            let l3 = table(l4[i4].addr());
            for i3 in 0..512 {
                if l3[i3].is_unused() {
                    continue;
                }
                let l2 = table(l3[i3].addr());
                for i2 in 0..512 {
                    if !l2[i2].is_unused() {
                        frame::free(PhysFrame::containing_address(l2[i2].addr()));
                    }
                }
                frame::free(PhysFrame::containing_address(l3[i3].addr()));
            }
            frame::free(PhysFrame::containing_address(l4[i4].addr()));
        }
        frame::free(PhysFrame::containing_address(self.pml4));
    }
}

#[test_case]
fn user_space_map_copy_free() {
    let (_, free_before) = frame::stats();
    {
        let mm = AddressSpace::new().unwrap();
        let a = mm.mmap(0, 3 * PAGE_SIZE, PROT_READ | PROT_WRITE, false, false, false).unwrap();
        mm.copy_out(a + PAGE_SIZE - 2, b"hello").unwrap();
        let mut buf = [0u8; 5];
        mm.copy_in(a + PAGE_SIZE - 2, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        let child = mm.fork().unwrap();
        mm.copy_out(a + PAGE_SIZE - 2, b"HELLO").unwrap();
        child.copy_in(a + PAGE_SIZE - 2, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        mm.munmap(a, PAGE_SIZE).unwrap();
        assert_eq!(mm.copy_in(a, &mut buf), Err(EFAULT));
        assert_eq!(mm.copy_in(0x1000, &mut buf), Err(EFAULT));
        let ro = mm.mmap(0, PAGE_SIZE, PROT_READ, false, false, false).unwrap();
        assert_eq!(mm.copy_out(ro, b"x"), Err(EFAULT));
    }
    assert_eq!(frame::stats().1, free_before);
}

#[cfg(test)]
fn phys_of(mm: &AddressSpace, va: u64) -> Option<PhysAddr> {
    mm.walk(va, false).filter(|e| has_frame(e)).map(|e| e.addr())
}

#[test_case]
fn cow_fork_shares_then_copies_without_leaks() {
    let (_, free_before) = frame::stats();
    {
        let mm = AddressSpace::new().unwrap();
        let rw = PROT_READ | PROT_WRITE;
        let a = mm.mmap(0, 8 * PAGE_SIZE, rw, false, false, false).unwrap();
        for i in 0..8 {
            mm.copy_out(a + i * PAGE_SIZE, &[i as u8]).unwrap();
        }
        let sh = mm.mmap(0, PAGE_SIZE, rw, true, false, false).unwrap();
        let child = mm.fork().unwrap();
        // Private pages are shared read-only, shared pages stay writable.
        let p0 = phys_of(&mm, a).unwrap();
        assert_eq!(phys_of(&child, a), Some(p0));
        assert_eq!(frame::refcount(PhysFrame::containing_address(p0)), 2);
        assert!(!mm.walk(a, false).unwrap().flags().contains(F::WRITABLE));
        assert!(child.walk(sh, false).unwrap().flags().contains(F::WRITABLE));
        assert_eq!(phys_of(&child, sh), phys_of(&mm, sh));
        // A write fault in the child copies; the parent then holds the only
        // reference and gets its page back without a copy.
        assert!(child.handle_fault(a, true, false));
        assert_ne!(phys_of(&child, a), Some(p0));
        assert!(mm.handle_fault(a, true, false));
        assert_eq!(phys_of(&mm, a), Some(p0));
        // Kernel writes (copy_out) break COW too.
        child.copy_out(a + PAGE_SIZE, b"c").unwrap();
        let mut b = [0u8; 1];
        mm.copy_in(a + PAGE_SIZE, &mut b).unwrap();
        assert_eq!(b[0], 1);
        child.copy_out(sh, b"s").unwrap();
        mm.copy_in(sh, &mut b).unwrap();
        assert_eq!(&b, b"s");
        // Grandchild, then the middle generation exits first.
        let grand = child.fork().unwrap();
        drop(child);
        grand.copy_in(a + PAGE_SIZE, &mut b).unwrap();
        assert_eq!(&b, b"c");
        // mprotect RW on a still-shared page keeps it read-only.
        mm.mprotect(a + 2 * PAGE_SIZE, PAGE_SIZE, PROT_READ).unwrap();
        mm.mprotect(a + 2 * PAGE_SIZE, PAGE_SIZE, rw).unwrap();
        assert!(!mm.walk(a + 2 * PAGE_SIZE, false).unwrap().flags().contains(F::WRITABLE));
        drop(grand);
    }
    assert_eq!(frame::stats().1, free_before);
}

#[test_case]
fn madvise_and_mremap() {
    let (_, free_before) = frame::stats();
    {
        let mm = AddressSpace::new().unwrap();
        let rw = PROT_READ | PROT_WRITE;
        let a = mm.mmap(0, 2 * PAGE_SIZE, rw, false, false, false).unwrap();
        mm.copy_out(a, b"x").unwrap();
        assert_eq!(mm.madvise(a + 1, PAGE_SIZE, MADV_DONTNEED), Err(EINVAL));
        assert_eq!(mm.madvise(a, PAGE_SIZE, 999), Err(EINVAL));
        mm.madvise(a, 2 * PAGE_SIZE, MADV_DONTNEED).unwrap();
        assert_eq!(phys_of(&mm, a), None);
        let mut b = [1u8; 1];
        mm.copy_in(a, &mut b).unwrap();
        assert_eq!(b[0], 0);

        // Move: the frame follows the PTE, the old range is gone.
        mm.copy_out(a, b"m").unwrap();
        let p = phys_of(&mm, a).unwrap();
        mm.mmap(a + 2 * PAGE_SIZE, PAGE_SIZE, PROT_READ, false, true, false).unwrap();
        let n = mm.mremap(a, 2 * PAGE_SIZE, 8 * PAGE_SIZE, MREMAP_MAYMOVE, 0).unwrap();
        assert_ne!(n, a);
        assert_eq!(phys_of(&mm, n), Some(p));
        assert_eq!(mm.copy_in(a, &mut b), Err(EFAULT));
        // Shrink in place, then grow in place into the freed tail.
        assert_eq!(mm.mremap(n, 8 * PAGE_SIZE, PAGE_SIZE, 0, 0), Ok(n));
        assert_eq!(mm.mremap(n, PAGE_SIZE, 4 * PAGE_SIZE, 0, 0), Ok(n));
        mm.copy_out(n + 3 * PAGE_SIZE, b"t").unwrap();
        // Fixed destination with DONTUNMAP: old range reads back zero.
        let dst = MMAP_BOTTOM;
        let r = mm.mremap(n, 4 * PAGE_SIZE, 4 * PAGE_SIZE, MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP, dst);
        assert_eq!(r, Ok(dst));
        mm.copy_in(dst, &mut b).unwrap();
        assert_eq!(&b, b"m");
        mm.copy_in(n, &mut b).unwrap();
        assert_eq!(b[0], 0);
        assert_eq!(mm.mremap(dst + PAGE_SIZE, PAGE_SIZE, 2 * PAGE_SIZE, MREMAP_FIXED, 0), Err(EINVAL));
    }
    assert_eq!(frame::stats().1, free_before);
}
