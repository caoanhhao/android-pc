//! One webcam: its V4L2 node, what it offers and whether a session has it open.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use log::{info, warn};
use pccamera::sensor::{Sensor, FOURCCS};
use pccamera::v4l2::Device;

/// The first camera id; ids are this plus the `/dev/videoN` number, like AOSP's external
/// camera HAL, so they never clash with the ids of another provider.
const FIRST_ID: u32 = 100;

/// A webcam found under `/dev`.
pub struct Camera {
    id: String,
    path: PathBuf,
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
            sensor,
            in_session: AtomicBool::new(false),
        })
    }

    /// The camera id (`"100"` for `/dev/video0`).
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

    /// Marks the camera as opened; `false` if a session already has it.
    pub fn begin_session(&self) -> bool {
        self.in_session.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// Marks the camera as free again.
    pub fn end_session(&self) {
        self.in_session.store(false, Ordering::Release);
    }
}
