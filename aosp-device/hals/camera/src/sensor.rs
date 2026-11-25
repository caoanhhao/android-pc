//! What a camera offers Android: output sizes, static characteristics, request templates
//! and result metadata, derived from the V4L2 capture modes.
//!
//! A webcam runs its own exposure, white balance and focus, so this is an `EXTERNAL`
//! hardware level camera, like AOSP's C++ external camera HAL: 3A states are reported as
//! converged, and settings the camera cannot apply are echoed back unchanged. Raw sensors
//! (CAMSS) get the same treatment: the HAL's software ISP runs their 3A on its own, and
//! each 2x2 Bayer block becomes one output pixel, so their output sizes are half the
//! capture sizes.

use std::collections::BTreeMap;

use crate::image::Size;
use crate::metadata::{Metadata, Rational, Tag, Value};
use crate::raw::Facing;
use crate::tags::*;
use crate::v4l2::{FourCc, Interval, Mode};

/// Capture formats the HAL can convert, in order of preference for equal frame rates
/// (YUYV needs no decoding).
pub const FOURCCS: [FourCc; 2] = [FourCc::YUYV, FourCc::MJPEG];

/// Modes slower than this are not offered: a still capture would stall for too long.
const MIN_FPS: f64 = 5.0;

/// Frame rate the request templates ask for when the camera reaches it.
const DEFAULT_FPS: i32 = 30;

/// Frames the HAL keeps in flight per stream: captured, converting, being returned.
pub const PIPELINE_DEPTH: u8 = 4;

/// The `CameraBlob` trailer at the end of a JPEG buffer (`blobId`, `blobSizeBytes`).
pub const JPEG_BLOB_TRAILER_SIZE: usize = 8;

/// `ANDROID_JPEG_MAX_SIZE` floor from camera3.h (`256 KiB + sizeof(CameraBlob)`); the
/// framework sizes JPEG buffers between it and the maximum.
const MIN_JPEG_BUFFER_SIZE: usize = 256 * 1024 + JPEG_BLOB_TRAILER_SIZE;

/// One `ANDROID_JPEG_MAX_SIZE` byte per pixel and a half: generous for webcam JPEGs at
/// quality 90 and below.
const JPEG_BYTES_PER_PIXEL_X2: u64 = 3;

/// Output formats the HAL produces (`android.hardware.graphics.common.PixelFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputFormat {
    /// `BLOB` with `Dataspace::JFIF`: a JPEG image.
    Jpeg,
    /// `YCBCR_420_888`, also what `IMPLEMENTATION_DEFINED` streams get.
    Ycbcr420,
}

/// `PixelFormat::BLOB`.
pub const PIXEL_FORMAT_BLOB: i32 = 0x21;
/// `PixelFormat::IMPLEMENTATION_DEFINED`.
pub const PIXEL_FORMAT_IMPLEMENTATION_DEFINED: i32 = 0x22;
/// `PixelFormat::YCBCR_420_888`.
pub const PIXEL_FORMAT_YCBCR_420_888: i32 = 0x23;

/// The formats advertised for every size. YUV comes first: clients (and VTS
/// `getSessionCharacteristics`) take the first entry as a sample stream with
/// `Dataspace::UNKNOWN`, which a BLOB stream would reject.
const STREAM_FORMATS: [i32; 3] =
    [PIXEL_FORMAT_YCBCR_420_888, PIXEL_FORMAT_IMPLEMENTATION_DEFINED, PIXEL_FORMAT_BLOB];

/// A request template (`RequestTemplate`) this camera can fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Template {
    /// Preview.
    Preview,
    /// Still capture.
    StillCapture,
    /// Video recording.
    VideoRecord,
    /// A still during video recording.
    VideoSnapshot,
}

impl Template {
    /// The template for a `RequestTemplate` value; `None` for MANUAL, ZERO_SHUTTER_LAG
    /// and vendor templates, which a webcam cannot honour.
    pub fn from_aidl(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::Preview),
            2 => Some(Self::StillCapture),
            3 => Some(Self::VideoRecord),
            4 => Some(Self::VideoSnapshot),
            _ => None,
        }
    }

    fn intent(self) -> u8 {
        match self {
            Self::Preview => CONTROL_CAPTURE_INTENT_PREVIEW,
            Self::StillCapture => CONTROL_CAPTURE_INTENT_STILL_CAPTURE,
            Self::VideoRecord => CONTROL_CAPTURE_INTENT_VIDEO_RECORD,
            Self::VideoSnapshot => CONTROL_CAPTURE_INTENT_VIDEO_SNAPSHOT,
        }
    }
}

