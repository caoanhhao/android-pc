//! android-pc kernel: a new x86_64 kernel in Rust that implements the Linux
//! syscall ABI plus the Android-specific drivers, so unmodified AOSP userspace
//! can run on it. See `docs/kernel-architecture.md`.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

#[macro_use]
mod drivers;

mod arch;
mod boot;
mod fs;
mod logger;
mod mm;
mod sched;
mod syscall;
mod testing;
mod time;

use core::panic::PanicInfo;

/// Kernel entry point, jumped to by Limine on the bootstrap processor (BSP)
/// with interrupts disabled, a valid stack and Limine's page tables.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    drivers::serial::init();
    logger::init();
    log::info!("android-pc kernel {} booting", env!("CARGO_PKG_VERSION"));

    boot::init();
    arch::init_bsp_early();
    mm::init();
    arch::init_bsp_late();
    arch::smp::start_aps();

    #[cfg(test)]
    test_main();

    log::info!("boot complete; {} CPU(s) online", arch::smp::online_cpus());
    for second in 1..=3 {
        time::sleep_ms(1000);
        log::info!("timer: {} ticks after {second}s", time::ticks());
    }
    log::info!("idle");
    arch::idle_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    arch::interrupts_disable();
    // The panic may have happened while the serial port was locked.
    unsafe { drivers::serial::force_unlock() };
    kprintln!("\nKERNEL PANIC on cpu{}: {}", arch::cpu_index(), info);
    #[cfg(test)]
    {
        kprintln!("[failed]");
        testing::exit_qemu(testing::QemuExitCode::Failed);
    }
    #[allow(unreachable_code)]
    arch::halt_forever()
}
