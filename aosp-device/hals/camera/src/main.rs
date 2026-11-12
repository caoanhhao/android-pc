//! android.hardware.camera.provider HAL for PCs: USB webcams (`uvcvideo`) through V4L2.
//!
//! The provider serves `ICameraProvider/pc/0` and reports each `/dev/videoN` capture node
//! with a YUYV or MJPEG mode as camera `100 + N`, an `EXTERNAL` level camera with YUV and
//! JPEG outputs. Webcams plugged in later are announced through the provider callback.

mod camera;
mod device;
mod gralloc;
mod provider;
mod session;

use std::process::ExitCode;

use android_hardware_camera_common::aidl::android::hardware::camera::common::Status::Status;
use android_hardware_camera_provider::aidl::android::hardware::camera::provider::ICameraProvider::{
    BnCameraProvider, ICameraProvider,
};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::provider::Provider;

const LOG_TAG: &str = "android.hardware.camera.provider-service.pc";

/// The provider type in the instance name and in device names (`device@1.1/pc/100`).
pub const PROVIDER_TYPE: &str = "pc";

/// Binder threads besides the main one: cameraserver calls the provider, the devices and
/// the sessions concurrently.
const EXTRA_THREADS: u32 = 3;

/// A camera `Status` as a binder service-specific error, the way camera HALs report them.
pub fn service_error(status: Status) -> binder::Status {
    binder::Status::new_service_specific_error(status.0, None)
}

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let provider = Provider::new();
    provider.watch_hotplug();
    let binder = BnCameraProvider::new_binder(provider, BinderFeatures::default());
    let instance = format!("{PROVIDER_TYPE}/0");
    let service = Service::new(Provider::get_descriptor(), &instance).extra_threads(EXTRA_THREADS);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
