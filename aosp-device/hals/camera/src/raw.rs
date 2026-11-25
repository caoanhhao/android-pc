//! Raw Bayer cameras behind a media controller (Qualcomm CAMSS): finding the sensors,
//! setting up the path from a sensor to a capture node, and turning its frames into YUV
//! with the software ISP while driving the sensor's exposure.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::image::{ConvertError, Size, Yuv420};
use crate::isp::{
    AutoExposure, AutoWhiteBalance, Exposure, ExposureLimits, Isp, RawLayout, Settings, Stats,
};
use crate::media::{
    BayerOrder, BusFormat, ControlRange, Entity, MediaDevice, Route, Subdev, CID_ANALOGUE_GAIN,
    CID_EXPOSURE, CID_FOCUS_ABSOLUTE, CID_HBLANK, CID_PIXEL_RATE, CID_VBLANK,
    FUNCTION_CAMERA_SENSOR,
};
use crate::v4l2::{FourCc, Interval, Mode};

/// `V4L2_CID_CAMERA_ORIENTATION`: 0 front, 1 back, 2 external.
const CID_CAMERA_ORIENTATION: u32 = 0x009a_0922;
/// `V4L2_CID_CAMERA_SENSOR_ROTATION`, degrees.
const CID_CAMERA_SENSOR_ROTATION: u32 = 0x009a_0923;

/// Highest frame rate a raw camera reports.
pub const MAX_FPS: u32 = 30;
/// Frames between exposure updates: a new exposure shows up two frames after it is set.
const AE_PERIOD: u32 = 3;
/// How far each update moves toward the estimate.
const AE_SPEED: f32 = 0.7;
const AWB_SPEED: f32 = 0.25;
/// Frames to skip after moving the lens: the next one is exposed while it moves.
const AF_SETTLE_FRAMES: u32 = 2;
/// The first focus sweep waits for exposure to settle, at most this many frames.
const AF_FIRST_FRAME: u32 = 20;
/// Positions of the coarse focus sweep, over the whole lens range.
const AF_COARSE_STEPS: i32 = 10;

/// Where a camera points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facing {
    /// Away from the user.
    Back,
    /// Toward the user.
    Front,
    /// Plugged in.
    External,
}

/// A raw sensor and the route of its frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSensor {
    /// The media device.
    pub media: PathBuf,
    /// The sensor entity's name (`"s5k3m5 20-002d"`).
    pub name: String,
    /// The sensor's sub-device node.
    pub subdev: PathBuf,
    /// The capture node at the end of its route.
    pub video: PathBuf,
    /// Its 10-bit Bayer media bus code.
    pub code: u32,
    /// The Bayer order of `code`.
    pub order: BayerOrder,
    /// Which way it faces.
    pub facing: Facing,
    /// Clockwise rotation that makes its image upright on the device, degrees.
    pub rotation: i32,
    /// Its modes: capture size and frame interval at the default blanking.
    pub modes: Vec<(Size, Interval)>,
    /// The sub-device of its focus motor, if it has one.
    pub lens: Option<PathBuf>,
}

fn is_pix_path(entity: &Entity) -> bool {
    entity.name.contains("pix")
}

