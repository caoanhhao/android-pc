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
//! A VMA may be backed by a shmem object (`shmem`): tmpfs/memfd file
//! mappings and MAP_SHARED anonymous memory (which gets an object of its
//! own at mmap time). Faults on such a VMA map the object's frames:
//! writable for MAP_SHARED, read-only (COW, the object holds a reference)
//! for MAP_PRIVATE. A page past the end of the object raises SIGBUS.
//!
//! The kernel never dereferences user pointers: `copy_in`/`copy_out` walk
//! the page tables under the address-space lock and go through the direct
//! map, so a bad pointer is an `EFAULT`, never a kernel page fault.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{PageTable, PageTableFlags as F, PhysFrame};

use super::paging::{kernel_pml4, phys_to_virt};
use super::shmem::Shmem;
use super::userfaultfd::{self as uffd, UffdCtx};
use super::{PAGE_SIZE, frame};
use crate::errno::*;
use crate::fs::file::OpenFile;

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

/// The object behind a file or shared mapping.
#[derive(Clone)]
pub struct Backing {
    pub obj: Arc<Shmem>,
    /// Page index in `obj` of the VMA's first page.
    pub pgoff: u64,
    /// The file it was mapped from (None: anonymous shared memory).
    pub file: Option<Arc<OpenFile>>,
}

#[derive(Clone)]
pub struct Vma {
    pub end: u64,
    pub prot: u32,
    /// MAP_SHARED: pages stay shared (never COW) across fork.
    pub shared: bool,
    /// Shared mappings only: PROT_WRITE may be added (Linux VM_MAYWRITE;
    /// false for a read-only fd or a write-sealed memfd).
    pub maywrite: bool,
    pub backing: Option<Backing>,
    /// MADV_WIPEONFORK (private anonymous only): a fork child sees zeroes.
    pub wipeonfork: bool,
    /// userfaultfd registration (not inherited by fork or mremap).
    pub uffd: Option<UffdReg>,
}

/// A VMA's userfaultfd registration: the context and the
/// UFFDIO_REGISTER_MODE_* bits.
#[derive(Clone)]
pub struct UffdReg {
    pub ctx: Arc<UffdCtx>,
    pub mode: u32,
}

impl UffdReg {
    fn same(a: &Option<UffdReg>, b: &Option<UffdReg>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => Arc::ptr_eq(&x.ctx, &y.ctx) && x.mode == y.mode,
            _ => false,
        }
    }
}

impl Vma {
    fn anon(end: u64, prot: u32) -> Vma {
        Vma { end, prot, shared: false, maywrite: true, backing: None, wipeonfork: false, uffd: None }
    }

    /// This VMA (starting at `start`) cut to begin at `at` instead.
    fn tail(&self, start: u64, at: u64) -> Vma {
        let mut v = self.clone();
        if let Some(b) = &mut v.backing {
            b.pgoff += (at - start) / PAGE_SIZE;
        }
        v
    }

    /// Whether `next` (at `next_start`) continues this VMA (at `start`).
    fn continued_by(&self, start: u64, next_start: u64, next: &Vma) -> bool {
        if self.end != next_start
            || self.prot != next.prot
            || self.shared != next.shared
            || self.maywrite != next.maywrite
            || self.wipeonfork != next.wipeonfork
            || !UffdReg::same(&self.uffd, &next.uffd)
        {
            return false;
        }
        match (&self.backing, &next.backing) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                Arc::ptr_eq(&a.obj, &b.obj)
                    && a.pgoff + (self.end - start) / PAGE_SIZE == b.pgoff
                    && match (&a.file, &b.file) {
                        (None, None) => true,
                        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// The object and page index behind `va` (this VMA starts at `start`).
    fn object_page(&self, start: u64, va: u64) -> Option<(&Arc<Shmem>, u64)> {
        self.backing.as_ref().map(|b| (&b.obj, b.pgoff + (va - start) / PAGE_SIZE))
    }
}

/// A VMA as /proc/<pid>/maps shows it.
pub struct VmaInfo {
    pub start: u64,
    pub end: u64,
    pub prot: u32,
    pub shared: bool,
    /// Byte offset in the file or object.
    pub offset: u64,
    pub file: Option<Arc<OpenFile>>,
    /// Backed by an object (anonymous shared memory when `file` is None).
    pub backed: bool,
}

/// Why a user page fault could not be resolved.
#[derive(Clone)]
pub enum FaultError {
    /// No mapping or not allowed: SIGSEGV.
    Segv,
    /// Past the end of the backing object (or out of memory), or a
    /// userfault in UFFD_FEATURE_SIGBUS mode: SIGBUS.
    Bus,
    /// A missing or minor fault in a userfaultfd-registered range: the
    /// thread waits for the monitor (UFFD_PAGEFAULT_FLAG_* in `.1`).
    Userfault(Arc<UffdCtx>, u64),
}

impl PartialEq for FaultError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Segv, Self::Segv) | (Self::Bus, Self::Bus) => true,
            (Self::Userfault(a, x), Self::Userfault(b, y)) => Arc::ptr_eq(a, b) && x == y,
            _ => false,
        }
    }
}

impl core::fmt::Debug for FaultError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Segv => f.write_str("Segv"),
            Self::Bus => f.write_str("Bus"),
            Self::Userfault(_, fl) => write!(f, "Userfault({fl:#x})"),
        }
    }
}

