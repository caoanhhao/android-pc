//! A software ISP for raw Bayer sensors: MIPI RAW10 frames in, YUV 4:2:0 out, with the
//! statistics that drive white balance and exposure.
//!
//! Each 2x2 Bayer block becomes one output pixel (half the sensor size in each
//! direction): no demosaic artifacts, half the noise, and cheap enough for the CPU. The
//! steps per block are black level, white balance and digital gain in linear light, a
//! soft highlight roll-off, sRGB gamma through a table, then BT.601 full range YCbCr with
//! a little extra saturation.
//!
//! [`AutoExposure`] and [`AutoWhiteBalance`] turn the [`Stats`] of a frame into the next
//! sensor exposure and gain and the next white balance gains.

use std::thread;

use crate::image::{chroma_size, ConvertError, Size, Yuv420};
use crate::media::BayerOrder;

/// Black level of the 10-bit Samsung sensors (Tab S7 family).
pub const BLACK_LEVEL: u16 = 64;
/// Largest 10-bit value.
const WHITE_LEVEL: u16 = 1023;
/// Linear values carry this many fraction bits through the pipeline.
const LINEAR_BITS: u32 = 12;
const LINEAR_ONE: i32 = 1 << LINEAR_BITS;
/// Gamma table entries: linear 0..=1.5 (the roll-off keeps values below that).
const GAMMA_ENTRIES: usize = (LINEAR_ONE as usize * 3) / 2 + 1;
/// Extra chroma, in 1/256.
const SATURATION: i32 = 320;
/// Worker threads for one frame.
const THREADS: usize = 4;
/// Statistics look at every STATS_STEP-th block in both directions.
const STATS_STEP: usize = 4;

/// How a raw frame is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLayout {
    /// The sensor frame size.
    pub size: Size,
    /// Bytes per row.
    pub stride: usize,
    /// The Bayer order of the top-left block.
    pub order: BayerOrder,
}

impl RawLayout {
    /// The output size: one pixel per 2x2 block, even in both directions for 4:2:0.
    pub fn output_size(&self) -> Size {
        Size { width: (self.size.width / 2) & !1, height: (self.size.height / 2) & !1 }
    }
}

/// The colour processing of one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Red, green and blue gains (white balance), green normally 1.
    pub wb: [f32; 3],
    /// Gain applied to all channels after white balance.
    pub digital_gain: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { wb: [1.8, 1.0, 1.6], digital_gain: 1.0 }
    }
}

/// What a frame looked like, from a sample of its blocks, in linear light after the
/// black level (0..1 is the sensor range) and before white balance.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    /// Mean red, green and blue of the mid-tone blocks (neither dark nor clipped).
    pub mean: [f32; 3],
    /// Number of mid-tone blocks behind `mean`.
    pub mid_tones: u32,
    /// Median green.
    pub median: f32,
    /// 99th percentile of green.
    pub highlight: f32,
    /// Share of sampled blocks with a clipped channel.
    pub clipped: f32,
    /// Focus measure of the centre: mean squared green difference between neighbouring
    /// blocks relative to the squared mean green there (steep edges count most, so
    /// in-focus point lights win over large blurred ones). Higher is sharper.
    pub sharpness: f32,
}

/// Histogram bins over the 10-bit range.
const BINS: usize = 1024;

