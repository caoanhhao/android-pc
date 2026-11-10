//! ACPI static tables: what a PC needs before any driver runs.
//!
//! From the RSDP Limine hands over: the XSDT (or RSDT), then
//! - MADT ("APIC"): the processors' local APIC ids (enabled or online
//!   capable) and the I/O APICs, cross-checked against Limine's MP list;
//! - FADT ("FACP"): the PM timer (3.579545 MHz, 24 or 32 bits, I/O port
//!   or MMIO), the reset register, PM1a control and the DSDT, whose
//!   `\_S5_` package gives the soft-off sleep type (found by its bytes, no
//!   AML interpreter);
//! - HPET: the event timer block's base address.
//!
//! The PM timer and the HPET are the reference clocks for calibrating the
//! TSC and LAPIC timer ([`super::apic`]); the PIT is only the fallback.
//! Tables live in ACPI reclaimable/NVS or reserved memory and are mapped
//! uncached on demand. Missing or malformed tables are logged and their
//! features left out, never fatal.

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

use crate::mm::paging;

/// A Generic Address Structure: address space (0 memory, 1 I/O) and
/// address.
#[derive(Clone, Copy, Debug)]
pub struct Gas {
    pub space: u8,
    pub addr: u64,
}

#[derive(Default)]
pub struct Acpi {
    /// Enabled (or online-capable) processors' local APIC ids.
    pub cpus: Vec<u32>,
    pub ioapics: usize,
    /// PM timer: register, 32 bits wide (else 24).
    pub pm_timer: Option<(Gas, bool)>,
    /// HPET registers (physical).
    pub hpet: Option<u64>,
    pub reset: Option<(Gas, u8)>,
    pub pm1a_cnt: Option<u16>,
    pub pm1b_cnt: Option<u16>,
    /// SLP_TYPa/b of \_S5.
    pub s5: Option<(u8, u8)>,
}

static ACPI: spin::Once<Acpi> = spin::Once::new();

pub const PM_TIMER_HZ: u64 = 3_579_545;

pub fn get() -> Option<&'static Acpi> {
    ACPI.get()
}

/// `len` bytes of physical memory at `phys`.
fn bytes(phys: u64, len: usize) -> &'static [u8] {
    let v = paging::map_mmio(phys, len as u64);
    unsafe { core::slice::from_raw_parts(v.as_ptr::<u8>(), len) }
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn gas(b: &[u8], o: usize) -> Gas {
    Gas { space: b[o], addr: le64(b, o + 4) }
}

/// A whole table (header length, checksum checked) at `phys`.
fn table(phys: u64) -> Option<&'static [u8]> {
    if phys == 0 {
        return None;
    }
    let len = le32(bytes(phys, 36), 4) as usize;
    if !(36..=16 << 20).contains(&len) {
        return None;
    }
    let t = bytes(phys, len);
    if t.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 {
        log::warn!("acpi: {} at {phys:#x}: bad checksum (used anyway)", core::str::from_utf8(&t[..4]).unwrap_or("????"));
    }
    Some(t)
}

