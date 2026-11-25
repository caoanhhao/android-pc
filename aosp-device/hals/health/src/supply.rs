//! Power supplies under `/sys/class/power_supply`: AC adapters, USB chargers, batteries.
//!
//! This module only reads sysfs and knows nothing about AIDL, so its tests run on the
//! host (`android.hardware.health-service.pc_supply_test`).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use log::debug;

/// A power supply's role, from its `type` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Mains,
    Usb,
    Wireless,
    Battery,
}

impl Kind {
    fn from_sysfs(kind: &str) -> Option<Self> {
        Some(match kind {
            "Mains" => Self::Mains,
            "USB" | "USB_DCP" | "USB_CDP" | "USB_ACA" | "USB_C" | "USB_PD" | "USB_PD_DRP"
            | "BrickID" => Self::Usb,
            "Wireless" => Self::Wireless,
            "Battery" => Self::Battery,
            _ => return None,
        })
    }
}

/// The kernel's battery charge status (`status` attribute).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ChargeStatus {
    /// `Unknown`, missing, or a value this HAL does not know.
    #[default]
    Unknown,
    /// `Charging`.
    Charging,
    /// `Discharging`.
    Discharging,
    /// `Not charging`: plugged in but held (for example at a charge limit).
    NotCharging,
    /// `Full`.
    Full,
}

impl FromStr for ChargeStatus {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Not charging" => Self::NotCharging,
            "Full" => Self::Full,
            _ => Self::Unknown,
        })
    }
}

/// The kernel's battery health (`health` attribute).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BatteryHealth {
    /// `Unknown`, missing, or a value this HAL does not know.
    #[default]
    Unknown,
    /// `Good`.
    Good,
    /// `Overheat` or `Hot`.
    Overheat,
    /// `Dead`.
    Dead,
    /// `Over voltage`.
    OverVoltage,
    /// `Cold`.
    Cold,
    /// `Unspecified failure`, `Over current`, a watchdog/safety timer expiry, ...
    Failure,
    /// `Calibration required`.
    Inconsistent,
}

impl FromStr for BatteryHealth {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "Good" | "Warm" | "Cool" => Self::Good,
            "Overheat" | "Hot" => Self::Overheat,
            "Dead" => Self::Dead,
            "Over voltage" => Self::OverVoltage,
            "Cold" => Self::Cold,
            "Unspecified failure"
            | "Over current"
            | "Watchdog timer expire"
            | "Safety timer expire" => Self::Failure,
            "Calibration required" => Self::Inconsistent,
            _ => Self::Unknown,
        })
    }
}

/// The kernel's coarse charge level (`capacity_level` attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapacityLevel {
    /// `Unknown`.
    Unknown,
    /// `Critical`.
    Critical,
    /// `Low`.
    Low,
    /// `Normal`.
    Normal,
    /// `High`.
    High,
    /// `Full`.
    Full,
}

impl FromStr for CapacityLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "Unknown" => Self::Unknown,
            "Critical" => Self::Critical,
            "Low" => Self::Low,
            "Normal" => Self::Normal,
            "High" => Self::High,
            "Full" => Self::Full,
            _ => return Err(()),
        })
    }
}

/// One battery's state. Currents are negative while discharging, as Android expects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Battery {
    /// The sysfs name, such as `BAT0`.
    pub name: String,
    /// The charge status.
    pub status: ChargeStatus,
    /// The health.
    pub health: BatteryHealth,
    /// State of charge, 0..=100.
    pub level: i32,
    /// Voltage now, in microvolts.
    pub voltage_uv: i32,
    /// Current now, in microamps.
    pub current_now_ua: i32,
    /// Average current, in microamps; `None` if the battery does not report one.
    pub current_avg_ua: Option<i32>,
    /// Temperature in tenths of a degree Celsius; 0 if unknown (ACPI batteries).
    pub temp_tenths_c: i32,
    /// Charge cycles; 0 if unknown.
    pub cycle_count: i32,
    /// Charge when full now, in microamp-hours; 0 if unknown.
    pub full_charge_uah: i32,
    /// Charge when full as designed, in microamp-hours; 0 if unknown.
    pub full_design_uah: i32,
    /// Remaining charge, in microamp-hours (derived from energy or level if needed).
    pub charge_counter_uah: i32,
    /// Remaining energy, in nanowatt-hours; `None` if the battery reports charge only.
    pub energy_nwh: Option<i64>,
    /// Chemistry, such as `Li-ion`.
    pub technology: String,
    /// The kernel's coarse level, if reported.
    pub capacity_level: Option<CapacityLevel>,
    /// Seconds until full; `None` if unknown.
    pub time_to_full_s: Option<i64>,
    /// Manufacturer, model and serial number as reported (may be empty or junk).
    pub manufacturer: String,
    /// The model name.
    pub model: String,
    /// The serial number.
    pub serial: String,
    /// Minimum design voltage, in microvolts; 0 if unknown.
    pub voltage_min_design_uv: i32,
}