#[derive(Clone)]
struct Accumulator {
    sums: [u64; 3],
    mid_tones: u32,
    samples: u32,
    clipped: u32,
    histogram: Vec<u32>,
    /// Sum of green differences and of green, and the count, in the centre.
    edge_sum: u64,
    centre_sum: u64,
    centre_count: u32,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            sums: [0; 3],
            mid_tones: 0,
            samples: 0,
            clipped: 0,
            histogram: vec![0; BINS],
            edge_sum: 0,
            centre_sum: 0,
            centre_count: 0,
        }
    }

    fn add_edge(&mut self, g: u16, previous: u16) {
        self.edge_sum += u64::from(g.abs_diff(previous)).pow(2);
        self.centre_sum += u64::from(g.saturating_sub(BLACK_LEVEL));
        self.centre_count += 1;
    }

    fn add(&mut self, r: u16, g: u16, b: u16, clipped: bool) {
        self.samples += 1;
        self.histogram[usize::from(g.min(WHITE_LEVEL))] += 1;
        if clipped {
            self.clipped += 1;
            return;
        }
        let dark = BLACK_LEVEL + 16;
        if g > dark && r > BLACK_LEVEL && b > BLACK_LEVEL {
            self.mid_tones += 1;
            self.sums[0] += u64::from(r - BLACK_LEVEL);
            self.sums[1] += u64::from(g - BLACK_LEVEL);
            self.sums[2] += u64::from(b - BLACK_LEVEL);
        }
    }

    fn merge(&mut self, other: &Self) {
        for (sum, add) in self.sums.iter_mut().zip(other.sums) {
            *sum += add;
        }
        self.mid_tones += other.mid_tones;
        self.samples += other.samples;
        self.clipped += other.clipped;
        for (bin, add) in self.histogram.iter_mut().zip(&other.histogram) {
            *bin += add;
        }
        self.edge_sum += other.edge_sum;
        self.centre_sum += other.centre_sum;
        self.centre_count += other.centre_count;
    }

    fn stats(&self) -> Stats {
        let range = f32::from(WHITE_LEVEL - BLACK_LEVEL);
        let linear = |raw: usize| (raw as f32 - f32::from(BLACK_LEVEL)).max(0.0) / range;
        let percentile = |fraction: f32| {
            let wanted = (self.samples as f32 * fraction) as u64;
            let mut seen = 0u64;
            for (value, count) in self.histogram.iter().enumerate() {
                seen += u64::from(*count);
                if seen > wanted {
                    return linear(value);
                }
            }
            1.0
        };
        let mean = if self.mid_tones == 0 {
            [0.0; 3]
        } else {
            self.sums.map(|sum| sum as f32 / self.mid_tones as f32 / range)
        };
        Stats {
            mean,
            mid_tones: self.mid_tones,
            median: percentile(0.5),
            highlight: percentile(0.99),
            clipped: if self.samples == 0 { 0.0 } else { self.clipped as f32 / self.samples as f32 },
            sharpness: if self.centre_sum == 0 {
                0.0
            } else {
                let mean = self.centre_sum as f64 / f64::from(self.centre_count);
                (self.edge_sum as f64 / f64::from(self.centre_count) / (mean * mean)) as f32
            },
        }
    }
}

