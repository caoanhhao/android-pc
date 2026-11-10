//! Per-CPU platform features of Intel CPUs: temperatures (coretemp,
//! x86_pkg_temp), HWP frequency control with cpufreq's sysfs view, and the
//! hybrid P-/E-core topology.
//!
//! Each CPU sets itself up from its own timer tick (MSRs are per logical
//! CPU) the first time it runs, then samples once a second:
//! - its digital thermal sensor (IA32_THERM_STATUS: degrees below TjMax
//!   from MSR_TEMPERATURE_TARGET, else 100 °C) and, on the BSP, the
//!   package sensor (IA32_PACKAGE_THERM_STATUS);
//! - APERF/MPERF, whose ratio times the base frequency is the frequency
//!   it ran at while not idle (`scaling_cur_freq`).
//!
//! With HWP (CPUID.06H:EAX[7], off with `nohwp` on the command line),
//! IA32_PM_ENABLE hands P-states to the hardware, and every CPU asks for
//! its whole range (lowest..highest from IA32_HWP_CAPABILITIES, so P-cores
//! turbo) with the "balance_performance" energy/performance preference
//! (0x80), as intel_pstate does by default. HWP performance levels are
//! 100 MHz each, 78.741 MHz on hybrid P-cores (intel_pstate's scaling).
//!
//! On hybrid parts (CPUID.07H:EDX[15]) CPUID.1AH gives each CPU's core
//! type; [`pick_idle`] makes the scheduler wake idle P-cores first.
//!
//! sysfs: thermal_zone0 (`x86_pkg_temp`, under devices/virtual/thermal),
//! hwmon0 (`coretemp`: "Package id 0" and one "Core N" per physical core,
//! under devices/platform/coretemp.0), cpufreq/policyN with cpuN/cpufreq
//! links, cpuN/topology, and cpu_core/cpu_atom's `cpus` lists. Every value
//! is read when the file is opened. Without the sensors (QEMU) there are
//! no thermal zones or hwmon device.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering};
use spin::Once;
use x86_64::registers::model_specific::Msr;

use crate::arch::x86_64::cpu::MAX_CPUS;
use crate::fs::sysfs;

const IA32_MPERF: u32 = 0xe7;
const IA32_APERF: u32 = 0xe8;
const IA32_THERM_STATUS: u32 = 0x19c;
const MSR_TEMPERATURE_TARGET: u32 = 0x1a2;
const IA32_PACKAGE_THERM_STATUS: u32 = 0x1b1;
const IA32_PM_ENABLE: u32 = 0x770;
const IA32_HWP_CAPABILITIES: u32 = 0x771;
const IA32_HWP_REQUEST: u32 = 0x774;
/// intel_pstate's balance_performance.
const EPP_BALANCE_PERFORMANCE: u64 = 0x80;
const CORE_TYPE_ATOM: u8 = 0x20;
const CORE_TYPE_CORE: u8 = 0x40;
const NO_TEMP: i32 = i32::MIN;

/// What the BSP found (same on every CPU).
struct Features {
    dts: bool,
    ptm: bool,
    hwp: bool,
    epp: bool,
    aperfmperf: bool,
    hybrid: bool,
    tjmax: i32,
    /// Base (nominal) frequency, kHz.
    base_khz: u32,
    /// CPUID.16H maximum, kHz (0: unknown).
    max_khz: u32,
}

static FEAT: Once<Features> = Once::new();

struct Cpu {
    ready: AtomicBool,
    apic: AtomicU32,
    core_id: AtomicU32,
    core_type: AtomicU8,
    /// Millidegrees Celsius.
    temp: AtomicI32,
    /// HWP lowest/highest performance levels.
    hwp_lo: AtomicU32,
    hwp_hi: AtomicU32,
    hwp_guar: AtomicU32,
    last_aperf: AtomicU64,
    last_mperf: AtomicU64,
    cur_khz: AtomicU32,
}

