//! Temperature sensors and cooling devices of a PC, from sysfs.
//!
//! Sources:
//! - `/sys/class/thermal/thermal_zone*`: `x86_pkg_temp` (CPU package), ACPI zones
//!   (`acpitz`), `iwlwifi_*` (Wi-Fi), with their trip points;
//! - `/sys/class/hwmon/hwmon*` drivers listed in [`hwmon_kind`]: `coretemp`/`k10temp` (CPU
//!   packages, with TjMax), GPU drivers, NVMe, and their fans (`fanN_input`, in RPM);
//! - `/sys/class/thermal/cooling_device*`: fans, ACPI processor throttling, powerclamp.
//!
//! Qualcomm SoC zones (the Galaxy Tab S7, docs/gts7l-hal/power.md) are typed by name:
//! `cpuN-*-thermal`/`clusterN-thermal` are CPUs, `gpu-*-thermal` GPUs, `batt-thermal` and
//! the fuel gauge's `max170xx_battery` the battery, `wlan-thermal` Wi-Fi, and the display
//! backlight thermistor `blkt-thermal`, behind the front glass, is the skin sensor.
//!
//! A PC has no skin sensor, so a virtual `SKIN` sensor reports the hottest CPU package:
//! that is what limits sustained performance on a desktop or laptop, and Android derives
//! its thermal status and headroom from the `SKIN` sensor.
//!
//! This module only reads sysfs and knows nothing about AIDL, so its tests run on the
//! host (`android.hardware.thermal-service.pc_sensors_test`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::{debug, info};

/// Sensor categories, matching the AIDL `TemperatureType`s this HAL reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    /// A CPU package.
    Cpu,
    /// A GPU.
    Gpu,
    /// A skin sensor: a real one, or the virtual one of a PC.
    Skin,
    /// The battery.
    Battery,
    /// A Wi-Fi adapter.
    Wifi,
    /// Anything else (ACPI zones, disks).
    Unknown,
}

/// Throttling severities, in the order of the AIDL `ThrottlingSeverity` values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// No throttling.
    #[default]
    None,
    /// Light throttling; the user does not notice.
    Light,
    /// Moderate throttling.
    Moderate,
    /// Severe throttling; performance drops noticeably.
    Severe,
    /// The platform has done everything to reduce power.
    Critical,
    /// Key components may shut down.
    Emergency,
    /// Android shuts the device down (for CPU, GPU and skin sensors).
    Shutdown,
}

impl Severity {
    /// Every severity, in ascending order; the index is the AIDL value.
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Light,
        Self::Moderate,
        Self::Severe,
        Self::Critical,
        Self::Emergency,
        Self::Shutdown,
    ];
}

/// Hot thresholds in °C, indexed by [`Severity`] (index 0, `None`, is never set).
pub type Thresholds = [Option<f32>; 7];

/// Default TjMax (°C) when a CPU sensor does not report one; Intel desktop parts use 100.
const DEFAULT_TJMAX: f32 = 100.0;
/// ACPI trip points below this (°C) are firmware placeholders: the Z790 board's
/// `acpitz` reports a 20.8 °C critical trip while reading a fixed 16.8 °C.
const MIN_PLAUSIBLE_TRIP: f32 = 40.0;

/// GPU thresholds (°C) for a GPU sensor whose driver reports none: nouveau on GSP-RM gives
/// only `temp1_input`, since GSP-RM runs the fans and thermal slowdown itself. NVIDIA rates
/// desktop Ada GPUs (RTX 4070) for 90 °C, where they slow down on their own: that is SEVERE.
/// SHUTDOWN is left to the GPU's own thermal shutdown, as for CPUs.
const GPU_DEFAULT_THRESHOLDS: Thresholds =
    [None, Some(80.0), Some(85.0), Some(90.0), Some(95.0), Some(100.0), None];

/// Skin thresholds (°C) for a skin thermistor without trip points, the values phones and
/// tablets use for their surface. The Tab S7's backlight thermistor reads 34-35 °C idle on
/// a charger.
const SKIN_DEFAULT_THRESHOLDS: Thresholds =
    [None, Some(39.0), Some(43.0), Some(45.0), Some(47.0), Some(52.0), Some(55.0)];

