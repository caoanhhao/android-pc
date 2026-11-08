//! Kernel page tables.
//!
//! At boot we build our own PML4 (instead of keeping Limine's) containing:
//! - the higher-half direct map (HHDM) of every memory-map region at the
//!   same offset Limine used, with the largest page size that fits;
//! - the kernel image with per-section permissions (W^X);
//! - MMIO windows added later with `map_mmio` (uncached).
//!
//! The upper half (PML4 entries 256..512) will be shared by every process
//! address space in phase B3.

use core::sync::atomic::{AtomicU64, Ordering};
use limine::memmap::{
    Entry, MEMMAP_ACPI_NVS, MEMMAP_ACPI_RECLAIMABLE, MEMMAP_BOOTLOADER_RECLAIMABLE,
    MEMMAP_EXECUTABLE_AND_MODULES, MEMMAP_FRAMEBUFFER, MEMMAP_MAPPED_RESERVED, MEMMAP_USABLE,
};
use spin::Mutex;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr3, Cr3Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTable, PageTableFlags as F, PhysFrame, Size1GiB, Size2MiB,
    Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use super::PAGE_SIZE;
use super::frame::{self, GlobalFrameAllocator};
use crate::boot;

const SIZE_2M: u64 = 2 << 20;
const SIZE_1G: u64 = 1 << 30;

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);
static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);
static KERNEL_TABLES: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

pub fn set_hhdm_offset(offset: u64) {
    HHDM_OFFSET.store(offset, Ordering::Relaxed);
}

/// Address of physical memory in the direct map.
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    VirtAddr::new(HHDM_OFFSET.load(Ordering::Relaxed) + phys.as_u64())
}

unsafe extern "C" {
    static __kernel_start: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

fn has_1g_pages() -> bool {
    let r = core::arch::x86_64::__cpuid(0x8000_0001);
    r.edx & (1 << 26) != 0
}

/// Map `[phys, phys+len)` at `virt` using 1 GiB / 2 MiB / 4 KiB pages.
unsafe fn map_range(pt: &mut OffsetPageTable, virt: u64, phys: u64, len: u64, flags: F, gib: bool) {
    let alloc = &mut GlobalFrameAllocator;
    let end = phys + len;
    let mut p = phys;
    while p < end {
        let v = virt + (p - phys);
        let left = end - p;
        let fits = |size: u64| p.is_multiple_of(size) && v.is_multiple_of(size) && left >= size;
        unsafe {
            if gib && fits(SIZE_1G) {
                let page = Page::<Size1GiB>::from_start_address(VirtAddr::new(v)).unwrap();
                let fr = PhysFrame::<Size1GiB>::from_start_address(PhysAddr::new(p)).unwrap();
                pt.map_to(page, fr, flags, alloc).expect("map 1G").ignore();
                p += SIZE_1G;
            } else if fits(SIZE_2M) {
                let page = Page::<Size2MiB>::from_start_address(VirtAddr::new(v)).unwrap();
                let fr = PhysFrame::<Size2MiB>::from_start_address(PhysAddr::new(p)).unwrap();
                pt.map_to(page, fr, flags, alloc).expect("map 2M").ignore();
                p += SIZE_2M;
            } else {
                let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(v)).unwrap();
                let fr = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(p)).unwrap();
                pt.map_to(page, fr, flags, alloc).expect("map 4K").ignore();
                p += PAGE_SIZE;
            }
        }
    }
}

/// Build the kernel page tables and switch the BSP to them.
pub fn init(memmap: &[&Entry]) {
    let hhdm = HHDM_OFFSET.load(Ordering::Relaxed);
    let gib = has_1g_pages();
    let pml4_frame = frame::alloc_zeroed().expect("no frame for the PML4");
    let pml4: &'static mut PageTable =
        unsafe { &mut *phys_to_virt(pml4_frame.start_address()).as_mut_ptr() };
    let mut pt = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(hhdm)) };

    // Direct map: coalesce adjacent regions so large pages can be used.
    let data = F::PRESENT | F::WRITABLE | F::NO_EXECUTE | F::GLOBAL;
    let mapped = |t| {
        matches!(
            t,
            MEMMAP_USABLE
                | MEMMAP_BOOTLOADER_RECLAIMABLE
                | MEMMAP_EXECUTABLE_AND_MODULES
                | MEMMAP_ACPI_RECLAIMABLE
                | MEMMAP_ACPI_NVS
                | MEMMAP_FRAMEBUFFER
                | MEMMAP_MAPPED_RESERVED
        )
    };
    let mut run: Option<(u64, u64)> = None;
    let mut direct_bytes = 0;
    for e in memmap.iter().filter(|e| mapped(e.type_)) {
        let (base, end) = (
            e.base & !(PAGE_SIZE - 1),
            (e.base + e.length).next_multiple_of(PAGE_SIZE),
        );
        run = match run {
            Some((s, r_end)) if r_end >= base => Some((s, r_end.max(end))),
            Some((s, r_end)) => {
                unsafe { map_range(&mut pt, hhdm + s, s, r_end - s, data, gib) };
                direct_bytes += r_end - s;
                Some((base, end))
            }
            None => Some((base, end)),
        };
    }
    if let Some((s, r_end)) = run {
        unsafe { map_range(&mut pt, hhdm + s, s, r_end - s, data, gib) };
        direct_bytes += r_end - s;
    }

    // Kernel image, W^X per section.
    let (kphys, kvirt) = boot::kernel_address();
    let sym = |s: &u8| s as *const u8 as u64;
    let (start, text, text_end, ro, ro_end, data_start, end) = unsafe {
        (
            sym(&__kernel_start),
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
            sym(&__data_start),
            sym(&__kernel_end),
        )
    };
    let sections = [
        (start, text, F::PRESENT | F::WRITABLE | F::NO_EXECUTE), // Limine requests
        (text, text_end, F::PRESENT),
        (ro, ro_end, F::PRESENT | F::NO_EXECUTE),
        (data_start, end, F::PRESENT | F::WRITABLE | F::NO_EXECUTE),
    ];
    for (s, e, flags) in sections {
        unsafe {
            map_range(
                &mut pt,
                s,
                s - kvirt + kphys,
                e - s,
                flags | F::GLOBAL,
                false,
            )
        };
    }

    KERNEL_PML4.store(pml4_frame.start_address().as_u64(), Ordering::Release);
    unsafe { activate_kernel_tables() };
    *KERNEL_TABLES.lock() = Some(pt);
    log::info!(
        "paging: kernel PML4 at {:#x}, direct map {} MiB at {:#x}, kernel {:#x}-{:#x}, 1G pages {}",
        pml4_frame.start_address().as_u64(),
        direct_bytes >> 20,
        hhdm,
        start,
        end,
        gib
    );
}

