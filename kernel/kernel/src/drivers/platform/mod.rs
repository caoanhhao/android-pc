//! Platform devices of a PC (E9): what makes Android on real hardware
//! behave like a proper machine.
//!
//! - [`rtc`]: the CMOS RTC (the wall clock at boot, /dev/rtc0);
//! - [`button`]: the ACPI fixed-feature power button as an input device;
//! - [`cpu`]: CPU temperatures (x86_pkg_temp thermal zone, coretemp
//!   hwmon), HWP and cpufreq's sysfs, the hybrid core topology;
//! - [`power_supply`]: the AC adapter, online.
//!
//! Power off and restart (ACPI S5, the reset register) live in
//! `arch::x86_64::{acpi, mod}`, the MWAIT idle loop in
//! `arch::x86_64::idle`.

pub mod button;
pub mod cpu;
pub mod power_supply;
pub mod rtc;

use core::sync::atomic::{AtomicBool, Ordering};

static PENDING: AtomicBool = AtomicBool::new(false);

/// Before the VFS (after ACPI, the TSC and the APs): the RTC's time
/// (UTC epoch seconds) for the wall clock, the CPU features.
pub fn init_early() -> Option<i64> {
    cpu::init();
    rtc::probe()
}

/// Devices and sysfs (from `drivers::init`).
pub fn init() {
    rtc::init();
    button::init();
    cpu::init_sysfs();
    power_supply::init();
}

/// Timer tick on every CPU.
pub fn tick() {
    cpu::tick();
    if crate::arch::x86_64::cpu::index_fast() == 0 {
        rtc::tick();
        if button::tick() {
            PENDING.store(true, Ordering::Relaxed);
        }
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// Task context (the idle loop): report power button presses.
pub fn softirq() {
    PENDING.store(false, Ordering::Relaxed);
    button::softirq();
}
