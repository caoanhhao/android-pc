//! One camera, a USB webcam or a raw sensor behind CAMSS: its V4L2 node, what it offers,
//! its flash LED and whether a session has it open.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use log::{info, warn};
use pccamera::flash::{FlashLed, LEDS_DIR};
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
    flash: Option<Flash>,
    in_session: AtomicBool,
}

/// A camera's flash LED and its torch.
#[derive(Debug)]
pub struct Flash {
    led: FlashLed,
    torch: Mutex<Torch>,
}

/// The torch as the framework set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Torch {
    /// Lit outside sessions.
    pub on: bool,
    /// Brightness, 1 to the LED's levels.
    pub level: u32,
}

impl Flash {
    fn new(led: FlashLed) -> Self {
        let torch = Torch { on: false, level: led.levels() };
        Self { led, torch: Mutex::new(torch) }
    }

    /// The LED, for sessions (which own it while they run).
    pub fn led(&self) -> &FlashLed {
        &self.led
    }

    fn lock(&self) -> MutexGuard<'_, Torch> {
        self.torch.lock().expect("torch lock poisoned")
    }

    /// The torch state.
    pub fn torch(&self) -> Torch {
        *self.lock()
    }

    /// Turns the torch on or off; `level` changes its brightness (and, when off, goes back
    /// to full brightness, the default level).
    ///
    /// # Errors
    ///
    /// Returns the error of driving the LED; the state stays as it was.
    pub fn set_torch(&self, on: bool, level: Option<u32>) -> io::Result<()> {
        let mut torch = self.lock();
        let level = match (on, level) {
            (_, Some(level)) => level.clamp(1, self.led.levels()),
            (true, None) => torch.level,
            (false, None) => self.led.levels(),
        };
        self.led.set(if on { level } else { 0 })?;
        *torch = Torch { on, level };
        Ok(())
    }
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
            flash: None,
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
        // The flash LED goes with the main back camera (sorted first).
        let mut led = sensors
            .first()
            .filter(|sensor| sensor.facing == Facing::Back)
            .and_then(|_| FlashLed::find(Path::new(LEDS_DIR)));
        let mut cameras = Vec::new();
        for raw in sensors {
            let Some(mut sensor) = Sensor::raw(&raw.capture_modes(), raw.facing, raw.rotation) else {
                info!("{}: no mode at 5 fps or more", raw.name);
                continue;
            };
            if raw.lens.is_some() {
                sensor = sensor.with_focus();
            }
            let flash = if raw.facing == Facing::Back { led.take().map(Flash::new) } else { None };
            if let Some(led) = flash.as_ref().map(Flash::led) {
                info!("{}: flash LED {} ({} torch levels)", raw.name, led.name(), led.levels());
                sensor = sensor.with_flash(led.levels());
            }
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
                flash,
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

    /// The flash LED, if the camera has one.
    pub fn flash(&self) -> Option<&Flash> {
        self.flash.as_ref()
    }

    /// Whether a session has the camera open.
    pub fn in_session(&self) -> bool {
        self.in_session.load(Ordering::Acquire)
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