/// The value of every `*_OFF`, `*_FALSE` and `*_IDLE` enum constant used here.
const OFF: u8 = 0;

/// The capture modes of one camera, one per output size.
#[derive(Debug, Clone, PartialEq)]
pub struct Sensor {
    /// Sorted by area, smallest first.
    modes: Vec<Mode>,
    /// Capture pixels per output pixel in each direction (2 for raw Bayer sensors).
    binning: u32,
    facing: Facing,
    /// `ANDROID_SENSOR_ORIENTATION`.
    orientation: i32,
}

impl Sensor {
    /// Picks one mode per size from the camera's modes: landscape sizes only, at least
    /// [`MIN_FPS`], the format with the highest frame rate (YUYV on a tie).
    ///
    /// Returns `None` if nothing usable is left.
    pub fn new(modes: &[Mode]) -> Option<Self> {
        let mut best: BTreeMap<Size, &Mode> = BTreeMap::new();
        for mode in modes {
            if mode.size.height > mode.size.width
                || mode.size.width == 0
                || max_fps(mode) < MIN_FPS
                || !FOURCCS.contains(&mode.fourcc)
            {
                continue;
            }
            let better = |current: &Mode| {
                let (fps, current_fps) = (max_fps(mode), max_fps(current));
                fps > current_fps
                    || (fps == current_fps && rank(mode.fourcc) < rank(current.fourcc))
            };
            if best.get(&mode.size).is_none_or(|current| better(current)) {
                best.insert(mode.size, mode);
            }
        }
        let mut modes: Vec<Mode> = best.into_values().cloned().collect();
        if modes.is_empty() {
            return None;
        }
        modes.sort_by_key(|mode| (mode.size.area(), mode.size.width));
        Some(Self { modes, binning: 1, facing: Facing::External, orientation: 0 })
    }

    /// A raw Bayer sensor built into the device: its RAW10 `modes`, where it faces and
    /// how its image is rotated. Returns `None` without a mode at [`MIN_FPS`] or more.
    pub fn raw(modes: &[Mode], facing: Facing, orientation: i32) -> Option<Self> {
        let mut modes: Vec<Mode> = modes
            .iter()
            .filter(|mode| max_fps(mode) >= MIN_FPS && mode.size.width >= 4 && mode.size.height >= 4)
            .cloned()
            .collect();
        if modes.is_empty() {
            return None;
        }
        modes.sort_by_key(|mode| (mode.size.area(), mode.size.width));
        modes.dedup_by_key(|mode| mode.size);
        Some(Self { modes, binning: 2, facing, orientation: orientation.rem_euclid(360) })
    }

    /// The size of the frames `mode` turns into: the capture size, or for raw sensors one
    /// pixel per 2x2 block, rounded to even sizes for 4:2:0.
    fn output(&self, mode: &Mode) -> Size {
        if self.binning == 1 {
            return mode.size;
        }
        Size {
            width: (mode.size.width / self.binning) & !1,
            height: (mode.size.height / self.binning) & !1,
        }
    }