impl RawSensor {
    /// The raw sensors of every `/dev/mediaN`.
    pub fn find_all() -> Vec<Self> {
        let Ok(entries) = fs::read_dir("/dev") else { return Vec::new() };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_str().is_some_and(|n| n.starts_with("media")))
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        let mut sensors = Vec::new();
        for path in paths {
            match Self::find(&path) {
                Ok(found) => sensors.extend(found),
                Err(err) => log::warn!("{}: {err}", path.display()),
            }
        }
        sensors
    }

    /// The raw sensors of the media device at `path`.
    ///
    /// # Errors
    ///
    /// Returns the error of opening the media device.
    pub fn find(path: &Path) -> io::Result<Vec<Self>> {
        let media = MediaDevice::open(path)?;
        let mut sensors = Vec::new();
        for entity in media.entities.iter().filter(|e| e.function == FUNCTION_CAMERA_SENSOR) {
            match Self::probe(path, &media, entity) {
                Ok(Some(sensor)) => sensors.push(sensor),
                Ok(None) => {}
                Err(err) => log::warn!("{}: {err}", entity.name),
            }
        }
        Ok(sensors)
    }

    fn probe(path: &Path, media: &MediaDevice, entity: &Entity) -> io::Result<Option<Self>> {
        let Some(subdev_path) = entity.node.clone() else { return Ok(None) };
        let Some(route) = media.route(entity.id, is_pix_path) else {
            log::info!("{}: no route to a capture node", entity.name);
            return Ok(None);
        };
        let video = route.video_node().map(Path::to_owned).ok_or_else(|| io::Error::other("route without node"))?;
        let subdev = Subdev::open(&subdev_path)?;
        let Some((code, order)) = subdev
            .codes(0)?
            .into_iter()
            .find_map(|code| BayerOrder::of_code(code).map(|order| (code, order)))
        else {
            log::info!("{}: no 10-bit Bayer format", entity.name);
            return Ok(None);
        };
        let facing = match subdev.control(CID_CAMERA_ORIENTATION) {
            Ok(0) => Facing::Front,
            Ok(2) => Facing::External,
            _ => Facing::Back,
        };
        let rotation = subdev.control(CID_CAMERA_SENSOR_ROTATION).unwrap_or(0);

        let saved = subdev.format(0)?;
        let mut modes = Vec::new();
        for (width, height) in subdev.sizes(0, code)? {
            let format = subdev.set_format(0, BusFormat::new(code, width, height))?;
            let size = Size { width: format.width, height: format.height };
            let interval = frame_interval(&subdev, size)?;
            if !modes.iter().any(|(known, _)| *known == size) {
                modes.push((size, interval));
            }
        }
        subdev.set_format(0, saved)?;
        if modes.is_empty() {
            return Ok(None);
        }
        let lens = media.lens_of(entity.id).and_then(|lens| lens.node.clone());
        Ok(Some(Self {
            lens,
            media: path.to_owned(),
            name: entity.name.clone(),
            subdev: subdev_path,
            video,
            code,
            order,
            facing,
            rotation,
            modes,
        }))
    }

    /// The V4L2 capture modes: packed RAW10 at each size, at most [`MAX_FPS`] (the
    /// software ISP keeps up with no more, faster small modes would only drop frames).
    pub fn capture_modes(&self) -> Vec<Mode> {
        self.modes
            .iter()
            .map(|(size, interval)| Mode {
                fourcc: FourCc(self.order.raw10_fourcc()),
                size: *size,
                intervals: vec![if interval.fps() > f64::from(MAX_FPS) {
                    Interval { numerator: 1, denominator: MAX_FPS }
                } else {
                    *interval
                }],
            })
            .collect()
    }

    /// Sets up the route for a capture of `size` and returns the pipeline that processes
    /// its frames, starting from the exposure and white balance of `previous` if given.
    /// Other cameras on the same media device stop working until then.
    ///
    /// # Errors
    ///
    /// Returns the first media, format or control error.
    pub fn start(&self, size: Size, previous: Option<RawPipeline>) -> io::Result<RawPipeline> {
        let mut media = MediaDevice::open(&self.media)?;
        let entity = media
            .entity_named(&self.name)
            .ok_or_else(|| io::Error::other(format!("{} is gone", self.name)))?
            .id;
        let route: Route = media
            .route(entity, is_pix_path)
            .ok_or_else(|| io::Error::other(format!("{}: no route", self.name)))?;
        let subdev = Subdev::open(&self.subdev)?;
        let format = subdev.set_format(0, BusFormat::new(self.code, size.width, size.height))?;
        if (format.width, format.height) != (size.width, size.height) {
            return Err(io::Error::other(format!(
                "{}: {}x{} not accepted (got {}x{})",
                self.name, size.width, size.height, format.width, format.height
            )));
        }
        media.reset_links()?;
        media.configure(&route, format)?;
        let limits = exposure_limits(&subdev, size)?;
        let previous_ae = previous.as_ref().map(|previous| (previous.ae.exposure(), previous.awb));
        // The lens stays powered (and in place) while its node is open, so carry it over.
        let af = match previous.and_then(|previous| previous.af) {
            Some(af) => Some(af),
            None => match &self.lens {
                Some(path) => AutoFocus::open(path)
                    .inspect_err(|err| log::warn!("{}: {err}", path.display()))
                    .ok()
                    .flatten(),
                None => None,
            },
        };
        let mut pipeline = RawPipeline {
            subdev,
            layout: RawLayout { size, stride: 0, order: self.order },
            isp: Isp::new(),
            ae: match previous_ae {
                Some((exposure, _)) => AutoExposure::resume(limits, exposure),
                None => AutoExposure::new(limits),
            },
            awb: previous_ae.map_or_else(AutoWhiteBalance::default, |(_, awb)| awb),
            af,
            frames: 0,
            frames_since_exposure: 0,
            applied: None,
            stats: Stats::default(),
        };
        pipeline.apply_exposure()?;
        Ok(pipeline)
    }
}