/// The sRGB transfer function from linear [`LINEAR_BITS`] fixed point to 8 bits, with a
/// soft roll-off above 1 so bright areas keep some detail instead of clipping hard.
fn gamma_table() -> Vec<u8> {
    (0..GAMMA_ENTRIES)
        .map(|index| {
            let x = index as f32 / LINEAR_ONE as f32;
            // A shoulder that keeps the low end linear and maps 1.5 to 1.
            let y = (x * (1.0 + x / 2.25) / (1.0 + x)).min(1.0);
            let encoded =
                if y <= 0.003_130_8 { 12.92 * y } else { 1.055 * y.powf(1.0 / 2.4) - 0.055 };
            (encoded * 255.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Unpacks the 10-bit sample at column `x` of a MIPI RAW10 row (4 pixels in 5 bytes).
#[inline]
fn raw10(row: &[u8], x: usize) -> u16 {
    let group = x / 4 * 5;
    let lane = x % 4;
    (u16::from(row[group + lane]) << 2) | u16::from((row[group + 4] >> (2 * lane)) & 3)
}

/// The software ISP. It holds the gamma table; the per-frame [`Settings`] come with each
/// call.
#[derive(Debug, Clone)]
pub struct Isp {
    gamma: Vec<u8>,
}

impl Default for Isp {
    fn default() -> Self {
        Self::new()
    }
}

impl Isp {
    /// An ISP with its tables built.
    pub fn new() -> Self {
        Self { gamma: gamma_table() }
    }

    /// Processes one raw frame into a [`Yuv420`] of [`RawLayout::output_size`] and the
    /// frame's statistics.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] if `data` is shorter than the layout says.
    pub fn process(
        &self,
        data: &[u8],
        layout: RawLayout,
        settings: &Settings,
    ) -> Result<(Yuv420, Stats), ConvertError> {
        let out = layout.output_size();
        let (width, height) = (layout.size.width as usize, layout.size.height as usize);
        if width < 4 || height < 4 || layout.stride * (height - 1) + width * 5 / 4 > data.len() {
            return Err(ConvertError(format!(
                "raw frame of {} bytes for {width}x{height} stride {}",
                data.len(),
                layout.stride
            )));
        }
        let (out_width, out_height) = (out.width as usize, out.height as usize);
        let chroma = chroma_size(out);
        let chroma_width = chroma.width as usize;
        let mut y_plane = vec![0u8; out_width * out_height];
        let mut cb_plane = vec![0u8; chroma_width * chroma.height as usize];
        let mut cr_plane = vec![0u8; cb_plane.len()];

        let gain = |channel: usize| {
            (settings.wb[channel] * settings.digital_gain * LINEAR_ONE as f32 / f32::from(WHITE_LEVEL - BLACK_LEVEL)
                * 4096.0) as i64
        };
        let gains = [gain(0), gain(1), gain(2)];
        let (red_row, red_col) = layout.order.red_at();

        // Bands of output row pairs, one per thread.
        let pairs = out_height / 2;
        let per_band = pairs.div_ceil(THREADS);
        let mut accumulator = Accumulator::new();
        thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut y_rest = y_plane.as_mut_slice();
            let mut cb_rest = cb_plane.as_mut_slice();
            let mut cr_rest = cr_plane.as_mut_slice();
            for band in 0..THREADS {
                let first = band * per_band;
                let count = per_band.min(pairs.saturating_sub(first));
                if count == 0 {
                    break;
                }
                let (y_band, y_next) = y_rest.split_at_mut(count * 2 * out_width);
                let (cb_band, cb_next) = cb_rest.split_at_mut(count * chroma_width);
                let (cr_band, cr_next) = cr_rest.split_at_mut(count * chroma_width);
                (y_rest, cb_rest, cr_rest) = (y_next, cb_next, cr_next);
                let gamma = &self.gamma;
                workers.push(scope.spawn(move || {
                    let mut acc = Accumulator::new();
                    let mut rgb = vec![[0u8; 3]; out_width * 2];
                    for pair in 0..count {
                        let out_row0 = (first + pair) * 2;
                        for half in 0..2 {
                            let block_row = out_row0 + half;
                            let top = &data[2 * block_row * layout.stride..];
                            let bottom = &data[(2 * block_row + 1) * layout.stride..];
                            let rows = [top, bottom];
                            let sample_row = block_row % STATS_STEP == 0;
                            // Focus is measured on every second row of the centre half.
                            let focus_row = block_row % 2 == 0
                                && (out_height / 4..out_height * 3 / 4).contains(&block_row);
                            let focus_columns = out_width / 4..out_width * 3 / 4;
                            let mut previous_g = 0;
                            for x in 0..out_width {
                                let column = |dx: usize| 2 * x + dx;
                                let r = raw10(rows[red_row], column(red_col));
                                let b = raw10(rows[1 - red_row], column(1 - red_col));
                                let g1 = raw10(rows[red_row], column(1 - red_col));
                                let g2 = raw10(rows[1 - red_row], column(red_col));
                                let g = (g1 + g2) / 2;
                                if focus_row && focus_columns.contains(&x) {
                                    if x > focus_columns.start {
                                        acc.add_edge(g, previous_g);
                                    }
                                    previous_g = g;
                                }
                                if sample_row && x % STATS_STEP == 0 {
                                    let clip = WHITE_LEVEL - 8;
                                    acc.add(r, g, b, r >= clip || g1 >= clip || g2 >= clip || b >= clip);
                                }
                                let encode = |value: u16, channel: usize| {
                                    let linear = i64::from(value.saturating_sub(BLACK_LEVEL))
                                        * gains[channel]
                                        >> 12;
                                    gamma[(linear as usize).min(GAMMA_ENTRIES - 1)]
                                };
                                rgb[half * out_width + x] = [encode(r, 0), encode(g, 1), encode(b, 2)];
                            }
                        }
                        write_yuv_pair(
                            &rgb,
                            out_width,
                            &mut y_band[pair * 2 * out_width..][..2 * out_width],
                            &mut cb_band[pair * chroma_width..][..chroma_width],
                            &mut cr_band[pair * chroma_width..][..chroma_width],
                        );
                    }
                    acc
                }));
            }
            for worker in workers {
                accumulator.merge(&worker.join().expect("ISP worker panicked"));
            }
        });
        let image = Yuv420::from_owned_planes(out, y_plane, cb_plane, cr_plane)?;
        Ok((image, accumulator.stats()))
    }
}

/// Converts two rows of sRGB pixels to full range BT.601 Y and one row of 4:2:0 chroma.
fn write_yuv_pair(rgb: &[[u8; 3]], width: usize, y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    for (pixel, out) in rgb.iter().zip(y.iter_mut()) {
        let [r, g, b] = pixel.map(i32::from);
        *out = ((77 * r + 150 * g + 29 * b + 128) >> 8) as u8;
    }
    for (cx, (cb, cr)) in cb.iter_mut().zip(cr.iter_mut()).enumerate() {
        let x = 2 * cx;
        let x1 = (x + 1).min(width - 1);
        let mut sum = [0i32; 3];
        for pixel in [rgb[x], rgb[x1], rgb[width + x], rgb[width + x1]] {
            for (total, value) in sum.iter_mut().zip(pixel) {
                *total += i32::from(value);
            }
        }
        let [r, g, b] = sum;
        // Sums of four pixels: shift by 2 more than the single pixel formula.
        let u = (-43 * r - 85 * g + 128 * b) >> 2;
        let v = (128 * r - 107 * g - 21 * b) >> 2;
        *cb = (128 + ((u * SATURATION) >> 16)).clamp(0, 255) as u8;
        *cr = (128 + ((v * SATURATION) >> 16)).clamp(0, 255) as u8;
    }
}

/// Grey-world white balance over the mid-tones, smoothed over frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoWhiteBalance {
    gains: [f32; 3],
}

impl Default for AutoWhiteBalance {
    fn default() -> Self {
        Self { gains: Settings::default().wb }
    }
}

impl AutoWhiteBalance {
    /// The current red, green and blue gains.
    pub fn gains(&self) -> [f32; 3] {
        self.gains
    }

    /// Moves the gains toward grey world for `stats`; `speed` 1 jumps there at once.
    pub fn update(&mut self, stats: &Stats, speed: f32) {
        if stats.mid_tones < 64 {
            return;
        }
        let [r, g, b] = stats.mean;
        if r <= 0.0 || g <= 0.0 || b <= 0.0 {
            return;
        }
        let target = [(g / r).clamp(0.6, 4.0), 1.0, (g / b).clamp(0.6, 4.0)];
        for (gain, wanted) in self.gains.iter_mut().zip(target) {
            *gain += (wanted - *gain) * speed;
        }
    }
}

/// The sensor's exposure controls, from its sub-device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureLimits {
    /// Exposure in lines, lowest.
    pub min_lines: i32,
    /// Exposure in lines, highest without lengthening the frame.
    pub max_lines: i32,
    /// Analogue gain code for 1x.
    pub min_gain: i32,
    /// Highest analogue gain code.
    pub max_gain: i32,
    /// Seconds per line.
    pub line_time: f64,
}

