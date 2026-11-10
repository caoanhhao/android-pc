//! ACPI static tables: what a PC needs before any driver runs.
//!
//! From the RSDP Limine hands over: the XSDT (or RSDT), then
//! - MADT ("APIC"): the processors' local APIC ids (enabled or online
//!   capable) and the I/O APICs, cross-checked against Limine's MP list;
//! - FADT ("FACP"): the PM timer (3.579545 MHz, 24 or 32 bits, I/O port
//!   or MMIO), the reset register, the PM1 event and control blocks (the
//!   fixed-feature power button, sleep), SMI_CMD/ACPI_ENABLE, the CMOS
//!   RTC's century and alarm registers, the SCI's interrupt, and the DSDT,
//!   whose `\_S5_` package gives the soft-off sleep type (found by its
//!   bytes, no AML interpreter);
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
    /// PM1a/b event blocks (status, then enable at +len/2) and PM1_EVT_LEN.
    pub pm1a_evt: Option<u16>,
    pub pm1b_evt: Option<u16>,
    pub pm1_evt_len: u8,
    /// SMI command port and the value that switches the chipset to ACPI
    /// mode (SCI_EN), if the firmware left it in legacy mode.
    pub smi_cmd: u16,
    pub acpi_enable: u8,
    /// FADT flags (PWR_BUTTON bit 4: the power button is a control-method
    /// device instead of the fixed feature; HW_REDUCED_ACPI bit 20).
    pub flags: u32,
    /// IAPC_BOOT_ARCH (bit 5: no CMOS RTC).
    pub boot_arch: u16,
    /// CMOS RAM indexes of the RTC's century, day-of-month alarm and
    /// month alarm registers (0: none).
    pub century: u8,
    pub day_alarm: u8,
    pub month_alarm: u8,
    /// The SCI's ISA interrupt.
    pub sci_irq: u16,
    /// Hardware-reduced ACPI sleep control/status registers.
    pub sleep_ctl: Option<Gas>,
    /// I/O APICs: (registers, first GSI).
    pub ioapic_list: Vec<(u64, u32)>,
    /// Interrupt source overrides: (ISA IRQ, GSI).
    pub isos: Vec<(u8, u32)>,
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
            1 if len >= 12 => {
                a.ioapics += 1;
                a.ioapic_list.push((le32(e, 4) as u64, le32(e, 8)));
            }
            2 if len >= 10 => a.isos.push((e[3], le32(e, 4))),
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
    a.pm1a_evt = pm1(56, 148);
    a.pm1b_evt = pm1(60, 160);
    a.pm1_evt_len = t[88];
    a.smi_cmd = le32(t, 48) as u16;
    a.acpi_enable = t[52];
    a.sci_irq = u16::from_le_bytes([t[46], t[47]]);
    a.flags = flags;
    a.day_alarm = t[106];
    a.month_alarm = t[107];
    a.century = t[108];
    a.boot_arch = u16::from_le_bytes([t[109], t[110]]);
    if flags & (1 << 20) != 0 && len >= 256 {
        let g = gas(t, 244);
        if g.addr != 0 {
            a.sleep_ctl = Some(g);
        }
    }
    let dsdt = if len >= 148 && le64(t, 140) != 0 { le64(t, 140) } else { le32(t, 40) as u64 };
    if let Some(d) = table(dsdt) {
        a.s5 = s5(d);
    }
}

/// SLP_TYPa/b from the `\_S5_` package: `08 5F53355F 12 <pkglen> <n>
/// <a> <b>`, each value a byte constant (0x0a xx) or Zero/One.
fn s5(dsdt: &[u8]) -> Option<(u8, u8)> {
    dsdt.windows(4).enumerate().filter(|(_, w)| *w == b"_S5_").find_map(|(i, _)| s5_at(dsdt, i + 4))
}