    /// Output sizes, smallest first.
    pub fn sizes(&self) -> impl Iterator<Item = Size> + '_ {
        self.modes.iter().map(|mode| self.output(mode))
    }

    /// The largest size, which is also the active pixel array.
    pub fn max_size(&self) -> Size {
        self.output(self.modes.last().expect("a sensor has at least one mode"))
    }

    /// Whether `size` is an output size.
    pub fn supports(&self, size: Size) -> bool {
        self.sizes().any(|output| output == size)
    }

    /// The capture mode for a set of output sizes: the smallest mode at least as wide and
    /// as high as every output (each output is a centred crop of it), else the largest.
    pub fn mode_for(&self, outputs: &[Size]) -> &Mode {
        let width = outputs.iter().map(|size| size.width).max().unwrap_or(0);
        let height = outputs.iter().map(|size| size.height).max().unwrap_or(0);
        self.modes
            .iter()
            .find(|mode| {
                let output = self.output(mode);
                output.width >= width && output.height >= height
            })
            .unwrap_or_else(|| self.modes.last().expect("a sensor has at least one mode"))
    }

    /// The frame rates the camera offers, ascending, as whole frames per second.
    fn frame_rates(&self) -> Vec<i32> {
        let mut rates: Vec<i32> = self
            .modes
            .iter()
            .flat_map(|mode| mode.intervals.iter().map(|interval| interval.fps().round() as i32))
            .filter(|fps| f64::from(*fps) >= MIN_FPS)
            .collect();
        rates.sort_unstable();
        rates.dedup();
        rates
    }

    /// `ANDROID_JPEG_MAX_SIZE`: the buffer size for a JPEG at the largest size.
    pub fn jpeg_max_size(&self) -> usize {
        let bytes = self.max_size().area() * JPEG_BYTES_PER_PIXEL_X2 / 2;
        usize::try_from(bytes).unwrap_or(usize::MAX).max(2 * MIN_JPEG_BUFFER_SIZE)
    }

    /// The static characteristics (`getCameraCharacteristics`).
    pub fn characteristics(&self) -> Metadata {
        let mut md = Metadata::new();
        md.set(INFO_SUPPORTED_HARDWARE_LEVEL, &[INFO_SUPPORTED_HARDWARE_LEVEL_EXTERNAL]);
        md.set(
            REQUEST_AVAILABLE_CAPABILITIES,
            &[REQUEST_AVAILABLE_CAPABILITIES_BACKWARD_COMPATIBLE],
        );

        // 3A: the camera runs its own; the HAL offers only automatic modes.
        md.set(COLOR_CORRECTION_AVAILABLE_ABERRATION_MODES, &[OFF]);
        md.set(CONTROL_AE_AVAILABLE_ANTIBANDING_MODES, &[CONTROL_AE_ANTIBANDING_MODE_AUTO]);
        md.set(CONTROL_AE_AVAILABLE_MODES, &[CONTROL_AE_MODE_ON]);
        md.set(CONTROL_AE_COMPENSATION_RANGE, &[0, 0]);
        md.set(CONTROL_AE_COMPENSATION_STEP, &[Rational { numerator: 0, denominator: 1 }]);
        md.set(CONTROL_AE_LOCK_AVAILABLE, &[OFF]);
        md.set(CONTROL_AF_AVAILABLE_MODES, &[OFF, CONTROL_AF_MODE_AUTO]);
        md.set(CONTROL_AVAILABLE_EFFECTS, &[OFF]);
        md.set(CONTROL_AVAILABLE_MODES, &[OFF, CONTROL_MODE_AUTO]);
        md.set(CONTROL_AVAILABLE_SCENE_MODES, &[CONTROL_SCENE_MODE_DISABLED]);
        md.set(CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES, &[OFF]);
        md.set(CONTROL_AWB_AVAILABLE_MODES, &[CONTROL_AWB_MODE_AUTO]);
        md.set(CONTROL_AWB_LOCK_AVAILABLE, &[OFF]);
        md.set(CONTROL_MAX_REGIONS, &[0, 0, 0]);
        let fps_ranges: Vec<i32> =
            self.frame_rates().iter().flat_map(|fps| [(fps / 2).max(1), *fps]).collect();
        md.set(CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES, &fps_ranges);

        md.set(EDGE_AVAILABLE_EDGE_MODES, &[OFF]);
        md.set(FLASH_INFO_AVAILABLE, &[OFF]);
        md.set(HOT_PIXEL_AVAILABLE_HOT_PIXEL_MODES, &[OFF]);
        md.set(JPEG_AVAILABLE_THUMBNAIL_SIZES, &[0, 0]);
        md.set(JPEG_MAX_SIZE, &[i32::try_from(self.jpeg_max_size()).unwrap_or(i32::MAX)]);
        let facing = match self.facing {
            Facing::Back => LENS_FACING_BACK,
            Facing::Front => LENS_FACING_FRONT,
            Facing::External => LENS_FACING_EXTERNAL,
        };
        md.set(LENS_FACING, &[facing]);
        md.set(LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION, &[OFF]);
        md.set(LENS_INFO_FOCUS_DISTANCE_CALIBRATION, &[OFF]);
        md.set(NOISE_REDUCTION_AVAILABLE_NOISE_REDUCTION_MODES, &[OFF]);

        md.set(REQUEST_PARTIAL_RESULT_COUNT, &[1]);
        md.set(REQUEST_PIPELINE_MAX_DEPTH, &[PIPELINE_DEPTH]);
        // Raw, processed (YUV), stalling (JPEG) outputs at once.
        md.set(REQUEST_MAX_NUM_OUTPUT_STREAMS, &[0, MAX_PROCESSED_STREAMS, MAX_STALL_STREAMS]);
        md.set(REQUEST_MAX_NUM_INPUT_STREAMS, &[0]);

        md.set(SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, &[1.0]);
        md.set(SCALER_CROPPING_TYPE, &[SCALER_CROPPING_TYPE_CENTER_ONLY]);
        self.set_stream_configurations(&mut md);

        let max = self.max_size();
        let (width, height) = (max.width as i32, max.height as i32);
        md.set(SENSOR_INFO_ACTIVE_ARRAY_SIZE, &[0, 0, width, height]);
        md.set(SENSOR_INFO_PRE_CORRECTION_ACTIVE_ARRAY_SIZE, &[0, 0, width, height]);
        md.set(SENSOR_INFO_PIXEL_ARRAY_SIZE, &[width, height]);
        let slowest = self.frame_rates().first().copied().unwrap_or(DEFAULT_FPS);
        md.set(SENSOR_INFO_MAX_FRAME_DURATION, &[1_000_000_000 / i64::from((slowest / 2).max(1))]);
        md.set(SENSOR_INFO_TIMESTAMP_SOURCE, &[SENSOR_INFO_TIMESTAMP_SOURCE_UNKNOWN]);
        // A webcam: a PC display is landscape, like the camera.
        md.set(SENSOR_ORIENTATION, &[self.orientation]);
        md.set(SENSOR_AVAILABLE_TEST_PATTERN_MODES, &[0]);
        md.set(SENSOR_READOUT_TIMESTAMP, &[OFF]);

        md.set(SHADING_AVAILABLE_MODES, &[OFF]);
        md.set(STATISTICS_INFO_AVAILABLE_FACE_DETECT_MODES, &[OFF]);
        md.set(STATISTICS_INFO_AVAILABLE_HOT_PIXEL_MAP_MODES, &[OFF]);
        md.set(STATISTICS_INFO_AVAILABLE_LENS_SHADING_MAP_MODES, &[OFF]);
        md.set(STATISTICS_INFO_MAX_FACE_COUNT, &[0]);
        md.set(SYNC_MAX_LATENCY, &[SYNC_MAX_LATENCY_UNKNOWN]);

        md.set(REQUEST_AVAILABLE_REQUEST_KEYS, &ids(&REQUEST_KEYS));
        md.set(REQUEST_AVAILABLE_RESULT_KEYS, &ids(&RESULT_KEYS));
        let mut keys: Vec<i32> = md.tags().map(as_key).collect();
        keys.push(as_key(REQUEST_AVAILABLE_CHARACTERISTICS_KEYS.id()));
        keys.sort_unstable();
        md.set(REQUEST_AVAILABLE_CHARACTERISTICS_KEYS, &keys);
        md
    }

    fn set_stream_configurations(&self, md: &mut Metadata) {
        let mut configurations = Vec::new();
        let mut durations = Vec::new();
        let mut stalls = Vec::new();
        for mode in self.modes.iter().rev() {
            let output = self.output(mode);
            let (width, height) = (output.width as i32, output.height as i32);
            let frame = mode.intervals.first().map_or(0, |interval| interval.nanos());
            for format in STREAM_FORMATS {
                configurations.extend([
                    format,
                    width,
                    height,
                    SCALER_AVAILABLE_STREAM_CONFIGURATIONS_OUTPUT.into(),
                ]);
                let (format, width, height) =
                    (i64::from(format), i64::from(width), i64::from(height));
                durations.extend([format, width, height, frame]);
                // A JPEG is encoded on the CPU after the frame; YUV outputs never stall.
                let stall = if format == i64::from(PIXEL_FORMAT_BLOB) { frame } else { 0 };
                stalls.extend([format, width, height, stall]);
            }
        }
        md.set(SCALER_AVAILABLE_STREAM_CONFIGURATIONS, &configurations);
        md.set(SCALER_AVAILABLE_MIN_FRAME_DURATIONS, &durations);
        md.set(SCALER_AVAILABLE_STALL_DURATIONS, &stalls);
    }

    /// The default settings for `template` (`constructDefaultRequestSettings`).
    pub fn default_request(&self, template: Template) -> Metadata {
        let mut md = Metadata::new();
        md.set(COLOR_CORRECTION_ABERRATION_MODE, &[OFF]);
        md.set(CONTROL_AE_ANTIBANDING_MODE, &[CONTROL_AE_ANTIBANDING_MODE_AUTO]);
        md.set(CONTROL_AE_EXPOSURE_COMPENSATION, &[0]);
        md.set(CONTROL_AE_LOCK, &[OFF]);
        md.set(CONTROL_AE_MODE, &[CONTROL_AE_MODE_ON]);
        md.set(CONTROL_AE_PRECAPTURE_TRIGGER, &[CONTROL_AE_PRECAPTURE_TRIGGER_IDLE]);
        let rates = self.frame_rates();
        let fps = if rates.contains(&DEFAULT_FPS) {
            DEFAULT_FPS
        } else {
            rates.last().copied().unwrap_or(DEFAULT_FPS)
        };
        md.set(CONTROL_AE_TARGET_FPS_RANGE, &[(fps / 2).max(1), fps]);
        md.set(CONTROL_AF_MODE, &[CONTROL_AF_MODE_AUTO]);
        md.set(CONTROL_AF_TRIGGER, &[CONTROL_AF_TRIGGER_IDLE]);
        md.set(CONTROL_AWB_LOCK, &[OFF]);
        md.set(CONTROL_AWB_MODE, &[CONTROL_AWB_MODE_AUTO]);
        md.set(CONTROL_CAPTURE_INTENT, &[template.intent()]);
        md.set(CONTROL_EFFECT_MODE, &[OFF]);
        md.set(CONTROL_MODE, &[CONTROL_MODE_AUTO]);
        md.set(CONTROL_SCENE_MODE, &[CONTROL_SCENE_MODE_DISABLED]);
        md.set(CONTROL_VIDEO_STABILIZATION_MODE, &[OFF]);
        md.set(FLASH_MODE, &[OFF]);
        md.set(JPEG_ORIENTATION, &[0]);
        md.set(JPEG_QUALITY, &[DEFAULT_JPEG_QUALITY]);
        md.set(JPEG_THUMBNAIL_QUALITY, &[DEFAULT_JPEG_QUALITY]);
        md.set(JPEG_THUMBNAIL_SIZE, &[0, 0]);
        md.set(LENS_OPTICAL_STABILIZATION_MODE, &[OFF]);
        md.set(NOISE_REDUCTION_MODE, &[OFF]);
        let max = self.max_size();
        md.set(SCALER_CROP_REGION, &[0, 0, max.width as i32, max.height as i32]);
        md.set(SENSOR_TEST_PATTERN_MODE, &[0]);
        md.set(STATISTICS_FACE_DETECT_MODE, &[OFF]);
        md.set(STATISTICS_HOT_PIXEL_MAP_MODE, &[OFF]);
        md
    }

    /// The frame interval of `mode` closest to the highest rate of `settings`'
    /// `CONTROL_AE_TARGET_FPS_RANGE` (the fastest interval without one).
    pub fn interval_for(mode: &Mode, settings: &Metadata) -> Interval {
        let fastest = mode.intervals[0];
        let range = settings.get(CONTROL_AE_TARGET_FPS_RANGE).unwrap_or_default();
        let [_, max] = range[..] else {
            return fastest;
        };
        let wanted = f64::from(max);
        mode.intervals
            .iter()
            .copied()
            .min_by(|a, b| (a.fps() - wanted).abs().total_cmp(&(b.fps() - wanted).abs()))
            .unwrap_or(fastest)
    }

    /// The result metadata of a capture: the settings it was taken with plus the dynamic
    /// state a webcam always has. `af_locked` reports a focus lock after an AF trigger.
    pub fn result(&self, settings: &Metadata, timestamp: i64, af_locked: bool) -> Metadata {
        let mut md = settings.clone();
        let af_state =
            if af_locked { CONTROL_AF_STATE_FOCUSED_LOCKED } else { CONTROL_AF_STATE_INACTIVE };
        md.set(CONTROL_AF_STATE, &[af_state]);
        md.set(CONTROL_AE_STATE, &[CONTROL_AE_STATE_CONVERGED]);
        md.set(CONTROL_AE_LOCK, &[OFF]);
        md.set(CONTROL_AWB_STATE, &[CONTROL_AWB_STATE_CONVERGED]);
        md.set(CONTROL_AWB_LOCK, &[OFF]);
        md.set(FLASH_STATE, &[FLASH_STATE_UNAVAILABLE]);
        md.set(REQUEST_PIPELINE_DEPTH, &[PIPELINE_DEPTH]);
        let max = self.max_size();
        md.set(SCALER_CROP_REGION, &[0, 0, max.width as i32, max.height as i32]);
        md.set(SENSOR_TIMESTAMP, &[timestamp]);
        md.set(STATISTICS_LENS_SHADING_MAP_MODE, &[OFF]);
        md.set(STATISTICS_SCENE_FLICKER, &[STATISTICS_SCENE_FLICKER_NONE]);
        md
    }

    /// The session characteristics (`getSessionCharacteristics`): only the zoom limit.
    pub fn session_characteristics() -> Metadata {
        let mut md = Metadata::new();
        md.set(SCALER_AVAILABLE_MAX_DIGITAL_ZOOM, &[1.0]);
        md
    }
}

