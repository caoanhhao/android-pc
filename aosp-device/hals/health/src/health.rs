//! The IHealth implementation over [`crate::supply`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use android_hardware_health::aidl::android::hardware::health::{
    BatteryCapacityLevel::BatteryCapacityLevel,
    BatteryChargingPolicy::BatteryChargingPolicy,
    BatteryChargingState::BatteryChargingState,
    BatteryHealth::BatteryHealth as AidlHealth,
    BatteryHealthData::BatteryHealthData,
    BatteryPartStatus::BatteryPartStatus,
    BatteryStatus::BatteryStatus,
    DiskStats::DiskStats,
    HealthInfo::{HealthInfo, BATTERY_CHARGE_TIME_TO_FULL_NOW_SECONDS_UNSUPPORTED},
    HingeInfo::HingeInfo,
    IHealth::IHealth,
    IHealthInfoCallback::IHealthInfoCallback,
    StorageInfo::StorageInfo,
};
use binder::{ExceptionCode, Interface, Status, Strong};
use log::{debug, info};
use pchal::Callbacks;

use crate::supply::{Battery, BatteryHealth, CapacityLevel, ChargeStatus, Snapshot};

/// Level reported when there is no battery. Android treats a battery-less device as
/// plugged in, so nothing reacts to the level, but apps that show it see "full" rather
/// than an alarming 0 %.
const NO_BATTERY_LEVEL: i32 = 100;

/// The health HAL. Cloning shares the same state, so the uevent thread can hold a copy.
#[derive(Debug, Clone)]
pub struct Health {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    class_dir: PathBuf,
    callbacks: Callbacks<dyn IHealthInfoCallback>,
}

impl Interface for Health {}

impl Health {
    /// Serves the power supplies in `class_dir` (normally `/sys/class/power_supply`).
    pub fn new(class_dir: &Path) -> Self {
        let health = Self {
            inner: Arc::new(Inner { class_dir: class_dir.to_owned(), callbacks: Callbacks::new() }),
        };
        info!("{}", describe(&health.snapshot()));
        health
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::read(&self.inner.class_dir)
    }

    /// Whether the machine has a battery, so the state changes without uevents too.
    pub fn has_battery(&self) -> bool {
        self.snapshot().battery.is_some()
    }

    /// Re-reads the supplies and sends the result to every registered callback.
    pub fn notify(&self) {
        let info = health_info(&self.snapshot());
        debug!("notify: {}", describe_info(&info));
        self.inner.callbacks.notify(|()| true, |callback| callback.healthInfoChanged(&info));
    }

    fn battery(&self) -> binder::Result<Battery> {
        self.snapshot().battery.ok_or_else(unsupported)
    }
}

impl IHealth for Health {
    fn registerCallback(&self, callback: &Strong<dyn IHealthInfoCallback>) -> binder::Result<()> {
        self.inner.callbacks.add(callback, ())?;
        // A new client gets the current state at once, as libhealth does.
        let info = health_info(&self.snapshot());
        if let Err(status) = callback.healthInfoChanged(&info) {
            debug!("initial healthInfoChanged failed: {status:?}");
        }
        Ok(())
    }

    fn unregisterCallback(&self, callback: &Strong<dyn IHealthInfoCallback>) -> binder::Result<()> {
        self.inner.callbacks.remove(callback)
    }

    fn update(&self) -> binder::Result<()> {
        self.notify();
        Ok(())
    }

    fn getChargeCounterUah(&self) -> binder::Result<i32> {
        Ok(self.battery()?.charge_counter_uah)
    }

    fn getCurrentNowMicroamps(&self) -> binder::Result<i32> {
        Ok(self.battery()?.current_now_ua)
    }

    fn getCurrentAverageMicroamps(&self) -> binder::Result<i32> {
        self.battery()?.current_avg_ua.ok_or_else(unsupported)
    }

    fn getCapacity(&self) -> binder::Result<i32> {
        Ok(self.snapshot().battery.map_or(NO_BATTERY_LEVEL, |battery| battery.level))
    }

    fn getEnergyCounterNwh(&self) -> binder::Result<i64> {
        self.battery()?.energy_nwh.ok_or_else(unsupported)
    }

    fn getChargeStatus(&self) -> binder::Result<BatteryStatus> {
        Ok(self.snapshot().battery.map_or(BatteryStatus::UNKNOWN, |battery| status(battery.status)))
    }

    fn getStorageInfo(&self) -> binder::Result<Vec<StorageInfo>> {
        // eMMC/UFS lifetime estimates; PC NVMe/SATA disks report SMART data instead.
        Err(unsupported())
    }

