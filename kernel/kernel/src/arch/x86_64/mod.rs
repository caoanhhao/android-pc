//! x86_64 platform: descriptor tables, interrupts, Local APIC, SMP.

pub mod acpi;
pub mod apic;
pub mod cpu;
pub mod entry;
pub mod fpu;
pub mod gdt;
pub mod idle;
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
    idle::init();
    interrupts::enable();
}

/// Power the machine off: ACPI S5 from the tables, then QEMU's PM1a on
/// q35 and PIIX4, falling back to the `isa-debug-exit` device, then
/// halting.
pub fn power_off() -> ! {
    use x86_64::instructions::port::Port;
    log::info!("power off");
    interrupts::disable();
    acpi::power_off();
    spin_ms(500);
    log::warn!("power off: ACPI S5 did not take; trying QEMU's PM ports");
    unsafe {
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xb004).write(0x2000);
        Port::<u32>::new(0xf4).write(0x10);
    }
    spin_ms(100);
    log::error!("power off failed: halted (hold the power button)");
    halt_forever()
}

/// Reset the machine through the FADT reset register, then port 0xcf9
/// (full reset), the keyboard controller, and a triple fault.
pub fn reboot() -> ! {
    use x86_64::instructions::port::Port;
    log::info!("restarting");
    interrupts::disable();
    acpi::reset();
    spin_ms(100);
    log::warn!("restart: trying port 0xcf9");
    unsafe {
        let mut cf9 = Port::<u8>::new(0xcf9);
        let v = cf9.read() & !0x0e;
        cf9.write(v | 0x02);
        spin_ms(1);
        cf9.write(v | 0x0e);
    }
    spin_ms(100);
    log::warn!("restart: trying the keyboard controller");
    unsafe {
        let mut status = Port::<u8>::new(0x64);
        for _ in 0..10_000 {
            if status.read() & 2 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        status.write(0xfe);
    }
    spin_ms(100);
    log::warn!("restart: triple fault");
    unsafe {
        let idt = x86_64::structures::DescriptorTablePointer { limit: 0, base: x86_64::VirtAddr::zero() };
        x86_64::instructions::tables::lidt(&idt);
        core::arch::asm!("int3");
    }
    halt_forever()
}

/// Busy-wait `ms` milliseconds on the TSC (interrupts may be off).
fn spin_ms(ms: u64) {
    let start = crate::time::monotonic_ns();
    while crate::time::monotonic_ns() - start < ms * 1_000_000 {
        core::hint::spin_loop();
    }
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