/// Exposure and gain of one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exposure {
    /// Exposure time in lines.
    pub lines: i32,
    /// Analogue gain code.
    pub gain: i32,
    /// Digital gain in the ISP, used once the sensor is at its limits.
    pub digital: f32,
}

impl Exposure {
    /// Exposure time in nanoseconds.
    pub fn nanos(&self, limits: &ExposureLimits) -> i64 {
        (f64::from(self.lines) * limits.line_time * 1e9) as i64
    }

    /// The total analogue and digital gain as ISO, 100 at unity.
    pub fn iso(&self, limits: &ExposureLimits) -> i32 {
        (100.0 * f64::from(self.gain) / f64::from(limits.min_gain.max(1)) * f64::from(self.digital))
            as i32
    }
}

/// Median green the auto exposure aims for (linear, after black level).
const AE_TARGET: f32 = 0.16;
/// Highest digital gain.
const MAX_DIGITAL_GAIN: f32 = 4.0;

/// Auto exposure: meters the median green and spends exposure time first (up to the
/// frame time), then analogue gain, then digital gain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoExposure {
    limits: ExposureLimits,
    current: Exposure,
    converged: bool,
}

impl AutoExposure {
    /// Starts at 1/60 s (or the longest exposure below it) at unity gain.
    pub fn new(limits: ExposureLimits) -> Self {
        let lines = ((1.0 / 60.0 / limits.line_time) as i32).clamp(limits.min_lines, limits.max_lines);
        Self {
            limits,
            current: Exposure { lines, gain: limits.min_gain, digital: 1.0 },
            converged: false,
        }
    }

