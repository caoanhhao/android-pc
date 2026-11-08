//! x86_64 platform: descriptor tables, interrupts, Local APIC, SMP.

pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod pic;
pub mod pit;
pub mod smp;

use x86_64::instructions::{hlt, interrupts};

/// BSP setup that does not need memory management: GDT/TSS, per-CPU data
/// and the IDT, so that faults during `mm::init` are reported.
pub fn init_bsp_early() {
    cpu::init_bsp();
    idt::load();
}

/// BSP setup that needs the heap and page tables: interrupt controllers and
/// the periodic timer. Enables interrupts.
pub fn init_bsp_late() {
    pic::disable();
    apic::init_bsp();
    interrupts::enable();
}

/// Index of the executing CPU (0 = BSP), or 0 before per-CPU data exists.
pub fn cpu_index() -> u32 {
    cpu::current().map_or(0, |c| c.index)
}

pub fn interrupts_disable() {
    interrupts::disable();
}

/// Enable interrupts and halt until the next one arrives.
pub fn wait_for_interrupt() {
    interrupts::enable_and_hlt();
}

/// Idle forever, servicing interrupts.
pub fn idle_loop() -> ! {
    loop {
        interrupts::enable_and_hlt();
    }
}

/// Stop this CPU for good (interrupts off). NMIs can still wake it, hence the loop.
pub fn halt_forever() -> ! {
    interrupts::disable();
    loop {
        hlt();
    }
}