/// Battery thresholds (°C) for a battery thermistor without trip points: Li-ion cells
/// stop charging around 45-50 °C and must not exceed 60 °C. SHUTDOWN is left to the
/// charger and the kernel.
const BATTERY_DEFAULT_THRESHOLDS: Thresholds =
    [None, None, Some(45.0), Some(50.0), Some(55.0), Some(60.0), None];

/// CPU thresholds relative to TjMax. A CPU at TjMax is already throttling itself, which
/// Android calls SEVERE. Readings never exceed TjMax (the sensor counts down to it), so
/// the higher severities only mark the order the framework expects; SHUTDOWN, which makes
/// Android power off, is left to the CPU's own THERMTRIP.
fn cpu_thresholds(tjmax: f32) -> Thresholds {
    [
        None,
        Some(tjmax - 10.0),
        Some(tjmax - 5.0),
        Some(tjmax),
        Some(tjmax + 1.0),
        Some(tjmax + 3.0),
        Some(tjmax + 5.0),
    ]
}

/// Where a sensor's value comes from.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// A millidegree Celsius attribute (`thermal_zoneN/temp`, `hwmonN/tempM_input`).
    File(PathBuf),
    /// The maximum of other sensors (by index), for the virtual skin sensor.
    MaxOf(Vec<usize>),
}

/// One temperature sensor.
#[derive(Debug, Clone, PartialEq)]
pub struct Sensor {
    /// The unique name Android shows, such as `CPU0` or `acpitz`.
    pub name: String,
    /// Its category.
    pub kind: SensorKind,
    /// Hot thresholds.
    pub thresholds: Thresholds,
    source: Source,
}

impl Sensor {
    /// The severity at temperature `celsius`: the highest one whose threshold it reached.
    pub fn severity(&self, celsius: f32) -> Severity {
        Severity::ALL
            .into_iter()
            .zip(self.thresholds)
            .rev()
            .find(|(_, threshold)| threshold.is_some_and(|threshold| celsius >= threshold))
            .map_or(Severity::None, |(severity, _)| severity)
    }
}

/// A cooling device category, matching the AIDL `CoolingType`s this HAL reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoolingKind {
    /// A fan.
    Fan,
    /// CPU throttling (ACPI processor states, powerclamp, TCC offset, cpufreq).
    Cpu,
    /// GPU throttling (devfreq).
    Gpu,
    /// A Wi-Fi adapter's transmit power.
    Wifi,
    /// Anything else.
    Component,
}

/// One cooling device: `cur_state` between 0 and `max_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoolingDevice {
    /// The unique name, such as `fan-0` or `processor-5`.
    pub name: String,
    /// Its category.
    pub kind: CoolingKind,
    state: PathBuf,
}

impl CoolingDevice {
    /// The current cooling state, or `None` if it cannot be read.
    pub fn state(&self) -> Option<i64> {
        read_attr(&self.state)
    }
}

/// Every sensor and cooling device found at startup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sensors {
    sensors: Vec<Sensor>,
    cooling: Vec<CoolingDevice>,
    thermal_dir: PathBuf,
    hwmon_dir: PathBuf,
    /// The class directory entries the sensors were found in.
    layout: Vec<PathBuf>,
}

