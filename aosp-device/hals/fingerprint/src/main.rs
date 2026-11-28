//! android.hardware.biometrics.fingerprint HAL for the Galaxy Tab S7 LTE: the Goodix GW36T1 in
//! the power key, matched by Samsung's `securefp` TrustZone app (docs/gts7l.md §25).

mod hal;

use std::process::ExitCode;

use android_hardware_biometrics_fingerprint::aidl::android::hardware::biometrics::fingerprint::IFingerprint::{
    BnFingerprint, IFingerprint,
};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::hal::Fingerprint;

const LOG_TAG: &str = "android.hardware.biometrics.fingerprint-service.gts7l";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let binder = BnFingerprint::new_binder(Fingerprint::new(), BinderFeatures::default());
    let Err(err) = Service::new(Fingerprint::get_descriptor(), "default").serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
