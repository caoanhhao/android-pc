//! `ICameraProvider`: finds webcams under `/dev` and follows them being plugged in and out.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use android_hardware_camera_common::aidl::android::hardware::camera::common::{
    CameraDeviceStatus::CameraDeviceStatus, Status::Status, VendorTagSection::VendorTagSection,
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
    callback: Option<Strong<dyn ICameraProviderCallback>>,
}

/// The camera provider. Cloning shares the same state, so the hotplug thread can hold one.
#[derive(Debug, Clone, Default)]
pub struct Provider {
    state: Arc<Mutex<State>>,
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

    /// Compares `/dev/video*` with the known cameras and reports the differences.
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
            if camera.path().exists() {
                cameras.insert(name, camera);
            } else {
                gone.push(name);
            }
        }
        let mut added = Vec::new();
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
            state.callback.clone()
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
        let names: Vec<String> = {
            let mut state = self.lock();
            state.callback = Some(callback.clone());
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
        let camera = self.lock().cameras.get(name).cloned();
        let Some(camera) = camera else {
            return Err(service_error(Status::ILLEGAL_ARGUMENT));
        };
        Ok(BnCameraDevice::new_binder(CameraDevice::new(camera), BinderFeatures::default()))
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