impl Sensors {
    /// Probes `thermal_dir` (`/sys/class/thermal`) and `hwmon_dir` (`/sys/class/hwmon`).
    pub fn discover(thermal_dir: &Path, hwmon_dir: &Path) -> Self {
        let mut sensors = Vec::new();
        for (name, sensor_kind, path, tjmax) in hwmon_sensors(hwmon_dir) {
            let thresholds = match (sensor_kind, tjmax) {
                (SensorKind::Cpu, tjmax) => cpu_thresholds(tjmax.unwrap_or(DEFAULT_TJMAX)),
                (SensorKind::Gpu, _) => Some(hwmon_thresholds(&path))
                    .filter(|thresholds| thresholds.iter().any(Option::is_some))
                    .unwrap_or(GPU_DEFAULT_THRESHOLDS),
                _ => hwmon_thresholds(&path),
            };
            sensors.push(Sensor {
                name,
                kind: sensor_kind,
                thresholds,
                source: Source::File(path),
            });
        }
        let have_cpu = sensors.iter().any(|sensor| sensor.kind == SensorKind::Cpu);
        sensors.extend(zone_sensors(thermal_dir, have_cpu));

        let have_skin = sensors.iter().any(|sensor| sensor.kind == SensorKind::Skin);
        let cpus: Vec<usize> = sensors
            .iter()
            .enumerate()
            .filter(|(_, sensor)| sensor.kind == SensorKind::Cpu)
            .map(|(index, _)| index)
            .collect();
        if !cpus.is_empty() && !have_skin {
            let thresholds = cpus
                .iter()
                .map(|&index| sensors[index].thresholds)
                .filter(|thresholds| thresholds[Severity::Severe as usize].is_some())
                .min_by(|a, b| {
                    a[Severity::Severe as usize]
                        .partial_cmp(&b[Severity::Severe as usize])
                        .expect("thresholds are not NaN")
                })
                .unwrap_or([None; 7]);
            sensors.push(Sensor {
                name: "VIRTUAL-SKIN".into(),
                kind: SensorKind::Skin,
                thresholds,
                source: Source::MaxOf(cpus),
            });
        }

        let mut cooling = cooling_devices(thermal_dir);
        cooling.extend(hwmon_fans(hwmon_dir));
        for sensor in &sensors {
            debug!("sensor {} ({:?}) {:?}", sensor.name, sensor.kind, sensor.source);
        }
        info!("found {} temperature sensors, {} cooling devices", sensors.len(), cooling.len());
        Self {
            sensors,
            cooling,
            thermal_dir: thermal_dir.to_owned(),
            hwmon_dir: hwmon_dir.to_owned(),
            layout: layout(thermal_dir, hwmon_dir),
        }
    }

    /// Probes again if thermal zones, cooling devices or hwmon devices appeared or went
    /// away since the last probe, and returns whether it did. Their drivers are modules
    /// (ACPI thermal, coretemp, iwlwifi) that may load after the HAL starts.
    pub fn rediscover_if_changed(&mut self) -> bool {
        if layout(&self.thermal_dir, &self.hwmon_dir) == self.layout {
            return false;
        }
        *self = Self::discover(&self.thermal_dir, &self.hwmon_dir);
        true
    }

    /// The sensors, in a fixed order.
    pub fn sensors(&self) -> &[Sensor] {
        &self.sensors
    }

    /// The cooling devices, in a fixed order.
    pub fn cooling_devices(&self) -> &[CoolingDevice] {
        &self.cooling
    }

    /// Reads every sensor; the result is indexed like [`Sensors::sensors`], `None` for a
    /// sensor that cannot be read right now.
    pub fn read(&self) -> Vec<Option<f32>> {
        let mut values: Vec<Option<f32>> = self
            .sensors
            .iter()
            .map(|sensor| match &sensor.source {
                Source::File(path) => read_attr::<i64>(path).map(millidegrees),
                Source::MaxOf(_) => None,
            })
            .collect();
        for (index, sensor) in self.sensors.iter().enumerate() {
            if let Source::MaxOf(inputs) = &sensor.source {
                values[index] = inputs.iter().filter_map(|&input| values[input]).reduce(f32::max);
            }
        }
        values
    }
}

/// The entries of both class directories, sorted.
fn layout(thermal_dir: &Path, hwmon_dir: &Path) -> Vec<PathBuf> {
    let mut entries = pchal::sysfs::entries(thermal_dir).unwrap_or_default();
    entries.extend(pchal::sysfs::entries(hwmon_dir).unwrap_or_default());
    entries
}

/// hwmon drivers this HAL reports, and the sensor kind of their temperatures.
fn hwmon_kind(driver: &str) -> Option<SensorKind> {
    Some(match driver {
        "coretemp" | "k10temp" | "zenpower" => SensorKind::Cpu,
        "amdgpu" | "radeon" | "nouveau" | "i915" | "xe" => SensorKind::Gpu,
        "nvme" => SensorKind::Unknown,
        _ => return None,
    })
}

