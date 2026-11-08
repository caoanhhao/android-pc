//! Kernel time keeping.
//!
//! - `ticks`: LAPIC timer interrupts on the BSP (`HZ` per second), used for
//!   timeouts' granularity and statistics.
//! - `monotonic_ns`: nanoseconds since boot from the TSC (calibrated against
//!   the PIT together with the LAPIC timer), never going backwards even if
//!   CPUs' TSCs differ slightly.
//! - `realtime_ns`: wall clock = Limine's boot timestamp + monotonic time.

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Timer interrupt frequency.
pub const HZ: u64 = 100;
pub const NSEC_PER_SEC: u64 = 1_000_000_000;

static TICKS: AtomicU64 = AtomicU64::new(0);
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static LAST_NS: AtomicU64 = AtomicU64::new(0);
/// Wall clock at boot, in seconds since the epoch.
static BOOT_EPOCH: AtomicI64 = AtomicI64::new(0);

/// Called from the timer interrupt on the BSP.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    let ns = monotonic_ns();
    if ns == 0 { ticks() * 1000 / HZ } else { ns / 1_000_000 }
}

#[inline]
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Set by the LAPIC timer calibration.
pub fn set_tsc_hz(hz: u64) {
    TSC_BOOT.store(rdtsc(), Ordering::Relaxed);
    TSC_HZ.store(hz, Ordering::Release);
    log::info!("TSC: {} MHz", hz / 1_000_000);
}

pub fn set_boot_epoch(secs: i64) {
    BOOT_EPOCH.store(secs, Ordering::Relaxed);
}

/// Nanoseconds since the TSC was calibrated (0 before that).
pub fn monotonic_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Acquire);
    if hz == 0 {
        return 0;
    }
    let cycles = rdtsc().saturating_sub(TSC_BOOT.load(Ordering::Relaxed));
    let ns = (cycles as u128 * NSEC_PER_SEC as u128 / hz as u128) as u64;
    // Monotonic across CPUs even with a small TSC skew between them.
    let last = LAST_NS.fetch_max(ns, Ordering::Relaxed);
    ns.max(last)
}

pub fn realtime_ns() -> i128 {
    BOOT_EPOCH.load(Ordering::Relaxed) as i128 * NSEC_PER_SEC as i128 + monotonic_ns() as i128
}

/// Halt until `ms` milliseconds have elapsed. Needs interrupts enabled;
/// only for early boot code (tasks sleep through the scheduler).
#[cfg_attr(not(test), allow(dead_code))]
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

#[test_case]
fn monotonic_clock_advances() {
    let a = monotonic_ns();
    sleep_ms(20);
    let b = monotonic_ns();
    assert!(b >= a + 15_000_000, "{a} -> {b}");
    assert!(b < a + 1_000_000_000, "{a} -> {b}");
}