    /// Continues from `exposure` (of another mode of the same sensor: lines mean the same
    /// time in every mode) within new `limits`.
    pub fn resume(limits: ExposureLimits, exposure: Exposure) -> Self {
        let mut ae = Self::new(limits);
        let total = ae.total(&exposure);
        ae.current = ae.split(total);
        ae
    }

    /// The exposure the next frames should use.
    pub fn exposure(&self) -> Exposure {
        self.current
    }

    /// The limits it works within.
    pub fn limits(&self) -> &ExposureLimits {
        &self.limits
    }

    /// Whether the last update found the exposure close to the target.
    pub fn converged(&self) -> bool {
        self.converged
    }

    /// The total exposure of `exposure` relative to 1 line at unity gain.
    fn total(&self, exposure: &Exposure) -> f64 {
        f64::from(exposure.lines) * f64::from(exposure.gain) / f64::from(self.limits.min_gain.max(1))
            * f64::from(exposure.digital)
    }

    /// Splits a total exposure into time, analogue gain and digital gain.
    fn split(&self, total: f64) -> Exposure {
        let limits = &self.limits;
        let lines = total.clamp(f64::from(limits.min_lines), f64::from(limits.max_lines));
        let lines = lines.round().max(1.0) as i32;
        let rest = total / f64::from(lines);
        let max_analogue = f64::from(limits.max_gain) / f64::from(limits.min_gain.max(1));
        let analogue = rest.clamp(1.0, max_analogue);
        let gain = ((analogue * f64::from(limits.min_gain)).round() as i32)
            .clamp(limits.min_gain, limits.max_gain);
        let digital = (rest / analogue).clamp(1.0, f64::from(MAX_DIGITAL_GAIN)) as f32;
        Exposure { lines, gain, digital }
    }

