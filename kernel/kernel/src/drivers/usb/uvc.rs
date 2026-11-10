//! USB Video Class cameras (E8): a Video Interface Collection's
//! VideoControl and first VideoStreaming interface become a V4L2 capture
//! device (`drivers::media::v4l2`).
//!
//! The VideoStreaming interface's class-specific descriptors give the
//! formats (MJPEG, uncompressed YUY2 or NV12; frame-based formats such as
//! H.264 are skipped) with their frame sizes and intervals; its alternate
//! settings give the isochronous endpoint's bandwidth. Streaming starts
//! with the probe/commit negotiation (VS_PROBE_CONTROL SET_CUR + GET_CUR,
//! then VS_COMMIT_CONTROL SET_CUR; 26, 34 or 48 bytes by UVC version),
//! which tells the largest payload per service interval; the smallest
//! alternate setting carrying it is selected ([`choose_alt`]) and its
//! endpoint streamed (`isoch`), every payload going to the V4L2 device.
//! Bulk streaming endpoints are not supported.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::isoch::{CC_BANDWIDTH, CC_SECONDARY_BANDWIDTH, IsochDesc, IsochParams, IsochSink};
use super::{CTRLS, MediaDev, Xhci};
use crate::drivers::media::v4l2::{self, Format, FrameSize, PIX_MJPEG, PIX_NV12, PIX_YUYV, Selection, VideoDev, VideoSource};
use crate::errno::*;

const CS_INTERFACE: u8 = 0x24;
const VC_HEADER: u8 = 1;
const VS_FORMAT_UNCOMPRESSED: u8 = 4;
const VS_FRAME_UNCOMPRESSED: u8 = 5;
const VS_FORMAT_MJPEG: u8 = 6;
const VS_FRAME_MJPEG: u8 = 7;
const VS_PROBE_CONTROL: u16 = 1;
const VS_COMMIT_CONTROL: u16 = 2;
const SET_CUR: u8 = 0x01;
const GET_CUR: u8 = 0x81;

/// A camera function's interfaces and streaming capabilities.
#[derive(Debug)]
pub struct UvcFunc {
    pub vc_iface: u8,
    pub vs_iface: u8,
    pub bcd_uvc: u16,
    pub formats: Vec<Format>,
    /// Alternate settings of the VideoStreaming interface with an
    /// isochronous IN endpoint.
    pub alts: Vec<(u8, IsochDesc)>,
    /// A bulk streaming endpoint (not supported).
    pub bulk: Option<u8>,
    /// Formats skipped (frame-based, unknown GUIDs).
    pub skipped: Vec<String>,
}