/// YUV outputs at once.
pub const MAX_PROCESSED_STREAMS: i32 = 2;
/// JPEG outputs at once.
pub const MAX_STALL_STREAMS: i32 = 1;
/// JPEG quality of the templates.
pub const DEFAULT_JPEG_QUALITY: u8 = 90;

/// The AF state a capture reports, from the AF trigger of its settings: a webcam
/// focuses on its own, so a trigger "locks" at once and a cancel unlocks.
pub fn af_locked_after(settings: &Metadata, locked: bool) -> bool {
    match settings.first(CONTROL_AF_TRIGGER) {
        Some(CONTROL_AF_TRIGGER_START) => true,
        Some(CONTROL_AF_TRIGGER_CANCEL) => false,
        _ => locked,
    }
}

/// The JPEG quality asked for by `settings`.
pub fn jpeg_quality(settings: &Metadata) -> u8 {
    settings.first(JPEG_QUALITY).unwrap_or(DEFAULT_JPEG_QUALITY)
}

fn max_fps(mode: &Mode) -> f64 {
    mode.intervals.first().map_or(0.0, |interval| interval.fps())
}

fn rank(fourcc: FourCc) -> usize {
    FOURCCS.iter().position(|candidate| *candidate == fourcc).unwrap_or(FOURCCS.len())
}