/// The frame interval of the sensor's current mode at its default vertical blanking.
fn frame_interval(subdev: &Subdev, size: Size) -> io::Result<Interval> {
    let rate = subdev.control64(CID_PIXEL_RATE)?;
    let hblank = subdev.control(CID_HBLANK).unwrap_or(0);
    let vblank = subdev.control_range(CID_VBLANK)?.map_or(0, |range| range.default);
    let pixels = (i64::from(size.width) + i64::from(hblank)) * (i64::from(size.height) + i64::from(vblank));
    let fps_milli = (rate * 1000 / pixels.max(1)).clamp(1, i64::from(u32::MAX)) as u32;
    Ok(Interval { numerator: 1000, denominator: fps_milli })
}

/// The exposure controls of the sensor's current mode.
fn exposure_limits(subdev: &Subdev, size: Size) -> io::Result<ExposureLimits> {
    let missing = |what: &str| io::Error::other(format!("sensor has no {what} control"));
    let exposure = subdev.control_range(CID_EXPOSURE)?.ok_or_else(|| missing("exposure"))?;
    let gain = subdev.control_range(CID_ANALOGUE_GAIN)?.ok_or_else(|| missing("analogue gain"))?;
    // Keep the default blanking (and frame rate): the exposure maximum follows it.
    if let Some(vblank) = subdev.control_range(CID_VBLANK)? {
        subdev.set_control(CID_VBLANK, vblank.default)?;
    }
    let exposure = subdev.control_range(CID_EXPOSURE)?.unwrap_or(exposure);
    let rate = subdev.control64(CID_PIXEL_RATE)?.max(1);
    let hblank = subdev.control(CID_HBLANK).unwrap_or(0);
    let line_time = (f64::from(size.width) + f64::from(hblank)) / rate as f64;
    // Gain codes of these sensors are linear, with 1x at the bottom of the range (or at
    // the default when the range starts lower).
    let unity = gain.min.max(gain.default.min(gain.max)).max(1);
    Ok(ExposureLimits {
        min_lines: exposure.min.max(1),
        max_lines: exposure.max,
        min_gain: unity,
        max_gain: gain.max,
        line_time,
    })
}

/// A running raw capture: processes frames and keeps exposure and white balance
/// converging.
#[derive(Debug)]
pub struct RawPipeline {
    subdev: Subdev,
    layout: RawLayout,
    isp: Isp,
    ae: AutoExposure,
    awb: AutoWhiteBalance,
    frames_since_exposure: u32,
    applied: Option<Exposure>,
    stats: Stats,
    af: Option<AutoFocus>,
    /// Frames processed so far.
    frames: u32,
}

impl RawPipeline {
    /// Whether the camera has a focus motor.
    pub fn has_focus(&self) -> bool {
        self.af.is_some()
    }

    /// Starts a focus sweep (an AF trigger, or the first frames of a camera).
    pub fn trigger_focus(&mut self) {
        if let Some(af) = &mut self.af {
            af.start();
        }
    }

    /// Whether a focus sweep is running.
    pub fn focusing(&self) -> bool {
        self.af.as_ref().is_some_and(AutoFocus::scanning)
    }

    /// The size of the processed frames.
    pub fn output_size(&self) -> Size {
        self.layout.output_size()
    }

    /// The exposure the sensor was last given.
    pub fn exposure(&self) -> Exposure {
        self.ae.exposure()
    }

    /// The limits of the exposure.
    pub fn limits(&self) -> &ExposureLimits {
        self.ae.limits()
    }

    /// Whether exposure has settled.
    pub fn converged(&self) -> bool {
        self.ae.converged()
    }