    fn getDiskStats(&self) -> binder::Result<Vec<DiskStats>> {
        Err(unsupported())
    }

    fn getHealthInfo(&self) -> binder::Result<HealthInfo> {
        Ok(health_info(&self.snapshot()))
    }

    fn setChargingPolicy(&self, _policy: BatteryChargingPolicy) -> binder::Result<()> {
        Err(unsupported())
    }

    fn getChargingPolicy(&self) -> binder::Result<BatteryChargingPolicy> {
        Ok(BatteryChargingPolicy::DEFAULT)
    }

    fn getBatteryHealthData(&self) -> binder::Result<BatteryHealthData> {
        // Required from vendor API level 202604; without a battery every field stays at
        // its "unknown" value.
        Ok(self.snapshot().battery.map(|battery| health_data(&battery)).unwrap_or_default())
    }

    fn getHingeInfo(&self) -> binder::Result<Vec<HingeInfo>> {
        Err(unsupported())
    }
}

fn unsupported() -> Status {
    Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None)
}

/// Builds the HealthInfo Android's BatteryService consumes.
pub fn health_info(snapshot: &Snapshot) -> HealthInfo {
    let mut info = HealthInfo {
        chargerAcOnline: snapshot.ac_online,
        chargerUsbOnline: snapshot.usb_online,
        chargerWirelessOnline: snapshot.wireless_online,
        chargerDockOnline: false,
        maxChargingCurrentMicroamps: snapshot.max_charging_current_ua,
        maxChargingVoltageMicrovolts: snapshot.max_charging_voltage_uv,
        // Battery-less devices must report UNKNOWN (VTS IsBatteryStatusCorrect).
        batteryStatus: BatteryStatus::UNKNOWN,
        batteryHealth: AidlHealth::UNKNOWN,
        batteryPresent: false,
        batteryLevel: NO_BATTERY_LEVEL,
        batteryCapacityLevel: BatteryCapacityLevel::UNSUPPORTED,
        batteryChargeTimeToFullNowSeconds: BATTERY_CHARGE_TIME_TO_FULL_NOW_SECONDS_UNSUPPORTED
            .into(),
        chargingState: BatteryChargingState::INVALID,
        chargingPolicy: BatteryChargingPolicy::DEFAULT,
        ..Default::default()
    };
    let Some(battery) = &snapshot.battery else {
        return info;
    };

    info.batteryPresent = true;
    info.batteryStatus = status(battery.status);
    info.batteryHealth = health(battery.health);
    info.batteryLevel = battery.level;
    info.batteryVoltageMillivolts = battery.voltage_uv / 1000;
    info.batteryTemperatureTenthsCelsius = battery.temp_tenths_c;
    info.batteryCurrentMicroamps = battery.current_now_ua;
    info.batteryCurrentAverageMicroamps = battery.current_avg_ua.unwrap_or(0);
    info.batteryCycleCount = battery.cycle_count;
    info.batteryFullChargeUah = battery.full_charge_uah;
    info.batteryChargeCounterUah = battery.charge_counter_uah.max(1);
    info.batteryTechnology = battery.technology.clone();
    info.batteryCapacityLevel =
        battery.capacity_level.map_or(BatteryCapacityLevel::UNSUPPORTED, capacity_level);
    info.batteryChargeTimeToFullNowSeconds = battery.time_to_full_s.unwrap_or(-1);
    // VTS: 0 (unknown) or between 100 mAh and 100,000 mAh.
    info.batteryFullChargeDesignCapacityUah =
        if (100_000..100_000_000).contains(&battery.full_design_uah) {
            battery.full_design_uah
        } else {
            0
        };
    info.chargingState = BatteryChargingState::NORMAL;
    info.batteryHealthData = Some(health_data(battery));
    info
}

fn health_data(battery: &Battery) -> BatteryHealthData {
    let state_of_health = if battery.full_design_uah > 0 && battery.full_charge_uah > 0 {
        (i64::from(battery.full_charge_uah) * 100 / i64::from(battery.full_design_uah)).min(100)
    } else {
        0
    };
    BatteryHealthData {
        batteryManufacturingDateSeconds: 0,
        batteryFirstUsageSeconds: 0,
        batteryStateOfHealth: state_of_health,
        batterySerialNumber: serial_number(&battery.serial),
        batteryPartStatus: BatteryPartStatus::UNSUPPORTED,
        batteryManufacturer: printable(&battery.manufacturer),
        batteryModelName: printable(&battery.model),
        // VTS: 0 (unknown) or at least 2 V.
        batteryVoltageMinDesignUv: if battery.voltage_min_design_uv >= 2_000_000 {
            battery.voltage_min_design_uv.into()
        } else {
            0
        },
    }
}

