//! `ICameraProvider`: finds webcams and raw CAMSS sensors under `/dev` and follows them
//! appearing and going away.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use android_hardware_camera_common::aidl::android::hardware::camera::common::{
    CameraDeviceStatus::CameraDeviceStatus, Status::Status, TorchModeStatus::TorchModeStatus,
    VendorTagSection::VendorTagSection,
};
use android_hardware_camera_device::aidl::android::hardware::camera::device::ICameraDevice::{
    BnCameraDevice, ICameraDevice,
};
use android_hardware_camera_provider::aidl::android::hardware::camera::provider::{
    CameraIdAndStreamCombination::CameraIdAndStreamCombination,
    ConcurrentCameraIdCombination::ConcurrentCameraIdCombination, ICameraProvider::ICameraProvider,
    ICameraProviderCallback::ICameraProviderCallback,
};
use binder::{BinderFeatures, Interface, Strong};
use log::{error, info, warn};
use pchal::uevent::UeventSocket;

use crate::camera::Camera;
use crate::device::CameraDevice;
use crate::service_error;

/// When to look at `/dev` after a `video4linux` uevent: ueventd creates the node, then
/// sets its owner and SELinux label.
const RESCAN_DELAYS: [Duration; 2] = [Duration::from_millis(300), Duration::from_millis(1500)];

#[derive(Debug, Default)]
struct State {
    /// Present cameras by device name.
    cameras: BTreeMap<String, Arc<Camera>>,
}

/// The framework's provider callback, once it has set one. Devices and sessions hold it
/// too, to report torch status changes.
#[derive(Debug, Clone, Default)]
pub struct ProviderCallback(Arc<Mutex<Option<Strong<dyn ICameraProviderCallback>>>>);

impl ProviderCallback {
    fn set(&self, callback: Strong<dyn ICameraProviderCallback>) {
        *self.0.lock().expect("provider callback lock poisoned") = Some(callback);
    }

    fn get(&self) -> Option<Strong<dyn ICameraProviderCallback>> {
        self.0.lock().expect("provider callback lock poisoned").clone()
    }

    /// Reports the torch of device `name` as `status`.
    pub fn torch_status(&self, name: &str, status: TorchModeStatus) {
        info!("{name}: torch {status:?}");
        if let Some(callback) = self.get() {
            if let Err(err) = callback.torchModeStatusChange(name, status) {
                warn!("torchModeStatusChange({name}): {err}");
            }
        }
    }
}

/// The camera provider. Cloning shares the same state, so the hotplug thread can hold one.
#[derive(Debug, Clone, Default)]
pub struct Provider {
    state: Arc<Mutex<State>>,
    callback: ProviderCallback,
}

impl Interface for Provider {}

impl Provider {
    /// A provider with the cameras present now.
    pub fn new() -> Self {
        let provider = Self::default();
        provider.rescan();
        provider
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("provider state lock poisoned")
    }

    /// Compares `/dev/video*` and the media devices with the known cameras and reports the
    /// differences.
    pub fn rescan(&self) {
        let numbers = video_nodes();
        let known: Vec<(String, Arc<Camera>)> = self
            .lock()
            .cameras
            .iter()
            .map(|(name, camera)| (name.clone(), Arc::clone(camera)))
            .collect();

        let mut gone = Vec::new();
        let mut cameras = BTreeMap::new();
        for (name, camera) in known {
            if camera.present() {
                cameras.insert(name, camera);
            } else {
                gone.push(name);
            }
        }
        let mut added = Vec::new();
        // Raw sensors appear together once CAMSS and the sensor drivers have probed; look
        // again while none is known (a session may hold one, so never re-probe them).
        if !cameras.values().any(|camera| camera.raw().is_some()) {
            for camera in Camera::probe_raw() {
                let name = camera.device_name();
                added.push(name.clone());
                cameras.insert(name, Arc::new(camera));
            }
        }
        for number in numbers {
            let path = format!("/dev/video{number}");
            if cameras.values().any(|camera| camera.path().as_os_str() == path.as_str()) {
                continue;
            }
            if let Some(camera) = Camera::probe(number) {
                let name = camera.device_name();
                added.push(name.clone());
                cameras.insert(name, Arc::new(camera));
            }
        }

        let callback = {
            let mut state = self.lock();
            state.cameras = cameras;
            self.callback.get()
        };
        for (names, status) in
            [(gone, CameraDeviceStatus::NOT_PRESENT), (added, CameraDeviceStatus::PRESENT)]
        {
            for name in names {
                info!("{name}: {status:?}");
                if let Some(callback) = &callback {
                    if let Err(err) = callback.cameraDeviceStatusChange(&name, status) {
                        warn!("cameraDeviceStatusChange({name}): {err}");
                    }
                }
            }
        }
    }

