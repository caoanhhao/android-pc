//! Bring-up tool for media controller cameras (CAMSS): prints the graph and the sensors,
//! and captures raw Bayer frames to a file.
//!
//! ```text
//! pccamera-raw graph [/dev/mediaN]
//! pccamera-raw capture SENSOR OUT [--size WxH] [--frames N] [--exposure LINES]
//!                                 [--gain CODE] [--media /dev/mediaN]
//! pccamera-raw still SENSOR OUT.yuv [--size WxH] [--frames N]
//! ```
//!
//! `still` runs the HAL's own pipeline (software ISP, auto exposure and white balance)
//! for N frames and writes the last one as I420.
//!
//! SENSOR is part of the sensor entity's name (`s5k3m5`). The frame is written as packed
//! MIPI RAW10 rows of the stride printed on stdout.

use std::fs;
use std::process::ExitCode;
use std::time::Duration;

use pccamera::image::Size;
use pccamera::media::{
    BayerOrder, BusFormat, MediaDevice, Subdev, CID_ANALOGUE_GAIN, CID_EXPOSURE, CID_HBLANK,
    CID_PIXEL_RATE, CID_VBLANK, FUNCTION_CAMERA_SENSOR,
};
use pccamera::raw::RawSensor;
use pccamera::v4l2::{Device, FourCc, Interval, Mode, Stream};