/// A manufacturer or model string Android accepts: at least two printable characters.
fn printable(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() >= 2 && value.bytes().all(|byte| byte.is_ascii_graphic() || byte == b' '))
        .then(|| value.to_owned())
}

/// A serial number Android accepts: at least four ASCII letters and digits.
fn serial_number(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() >= 4 && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then(|| value.to_owned())
}

fn status(status: ChargeStatus) -> BatteryStatus {
    match status {
        ChargeStatus::Unknown => BatteryStatus::UNKNOWN,
        ChargeStatus::Charging => BatteryStatus::CHARGING,
        ChargeStatus::Discharging => BatteryStatus::DISCHARGING,
        ChargeStatus::NotCharging => BatteryStatus::NOT_CHARGING,
        ChargeStatus::Full => BatteryStatus::FULL,
    }
}

fn health(health: BatteryHealth) -> AidlHealth {
    match health {
        BatteryHealth::Unknown => AidlHealth::UNKNOWN,
        BatteryHealth::Good => AidlHealth::GOOD,
        BatteryHealth::Overheat => AidlHealth::OVERHEAT,
        BatteryHealth::Dead => AidlHealth::DEAD,
        BatteryHealth::OverVoltage => AidlHealth::OVER_VOLTAGE,
        BatteryHealth::Cold => AidlHealth::COLD,
        BatteryHealth::Failure => AidlHealth::UNSPECIFIED_FAILURE,
        BatteryHealth::Inconsistent => AidlHealth::INCONSISTENT,
    }
}

fn capacity_level(level: CapacityLevel) -> BatteryCapacityLevel {
    match level {
        CapacityLevel::Unknown => BatteryCapacityLevel::UNKNOWN,
        CapacityLevel::Critical => BatteryCapacityLevel::CRITICAL,
        CapacityLevel::Low => BatteryCapacityLevel::LOW,
        CapacityLevel::Normal => BatteryCapacityLevel::NORMAL,
        CapacityLevel::High => BatteryCapacityLevel::HIGH,
        CapacityLevel::Full => BatteryCapacityLevel::FULL,
    }
}

/// One log line describing the supplies.
fn describe(snapshot: &Snapshot) -> String {
    describe_info(&health_info(snapshot))
}

fn describe_info(info: &HealthInfo) -> String {
    format!(
        "ac={} usb={} wireless={} battery present={} level={} status={:?} current={}uA",
        info.chargerAcOnline,
        info.chargerUsbOnline,
        info.chargerWirelessOnline,
        info.batteryPresent,
        info.batteryLevel,
        info.batteryStatus,
        info.batteryCurrentMicroamps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_less_desktop() {
        let info = health_info(&Snapshot { ac_online: true, ..Default::default() });
        assert!(info.chargerAcOnline);
        assert!(!info.batteryPresent);
        assert_eq!(info.batteryStatus, BatteryStatus::UNKNOWN);
        assert_eq!(info.batteryLevel, NO_BATTERY_LEVEL);
        assert_eq!(info.batteryChargeTimeToFullNowSeconds, -1);
        assert!(info.batteryHealthData.is_none());
    }

    #[test]
    fn battery_fields_meet_vts_ranges() {
        let battery = Battery {
            status: ChargeStatus::Charging,
            health: BatteryHealth::Good,
            level: 64,
            charge_counter_uah: 0,
            full_charge_uah: 4_500_000,
            full_design_uah: 5_000_000,
            serial: "12".into(),
            manufacturer: "SMP".into(),
            voltage_min_design_uv: 1_000,
            ..Default::default()
        };
        let info = health_info(&Snapshot {
            ac_online: true,
            battery: Some(battery),
            ..Default::default()
        });
        assert!(info.batteryPresent);
        assert_eq!(info.batteryStatus, BatteryStatus::CHARGING);
        assert!(info.batteryChargeCounterUah > 0);
        assert_eq!(info.batteryFullChargeDesignCapacityUah, 5_000_000);
        let data = info.batteryHealthData.expect("health data with a battery");
        assert_eq!(data.batteryStateOfHealth, 90);
        assert_eq!(data.batterySerialNumber, None);
        assert_eq!(data.batteryManufacturer.as_deref(), Some("SMP"));
        assert_eq!(data.batteryVoltageMinDesignUv, 0);
    }

    #[test]
    fn strings_are_filtered() {
        assert_eq!(printable(" LGC "), Some("LGC".into()));
        assert_eq!(printable("x"), None);
        assert_eq!(printable("bad\u{1}"), None);
        assert_eq!(serial_number("A1B2C3"), Some("A1B2C3".into()));
        assert_eq!(serial_number("A1-B2"), None);
    }
}