/// Every power supply at one point in time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// An AC adapter (or, with no battery and no charger at all, mains power) is online.
    pub ac_online: bool,
    /// A USB charger is online.
    pub usb_online: bool,
    /// A wireless charger is online.
    pub wireless_online: bool,
    /// Largest `current_max` of the online chargers, in microamps.
    pub max_charging_current_ua: i32,
    /// Largest `voltage_max` of the online chargers, in microvolts.
    pub max_charging_voltage_uv: i32,
    /// The system battery, if one is present.
    pub battery: Option<Battery>,
}

impl Snapshot {
    /// Reads every supply in `class_dir` (normally `/sys/class/power_supply`).
    ///
    /// The system battery is the first present battery whose `scope` is not `Device`
    /// (a wireless mouse's battery has scope `Device`). A machine with neither a battery
    /// nor any charger is a desktop on mains power, so AC is reported online, as
    /// Android expects for battery-less devices.
    pub fn read(class_dir: &Path) -> Self {
        let mut snapshot = Self::default();
        let mut chargers = 0;
        for dir in pchal::sysfs::entries(class_dir).unwrap_or_default() {
            let Some(kind) = attr::<String>(&dir, "type").as_deref().and_then(Kind::from_sysfs)
            else {
                continue;
            };
            if attr::<String>(&dir, "scope").as_deref() == Some("Device") {
                continue;
            }
            if kind == Kind::Battery {
                if snapshot.battery.is_none() && attr::<i32>(&dir, "present").unwrap_or(1) != 0 {
                    snapshot.battery = Some(read_battery(&dir));
                }
                continue;
            }
            chargers += 1;
            if attr::<i32>(&dir, "online").unwrap_or(0) == 0 {
                continue;
            }
            match kind {
                Kind::Mains => snapshot.ac_online = true,
                Kind::Usb => snapshot.usb_online = true,
                Kind::Wireless => snapshot.wireless_online = true,
                Kind::Battery => unreachable!("batteries are handled above"),
            }
            let current = attr(&dir, "current_max").unwrap_or(0);
            let voltage = attr(&dir, "voltage_max").unwrap_or(0);
            snapshot.max_charging_current_ua = snapshot.max_charging_current_ua.max(current);
            snapshot.max_charging_voltage_uv = snapshot.max_charging_voltage_uv.max(voltage);
        }
        if snapshot.battery.is_none() && chargers == 0 {
            snapshot.ac_online = true;
        }
        snapshot
    }
}

