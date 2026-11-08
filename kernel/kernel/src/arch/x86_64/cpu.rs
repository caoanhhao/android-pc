//! Per-CPU data, reached through the GS base register, and per-CPU feature
//! setup (FPU/SSE, `syscall` MSRs).
//!
//! In the kernel GS base points at the CPU's `PerCpu`; while user code runs
//! it holds the user's GS base and `swapgs` exchanges the two on every
//! kernel entry/exit from ring 3 (see `entry.rs`).

use alloc::alloc::{Layout, alloc};
use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Efer, EferFlags, GsBase, Msr};
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::gdt;

/// IST slot used for the double fault handler (a known-good stack).
pub const DOUBLE_FAULT_IST: u16 = 0;
pub const IST_STACK_SIZE: usize = 16 * 1024;
pub const AP_STACK_SIZE: usize = 64 * 1024;
pub const MAX_CPUS: usize = 64;

/// `gs:` offsets used by the assembly entry code.
pub const PERCPU_USER_RSP: usize = 8;
pub const PERCPU_KERNEL_RSP: usize = 16;

#[repr(C)]
pub struct PerCpu {
    /// Points to itself, so `gs:[0]` yields the structure's address.
    self_ptr: *const PerCpu,
    /// Scratch slot for the user RSP during `syscall` entry.
    user_rsp: u64,
    /// Top of the running task's kernel stack (`syscall` entry stack).
    kernel_rsp: u64,
    /// Dense CPU index, 0 = BSP.
    pub index: u32,
    pub lapic_id: u32,
    /// Initial stack of an AP (unused on the BSP, which keeps Limine's).
    pub boot_stack_top: u64,
    pub double_fault_stack_top: u64,
    gdt: GlobalDescriptorTable,
    tss: TaskStateSegment,
}

const _: () = {
    assert!(core::mem::offset_of!(PerCpu, user_rsp) == PERCPU_USER_RSP);
    assert!(core::mem::offset_of!(PerCpu, kernel_rsp) == PERCPU_KERNEL_RSP);
};

/// LAPIC id of every CPU by dense index (for IPIs).
pub static LAPIC_IDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];

impl PerCpu {
    pub const fn new(index: u32, lapic_id: u32) -> Self {
        Self {
            self_ptr: core::ptr::null(),
            user_rsp: 0,
            kernel_rsp: 0,
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
            assert!((pc.index as usize) < MAX_CPUS, "too many CPUs");
            LAPIC_IDS[pc.index as usize].store(pc.lapic_id, Ordering::Relaxed);
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

/// Index of the executing CPU; only valid after `install` (one `gs:` load).
#[inline]
pub fn index_fast() -> usize {
    let pc: *const PerCpu;
    unsafe { core::arch::asm!("mov {}, gs:[0]", out(reg) pc, options(nostack, readonly, preserves_flags)) };
    unsafe { (*pc).index as usize }
}

/// Set the stack the CPU switches to when entering the kernel from ring 3
/// (TSS.RSP0 for interrupts, `gs:[16]` for `syscall`).
pub fn set_kernel_stack(top: u64) {
    unsafe {
        let pc: *mut PerCpu;
        core::arch::asm!("mov {}, gs:[0]", out(reg) pc, options(nostack, readonly, preserves_flags));
        (*pc).kernel_rsp = top;
        (*pc).tss.privilege_stack_table[0] = VirtAddr::new(top);
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

const MSR_STAR: u32 = 0xc000_0081;
const MSR_LSTAR: u32 = 0xc000_0082;
const MSR_SFMASK: u32 = 0xc000_0084;
/// RFLAGS bits cleared on `syscall`: TF, IF, DF, AC, NT.
const SYSCALL_RFLAGS_MASK: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 18) | (1 << 14);

/// True when user code may use RDFSBASE/WRFSBASE (CR4.FSGSBASE is set).
pub fn has_fsgsbase() -> bool {
    core::arch::x86_64::__cpuid_count(7, 0).ebx & 1 != 0
}

/// Per-CPU setup for running user code: SSE/FXSR for user programs (the
/// kernel itself is soft-float), FSGSBASE, and the `syscall` MSRs.
///
/// With FSGSBASE user code can change its FS/GS bases without a syscall,
/// so the live values are always read back from the MSRs (on context
/// switch, arch_prctl, clone), never trusted from the task struct.
pub fn init_user_support() {
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            f.insert(Cr0Flags::MONITOR_COPROCESSOR);
        });
        Cr4::update(|f| {
            f.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
            if has_fsgsbase() {
                f.insert(Cr4Flags::FSGSBASE);
            }
        });
        Efer::update(|f| f.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
        // STAR[47:32]: kernel CS (SS = +8); STAR[63:48]: base for sysret
        // (SS = +8 = USER_DS, CS = +16 = USER_CS, both RPL 3).
        let star = ((gdt::KERNEL_DS.0 as u64 | 3) << 48) | ((gdt::KERNEL_CS.0 as u64) << 32);
        Msr::new(MSR_STAR).write(star);
        Msr::new(MSR_LSTAR).write(super::entry::syscall_entry_addr());
        Msr::new(MSR_SFMASK).write(SYSCALL_RFLAGS_MASK);
    }
}