/// Parse the tables (once, on the BSP after paging is up).
pub fn init() {
    let Some(rsdp) = crate::boot::rsdp() else {
        log::warn!("acpi: no RSDP from the bootloader");
        return;
    };
    let r = bytes(rsdp, 36);
    if &r[..8] != b"RSD PTR " {
        log::warn!("acpi: no RSDP signature at {rsdp:#x}");
        return;
    }
    let (root, wide) = if r[15] >= 2 && le64(r, 24) != 0 { (le64(r, 24), true) } else { (le32(r, 16) as u64, false) };
    let Some(root) = table(root) else {
        log::warn!("acpi: no XSDT/RSDT");
        return;
    };
    let mut a = Acpi::default();
    let mut sigs = alloc::string::String::new();
    let step = if wide { 8 } else { 4 };
    for o in (36..root.len().saturating_sub(step - 1)).step_by(step) {
        let phys = if wide { le64(root, o) } else { le32(root, o) as u64 };
        let Some(t) = table(phys) else { continue };
        let sig = &t[..4];
        sigs.push_str(core::str::from_utf8(sig).unwrap_or("????"));
        sigs.push(' ');
        match sig {
            b"APIC" => madt(t, &mut a),
            b"FACP" => fadt(t, &mut a),
            b"HPET" if t.len() >= 56 => {
                let g = gas(t, 40);
                if g.space == 0 && g.addr != 0 {
                    a.hpet = Some(g.addr);
                }
            }
            _ => {}
        }
    }
    log::info!("acpi: tables {}", sigs.trim_end());
    log::info!(
        "acpi: {} CPUs, {} I/O APICs, PM timer {}, HPET {}, reset {}, S5 {}",
        a.cpus.len(),
        a.ioapics,
        match a.pm_timer {
            Some((g, wide)) => alloc::format!("{:#x} ({} bits{})", g.addr, if wide { 32 } else { 24 }, if g.space == 1 { ", I/O" } else { ", MMIO" }),
            None => "none".into(),
        },
        a.hpet.map_or("none".into(), |h| alloc::format!("{h:#x}")),
        a.reset.map_or("none".into(), |(g, v)| alloc::format!("{:#x}={v:#x}", g.addr)),
        a.s5.map_or("unknown".into(), |s| alloc::format!("{s:?}"))
    );
    ACPI.call_once(|| a);
}

fn madt(t: &[u8], a: &mut Acpi) {
    let mut o = 44;
    while o + 2 <= t.len() {
        let (ty, len) = (t[o], t[o + 1] as usize);
        if len < 2 || o + len > t.len() {
            break;
        }
        let e = &t[o..o + len];
        match ty {
            // Processor local APIC: enabled or online capable.
            0 if len >= 8 && le32(e, 4) & 3 != 0 => a.cpus.push(e[3] as u32),
            1 => a.ioapics += 1,
            // Processor local x2APIC.
            9 if len >= 16 && le32(e, 8) & 3 != 0 && !a.cpus.contains(&le32(e, 4)) => a.cpus.push(le32(e, 4)),
            _ => {}
        }
        o += len;
    }
}

fn fadt(t: &[u8], a: &mut Acpi) {
    let len = t.len();
    if len < 116 {
        return;
    }
    let flags = le32(t, 112);
    let wide = flags & (1 << 8) != 0;
    // The extended (GAS) PM timer block wins over the I/O port one.
    if len >= 220 && le64(t, 212) != 0 {
        a.pm_timer = Some((gas(t, 208), wide));
    } else if le32(t, 76) != 0 && t[91] == 4 {
        a.pm_timer = Some((Gas { space: 1, addr: le32(t, 76) as u64 }, wide));
    }
    if len >= 129 && flags & (1 << 10) != 0 {
        let g = gas(t, 116);
        if g.addr != 0 {
            a.reset = Some((g, t[128]));
        }
    }
    let pm1 = |legacy: usize, ext: usize| {
        let x = if len >= ext + 12 { gas(t, ext) } else { Gas { space: 1, addr: 0 } };
        if x.space == 1 && x.addr != 0 {
            Some(x.addr as u16)
        } else {
            Some(le32(t, legacy) as u16).filter(|&p| p != 0)
        }
    };
    a.pm1a_cnt = pm1(64, 172);
    a.pm1b_cnt = pm1(68, 184);
    let dsdt = if len >= 148 && le64(t, 140) != 0 { le64(t, 140) } else { le32(t, 40) as u64 };
    if let Some(d) = table(dsdt) {
        a.s5 = s5(d);
    }
}

