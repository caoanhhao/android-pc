//! android.hardware.sensors HAL for the Galaxy Tab S7 LTE (gts7l): the accelerometer of the
//! sensor DSP (SLPI), read through `gts7l-sensord --stream accel`.

mod sensors;
mod stream;

use std::process::ExitCode;

use android_hardware_sensors::aidl::android::hardware::sensors::ISensors::{BnSensors, ISensors};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::sensors::Sensors;

const LOG_TAG: &str = "android.hardware.sensors-service.gts7l";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let binder = BnSensors::new_binder(Sensors::new(), BinderFeatures::default());
    let Err(err) = Service::new(Sensors::get_descriptor(), "default").serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
