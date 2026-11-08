//! User address spaces.
//!
//! Each process has its own PML4: entries 0..256 (the lower half) map user
//! memory, entries 256..512 are copied from the kernel PML4 (whose PDPTs
//! are all pre-allocated, so the kernel half stays in sync).
//!
//! Memory is described by VMAs (start, end, protection). B1 keeps it
//! simple: anonymous pages are allocated zero-filled on first touch (page
//! fault or kernel access), ELF segments are copied in eagerly, and `fork`
//! copies every present page (copy-on-write is phase B3). Shared and
//! file-backed mappings are B3 as well.
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
}

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
            self.vmas.insert(s, Vma { end: at, prot: v.prot });
            self.vmas.insert(at, v);
        }
    }

    /// Insert a VMA, merging with an adjacent one of the same protection.
    fn insert(&mut self, mut start: u64, end: u64, prot: u32) {
        if let Some((&ps, pv)) = self.vmas.range(..start).next_back()
            && pv.end == start
            && pv.prot == prot
        {
            start = ps;
        }
        let end = match self.vmas.get(&end) {
            Some(nv) if nv.prot == prot => {
                let e = nv.end;
                self.vmas.remove(&end);
                e
            }
            _ => end,
        };
        self.vmas.insert(start, Vma { end, prot });
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
        e.set_frame(f, pte_flags(prot));
        Some(f.start_address())
    }

    /// Physical address backing user page `va`, faulting it in when the VMA
    /// allows. `write` requires a writable mapping unless `force`.
    fn user_page(&self, inner: &Inner, va: u64, write: bool, force: bool) -> KResult<PhysAddr> {
        if va >= USER_END {
            return Err(EFAULT);
        }
        let page = page_down(va);
        if let Some(e) = self.walk(page, false)
            && e.flags().contains(F::PRESENT)
        {
            if write && !force && !e.flags().contains(F::WRITABLE) {
                return Err(EFAULT);
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
        let mut done = 0usize;
        while done < len {
            let a = va + done as u64;
            let off = (a % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - off).min(len - done);
            let phys = self.user_page(&inner, a, write, force)?;
            let p = phys_to_virt(phys + off as u64).as_mut_ptr::<u8>();
            f(p, done, n);
            done += n;
        }
        Ok(())
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

    /// Resolve a fault on a not-present page. False means SIGSEGV.
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
        if self.walk(page, false).is_some_and(|e| e.flags().contains(F::PRESENT)) {
            return true; // another thread got there first
        }
        self.map_zero(page, vma.prot).is_some()
    }

    /// Add a VMA for `[start, end)`; the range must be free.
    pub fn add_vma(&self, start: u64, end: u64, prot: u32) -> KResult<()> {
        let mut inner = self.inner.lock();
        if inner.overlaps(start, end) {
            return Err(EINVAL);
        }
        inner.insert(start, end, prot);
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
            let combined = v.prot | prot;
            for p in (s..shared_end).step_by(PAGE_SIZE as usize) {
                if let Some(e) = self.walk(p, false)
                    && e.flags().contains(F::PRESENT)
                {
                    let addr = e.addr();
                    e.set_addr(addr, pte_flags(combined));
                }
            }
            inner.split_at(s);
            inner.split_at(shared_end);
            if let Some(vv) = inner.vmas.get_mut(&s) {
                vv.prot = combined;
            }
            s = shared_end;
        }
        if s < end {
            if inner.overlaps(s, end) {
                return Err(EINVAL);
            }
            inner.insert(s, end, prot);
        }
        Ok(())
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
            inner.insert(old_end, new_end, PROT_READ | PROT_WRITE);
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

    /// Anonymous private mapping. `fixed` replaces whatever is there.
    pub fn mmap(&self, addr: u64, len: u64, prot: u32, fixed: bool, noreplace: bool) -> KResult<u64> {
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
        inner.insert(start, start + len, prot);
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
        // Every page must be mapped.
        let mut a = addr;
        while a < end {
            let (_, v) = inner.find(a).ok_or(ENOMEM)?;
            a = v.end;
        }
        inner.split_at(addr);
        inner.split_at(end);
        let starts: Vec<u64> = inner.vmas.range(addr..end).map(|(&s, _)| s).collect();
        for s in starts {
            inner.vmas.get_mut(&s).unwrap().prot = prot;
        }
        for p in (addr..end).step_by(PAGE_SIZE as usize) {
            if let Some(e) = self.walk(p, false)
                && e.flags().contains(F::PRESENT)
            {
                let f = e.addr();
                if prot == 0 {
                    // Keep the frame reachable for a later mprotect.
                    e.set_addr(f, (pte_flags(0) - F::PRESENT) | F::BIT_9);
                } else {
                    e.set_addr(f, pte_flags(prot));
                }
            } else if let Some(e) = self.walk(p, false)
                && e.flags().contains(F::BIT_9)
                && prot != 0
            {
                let f = e.addr();
                e.set_addr(f, pte_flags(prot));
            }
        }
        drop(inner);
        crate::arch::x86_64::tlb::flush_local();
        crate::arch::x86_64::tlb::shootdown(self.active_cpus.load(Ordering::SeqCst));
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
                    && (e.flags().contains(F::PRESENT) || e.flags().contains(F::BIT_9))
                {
                    frames.push(PhysFrame::containing_address(e.addr()));
                    e.set_unused();
                }
            }
        }
        frames
    }

    fn finish_unmap(&self, frames: Vec<PhysFrame>) {
        if frames.is_empty() {
            return;
        }
        crate::arch::x86_64::tlb::flush_local();
        crate::arch::x86_64::tlb::shootdown(self.active_cpus.load(Ordering::SeqCst));
        for f in frames {
            frame::free(f);
        }
    }

    /// Eager copy of the whole address space (`fork` without CLONE_VM).
    pub fn fork(&self) -> Option<Arc<AddressSpace>> {
        let child = AddressSpace::new()?;
        let inner = self.inner.lock();
        {
            let mut ci = child.inner.lock();
            ci.vmas = inner.vmas.clone();
            ci.brk_start = inner.brk_start;
            ci.brk = inner.brk;
        }
        let mut ok = true;
        for_each_leaf(self.pml4, |va, e| {
            if !ok {
                return;
            }
            let Some(f) = frame::alloc() else {
                ok = false;
                return;
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    phys_to_virt(e.addr()).as_ptr::<u8>(),
                    phys_to_virt(f.start_address()).as_mut_ptr::<u8>(),
                    PAGE_SIZE as usize,
                )
            };
            match child.walk(va, true) {
                Some(ce) => ce.set_frame(f, e.flags()),
                None => {
                    frame::free(f);
                    ok = false;
                }
            }
        });
        ok.then_some(child)
    }
}

/// Call `f(va, entry)` for every present (or PROT_NONE-parked) user page.
fn for_each_leaf(pml4: PhysAddr, mut f: impl FnMut(u64, &PageTableEntry)) {
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
                    let e = &l1[i1];
                    if e.flags().contains(F::PRESENT) || e.flags().contains(F::BIT_9) {
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
        let a = mm.mmap(0, 3 * PAGE_SIZE, PROT_READ | PROT_WRITE, false, false).unwrap();
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
        let ro = mm.mmap(0, PAGE_SIZE, PROT_READ, false, false).unwrap();
        assert_eq!(mm.copy_out(ro, b"x"), Err(EFAULT));
    }
    assert_eq!(frame::stats().1, free_before);
}