static CPUS: [Cpu; MAX_CPUS] = [const {
    Cpu {
        ready: AtomicBool::new(false),
        apic: AtomicU32::new(0),
        core_id: AtomicU32::new(0),
        core_type: AtomicU8::new(0),
        temp: AtomicI32::new(NO_TEMP),
        hwp_lo: AtomicU32::new(0),
        hwp_hi: AtomicU32::new(0),
        hwp_guar: AtomicU32::new(0),
        last_aperf: AtomicU64::new(0),
        last_mperf: AtomicU64::new(0),
        cur_khz: AtomicU32::new(0),
    }
}; MAX_CPUS];
static PKG_TEMP: AtomicI32 = AtomicI32::new(NO_TEMP);
/// CPUs that are P-cores (hybrid parts only).
static P_CORES: AtomicU64 = AtomicU64::new(0);
static TICKS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

fn rdmsr(m: u32) -> u64 {
    unsafe { Msr::new(m).read() }
}

fn wrmsr(m: u32, v: u64) {
    unsafe { Msr::new(m).write(v) }
}

/// Detect the features on the BSP (after the TSC is calibrated).
pub fn init() {
    let max_leaf = __cpuid(0);
    let intel = (max_leaf.ebx, max_leaf.edx, max_leaf.ecx) == (0x756e_6547, 0x4965_6e69, 0x6c65_746e);
    let max_leaf = max_leaf.eax;
    let zero = core::arch::x86_64::CpuidResult { eax: 0, ebx: 0, ecx: 0, edx: 0 };
    let (l6, l7) = if max_leaf >= 7 { (__cpuid(6), __cpuid_count(7, 0)) } else { (zero, zero) };
    let nohwp = crate::boot::cmdline().split_ascii_whitespace().any(|w| w == "nohwp" || w == "intel_pstate=no_hwp");
    let dts = intel && l6.eax & 1 != 0;
    let tjmax = if dts {
        let t = ((rdmsr(MSR_TEMPERATURE_TARGET) >> 16) & 0xff) as i32;
        if (70..=130).contains(&t) { t } else { 100 }
    } else {
        100
    };
    let l16 = if max_leaf >= 0x16 { __cpuid(0x16) } else { zero };
    let tsc_khz = (crate::time::tsc_hz() / 1000) as u32;
    let base_khz = if l16.eax & 0xffff != 0 { (l16.eax & 0xffff) * 1000 } else { tsc_khz };
    let f = Features {
        dts,
        ptm: dts && l6.eax & (1 << 6) != 0,
        hwp: intel && l6.eax & (1 << 7) != 0 && !nohwp,
        epp: l6.eax & (1 << 10) != 0,
        aperfmperf: l6.ecx & 1 != 0,
        hybrid: l7.edx & (1 << 15) != 0 && max_leaf >= 0x1a,
        tjmax,
        base_khz,
        max_khz: (l16.ebx & 0xffff) * 1000,
    };
    log::info!(
        "cpu: {} thermal sensors{} (TjMax {} C), HWP {}, APERF/MPERF {}, {}base {} MHz{}",
        if f.dts { "digital" } else { "no" },
        if f.ptm { " + package" } else { "" },
        f.tjmax,
        if f.hwp { if f.epp { "on (EPP balance_performance)" } else { "on" } } else if l6.eax & (1 << 7) != 0 { "off (nohwp)" } else { "no" },
        if f.aperfmperf { "yes" } else { "no" },
        if f.hybrid { "hybrid P/E cores, " } else { "" },
        f.base_khz / 1000,
        if f.max_khz != 0 { format!(", max {} MHz", f.max_khz / 1000) } else { String::new() }
    );
    FEAT.call_once(|| f);
    // The BSP sets itself up now (the log above then has its numbers).
    setup_this_cpu(crate::arch::cpu_index() as usize);
}

/// x2APIC id, core id (APIC id without the SMT bits) and core type of the
/// executing CPU.
fn topology(hybrid: bool) -> (u32, u32, u8) {
    let max = __cpuid(0).eax;
    let (apic, smt_shift) = if max >= 0xb && __cpuid_count(0xb, 0).ebx != 0 {
        let l = __cpuid_count(0xb, 0);
        let shift = if (l.ecx >> 8) & 0xff == 1 { l.eax & 0x1f } else { 0 };
        (l.edx, shift)
    } else {
        (__cpuid(1).ebx >> 24, 0)
    };
    let ty = if hybrid { (__cpuid(0x1a).eax >> 24) as u8 } else { 0 };
    (apic, apic >> smt_shift, ty)
}