fn le16(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn le32(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}

/// Parse the camera function of a configuration descriptor.
pub fn parse(cfg: &[u8]) -> Option<UvcFunc> {
    let mut f = UvcFunc { vc_iface: 0xff, vs_iface: 0xff, bcd_uvc: 0x100, formats: Vec::new(), alts: Vec::new(), bulk: None, skipped: Vec::new() };
    // The interface the descriptors belong to: (number, alt, class, subclass).
    let mut cur = (0xffu8, 0u8, 0u8, 0u8);
    // Whether the frames that follow belong to a format we keep.
    let mut keep = false;
    let mut last_ep: Option<usize> = None;
    let mut i = 0;
    while i + 2 <= cfg.len() {
        let (len, ty) = (cfg[i] as usize, cfg[i + 1]);
        if len < 2 || i + len > cfg.len() {
            break;
        }
        let d = &cfg[i..i + len];
        i += len;
        match ty {
            4 if len >= 9 => {
                cur = (d[2], d[3], d[5], d[6]);
                last_ep = None;
                if d[5] == 14 && d[6] == 1 && f.vc_iface == 0xff {
                    f.vc_iface = d[2];
                }
                if d[5] == 14 && d[6] == 2 && f.vs_iface == 0xff {
                    f.vs_iface = d[2];
                }
            }
            CS_INTERFACE if cur.2 == 14 && cur.3 == 1 && cur.0 == f.vc_iface && len >= 5 && d[2] == VC_HEADER => {
                f.bcd_uvc = le16(d, 3);
            }
            CS_INTERFACE if cur.2 == 14 && cur.3 == 2 && cur.0 == f.vs_iface && cur.1 == 0 && len >= 3 => match d[2] {
                VS_FORMAT_UNCOMPRESSED if len >= 27 => {
                    let guid = &d[5..9];
                    let (fourcc, name) = match guid {
                        b"YUY2" => (PIX_YUYV, "YUYV 4:2:2"),
                        b"NV12" => (PIX_NV12, "Y/UV 4:2:0"),
                        _ => {
                            f.skipped.push(format!("uncompressed {:02x?}", guid));
                            keep = false;
                            continue;
                        }
                    };
                    f.formats.push(Format { index: d[3], fourcc, name, compressed: false, bpp: d[21] as u32, frames: Vec::new() });
                    keep = true;
                }
                VS_FORMAT_MJPEG if len >= 11 => {
                    f.formats.push(Format { index: d[3], fourcc: PIX_MJPEG, name: "Motion-JPEG", compressed: true, bpp: 0, frames: Vec::new() });
                    keep = true;
                }
                VS_FRAME_UNCOMPRESSED | VS_FRAME_MJPEG if keep && len >= 26 => {
                    let n = d[25] as usize;
                    let mut intervals: Vec<u32> = if n > 0 {
                        (0..n).filter(|k| 26 + 4 * k + 4 <= len).map(|k| le32(d, 26 + 4 * k)).collect()
                    } else if len >= 38 {
                        // Continuous: min, max, step; offer min, default, max.
                        alloc::vec![le32(d, 26), le32(d, 21), le32(d, 30)]
                    } else {
                        Vec::new()
                    };
                    let default_interval = le32(d, 21);
                    if intervals.is_empty() {
                        intervals.push(default_interval);
                    }
                    intervals.retain(|&v| v > 0);
                    intervals.sort_unstable();
                    intervals.dedup();
                    if let Some(fm) = f.formats.last_mut() {
                        fm.frames.push(FrameSize {
                            index: d[3],
                            width: le16(d, 5) as u32,
                            height: le16(d, 7) as u32,
                            max_bytes: le32(d, 17),
                            default_interval: if default_interval > 0 { default_interval } else { intervals.first().copied().unwrap_or(333_333) },
                            intervals,
                        });
                    }
                }
                // Frame-based formats (0x10) and their frames (0x11),
                // DV, stream-based: not supported.
                0x10 | 0x0c | 0x12 | 0x13 => {
                    f.skipped.push(format!("format subtype {:#x}", d[2]));
                    keep = false;
                }
                _ => {}
            },
            5 if len >= 7 && cur.0 == f.vs_iface && cur.2 == 14 => {
                let (addr, attr, mps, interval) = (d[2], d[3], le16(d, 4), d[6]);
                if addr & 0x80 != 0 && attr & 3 == 1 && mps & 0x7ff > 0 {
                    f.alts.push((cur.1, IsochDesc { addr, max_packet: mps, interval, ss_burst: 0, ss_mult: 0 }));
                    last_ep = Some(f.alts.len() - 1);
                } else if addr & 0x80 != 0 && attr & 3 == 2 {
                    f.bulk = Some(addr);
                }
            }
            0x30 if len >= 6 => {
                if let Some(k) = last_ep {
                    f.alts[k].1.ss_burst = d[2];
                    f.alts[k].1.ss_mult = d[3] & 3;
                }
            }
            _ => {}
        }
    }
    f.formats.retain(|fm| !fm.frames.is_empty());
    (f.vc_iface != 0xff && f.vs_iface != 0xff).then_some(f)
}

/// The alternate setting for a payload of `need` bytes per service
/// interval: the smallest that carries it, else the largest.
pub fn choose_alt(alts: &[(u8, IsochDesc)], speed: u32, need: u32) -> Option<(u8, IsochDesc)> {
    let mut sorted: Vec<(u8, IsochDesc)> = alts.to_vec();
    sorted.sort_by_key(|(_, d)| d.esit_bytes(speed));
    sorted.iter().find(|(_, d)| d.esit_bytes(speed) >= need).or(sorted.last()).copied()
}

/// The probe/commit control block for a format, frame and interval.
fn probe_block(len: usize, fmt: u8, frame: u8, interval: u32) -> Vec<u8> {
    let mut b = alloc::vec![0u8; len];
    b[0] = 1; // bmHint: dwFrameInterval fixed
    b[2] = fmt;
    b[3] = frame;
    b[4..8].copy_from_slice(&interval.to_le_bytes());
    b
}

/// The probe/commit block size of a UVC version.
fn probe_len(bcd: u16) -> usize {
    match bcd {
        0..0x0110 => 26,
        0x0110..0x0150 => 34,
        _ => 48,
    }
}

/// The streaming side of a camera on a controller.
struct UvcSource {
    ctrl: usize,
    media: usize,
    vs_iface: u8,
    bcd_uvc: u16,
    alts: Vec<(u8, IsochDesc)>,
    label: String,
    /// The endpoint streaming, for stop.
    ep: spin::Mutex<Option<u8>>,
}

struct UvcSink(Arc<VideoDev>);

impl IsochSink for UvcSink {
    fn packet(&mut self, data: &[u8], err: bool) {
        self.0.payload(data, err);
    }
}

impl UvcSource {
    fn with_dev<T>(&self, f: impl FnOnce(&mut Xhci, &mut MediaDev) -> T) -> KResult<T> {
        let mut ctrls = CTRLS.lock();
        let x = ctrls.get_mut(self.ctrl).ok_or(ENODEV)?;
        let mut m = x.media.get_mut(self.media).and_then(Option::take).ok_or(EBUSY)?;
        let r = f(x, &mut m);
        x.media[self.media] = Some(m);
        Ok(r)
    }
}

impl VideoSource for UvcSource {
    fn start(&self, dev: &Arc<VideoDev>, sel: Selection) -> KResult<()> {
        let fmt = &dev.formats[sel.fmt];
        let fr = &fmt.frames[sel.frame];
        let (fi, fri, interval) = (fmt.index, fr.index, sel.interval);
        self.with_dev(|x, m| x.uvc_start(m, self, dev, fi, fri, interval))?
    }

    fn stop(&self, _dev: &Arc<VideoDev>) {
        let Some(addr) = self.ep.lock().take() else { return };
        let _ = self.with_dev(|x, m| x.isoch_stop(&mut m.dev, &mut m.enabled, self.vs_iface, addr));
    }
}

impl Xhci {
    /// A VideoStreaming control request on the interface; IN data is
    /// returned.
    fn vs_control(&mut self, m: &mut MediaDev, iface: u8, req: u8, cs: u16, out: Option<&[u8]>, len: usize) -> Option<Vec<u8>> {
        if let Some(b) = out {
            unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), self.data.1, b.len()) };
        }
        let rt = if out.is_some() { 0x21 } else { 0xa1 };
        if !self.control(&mut m.dev, rt, req, cs << 8, iface as u16, len as u16) {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(self.data.1, len) }.to_vec())
    }

    fn uvc_start(&mut self, m: &mut MediaDev, src: &UvcSource, dev: &Arc<VideoDev>, fmt: u8, frame: u8, interval: u32) -> KResult<()> {
        let len = probe_len(src.bcd_uvc);
        let iface = src.vs_iface;
        let want = probe_block(len, fmt, frame, interval);
        if self.vs_control(m, iface, SET_CUR, VS_PROBE_CONTROL, Some(&want), len).is_none() {
            log::warn!("uvc: {}: probe SET_CUR failed", src.label);
            return Err(EIO);
        }
        let got = self.vs_control(m, iface, GET_CUR, VS_PROBE_CONTROL, None, len).unwrap_or_else(|| {
            log::warn!("uvc: {}: probe GET_CUR failed, committing the request as is", src.label);
            want.clone()
        });
        let (max_frame, mut max_payload) = (le32(&got, 18), le32(&got, 22));
        if self.vs_control(m, iface, SET_CUR, VS_COMMIT_CONTROL, Some(&got), len).is_none() {
            log::warn!("uvc: {}: commit failed", src.label);
            return Err(EIO);
        }
        let speed = m.dev.speed;
        let largest = src.alts.iter().map(|(_, d)| d.esit_bytes(speed)).max().unwrap_or(0);
        if max_payload == 0 || max_payload > largest {
            max_payload = largest;
        }
        let Some((alt, desc)) = choose_alt(&src.alts, speed, max_payload) else {
            log::warn!("uvc: {}: no isochronous alternate setting", src.label);
            return Err(EIO);
        };
        log::info!(
            "uvc: {}: committed format {fmt} frame {frame} interval {} -> {} (max frame {max_frame} bytes, max payload {max_payload} bytes): alt {alt} ({} bytes/interval)",
            src.label,
            le32(&want, 4),
            le32(&got, 4),
            desc.esit_bytes(speed)
        );
        let p = IsochParams { iface, alt, desc, ioc_every: 8, sentinel: true, queue_ms: 64, label: format!("uvc: {}", src.label) };
        match self.isoch_start(&mut m.dev, &mut m.enabled, p, Box::new(UvcSink(dev.clone()))) {
            Ok(()) => {
                *src.ep.lock() = Some(desc.addr);
                Ok(())
            }
            Err(code @ (CC_BANDWIDTH | CC_SECONDARY_BANDWIDTH)) => {
                log::warn!("uvc: {}: not enough USB bandwidth for alt {alt} (completion code {code})", src.label);
                Err(ENOSPC)
            }
            Err(code) => {
                log::warn!("uvc: {}: cannot start streaming (completion code {code})", src.label);
                Err(EIO)
            }
        }
    }

    /// Bind the camera function of a configured device stored at
    /// `self.media[media]`; `iface_dir(n)` is interface n's sysfs path.
    pub(super) fn uvc_bind(&mut self, media: usize, f: UvcFunc, name: &str, bus_info: &str, iface_dir: &dyn Fn(u8) -> String) {
        let label = format!("usb{}-{} video", self.bus, self.media[media].as_ref().map_or("?".into(), |m| m.dev.topo.path.clone()));
        for s in &f.skipped {
            log::info!("uvc: {label}: skipped {s}");
        }
        let speed = self.media[media].as_ref().map_or(0, |m| m.dev.speed);
        let alts: Vec<String> = f.alts.iter().map(|(a, d)| format!("{a}:{}x{}", d.packets(speed), d.mps())).collect();
        log::info!(
            "uvc: {label}: UVC {:x}.{:02x}, control if{}, streaming if{}, {} formats, isochronous alts [{}]",
            f.bcd_uvc >> 8,
            f.bcd_uvc & 0xff,
            f.vc_iface,
            f.vs_iface,
            f.formats.len(),
            alts.join(" ")
        );
        if f.alts.is_empty() {
            log::warn!("uvc: {label}: {}, not supported", if f.bulk.is_some() { "bulk streaming" } else { "no streaming endpoint" });
            return;
        }
        // Start from the zero-bandwidth setting.
        if let Some(mut m) = self.media[media].take() {
            self.control_quiet(&mut m.dev, 0x01, 11, 0, f.vs_iface as u16, 0, true);
            self.media[media] = Some(m);
        }
        let src = UvcSource {
            ctrl: self.index,
            media,
            vs_iface: f.vs_iface,
            bcd_uvc: f.bcd_uvc,
            alts: f.alts,
            label: label.clone(),
            ep: spin::Mutex::new(None),
        };
        match v4l2::register(name, "uvcvideo", bus_info, &iface_dir(f.vc_iface), f.formats, Box::new(src)) {
            Some(d) => log::info!("uvc: {label}: \"{name}\" is /dev/video{}", d.index),
            None => log::warn!("uvc: {label}: not registered"),
        }
    }
}