/// Tag numbers are `i32` in the key lists; every Android tag fits.
fn as_key(id: u32) -> i32 {
    i32::try_from(id).expect("android metadata tags are below 2^31")
}

/// A tag of any element type, for the key lists.
#[derive(Debug, Clone, Copy)]
struct Key(u32);

impl<T: Value> From<Tag<T>> for Key {
    fn from(tag: Tag<T>) -> Self {
        Self(tag.id())
    }
}

fn ids(keys: &[Key]) -> Vec<i32> {
    keys.iter().map(|key| as_key(key.0)).collect()
}

macro_rules! keys {
    ($($tag:ident),* $(,)?) => { [$(Key($tag.id())),*] };
}

/// Settings a request may carry (`ANDROID_REQUEST_AVAILABLE_REQUEST_KEYS`).
const REQUEST_KEYS: [Key; 27] = keys![
    COLOR_CORRECTION_ABERRATION_MODE,
    CONTROL_AE_ANTIBANDING_MODE,
    CONTROL_AE_EXPOSURE_COMPENSATION,
    CONTROL_AE_LOCK,
    CONTROL_AE_MODE,
    CONTROL_AE_PRECAPTURE_TRIGGER,
    CONTROL_AE_TARGET_FPS_RANGE,
    CONTROL_AF_MODE,
    CONTROL_AF_TRIGGER,
    CONTROL_AWB_LOCK,
    CONTROL_AWB_MODE,
    CONTROL_CAPTURE_INTENT,
    CONTROL_EFFECT_MODE,
    CONTROL_MODE,
    CONTROL_SCENE_MODE,
    CONTROL_VIDEO_STABILIZATION_MODE,
    FLASH_MODE,
    JPEG_ORIENTATION,
    JPEG_QUALITY,
    JPEG_THUMBNAIL_QUALITY,
    JPEG_THUMBNAIL_SIZE,
    LENS_OPTICAL_STABILIZATION_MODE,
    NOISE_REDUCTION_MODE,
    SCALER_CROP_REGION,
    SENSOR_TEST_PATTERN_MODE,
    STATISTICS_FACE_DETECT_MODE,
    STATISTICS_HOT_PIXEL_MAP_MODE,
];

