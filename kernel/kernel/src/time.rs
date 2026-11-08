//! Kernel time keeping, driven by the BSP's Local APIC timer.

use core::sync::atomic::{AtomicU64, Ordering};

/// Timer interrupt frequency.
pub const HZ: u64 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called from the timer interrupt on the BSP.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    ticks() * 1000 / HZ
}

/// Halt until `ms` milliseconds have elapsed. Needs interrupts enabled.
pub fn sleep_ms(ms: u64) {
    let target = ticks() + ms.div_ceil(1000 / HZ);
    while ticks() < target {
        crate::arch::wait_for_interrupt();
    }
}

#[test_case]
fn timer_ticks_advance() {
    let start = ticks();
    sleep_ms(50);
    assert!(ticks() >= start + 5);
}