    /// Updates from the statistics of a frame taken with the current exposure. `speed`
    /// in (0, 1] damps the step (1 jumps to the estimate).
    pub fn update(&mut self, stats: &Stats, speed: f32) {
        let median = stats.median.max(1e-4);
        let mut ratio = AE_TARGET / median;
        // Keep highlights when the median allows it.
        if stats.highlight * ratio > 0.98 && stats.highlight > 0.0 {
            let protect = 0.98 / stats.highlight;
            if median * protect >= AE_TARGET * 0.4 {
                ratio = protect;
            }
        }
        // A clipped median says little about how much too bright the frame is.
        if stats.median >= 0.97 {
            ratio = 0.25;
        }
        let ratio = ratio.clamp(0.125, 8.0);
        self.converged = (0.85..1.18).contains(&ratio);
        if self.converged {
            return;
        }
        let step = f64::from(ratio).powf(f64::from(speed.clamp(0.05, 1.0)));
        self.current = self.split(self.total(&self.current) * step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs 10-bit samples into MIPI RAW10 rows.
    fn pack(samples: &[u16], width: usize) -> (Vec<u8>, usize) {
        let stride = width * 5 / 4;
        let mut out = Vec::new();
        for row in samples.chunks(width) {
            for group in row.chunks(4) {
                let mut low = 0u8;
                for (lane, sample) in group.iter().enumerate() {
                    out.push((sample >> 2) as u8);
                    low |= ((sample & 3) as u8) << (2 * lane);
                }
                out.push(low);
            }
        }
        (out, stride)
    }

    /// A GBRG frame where every block has the given raw R, G and B.
    fn flat(width: usize, height: usize, [r, g, b]: [u16; 3]) -> (Vec<u8>, RawLayout) {
        let mut samples = vec![0u16; width * height];
        for y in 0..height {
            for x in 0..width {
                samples[y * width + x] = match (y % 2, x % 2) {
                    (0, 0) | (1, 1) => g,
                    (0, 1) => b,
                    _ => r,
                };
            }
        }
        let (data, stride) = pack(&samples, width);
        let size = Size { width: width as u32, height: height as u32 };
        (data, RawLayout { size, stride, order: BayerOrder::Gbrg })
    }

    #[test]
    fn unpacks_raw10() {
        let (data, _) = pack(&[1023, 0, 513, 2], 4);
        assert_eq!([0, 1, 2, 3].map(|x| raw10(&data, x)), [1023, 0, 513, 2]);
    }

    #[test]
    fn grey_stays_grey_and_balanced() {
        // A grey card seen through a sensor with strong green: AWB must undo it.
        let (data, layout) = flat(128, 64, [200, 400, 250]);
        let isp = Isp::new();
        let (_, stats) = isp.process(&data, layout, &Settings::default()).unwrap();
        let mut awb = AutoWhiteBalance::default();
        awb.update(&stats, 1.0);
        let settings = Settings { wb: awb.gains(), digital_gain: 1.0 };
        let (image, _) = isp.process(&data, layout, &settings).unwrap();
        assert_eq!(image.size(), Size { width: 64, height: 32 });
        let [y, cb, cr] = image.planes();
        assert!(y.iter().all(|v| *v == y[0]) && y[0] > 100, "luma {}", y[0]);
        assert!(cb.iter().all(|v| v.abs_diff(128) <= 1), "cb {}", cb[0]);
        assert!(cr.iter().all(|v| v.abs_diff(128) <= 1), "cr {}", cr[0]);
    }

    #[test]
    fn colours_land_in_the_right_channels() {
        // Pure red light on a GBRG sensor: Cr high, Cb low.
        let (data, layout) = flat(16, 8, [900, 64, 64]);
        let settings = Settings { wb: [1.0; 3], digital_gain: 1.0 };
        let (image, stats) = Isp::new().process(&data, layout, &settings).unwrap();
        let [_, cb, cr] = image.planes();
        assert!(cr[0] > 200 && cb[0] < 100, "cb {} cr {}", cb[0], cr[0]);
        assert_eq!(stats.median, 0.0);
    }

    #[test]
    fn rejects_short_frames() {
        let (data, layout) = flat(16, 8, [100, 100, 100]);
        assert!(Isp::new().process(&data[..data.len() - 1], layout, &Settings::default()).is_err());
    }

    fn limits() -> ExposureLimits {
        ExposureLimits { min_lines: 4, max_lines: 3000, min_gain: 32, max_gain: 512, line_time: 1e-5 }
    }

    #[test]
    fn exposure_spends_time_before_gain() {
        let ae = AutoExposure::new(limits());
        assert_eq!(ae.exposure().lines, 1666);
        let bright = ae.split(500.0);
        assert_eq!((bright.lines, bright.gain, bright.digital), (500, 32, 1.0));
        let dark = ae.split(3000.0 * 4.0);
        assert_eq!((dark.lines, dark.gain), (3000, 128));
        let night = ae.split(3000.0 * 16.0 * 2.0);
        assert_eq!((night.gain, night.digital), (512, 2.0));
    }

    #[test]
    fn exposure_converges() {
        let mut ae = AutoExposure::new(limits());
        // The scene: median = total * k.
        let k = 0.16 / 5000.0;
        for _ in 0..20 {
            let median = (ae.total(&ae.exposure()) * k) as f32;
            ae.update(&Stats { median, highlight: median * 2.0, ..Default::default() }, 0.7);
        }
        assert!(ae.converged());
        let total = ae.total(&ae.exposure());
        assert!((4000.0..6000.0).contains(&total), "{total}");
    }
}
