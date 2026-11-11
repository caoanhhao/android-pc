//! android.hardware.light HAL for PCs: LED class devices and display backlights.

mod lights;

use std::path::Path;
use std::process::ExitCode;

use android_hardware_light::aidl::android::hardware::light::ILights::{BnLights, ILights};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::lights::Lights;

const LOG_TAG: &str = "android.hardware.light-service.pc";
const LEDS_DIR: &str = "/sys/class/leds";
const BACKLIGHT_DIR: &str = "/sys/class/backlight";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let lights = Lights::discover(Path::new(LEDS_DIR), Path::new(BACKLIGHT_DIR));

    let binder = BnLights::new_binder(lights, BinderFeatures::default());
    let Err(err) = Service::new(Lights::get_descriptor(), "default").serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
