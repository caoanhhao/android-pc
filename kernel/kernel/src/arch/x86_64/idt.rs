//! Interrupt Descriptor Table: CPU exceptions and device interrupt vectors.
//!
//! Vector map: 0x00-0x1f exceptions, 0x20 LAPIC timer, 0xe0-0xef masked
//! legacy PIC (spurious only), 0xff LAPIC spurious.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Lazy;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use super::{apic, cpu, pic};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divide_error);
    idt.debug.set_handler_fn(debug);
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.overflow.set_handler_fn(overflow);
    idt.bound_range_exceeded
        .set_handler_fn(bound_range_exceeded);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
    idt.device_not_available
        .set_handler_fn(device_not_available);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(cpu::DOUBLE_FAULT_IST);
    }
    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.stack_segment_fault.set_handler_fn(stack_segment_fault);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault);
    idt.page_fault.set_handler_fn(page_fault);
    idt.x87_floating_point.set_handler_fn(x87_floating_point);
    idt.alignment_check.set_handler_fn(alignment_check);
    idt.machine_check.set_handler_fn(machine_check);
    idt.simd_floating_point.set_handler_fn(simd_floating_point);
    idt.virtualization.set_handler_fn(virtualization);

    idt[apic::TIMER_VECTOR].set_handler_fn(timer_interrupt);
    for v in pic::VECTOR_BASE..pic::VECTOR_BASE + 16 {
        idt[v].set_handler_fn(spurious_interrupt);
    }
    idt[apic::SPURIOUS_VECTOR].set_handler_fn(spurious_interrupt);
    idt
});

/// Load the shared IDT on the executing CPU.
pub fn load() {
    IDT.load();
}

/// Number of `int3` traps taken (used by tests).
pub static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

macro_rules! fatal_exception {
    ($name:ident, $what:literal) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            panic!("exception: {}\n{:#?}", $what, frame);
        }
    };
    ($name:ident, $what:literal, error_code) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, code: u64) {
            panic!(
                "exception: {} (error code {:#x})\n{:#?}",
                $what, code, frame
            );
        }
    };
}

fatal_exception!(divide_error, "#DE divide error");
fatal_exception!(debug, "#DB debug");
fatal_exception!(nmi, "NMI");
fatal_exception!(overflow, "#OF overflow");
fatal_exception!(bound_range_exceeded, "#BR bound range exceeded");
fatal_exception!(invalid_opcode, "#UD invalid opcode");
fatal_exception!(device_not_available, "#NM device not available");
fatal_exception!(x87_floating_point, "#MF x87 floating point");
fatal_exception!(simd_floating_point, "#XM SIMD floating point");
fatal_exception!(virtualization, "#VE virtualization");
fatal_exception!(invalid_tss, "#TS invalid TSS", error_code);
fatal_exception!(segment_not_present, "#NP segment not present", error_code);
fatal_exception!(stack_segment_fault, "#SS stack segment fault", error_code);
fatal_exception!(
    general_protection_fault,
    "#GP general protection",
    error_code
);
fatal_exception!(alignment_check, "#AC alignment check", error_code);

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
    log::trace!("breakpoint at {:#x}", frame.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    // Phase B3 resolves user faults (demand paging, COW) here.
    panic!(
        "exception: #PF page fault at {:?} ({:?})\n{:#?}",
        Cr2::read(),
        code,
        frame
    );
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    panic!("exception: #DF double fault (error code {code:#x})\n{frame:#?}");
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    panic!("exception: #MC machine check\n{frame:#?}");
}

extern "x86-interrupt" fn timer_interrupt(_frame: InterruptStackFrame) {
    // Only the BSP runs the periodic timer for now.
    crate::time::tick();
    apic::eoi();
}

/// Spurious interrupts must not be acknowledged with an EOI.
extern "x86-interrupt" fn spurious_interrupt(_frame: InterruptStackFrame) {}

#[test_case]
fn breakpoint_returns() {
    let before = BREAKPOINTS.load(Ordering::Relaxed);
    x86_64::instructions::interrupts::int3();
    assert_eq!(BREAKPOINTS.load(Ordering::Relaxed), before + 1);
}