fn read_battery(dir: &Path) -> Battery {
    let status: ChargeStatus = attr(dir, "status").unwrap_or_default();
    let voltage_uv: i32 = attr(dir, "voltage_now").unwrap_or(0);
    let voltage_min_design_uv: i32 = attr(dir, "voltage_min_design").unwrap_or(0);
    // Energy-based batteries (most ACPI laptops) report µWh and µW; convert to charge and
    // current at the design voltage (or the present one).
    let volts_uv =
        i64::from(if voltage_min_design_uv > 0 { voltage_min_design_uv } else { voltage_uv });
    let to_charge = |energy_uwh: i64| -> Option<i32> {
        (volts_uv > 0).then(|| saturate(energy_uwh * 1_000_000 / volts_uv))
    };

    let current_now_ua = attr::<i64>(dir, "current_now")
        .or_else(|| {
            let power_uw = attr::<i64>(dir, "power_now")?;
            (voltage_uv > 0).then(|| power_uw * 1_000_000 / i64::from(voltage_uv))
        })
        .map(|current| signed_current(saturate(current), status))
        .unwrap_or(0);
    let current_avg_ua =
        attr::<i64>(dir, "current_avg").map(|current| signed_current(saturate(current), status));

    let energy_now = attr::<i64>(dir, "energy_now");
    let full_charge_uah =
        attr(dir, "charge_full").or_else(|| to_charge(attr(dir, "energy_full")?)).unwrap_or(0);
    let full_design_uah = attr(dir, "charge_full_design")
        .or_else(|| to_charge(attr(dir, "energy_full_design")?))
        .unwrap_or(0);
    let level = attr::<i32>(dir, "capacity").map_or(0, |level| level.clamp(0, 100));
    // Android's charge counter is the remaining charge, which is the kernel's `charge_now`.
    // The kernel's `charge_counter` is a coulomb counter that only some drivers keep equal
    // to it (max17042 exports its free-running QH register there), so it comes second.
    let charge_counter_uah = attr(dir, "charge_now")
        .or_else(|| attr(dir, "charge_counter"))
        .or_else(|| to_charge(energy_now?))
        .unwrap_or_else(|| saturate(i64::from(full_charge_uah) * i64::from(level) / 100));

    Battery {
        name: dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        status,
        health: attr(dir, "health").unwrap_or_default(),
        level,
        voltage_uv,
        current_now_ua,
        current_avg_ua,
        temp_tenths_c: attr(dir, "temp").unwrap_or(0),
        cycle_count: attr::<i32>(dir, "cycle_count").unwrap_or(0).max(0),
        full_charge_uah,
        full_design_uah,
        charge_counter_uah,
        energy_nwh: energy_now.map(|energy| energy * 1000),
        technology: attr(dir, "technology").unwrap_or_default(),
        capacity_level: attr::<String>(dir, "capacity_level").and_then(|level| level.parse().ok()),
        time_to_full_s: attr(dir, "time_to_full_now"),
        manufacturer: attr(dir, "manufacturer").unwrap_or_default(),
        model: attr(dir, "model_name").unwrap_or_default(),
        serial: attr(dir, "serial_number").unwrap_or_default(),
        voltage_min_design_uv,
    }
}

/// Reads an optional attribute of a supply. Errors (many drivers fail reads with
/// `ENODATA` or `EIO` when a value is unavailable) count as missing.
fn attr<T: FromStr>(dir: &Path, name: &str) -> Option<T> {
    let path: PathBuf = dir.join(name);
    pchal::sysfs::read_optional(&path).unwrap_or_else(|err| {
        debug!("{err}");
        None
    })
}

/// Android wants a negative current while discharging and a positive one while
/// charging; ACPI reports the magnitude only.
fn signed_current(current: i32, status: ChargeStatus) -> i32 {
    match status {
        ChargeStatus::Discharging => -current.saturating_abs(),
        ChargeStatus::Charging => current.saturating_abs(),
        _ => current,
    }
}

