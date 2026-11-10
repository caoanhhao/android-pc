//! The ACPI fixed-feature power button, as Linux's "Power Button" input
//! device (LNXPWRBN, bus BUS_HOST, product 1, KEY_POWER only).
//!
//! The chipset is switched to ACPI mode first (`acpi::enable_acpi_mode`),
//! so a press sets PWRBTN_STS in PM1 status instead of reaching the
//! firmware's SMI handler, and PWRBTN_EN is set. The SCI is not routed
//! (its I/O APIC pin stays masked): the BSP's timer tick polls PM1 status
//! every 50 ms; a set PWRBTN_STS is cleared (write 1) and, in task
//! context, reported as KEY_POWER down, SYN, KEY_POWER up, SYN, as Linux's
//! button driver does. Android's PhoneWindowManager turns that into
//! sleep/wake (short press) and the power menu (long press, from a key
//! held down: a fixed-feature button has no release, so only keys that
//! report one get there; `input keyevent --longpress POWER` does).
//!
//! Without the fixed feature (FADT PWR_BUTTON set: a control-method
//! button that needs AML) or without PM1 event blocks, there is no device.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Once;

use crate::arch::x86_64::acpi;
use crate::drivers::input::{self, Caps, EV_KEY, EV_SYN, InputDev, SYN_REPORT};

const PWRBTN: u16 = 1 << 8;
const KEY_POWER: u16 = 116;
const BUS_HOST: u16 = 0x19;
const SYSDIR: &str = "devices/LNXSYSTM:00/LNXPWRBN:00";

static DEV: Once<Arc<InputDev>> = Once::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Presses seen by the tick, not yet reported.
static PRESSES: AtomicU32 = AtomicU32::new(0);
static TICKS: AtomicU32 = AtomicU32::new(0);

pub fn init() {
    let Some(a) = acpi::get() else { return };
    if a.flags & (1 << 4) != 0 {
        log::info!("power button: control-method button (needs AML), not supported");
        return;
    }
    if a.pm1a_evt.is_none() {
        log::info!("power button: no PM1 event block");
        return;
    }
    acpi::mask_isa_irq(a.sci_irq as u8);
    acpi::enable_acpi_mode();
    // A press from before boot (the one that turned the machine on) is
    // not a request to sleep.
    acpi::pm1_write(false, PWRBTN);
    let en = acpi::pm1_read(true);
    acpi::pm1_write(true, en | PWRBTN);
    let mut caps = Caps::new("Power Button");
    caps.phys = String::from("LNXPWRBN/button/input0");
    caps.id = [BUS_HOST, 0, 1, 0];
    caps.ev.set(EV_KEY as usize, true);
    caps.codes[EV_KEY as usize].set(KEY_POWER as usize, true);
    crate::fs::sysfs::add_device(SYSDIR, "acpi", "bus/acpi", String::from("MODALIAS=acpi:LNXPWRBN:\n"), false);
    crate::fs::sysfs::set(SYSDIR, "hid", "LNXPWRBN\n");
    match input::register(caps, SYSDIR, None) {
        Ok(d) => {
            log::info!(
                "power button: ACPI fixed feature, PM1a_EVT {:#x}, SCI IRQ {} masked, input{}",
                a.pm1a_evt.unwrap_or(0),
                a.sci_irq,
                d.index
            );
            DEV.call_once(|| d);
            ACTIVE.store(true, Ordering::Release);
        }
        Err(e) => log::warn!("power button: input device: {e:?}"),
    }
}

/// BSP timer tick: poll PWRBTN_STS every 5 ticks. True when a press
/// waits for [`softirq`].
pub fn tick() -> bool {
    if !ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    if TICKS.fetch_add(1, Ordering::Relaxed) % 5 == 0 && acpi::pm1_read(false) & PWRBTN != 0 {
        acpi::pm1_write(false, PWRBTN);
        PRESSES.fetch_add(1, Ordering::Relaxed);
    }
    PRESSES.load(Ordering::Relaxed) != 0
}

/// Report the presses (task context).
pub fn softirq() {
    let n = PRESSES.swap(0, Ordering::Relaxed);
    let Some(d) = DEV.get() else { return };
    for _ in 0..n {
        log::info!("power button pressed");
        d.event(EV_KEY, KEY_POWER, 1);
        d.event(EV_SYN, SYN_REPORT, 0);
        d.event(EV_KEY, KEY_POWER, 0);
        d.event(EV_SYN, SYN_REPORT, 0);
    }
}