fn s5_at(dsdt: &[u8], mut p: usize) -> Option<(u8, u8)> {
    if *dsdt.get(p)? != 0x12 {
        return None;
    }
    p += 1;
    p += ((dsdt.get(p)? >> 6) & 3) as usize + 1; // PkgLength
    p += 1; // NumElements
    let mut val = || -> Option<u8> {
        let b = *dsdt.get(p)?;
        match b {
            0x0a => {
                p += 2;
                dsdt.get(p - 1).copied()
            }
            // ZeroOp / OneOp
            0x00 | 0x01 => {
                p += 1;
                Some(b)
            }
            _ => None,
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

/// Reset through the FADT reset register, if there is one (I/O port,
/// memory, or a bus-0 PCI configuration register).
pub fn reset() {
    let Some((g, val)) = get().and_then(|a| a.reset) else { return };
    log::info!("acpi: reset register {:#x} (space {}) <- {val:#x}", g.addr, g.space);
    unsafe {
        match g.space {
            1 => Port::<u8>::new(g.addr as u16).write(val),
            0 => core::ptr::write_volatile(paging::map_mmio(g.addr, 1).as_mut_ptr::<u8>(), val),
            2 => {
                // Device [47:32], function [31:16], offset [15:0].
                let (dev, func, off) = ((g.addr >> 32) & 0x1f, (g.addr >> 16) & 7, g.addr & 0xff);
                Port::<u32>::new(0xcf8).write(0x8000_0000 | (dev << 11 | func << 8 | (off & 0xfc)) as u32);
                Port::<u8>::new(0xcfc + (off & 3) as u16).write(val);
            }
            _ => {}
        }
    }
}

const SLP_EN: u16 = 1 << 13;
/// PM1 control: SCI_EN (ACPI mode).
const SCI_EN: u16 = 1;
/// PM1 status: WAK_STS.
const WAK_STS: u16 = 1 << 15;

/// Enter S5 (soft off) through PM1a/b control (or the hardware-reduced
/// sleep control register), if \_S5 was found. Like Linux: clear WAK_STS,
/// write SLP_TYP, then SLP_TYP | SLP_EN. No _PTS: there is no AML
/// interpreter.
pub fn power_off() {
    let Some(a) = get() else { return };
    let Some((ta, tb)) = a.s5 else {
        log::warn!("acpi: no \\_S5 sleep type, cannot power off through ACPI");
        return;
    };
    log::info!("acpi: entering S5 (SLP_TYPa {ta} SLP_TYPb {tb})");
    unsafe {
        if let Some(g) = a.sleep_ctl {
            // SLP_TYP [4:2], SLP_EN bit 5.
            let v = ((ta & 7) << 2) | 1 << 5;
            match g.space {
                1 => Port::<u8>::new(g.addr as u16).write(v),
                0 => core::ptr::write_volatile(paging::map_mmio(g.addr, 1).as_mut_ptr::<u8>(), v),
                _ => {}
            }
            return;
        }
        let Some(p1a) = a.pm1a_cnt else { return };
        for evt in [a.pm1a_evt, a.pm1b_evt].into_iter().flatten() {
            Port::<u16>::new(evt).write(WAK_STS);
        }
        for (port, typ) in [(Some(p1a), ta), (a.pm1b_cnt, tb)] {
            let Some(port) = port else { continue };
            let mut port = Port::<u16>::new(port);
            let v = port.read() & !(7 << 10 | SLP_EN);
            port.write(v | (typ as u16 & 7) << 10);
        }
        for (port, typ) in [(Some(p1a), ta), (a.pm1b_cnt, tb)] {
            let Some(port) = port else { continue };
            let mut port = Port::<u16>::new(port);
            let v = port.read() & !(7 << 10 | SLP_EN);
            port.write(v | (typ as u16 & 7) << 10 | SLP_EN);
        }
    }
}

/// Is the chipset in ACPI mode (SCI_EN)?
pub fn acpi_mode() -> bool {
    get().and_then(|a| a.pm1a_cnt).is_some_and(|p| unsafe { Port::<u16>::new(p).read() } & SCI_EN != 0)
}

/// Switch the chipset from legacy (SMI) to ACPI mode if the firmware left
/// it in legacy mode, as Linux does: write ACPI_ENABLE to SMI_CMD and wait
/// up to 3 s for SCI_EN. In legacy mode the firmware's SMI handler owns
/// the power button (and typically powers off at once).
pub fn enable_acpi_mode() -> bool {
    let Some(a) = get() else { return false };
    if a.flags & (1 << 20) != 0 || a.pm1a_cnt.is_none() {
        return false;
    }
    if acpi_mode() {
        log::info!("acpi: already in ACPI mode (SCI_EN set)");
        return true;
    }
    if a.smi_cmd == 0 || a.acpi_enable == 0 {
        log::warn!("acpi: legacy mode and no SMI_CMD/ACPI_ENABLE to leave it");
        return false;
    }
    unsafe { Port::<u8>::new(a.smi_cmd).write(a.acpi_enable) };
    let start = crate::time::monotonic_ns();
    while crate::time::monotonic_ns() - start < 3_000_000_000 {
        if acpi_mode() {
            log::info!("acpi: switched to ACPI mode (SMI_CMD {:#x} <- {:#x})", a.smi_cmd, a.acpi_enable);
            return true;
        }
        core::hint::spin_loop();
    }
    log::warn!("acpi: SCI_EN still clear 3 s after ACPI_ENABLE");
    false
}

/// Read the PM1 status (`enable` false) or enable register of the PM1a
/// and PM1b event blocks, ORed.
pub fn pm1_read(enable: bool) -> u16 {
    let Some(a) = get() else { return 0 };
    let off = if enable { (a.pm1_evt_len / 2).max(2) as u16 } else { 0 };
    [a.pm1a_evt, a.pm1b_evt].into_iter().flatten().fold(0, |v, p| v | unsafe { Port::<u16>::new(p + off).read() })
}

/// Write `v` to the PM1a/b status (write-1-to-clear) or enable registers.
pub fn pm1_write(enable: bool, v: u16) {
    let Some(a) = get() else { return };
    let off = if enable { (a.pm1_evt_len / 2).max(2) as u16 } else { 0 };
    for p in [a.pm1a_evt, a.pm1b_evt].into_iter().flatten() {
        unsafe { Port::<u16>::new(p + off).write(v) };
    }
}

/// Mask the I/O APIC pin of ISA interrupt `irq` (after interrupt source
/// overrides). Nothing here routes I/O APIC interrupts: drivers poll, so
/// a level-triggered line (the SCI once ACPI mode is on) must stay masked
/// whatever the firmware left in the redirection table.
pub fn mask_isa_irq(irq: u8) {
    let Some(a) = get() else { return };
    let gsi = a.isos.iter().find(|&&(src, _)| src == irq).map_or(irq as u32, |&(_, g)| g);
    for &(base, gsi_base) in &a.ioapic_list {
        let v = paging::map_mmio(base, 0x20).as_u64();
        let sel = v as *mut u32;
        let win = (v + 0x10) as *mut u32;
        unsafe {
            core::ptr::write_volatile(sel, 1);
            let pins = ((core::ptr::read_volatile(win) >> 16) & 0xff) + 1;
            if !(gsi_base..gsi_base + pins).contains(&gsi) {
                continue;
            }
            let reg = 0x10 + 2 * (gsi - gsi_base);
            core::ptr::write_volatile(sel, reg);
            let low = core::ptr::read_volatile(win);
            core::ptr::write_volatile(win, low | 1 << 16);
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
    // A reference to _S5_ before the definition is skipped.
    let d = [b'_', b'S', b'5', b'_', 0x70, 0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x0a, 0x07, 0x00, 0x00, 0x00];
    assert_eq!(s5(&d), Some((7, 0)));
}
