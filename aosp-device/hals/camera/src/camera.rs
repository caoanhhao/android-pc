//! One camera, a USB webcam or a raw sensor behind CAMSS: its V4L2 node, what it offers
//! and whether a session has it open.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use log::{info, warn};
use pccamera::raw::{Facing, RawSensor};
use pccamera::sensor::{Sensor, FOURCCS};
use pccamera::v4l2::Device;

/// Set while a raw camera streams: they share one CAMSS graph.
static RAW_IN_USE: AtomicBool = AtomicBool::new(false);

/// The first camera id; ids are this plus the `/dev/videoN` number, like AOSP's external
/// camera HAL, so they never clash with the ids of another provider.
const FIRST_ID: u32 = 100;

/// A camera found under `/dev`.
pub struct Camera {
    id: String,
    /// The capture node.
    path: PathBuf,
    /// The raw sensor and its route, for CAMSS cameras.
    raw: Option<RawSensor>,
    sensor: Sensor,
    /// The encoded static characteristics.
    characteristics: Vec<u8>,
    in_session: AtomicBool,
}

impl fmt::Debug for Camera {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Camera")
            .field("id", &self.id)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Camera {
    /// Probes `/dev/video<number>`. Returns `None` for nodes that are not video capture
    /// devices (UVC metadata nodes) or offer no usable format.
    pub fn probe(number: u32) -> Option<Self> {
        let path = PathBuf::from(format!("/dev/video{number}"));
        let probed = Device::open(&path).and_then(|device| {
            let Some(name) = device.capture_name()? else { return Ok(None) };
            Ok(Some((name, device.modes(&FOURCCS)?)))
        });
        let (name, modes) = match probed {
            Ok(Some(found)) => found,
            Ok(None) => return None,
            Err(err) => {
                warn!("{}: {err}", path.display());
                return None;
            }
        };
        let Some(sensor) = Sensor::new(&modes) else {
            info!("{}: {name}: no YUYV or MJPEG mode at 5 fps or more", path.display());
            return None;
        };
        let max = sensor.max_size();
        info!(
            "{}: {name}: {} sizes up to {}x{}",
            path.display(),
            sensor.sizes().count(),
            max.width,
            max.height
        );
        Some(Self {
            id: (FIRST_ID + number).to_string(),
            characteristics: sensor.characteristics().to_bytes(),
            path,
            raw: None,
            sensor,
            in_session: AtomicBool::new(false),
        })
    }

    /// The built-in raw cameras of every media device. Ids follow Android's habit: the
    /// main back camera (the largest) is `"0"`, the first front camera `"1"`, then the
    /// others by size.
    pub fn probe_raw() -> Vec<Self> {
        let mut sensors = RawSensor::find_all();
        let largest = |sensor: &RawSensor| {
            sensor.modes.iter().map(|(size, _)| size.area()).max().unwrap_or(0)
        };
        sensors.sort_by_key(|sensor| (sensor.facing != Facing::Back, std::cmp::Reverse(largest(sensor))));
        if let Some(front) = sensors.iter().position(|sensor| sensor.facing == Facing::Front) {
            if front > 1 {
                let sensor = sensors.remove(front);
                sensors.insert(1, sensor);
            }
        }
        let mut cameras = Vec::new();
        for raw in sensors {
            let Some(sensor) = Sensor::raw(&raw.capture_modes(), raw.facing, raw.rotation) else {
                info!("{}: no mode at 5 fps or more", raw.name);
                continue;
            };
            let max = sensor.max_size();
            info!(
                "{} ({}): {:?}, rotation {}, outputs up to {}x{}",
                raw.name,
                raw.video.display(),
                raw.facing,
                raw.rotation,
                max.width,
                max.height
            );
            cameras.push(Self {
                id: cameras.len().to_string(),
                characteristics: sensor.characteristics().to_bytes(),
                path: raw.video.clone(),
                raw: Some(raw),
                sensor,
                in_session: AtomicBool::new(false),
            });
        }
        cameras
    }

    /// The raw sensor behind a CAMSS camera.
    pub fn raw(&self) -> Option<&RawSensor> {
        self.raw.as_ref()
    }

    /// Whether the camera is still there: its capture node (and sensor) exist.
    pub fn present(&self) -> bool {
        self.path.exists() && self.raw.as_ref().is_none_or(|raw| raw.subdev.exists())
    }

    /// The camera id (`"100"` for `/dev/video0`, `"0"` for the main raw camera).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name the provider reports: `device@1.1/pc/<id>`.
    pub fn device_name(&self) -> String {
        format!("device@1.1/{}/{}", crate::PROVIDER_TYPE, self.id)
    }

    /// The V4L2 node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The capture modes and metadata builder.
    pub fn sensor(&self) -> &Sensor {
        &self.sensor
    }

    /// The encoded static characteristics.
    pub fn characteristics(&self) -> &[u8] {
        &self.characteristics
    }

    /// Marks the camera as opened; `false` if a session already has it (or, for raw
    /// cameras, any other raw camera: they share one CAMSS graph).
    pub fn begin_session(&self) -> bool {
        let raw = self.raw.is_some();
        if raw && RAW_IN_USE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        if self.in_session.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            if raw {
                RAW_IN_USE.store(false, Ordering::Release);
            }
            return false;
        }
        true
    }

    /// Marks the camera as free again.
    pub fn end_session(&self) {
        self.in_session.store(false, Ordering::Release);
        if self.raw.is_some() {
            RAW_IN_USE.store(false, Ordering::Release);
        }
    }
}
