//! Per-CPU data, reached through the GS base register.
//!
//! In phase B1 the user GS base will be swapped in with `swapgs` on kernel
//! entry/exit; the kernel-side base always points at the CPU's `PerCpu`.

use alloc::alloc::{Layout, alloc};
use x86_64::VirtAddr;
use x86_64::registers::model_specific::GsBase;
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::gdt;

/// IST slot used for the double fault handler (a known-good stack).
pub const DOUBLE_FAULT_IST: u16 = 0;
pub const IST_STACK_SIZE: usize = 16 * 1024;
pub const AP_STACK_SIZE: usize = 64 * 1024;

#[repr(C)]
pub struct PerCpu {
    /// Points to itself, so `gs:[0]` yields the structure's address.
    self_ptr: *const PerCpu,
    /// Dense CPU index, 0 = BSP.
    pub index: u32,
    pub lapic_id: u32,
    /// Initial stack of an AP (unused on the BSP, which keeps Limine's).
    pub boot_stack_top: u64,
    pub double_fault_stack_top: u64,
    gdt: GlobalDescriptorTable,
    tss: TaskStateSegment,
}

impl PerCpu {
    pub const fn new(index: u32, lapic_id: u32) -> Self {
        Self {
            self_ptr: core::ptr::null(),
            index,
            lapic_id,
            boot_stack_top: 0,
            double_fault_stack_top: 0,
            gdt: GlobalDescriptorTable::new(),
            tss: TaskStateSegment::new(),
        }
    }

    /// Make `this` the executing CPU's per-CPU area: loads its GDT and TSS
    /// and points GS base at it.
    ///
    /// # Safety
    /// Once per CPU; `this` must not be used mutably afterwards.
    pub unsafe fn install(this: *mut PerCpu) {
        unsafe {
            let pc = &mut *this;
            pc.self_ptr = this;
            pc.tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] =
                VirtAddr::new(pc.double_fault_stack_top);
            gdt::load(&mut (*this).gdt, &(*this).tss);
            // After loading segment registers: writing GS would clear the base.
            GsBase::write(VirtAddr::from_ptr(this));
        }
    }
}

/// The executing CPU's data, or `None` before `install`.
pub fn current() -> Option<&'static PerCpu> {
    let base = GsBase::read();
    if base.is_null() {
        None
    } else {
        Some(unsafe { &*base.as_ptr() })
    }
}

#[repr(C, align(16))]
struct Stack<const N: usize>([u8; N]);

static mut BSP_PERCPU: PerCpu = PerCpu::new(0, 0);
static mut BSP_DOUBLE_FAULT_STACK: Stack<IST_STACK_SIZE> = Stack([0; IST_STACK_SIZE]);

/// Set up the BSP's per-CPU area from static storage (no heap yet).
pub fn init_bsp() {
    unsafe {
        let pc = &raw mut BSP_PERCPU;
        (*pc).double_fault_stack_top =
            (&raw mut BSP_DOUBLE_FAULT_STACK) as u64 + IST_STACK_SIZE as u64;
        PerCpu::install(pc);
    }
}

/// Allocate a kernel stack from the heap; returns its (16-byte aligned) top.
pub fn alloc_stack(size: usize) -> u64 {
    let layout = Layout::from_size_align(size, 16).unwrap();
    let base = unsafe { alloc(layout) };
    assert!(!base.is_null(), "out of memory allocating a stack");
    base as u64 + size as u64
}