/// The UGREEN Camera 4K's (eba4:6579) configuration descriptor as Linux
/// reads it (`lsusb -v`), cut to its video function: VC interface 0,
/// VS interface 1 with MJPEG and YUY2 formats (two frames each here) and
/// alternate settings 1 (192 bytes), 7 (2 x 640) and 11 (3 x 1024).
#[cfg(test)]
fn ugreen_video_cfg() -> Vec<u8> {
    let mut c = alloc::vec![9, 2, 0, 0, 4, 1, 0, 0x80, 250];
    c.extend_from_slice(&[8, 11, 0, 2, 14, 3, 0, 2]);
    c.extend_from_slice(&[9, 4, 0, 0, 1, 14, 1, 0, 2]);
    c.extend_from_slice(&[13, 0x24, 1, 0x00, 0x01, 0x4d, 0, 0x80, 0x8d, 0x5b, 0x00, 1, 1]);
    c.extend_from_slice(&[7, 5, 0x81, 3, 16, 0, 8]);
    c.extend_from_slice(&[9, 4, 1, 0, 0, 14, 2, 0, 0]);
    c.extend_from_slice(&[15, 0x24, 1, 2, 0x72, 0x03, 0x82, 0, 4, 2, 1, 1, 1, 0, 0]);
    c.extend_from_slice(&[11, 0x24, 6, 1, 2, 1, 1, 0, 0, 0, 0]);
    // 3840x2160 MJPEG, one interval (30 fps).
    let fr = |c: &mut Vec<u8>, sub: u8, idx: u8, w: u16, h: u16, max: u32, ivs: &[u32]| {
        let len = 26 + 4 * ivs.len();
        c.extend_from_slice(&[len as u8, 0x24, sub, idx, 0]);
        c.extend_from_slice(&w.to_le_bytes());
        c.extend_from_slice(&h.to_le_bytes());
        c.extend_from_slice(&[0; 8]);
        c.extend_from_slice(&max.to_le_bytes());
        c.extend_from_slice(&ivs[0].to_le_bytes());
        c.push(ivs.len() as u8);
        for iv in ivs {
            c.extend_from_slice(&iv.to_le_bytes());
        }
    };
    fr(&mut c, 7, 1, 3840, 2160, 8_294_400, &[333_333]);
    fr(&mut c, 7, 2, 1280, 720, 921_600, &[166_666, 333_333]);
    let mut yuy2 = alloc::vec![27, 0x24, 4, 2, 2];
    yuy2.extend_from_slice(&[0x59, 0x55, 0x59, 0x32, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
    yuy2.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
    c.extend_from_slice(&yuy2);
    fr(&mut c, 5, 1, 640, 480, 614_400, &[333_333]);
    fr(&mut c, 5, 2, 1920, 1080, 4_147_200, &[2_000_000]);
    c.extend_from_slice(&[6, 0x24, 13, 1, 1, 4]);
    for (alt, mps) in [(1u8, 0x00c0u16), (7, 0x0a80), (11, 0x1400)] {
        c.extend_from_slice(&[9, 4, 1, alt, 1, 14, 2, 0, 0]);
        c.extend_from_slice(&[7, 5, 0x82, 5]);
        c.extend_from_slice(&mps.to_le_bytes());
        c.push(1);
    }
    c
}

#[test_case]
fn uvc_parse_ugreen() {
    let f = parse(&ugreen_video_cfg()).unwrap();
    assert_eq!((f.vc_iface, f.vs_iface, f.bcd_uvc), (0, 1, 0x100));
    assert_eq!(f.formats.len(), 2);
    let mj = &f.formats[0];
    assert_eq!((mj.index, mj.fourcc, mj.compressed), (1, PIX_MJPEG, true));
    assert_eq!(mj.frames.len(), 2);
    assert_eq!((mj.frames[1].width, mj.frames[1].height, mj.frames[1].index), (1280, 720, 2));
    assert_eq!(mj.frames[1].intervals, alloc::vec![166_666, 333_333]);
    assert_eq!(mj.frames[0].max_bytes, 8_294_400);
    let yu = &f.formats[1];
    assert_eq!((yu.index, yu.fourcc, yu.bpp), (2, PIX_YUYV, 16));
    assert_eq!(yu.frames[1].default_interval, 2_000_000);
    assert_eq!(f.alts.len(), 3);
    assert_eq!(f.alts[2], (11, IsochDesc { addr: 0x82, max_packet: 0x1400, interval: 1, ss_burst: 0, ss_mult: 0 }));
    // High speed: 1280 bytes fit alt 7 (2 x 640); more than any: alt 11.
    assert_eq!(choose_alt(&f.alts, 3, 1280).unwrap().0, 7);
    assert_eq!(choose_alt(&f.alts, 3, 100).unwrap().0, 1);
    assert_eq!(choose_alt(&f.alts, 3, 5000).unwrap().0, 11);
    let p = probe_block(probe_len(0x100), 1, 2, 333_333);
    assert_eq!(p.len(), 26);
    assert_eq!(&p[..8], &[1, 0, 1, 2, 0x15, 0x16, 0x05, 0]);
    assert_eq!((probe_len(0x110), probe_len(0x150)), (34, 48));
}