fn setup_this_cpu(i: usize) {
    let (Some(f), Some(c)) = (FEAT.get(), CPUS.get(i)) else { return };
    if c.ready.load(Ordering::Relaxed) {
        return;
    }
    let (apic, core, ty) = topology(f.hybrid);
    c.apic.store(apic, Ordering::Relaxed);
    c.core_id.store(core, Ordering::Relaxed);
    c.core_type.store(ty, Ordering::Relaxed);
    if ty == CORE_TYPE_CORE {
        P_CORES.fetch_or(1 << i, Ordering::Relaxed);
    }
    if f.hwp {
        if rdmsr(IA32_PM_ENABLE) & 1 == 0 {
            wrmsr(IA32_PM_ENABLE, 1);
        }
        let caps = rdmsr(IA32_HWP_CAPABILITIES);
        let (hi, guar, lo) = (caps & 0xff, (caps >> 8) & 0xff, (caps >> 24) & 0xff);
        c.hwp_lo.store(lo as u32, Ordering::Relaxed);
        c.hwp_hi.store(hi as u32, Ordering::Relaxed);
        c.hwp_guar.store(guar as u32, Ordering::Relaxed);
        // min, max, desired 0 (autonomous), EPP (a reserved field without
        // CPUID.06H:EAX[10]).
        let epp = if f.epp { EPP_BALANCE_PERFORMANCE } else { 0 };
        wrmsr(IA32_HWP_REQUEST, lo | hi << 8 | epp << 24);
    }
    if f.aperfmperf {
        c.last_aperf.store(rdmsr(IA32_APERF), Ordering::Relaxed);
        c.last_mperf.store(rdmsr(IA32_MPERF), Ordering::Relaxed);
    }
    c.cur_khz.store(f.base_khz, Ordering::Relaxed);
    c.ready.store(true, Ordering::Release);
    sample(i, f, c);
}

/// Thermal readings and the frequency since the last sample.
fn sample(i: usize, f: &Features, c: &Cpu) {
    if f.dts {
        let s = rdmsr(IA32_THERM_STATUS);
        if s & (1 << 31) != 0 {
            c.temp.store((f.tjmax - ((s >> 16) & 0x7f) as i32) * 1000, Ordering::Relaxed);
        }
        if f.ptm && i == 0 {
            let s = rdmsr(IA32_PACKAGE_THERM_STATUS);
            PKG_TEMP.store((f.tjmax - ((s >> 16) & 0x7f) as i32) * 1000, Ordering::Relaxed);
        }
    }
    if f.aperfmperf {
        let (a, m) = (rdmsr(IA32_APERF), rdmsr(IA32_MPERF));
        let da = a.wrapping_sub(c.last_aperf.swap(a, Ordering::Relaxed));
        let dm = m.wrapping_sub(c.last_mperf.swap(m, Ordering::Relaxed));
        if dm > 1000 {
            c.cur_khz.store((f.base_khz as u128 * da as u128 / dm as u128) as u32, Ordering::Relaxed);
        }
    }
}

/// Timer tick on every CPU.
pub fn tick() {
    let i = crate::arch::x86_64::cpu::index_fast();
    let (Some(f), Some(c), Some(t)) = (FEAT.get(), CPUS.get(i), TICKS.get(i)) else { return };
    if !c.ready.load(Ordering::Acquire) {
        setup_this_cpu(i);
        return;
    }
    if t.fetch_add(1, Ordering::Relaxed) % crate::time::HZ as u32 == 0 {
        sample(i, f, c);
    }
}

/// The idle CPU (in `idle`, non-empty) the scheduler should wake: a P-core
/// if one is idle.
pub fn pick_idle(idle: u64) -> usize {
    let p = idle & P_CORES.load(Ordering::Relaxed);
    if p != 0 { p.trailing_zeros() as usize } else { idle.trailing_zeros() as usize }
}

/// kHz of HWP performance level `perf` on CPU `c`.
fn perf_khz(c: &Cpu, perf: u32) -> u32 {
    if c.core_type.load(Ordering::Relaxed) == CORE_TYPE_CORE { perf * 78_741 } else { perf * 100_000 }
}

