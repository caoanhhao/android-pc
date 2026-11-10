//! x86_64 platform: descriptor tables, interrupts, Local APIC, SMP.

pub mod acpi;
pub mod apic;
pub mod cpu;
pub mod entry;
pub mod fpu;
pub mod gdt;
pub mod idt;
pub mod pic;
pub mod pit;
pub mod smp;
pub mod tlb;
pub mod trap;

use x86_64::instructions::{hlt, interrupts};

/// BSP setup that does not need memory management: GDT/TSS, per-CPU data
/// and the IDT, so that faults during `mm::init` are reported.
pub fn init_bsp_early() {
    cpu::init_bsp();
    idt::load();
    cpu::init_user_support();
}

/// BSP setup that needs the heap and page tables: interrupt controllers and
/// the periodic timer. Enables interrupts.
pub fn init_bsp_late() {
    pic::disable();
    acpi::init();
    apic::init_bsp();
    interrupts::enable();
}

/// Power the machine off: ACPI S5 from the tables, then QEMU's PM1a on
/// q35 and PIIX4, falling back to the `isa-debug-exit` device, then
/// halting.
pub fn power_off() -> ! {
    use x86_64::instructions::port::Port;
    interrupts::disable();
    acpi::power_off();
    unsafe {
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xb004).write(0x2000);
        Port::<u32>::new(0xf4).write(0x10);
    }
    halt_forever()
}

/// Reset the machine through the FADT reset register, then port 0xcf9 /
/// the keyboard controller.
pub fn reboot() -> ! {
    use x86_64::instructions::port::Port;
    interrupts::disable();
    acpi::reset();
    unsafe {
        Port::<u8>::new(0xcf9).write(0x06);
        Port::<u8>::new(0x64).write(0xfe);
    }
    halt_forever()
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

/// Stop this CPU for good (interrupts off). NMIs can still wake it, hence the loop.
pub fn halt_forever() -> ! {
    interrupts::disable();
    loop {
        hlt();
    }
}