pub const MADV_NORMAL: u32 = 0;
pub const MADV_RANDOM: u32 = 1;
pub const MADV_SEQUENTIAL: u32 = 2;
pub const MADV_WILLNEED: u32 = 3;
pub const MADV_DONTNEED: u32 = 4;
pub const MADV_FREE: u32 = 8;
pub const MADV_REMOVE: u32 = 9;
pub const MADV_WIPEONFORK: u32 = 18;
pub const MADV_KEEPONFORK: u32 = 19;

pub const MS_ASYNC: u32 = 1;
pub const MS_INVALIDATE: u32 = 2;
pub const MS_SYNC: u32 = 4;

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
    /// Weak self-reference, handed to the shmem objects this maps.
    me: Weak<AddressSpace>,
    /// CPUs (bit = index) that may have this address space loaded.
    pub active_cpus: AtomicU64,
    /// Live tasks using it (Linux's mm_users). `Arc` references also come
    /// from exited tasks not yet freed and from kernel code holding it.
    pub users: AtomicU64,
    /// Initial stack pointer (/proc/<pid>/stat startstack, which bionic's
    /// pthread_getattr_np looks up in /proc/self/maps).
    pub start_stack: AtomicU64,
    /// Where exec put the argument strings (/proc/<pid>/cmdline reads
    /// them from memory, so argv[0] rewrites such as zygote's show).
    pub arg_range: Mutex<(u64, u64)>,
    /// membarrier(2) registrations (MEMBARRIER_CMD_REGISTER_* bits).
    pub membarrier: core::sync::atomic::AtomicU32,
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
            .map(|(&s, v)| (s, v.clone()))
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
            self.vmas.insert(at, v.tail(s, at));
            self.vmas.insert(s, Vma { end: at, ..v });
        }
    }

    /// Insert `vma` at `start`, merging with neighbours it continues.
    fn insert(&mut self, mut start: u64, mut vma: Vma) {
        if let Some((&ps, pv)) = self.vmas.range(..start).next_back()
            && pv.continued_by(ps, start, &vma)
        {
            vma = Vma { end: vma.end, ..pv.clone() };
            start = ps;
        }
        if let Some(nv) = self.vmas.get(&vma.end)
            && vma.continued_by(start, vma.end, nv)
        {
            let next = vma.end;
            vma.end = nv.end;
            self.vmas.remove(&next);
        }
        self.vmas.insert(start, vma);
    }

    /// Merge the VMAs touching `[start, end)` with neighbours they
    /// continue (after their attributes changed).
    fn remerge(&mut self, start: u64, end: u64) {
        let lo = self.vmas.range(..start).next_back().map_or(start, |(&s, _)| s);
        let keys: Vec<u64> = self.vmas.range(lo..=end).map(|(&s, _)| s).collect();
        for k in keys {
            if let Some(v) = self.vmas.remove(&k) {
                self.insert(k, v);
            }
        }
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
        Some(Arc::new_cyclic(|me| Self {
            pml4,
            inner: Mutex::new(Inner {
                vmas: BTreeMap::new(),
                brk_start: 0,
                brk: 0,
            }),
            me: me.clone(),
            active_cpus: AtomicU64::new(0),
            users: AtomicU64::new(0),
            start_stack: AtomicU64::new(0),
            arg_range: Mutex::new((0, 0)),
            membarrier: core::sync::atomic::AtomicU32::new(0),
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
        let new = copy_frame(old)?;
        e.set_addr(new.start_address(), pte_flags(vma.prot));
        Ok(Some(old))
    }

    /// Map the missing page `page` of `vma` (which starts at `start`): a
    /// zeroed page for anonymous memory, the object's frame otherwise (a
    /// private copy right away for a write to a private mapping). EFAULT
    /// past the end of the object.
    fn fault_missing(&self, start: u64, vma: &Vma, page: u64, write: bool) -> KResult<PhysAddr> {
        let Some((obj, idx)) = vma.object_page(start, page) else {
            return self.map_zero(page, vma.prot).ok_or(ENOMEM);
        };
        let e = self.walk(page, true).ok_or(ENOMEM)?;
        let mut f = obj.fault_page(idx)?;
        let mut flags = pte_flags(vma.prot);
        if !vma.shared {
            if write {
                let copy = copy_frame(f);
                frame::free(f); // the object still holds its reference
                f = copy?;
            } else {
                flags -= F::WRITABLE;
            }
        }
        if vma.prot == 0 {
            flags = PARKED_FLAGS;
        }
        e.set_frame(f, flags);
        Ok(f.start_address())
    }

    /// Physical address backing user page `va`, faulting it in when the VMA
    /// allows. `write` requires a writable mapping unless `force`; a COW
    /// page is copied first, its old frame pushed to `release`.
    fn user_page(&self, inner: &Inner, va: u64, write: bool, force: bool, release: &mut Vec<PhysFrame>, wait: &mut Option<(Arc<UffdCtx>, u64)>) -> KResult<PhysAddr> {
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
        let (start, vma) = inner.find(page).ok_or(EFAULT)?;
        if !force && (vma.prot == 0 || (write && vma.prot & PROT_WRITE == 0)) {
            return Err(EFAULT);
        }
        if let Some((ctx, flags)) = self.userfault(start, &vma, page) {
            if ctx.kernel_faults_fail() {
                return Err(EFAULT);
            }
            *wait = Some((ctx, flags | if write { uffd::PAGEFAULT_FLAG_WRITE } else { 0 }));
            return Err(EAGAIN);
        }
        self.fault_missing(start, &vma, page, write)
    }

    /// The userfault a missing PTE at `page` of `vma` (at `start`) raises,
    /// if the VMA is registered for it: the context and
    /// UFFD_PAGEFAULT_FLAG_MINOR when the shmem object has the page.
    fn userfault(&self, start: u64, vma: &Vma, page: u64) -> Option<(Arc<UffdCtx>, u64)> {
        let reg = vma.uffd.as_ref()?;
        let cached = vma.object_page(start, page).is_some_and(|(obj, idx)| obj.has_page(idx));
        let (mode, flags) = if cached { (uffd::MODE_MINOR, uffd::PAGEFAULT_FLAG_MINOR) } else { (uffd::MODE_MISSING, 0) };
        (reg.mode & mode != 0).then(|| (reg.ctx.clone(), flags))
    }

    /// Whether `va` has a present PTE (a userfault was resolved).
    pub fn page_present(&self, va: u64) -> bool {
        let _inner = self.inner.lock();
        self.walk(page_down(va), false).is_some_and(|e| e.flags().contains(F::PRESENT))
    }

    fn copy(&self, va: u64, len: usize, write: bool, force: bool, mut f: impl FnMut(*mut u8, usize, usize)) -> KResult<()> {
        let end = va.checked_add(len as u64).ok_or(EFAULT)?;
        if end > USER_END {
            return Err(EFAULT);
        }
        let mut done = 0usize;
        loop {
            let inner = self.inner.lock();
            let mut release = Vec::new();
            let mut wait = None;
            let mut result = Ok(());
            while done < len {
                let a = va + done as u64;
                let off = (a % PAGE_SIZE) as usize;
                let n = (PAGE_SIZE as usize - off).min(len - done);
                match self.user_page(&inner, a, write, force, &mut release, &mut wait) {
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
            // A userfault: wait for the monitor to fill the page, then go on.
            let Some((ctx, flags)) = wait else {
                return result;
            };
            let a = va + done as u64;
            if !ctx.wait_fault(a, flags, || self.page_present(a)) {
                return Err(EINTR);
            }
        }
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

    /// Atomically replace the u32 at `va` (4-byte aligned) with `new` if
    /// it holds `old`; returns the value found (PI futex words).
    pub fn cmpxchg_u32(&self, va: u64, old: u32, new: u32) -> KResult<u32> {
        if !va.is_multiple_of(4) {
            return Err(EINVAL);
        }
        let mut found = 0;
        self.copy(va, 4, true, false, |p, _, _| {
            let a = unsafe { &*(p as *const core::sync::atomic::AtomicU32) };
            found = match a.compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(v) | Err(v) => v,
            };
        })?;
        Ok(found)
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

    /// Resolve a user page fault: demand-zero a missing page, map a page
    /// of the backing object, or copy a COW page on write.
    pub fn handle_fault(&self, va: u64, write: bool, exec: bool) -> Result<(), FaultError> {
        use FaultError::*;
        if va >= USER_END {
            return Err(Segv);
        }
        let inner = self.inner.lock();
        let Some((start, vma)) = inner.find(va) else {
            return Err(Segv);
        };
        let allowed = match (write, exec) {
            (true, _) => vma.prot & PROT_WRITE != 0,
            (_, true) => vma.prot & PROT_EXEC != 0,
            _ => vma.prot != 0,
        };
        if !allowed {
            return Err(Segv);
        }
        let page = page_down(va);
        let Some(e) = self.walk(page, false).filter(|e| e.flags().contains(F::PRESENT)) else {
            if let Some((ctx, flags)) = self.userfault(start, &vma, page) {
                if ctx.sigbus() {
                    return Err(Bus);
                }
                return Err(Userfault(ctx, flags | if write { uffd::PAGEFAULT_FLAG_WRITE } else { 0 }));
            }
            return match self.fault_missing(start, &vma, page, write) {
                Ok(_) => Ok(()),
                Err(_) if vma.backing.is_some() => Err(Bus),
                Err(_) => Err(Segv),
            };
        };
        // Present: either another thread or CPU got there first (stale TLB
        // entry), or this is a write to a COW page.
        if exec && e.flags().contains(F::NO_EXECUTE) {
            return Err(Segv);
        }
        if !write || e.flags().contains(F::WRITABLE) {
            return Ok(());
        }
        if vma.shared {
            return Err(Segv);
        }
        match self.break_cow(e, &vma) {
            Ok(old) => {
                drop(inner);
                self.finish_unmap(old.into_iter().collect());
                Ok(())
            }
            Err(_) => Err(Segv),
        }
    }

    /// Add a VMA for `[start, end)`; the range must be free.
    pub fn add_vma(&self, start: u64, end: u64, prot: u32) -> KResult<()> {
        let mut inner = self.inner.lock();
        if inner.overlaps(start, end) {
            return Err(EINVAL);
        }
        inner.insert(start, Vma::anon(end, prot));
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
            inner.insert(s, Vma::anon(end, prot));
        }
        Ok(())
    }

    /// Every VMA, for /proc/<pid>/maps.
    pub fn vma_list(&self) -> Vec<VmaInfo> {
        let inner = self.inner.lock();
        inner
            .vmas
            .iter()
            .map(|(&start, v)| VmaInfo {
                start,
                end: v.end,
                prot: v.prot,
                shared: v.shared,
                offset: v.backing.as_ref().map_or(0, |b| b.pgoff * PAGE_SIZE),
                file: v.backing.as_ref().and_then(|b| b.file.clone()),
                backed: v.backing.is_some(),
            })
            .collect()
    }

    /// Total size of the VMAs in bytes.
    pub fn total_size(&self) -> u64 {
        self.inner.lock().vmas.iter().map(|(&s, v)| v.end - s).sum()
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
            inner.insert(old_end, Vma::anon(new_end, PROT_READ | PROT_WRITE));
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

    /// Anonymous mapping. `fixed` replaces whatever is there. MAP_SHARED
    /// memory gets a shmem object of its own (shared across fork).
    pub fn mmap(&self, addr: u64, len: u64, prot: u32, shared: bool, fixed: bool, noreplace: bool) -> KResult<u64> {
        let mut vma = Vma::anon(0, prot);
        if shared {
            let size = page_up(len).ok_or(EINVAL)?;
            vma.shared = true;
            vma.backing = Some(Backing { obj: Shmem::new(size), pgoff: 0, file: None });
        }
        self.map(addr, len, fixed, noreplace, vma)
    }

    /// Mapping of a shmem object (a file, or /dev/zero).
    #[allow(clippy::too_many_arguments)]
    pub fn mmap_object(&self, addr: u64, len: u64, prot: u32, shared: bool, maywrite: bool, backing: Backing, fixed: bool, noreplace: bool) -> KResult<u64> {
        let vma = Vma { end: 0, prot, shared, maywrite: maywrite || !shared, backing: Some(backing), wipeonfork: false, uffd: None };
        self.map(addr, len, fixed, noreplace, vma)
    }

    /// Map `vma` (its `end` is filled in) at `addr` / a free range.
    fn map(&self, addr: u64, len: u64, fixed: bool, noreplace: bool, mut vma: Vma) -> KResult<u64> {
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
        if let Some(b) = &vma.backing {
            b.obj.register(&self.me);
        }
        vma.end = start + len;
        inner.insert(start, vma);
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
        // A shared mapping of a read-only fd (or write-sealed memfd) can
        // never become writable.
        if prot & PROT_WRITE != 0 {
            let mut a = addr;
            while a < end {
                let (_, v) = inner.find(a).unwrap();
                if v.shared && !v.maywrite {
                    return Err(EACCES);
                }
                a = v.end;
            }
        }
        inner.split_at(addr);
        inner.split_at(end);
        let starts: Vec<u64> = inner.vmas.range(addr..end).map(|(&s, _)| s).collect();
        for s in starts {
            inner.vmas.get_mut(&s).unwrap().prot = prot;
        }
        // Rejoin pieces that match again (bionic's atexit array is
        // mprotect()ed page by page and later mremap()ed as one VMA).
        inner.remerge(addr, end);
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

    /// `madvise(2)`. DONTNEED and FREE drop the pages of the range: the
    /// next touch maps a zeroed page (private anonymous memory) or the
    /// backing object's page again. REMOVE punches a hole in the object
    /// behind a shared mapping. The other accepted hints are no-ops.
    pub fn madvise(&self, addr: u64, len: u64, advice: u32) -> KResult<()> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(EINVAL);
        }
        if advice == MADV_REMOVE {
            return self.madvise_remove(addr, len);
        }
        if advice == MADV_WIPEONFORK || advice == MADV_KEEPONFORK {
            return self.set_wipeonfork(addr, len, advice == MADV_WIPEONFORK);
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

    /// MADV_WIPEONFORK / MADV_KEEPONFORK on private anonymous memory.
    fn set_wipeonfork(&self, addr: u64, len: u64, on: bool) -> KResult<()> {
        let end = page_up(addr.checked_add(len).ok_or(EINVAL)?).ok_or(EINVAL)?;
        if end > USER_END {
            return Err(ENOMEM);
        }
        let mut inner = self.inner.lock();
        inner.require_mapped(addr, end, ENOMEM)?;
        let mut a = addr;
        while a < end {
            let (_, v) = inner.find(a).unwrap();
            if on && (v.shared || v.backing.is_some()) {
                return Err(EINVAL);
            }
            a = v.end;
        }
        inner.split_at(addr);
        inner.split_at(end);
        let starts: Vec<u64> = inner.vmas.range(addr..end).map(|(&s, _)| s).collect();
        for s in starts {
            inner.vmas.get_mut(&s).unwrap().wipeonfork = on;
        }
        inner.remerge(addr, end);
        Ok(())
    }

    /// MADV_REMOVE: free the object pages behind a shared mapping.
    fn madvise_remove(&self, addr: u64, len: u64) -> KResult<()> {
        let end = page_up(addr.checked_add(len).ok_or(EINVAL)?).ok_or(EINVAL)?;
        let mut holes = Vec::new();
        {
            let inner = self.inner.lock();
            inner.require_mapped(addr, end, ENOMEM)?;
            let mut a = addr;
            while a < end {
                let (s, v) = inner.find(a).unwrap();
                let to = v.end.min(end);
                match v.object_page(s, a) {
                    Some((obj, idx)) if v.shared && v.maywrite => holes.push((obj.clone(), idx, (to - a) / PAGE_SIZE)),
                    Some(_) if v.shared => return Err(EACCES),
                    _ => return Err(EINVAL),
                }
                a = to;
            }
        }
        // The object unmaps the punched pages from every mapping itself.
        for (obj, idx, n) in holes {
            obj.fallocate(
                super::shmem::FALLOC_FL_PUNCH_HOLE | super::shmem::FALLOC_FL_KEEP_SIZE,
                idx * PAGE_SIZE,
                n * PAGE_SIZE,
            )?;
        }
        Ok(())
    }

    /// `msync(2)`: argument checks only, every object is RAM-backed.
    pub fn msync(&self, addr: u64, len: u64, flags: u32) -> KResult<()> {
        if flags & !(MS_ASYNC | MS_INVALIDATE | MS_SYNC) != 0
            || !addr.is_multiple_of(PAGE_SIZE)
            || flags & (MS_ASYNC | MS_SYNC) == MS_ASYNC | MS_SYNC
        {
            return Err(EINVAL);
        }
        let end = page_up(addr.checked_add(len).ok_or(ENOMEM)?).ok_or(ENOMEM)?;
        if end > USER_END {
            return Err(ENOMEM);
        }
        self.inner.lock().require_mapped(addr, end, ENOMEM)
    }

    /// `mincore(2)`: one byte per page, 1 if resident (mapped, or in the
    /// backing object's cache).
    pub fn mincore(&self, addr: u64, len: u64) -> KResult<Vec<u8>> {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(EINVAL);
        }
        let end = page_up(addr.checked_add(len).ok_or(ENOMEM)?).ok_or(ENOMEM)?;
        if end > USER_END {
            return Err(ENOMEM);
        }
        let inner = self.inner.lock();
        inner.require_mapped(addr, end, ENOMEM)?;
        let mut v = Vec::new();
        v.try_reserve_exact(((end - addr) / PAGE_SIZE) as usize).map_err(|_| ENOMEM)?;
        for p in (addr..end).step_by(PAGE_SIZE as usize) {
            let mapped = self.walk(p, false).is_some_and(|e| has_frame(e));
            let cached = || {
                let (s, vma) = inner.find(p).unwrap();
                vma.object_page(s, p).is_some_and(|(obj, idx)| obj.has_page(idx))
            };
            v.push((mapped || cached()) as u8);
        }
        Ok(v)
    }

    /// Futex key for `uaddr`: (object, offset) inside a shared mapping, so
    /// that every process mapping the object agrees; None otherwise.
    pub fn shared_futex_key(&self, uaddr: u64) -> Option<(u64, u64)> {
        let inner = self.inner.lock();
        let (s, v) = inner.find(uaddr)?;
        if !v.shared {
            return None;
        }
        let (obj, idx) = v.object_page(s, uaddr)?;
        Some((Arc::as_ptr(obj) as u64, idx * PAGE_SIZE + uaddr % PAGE_SIZE))
    }

    /// Whether a shared mapping of `obj` could be written (F_SEAL_WRITE).
    pub fn maps_writable(&self, obj: &Shmem) -> bool {
        let inner = self.inner.lock();
        inner.vmas.values().any(|v| {
            v.shared && v.maywrite && v.backing.as_ref().is_some_and(|b| core::ptr::eq(Arc::as_ptr(&b.obj), obj))
        })
    }

    /// Unmap pages `[first, last)` of `obj` from every VMA mapping it
    /// (truncate, hole punch). With `only`, just the PTEs pointing at one
    /// of those physical addresses (private copies survive).
    pub fn zap_object(&self, obj: &Shmem, first: u64, last: u64, only: Option<&BTreeSet<u64>>) {
        let inner = self.inner.lock();
        let mut frames = Vec::new();
        for (&s, v) in &inner.vmas {
            let Some(b) = v.backing.as_ref().filter(|b| core::ptr::eq(Arc::as_ptr(&b.obj), obj)) else {
                continue;
            };
            let pages = (v.end - s) / PAGE_SIZE;
            let (lo, hi) = (first.max(b.pgoff), last.min(b.pgoff + pages));
            for idx in lo..hi {
                let va = s + (idx - b.pgoff) * PAGE_SIZE;
                if let Some(e) = self.walk(va, false)
                    && has_frame(e)
                    && only.is_none_or(|set| set.contains(&e.addr().as_u64()))
                {
                    frames.push(PhysFrame::containing_address(e.addr()));
                    e.set_unused();
                }
            }
        }
        drop(inner);
        self.finish_unmap(frames);
    }

    /// `mremap(2)`. Pages move by moving their PTEs; a backed VMA keeps
    /// its object (at the matching offset).
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
        if dontunmap && old_len != new_len {
            return Err(EINVAL);
        }
        let old_end = old.checked_add(old_len).filter(|&e| e <= USER_END).ok_or(EFAULT)?;
        let mut inner = self.inner.lock();
        let (vs, vma) = inner.find(old).filter(|(_, v)| v.end >= old_end).ok_or(EFAULT)?;
        // `vma` from `old` on.
        let vma = vma.tail(vs, old);
        if old_len == 0 {
            // A second mapping of the same shared pages.
            if !vma.shared || vma.backing.is_none() || !may_move || dontunmap {
                return Err(EINVAL);
            }
            let dest = if fixed {
                let new_end = new_addr.checked_add(new_len).filter(|&e| e <= USER_END).ok_or(EINVAL)?;
                if !new_addr.is_multiple_of(PAGE_SIZE) || new_addr == 0 {
                    return Err(EINVAL);
                }
                let frames = self.unmap_locked(&mut inner, new_addr, new_end);
                drop(inner);
                self.finish_unmap(frames);
                inner = self.inner.lock();
                new_addr
            } else {
                inner.find_free(new_len).ok_or(ENOMEM)?
            };
            inner.insert(dest, Vma { end: dest + new_len, uffd: None, ..vma });
            return Ok(dest);
        }
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
            inner.insert(old_end, Vma { end: old + new_len, ..vma.tail(old, old_end) });
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

    /// Move the pages of `[old, old + old_len)` (VMA `vma`, starting at
    /// `old`) to `dest` and give `dest` a VMA of `new_len` bytes. The old
    /// range is unmapped unless `dontunmap` (then it stays mapped and reads
    /// back as zeroes, or refaults from its object); frames past `new_len`
    /// are pushed to `release`.
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
        inner.insert(dest, Vma { end: dest + new_len, uffd: None, ..vma.clone() });
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

    /// Remove every mapping (process exit). The page tables stay until the
    /// address space is dropped: the exiting CPU still runs on them.
    pub fn unmap_all(&self) {
        let mut inner = self.inner.lock();
        let frames = self.unmap_locked(&mut inner, 0, USER_END);
        drop(inner);
        self.finish_unmap(frames);
    }

    /// UFFDIO_REGISTER of `[start, end)` with `ctx`: private anonymous
    /// memory, or writable shmem objects (MINOR needs shmem). Returns
    /// whether the whole range is private anonymous (UFFDIO_MOVE works).
    pub fn uffd_register(&self, ctx: &Arc<UffdCtx>, start: u64, end: u64, mode: u32) -> KResult<bool> {
        let mut inner = self.inner.lock();
        if inner.find(start).is_none() && !inner.overlaps(start, end) {
            return Err(ENOMEM);
        }
        inner.require_mapped(start, end, EINVAL)?;
        let mut all_anon = true;
        let mut a = start;
        while a < end {
            let (_, v) = inner.find(a).unwrap();
            let anon = v.backing.is_none() && !v.shared;
            let shmem = v.backing.as_ref().is_some_and(|b| !b.obj.is_readonly());
            if !(anon || shmem) || (mode & uffd::MODE_MINOR != 0 && !shmem) {
                return Err(EINVAL);
            }
            if v.uffd.as_ref().is_some_and(|r| !Arc::ptr_eq(&r.ctx, ctx)) {
                return Err(EBUSY);
            }
            all_anon &= anon;
            a = v.end;
        }
        inner.split_at(start);
        inner.split_at(end);
        for v in inner.vmas.range_mut(start..end).map(|(_, v)| v) {
            v.uffd = Some(UffdReg { ctx: ctx.clone(), mode });
        }
        inner.remerge(start, end);
        Ok(all_anon)
    }

    /// UFFDIO_UNREGISTER: drop `ctx`'s registrations in `[start, end)`.
    pub fn uffd_unregister(&self, ctx: &Arc<UffdCtx>, start: u64, end: u64) -> KResult<()> {
        let mut inner = self.inner.lock();
        if inner.find(start).is_none() && !inner.overlaps(start, end) {
            return Err(ENOMEM);
        }
        inner.split_at(start);
        inner.split_at(end);
        for v in inner.vmas.range_mut(start..end).map(|(_, v)| v) {
            if v.uffd.as_ref().is_some_and(|r| Arc::ptr_eq(&r.ctx, ctx)) {
                v.uffd = None;
            }
        }
        inner.remerge(start, end);
        Ok(())
    }

    /// The userfaultfd was closed: drop every registration of `ctx`.
    pub fn uffd_release(&self, ctx: &UffdCtx) {
        let mut inner = self.inner.lock();
        let mut any = false;
        for v in inner.vmas.values_mut() {
            if v.uffd.as_ref().is_some_and(|r| core::ptr::eq(Arc::as_ptr(&r.ctx), ctx)) {
                v.uffd = None;
                any = true;
            }
        }
        if any {
            inner.remerge(0, USER_END);
        }
    }

    /// Fill the missing page `va` of a range registered with `ctx`
    /// (UFFDIO_COPY, ZEROPAGE, CONTINUE). ENOENT outside such a range,
    /// EEXIST if the page is already mapped (or, for shmem, already in
    /// the object), EFAULT for CONTINUE without an object page.
    pub fn uffd_fill(&self, ctx: &Arc<UffdCtx>, va: u64, fill: uffd::Fill) -> KResult<()> {
        let owned = match &fill {
            uffd::Fill::Frame(f) => Some(*f),
            _ => None,
        };
        let r = self.uffd_fill_inner(ctx, va, fill);
        if r.is_err()
            && let Some(f) = owned
        {
            frame::free(f);
        }
        r
    }

    fn uffd_fill_inner(&self, ctx: &Arc<UffdCtx>, va: u64, fill: uffd::Fill) -> KResult<()> {
        let inner = self.inner.lock();
        let (start, vma) = inner.find(va).ok_or(ENOENT)?;
        if !vma.uffd.as_ref().is_some_and(|r| Arc::ptr_eq(&r.ctx, ctx)) {
            return Err(ENOENT);
        }
        if self.walk(va, false).is_some_and(|e| has_frame(e)) {
            return Err(EEXIST);
        }
        let e = self.walk(va, true).ok_or(ENOMEM)?;
        let mut flags = pte_flags(vma.prot);
        let frame = match (fill, vma.object_page(start, va)) {
            (uffd::Fill::Continue, None) => return Err(EINVAL),
            (uffd::Fill::Continue, Some((obj, idx))) => {
                if !obj.has_page(idx) {
                    return Err(EFAULT);
                }
                if !vma.shared {
                    flags -= F::WRITABLE;
                }
                obj.fault_page(idx).map_err(|_| EFAULT)?
            }
            (fill, obj) => {
                let owned_here = !matches!(fill, uffd::Fill::Frame(_));
                let f = match fill {
                    uffd::Fill::Frame(f) => f,
                    _ => frame::alloc_zeroed().ok_or(ENOMEM)?,
                };
                match obj {
                    // Shared shmem: the page goes into the object.
                    Some((obj, idx)) if vma.shared => {
                        if let Err(e) = obj.insert_page(idx, f) {
                            // A zero page we allocated here; a copied one
                            // is freed by the caller.
                            if owned_here {
                                frame::free(f);
                            }
                            return Err(e);
                        }
                        frame::get(f);
                    }
                    _ => {}
                }
                f
            }
        };
        if vma.prot == 0 {
            flags = PARKED_FLAGS;
        }
        e.set_frame(frame, flags);
        Ok(())
    }

    /// UFFDIO_MOVE: move the pages of `[src, src + len)` (private
    /// anonymous) to the missing pages at `dst` (registered with `ctx`).
    /// Returns the bytes moved and the error that stopped it, if any.
    pub fn uffd_move(&self, ctx: &Arc<UffdCtx>, dst: u64, src: u64, len: u64, allow_holes: bool) -> (u64, Option<Errno>) {
        let inner = self.inner.lock();
        let mut done = 0;
        let mut err = None;
        let mut release = Vec::new();
        while done < len {
            let (d, s) = (dst + done, src + done);
            let r = (|| {
                let (_, dv) = inner.find(d).ok_or(ENOENT)?;
                let (_, sv) = inner.find(s).ok_or(ENOENT)?;
                if !dv.uffd.as_ref().is_some_and(|r| Arc::ptr_eq(&r.ctx, ctx))
                    || dv.backing.is_some()
                    || dv.shared
                    || sv.backing.is_some()
                    || sv.shared
                {
                    return Err(EINVAL);
                }
                if self.walk(d, false).is_some_and(|e| has_frame(e)) {
                    return Err(EEXIST);
                }
                let Some(se) = self.walk(s, false).filter(|e| e.flags().contains(F::PRESENT)) else {
                    return if allow_holes { Ok(()) } else { Err(ENOENT) };
                };
                let de = self.walk(d, true).ok_or(ENOMEM)?;
                let old = PhysFrame::containing_address(se.addr());
                // A page still shared with a fork child (COW) is copied.
                let f = if frame::refcount(old) == 1 {
                    old
                } else {
                    let copy = copy_frame(old)?;
                    release.push(old);
                    copy
                };
                se.set_unused();
                de.set_frame(f, if dv.prot == 0 { PARKED_FLAGS } else { pte_flags(dv.prot) });
                Ok(())
            })();
            if let Err(e) = r {
                err = Some(e);
                break;
            }
            done += PAGE_SIZE;
        }
        drop(inner);
        self.flush_tlb();
        for f in release {
            frame::free(f);
        }
        (done, err)
    }

    /// Copy-on-write copy of the address space (`fork` without CLONE_VM).
    pub fn fork(&self) -> Option<Arc<AddressSpace>> {
        let child = AddressSpace::new()?;
        child.start_stack.store(self.start_stack.load(Ordering::Relaxed), Ordering::Relaxed);
        *child.arg_range.lock() = *self.arg_range.lock();
        let inner = self.inner.lock();
        {
            let mut ci = child.inner.lock();
            ci.vmas = inner.vmas.clone();
            ci.brk_start = inner.brk_start;
            ci.brk = inner.brk;
            // userfaultfd registrations stay with the parent (no EVENT_FORK).
            if ci.vmas.values().any(|v| v.uffd.is_some()) {
                for v in ci.vmas.values_mut() {
                    v.uffd = None;
                }
                ci.remerge(0, USER_END);
            }
        }
        for b in inner.vmas.values().filter_map(|v| v.backing.as_ref()) {
            b.obj.register(&child.me);
        }
        let mut ok = true;
        for_each_leaf(self.pml4, |va, e| {
            if !ok {
                return;
            }
            let (shared, wipe) = inner.find(va).map_or((false, false), |(_, v)| (v.shared, v.wipeonfork));
            if wipe {
                // The child faults in zero pages.
                return;
            }
            let Some(ce) = child.walk(va, true) else {
                ok = false;
                return;
            };
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

/// A private copy of `src` (COW break).
fn copy_frame(src: PhysFrame) -> KResult<PhysFrame> {
    let new = frame::alloc().ok_or(ENOMEM)?;
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(src.start_address()).as_ptr::<u8>(),
            phys_to_virt(new.start_address()).as_mut_ptr::<u8>(),
            PAGE_SIZE as usize,
        )
    };
    Ok(new)
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
        // The shared page was never touched: both fault it in from the
        // object and get the same frame.
        assert_eq!(phys_of(&child, sh), None);
        child.copy_out(sh, b"0").unwrap();
        assert!(child.walk(sh, false).unwrap().flags().contains(F::WRITABLE));
        let mut b = [0u8; 1];
        mm.copy_in(sh, &mut b).unwrap();
        assert_eq!(&b, b"0");
        assert_eq!(phys_of(&child, sh), phys_of(&mm, sh));
        // A write fault in the child copies; the parent then holds the only
        // reference and gets its page back without a copy.
        assert!(child.handle_fault(a, true, false).is_ok());
        assert_ne!(phys_of(&child, a), Some(p0));
        assert!(mm.handle_fault(a, true, false).is_ok());
        assert_eq!(phys_of(&mm, a), Some(p0));
        // Kernel writes (copy_out) break COW too.
        child.copy_out(a + PAGE_SIZE, b"c").unwrap();
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

#[test_case]
fn shmem_mappings_share_pages_without_leaks() {
    use super::shmem::F_SEAL_WRITE;
    let (_, free_before) = frame::stats();
    {
        let rw = PROT_READ | PROT_WRITE;
        let obj = Shmem::new(2 * PAGE_SIZE);
        obj.set_initial_seals(0);
        obj.write(0, b"file").unwrap();
        let backing = |pgoff| Backing { obj: obj.clone(), pgoff, file: None };
        let mm = AddressSpace::new().unwrap();
        let other = AddressSpace::new().unwrap();
        let a = mm.mmap_object(0, 2 * PAGE_SIZE, rw, true, true, backing(0), false, false).unwrap();
        let b = other.mmap_object(0, PAGE_SIZE, rw, true, true, backing(1), false, false).unwrap();
        let p = mm.mmap_object(0, 2 * PAGE_SIZE, rw, false, true, backing(0), false, false).unwrap();
        // Shared mappings, read()/write() and other processes agree.
        let mut buf = [0u8; 4];
        mm.copy_in(a, &mut buf).unwrap();
        assert_eq!(&buf, b"file");
        mm.copy_out(a + PAGE_SIZE, b"two").unwrap();
        other.copy_in(b, &mut buf[..3]).unwrap();
        assert_eq!(&buf[..3], b"two");
        obj.write(PAGE_SIZE + 3, b"!").unwrap();
        other.copy_in(b + 3, &mut buf[..1]).unwrap();
        assert_eq!(buf[0], b'!');
        assert_eq!(phys_of(&mm, a + PAGE_SIZE), phys_of(&other, b));
        assert_eq!(mm.shared_futex_key(a + PAGE_SIZE + 8), other.shared_futex_key(b + 8));
        assert!(mm.shared_futex_key(a).is_some() && mm.shared_futex_key(p).is_none());
        // Private: a read maps the cached frame read-only, a write copies.
        mm.copy_in(p, &mut buf).unwrap();
        assert_eq!(phys_of(&mm, p), phys_of(&mm, a));
        assert!(!mm.walk(p, false).unwrap().flags().contains(F::WRITABLE));
        assert!(mm.handle_fault(p, true, false).is_ok());
        assert_ne!(phys_of(&mm, p), phys_of(&mm, a));
        mm.copy_out(p, b"PRIV").unwrap();
        assert_eq!(obj.read(0, &mut buf), 4);
        assert_eq!(&buf, b"file");
        // F_SEAL_WRITE is refused while a writable shared mapping exists.
        assert_eq!(obj.add_seals(F_SEAL_WRITE), Err(EBUSY));
        // Shrinking unmaps the pages past the end (SIGBUS on refault) and
        // zeroes the tail of the last one.
        obj.truncate(PAGE_SIZE / 2).unwrap();
        assert_eq!(phys_of(&other, b), None);
        assert_eq!(other.handle_fault(b, false, false), Err(FaultError::Bus));
        assert_eq!(other.copy_in(b, &mut buf), Err(EFAULT));
        mm.copy_in(a + PAGE_SIZE / 2, &mut buf).unwrap();
        assert_eq!(buf, [0; 4]);
        // Shared across fork.
        let child = mm.fork().unwrap();
        child.copy_out(a, b"C").unwrap();
        assert_eq!(obj.read(0, &mut buf[..1]), 1);
        assert_eq!(buf[0], b'C');
        drop(child);
        mm.munmap(a, 2 * PAGE_SIZE).unwrap();
        other.munmap(b, PAGE_SIZE).unwrap();
        assert_eq!(obj.add_seals(F_SEAL_WRITE), Ok(()));
        // Shared anonymous memory: mremap(old_len = 0) maps the same pages
        // again; growing past the object's size gives SIGBUS there.
        let s = mm.mmap(0, PAGE_SIZE, rw, true, false, false).unwrap();
        mm.copy_out(s, b"s").unwrap();
        let s2 = mm.mremap(s, 0, PAGE_SIZE, MREMAP_MAYMOVE, 0).unwrap();
        assert_ne!(s2, s);
        assert_eq!(phys_of(&mm, s), {
            mm.copy_in(s2, &mut buf[..1]).unwrap();
            phys_of(&mm, s2)
        });
        let s3 = mm.mremap(s2, PAGE_SIZE, 2 * PAGE_SIZE, MREMAP_MAYMOVE, 0).unwrap();
        mm.copy_in(s3, &mut buf[..1]).unwrap();
        assert_eq!(buf[0], b's');
        assert_eq!(mm.handle_fault(s3 + PAGE_SIZE, false, false), Err(FaultError::Bus));
        // madvise(DONTNEED) on a shared page keeps its contents.
        mm.madvise(s3, PAGE_SIZE, MADV_DONTNEED).unwrap();
        mm.copy_in(s3, &mut buf[..1]).unwrap();
        assert_eq!(buf[0], b's');
        assert_eq!(mm.mincore(s, 2 * PAGE_SIZE), Err(ENOMEM));
        assert_eq!(mm.mincore(s, PAGE_SIZE), Ok(alloc::vec![1]));
        assert_eq!(mm.msync(s + 1, PAGE_SIZE, MS_SYNC), Err(EINVAL));
        assert_eq!(mm.msync(s, PAGE_SIZE, MS_SYNC | MS_ASYNC), Err(EINVAL));
        assert_eq!(mm.msync(s, PAGE_SIZE, MS_SYNC), Ok(()));
    }
    assert_eq!(frame::stats().1, free_before);
}

#[test_case]
fn shared_mapping_of_readonly_object_cannot_gain_write() {
    let mm = AddressSpace::new().unwrap();
    let obj = Shmem::new(PAGE_SIZE);
    let b = Backing { obj, pgoff: 0, file: None };
    let a = mm.mmap_object(0, PAGE_SIZE, PROT_READ, true, false, b, false, false).unwrap();
    assert_eq!(mm.mprotect(a, PAGE_SIZE, PROT_READ | PROT_WRITE), Err(EACCES));
    assert_eq!(mm.handle_fault(a, true, false), Err(FaultError::Segv));
}