/// Enable the paging features our tables rely on and load the kernel PML4
/// on the executing CPU (BSP after `init`, every AP on entry).
///
/// # Safety
/// The kernel tables must be fully built.
pub unsafe fn activate_kernel_tables() {
    let pml4 = PhysFrame::containing_address(PhysAddr::new(KERNEL_PML4.load(Ordering::Acquire)));
    unsafe {
        Efer::update(|f| f.insert(EferFlags::NO_EXECUTE_ENABLE));
        Cr4::update(|f| f.insert(Cr4Flags::PAGE_GLOBAL));
        Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
        Cr3::write(pml4, Cr3Flags::empty());
    }
}

fn with_tables<R>(f: impl FnOnce(&mut OffsetPageTable<'static>) -> R) -> R {
    f(KERNEL_TABLES
        .lock()
        .as_mut()
        .expect("kernel page tables not initialised"))
}

/// Map a device MMIO window (uncached) into the direct map and return its
/// virtual address. Already-mapped pages (e.g. RAM) are left as they are.
pub fn map_mmio(phys: u64, size: u64) -> VirtAddr {
    let start = phys & !(PAGE_SIZE - 1);
    let end = (phys + size).next_multiple_of(PAGE_SIZE);
    let flags =
        F::PRESENT | F::WRITABLE | F::NO_EXECUTE | F::NO_CACHE | F::WRITE_THROUGH | F::GLOBAL;
    with_tables(|pt| {
        for p in (start..end).step_by(PAGE_SIZE as usize) {
            let virt = phys_to_virt(PhysAddr::new(p));
            if pt.translate_addr(virt).is_none() {
                unsafe { map_range(pt, virt.as_u64(), p, PAGE_SIZE, flags, false) };
                x86_64::instructions::tlb::flush(virt);
            }
        }
    });
    phys_to_virt(PhysAddr::new(phys))
}

/// Map one 4 KiB page in the kernel half (page tables shared by all CPUs).
#[allow(dead_code)]
pub fn map_page(virt: VirtAddr, frame: PhysFrame, flags: F) {
    let page = Page::<Size4KiB>::containing_address(virt);
    with_tables(|pt| unsafe {
        pt.map_to(page, frame, flags, &mut GlobalFrameAllocator)
            .expect("map_page")
            .flush()
    });
}

/// Unmap one 4 KiB page and return the frame it pointed to. Only flushes the
/// local TLB; TLB shootdown IPIs come with the scheduler.
#[allow(dead_code)]
pub fn unmap_page(virt: VirtAddr) -> PhysFrame {
    let page = Page::<Size4KiB>::containing_address(virt);
    with_tables(|pt| {
        let (frame, flush) = pt.unmap(page).expect("unmap_page");
        flush.flush();
        frame
    })
}

#[allow(dead_code)]
pub fn translate(virt: VirtAddr) -> Option<PhysAddr> {
    with_tables(|pt| pt.translate_addr(virt))
}

#[test_case]
fn map_write_unmap() {
    let virt = VirtAddr::new(0xffff_e000_0000_0000);
    let f = frame::alloc_zeroed().unwrap();
    map_page(virt, f, F::PRESENT | F::WRITABLE | F::NO_EXECUTE);
    assert_eq!(translate(virt), Some(f.start_address()));
    unsafe {
        virt.as_mut_ptr::<u64>()
            .write_volatile(0xdead_beef_cafe_f00d)
    };
    let via_hhdm = unsafe {
        phys_to_virt(f.start_address())
            .as_ptr::<u64>()
            .read_volatile()
    };
    assert_eq!(via_hhdm, 0xdead_beef_cafe_f00d);
    assert_eq!(unmap_page(virt), f);
    assert_eq!(translate(virt), None);
    frame::free(f);
}

#[test_case]
fn kernel_text_is_not_writable() {
    use x86_64::structures::paging::mapper::TranslateResult;
    let text = phys_to_virt as *const () as u64;
    let flags = with_tables(|pt| match pt.translate(VirtAddr::new(text)) {
        TranslateResult::Mapped { flags, .. } => flags,
        _ => panic!("kernel text not mapped"),
    });
    assert!(!flags.contains(F::WRITABLE));
    assert!(!flags.contains(F::NO_EXECUTE));
}
