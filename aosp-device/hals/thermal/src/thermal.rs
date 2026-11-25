//! The IThermal implementation over [`crate::sensors`].

use std::sync::{Arc, Mutex};

use android_hardware_thermal::aidl::android::hardware::thermal::{
    CoolingDevice::CoolingDevice as AidlCoolingDevice, CoolingType::CoolingType,
    ICoolingDeviceChangedCallback::ICoolingDeviceChangedCallback, IThermal::IThermal,
    IThermalChangedCallback::IThermalChangedCallback, Temperature::Temperature,
    TemperatureThreshold::TemperatureThreshold, TemperatureType::TemperatureType,
    ThrottlingSeverity::ThrottlingSeverity,
};
use binder::{ExceptionCode, Interface, Status, Strong};
use log::info;
use pchal::Callbacks;

use crate::sensors::{CoolingDevice, CoolingKind, Sensor, SensorKind, Sensors, Severity};

/// The thermal HAL. Cloning shares the same state, so the poll thread can hold a copy.
#[derive(Debug, Clone)]
pub struct Thermal {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    /// Filter: the temperature type asked for, `None` for every type.
    thermal_callbacks: Callbacks<dyn IThermalChangedCallback, Option<TemperatureType>>,
    cooling_callbacks: Callbacks<dyn ICoolingDeviceChangedCallback, CoolingType>,
}

/// The sensors and what the previous poll saw of them, to notify only changes.
#[derive(Debug)]
struct State {
    sensors: Sensors,
    severities: Vec<Severity>,
    cooling: Vec<Option<i64>>,
}

impl State {
    fn new(sensors: Sensors) -> Self {
        Self {
            severities: vec![Severity::None; sensors.sensors().len()],
            cooling: sensors.cooling_devices().iter().map(CoolingDevice::state).collect(),
            sensors,
        }
    }
}

impl Interface for Thermal {}

impl Thermal {
    /// Serves `sensors`.
    pub fn new(sensors: Sensors) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::new(sensors)),
                thermal_callbacks: Callbacks::new(),
                cooling_callbacks: Callbacks::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().expect("thermal state lock poisoned")
    }

    /// Picks up sensors whose drivers loaded late, reads every sensor and cooling device
    /// and notifies the callbacks of severity and cooling state changes. Returns the highest severity, so the caller can poll faster
    /// while something is hot.
    pub fn poll(&self) -> Severity {
        let inner = &self.inner;
        let (changed_sensors, changed_cooling, highest) = {
            let mut state = self.lock();
            if state.sensors.rediscover_if_changed() {
                let sensors = std::mem::take(&mut state.sensors);
                *state = State::new(sensors);
            }
            let state = &mut *state;
            let values = state.sensors.read();
            let states: Vec<Option<i64>> =
                state.sensors.cooling_devices().iter().map(CoolingDevice::state).collect();

            let mut changed_sensors = Vec::new();
            let mut highest = Severity::None;
            for (index, (sensor, value)) in state.sensors.sensors().iter().zip(&values).enumerate()
            {
                let Some(value) = *value else { continue };
                let severity = sensor.severity(value);
                highest = highest.max(severity);
                if severity != state.severities[index] {
                    state.severities[index] = severity;
                    changed_sensors.push(temperature(sensor, value));
                }
            }
            let mut changed_cooling = Vec::new();
            let devices = state.sensors.cooling_devices().iter().zip(&states);
            for ((device, &now), old) in devices.zip(&mut state.cooling) {
                if let Some(value) = now.filter(|_| now != *old) {
                    *old = now;
                    changed_cooling.push(cooling_device(device, value));
                }
            }
            (changed_sensors, changed_cooling, highest)
        };

        for temperature in changed_sensors {
            info!(
                "{} at {:.1} C: {:?}",
                temperature.name, temperature.value, temperature.throttlingStatus
            );
            inner.thermal_callbacks.notify(
                |filter| filter.is_none_or(|kind| kind == temperature.r#type),
                |callback| callback.notifyThrottling(&temperature),
            );
        }
        for device in changed_cooling {
            inner.cooling_callbacks.notify(
                |&kind| kind == device.r#type,
                |callback| callback.notifyCoolingDeviceChanged(&device),
            );
        }
        highest
    }

    fn temperatures(&self, kind: Option<TemperatureType>) -> Vec<Temperature> {
        let state = self.lock();
        state
            .sensors
            .sensors()
            .iter()
            .zip(state.sensors.read())
            .filter(|(sensor, _)| kind.is_none_or(|kind| temperature_type(sensor.kind) == kind))
            .filter_map(|(sensor, value)| Some(temperature(sensor, value?)))
            .collect()
    }

    fn thresholds(&self, kind: Option<TemperatureType>) -> Vec<TemperatureThreshold> {
        self.lock()
            .sensors
            .sensors()
            .iter()
            .filter(|sensor| kind.is_none_or(|kind| temperature_type(sensor.kind) == kind))
            .map(|sensor| TemperatureThreshold {
                r#type: temperature_type(sensor.kind),
                name: sensor.name.clone(),
                hotThrottlingThresholds: sensor
                    .thresholds
                    .iter()
                    .map(|threshold| threshold.unwrap_or(f32::NAN))
                    .collect(),
                coldThrottlingThresholds: Vec::new(),
            })
            .collect()
    }

    fn cooling(&self, kind: Option<CoolingType>) -> Vec<AidlCoolingDevice> {
        self.lock()
            .sensors
            .cooling_devices()
            .iter()
            .filter(|device| kind.is_none_or(|kind| cooling_type(device.kind) == kind))
            .filter_map(|device| Some(cooling_device(device, device.state()?)))
            .collect()
    }
}

