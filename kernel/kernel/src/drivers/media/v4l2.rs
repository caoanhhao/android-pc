//! V4L2 video capture devices: `/dev/videoN` (char 81:N), the subset of
//! the Video4Linux2 ABI that AOSP's external camera HAL
//! (`android.hardware.camera.provider-V1-external-service`) and v4l2-ctl
//! use, single-planar capture with MMAP streaming:
//!
//! QUERYCAP, ENUM_FMT, G/S/TRY_FMT, ENUM_FRAMESIZES, ENUM_FRAMEINTERVALS,
//! G/S_PARM (frame interval), ENUMINPUT, G/S_INPUT, REQBUFS (MMAP),
//! QUERYBUF, QBUF, DQBUF (blocking or O_NONBLOCK, poll/epoll), STREAMON,
//! STREAMOFF; controls are not supported (EINVAL), anything else is
//! ENOTTY. Buffers are shmem objects mapped at the offset QUERYBUF gives.
//!
//! A [`VideoSource`] (a UVC camera, or the test camera) streams UVC
//! payloads into [`VideoDev::payload`], which finds frame boundaries
//! ([`FrameSync`]: FID toggles and EOF bits) and fills the queued buffers
//! in order. A frame with errors (the payload ERR bit, a lost interval,
//! a size that does not match an uncompressed format, an MJPEG frame not
//! starting with a JPEG SOI marker, an overflow) is
//! dropped and its buffer reused, as Linux's uvcvideo does by default;
//! a frame with no buffer queued is dropped. Done buffers carry a
//! monotonic start-of-exposure timestamp and a sequence number counting
//! every frame the device sent.
//!
//! While streaming, a status line per second goes to the kernel log (and
//! the framebuffer console): frames per second, bytes per frame, dropped
//! and erroneous frames.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLERR, POLLIN, POLLRDNORM, WaitQueue};
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;
use crate::sched::{self, Wake, WaitMode};

pub const VIDEO_MAJOR: u32 = 81;
const MAX_DEVICES: u32 = 16;
const MAX_BUFFERS: usize = 32;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

pub const PIX_MJPEG: u32 = fourcc(b"MJPG");
pub const PIX_YUYV: u32 = fourcc(b"YUYV");
pub const PIX_NV12: u32 = fourcc(b"NV12");

const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const FIELD_NONE: u32 = 1;
const COLORSPACE_SRGB: u32 = 8;
const CAP_VIDEO_CAPTURE: u32 = 0x1;
const CAP_EXT_PIX_FORMAT: u32 = 0x0020_0000;
const CAP_STREAMING: u32 = 0x0400_0000;
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const CAP_TIMEPERFRAME: u32 = 0x1000;
const FMT_FLAG_COMPRESSED: u32 = 1;
const BUF_FLAG_MAPPED: u32 = 0x1;
const BUF_FLAG_QUEUED: u32 = 0x2;
const BUF_FLAG_DONE: u32 = 0x4;
const BUF_FLAG_TIMESTAMP_MONOTONIC: u32 = 0x2000;
const BUF_FLAG_TSTAMP_SRC_SOE: u32 = 0x1_0000;
const BUF_CAP_SUPPORTS_MMAP: u32 = 1;
const PIX_FMT_PRIV_MAGIC: u32 = 0xfeed_cafe;
const INPUT_TYPE_CAMERA: u32 = 2;

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, nr: u32, size: u32) -> u32 {
    dir << 30 | size << 16 | (b'V' as u32) << 8 | nr
}

const VIDIOC_QUERYCAP: u32 = ioc(IOC_READ, 0, 104);
const VIDIOC_ENUM_FMT: u32 = ioc(IOC_READ | IOC_WRITE, 2, 64);
const VIDIOC_G_FMT: u32 = ioc(IOC_READ | IOC_WRITE, 4, 208);
const VIDIOC_S_FMT: u32 = ioc(IOC_READ | IOC_WRITE, 5, 208);
const VIDIOC_REQBUFS: u32 = ioc(IOC_READ | IOC_WRITE, 8, 20);
const VIDIOC_QUERYBUF: u32 = ioc(IOC_READ | IOC_WRITE, 9, 88);
const VIDIOC_QBUF: u32 = ioc(IOC_READ | IOC_WRITE, 15, 88);
const VIDIOC_DQBUF: u32 = ioc(IOC_READ | IOC_WRITE, 17, 88);
const VIDIOC_STREAMON: u32 = ioc(IOC_WRITE, 18, 4);
const VIDIOC_STREAMOFF: u32 = ioc(IOC_WRITE, 19, 4);
const VIDIOC_G_PARM: u32 = ioc(IOC_READ | IOC_WRITE, 21, 204);
const VIDIOC_S_PARM: u32 = ioc(IOC_READ | IOC_WRITE, 22, 204);
const VIDIOC_ENUMINPUT: u32 = ioc(IOC_READ | IOC_WRITE, 26, 80);
const VIDIOC_G_CTRL: u32 = ioc(IOC_READ | IOC_WRITE, 27, 8);
const VIDIOC_S_CTRL: u32 = ioc(IOC_READ | IOC_WRITE, 28, 8);
const VIDIOC_QUERYCTRL: u32 = ioc(IOC_READ | IOC_WRITE, 36, 68);
const VIDIOC_QUERYMENU: u32 = ioc(IOC_READ | IOC_WRITE, 37, 44);
const VIDIOC_G_INPUT: u32 = ioc(IOC_READ, 38, 4);
const VIDIOC_S_INPUT: u32 = ioc(IOC_READ | IOC_WRITE, 39, 4);
const VIDIOC_TRY_FMT: u32 = ioc(IOC_READ | IOC_WRITE, 64, 208);
const VIDIOC_G_EXT_CTRLS: u32 = ioc(IOC_READ | IOC_WRITE, 71, 32);
const VIDIOC_S_EXT_CTRLS: u32 = ioc(IOC_READ | IOC_WRITE, 72, 32);
const VIDIOC_TRY_EXT_CTRLS: u32 = ioc(IOC_READ | IOC_WRITE, 73, 32);
const VIDIOC_ENUM_FRAMESIZES: u32 = ioc(IOC_READ | IOC_WRITE, 74, 44);
const VIDIOC_ENUM_FRAMEINTERVALS: u32 = ioc(IOC_READ | IOC_WRITE, 75, 52);
const VIDIOC_QUERY_EXT_CTRL: u32 = ioc(IOC_READ | IOC_WRITE, 103, 232);