    /// Starts a thread that rescans on every `video4linux` uevent.
    pub fn watch_hotplug(&self) {
        let socket = match UeventSocket::open() {
            Ok(socket) => socket,
            Err(err) => {
                warn!("no hotplug: {err}");
                return;
            }
        };
        let provider = self.clone();
        let spawned = thread::Builder::new().name("hotplug".into()).spawn(move || loop {
            match socket.recv() {
                Ok(event) if event.subsystem() == Some("video4linux") => {
                    for delay in RESCAN_DELAYS {
                        thread::sleep(delay);
                        provider.rescan();
                    }
                }
                Ok(_) => {}
                Err(err) => warn!("{err}"),
            }
        });
        if let Err(err) = spawned {
            error!("cannot start the hotplug thread: {err}");
        }
    }
}

/// The numbers N of the `/dev/videoN` nodes, ascending.
fn video_nodes() -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/dev") else { return Vec::new() };
    let mut numbers: Vec<u32> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|name| name.strip_prefix("video")?.parse().ok())
        .collect();
    numbers.sort_unstable();
    numbers
}

impl ICameraProvider for Provider {
    fn setCallback(&self, callback: &Strong<dyn ICameraProviderCallback>) -> binder::Result<()> {
        // Under the state lock, so a rescan cannot report a camera before or after it.
        let names: Vec<String> = {
            let state = self.lock();
            self.callback.set(callback.clone());
            state.cameras.keys().cloned().collect()
        };
        // Clients learn of hot-pluggable cameras from the callback, during this call.
        for name in names {
            if let Err(err) = callback.cameraDeviceStatusChange(&name, CameraDeviceStatus::PRESENT)
            {
                warn!("cameraDeviceStatusChange({name}): {err}");
            }
        }
        Ok(())
    }

    fn getVendorTags(&self) -> binder::Result<Vec<VendorTagSection>> {
        Ok(Vec::new())
    }

    fn getCameraIdList(&self) -> binder::Result<Vec<String>> {
        Ok(self.lock().cameras.keys().cloned().collect())
    }

    fn getCameraDeviceInterface(&self, name: &str) -> binder::Result<Strong<dyn ICameraDevice>> {
        let (camera, conflicting) = {
            let state = self.lock();
            let Some(camera) = state.cameras.get(name).cloned() else {
                return Err(service_error(Status::ILLEGAL_ARGUMENT));
            };
            // Raw cameras share the CAMSS graph: only one streams at a time.
            let conflicting: Vec<String> = if camera.raw().is_some() {
                state
                    .cameras
                    .iter()
                    .filter(|(other, cam)| other.as_str() != name && cam.raw().is_some())
                    .map(|(other, _)| other.clone())
                    .collect()
            } else {
                Vec::new()
            };
            (camera, conflicting)
        };
        let device = CameraDevice::new(camera, conflicting, self.callback.clone());
        Ok(BnCameraDevice::new_binder(device, BinderFeatures::default()))
    }

    fn notifyDeviceStateChange(&self, _state: i64) -> binder::Result<()> {
        Ok(())
    }

    fn getConcurrentCameraIds(&self) -> binder::Result<Vec<ConcurrentCameraIdCombination>> {
        Ok(Vec::new())
    }

    fn isConcurrentStreamCombinationSupported(
        &self,
        _configs: &[CameraIdAndStreamCombination],
    ) -> binder::Result<bool> {
        Ok(false)
    }
}
