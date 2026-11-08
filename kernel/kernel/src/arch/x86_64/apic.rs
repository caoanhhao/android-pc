//! Local APIC (x2APIC via MSRs when Limine enabled it, else xAPIC via MMIO)
//! and its timer.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::registers::model_specific::Msr;

use super::pit;
use crate::time::HZ;

pub const TIMER_VECTOR: u8 = 0x20;
pub const SPURIOUS_VECTOR: u8 = 0xff;

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const X2APIC_MSR_BASE: u32 = 0x800;

// Register offsets (xAPIC MMIO; x2APIC MSR = 0x800 + offset / 16).
const REG_ID: u32 = 0x20;
const REG_VERSION: u32 = 0x30;
const REG_TPR: u32 = 0x80;
const REG_EOI: u32 = 0xb0;
const REG_SVR: u32 = 0xf0;
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_LINT0: u32 = 0x350;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3e0;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
const TIMER_DIVIDE_BY_16: u32 = 0b0011;

static X2APIC: AtomicBool = AtomicBool::new(false);
static MMIO_BASE: AtomicU64 = AtomicU64::new(0);
/// LAPIC timer counts (divide by 16) per timer tick, calibrated on the BSP.
static TIMER_COUNT: AtomicU32 = AtomicU32::new(0);

fn read(reg: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        unsafe { Msr::new(X2APIC_MSR_BASE + reg / 16).read() as u32 }
    } else {
        let base = MMIO_BASE.load(Ordering::Relaxed);
        unsafe { core::ptr::read_volatile((base + reg as u64) as *const u32) }
    }
}

fn write(reg: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        unsafe { Msr::new(X2APIC_MSR_BASE + reg / 16).write(value as u64) }
    } else {
        let base = MMIO_BASE.load(Ordering::Relaxed);
        unsafe { core::ptr::write_volatile((base + reg as u64) as *mut u32, value) }
    }
}

pub fn is_x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

/// Discover the APIC mode, enable the BSP's LAPIC, calibrate and start the
/// periodic timer at `time::HZ`.
pub fn init_bsp() {
    let base = unsafe { Msr::new(IA32_APIC_BASE).read() };
    let x2apic = base & APIC_BASE_X2APIC != 0;
    X2APIC.store(x2apic, Ordering::Relaxed);
    if !x2apic {
        let phys = base & 0x000f_ffff_ffff_f000;
        let virt = crate::mm::paging::map_mmio(phys, 4096);
        MMIO_BASE.store(virt.as_u64(), Ordering::Relaxed);
    }
    init_cpu();
    log::info!(
        "LAPIC: id {} version {:#x}, {} mode",
        id(),
        read(REG_VERSION) & 0xff,
        if x2apic { "x2APIC" } else { "xAPIC (MMIO)" }
    );

    // Count LAPIC timer decrements (and TSC cycles) during 10 ms of PIT time.
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED);
    let mut tsc_start = 0;
    let (elapsed, tsc_end) = pit::measure(
        10,
        || {
            write(REG_TIMER_INITIAL, u32::MAX);
            tsc_start = crate::time::rdtsc();
        },
        || (u32::MAX - read(REG_TIMER_CURRENT), crate::time::rdtsc()),
    );
    write(REG_TIMER_INITIAL, 0);
    crate::time::set_tsc_hz((tsc_end - tsc_start) * 100);
    let per_tick = (elapsed as u64 * 100 / HZ) as u32;
    TIMER_COUNT.store(per_tick, Ordering::Relaxed);
    log::info!(
        "LAPIC timer: {} kHz bus/16, {HZ} Hz tick = {per_tick} counts",
        elapsed as u64 / 10
    );
    start_timer();
}

/// Software-enable the executing CPU's LAPIC with all local interrupts masked.
pub fn init_cpu() {
    unsafe {
        let mut msr = Msr::new(IA32_APIC_BASE);
        let v = msr.read();
        if v & APIC_BASE_ENABLE == 0 {
            msr.write(v | APIC_BASE_ENABLE);
        }
    }
    write(REG_TPR, 0);
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_ERROR, LVT_MASKED);
    write(REG_SVR, SVR_ENABLE | SPURIOUS_VECTOR as u32);
    eoi();
}

/// Start the periodic timer on the executing CPU (calibration must be done).
pub fn start_timer() {
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, TIMER_VECTOR as u32 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_INITIAL, TIMER_COUNT.load(Ordering::Relaxed));
}

pub fn id() -> u32 {
    let raw = read(REG_ID);
    if is_x2apic() { raw } else { raw >> 24 }
}

pub fn eoi() {
    write(REG_EOI, 0);
}

const ICR_ASSERT: u32 = 1 << 14;
const ICR_PENDING: u32 = 1 << 12;

/// Send a fixed-delivery IPI with `vector` to the CPU with `lapic_id`.
pub fn send_ipi(lapic_id: u32, vector: u8) {
    if is_x2apic() {
        let icr = ((lapic_id as u64) << 32) | ICR_ASSERT as u64 | vector as u64;
        unsafe { Msr::new(X2APIC_MSR_BASE + REG_ICR_LOW / 16).write(icr) };
    } else {
        x86_64::instructions::interrupts::without_interrupts(|| {
            while read(REG_ICR_LOW) & ICR_PENDING != 0 {
                core::hint::spin_loop();
            }
            write(REG_ICR_HIGH, lapic_id << 24);
            write(REG_ICR_LOW, ICR_ASSERT | vector as u32);
        });
    }
}
