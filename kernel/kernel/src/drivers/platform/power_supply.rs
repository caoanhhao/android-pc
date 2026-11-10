//! The AC adapter as a power supply, so Android's health HAL (healthd's
//! BatteryMonitor, which scans /sys/class/power_supply) sees a mains
//! charger online and no battery: a desktop PC is always plugged in.
//!
//! Linux names it after the ACPI AC device (ACPI0003, "AC"); desktop
//! boards often have none, but the machine is on mains all the same.

use alloc::string::String;

use crate::fs::sysfs;

const SYSDIR: &str = "devices/LNXSYSTM:00/LNXSYBUS:00/ACPI0003:00/power_supply/AC";

pub fn init() {
    let vars = String::from("POWER_SUPPLY_NAME=AC\nPOWER_SUPPLY_TYPE=Mains\nPOWER_SUPPLY_ONLINE=1\n");
    sysfs::add_device(SYSDIR, "power_supply", "class/power_supply", vars, true);
    sysfs::set(SYSDIR, "type", "Mains\n");
    sysfs::set(SYSDIR, "online", "1\n");
    log::info!("power supply: AC (Mains) online, no battery");
}