fn saturate(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN + 1 } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn supply(root: &Path, name: &str, attrs: &[(&str, &str)]) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("create fixture dir");
        for (key, value) in attrs {
            fs::write(dir.join(key), format!("{value}\n")).expect("write fixture");
        }
    }

    #[test]
    fn desktop_without_supplies_is_on_mains() {
        let root = tempfile::tempdir().expect("create temp dir");
        let snapshot = Snapshot::read(root.path());
        assert!(snapshot.ac_online);
        assert_eq!(snapshot.battery, None);
        // A missing class directory (no POWER_SUPPLY in the kernel) reads the same.
        assert_eq!(Snapshot::read(&root.path().join("missing")), snapshot);
    }

    #[test]
    fn laptop_on_battery_with_energy_counters() {
        let root = tempfile::tempdir().expect("create temp dir");
        supply(root.path(), "AC", &[("type", "Mains"), ("online", "0")]);
        supply(
            root.path(),
            "BAT0",
            &[
                ("type", "Battery"),
                ("present", "1"),
                ("status", "Discharging"),
                ("capacity", "80"),
                ("capacity_level", "Normal"),
                ("voltage_now", "12000000"),
                ("voltage_min_design", "11400000"),
                ("power_now", "6000000"),
                ("energy_now", "45600000"),
                ("energy_full", "57000000"),
                ("energy_full_design", "57000000"),
                ("technology", "Li-ion"),
                ("model_name", "5B10W13930"),
                ("cycle_count", "42"),
            ],
        );
        supply(root.path(), "hidpp_battery_0", &[("type", "Battery"), ("scope", "Device")]);

        let snapshot = Snapshot::read(root.path());
        assert!(!snapshot.ac_online && !snapshot.usb_online && !snapshot.wireless_online);
        let battery = snapshot.battery.expect("BAT0 found");
        assert_eq!(battery.name, "BAT0");
        assert_eq!(battery.status, ChargeStatus::Discharging);
        assert_eq!(battery.level, 80);
        // 6 W at 12 V, negative while discharging.
        assert_eq!(battery.current_now_ua, -500_000);
        // 45.6 Wh at the 11.4 V design voltage.
        assert_eq!(battery.charge_counter_uah, 4_000_000);
        assert_eq!(battery.full_charge_uah, 5_000_000);
        assert_eq!(battery.energy_nwh, Some(45_600_000_000));
        assert_eq!(battery.capacity_level, Some(CapacityLevel::Normal));
        assert_eq!(battery.cycle_count, 42);
    }

    #[test]
    fn charging_on_usb_c_with_charge_counters() {
        let root = tempfile::tempdir().expect("create temp dir");
        supply(
            root.path(),
            "ucsi-source-psy-USBC000:001",
            &[
                ("type", "USB"),
                ("online", "1"),
                ("current_max", "3000000"),
                ("voltage_max", "20000000"),
            ],
        );
        supply(
            root.path(),
            "battery",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("health", "Good"),
                ("capacity", "50"),
                ("current_now", "-1500000"),
                ("charge_full", "4000000"),
                ("temp", "250"),
            ],
        );
        let snapshot = Snapshot::read(root.path());
        assert!(snapshot.usb_online && !snapshot.ac_online);
        assert_eq!(snapshot.max_charging_current_ua, 3_000_000);
        assert_eq!(snapshot.max_charging_voltage_uv, 20_000_000);
        let battery = snapshot.battery.expect("battery found");
        assert_eq!(battery.health, BatteryHealth::Good);
        assert_eq!(battery.current_now_ua, 1_500_000);
        // No charge_now: derived from the level.
        assert_eq!(battery.charge_counter_uah, 2_000_000);
        assert_eq!(battery.temp_tenths_c, 250);
        assert_eq!(battery.energy_nwh, None);
    }

    #[test]
    fn fuel_gauge_charge_now_wins_over_coulomb_counter() {
        // The Galaxy Tab S7's MAX77705 gauge (max17042 driver) on a USB charger.
        let root = tempfile::tempdir().expect("create temp dir");
        supply(root.path(), "max77705-charger", &[("type", "USB"), ("online", "1")]);
        supply(
            root.path(),
            "max170xx_battery",
            &[
                ("type", "Battery"),
                ("present", "1"),
                ("status", "Full"),
                ("capacity", "98"),
                ("charge_now", "7310000"),
                ("charge_counter", "55325000"),
                ("charge_full", "7257500"),
                ("current_now", "-170312"),
                ("temp", "337"),
                ("cycle_count", "276"),
            ],
        );
        let battery = Snapshot::read(root.path()).battery.expect("battery found");
        assert_eq!(battery.charge_counter_uah, 7_310_000);
        // Neither charging nor discharging: the sign stays as the kernel reports it.
        assert_eq!(battery.current_now_ua, -170_312);
        assert_eq!(battery.cycle_count, 276);
    }

    #[test]
    fn empty_battery_bay_is_not_a_battery() {
        let root = tempfile::tempdir().expect("create temp dir");
        supply(root.path(), "AC", &[("type", "Mains"), ("online", "1")]);
        supply(root.path(), "BAT1", &[("type", "Battery"), ("present", "0")]);
        let snapshot = Snapshot::read(root.path());
        assert!(snapshot.ac_online);
        assert_eq!(snapshot.battery, None);
    }

    #[test]
    fn kernel_strings_parse() {
        assert_eq!("Not charging".parse(), Ok(ChargeStatus::NotCharging));
        assert_eq!("Bogus".parse(), Ok(ChargeStatus::Unknown));
        assert_eq!("Over voltage".parse(), Ok(BatteryHealth::OverVoltage));
        assert_eq!("Hot".parse(), Ok(BatteryHealth::Overheat));
        assert_eq!("Critical".parse(), Ok(CapacityLevel::Critical));
        assert!("bogus".parse::<CapacityLevel>().is_err());
    }
}