    /// The statistics of the last frame.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    fn apply_exposure(&mut self) -> io::Result<()> {
        let wanted = self.ae.exposure();
        let changed = |old: Option<Exposure>, f: fn(&Exposure) -> i32| {
            old.is_none_or(|old| f(&old) != f(&wanted))
        };
        if changed(self.applied, |e| e.gain) {
            self.subdev.set_control(CID_ANALOGUE_GAIN, wanted.gain)?;
        }
        if changed(self.applied, |e| e.lines) {
            self.subdev.set_control(CID_EXPOSURE, wanted.lines)?;
        }
        self.applied = Some(wanted);
        self.frames_since_exposure = 0;
        Ok(())
    }

    /// Processes one frame of `stride` bytes per row, then updates white balance and,
    /// every few frames, the sensor exposure.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] for a short frame.
    pub fn process(&mut self, data: &[u8], stride: usize) -> Result<Yuv420, ConvertError> {
        self.layout.stride = stride;
        let settings = Settings { wb: self.awb.gains(), digital_gain: self.ae.exposure().digital };
        let (image, stats) = self.isp.process(data, self.layout, &settings)?;
        self.stats = stats;
        self.awb.update(&stats, AWB_SPEED);
        self.frames_since_exposure += 1;
        self.frames = self.frames.saturating_add(1);
        let settled = self.ae.converged() || self.frames >= AF_FIRST_FRAME;
        if let Some(af) = self.af.as_mut().filter(|_| settled) {
            af.update(&stats);
        }
        if self.frames_since_exposure >= AE_PERIOD {
            // The ISP's digital gain counts as exposure too.
            let mut metered = stats;
            metered.median *= settings.digital_gain;
            metered.highlight *= settings.digital_gain;
            self.ae.update(&metered, AE_SPEED);
            if let Err(err) = self.apply_exposure() {
                log::warn!("exposure: {err}");
            }
        }
        Ok(image)
    }
}

/// Contrast auto focus: a coarse sweep over the lens range, then a fine one around the
/// sharpest position, then the lens stays at the best one.
#[derive(Debug)]
struct AutoFocus {
    lens: Subdev,
    range: ControlRange,
    /// Positions still to measure, next first.
    pending: Vec<i32>,
    /// Measured positions and their sharpness.
    measured: Vec<(i32, f32)>,
    fine: bool,
    /// Frames to skip before the next measurement.
    wait: u32,
}

impl AutoFocus {
    /// Opens the lens at `path`; `None` if it has no focus control.
    fn open(path: &Path) -> io::Result<Option<Self>> {
        let lens = Subdev::open(path)?;
        let Some(range) = lens.control_range(CID_FOCUS_ABSOLUTE)? else { return Ok(None) };
        let mut af = Self { lens, range, pending: Vec::new(), measured: Vec::new(), fine: false, wait: 0 };
        af.start();
        Ok(Some(af))
    }

    fn scanning(&self) -> bool {
        !self.pending.is_empty()
    }

    fn start(&mut self) {
        let span = self.range.max - self.range.min;
        self.pending = (0..=AF_COARSE_STEPS).map(|step| self.range.min + span * step / AF_COARSE_STEPS).collect();
        self.measured.clear();
        self.fine = false;
        self.move_to(self.pending[0]);
    }

    fn move_to(&mut self, position: i32) {
        if let Err(err) = self.lens.set_control(CID_FOCUS_ABSOLUTE, position) {
            log::warn!("focus {position}: {err}");
        }
        self.wait = AF_SETTLE_FRAMES;
    }

    fn update(&mut self, stats: &Stats) {
        if self.pending.is_empty() {
            return;
        }
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        let position = self.pending.remove(0);
        self.measured.push((position, stats.sharpness));
        if self.pending.is_empty() {
            let best = self.best();
            if !self.fine {
                self.fine = true;
                let step = ((self.range.max - self.range.min) / AF_COARSE_STEPS / 4).max(1);
                self.pending = (-3..=3)
                    .map(|offset| best + offset * step)
                    .filter(|p| (self.range.min..=self.range.max).contains(p))
                    .filter(|p| !self.measured.iter().any(|(known, _)| known == p))
                    .collect();
            }
            if self.pending.is_empty() {
                log::info!("focus at {best} ({:?})", self.measured);
                self.move_to(best);
                return;
            }
        }
        self.move_to(self.pending[0]);
    }

    fn best(&self) -> i32 {
        self.measured
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(self.range.default, |(position, _)| *position)
    }
}