/// `(name, kind, tempN_input path, TjMax)` for each hwmon temperature worth reporting:
/// CPU packages (`Package id N`, `Tctl`), the first temperature of a GPU or disk.
fn hwmon_sensors(hwmon_dir: &Path) -> Vec<(String, SensorKind, PathBuf, Option<f32>)> {
    let mut found = Vec::new();
    let mut counts: HashMap<SensorKind, usize> = HashMap::new();
    for dir in pchal::sysfs::entries(hwmon_dir).unwrap_or_default() {
        let Some(driver) = read_attr::<String>(&dir.join("name")) else { continue };
        let Some(kind) = hwmon_kind(&driver) else { continue };
        let inputs = temp_inputs(&dir);
        let chosen: Vec<(PathBuf, String)> = match kind {
            SensorKind::Cpu => inputs
                .into_iter()
                .filter(|(_, label)| label.starts_with("Package id") || label == "Tctl")
                .collect(),
            _ => inputs.into_iter().take(1).collect(),
        };
        for (input, _) in chosen {
            let count = counts.entry(kind).or_default();
            let name = match kind {
                SensorKind::Cpu => format!("CPU{count}"),
                SensorKind::Gpu => format!("GPU{count}"),
                _ => format!("{driver}{count}"),
            };
            *count += 1;
            let tjmax = read_attr::<i64>(&sibling(&input, "crit")).map(millidegrees);
            found.push((name, kind, input, tjmax));
        }
    }
    found
}

/// The `tempN_input` attributes of a hwmon device with their labels, by N.
fn temp_inputs(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut inputs: Vec<(u32, PathBuf, String)> = pchal::sysfs::entries(dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let file = path.file_name()?.to_str()?;
            let index: u32 = file.strip_prefix("temp")?.strip_suffix("_input")?.parse().ok()?;
            let label = read_attr::<String>(&sibling(&path, "label")).unwrap_or_default();
            Some((index, path, label))
        })
        .collect();
    inputs.sort_by_key(|(index, _, _)| *index);
    inputs.into_iter().map(|(_, path, label)| (path, label)).collect()
}

/// `tempN_<suffix>` next to `tempN_input`.
fn sibling(input: &Path, suffix: &str) -> PathBuf {
    let file = input.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    input.with_file_name(file.replace("_input", &format!("_{suffix}")))
}

/// Generic hwmon thresholds: `max` is SEVERE, `crit` CRITICAL, `emergency` EMERGENCY.
fn hwmon_thresholds(input: &Path) -> Thresholds {
    let mut thresholds: Thresholds = [None; 7];
    for (suffix, severity) in [
        ("max", Severity::Severe),
        ("crit", Severity::Critical),
        ("emergency", Severity::Emergency),
    ] {
        thresholds[severity as usize] = read_attr::<i64>(&sibling(input, suffix))
            .map(millidegrees)
            .filter(|&celsius| celsius >= MIN_PLAUSIBLE_TRIP);
    }
    thresholds
}

/// The kind of a thermal zone from its type, for zones not named after the PC CPU package.
fn zone_kind(zone_type: &str) -> SensorKind {
    let soc_zone = |prefix: &str| {
        zone_type.strip_prefix(prefix).is_some_and(|rest| rest.ends_with("-thermal"))
    };
    match zone_type {
        t if t.starts_with("iwlwifi") || t == "wlan-thermal" => SensorKind::Wifi,
        _ if soc_zone("cpu") || soc_zone("cluster") => SensorKind::Cpu,
        _ if soc_zone("gpu") => SensorKind::Gpu,
        "batt-thermal" | "max170xx_battery" | "battery" => SensorKind::Battery,
        "blkt-thermal" => SensorKind::Skin,
        _ => SensorKind::Unknown,
    }
}

/// Thermal zones. `x86_pkg_temp` counts as a CPU only when no hwmon CPU sensor exists
/// (it reads the same package sensor as `coretemp`).
fn zone_sensors(thermal_dir: &Path, have_cpu: bool) -> Vec<Sensor> {
    let mut sensors = Vec::new();
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut cpus = 0;
    for dir in pchal::sysfs::entries(thermal_dir).unwrap_or_default() {
        let is_zone =
            dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("thermal_zone"));
        if !is_zone {
            continue;
        }
        let Some(zone_type) = read_attr::<String>(&dir.join("type")) else { continue };
        if zone_type == "x86_pkg_temp" {
            if have_cpu {
                continue;
            }
            cpus += 1;
            sensors.push(Sensor {
                name: format!("CPU{}", cpus - 1),
                kind: SensorKind::Cpu,
                thresholds: cpu_thresholds(DEFAULT_TJMAX),
                source: Source::File(dir.join("temp")),
            });
            continue;
        }
        let kind = zone_kind(&zone_type);
        let seen = names.entry(zone_type.clone()).or_default();
        *seen += 1;
        let name =
            if *seen == 1 { zone_type.clone() } else { format!("{zone_type}-{}", *seen - 1) };
        let mut thresholds = trip_thresholds(&dir);
        if thresholds.iter().all(Option::is_none) {
            match kind {
                SensorKind::Skin => thresholds = SKIN_DEFAULT_THRESHOLDS,
                SensorKind::Battery => thresholds = BATTERY_DEFAULT_THRESHOLDS,
                _ => {}
            }
        }
        sensors.push(Sensor { name, kind, thresholds, source: Source::File(dir.join("temp")) });
    }
    sensors
}

