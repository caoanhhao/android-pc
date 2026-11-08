//! Interrupt Descriptor Table.
//!
//! Vector map: 0x00-0x1f exceptions, 0x20 LAPIC timer, 0xe0-0xef masked
//! legacy PIC (spurious only), 0xf0 TLB shootdown IPI, 0xf1 reschedule IPI,
//! 0xff LAPIC spurious.
//!
//! Everything that can be raised from user mode (or must be able to switch
//! tasks) goes through the assembly stubs in `entry.rs`, which build a
//! `TrapFrame` and call `trap::trap_handler`. NMI, #DF and #MC stay on
//! `x86-interrupt` handlers and are fatal.

use spin::Lazy;
use x86_64::PrivilegeLevel;
use x86_64::VirtAddr;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

use super::{apic, cpu, entry, pic};

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(cpu::DOUBLE_FAULT_IST);
    }
    idt.machine_check.set_handler_fn(machine_check);
    idt.virtualization.set_handler_fn(virtualization);

    for (vector, addr) in entry::trap_stubs() {
        let addr = VirtAddr::new(addr);
        let opts = unsafe {
            match vector {
                0 => idt.divide_error.set_handler_addr(addr),
                1 => idt.debug.set_handler_addr(addr),
                3 => idt.breakpoint.set_handler_addr(addr),
                4 => idt.overflow.set_handler_addr(addr),
                5 => idt.bound_range_exceeded.set_handler_addr(addr),
                6 => idt.invalid_opcode.set_handler_addr(addr),
                7 => idt.device_not_available.set_handler_addr(addr),
                10 => idt.invalid_tss.set_handler_addr(addr),
                11 => idt.segment_not_present.set_handler_addr(addr),
                12 => idt.stack_segment_fault.set_handler_addr(addr),
                13 => idt.general_protection_fault.set_handler_addr(addr),
                14 => idt.page_fault.set_handler_addr(addr),
                16 => idt.x87_floating_point.set_handler_addr(addr),
                17 => idt.alignment_check.set_handler_addr(addr),
                19 => idt.simd_floating_point.set_handler_addr(addr),
                v => idt[v].set_handler_addr(addr),
            }
        };
        // int3 (and into) are allowed from ring 3.
        if vector == 3 || vector == 4 {
            opts.set_privilege_level(PrivilegeLevel::Ring3);
        }
    }
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

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    panic!("exception: NMI\n{frame:#?}");
}

extern "x86-interrupt" fn virtualization(frame: InterruptStackFrame) {
    panic!("exception: #VE virtualization\n{frame:#?}");
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    panic!("exception: #DF double fault (error code {code:#x})\n{frame:#?}");
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    panic!("exception: #MC machine check\n{frame:#?}");
}

/// Spurious interrupts must not be acknowledged with an EOI.
extern "x86-interrupt" fn spurious_interrupt(_frame: InterruptStackFrame) {}