fn online() -> usize {
    crate::arch::x86_64::smp::online_cpus().min(MAX_CPUS)
}

/// "0-3,8" for a CPU mask.
fn cpu_list(mask: u64) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < 64 {
        if mask & (1 << i) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < 64 && mask & (1 << i) != 0 {
            i += 1;
        }
        if !s.is_empty() {
            s.push(',');
        }
        s += &if i - 1 > start { format!("{}-{}", start, i - 1) } else { format!("{start}") };
    }
    s.push('\n');
    s
}

fn max_khz(c: &Cpu, f: &Features) -> u32 {
    let hi = c.hwp_hi.load(Ordering::Relaxed);
    if hi != 0 { perf_khz(c, hi) } else if f.max_khz != 0 { f.max_khz } else { f.base_khz }
}

fn min_khz(c: &Cpu, f: &Features) -> u32 {
    let lo = c.hwp_lo.load(Ordering::Relaxed);
    if lo != 0 { perf_khz(c, lo) } else { f.base_khz }
}

/// sysfs (after the VFS exists).
pub fn init_sysfs() {
    let Some(f) = FEAT.get() else { return };
    let n = online();
    // Each CPU sets itself up at its next tick.
    let start = crate::time::monotonic_ns();
    while !CPUS[..n].iter().all(|c| c.ready.load(Ordering::Acquire)) && crate::time::monotonic_ns() - start < 500_000_000 {
        core::hint::spin_loop();
    }
    for i in 0..n {
        let pol = format!("devices/system/cpu/cpufreq/policy{i}");
        let line = |v: u32| format!("{v}\n");
        sysfs::set_fn(&pol, "scaling_cur_freq", move || line(CPUS[i].cur_khz.load(Ordering::Relaxed)));
        sysfs::set_fn(&pol, "cpuinfo_max_freq", move || line(max_khz(&CPUS[i], FEAT.get().unwrap())));
        sysfs::set_fn(&pol, "cpuinfo_min_freq", move || line(min_khz(&CPUS[i], FEAT.get().unwrap())));
        sysfs::set_fn(&pol, "scaling_max_freq", move || line(max_khz(&CPUS[i], FEAT.get().unwrap())));
        sysfs::set_fn(&pol, "scaling_min_freq", move || line(min_khz(&CPUS[i], FEAT.get().unwrap())));
        sysfs::set_fn(&pol, "base_frequency", move || {
            let c = &CPUS[i];
            let g = c.hwp_guar.load(Ordering::Relaxed);
            line(if g != 0 { perf_khz(c, g) } else { FEAT.get().unwrap().base_khz })
        });
        sysfs::set(&pol, "cpuinfo_transition_latency", "0\n");
        sysfs::set(&pol, "affected_cpus", &format!("{i}\n"));
        sysfs::set(&pol, "related_cpus", &format!("{i}\n"));
        sysfs::set(&pol, "scaling_driver", if f.hwp { "intel_pstate\n" } else { "acpi-cpufreq\n" });
        sysfs::set_rw(&pol, "scaling_governor", if f.hwp { "powersave\n" } else { "performance\n" });
        sysfs::set(&pol, "scaling_available_governors", "performance powersave\n");
        if f.hwp && f.epp {
            sysfs::set_rw(&pol, "energy_performance_preference", "balance_performance\n");
            sysfs::set(&pol, "energy_performance_available_preferences", "default performance balance_performance balance_power power\n");
        }
        let cpu = format!("devices/system/cpu/cpu{i}");
        sysfs::symlink(&cpu, "cpufreq", &format!("../cpufreq/policy{i}"));
        sysfs::set(&cpu, "online", "1\n");
        let topo = format!("{cpu}/topology");
        sysfs::set(&topo, "physical_package_id", "0\n");
        sysfs::set(&topo, "die_id", "0\n");
        sysfs::set_fn(&topo, "core_id", move || format!("{}\n", CPUS[i].core_id.load(Ordering::Relaxed)));
        let siblings = move || {
            let id = CPUS[i].core_id.load(Ordering::Relaxed);
            cpu_list((0..online()).filter(|&j| CPUS[j].core_id.load(Ordering::Relaxed) == id).fold(0, |m, j| m | 1 << j))
        };
        sysfs::set_fn(&topo, "thread_siblings_list", siblings);
        sysfs::set_fn(&topo, "core_cpus_list", siblings);
        sysfs::set_fn(&topo, "package_cpus_list", || cpu_list(u64::MAX >> (64 - online())));
    }
    if f.hybrid {
        sysfs::set_fn("devices/cpu_core", "cpus", || cpu_list(P_CORES.load(Ordering::Relaxed)));
        sysfs::set_fn("devices/cpu_atom", "cpus", || {
            cpu_list((0..online()).filter(|&j| CPUS[j].core_type.load(Ordering::Relaxed) == CORE_TYPE_ATOM).fold(0, |m, j| m | 1 << j))
        });
    }
    if f.dts {
        thermal_sysfs(f);
    }
    let types = if f.hybrid {
        let p = P_CORES.load(Ordering::Relaxed).count_ones();
        format!(", {} P + {} E CPUs online", p, n as u32 - p)
    } else {
        String::new()
    };
    log::info!("cpufreq: {} policies ({}){types}", n, if f.hwp { "intel_pstate, HWP" } else { "fixed" });
}