/// ACPI-style trip points: the lowest `passive` trip is SEVERE (the kernel starts
/// throttling there), `hot` EMERGENCY, `critical` SHUTDOWN (the kernel powers off there
/// itself). Implausible trips are ignored.
fn trip_thresholds(zone: &Path) -> Thresholds {
    let mut thresholds: Thresholds = [None; 7];
    for index in 0.. {
        let trip = zone.join(format!("trip_point_{index}_temp"));
        let Some(temp) = read_attr::<i64>(&trip).map(millidegrees) else { break };
        if temp < MIN_PLAUSIBLE_TRIP {
            continue;
        }
        let trip_type =
            read_attr::<String>(&zone.join(format!("trip_point_{index}_type"))).unwrap_or_default();
        let severity = match trip_type.as_str() {
            "passive" => Severity::Severe,
            "hot" => Severity::Emergency,
            "critical" => Severity::Shutdown,
            _ => continue,
        };
        let slot = &mut thresholds[severity as usize];
        *slot = Some(slot.map_or(temp, |old| old.min(temp)));
    }
    thresholds
}

/// The fans of the hwmon devices this HAL reports (`fanN_input`, RPM), as cooling devices
/// named after their kind: `gpu-fan0`, `cpu-fan0`. Their state is the speed in RPM.
fn hwmon_fans(hwmon_dir: &Path) -> Vec<CoolingDevice> {
    let mut fans = Vec::new();
    let mut counts: HashMap<SensorKind, usize> = HashMap::new();
    for dir in pchal::sysfs::entries(hwmon_dir).unwrap_or_default() {
        let Some(driver) = read_attr::<String>(&dir.join("name")) else { continue };
        let Some(kind) = hwmon_kind(&driver) else { continue };
        let mut inputs: Vec<(u32, PathBuf)> = pchal::sysfs::entries(&dir)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|path| {
                let file = path.file_name()?.to_str()?;
                let index: u32 = file.strip_prefix("fan")?.strip_suffix("_input")?.parse().ok()?;
                Some((index, path))
            })
            .collect();
        inputs.sort_by_key(|(index, _)| *index);
        for (_, state) in inputs {
            let count = counts.entry(kind).or_default();
            let prefix = match kind {
                SensorKind::Cpu => "cpu".to_owned(),
                SensorKind::Gpu => "gpu".to_owned(),
                _ => driver.clone(),
            };
            fans.push(CoolingDevice {
                name: format!("{prefix}-fan{count}"),
                kind: CoolingKind::Fan,
                state,
            });
            *count += 1;
        }
    }
    fans
}

fn cooling_devices(thermal_dir: &Path) -> Vec<CoolingDevice> {
    let mut devices: Vec<(u32, CoolingDevice)> = pchal::sysfs::entries(thermal_dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|dir| {
            let index: u32 =
                dir.file_name()?.to_str()?.strip_prefix("cooling_device")?.parse().ok()?;
            let device_type = read_attr::<String>(&dir.join("type"))?;
            let kind = match device_type.as_str() {
                "Fan" => CoolingKind::Fan,
                "Processor" | "intel_powerclamp" | "TCC Offset" => CoolingKind::Cpu,
                t if t.starts_with("cpufreq-") => CoolingKind::Cpu,
                t if t.starts_with("devfreq-") && t.ends_with(".gpu") => CoolingKind::Gpu,
                t if t.starts_with("iwlwifi") || t == "ath11k_thermal" => CoolingKind::Wifi,
                _ => CoolingKind::Component,
            };
            let name = format!("{}-{index}", device_type.to_lowercase().replace(' ', "_"));
            Some((index, CoolingDevice { name, kind, state: dir.join("cur_state") }))
        })
        .collect();
    devices.sort_by_key(|(index, _)| *index);
    devices.into_iter().map(|(_, device)| device).collect()
}

/// Reads an attribute, treating errors (missing sensor, `EIO`/`ENODATA` from a driver)
/// as "no value".
fn read_attr<T: std::str::FromStr>(path: &Path) -> Option<T> {
    pchal::sysfs::read_optional(path).ok().flatten()
}