/// Entries a result carries (`ANDROID_REQUEST_AVAILABLE_RESULT_KEYS`).
const RESULT_KEYS: [Key; 34] = keys![
    COLOR_CORRECTION_ABERRATION_MODE,
    CONTROL_AE_ANTIBANDING_MODE,
    CONTROL_AE_EXPOSURE_COMPENSATION,
    CONTROL_AE_LOCK,
    CONTROL_AE_MODE,
    CONTROL_AE_PRECAPTURE_TRIGGER,
    CONTROL_AE_STATE,
    CONTROL_AE_TARGET_FPS_RANGE,
    CONTROL_AF_MODE,
    CONTROL_AF_STATE,
    CONTROL_AF_TRIGGER,
    CONTROL_AWB_LOCK,
    CONTROL_AWB_MODE,
    CONTROL_AWB_STATE,
    CONTROL_CAPTURE_INTENT,
    CONTROL_EFFECT_MODE,
    CONTROL_MODE,
    CONTROL_SCENE_MODE,
    CONTROL_VIDEO_STABILIZATION_MODE,
    FLASH_MODE,
    FLASH_STATE,
    JPEG_ORIENTATION,
    JPEG_QUALITY,
    JPEG_THUMBNAIL_QUALITY,
    JPEG_THUMBNAIL_SIZE,
    LENS_OPTICAL_STABILIZATION_MODE,
    NOISE_REDUCTION_MODE,
    REQUEST_PIPELINE_DEPTH,
    SCALER_CROP_REGION,
    SENSOR_TIMESTAMP,
    STATISTICS_FACE_DETECT_MODE,
    STATISTICS_HOT_PIXEL_MAP_MODE,
    STATISTICS_LENS_SHADING_MAP_MODE,
    STATISTICS_SCENE_FLICKER,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(fourcc: FourCc, width: u32, height: u32, fps: &[u32]) -> Mode {
        Mode {
            fourcc,
            size: Size { width, height },
            intervals: fps.iter().map(|fps| Interval { numerator: 1, denominator: *fps }).collect(),
        }
    }

    /// A UVC webcam: YUYV is slow at large sizes, MJPEG is fast everywhere.
    fn webcam() -> Sensor {
        Sensor::new(&[
            mode(FourCc::YUYV, 640, 480, &[30, 15]),
            mode(FourCc::YUYV, 1920, 1080, &[5]),
            mode(FourCc::MJPEG, 640, 480, &[30, 15]),
            mode(FourCc::MJPEG, 1920, 1080, &[30, 15]),
            mode(FourCc::MJPEG, 1280, 720, &[30]),
            mode(FourCc::MJPEG, 480, 640, &[30]),
            mode(FourCc::YUYV, 3840, 2160, &[1]),
        ])
        .unwrap()
    }

    #[test]
    fn picks_one_mode_per_landscape_size() {
        let sensor = webcam();
        let picked: Vec<(FourCc, u32)> =
            sensor.modes.iter().map(|mode| (mode.fourcc, mode.size.width)).collect();
        assert_eq!(picked, [(FourCc::YUYV, 640), (FourCc::MJPEG, 1280), (FourCc::MJPEG, 1920)]);
        assert_eq!(sensor.max_size(), Size { width: 1920, height: 1080 });
        assert!(Sensor::new(&[mode(FourCc::YUYV, 640, 480, &[1])]).is_none());
    }

    #[test]
    fn mode_covers_every_output() {
        let sensor = webcam();
        let small = Size { width: 640, height: 480 };
        let wide = Size { width: 1280, height: 720 };
        assert_eq!(sensor.mode_for(&[small]).size, small);
        assert_eq!(sensor.mode_for(&[small, wide]).size, wide);
        assert_eq!(sensor.mode_for(&[Size { width: 4000, height: 3000 }]).size.width, 1920);
    }

    #[test]
    fn characteristics_list_every_size_and_key() {
        let md = webcam().characteristics();
        let configs = md.get(SCALER_AVAILABLE_STREAM_CONFIGURATIONS).unwrap();
        assert_eq!(configs.len(), 3 * 3 * 4);
        assert_eq!(&configs[..4], &[PIXEL_FORMAT_YCBCR_420_888, 1920, 1080, 0]);
        let keys = md.get(REQUEST_AVAILABLE_CHARACTERISTICS_KEYS).unwrap();
        assert_eq!(keys.len(), md.len());
        assert!(md.tags().all(|tag| keys.contains(&as_key(tag))));
        assert_eq!(md.get(CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES).unwrap(), [7, 15, 15, 30]);
        assert_eq!(md.first(SENSOR_INFO_MAX_FRAME_DURATION), Some(1_000_000_000 / 7));
        assert!(webcam().jpeg_max_size() > MIN_JPEG_BUFFER_SIZE);
    }

    #[test]
    fn raw_sensors_output_half_size() {
        let raw = FourCc(u32::from_le_bytes(*b"pGAA"));
        let sensor = Sensor::raw(
            &[mode(raw, 4128, 3096, &[30]), mode(raw, 2064, 1548, &[30]), mode(raw, 100, 100, &[2])],
            Facing::Back,
            -270,
        )
        .unwrap();
        assert_eq!(sensor.max_size(), Size { width: 2064, height: 1548 });
        assert_eq!(sensor.sizes().collect::<Vec<_>>()[0], Size { width: 1032, height: 774 });
        assert_eq!(sensor.mode_for(&[Size { width: 1920, height: 1080 }]).size.width, 4128);
        assert_eq!(sensor.mode_for(&[Size { width: 640, height: 480 }]).size.width, 2064);
        let md = sensor.characteristics();
        assert_eq!(md.first(LENS_FACING), Some(LENS_FACING_BACK));
        assert_eq!(md.first(SENSOR_ORIENTATION), Some(90));
    }

    #[test]
    fn templates_and_intervals() {
        let sensor = webcam();
        let preview = sensor.default_request(Template::Preview);
        assert_eq!(preview.get(CONTROL_AE_TARGET_FPS_RANGE).unwrap(), [15, 30]);
        assert_eq!(Template::from_aidl(6), None);
        let mode = mode(FourCc::MJPEG, 640, 480, &[30, 15]);
        let mut settings = Metadata::new();
        settings.set(CONTROL_AE_TARGET_FPS_RANGE, &[15, 15]);
        assert_eq!(Sensor::interval_for(&mode, &settings).denominator, 15);
        assert_eq!(Sensor::interval_for(&mode, &Metadata::new()).denominator, 30);
    }

    #[test]
    fn results_echo_settings() {
        let sensor = webcam();
        let mut settings = sensor.default_request(Template::StillCapture);
        settings.set(CONTROL_AF_TRIGGER, &[CONTROL_AF_TRIGGER_START]);
        assert!(af_locked_after(&settings, false));
        let result = sensor.result(&settings, 42, true);
        assert_eq!(result.first(SENSOR_TIMESTAMP), Some(42));
        assert_eq!(result.first(CONTROL_AF_STATE), Some(CONTROL_AF_STATE_FOCUSED_LOCKED));
        assert_eq!(
            result.first(CONTROL_CAPTURE_INTENT),
            Some(CONTROL_CAPTURE_INTENT_STILL_CAPTURE)
        );
        assert_eq!(jpeg_quality(&result), DEFAULT_JPEG_QUALITY);
    }
}