/// SLP_TYPa/b from the `\_S5_` package: `08 5F53355F 12 <pkglen> <n>
/// <a> <b>`, each value a byte constant (0x0a xx) or Zero/One.
fn s5(dsdt: &[u8]) -> Option<(u8, u8)> {
    let at = dsdt.windows(4).position(|w| w == b"_S5_")?;
    let mut p = at + 4;
    if *dsdt.get(p)? != 0x12 {
        return None;
    }
    p += 1;
    p += ((dsdt.get(p)? >> 6) & 3) as usize + 1; // PkgLength
    p += 1; // NumElements
    let mut val = || -> Option<u8> {
        let b = *dsdt.get(p)?;
        if b == 0x0a {
            p += 2;
            dsdt.get(p - 1).copied()
        } else {
            p += 1;
            Some(b) // ZeroOp 0x00 / OneOp 0x01
        }
    };
    Some((val()?, val()?))
}

/// Read a 32-bit GAS register (I/O port or memory).
fn read_gas(g: &Gas) -> u32 {
    if g.space == 1 {
        unsafe { Port::<u32>::new(g.addr as u16).read() }
    } else {
        let v = paging::map_mmio(g.addr, 4);
        unsafe { core::ptr::read_volatile(v.as_ptr::<u32>()) }
    }
}

/// The PM timer's current count and its mask.
pub fn pm_timer() -> Option<(u32, u32)> {
    let (g, wide) = get()?.pm_timer?;
    let mask = if wide { u32::MAX } else { 0xff_ffff };
    Some((read_gas(&g) & mask, mask))
}

/// The HPET's main counter (enabled if the firmware left it off) and its
/// period in femtoseconds; maps the registers on first use.
pub fn hpet() -> Option<(u64, u64)> {
    let base = get()?.hpet?;
    let v = paging::map_mmio(base, 1024).as_u64();
    let rd = |o: u64| unsafe { core::ptr::read_volatile((v + o) as *const u64) };
    let period = rd(0) >> 32;
    if period == 0 || period > 100_000_000 {
        return None;
    }
    let cfg = rd(0x10);
    if cfg & 1 == 0 {
        unsafe { core::ptr::write_volatile((v + 0x10) as *mut u64, cfg | 1) };
    }
    Some((v, period))
}

/// Reset through the FADT reset register, if there is one.
pub fn reset() {
    let Some((g, val)) = get().and_then(|a| a.reset) else { return };
    unsafe {
        match g.space {
            1 => Port::<u8>::new(g.addr as u16).write(val),
            0 => core::ptr::write_volatile(paging::map_mmio(g.addr, 1).as_mut_ptr::<u8>(), val),
            _ => {}
        }
    }
}

/// Enter S5 (soft off) through PM1a/b control, if \_S5 was found.
pub fn power_off() {
    let Some(a) = get() else { return };
    let (Some((ta, tb)), Some(p1a)) = (a.s5, a.pm1a_cnt) else { return };
    const SLP_EN: u16 = 1 << 13;
    unsafe {
        let mut port = Port::<u16>::new(p1a);
        let v = port.read() & !(7 << 10);
        port.write(v | (ta as u16 & 7) << 10 | SLP_EN);
        if let Some(p1b) = a.pm1b_cnt {
            let mut port = Port::<u16>::new(p1b);
            let v = port.read() & !(7 << 10);
            port.write(v | (tb as u16 & 7) << 10 | SLP_EN);
        }
    }
}

#[test_case]
fn acpi_s5_package() {
    // Name(_S5_, Package(4) {0x05, 0x05, Zero, Zero})
    let d = [0x08, b'_', b'S', b'5', b'_', 0x12, 0x0a, 0x04, 0x0a, 0x05, 0x0a, 0x05, 0x00, 0x00];
    assert_eq!(s5(&d), Some((5, 5)));
    // Package(2) {Zero, One}
    let d = [b'_', b'S', b'5', b'_', 0x12, 0x06, 0x02, 0x00, 0x01];
    assert_eq!(s5(&d), Some((0, 1)));
}