/// Converts millidegrees Celsius to degrees.
fn millidegrees(value: i64) -> f32 {
    // Temperatures are small; the conversion is exact to well under 0.01 °C.
    (value as f64 / 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn write(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create dir");
        fs::write(path, format!("{value}\n")).expect("write fixture");
    }

    /// The Z790 PC: two ACPI zones (one with placeholder trips), x86_pkg_temp, iwlwifi,
    /// coretemp, NVMe, a board sensor driver this HAL ignores, and cooling devices.
    fn z790(root: &Path) -> (PathBuf, PathBuf) {
        let thermal = root.join("thermal");
        let hwmon = root.join("hwmon");
        for (zone, zone_type, temp, trips) in [
            ("thermal_zone0", "acpitz", "16800", &[("20800", "critical"), ("19800", "hot")][..]),
            (
                "thermal_zone1",
                "acpitz",
                "27800",
                &[("105000", "critical"), ("80000", "active")][..],
            ),
            ("thermal_zone2", "x86_pkg_temp", "47000", &[("-274000", "passive")][..]),
            ("thermal_zone3", "iwlwifi_1", "52000", &[][..]),
        ] {
            let dir = thermal.join(zone);
            write(&dir.join("type"), zone_type);
            write(&dir.join("temp"), temp);
            for (index, (trip, trip_type)) in trips.iter().enumerate() {
                write(&dir.join(format!("trip_point_{index}_temp")), trip);
                write(&dir.join(format!("trip_point_{index}_type")), trip_type);
            }
        }
        for (device, device_type, state) in [
            ("cooling_device0", "Fan", "0"),
            ("cooling_device10", "Processor", "1"),
            ("cooling_device2", "intel_powerclamp", "0"),
        ] {
            write(&thermal.join(device).join("type"), device_type);
            write(&thermal.join(device).join("cur_state"), state);
        }
        let core = hwmon.join("hwmon6");
        write(&core.join("name"), "coretemp");
        write(&core.join("temp1_input"), "52000");
        write(&core.join("temp1_label"), "Package id 0");
        write(&core.join("temp1_crit"), "100000");
        write(&core.join("temp2_input"), "50000");
        write(&core.join("temp2_label"), "Core 0");
        let nvme = hwmon.join("hwmon1");
        write(&nvme.join("name"), "nvme");
        write(&nvme.join("temp1_input"), "38850");
        write(&nvme.join("temp1_max"), "81850");
        write(&nvme.join("temp1_crit"), "84850");
        let board = hwmon.join("hwmon2");
        write(&board.join("name"), "gigabyte_wmi");
        write(&board.join("temp1_input"), "30000");
        (thermal, hwmon)
    }

    #[test]
    fn discovers_the_z790_pc() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (thermal, hwmon) = z790(root.path());
        let sensors = Sensors::discover(&thermal, &hwmon);

        let names: Vec<(&str, SensorKind)> =
            sensors.sensors().iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            names,
            [
                ("nvme0", SensorKind::Unknown),
                ("CPU0", SensorKind::Cpu),
                ("acpitz", SensorKind::Unknown),
                ("acpitz-1", SensorKind::Unknown),
                ("iwlwifi_1", SensorKind::Wifi),
                ("VIRTUAL-SKIN", SensorKind::Skin),
            ]
        );
        let values = sensors.read();
        assert_eq!(values[1], Some(52.0));
        assert_eq!(values[5], Some(52.0), "skin follows the hottest CPU package");

        // Placeholder ACPI trips are dropped; real ones map to severities.
        assert_eq!(sensors.sensors()[2].thresholds, [None; 7]);
        assert_eq!(sensors.sensors()[3].thresholds[Severity::Shutdown as usize], Some(105.0));
        assert_eq!(sensors.sensors()[0].thresholds[Severity::Severe as usize], Some(81.85));

        let cooling: Vec<(&str, CoolingKind, Option<i64>)> = sensors
            .cooling_devices()
            .iter()
            .map(|c| (c.name.as_str(), c.kind, c.state()))
            .collect();
        assert_eq!(
            cooling,
            [
                ("fan-0", CoolingKind::Fan, Some(0)),
                ("intel_powerclamp-2", CoolingKind::Cpu, Some(0)),
                ("processor-10", CoolingKind::Cpu, Some(1)),
            ]
        );
    }

    #[test]
    fn nouveau_on_gsp_gets_default_gpu_thresholds_and_fans() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (thermal, hwmon) = z790(root.path());
        // nouveau on GSP-RM: temp1_input only.
        let gpu = hwmon.join("hwmon8");
        write(&gpu.join("name"), "nouveau");
        write(&gpu.join("temp1_input"), "41000");
        // A GPU driver with fans and its own limits (amdgpu-style).
        let amd = hwmon.join("hwmon9");
        write(&amd.join("name"), "amdgpu");
        write(&amd.join("temp1_input"), "50000");
        write(&amd.join("temp1_crit"), "100000");
        write(&amd.join("fan1_input"), "1200");
        write(&amd.join("fan2_input"), "1300");
        let sensors = Sensors::discover(&thermal, &hwmon);

        let gpus: Vec<&Sensor> =
            sensors.sensors().iter().filter(|s| s.kind == SensorKind::Gpu).collect();
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "GPU0");
        assert_eq!(gpus[0].thresholds, GPU_DEFAULT_THRESHOLDS);
        assert_eq!(gpus[0].severity(70.0), Severity::None);
        assert_eq!(gpus[0].severity(91.0), Severity::Severe);
        assert_eq!(gpus[1].thresholds[Severity::Critical as usize], Some(100.0));
        assert_eq!(gpus[1].thresholds[Severity::Light as usize], None, "driver limits win");
        let index = sensors.sensors().iter().position(|s| s.name == "GPU0").expect("GPU0");
        assert_eq!(sensors.read()[index], Some(41.0));

        let fans: Vec<(&str, CoolingKind, Option<i64>)> = sensors
            .cooling_devices()
            .iter()
            .filter(|c| c.name.contains("-fan"))
            .map(|c| (c.name.as_str(), c.kind, c.state()))
            .collect();
        assert_eq!(
            fans,
            [
                ("gpu-fan0", CoolingKind::Fan, Some(1200)),
                ("gpu-fan1", CoolingKind::Fan, Some(1300)),
            ]
        );
    }

    #[test]
    fn x86_pkg_temp_is_the_cpu_without_coretemp() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (thermal, _) = z790(root.path());
        let sensors = Sensors::discover(&thermal, &root.path().join("no-hwmon"));
        let cpu = sensors.sensors().iter().find(|s| s.kind == SensorKind::Cpu).expect("a CPU");
        assert_eq!(cpu.name, "CPU0");
        assert_eq!(sensors.sensors().last().map(|s| s.kind), Some(SensorKind::Skin));
    }

    /// The Galaxy Tab S7 (SM8250): tsens zones with trip points, PMIC ADC thermistors
    /// without, the fuel gauge's zone, cpufreq/devfreq/ath11k cooling devices.
    #[test]
    fn discovers_the_sm8250_tablet() {
        let root = tempfile::tempdir().expect("create temp dir");
        let thermal = root.path().join("thermal");
        let three_trips = &[("90000", "passive"), ("95000", "passive"), ("110000", "critical")][..];
        for (zone, zone_type, temp, trips) in [
            ("thermal_zone0", "aoss0-thermal", "40500", &[("90000", "hot")][..]),
            ("thermal_zone1", "cpu0-thermal", "42000", three_trips),
            ("thermal_zone10", "cpu7-top-thermal", "45900", three_trips),
            ("thermal_zone5", "cluster0-thermal", "42800", &[("90000", "hot")][..]),
            (
                "thermal_zone15",
                "gpu-top-thermal",
                "38600",
                &[("85000", "passive"), ("90000", "hot"), ("110000", "critical")][..],
            ),
            ("thermal_zone17", "wlan-thermal", "39900", &[("90000", "hot")][..]),
            ("thermal_zone25", "max170xx_battery", "33600", &[][..]),
            ("thermal_zone26", "batt-thermal", "33676", &[][..]),
            ("thermal_zone28", "skin-msm-thermal", "40639", &[][..]),
            ("thermal_zone29", "blkt-thermal", "34597", &[][..]),
        ] {
            let dir = thermal.join(zone);
            write(&dir.join("type"), zone_type);
            write(&dir.join("temp"), temp);
            for (index, (trip, trip_type)) in trips.iter().enumerate() {
                write(&dir.join(format!("trip_point_{index}_temp")), trip);
                write(&dir.join(format!("trip_point_{index}_type")), trip_type);
            }
        }
        for (device, device_type) in [
            ("cooling_device0", "cpufreq-cpu0"),
            ("cooling_device3", "devfreq-3d00000.gpu"),
            ("cooling_device4", "ath11k_thermal"),
        ] {
            write(&thermal.join(device).join("type"), device_type);
            write(&thermal.join(device).join("cur_state"), "0");
        }
        let sensors = Sensors::discover(&thermal, &root.path().join("no-hwmon"));

        let kind = |name: &str| {
            sensors.sensors().iter().find(|s| s.name == name).map(|s| s.kind).expect(name)
        };
        assert_eq!(kind("cpu0-thermal"), SensorKind::Cpu);
        assert_eq!(kind("cpu7-top-thermal"), SensorKind::Cpu);
        assert_eq!(kind("cluster0-thermal"), SensorKind::Cpu);
        assert_eq!(kind("gpu-top-thermal"), SensorKind::Gpu);
        assert_eq!(kind("wlan-thermal"), SensorKind::Wifi);
        assert_eq!(kind("batt-thermal"), SensorKind::Battery);
        assert_eq!(kind("max170xx_battery"), SensorKind::Battery);
        assert_eq!(kind("blkt-thermal"), SensorKind::Skin);
        assert_eq!(kind("skin-msm-thermal"), SensorKind::Unknown);
        assert_eq!(kind("aoss0-thermal"), SensorKind::Unknown);
        assert!(
            sensors.sensors().iter().all(|s| s.name != "VIRTUAL-SKIN"),
            "a real skin sensor replaces the virtual one"
        );

        let skin = sensors.sensors().iter().find(|s| s.kind == SensorKind::Skin).expect("skin");
        assert_eq!(skin.thresholds, SKIN_DEFAULT_THRESHOLDS);
        assert_eq!(skin.severity(34.6), Severity::None);
        assert_eq!(skin.severity(44.0), Severity::Moderate);
        let cpu = sensors.sensors().iter().find(|s| s.name == "cpu0-thermal").expect("cpu0");
        assert_eq!(cpu.thresholds[Severity::Severe as usize], Some(90.0));
        assert_eq!(cpu.thresholds[Severity::Shutdown as usize], Some(110.0));

        let cooling: Vec<(&str, CoolingKind)> =
            sensors.cooling_devices().iter().map(|c| (c.name.as_str(), c.kind)).collect();
        assert_eq!(
            cooling,
            [
                ("cpufreq-cpu0-0", CoolingKind::Cpu),
                ("devfreq-3d00000.gpu-3", CoolingKind::Gpu),
                ("ath11k_thermal-4", CoolingKind::Wifi),
            ]
        );
    }

    #[test]
    fn severity_follows_cpu_thresholds() {
        let sensor = Sensor {
            name: "CPU0".into(),
            kind: SensorKind::Cpu,
            thresholds: cpu_thresholds(100.0),
            source: Source::File(PathBuf::new()),
        };
        assert_eq!(sensor.severity(50.0), Severity::None);
        assert_eq!(sensor.severity(90.0), Severity::Light);
        assert_eq!(sensor.severity(97.0), Severity::Moderate);
        assert_eq!(sensor.severity(100.0), Severity::Severe);
        let thresholds: Vec<f32> = sensor.thresholds[1..].iter().map(|t| t.expect("set")).collect();
        assert!(thresholds.windows(2).all(|pair| pair[0] <= pair[1]), "non-decreasing");
    }

    #[test]
    fn late_drivers_are_picked_up() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (thermal, hwmon) = (root.path().join("thermal"), root.path().join("hwmon"));
        let mut sensors = Sensors::discover(&thermal, &hwmon);
        assert!(sensors.sensors().is_empty());
        assert!(!sensors.rediscover_if_changed());

        z790(root.path());
        assert!(sensors.rediscover_if_changed());
        assert_eq!(sensors.sensors().len(), 6);
        assert!(!sensors.rediscover_if_changed());
    }

    #[test]
    fn empty_machine_has_no_sensors() {
        let root = tempfile::tempdir().expect("create temp dir");
        let sensors = Sensors::discover(&root.path().join("thermal"), &root.path().join("hwmon"));
        assert!(sensors.sensors().is_empty());
        assert!(sensors.cooling_devices().is_empty());
    }
}