fn thermal_sysfs(f: &Features) {
    let temp = |v: i32| if v == NO_TEMP { String::new() } else { format!("{v}\n") };
    if f.ptm {
        let tz = "devices/virtual/thermal/thermal_zone0";
        sysfs::add_device(tz, "thermal", "class/thermal", String::new(), true);
        sysfs::set(tz, "type", "x86_pkg_temp\n");
        sysfs::set_fn(tz, "temp", move || temp(PKG_TEMP.load(Ordering::Relaxed)));
        sysfs::set(tz, "mode", "enabled\n");
        sysfs::set(tz, "policy", "step_wise\n");
        sysfs::set(tz, "available_policies", "step_wise user_space\n");
        sysfs::set(tz, "trip_point_0_type", "passive\n");
        sysfs::set(tz, "trip_point_0_temp", "0\n");
    }
    let hw = "devices/platform/coretemp.0/hwmon/hwmon0";
    sysfs::add_device(hw, "hwmon", "class/hwmon", String::new(), true);
    sysfs::set(hw, "name", "coretemp\n");
    let crit = format!("{}\n", f.tjmax * 1000);
    let max = format!("{}\n", (f.tjmax - 10) * 1000);
    let mut n = 1;
    if f.ptm {
        sysfs::set(hw, "temp1_label", "Package id 0\n");
        sysfs::set_fn(hw, "temp1_input", move || temp(PKG_TEMP.load(Ordering::Relaxed)));
        sysfs::set(hw, "temp1_crit", &crit);
        sysfs::set(hw, "temp1_max", &max);
        n = 2;
    }
    // One entry per physical core (its first CPU), as coretemp.
    let mut seen: Vec<u32> = Vec::new();
    for i in 0..online() {
        let c = &CPUS[i];
        if !c.ready.load(Ordering::Acquire) {
            continue;
        }
        let core = c.core_id.load(Ordering::Relaxed);
        if seen.contains(&core) {
            continue;
        }
        seen.push(core);
        sysfs::set(hw, &format!("temp{n}_label"), &format!("Core {core}\n"));
        sysfs::set_fn(hw, &format!("temp{n}_input"), move || temp(CPUS[i].temp.load(Ordering::Relaxed)));
        sysfs::set(hw, &format!("temp{n}_crit"), &crit);
        sysfs::set(hw, &format!("temp{n}_max"), &max);
        n += 1;
    }
    log::info!(
        "thermal: x86_pkg_temp {}, coretemp {} cores, package {} C",
        if f.ptm { "thermal_zone0" } else { "none" },
        seen.len(),
        PKG_TEMP.load(Ordering::Relaxed) / 1000
    );
}

#[test_case]
fn cpu_list_format() {
    assert_eq!(cpu_list(0b1111), "0-3\n");
    assert_eq!(cpu_list(0b1011_0001), "0,4-5,7\n");
    assert_eq!(cpu_list(1 << 63), "63\n");
}