/// A frame size of a format, as the device describes it.
#[derive(Clone, Debug)]
pub struct FrameSize {
    /// The device's frame index (UVC bFrameIndex).
    pub index: u8,
    pub width: u32,
    pub height: u32,
    /// Largest frame in bytes.
    pub max_bytes: u32,
    /// Frame intervals in 100 ns units, shortest first.
    pub intervals: Vec<u32>,
    pub default_interval: u32,
}

/// A pixel format and its frame sizes.
#[derive(Clone, Debug)]
pub struct Format {
    /// The device's format index (UVC bFormatIndex).
    pub index: u8,
    pub fourcc: u32,
    pub name: &'static str,
    pub compressed: bool,
    /// Bits per pixel of an uncompressed format.
    pub bpp: u32,
    pub frames: Vec<FrameSize>,
}

/// The format, frame size (indices into the device's lists) and interval
/// in use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub fmt: usize,
    pub frame: usize,
    pub interval: u32,
}

/// Where frames come from.
pub trait VideoSource: Send + Sync {
    /// Start sending UVC payloads to `dev.payload()`.
    fn start(&self, dev: &Arc<VideoDev>, sel: Selection) -> KResult<()>;
    fn stop(&self, dev: &Arc<VideoDev>);
    /// Called before a reader looks for finished frames (the test camera
    /// generates the frames that are due).
    fn pump(&self, _dev: &Arc<VideoDev>) {}
}

/// A UVC payload header and its data.
#[derive(Debug, PartialEq)]
pub struct Payload<'a> {
    pub fid: bool,
    pub eof: bool,
    pub err: bool,
    pub data: &'a [u8],
}

/// Parse a UVC payload (header + data); None if the header is invalid.
pub fn parse_payload(p: &[u8]) -> Option<Payload<'_>> {
    let hlen = *p.first()? as usize;
    if hlen < 2 || hlen > p.len() {
        return None;
    }
    let info = p[1];
    // bmHeaderInfo: FID, EOF, PTS, SCR, RES, STI, ERR, EOH (always set
    // in the last header byte UVC defines).
    let mut need = 2;
    if info & 0x04 != 0 {
        need += 4;
    }
    if info & 0x08 != 0 {
        need += 6;
    }
    if hlen < need {
        return None;
    }
    Some(Payload { fid: info & 1 != 0, eof: info & 2 != 0, err: info & 0x40 != 0, data: &p[hlen..] })
}

/// What a payload means for the frame being assembled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Not part of a frame we follow (before the first frame start).
    Skip,
    /// Starts a new frame (the previous one, if open, is complete).
    NewFrame,
    /// More of the current frame.
    Continue,
}

/// Frame boundaries in a payload stream: a frame ends with EOF or when
/// FID toggles. The stream is joined at the first boundary seen, so a
/// frame already under way at stream start is skipped.
#[derive(Default, Debug)]
pub struct FrameSync {
    last_fid: Option<bool>,
    synced: bool,
    /// EOF seen: the next payload with data starts a frame even if FID
    /// did not toggle (some devices never toggle it).
    ended: bool,
}

