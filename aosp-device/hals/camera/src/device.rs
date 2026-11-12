//! `ICameraDevice`: one webcam's description and the entry to a session.

use std::sync::Arc;

use android_hardware_camera_common::aidl::android::hardware::camera::common::{
    CameraResourceCost::CameraResourceCost, Status::Status,
};
use android_hardware_camera_device::aidl::android::hardware::camera::device::{
    CameraMetadata::CameraMetadata,
    ICameraDevice::ICameraDevice,
    ICameraDeviceCallback::ICameraDeviceCallback,
    ICameraDeviceSession::{BnCameraDeviceSession, ICameraDeviceSession},
    ICameraInjectionSession::ICameraInjectionSession,
    RequestTemplate::RequestTemplate,
    StreamConfiguration::StreamConfiguration,
};
use binder::{BinderFeatures, Interface, Strong};
use pccamera::sensor::Sensor;

use crate::camera::Camera;
use crate::service_error;
use crate::session::{self, Session};

/// A webcam as an `ICameraDevice`.
#[derive(Debug)]
pub struct CameraDevice {
    camera: Arc<Camera>,
}

impl CameraDevice {
    /// Serves `camera`.
    pub fn new(camera: Arc<Camera>) -> Self {
        Self { camera }
    }
}

impl Interface for CameraDevice {}

fn unsupported<T>() -> binder::Result<T> {
    Err(service_error(Status::OPERATION_NOT_SUPPORTED))
}

impl ICameraDevice for CameraDevice {
    fn getCameraCharacteristics(&self) -> binder::Result<CameraMetadata> {
        Ok(CameraMetadata { metadata: self.camera.characteristics().to_vec() })
    }

    fn getPhysicalCameraCharacteristics(&self, _id: &str) -> binder::Result<CameraMetadata> {
        // Not a logical multi-camera.
        Err(service_error(Status::ILLEGAL_ARGUMENT))
    }

    fn getResourceCost(&self) -> binder::Result<CameraResourceCost> {
        // A webcam takes its USB bandwidth whole.
        Ok(CameraResourceCost { resourceCost: 100, conflictingDevices: Vec::new() })
    }

    fn isStreamCombinationSupported(&self, streams: &StreamConfiguration) -> binder::Result<bool> {
        Ok(session::validate(self.camera.sensor(), streams).is_ok())
    }

    fn isStreamCombinationWithSettingsSupported(
        &self,
        streams: &StreamConfiguration,
    ) -> binder::Result<bool> {
        self.isStreamCombinationSupported(streams)
    }

    fn open(
        &self,
        callback: &Strong<dyn ICameraDeviceCallback>,
    ) -> binder::Result<Strong<dyn ICameraDeviceSession>> {
        if !self.camera.begin_session() {
            return Err(service_error(Status::CAMERA_IN_USE));
        }
        match Session::open(Arc::clone(&self.camera), callback.clone()) {
            Ok(session) => {
                Ok(BnCameraDeviceSession::new_binder(session, BinderFeatures::default()))
            }
            Err(err) => {
                self.camera.end_session();
                Err(err)
            }
        }
    }

    fn openInjectionSession(
        &self,
        _callback: &Strong<dyn ICameraDeviceCallback>,
    ) -> binder::Result<Strong<dyn ICameraInjectionSession>> {
        unsupported()
    }

    fn setTorchMode(&self, _on: bool) -> binder::Result<()> {
        unsupported()
    }

    fn turnOnTorchWithStrengthLevel(&self, _level: i32) -> binder::Result<()> {
        unsupported()
    }

    fn getTorchStrengthLevel(&self) -> binder::Result<i32> {
        unsupported()
    }

    fn constructDefaultRequestSettings(
        &self,
        template: RequestTemplate,
    ) -> binder::Result<CameraMetadata> {
        session::default_settings(self.camera.sensor(), template)
    }

    fn getSessionCharacteristics(
        &self,
        config: &StreamConfiguration,
    ) -> binder::Result<CameraMetadata> {
        session::validate(self.camera.sensor(), config)?;
        Ok(CameraMetadata { metadata: Sensor::session_characteristics().to_bytes() })
    }

    fn warmUp(&self) -> binder::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{c_int, c_void};

    use pccamera::image::Size;
    use pccamera::metadata::Metadata;
    use pccamera::sensor::{Sensor, Template};
    use pccamera::v4l2::{FourCc, Interval, Mode};

    extern "C" {
        // libcamera_metadata: checks offsets, alignment and every entry's element type.
        fn validate_camera_metadata_structure(
            metadata: *const c_void,
            expected_size: *const usize,
        ) -> c_int;
    }

    fn assert_valid(what: &str, metadata: &Metadata) {
        let bytes = metadata.to_bytes();
        // Copy into 8-byte aligned memory, as the framework does.
        let mut aligned = vec![0u64; bytes.len().div_ceil(8)];
        // SAFETY: `aligned` holds at least `bytes.len()` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), aligned.as_mut_ptr().cast(), bytes.len())
        };
        let size = bytes.len();
        // SAFETY: a valid buffer of `size` bytes and a pointer to its size.
        let status = unsafe { validate_camera_metadata_structure(aligned.as_ptr().cast(), &size) };
        assert_eq!(status, 0, "{what} fails validate_camera_metadata_structure");
    }

    #[test]
    fn metadata_passes_libcamera_metadata() {
        let modes = [Mode {
            fourcc: FourCc::MJPEG,
            size: Size { width: 1280, height: 720 },
            intervals: vec![Interval { numerator: 1, denominator: 30 }],
        }];
        let sensor = Sensor::new(&modes).unwrap();
        assert_valid("characteristics", &sensor.characteristics());
        for template in [
            Template::Preview,
            Template::StillCapture,
            Template::VideoRecord,
            Template::VideoSnapshot,
        ] {
            let settings = sensor.default_request(template);
            assert_valid("template", &settings);
            assert_valid("result", &sensor.result(&settings, 1, false));
        }
        assert_valid("session characteristics", &Sensor::session_characteristics());
    }
}
