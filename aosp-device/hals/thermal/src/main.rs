//! android.hardware.thermal HAL for PCs: CPU package, GPU, disk, ACPI and Wi-Fi
//! temperatures, a virtual skin sensor, and fans/processor throttling as cooling devices.

mod sensors;
mod thermal;

use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use android_hardware_thermal::aidl::android::hardware::thermal::IThermal::{BnThermal, IThermal};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::sensors::{Sensors, Severity};
use crate::thermal::Thermal;

const LOG_TAG: &str = "android.hardware.thermal-service.pc";
const THERMAL_DIR: &str = "/sys/class/thermal";
const HWMON_DIR: &str = "/sys/class/hwmon";
/// Poll interval while every sensor is below its first threshold.
const POLL_IDLE: Duration = Duration::from_secs(5);
/// Poll interval while something is throttling, so severity changes reach Android fast.
const POLL_HOT: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let thermal = Thermal::new(Sensors::discover(Path::new(THERMAL_DIR), Path::new(HWMON_DIR)));
    let poller = thermal.clone();
    let spawned = thread::Builder::new().name("poll".into()).spawn(move || loop {
        let interval = if poller.poll() > Severity::None { POLL_HOT } else { POLL_IDLE };
        thread::sleep(interval);
    });
    if let Err(err) = spawned {
        error!("cannot start the poll thread, no throttling callbacks: {err}");
    }

    let binder = BnThermal::new_binder(thermal, BinderFeatures::default());
    let service = Service::new(Thermal::get_descriptor(), "default").extra_threads(1);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
