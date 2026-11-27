//! `ICameraDevice`: one camera's description and the entry to a session.

use std::sync::Arc;

use android_hardware_camera_common::aidl::android::hardware::camera::common::{
    CameraResourceCost::CameraResourceCost, Status::Status, TorchModeStatus::TorchModeStatus,
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
use log::error;
use pccamera::sensor::Sensor;

use crate::camera::{Camera, Flash};
use crate::provider::ProviderCallback;
use crate::service_error;
use crate::session::{self, Session};

/// A camera as an `ICameraDevice`.
#[derive(Debug)]
pub struct CameraDevice {
    camera: Arc<Camera>,
    /// Device names that cannot stream at the same time as this one.
    conflicting: Vec<String>,
    /// For torch status changes.
    callback: ProviderCallback,
}

impl CameraDevice {
    /// Serves `camera`, which excludes the `conflicting` devices.
    pub fn new(camera: Arc<Camera>, conflicting: Vec<String>, callback: ProviderCallback) -> Self {
        Self { camera, conflicting, callback }
    }

    /// The flash for torch calls: `OPERATION_NOT_SUPPORTED` without one, `CAMERA_IN_USE`
    /// while a session owns it.
    fn torch_flash(&self) -> binder::Result<&Flash> {
        let flash = self.camera.flash().ok_or_else(|| service_error(Status::OPERATION_NOT_SUPPORTED))?;
        if self.camera.in_session() {
            return Err(service_error(Status::CAMERA_IN_USE));
        }
        Ok(flash)
    }

    fn set_torch(&self, on: bool, level: Option<u32>) -> binder::Result<()> {
        let flash = self.torch_flash()?;
        flash.set_torch(on, level).map_err(|err| {
            error!("{}: torch: {err}", self.camera.id());
            service_error(Status::INTERNAL_ERROR)
        })?;
        let status = if on { TorchModeStatus::AVAILABLE_ON } else { TorchModeStatus::AVAILABLE_OFF };
        self.callback.torch_status(&self.camera.device_name(), status);
        Ok(())
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
        // A webcam takes its USB bandwidth whole, a raw camera the CAMSS graph.
        Ok(CameraResourceCost { resourceCost: 100, conflictingDevices: self.conflicting.clone() })
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
        // The session owns the flash LED now.
        if let Some(flash) = self.camera.flash() {
            if let Err(err) = flash.set_torch(false, None) {
                error!("{}: torch off: {err}", self.camera.id());
            }
            self.callback.torch_status(&self.camera.device_name(), TorchModeStatus::NOT_AVAILABLE);
        }
        match Session::open(Arc::clone(&self.camera), callback.clone(), self.callback.clone()) {
            Ok(session) => {
                Ok(BnCameraDeviceSession::new_binder(session, BinderFeatures::default()))
            }
            Err(err) => {
                self.camera.end_session();
                if self.camera.flash().is_some() {
                    self.callback
                        .torch_status(&self.camera.device_name(), TorchModeStatus::AVAILABLE_OFF);
                }
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

    fn setTorchMode(&self, on: bool) -> binder::Result<()> {
        self.set_torch(on, None)
    }

    fn turnOnTorchWithStrengthLevel(&self, level: i32) -> binder::Result<()> {
        let flash = self.torch_flash()?;
        let level = u32::try_from(level)
            .ok()
            .filter(|level| (1..=flash.led().levels()).contains(level))
            .ok_or_else(|| service_error(Status::ILLEGAL_ARGUMENT))?;
        self.set_torch(true, Some(level))
    }

    fn getTorchStrengthLevel(&self) -> binder::Result<i32> {
        let flash = self.camera.flash().ok_or_else(|| service_error(Status::OPERATION_NOT_SUPPORTED))?;
        Ok(i32::try_from(flash.torch().level).unwrap_or(i32::MAX))
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
            assert_valid("result", &sensor.result(&settings, 1, 0));
        }
        assert_valid("session characteristics", &Sensor::session_characteristics());
    }
}
