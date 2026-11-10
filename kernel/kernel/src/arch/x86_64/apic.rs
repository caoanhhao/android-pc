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

    // Count LAPIC timer decrements and TSC cycles against a reference
    // clock: the HPET or ACPI PM timer (PCs whose 8254 is gated off),
    // else 10 ms of PIT time.
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED);
    let pit = || {
        let mut tsc_start = 0;
        let (elapsed, tsc_end) = pit::measure(
            10,
            || {
                write(REG_TIMER_INITIAL, u32::MAX);
                tsc_start = crate::time::rdtsc();
            },
            || (u32::MAX - read(REG_TIMER_CURRENT), crate::time::rdtsc()),
        )?;
        Some(("PIT", elapsed as u64 * 100, (tsc_end - tsc_start) * 100))
    };
    // Last resort: what CPUID states (the LAPIC timer counts the core
    // crystal clock on Intel CPUs that report it).
    let stated = || {
        let crystal = cpuid_crystal_hz()?;
        Some(("CPUID crystal clock", crystal / 16, cpuid_tsc_hz()?))
    };
    let Some((source, lapic_hz, mut tsc_hz)) = calibrate_acpi(20).or_else(pit).or_else(stated) else {
        panic!("no clock to calibrate the timers against (no HPET, ACPI PM timer, PIT or CPUID crystal frequency)");
    };
    write(REG_TIMER_INITIAL, 0);
    // The TSC frequency the CPU states (Intel leaf 0x15/0x16) is exact;
    // prefer it when the measurement is off by more than 2%.
    if let Some(stated) = cpuid_tsc_hz()
        && tsc_hz.abs_diff(stated) > stated / 50
    {
        log::warn!("TSC: measured {} kHz against the {source}, CPUID says {} kHz; using CPUID", tsc_hz / 1000, stated / 1000);
        tsc_hz = stated;
    }
    crate::time::set_tsc_hz(tsc_hz);
    let per_tick = (lapic_hz / HZ).clamp(1, u32::MAX as u64) as u32;
    TIMER_COUNT.store(per_tick, Ordering::Relaxed);
    log::info!(
        "LAPIC timer: {} kHz bus/16 (calibrated against the {source}), {HZ} Hz tick = {per_tick} counts",
        lapic_hz / 1000
    );
    start_timer();
}

/// Measure the LAPIC timer (divide by 16) and TSC rates over `ms`
/// milliseconds of HPET or ACPI PM timer time: (source, LAPIC Hz, TSC Hz).
fn calibrate_acpi(ms: u64) -> Option<(&'static str, u64, u64)> {
    // A reference: (name, read the counter, counter ticks to ns).
    let (name, now, to_ns): (&str, &dyn Fn() -> u64, &dyn Fn(u64, u64) -> u64);
    let hpet = super::acpi::hpet();
    let pm = super::acpi::pm_timer();
    let hpet_now;
    let hpet_ns;
    let pm_now;
    let pm_ns;
    if let Some((base, period_fs)) = hpet {
        hpet_now = move || unsafe { core::ptr::read_volatile((base + 0xf0) as *const u64) };
        hpet_ns = move |a: u64, b: u64| (b.wrapping_sub(a) as u128 * period_fs as u128 / 1_000_000) as u64;
        (name, now, to_ns) = ("HPET", &hpet_now, &hpet_ns);
    } else if let Some((_, mask)) = pm {
        pm_now = || super::acpi::pm_timer().map_or(0, |t| t.0 as u64);
        pm_ns = move |a: u64, b: u64| (b.wrapping_sub(a) & mask as u64) * 1_000_000_000 / super::acpi::PM_TIMER_HZ;
        (name, now, to_ns) = ("ACPI PM timer", &pm_now, &pm_ns);
    } else {
        return None;
    }
    let t0 = now();
    write(REG_TIMER_INITIAL, u32::MAX);
    let tsc0 = crate::time::rdtsc();
    let target = ms * 1_000_000;
    loop {
        let t = now();
        if to_ns(t0, t) >= target {
            break;
        }
        if crate::time::rdtsc().wrapping_sub(tsc0) > 1 << 34 {
            log::warn!("{name}: does not count, not used for calibration");
            return None;
        }
        core::hint::spin_loop();
    }
    let lapic = u32::MAX - read(REG_TIMER_CURRENT);
    let tsc1 = crate::time::rdtsc();
    let ns = to_ns(t0, now()).max(1);
    let per_s = |n: u64| (n as u128 * 1_000_000_000 / ns as u128) as u64;
    Some((name, per_s(lapic as u64), per_s(tsc1 - tsc0)))
}

/// The core crystal clock from CPUID leaf 0x15 (None under a hypervisor
/// or when unstated).
fn cpuid_crystal_hz() -> Option<u64> {
    use core::arch::x86_64::__cpuid;
    if __cpuid(1).ecx & (1 << 31) != 0 || __cpuid(0).eax < 0x15 {
        return None;
    }
    Some(__cpuid(0x15).ecx as u64).filter(|&hz| hz != 0)
}

/// The TSC frequency from CPUID leaf 0x15 (crystal clock x ratio), or the
/// base frequency of leaf 0x16; None under a hypervisor or when unstated.
fn cpuid_tsc_hz() -> Option<u64> {
    use core::arch::x86_64::__cpuid;
    let max = __cpuid(0).eax;
    if __cpuid(1).ecx & (1 << 31) != 0 || max < 0x15 {
        return None;
    }
    let l15 = __cpuid(0x15);
    if l15.eax != 0 && l15.ebx != 0 && l15.ecx != 0 {
        return Some(l15.ecx as u64 * l15.ebx as u64 / l15.eax as u64);
    }
    if max >= 0x16 && __cpuid(0x16).eax & 0xffff != 0 {
        return Some((__cpuid(0x16).eax & 0xffff) as u64 * 1_000_000);
    }
    None
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