const DEFAULT_MEDIA: &str = "/dev/media0";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("graph") => graph(args.get(1).map_or(DEFAULT_MEDIA, String::as_str)),
        Some("capture") => capture(&args[1..]),
        Some("still") => still(&args[1..]),
        _ => Err("usage: pccamera-raw graph [MEDIA] | capture SENSOR OUT [options]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("pccamera-raw: {err}");
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, String>;

fn graph(path: &str) -> Result<()> {
    let media = MediaDevice::open(path).map_err(|err| format!("{path}: {err}"))?;
    print!("{}", media.describe());
    for entity in media.entities.iter().filter(|e| e.function == FUNCTION_CAMERA_SENSOR) {
        let Some(node) = &entity.node else { continue };
        let subdev = Subdev::open(node).map_err(|err| format!("{}: {err}", node.display()))?;
        println!("\nsensor {} ({})", entity.name, node.display());
        if let Ok(format) = subdev.format(0) {
            println!("  active {:#x} {}x{}", format.code, format.width, format.height);
        }
        for code in subdev.codes(0).unwrap_or_default() {
            let sizes = subdev.sizes(0, code).unwrap_or_default();
            println!("  code {code:#x}: {sizes:?}");
        }
        for (name, id) in [
            ("exposure", CID_EXPOSURE),
            ("analogue_gain", CID_ANALOGUE_GAIN),
            ("vblank", CID_VBLANK),
            ("hblank", CID_HBLANK),
            ("orientation", 0x009a_0922),
            ("rotation", 0x009a_0923),
        ] {
            if let Ok(Some(range)) = subdev.control_range(id) {
                let value = subdev.control(id).unwrap_or(-1);
                println!("  {name} = {value} {range:?}");
            }
        }
        if let Ok(rate) = subdev.control64(CID_PIXEL_RATE) {
            println!("  pixel_rate = {rate}");
        }
        match media.route(entity.id, |e| e.name.contains("pix")) {
            Some(route) => {
                let names: Vec<&str> = route.hops.iter().map(|h| h.entity.name.as_str()).collect();
                println!("  route: {}", names.join(" -> "));
            }
            None => println!("  no route to a video node"),
        }
    }
    Ok(())
}

fn value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|arg| arg == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn number(args: &[String], name: &str) -> Result<Option<i32>> {
    value(args, name).map(|v| v.parse().map_err(|_| format!("bad {name} {v}"))).transpose()
}

fn capture(args: &[String]) -> Result<()> {
    let [wanted, out, ..] = args else {
        return Err("capture SENSOR OUT [options]".into());
    };
    let media_path = value(args, "--media").unwrap_or(DEFAULT_MEDIA);
    let mut media = MediaDevice::open(media_path).map_err(|err| format!("{media_path}: {err}"))?;
    let sensor = media
        .entities
        .iter()
        .find(|e| e.function == FUNCTION_CAMERA_SENSOR && e.name.contains(wanted.as_str()))
        .cloned()
        .ok_or_else(|| format!("no sensor named like {wanted}"))?;
    let route = media
        .route(sensor.id, |e| e.name.contains("pix"))
        .ok_or_else(|| format!("{}: no route to a video node", sensor.name))?;
    let sensor_node = route.sensor_node().ok_or("sensor has no sub-device node")?.to_owned();
    let video = route.video_node().ok_or("route ends without a node")?.to_owned();
    let subdev = Subdev::open(&sensor_node).map_err(|err| err.to_string())?;

    let current = subdev.format(0).map_err(|err| format!("sensor format: {err}"))?;
    let mut format = BusFormat::new(current.code, current.width, current.height);
    if let Some(size) = value(args, "--size") {
        let (w, h) = size.split_once('x').ok_or("--size WxH")?;
        format.width = w.parse().map_err(|_| "bad width")?;
        format.height = h.parse().map_err(|_| "bad height")?;
    }
    let format = subdev.set_format(0, format).map_err(|err| format!("sensor S_FMT: {err}"))?;
    let order =
        BayerOrder::of_code(format.code).ok_or(format!("not a 10-bit Bayer code: {:#x}", format.code))?;

    media.reset_links().map_err(|err| format!("reset links: {err}"))?;
    media.configure(&route, format).map_err(|err| err.to_string())?;
    if let Some(gain) = number(args, "--gain")? {
        subdev.set_control(CID_ANALOGUE_GAIN, gain).map_err(|err| format!("gain: {err}"))?;
    }
    if let Some(lines) = number(args, "--exposure")? {
        subdev.set_control(CID_EXPOSURE, lines).map_err(|err| format!("exposure: {err}"))?;
    }
    let names: Vec<&str> = route.hops.iter().map(|h| h.entity.name.as_str()).collect();
    eprintln!("route: {}", names.join(" -> "));

    let device = Device::open(&video).map_err(|err| format!("{}: {err}", video.display()))?;
    let mode = Mode {
        fourcc: FourCc(order.raw10_fourcc()),
        size: Size { width: format.width, height: format.height },
        intervals: vec![Interval { numerator: 1, denominator: 30 }],
    };
    let mut stream =
        Stream::start(&device, &mode, mode.intervals[0], 4).map_err(|err| format!("stream: {err}"))?;
    let frames = number(args, "--frames")?.unwrap_or(4).max(1);
    let mut last = Vec::new();
    for index in 0..frames {
        let frame = stream
            .next_frame(Duration::from_secs(3))
            .map_err(|err| format!("frame {index}: {err}"))?
            .ok_or(format!("frame {index}: timeout"))?;
        last = frame.data().to_vec();
    }
    let stride = stream.stride();
    drop(stream);
    fs::write(out, &last).map_err(|err| format!("{out}: {err}"))?;
    println!(
        "{} {}x{} {:?} stride {stride} bytes {}",
        sensor.name,
        format.width,
        format.height,
        order,
        last.len()
    );
    Ok(())
}

fn still(args: &[String]) -> Result<()> {
    let [wanted, out, ..] = args else {
        return Err("still SENSOR OUT.yuv [options]".into());
    };
    let sensors = RawSensor::find_all();
    for sensor in &sensors {
        eprintln!(
            "{}: {:?} rotation {} {:?} via {}",
            sensor.name,
            sensor.facing,
            sensor.rotation,
            sensor.modes,
            sensor.video.display()
        );
    }
    let sensor = sensors
        .iter()
        .find(|s| s.name.contains(wanted.as_str()))
        .ok_or_else(|| format!("no raw sensor named like {wanted}"))?;
    let mode = match value(args, "--size") {
        Some(size) => {
            let (w, h) = size.split_once('x').ok_or("--size WxH")?;
            let size = Size {
                width: w.parse().map_err(|_| "bad width")?,
                height: h.parse().map_err(|_| "bad height")?,
            };
            sensor
                .capture_modes()
                .into_iter()
                .find(|mode| mode.size == size)
                .ok_or(format!("no mode {size:?}"))?
        }
        None => sensor
            .capture_modes()
            .into_iter()
            .max_by_key(|mode| mode.size.area())
            .ok_or("no modes")?,
    };
    let mut pipeline = sensor.start(mode.size, None).map_err(|err| format!("start: {err}"))?;
    let device = Device::open(&sensor.video).map_err(|err| err.to_string())?;
    let mut stream =
        Stream::start(&device, &mode, mode.intervals[0], 4).map_err(|err| format!("stream: {err}"))?;
    let frames = number(args, "--frames")?.unwrap_or(30).max(1);
    let mut last = None;
    let started = std::time::Instant::now();
    let mut busy = Duration::ZERO;
    for index in 0..frames {
        let stride = stream.stride();
        let frame = stream
            .next_frame(Duration::from_secs(3))
            .map_err(|err| format!("frame {index}: {err}"))?
            .ok_or(format!("frame {index}: timeout"))?;
        let begin = std::time::Instant::now();
        let image = pipeline.process(frame.data(), stride).map_err(|err| err.to_string())?;
        busy += begin.elapsed();
        let exposure = pipeline.exposure();
        let stats = pipeline.stats();
        eprintln!(
            "frame {index}: {} lines gain {} digital {:.2} median {:.3} p99 {:.3} mean {:.3?}",
            exposure.lines, exposure.gain, exposure.digital, stats.median, stats.highlight, stats.mean
        );
        last = Some(image);
    }
    let elapsed = started.elapsed();
    drop(stream);
    let image = last.ok_or("no frame")?;
    let mut bytes = Vec::new();
    for plane in image.planes() {
        bytes.extend_from_slice(plane);
    }
    fs::write(out, &bytes).map_err(|err| format!("{out}: {err}"))?;
    let size = image.size();
    println!(
        "{} I420 {}x{} converged {} ({frames} frames in {elapsed:?}, ISP {:?} per frame)",
        sensor.name,
        size.width,
        size.height,
        pipeline.converged(),
        busy / frames as u32
    );
    Ok(())
}
