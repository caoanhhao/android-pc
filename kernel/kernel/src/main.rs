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
mod block;
mod boot;
mod bpf;
mod errno;
mod fs;
mod logger;
mod mm;
mod net;
mod process;
mod random;
mod sched;
mod security;
mod sync;
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

    if let Some(t) = boot::boot_time() {
        time::set_boot_epoch(t);
    }

    #[cfg(test)]
    test_main();

    log::info!("boot complete; {} CPU(s) online", arch::smp::online_cpus());
    fs::init(boot::initramfs());
    block::init();
    drivers::init();
    start_init(boot::cmdline());
    sched::idle()
}

/// Parse the command line and start init. Recognised words: `init=PATH`,
/// `strace` (log every syscall), `strace=COMM` (only processes whose
/// name starts with COMM); words after `--` become init's arguments.
fn start_init(cmdline: &str) {
    let mut init = "/init";
    let mut args = alloc::vec::Vec::new();
    let mut words = cmdline.split_ascii_whitespace();
    for w in words.by_ref() {
        match w {
            "--" => break,
            "strace" => syscall::TRACE.store(true, core::sync::atomic::Ordering::Relaxed),
            _ => {
                if let Some(c) = w.strip_prefix("strace=") {
                    let mut b = [0u8; 16];
                    let n = c.len().min(15);
                    b[..n].copy_from_slice(&c.as_bytes()[..n]);
                    syscall::TRACE_COMM.call_once(|| b);
                    syscall::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
                }
                if let Some(p) = w.strip_prefix("init=") {
                    init = p;
                }
            }
        }
    }
    args.extend(words.map(|w| w.as_bytes()));
    process::start_init(init.as_bytes(), &args);
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