impl FrameSync {
    pub fn step(&mut self, p: &Payload) -> Step {
        let r = match self.last_fid {
            None => Step::Skip,
            Some(f) if f != p.fid => {
                self.synced = true;
                Step::NewFrame
            }
            Some(_) if !self.synced || (self.ended && p.data.is_empty()) => Step::Skip,
            Some(_) if self.ended => Step::NewFrame,
            Some(_) => Step::Continue,
        };
        self.last_fid = Some(p.fid);
        if r != Step::Skip {
            self.ended = false;
        }
        if p.eof {
            self.synced = true;
            self.ended = true;
        }
        if r == Step::NewFrame && p.data.is_empty() && p.eof {
            // A header-only payload that ends a frame: nothing to start.
            return Step::Skip;
        }
        r
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum BufState {
    Dequeued,
    Queued,
    Done,
}

struct Buf {
    shmem: Arc<Shmem>,
    /// mmap offset (bytes).
    offset: u64,
    state: BufState,
    bytesused: u32,
    sequence: u32,
    ts_ns: u64,
}

/// The frame being filled.
struct Fill {
    buf: usize,
    off: u32,
    err: bool,
    ts_ns: u64,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    frames: u64,
    dropped: u64,
    errors: u64,
    bytes: u64,
    /// At the last status line: (time, frames, bytes, dropped, errors).
    mark: (u64, u64, u64, u64, u64),
}

struct VState {
    sel: Selection,
    bufs: Vec<Buf>,
    /// Buffer length (page-aligned image size).
    buf_len: u32,
    queued: VecDeque<usize>,
    done: VecDeque<usize>,
    streaming: bool,
    /// The open file that owns the buffers.
    owner: Option<u64>,
    sync: FrameSync,
    /// In a frame (between a start and its end).
    in_frame: bool,
    fill: Option<Fill>,
    /// The current frame has no buffer: skip its data.
    dropping: bool,
    sequence: u32,
    stats: Stats,
}

pub struct VideoDev {
    pub index: u32,
    pub name: String,
    driver: &'static str,
    bus_info: String,
    pub formats: Vec<Format>,
    source: Box<dyn VideoSource>,
    st: Mutex<VState>,
    pub wq: Arc<WaitQueue>,
}

static DEVICES: Mutex<Vec<Arc<VideoDev>>> = Mutex::new(Vec::new());
static NEXT_INDEX: AtomicU32 = AtomicU32::new(0);
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

/// Bytes of an image (uncompressed: exact; compressed: the device's
/// maximum frame size).
fn image_size(f: &Format, fr: &FrameSize) -> u32 {
    if f.compressed {
        fr.max_bytes.max(fr.width * fr.height / 2).max(4096)
    } else {
        fr.width * fr.height * f.bpp / 8
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// A 100 ns interval as a reduced fraction of seconds.
fn interval_fract(i: u32) -> (u32, u32) {
    let g = gcd(i.max(1), 10_000_000);
    (i.max(1) / g, 10_000_000 / g)
}

/// The interval of `fr` closest to `want` (100 ns units).
fn nearest_interval(fr: &FrameSize, want: u32) -> u32 {
    fr.intervals.iter().copied().min_by_key(|&i| (i as i64 - want as i64).unsigned_abs()).unwrap_or(fr.default_interval)
}

fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn get32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn put_str(b: &mut [u8], off: usize, len: usize, s: &str) {
    let n = s.len().min(len - 1);
    b[off..off + n].copy_from_slice(&s.as_bytes()[..n]);
    b[off + n..off + len].fill(0);
}

fn copy_in(arg: u64, b: &mut [u8]) -> KResult<()> {
    crate::process::current().mm().copy_in(arg, b)
}

fn copy_out(arg: u64, b: &[u8]) -> KResult<()> {
    crate::process::current().mm().copy_out(arg, b)
}

/// Register a capture device below `parent` (a sysfs directory relative
/// to /sys, e.g. a USB interface's).
pub fn register(name: &str, driver: &'static str, bus_info: &str, parent: &str, formats: Vec<Format>, source: Box<dyn VideoSource>) -> Option<Arc<VideoDev>> {
    if formats.is_empty() || formats.iter().any(|f| f.frames.is_empty()) {
        log::warn!("v4l2: {name}: no usable format");
        return None;
    }
    let index = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
    if index >= MAX_DEVICES {
        return None;
    }
    let fr0 = &formats[0].frames[0];
    let sel = Selection { fmt: 0, frame: 0, interval: fr0.default_interval };
    let dev = Arc::new(VideoDev {
        index,
        name: name.into(),
        driver,
        bus_info: bus_info.into(),
        formats,
        source,
        st: Mutex::new(VState {
            sel,
            bufs: Vec::new(),
            buf_len: 0,
            queued: VecDeque::new(),
            done: VecDeque::new(),
            streaming: false,
            owner: None,
            sync: FrameSync::default(),
            in_frame: false,
            fill: None,
            dropping: false,
            sequence: 0,
            stats: Stats::default(),
        }),
        wq: WaitQueue::new(),
    });
    let node = format!("video{index}");
    let sysdir = format!("{parent}/video4linux/{node}");
    crate::fs::sysfs::set(&sysdir, "name", &format!("{name}\n"));
    crate::fs::sysfs::set(&sysdir, "index", "0\n");
    crate::fs::sysfs::set(&sysdir, "dev_debug", "0\n");
    crate::fs::sysfs::add_char_device(&sysdir, "video4linux", &node, &node, VIDEO_MAJOR, index);
    crate::fs::sysfs::symlink(&sysdir, "device", "../..");
    devtmpfs_node(&node, index);
    for f in &dev.formats {
        let sizes: Vec<String> = f.frames.iter().map(|fr| format!("{}x{}@{}", fr.width, fr.height, 10_000_000 / fr.default_interval.max(1))).collect();
        log::info!("v4l2: {node} \"{name}\": {} ({}): {}", f.name, f.index, sizes.join(" "));
    }
    DEVICES.lock().push(dev.clone());
    Some(dev)
}

fn devtmpfs_node(name: &str, minor: u32) {
    use crate::fs::vfs::{S_IFCHR, encode_dev};
    let _ = crate::fs::devtmpfs_root().add(name.as_bytes(), S_IFCHR | 0o660, encode_dev(VIDEO_MAJOR, minor), None);
}

impl VideoDev {
    /// One UVC payload from the device (`err`: the interval was lost).
    pub fn payload(&self, pkt: &[u8], err: bool) {
        let mut guard = self.st.lock();
        let st: &mut VState = &mut guard;
        if !st.streaming {
            return;
        }
        if err || pkt.is_empty() {
            // A lost interval: the frame under way is damaged.
            if err && let Some(f) = st.fill.as_mut() {
                f.err = true;
            }
            if err && st.in_frame && st.fill.is_none() && !st.dropping {
                st.dropping = true;
            }
            return;
        }
        let Some(p) = parse_payload(pkt) else {
            if let Some(f) = st.fill.as_mut() {
                f.err = true;
            }
            return;
        };
        match st.sync.step(&p) {
            Step::Skip => return,
            Step::NewFrame => {
                self.finish(st);
                st.in_frame = true;
                st.dropping = false;
            }
            Step::Continue => {}
        }
        if !st.in_frame {
            return;
        }
        if st.fill.is_none() && !st.dropping {
            match st.queued.pop_front() {
                Some(buf) => st.fill = Some(Fill { buf, off: 0, err: false, ts_ns: crate::time::monotonic_ns() }),
                None => {
                    st.dropping = true;
                    st.stats.dropped += 1;
                }
            }
        }
        if let Some(f) = st.fill.as_mut() {
            let room = st.buf_len.saturating_sub(f.off) as usize;
            let n = p.data.len().min(room);
            if n < p.data.len() || p.err {
                f.err = true;
            }
            if n > 0 {
                let _ = st.bufs[f.buf].shmem.write(f.off as u64, &p.data[..n]);
                f.off += n as u32;
            }
        }
        if p.eof {
            self.finish(st);
        }
    }

    /// End the current frame: hand its buffer to the reader, or reuse it
    /// if the frame is damaged.
    fn finish(&self, st: &mut VState) {
        let was_in = core::mem::replace(&mut st.in_frame, false);
        st.dropping = false;
        let Some(f) = st.fill.take() else {
            if was_in {
                st.sequence = st.sequence.wrapping_add(1);
            }
            return;
        };
        let seq = st.sequence;
        st.sequence = st.sequence.wrapping_add(1);
        let fmt = &self.formats[st.sel.fmt];
        let fr = &fmt.frames[st.sel.frame];
        // A JPEG must start with SOI: cameras send garbage first frames
        // (the UGREEN at 4K on Linux), which the HAL could not decode.
        let mut soi = [0u8; 2];
        let not_jpeg = fmt.fourcc == PIX_MJPEG && (st.bufs[f.buf].shmem.read(0, &mut soi) < 2 || soi != [0xff, 0xd8]);
        let bad = f.err || f.off == 0 || not_jpeg || !fmt.compressed && f.off != image_size(fmt, fr);
        if bad {
            st.stats.errors += 1;
            if st.stats.errors <= 3 || st.stats.errors.is_multiple_of(100) {
                log::warn!(
                    "v4l2: video{}: frame {seq} damaged ({} of {} bytes{}), dropped ({} so far)",
                    self.index,
                    f.off,
                    image_size(fmt, fr),
                    if f.err {
                        ", payload error or lost data"
                    } else if not_jpeg {
                        ", no JPEG SOI"
                    } else {
                        ""
                    },
                    st.stats.errors
                );
            }
            st.queued.push_front(f.buf);
            return;
        }
        let b = &mut st.bufs[f.buf];
        b.state = BufState::Done;
        b.bytesused = f.off;
        b.sequence = seq;
        b.ts_ns = f.ts_ns;
        st.done.push_back(f.buf);
        st.stats.frames += 1;
        st.stats.bytes += f.off as u64;
        if st.stats.frames == 1 {
            log::info!("v4l2: video{}: first frame, {} bytes", self.index, f.off);
        }
        self.status(st, false);
        self.wq.wake(POLLIN | POLLRDNORM);
    }

    /// The once-a-second status line.
    fn status(&self, st: &mut VState, last: bool) {
        let now = crate::time::monotonic_ns();
        let s = &mut st.stats;
        let (t0, f0, b0, d0, e0) = s.mark;
        if t0 == 0 {
            s.mark = (now, s.frames, s.bytes, s.dropped, s.errors);
            return;
        }
        let dt = now - t0;
        if dt < 1_000_000_000 && !last {
            return;
        }
        let frames = s.frames - f0;
        let fps10 = frames * 10_000_000_000 / dt.max(1);
        let per = if frames > 0 { (s.bytes - b0) / frames } else { 0 };
        let fr = &self.formats[st.sel.fmt].frames[st.sel.frame];
        log::info!(
            "v4l2: video{}: {}x{} {}: {}.{} fps, {} KiB/frame, {} dropped (no buffer), {} damaged; total {} frames",
            self.index,
            fr.width,
            fr.height,
            self.formats[st.sel.fmt].name,
            fps10 / 10,
            fps10 % 10,
            per >> 10,
            s.dropped - d0,
            s.errors - e0,
            s.frames
        );
        s.mark = (now, s.frames, s.bytes, s.dropped, s.errors);
    }

    fn image_size(&self, sel: Selection) -> u32 {
        let f = &self.formats[sel.fmt];
        image_size(f, &f.frames[sel.frame])
    }

    /// The format and frame closest to a requested fourcc and size.
    fn closest(&self, fourcc: u32, w: u32, h: u32) -> (usize, usize) {
        let fi = self.formats.iter().position(|f| f.fourcc == fourcc).unwrap_or(0);
        let frames = &self.formats[fi].frames;
        let area = |fr: &FrameSize| (fr.width as i64 * fr.height as i64 - w as i64 * h as i64).unsigned_abs();
        let dist = |fr: &FrameSize| (fr.width as i64 - w as i64).unsigned_abs() + (fr.height as i64 - h as i64).unsigned_abs();
        let fr = (0..frames.len()).min_by_key(|&i| (dist(&frames[i]), area(&frames[i]))).unwrap_or(0);
        (fi, fr)
    }

    /// struct v4l2_format (208 bytes) for a selection.
    fn format_bytes(&self, sel: Selection) -> [u8; 208] {
        let f = &self.formats[sel.fmt];
        let fr = &f.frames[sel.frame];
        let mut b = [0u8; 208];
        put32(&mut b, 0, BUF_TYPE_VIDEO_CAPTURE);
        put32(&mut b, 8, fr.width);
        put32(&mut b, 12, fr.height);
        put32(&mut b, 16, f.fourcc);
        put32(&mut b, 20, FIELD_NONE);
        put32(&mut b, 24, if f.compressed { 0 } else { fr.width * f.bpp / 8 });
        put32(&mut b, 28, image_size(f, fr));
        put32(&mut b, 32, COLORSPACE_SRGB);
        put32(&mut b, 36, PIX_FMT_PRIV_MAGIC);
        b
    }

    fn stop_locked_parts(&self, st: &mut VState) {
        st.streaming = false;
        st.fill = None;
        st.in_frame = false;
        st.dropping = false;
        st.queued.clear();
        st.done.clear();
        for b in st.bufs.iter_mut() {
            b.state = BufState::Dequeued;
        }
    }

    fn streamoff(self: &Arc<Self>) {
        let was = {
            let mut st = self.st.lock();
            let was = st.streaming;
            if was {
                self.status(&mut st, true);
            }
            self.stop_locked_parts(&mut st);
            was
        };
        if was {
            self.source.stop(self);
            log::info!("v4l2: video{}: stream off", self.index);
        }
        self.wq.wake(POLLERR);
    }

    fn streamon(self: &Arc<Self>) -> KResult<()> {
        let (sel, nbufs, buf_len) = {
            let mut st = self.st.lock();
            if st.streaming {
                return Ok(());
            }
            if st.bufs.is_empty() {
                return Err(EINVAL);
            }
            st.streaming = true;
            st.sync = FrameSync::default();
            st.in_frame = false;
            st.fill = None;
            st.dropping = false;
            st.sequence = 0;
            st.stats = Stats::default();
            (st.sel, st.bufs.len(), st.buf_len)
        };
        let f = &self.formats[sel.fmt];
        let fr = &f.frames[sel.frame];
        log::info!(
            "v4l2: video{}: stream on, {} {}x{} at {} ms/frame, {} buffers of {} KiB",
            self.index,
            f.name,
            fr.width,
            fr.height,
            sel.interval / 10_000,
            nbufs,
            buf_len >> 10
        );
        if let Err(e) = self.source.start(self, sel) {
            log::warn!("v4l2: video{}: cannot start streaming ({e:?})", self.index);
            self.stop_locked_parts(&mut self.st.lock());
            return Err(e);
        }
        Ok(())
    }

    /// struct v4l2_buffer (88 bytes) of buffer `i`.
    fn buffer_bytes(&self, st: &VState, i: usize) -> [u8; 88] {
        let b = &st.bufs[i];
        let mut o = [0u8; 88];
        put32(&mut o, 0, i as u32);
        put32(&mut o, 4, BUF_TYPE_VIDEO_CAPTURE);
        let mut flags = BUF_FLAG_MAPPED | BUF_FLAG_TIMESTAMP_MONOTONIC | BUF_FLAG_TSTAMP_SRC_SOE;
        match b.state {
            BufState::Queued => flags |= BUF_FLAG_QUEUED,
            BufState::Done => flags |= BUF_FLAG_DONE,
            BufState::Dequeued => {}
        }
        put32(&mut o, 8, if b.state == BufState::Queued { 0 } else { b.bytesused });
        put32(&mut o, 12, flags);
        put32(&mut o, 16, FIELD_NONE);
        o[24..32].copy_from_slice(&((b.ts_ns / 1_000_000_000) as i64).to_le_bytes());
        o[32..40].copy_from_slice(&((b.ts_ns % 1_000_000_000 / 1000) as i64).to_le_bytes());
        put32(&mut o, 56, b.sequence);
        put32(&mut o, 60, MEMORY_MMAP);
        put32(&mut o, 64, b.offset as u32);
        put32(&mut o, 72, st.buf_len);
        o
    }

    fn reqbufs(&self, owner: u64, count: u32) -> KResult<u32> {
        let mut st = self.st.lock();
        if st.streaming {
            return Err(EBUSY);
        }
        if st.owner.is_some_and(|o| o != owner) && !st.bufs.is_empty() {
            return Err(EBUSY);
        }
        st.bufs.clear();
        st.queued.clear();
        st.done.clear();
        if count == 0 {
            st.owner = None;
            return Ok(0);
        }
        let size = self.image_size(st.sel).next_multiple_of(PAGE_SIZE as u32);
        let n = (count as usize).clamp(2, MAX_BUFFERS);
        for i in 0..n {
            st.bufs.push(Buf {
                shmem: Shmem::new(size as u64),
                offset: i as u64 * size as u64,
                state: BufState::Dequeued,
                bytesused: 0,
                sequence: 0,
                ts_ns: 0,
            });
        }
        st.buf_len = size;
        st.owner = Some(owner);
        Ok(n as u32)
    }

    /// Wait for a finished frame; its index.
    fn dqbuf(self: &Arc<Self>, nonblock: bool) -> KResult<usize> {
        let mut res = Err(EAGAIN);
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            self.source.pump(self);
            let mut st = self.st.lock();
            if !st.streaming {
                res = Err(EINVAL);
                return true;
            }
            if let Some(i) = st.done.pop_front() {
                st.bufs[i].state = BufState::Dequeued;
                res = Ok(i);
                return true;
            }
            nonblock
        });
        match w {
            Wake::Interrupted if res.is_err() => Err(ERESTARTSYS),
            _ => res,
        }
    }

    fn ioctl(self: &Arc<Self>, file: &VideoFile, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            VIDIOC_QUERYCAP => {
                let mut b = [0u8; 104];
                put_str(&mut b, 0, 16, self.driver);
                put_str(&mut b, 16, 32, &self.name);
                put_str(&mut b, 48, 32, &self.bus_info);
                put32(&mut b, 80, 0x0006_0600);
                let caps = CAP_VIDEO_CAPTURE | CAP_STREAMING | CAP_EXT_PIX_FORMAT;
                put32(&mut b, 84, caps | CAP_DEVICE_CAPS);
                put32(&mut b, 88, caps);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_ENUM_FMT => {
                let mut b = [0u8; 64];
                copy_in(arg, &mut b)?;
                let (i, ty) = (get32(&b, 0) as usize, get32(&b, 4));
                let fm = self.formats.get(i).filter(|_| ty == BUF_TYPE_VIDEO_CAPTURE).ok_or(EINVAL)?;
                put32(&mut b, 8, if fm.compressed { FMT_FLAG_COMPRESSED } else { 0 });
                put_str(&mut b, 12, 32, fm.name);
                put32(&mut b, 44, fm.fourcc);
                put32(&mut b, 48, 0);
                b[52..64].fill(0);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_G_FMT => {
                let mut b = [0u8; 4];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != BUF_TYPE_VIDEO_CAPTURE {
                    return Err(EINVAL);
                }
                let sel = self.st.lock().sel;
                copy_out(arg, &self.format_bytes(sel))?;
                Ok(0)
            }
            VIDIOC_S_FMT | VIDIOC_TRY_FMT => {
                let mut b = [0u8; 208];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != BUF_TYPE_VIDEO_CAPTURE {
                    return Err(EINVAL);
                }
                let (fi, fr) = self.closest(get32(&b, 16), get32(&b, 8), get32(&b, 12));
                let frame = &self.formats[fi].frames[fr];
                let mut sel = Selection { fmt: fi, frame: fr, interval: frame.default_interval };
                if cmd == VIDIOC_S_FMT {
                    let mut st = self.st.lock();
                    if st.streaming || (!st.bufs.is_empty() && st.owner != Some(file.id)) {
                        return Err(EBUSY);
                    }
                    if !st.bufs.is_empty() && self.image_size(sel).next_multiple_of(PAGE_SIZE as u32) > st.buf_len {
                        return Err(EBUSY);
                    }
                    // Keep the interval if the new size has it.
                    if frame.intervals.contains(&st.sel.interval) {
                        sel.interval = st.sel.interval;
                    }
                    st.sel = sel;
                }
                copy_out(arg, &self.format_bytes(sel))?;
                Ok(0)
            }
            VIDIOC_G_PARM | VIDIOC_S_PARM => {
                let mut b = [0u8; 204];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != BUF_TYPE_VIDEO_CAPTURE {
                    return Err(EINVAL);
                }
                let mut st = self.st.lock();
                if cmd == VIDIOC_S_PARM {
                    let (num, den) = (get32(&b, 12) as u64, get32(&b, 16) as u64);
                    if num != 0 && den != 0 {
                        let want = (num * 10_000_000 / den).min(u32::MAX as u64) as u32;
                        if st.streaming {
                            return Err(EBUSY);
                        }
                        let fr = &self.formats[st.sel.fmt].frames[st.sel.frame];
                        st.sel.interval = nearest_interval(fr, want);
                    }
                }
                let (num, den) = interval_fract(st.sel.interval);
                b[4..].fill(0);
                put32(&mut b, 4, CAP_TIMEPERFRAME);
                put32(&mut b, 12, num);
                put32(&mut b, 16, den);
                put32(&mut b, 24, st.bufs.len().max(2) as u32);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_ENUM_FRAMESIZES => {
                let mut b = [0u8; 44];
                copy_in(arg, &mut b)?;
                let (i, pix) = (get32(&b, 0) as usize, get32(&b, 4));
                let fm = self.formats.iter().find(|f| f.fourcc == pix).ok_or(EINVAL)?;
                let fr = fm.frames.get(i).ok_or(EINVAL)?;
                put32(&mut b, 8, 1); // discrete
                put32(&mut b, 12, fr.width);
                put32(&mut b, 16, fr.height);
                b[20..].fill(0);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_ENUM_FRAMEINTERVALS => {
                let mut b = [0u8; 52];
                copy_in(arg, &mut b)?;
                let (i, pix, w, h) = (get32(&b, 0) as usize, get32(&b, 4), get32(&b, 8), get32(&b, 12));
                let fm = self.formats.iter().find(|f| f.fourcc == pix).ok_or(EINVAL)?;
                let fr = fm.frames.iter().find(|fr| fr.width == w && fr.height == h).ok_or(EINVAL)?;
                let iv = *fr.intervals.get(i).ok_or(EINVAL)?;
                let (num, den) = interval_fract(iv);
                put32(&mut b, 16, 1); // discrete
                put32(&mut b, 20, num);
                put32(&mut b, 24, den);
                b[28..].fill(0);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_ENUMINPUT => {
                let mut b = [0u8; 80];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != 0 {
                    return Err(EINVAL);
                }
                put_str(&mut b, 4, 32, "Camera 1");
                put32(&mut b, 36, INPUT_TYPE_CAMERA);
                b[40..].fill(0);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_G_INPUT => {
                copy_out(arg, &0u32.to_le_bytes())?;
                Ok(0)
            }
            VIDIOC_S_INPUT => {
                let mut b = [0u8; 4];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != 0 {
                    return Err(EINVAL);
                }
                Ok(0)
            }
            VIDIOC_REQBUFS => {
                let mut b = [0u8; 20];
                copy_in(arg, &mut b)?;
                if get32(&b, 4) != BUF_TYPE_VIDEO_CAPTURE || get32(&b, 8) != MEMORY_MMAP {
                    return Err(EINVAL);
                }
                let n = self.reqbufs(file.id, get32(&b, 0))?;
                put32(&mut b, 0, n);
                put32(&mut b, 12, BUF_CAP_SUPPORTS_MMAP);
                b[16..].fill(0);
                copy_out(arg, &b)?;
                Ok(0)
            }
            VIDIOC_QUERYBUF | VIDIOC_QBUF => {
                let mut b = [0u8; 88];
                copy_in(arg, &mut b)?;
                let i = get32(&b, 0) as usize;
                if get32(&b, 4) != BUF_TYPE_VIDEO_CAPTURE {
                    return Err(EINVAL);
                }
                let mut st = self.st.lock();
                if i >= st.bufs.len() {
                    return Err(EINVAL);
                }
                if cmd == VIDIOC_QBUF {
                    if get32(&b, 60) != MEMORY_MMAP || st.owner != Some(file.id) {
                        return Err(EINVAL);
                    }
                    if st.bufs[i].state != BufState::Dequeued {
                        return Err(EINVAL);
                    }
                    st.bufs[i].state = BufState::Queued;
                    st.queued.push_back(i);
                }
                let o = self.buffer_bytes(&st, i);
                drop(st);
                copy_out(arg, &o)?;
                Ok(0)
            }
            VIDIOC_DQBUF => {
                let mut b = [0u8; 88];
                copy_in(arg, &mut b)?;
                if get32(&b, 4) != BUF_TYPE_VIDEO_CAPTURE || get32(&b, 60) != MEMORY_MMAP {
                    return Err(EINVAL);
                }
                if self.st.lock().owner != Some(file.id) {
                    return Err(EINVAL);
                }
                let i = self.dqbuf(f.nonblock())?;
                let o = self.buffer_bytes(&self.st.lock(), i);
                copy_out(arg, &o)?;
                Ok(0)
            }
            VIDIOC_STREAMON | VIDIOC_STREAMOFF => {
                let mut b = [0u8; 4];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) != BUF_TYPE_VIDEO_CAPTURE {
                    return Err(EINVAL);
                }
                let owner = self.st.lock().owner;
                if owner.is_some() && owner != Some(file.id) {
                    return Err(EBUSY);
                }
                if cmd == VIDIOC_STREAMON { self.streamon().map(|_| 0) } else {
                    self.streamoff();
                    Ok(0)
                }
            }
            VIDIOC_G_CTRL | VIDIOC_S_CTRL | VIDIOC_QUERYCTRL | VIDIOC_QUERYMENU | VIDIOC_G_EXT_CTRLS | VIDIOC_S_EXT_CTRLS | VIDIOC_TRY_EXT_CTRLS
            | VIDIOC_QUERY_EXT_CTRL => Err(EINVAL),
            _ => Err(ENOTTY),
        }
    }
}

pub struct VideoFile {
    dev: Arc<VideoDev>,
    id: u64,
}

/// Open of /dev/videoN.
pub fn open(minor: u32) -> KResult<Box<dyn FileOps>> {
    let dev = DEVICES.lock().iter().find(|d| d.index == minor).cloned().ok_or(ENODEV)?;
    Ok(Box::new(VideoFile { dev, id: NEXT_FILE.fetch_add(1, Ordering::Relaxed) }))
}

impl FileOps for VideoFile {
    fn ioctl(&self, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        self.dev.ioctl(self, f, cmd, arg)
    }

    fn mmap_shmem(&self, _: &OpenFile, _shared_write: bool, pgoff: u64) -> KResult<Option<(Arc<Shmem>, u64)>> {
        let off = pgoff * PAGE_SIZE;
        let st = self.dev.st.lock();
        let b = st.bufs.iter().find(|b| off >= b.offset && off < b.offset + st.buf_len as u64).ok_or(EINVAL)?;
        Ok(Some((b.shmem.clone(), (off - b.offset) / PAGE_SIZE)))
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        self.dev.source.pump(&self.dev);
        let st = self.dev.st.lock();
        if !st.streaming {
            return POLLERR;
        }
        if st.done.is_empty() { 0 } else { POLLIN | POLLRDNORM }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.dev.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for VideoFile {
    fn drop(&mut self) {
        let owner = self.dev.st.lock().owner == Some(self.id);
        if owner {
            self.dev.streamoff();
            let mut st = self.dev.st.lock();
            st.bufs.clear();
            st.owner = None;
        }
    }
}

#[test_case]
fn uvc_payload_headers() {
    // 12-byte header with PTS and SCR, FID 1, EOF.
    let mut p = [0u8; 16];
    p[0] = 12;
    p[1] = 0x80 | 0x08 | 0x04 | 0x02 | 0x01;
    p[12..].copy_from_slice(&[1, 2, 3, 4]);
    let h = parse_payload(&p).unwrap();
    assert!(h.fid && h.eof && !h.err);
    assert_eq!(h.data, &[1, 2, 3, 4]);
    // Header longer than the packet, too short for its PTS, zero length.
    assert!(parse_payload(&[20, 0x80, 0]).is_none());
    assert!(parse_payload(&[2, 0x84]).is_none());
    assert!(parse_payload(&[0, 0x80]).is_none());
    // ERR bit; a header-only payload.
    let h = parse_payload(&[2, 0xc0]).unwrap();
    assert!(h.err && h.data.is_empty());
}

#[test_case]
fn uvc_frame_sync() {
    let pl = |fid: bool, eof: bool, data: &'static [u8]| Payload { fid, eof, err: false, data };
    let mut s = FrameSync::default();
    // Joining mid-frame: skipped until FID toggles.
    assert_eq!(s.step(&pl(false, false, b"x")), Step::Skip);
    assert_eq!(s.step(&pl(false, false, b"x")), Step::Skip);
    assert_eq!(s.step(&pl(true, false, b"a")), Step::NewFrame);
    assert_eq!(s.step(&pl(true, false, b"b")), Step::Continue);
    assert_eq!(s.step(&pl(true, true, b"c")), Step::Continue);
    // Header-only payloads between frames.
    assert_eq!(s.step(&pl(true, false, b"")), Step::Skip);
    assert_eq!(s.step(&pl(false, false, b"d")), Step::NewFrame);
    // A device that never toggles FID but sets EOF.
    let mut s = FrameSync::default();
    assert_eq!(s.step(&pl(false, true, b"x")), Step::Skip);
    assert_eq!(s.step(&pl(false, false, b"a")), Step::NewFrame);
    assert_eq!(s.step(&pl(false, true, b"b")), Step::Continue);
    assert_eq!(s.step(&pl(false, false, b"c")), Step::NewFrame);
    // A frame ended by an EOF-only header.
    let mut s = FrameSync::default();
    assert_eq!(s.step(&pl(true, false, b"")), Step::Skip);
    assert_eq!(s.step(&pl(false, false, b"a")), Step::NewFrame);
    assert_eq!(s.step(&pl(false, true, b"")), Step::Continue);
    assert_eq!(s.step(&pl(true, false, b"b")), Step::NewFrame);
}

#[test_case]
fn v4l2_ioctl_numbers() {
    // Values from Linux's <linux/videodev2.h> on x86_64.
    assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
    assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
    assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
    assert_eq!(VIDIOC_QBUF, 0xc058_560f);
    assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
    assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
    assert_eq!(VIDIOC_G_PARM, 0xc0cc_5615);
    assert_eq!(VIDIOC_ENUM_FRAMESIZES, 0xc02c_564a);
    assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS, 0xc034_564b);
    assert_eq!(interval_fract(333_333), (333_333, 10_000_000));
    assert_eq!(interval_fract(166_666 * 3 / 3), (83_333, 5_000_000));
    assert_eq!(interval_fract(2_000_000), (1, 5));
}