impl IThermal for Thermal {
    fn getCoolingDevices(&self) -> binder::Result<Vec<AidlCoolingDevice>> {
        Ok(self.cooling(None))
    }

    fn getCoolingDevicesWithType(
        &self,
        kind: CoolingType,
    ) -> binder::Result<Vec<AidlCoolingDevice>> {
        Ok(self.cooling(Some(kind)))
    }

    fn getTemperatures(&self) -> binder::Result<Vec<Temperature>> {
        Ok(self.temperatures(None))
    }

    fn getTemperaturesWithType(&self, kind: TemperatureType) -> binder::Result<Vec<Temperature>> {
        Ok(self.temperatures(Some(kind)))
    }

    fn getTemperatureThresholds(&self) -> binder::Result<Vec<TemperatureThreshold>> {
        Ok(self.thresholds(None))
    }

    fn getTemperatureThresholdsWithType(
        &self,
        kind: TemperatureType,
    ) -> binder::Result<Vec<TemperatureThreshold>> {
        Ok(self.thresholds(Some(kind)))
    }

    fn registerThermalChangedCallback(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
    ) -> binder::Result<()> {
        self.inner.thermal_callbacks.add(callback, None)
    }

    fn registerThermalChangedCallbackWithType(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
        kind: TemperatureType,
    ) -> binder::Result<()> {
        self.inner.thermal_callbacks.add(callback, Some(kind))
    }

    fn unregisterThermalChangedCallback(
        &self,
        callback: &Strong<dyn IThermalChangedCallback>,
    ) -> binder::Result<()> {
        self.inner.thermal_callbacks.remove(callback)
    }

    fn registerCoolingDeviceChangedCallbackWithType(
        &self,
        callback: &Strong<dyn ICoolingDeviceChangedCallback>,
        kind: CoolingType,
    ) -> binder::Result<()> {
        self.inner.cooling_callbacks.add(callback, kind)
    }

    fn unregisterCoolingDeviceChangedCallback(
        &self,
        callback: &Strong<dyn ICoolingDeviceChangedCallback>,
    ) -> binder::Result<()> {
        self.inner.cooling_callbacks.remove(callback)
    }

    fn forecastSkinTemperature(&self, _forecast_seconds: i32) -> binder::Result<f32> {
        Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }
}

fn temperature(sensor: &Sensor, value: f32) -> Temperature {
    Temperature {
        r#type: temperature_type(sensor.kind),
        name: sensor.name.clone(),
        value,
        throttlingStatus: throttling_severity(sensor.severity(value)),
    }
}

fn cooling_device(device: &CoolingDevice, state: i64) -> AidlCoolingDevice {
    AidlCoolingDevice {
        r#type: cooling_type(device.kind),
        name: device.name.clone(),
        value: state,
        // Power figures: not measured on a PC.
        powerLimitMw: 0,
        powerMw: 0,
        timeWindowMs: 0,
    }
}

fn temperature_type(kind: SensorKind) -> TemperatureType {
    match kind {
        SensorKind::Cpu => TemperatureType::CPU,
        SensorKind::Gpu => TemperatureType::GPU,
        SensorKind::Skin => TemperatureType::SKIN,
        SensorKind::Battery => TemperatureType::BATTERY,
        SensorKind::Wifi => TemperatureType::WIFI,
        SensorKind::Unknown => TemperatureType::UNKNOWN,
    }
}

fn cooling_type(kind: CoolingKind) -> CoolingType {
    match kind {
        CoolingKind::Fan => CoolingType::FAN,
        CoolingKind::Cpu => CoolingType::CPU,
        CoolingKind::Gpu => CoolingType::GPU,
        CoolingKind::Wifi => CoolingType::WIFI,
        CoolingKind::Component => CoolingType::COMPONENT,
    }
}

fn throttling_severity(severity: Severity) -> ThrottlingSeverity {
    match severity {
        Severity::None => ThrottlingSeverity::NONE,
        Severity::Light => ThrottlingSeverity::LIGHT,
        Severity::Moderate => ThrottlingSeverity::MODERATE,
        Severity::Severe => ThrottlingSeverity::SEVERE,
        Severity::Critical => ThrottlingSeverity::CRITICAL,
        Severity::Emergency => ThrottlingSeverity::EMERGENCY,
        Severity::Shutdown => ThrottlingSeverity::SHUTDOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::Path;

    fn write(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create dir");
        fs::write(path, format!("{value}\n")).expect("write fixture");
    }

    #[test]
    fn reports_cpu_and_skin_with_thresholds() -> binder::Result<()> {
        let root = tempfile::tempdir().expect("create temp dir");
        let zone = root.path().join("thermal/thermal_zone0");
        write(&zone.join("type"), "x86_pkg_temp");
        write(&zone.join("temp"), "96000");
        let thermal = Thermal::new(Sensors::discover(
            &root.path().join("thermal"),
            &root.path().join("hwmon"),
        ));

        let skin = thermal.getTemperaturesWithType(TemperatureType::SKIN)?;
        assert_eq!(skin.len(), 1);
        assert_eq!(skin[0].value, 96.0);
        assert_eq!(skin[0].throttlingStatus, ThrottlingSeverity::MODERATE);

        let thresholds = thermal.getTemperatureThresholdsWithType(TemperatureType::SKIN)?;
        assert_eq!(thresholds.len(), 1);
        assert_eq!(thresholds[0].name, skin[0].name);
        assert_eq!(thresholds[0].hotThrottlingThresholds.len(), 7);
        assert!(thresholds[0].hotThrottlingThresholds[0].is_nan());

        assert_eq!(thermal.getTemperatures()?.len(), 2);
        assert!(thermal.getTemperaturesWithType(TemperatureType::GPU)?.is_empty());
        assert_eq!(thermal.poll(), Severity::Moderate);
        Ok(())
    }
}
