//! The test camera and microphone (`testcam=1`): a V4L2 device and an
//! ALSA capture card that behave like a USB webcam, for testing the
//! layers above the USB drivers without one (QEMU has no UVC device).
//!
//! The camera sends UVC payloads through [`VideoDev::payload`] exactly as
//! a camera on an isochronous endpoint would: 12-byte headers (PTS and
//! SCR), 3060 data bytes per payload, FID toggling every frame, EOF on
//! the last payload, a header-only payload and an empty interval after
//! each frame. Formats: MJPEG 320x240 (a fixed JPEG, `testcam.jpg`), YUYV
//! 640x480 and 320x240 (colour bars with a white square moving one step
//! per frame, frame number in the first pixels' luma), 30 or 15 fps.
//! The microphone delivers 48 kHz stereo 16-bit in 1 ms packets: a 1 kHz
//! triangle wave of amplitude 8000, the same on both channels.
//!
//! Frames and samples are produced when due, by whoever looks for them
//! (DQBUF, poll, READI, SYNC_PTR) on the clock; a kernel timer wakes the
//! waiters when the next frame or 10 ms of samples is due.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::drivers::sound::{self, capture::{CapturePcm, CaptureSource, HwFormat}};
use super::v4l2::{self, Format, FrameSize, PIX_MJPEG, PIX_YUYV, Selection, VideoDev, VideoSource};
use crate::errno::*;
use crate::fs::poll::{POLLIN, WaitQueue};
use crate::sched::timer::{self, TimerCallback};

pub static JPEG: &[u8] = include_bytes!("testcam.jpg");
const PAYLOAD: usize = 3072;
const HEADER: usize = 12;

/// Wakes a wait queue when a kernel timer fires.
struct Waker(Once<Arc<WaitQueue>>);

impl TimerCallback for Waker {
    fn fire(&self, _: u64) {
        if let Some(wq) = self.0.get() {
            wq.wake(POLLIN);
        }
    }
}

/// Arm `w` for `deadline` unless a timer at or before it is armed.
fn arm(w: &Arc<Waker>, armed: &mut u64, deadline: u64) {
    let now = crate::time::monotonic_ns();
    if *armed > now && *armed <= deadline {
        return;
    }
    *armed = deadline;
    let weak: alloc::sync::Weak<dyn TimerCallback> = Arc::downgrade(&(w.clone() as Arc<dyn TimerCallback>));
    timer::arm(deadline, weak);
}

/// A YUYV frame: eight colour bars, a 32x32 white square moving with the
/// frame number, the frame number in the luma of the first 32 pixels.
pub fn yuyv_frame(w: u32, h: u32, n: u32) -> Vec<u8> {
    // (Y, U, V) of white, yellow, cyan, green, magenta, red, blue, black.
    const BARS: [(u8, u8, u8); 8] =
        [(235, 128, 128), (210, 16, 146), (170, 166, 16), (145, 54, 34), (106, 202, 222), (81, 90, 240), (41, 240, 110), (16, 128, 128)];
    let (w, h) = (w as usize, h as usize);
    let mut f = vec![0u8; w * h * 2];
    let sx = (n as usize * 4) % w.saturating_sub(32).max(1);
    let sy = (n as usize * 2) % h.saturating_sub(32).max(1);
    for y in 0..h {
        for x in (0..w).step_by(2) {
            let (mut yy, u, v) = BARS[x * 8 / w];
            let mut y1 = yy;
            if (sy..sy + 32).contains(&y) && (sx..sx + 32).contains(&x) {
                yy = 235;
                y1 = 235;
            }
            let o = (y * w + x) * 2;
            f[o] = yy;
            f[o + 1] = u;
            f[o + 2] = y1;
            f[o + 3] = v;
        }
    }
    // Frame number, one bit per even pixel's luma (16 or 235).
    for bit in 0..16 {
        f[bit * 4] = if n >> bit & 1 != 0 { 235 } else { 16 };
    }
    f
}

/// Split a frame into UVC payloads.
pub fn packetize(frame: &[u8], fid: bool, pts: u32, mut out: impl FnMut(&[u8])) {
    let mut pkt = [0u8; PAYLOAD];
    let chunks = frame.len().div_ceil(PAYLOAD - HEADER).max(1);
    for (i, data) in frame.chunks(PAYLOAD - HEADER).enumerate() {
        let eof = i + 1 == chunks;
        pkt[0] = HEADER as u8;
        pkt[1] = 0x80 | 0x08 | 0x04 | if eof { 2 } else { 0 } | fid as u8;
        pkt[2..6].copy_from_slice(&pts.to_le_bytes());
        pkt[6..10].copy_from_slice(&pts.wrapping_add(i as u32).to_le_bytes());
        pkt[10..12].copy_from_slice(&(i as u16).to_le_bytes());
        pkt[HEADER..HEADER + data.len()].copy_from_slice(data);
        out(&pkt[..HEADER + data.len()]);
    }
}

struct CamState {
    running: bool,
    sel: Selection,
    start_ns: u64,
    /// Frames sent so far.
    sent: u64,
    fid: bool,
    armed: u64,
}

pub struct TestCam {
    st: Mutex<CamState>,
    waker: Arc<Waker>,
}

impl VideoSource for TestCam {
    fn start(&self, dev: &Arc<VideoDev>, sel: Selection) -> KResult<()> {
        self.waker.0.call_once(|| dev.wq.clone());
        let mut st = self.st.lock();
        st.running = true;
        st.sel = sel;
        st.start_ns = crate::time::monotonic_ns();
        st.sent = 0;
        let first = st.start_ns + sel.interval as u64 * 100;
        arm(&self.waker, &mut st.armed, first);
        Ok(())
    }

    fn stop(&self, _dev: &Arc<VideoDev>) {
        self.st.lock().running = false;
    }

    fn pump(&self, dev: &Arc<VideoDev>) {
        let (sel, first, count, fid0) = {
            let mut st = self.st.lock();
            if !st.running {
                return;
            }
            let period = st.sel.interval as u64 * 100;
            let now = crate::time::monotonic_ns();
            let due = (now - st.start_ns) / period;
            // Fall behind by at most two frames (a reader that stopped
            // reading does not get a burst).
            if due > st.sent + 2 {
                let skip = due - st.sent - 2;
                st.sent += skip;
                st.fid ^= skip % 2 == 1;
            }
            let count = due - st.sent;
            let r = (st.sel, st.sent, count, st.fid);
            st.sent = due;
            st.fid ^= count % 2 == 1;
            let next = st.start_ns + (due + 1) * period;
            arm(&self.waker, &mut st.armed, next);
            r
        };
        let fmt = &dev.formats[sel.fmt];
        let fr = &fmt.frames[sel.frame];
        let mut fid = fid0;
        for k in 0..count {
            let n = (first + k) as u32;
            let pts = n.wrapping_mul(sel.interval);
            let frame = if fmt.fourcc == PIX_MJPEG { JPEG.to_vec() } else { yuyv_frame(fr.width, fr.height, n) };
            fid = !fid;
            packetize(&frame, fid, pts, |p| dev.payload(p, false));
            // What cameras send between frames.
            dev.payload(&[2, 0x80 | 0x02 | fid as u8], false);
            dev.payload(&[], false);
        }
    }
}

struct MicState {
    running: bool,
    start_ns: u64,
    /// Frames sent so far.
    sent: u64,
    armed: u64,
}

pub struct TestMic {
    st: Mutex<MicState>,
    waker: Arc<Waker>,
}

/// The test tone: a 1 kHz triangle at 48 kHz, amplitude 8000.
pub fn tone(n: u64) -> i16 {
    let p = (n % 48) as i32;
    let v = if p < 24 { p * 2 * 8000 / 24 - 8000 } else { 8000 - (p - 24) * 2 * 8000 / 24 };
    v as i16
}

impl CaptureSource for TestMic {
    fn start(&self, pcm: &Arc<CapturePcm>) -> KResult<()> {
        self.waker.0.call_once(|| pcm.wq.clone());
        let mut st = self.st.lock();
        st.running = true;
        st.start_ns = crate::time::monotonic_ns();
        st.sent = 0;
        let d = st.start_ns + 10_000_000;
        arm(&self.waker, &mut st.armed, d);
        Ok(())
    }

    fn stop(&self, _pcm: &Arc<CapturePcm>) {
        self.st.lock().running = false;
    }

    fn format(&self) -> HwFormat {
        HwFormat { rate: 48000, channels: 2, bytes: 2 }
    }

    fn pump(&self, pcm: &Arc<CapturePcm>) {
        let (from, to) = {
            let mut st = self.st.lock();
            if !st.running {
                return;
            }
            let now = crate::time::monotonic_ns();
            // Whole milliseconds (48 frames, one USB packet each).
            let due = (now - st.start_ns) / 1_000_000 * 48;
            if due > st.sent + 48_000 {
                st.sent = due - 48_000;
            }
            let r = (st.sent, due);
            st.sent = due;
            let next = now + 10_000_000;
            arm(&self.waker, &mut st.armed, next);
            r
        };
        let mut pkt = [0u8; 192];
        let mut n = from;
        while n < to {
            for i in 0..48 {
                let v = tone(n + i).to_le_bytes();
                pkt[i as usize * 4..i as usize * 4 + 2].copy_from_slice(&v);
                pkt[i as usize * 4 + 2..i as usize * 4 + 4].copy_from_slice(&v);
            }
            pcm.push(&pkt);
            n += 48;
        }
    }
}

fn frame(index: u8, width: u32, height: u32, max_bytes: u32, intervals: Vec<u32>) -> FrameSize {
    FrameSize { index, width, height, max_bytes, default_interval: intervals[0], intervals }
}

/// Register the test camera (/dev/videoN) and microphone (a card).
pub fn register() {
    let formats = vec![
        Format { index: 1, fourcc: PIX_MJPEG, name: "Motion-JPEG", compressed: true, bpp: 0, frames: vec![frame(1, 320, 240, 65536, vec![333_333, 666_666])] },
        Format {
            index: 2,
            fourcc: PIX_YUYV,
            name: "YUYV 4:2:2",
            compressed: false,
            bpp: 16,
            frames: vec![frame(1, 640, 480, 614_400, vec![333_333, 666_666]), frame(2, 320, 240, 153_600, vec![333_333])],
        },
    ];
    let cam = TestCam {
        st: Mutex::new(CamState { running: false, sel: Selection { fmt: 0, frame: 0, interval: 333_333 }, start_ns: 0, sent: 0, fid: false, armed: 0 }),
        waker: Arc::new(Waker(Once::new())),
    };
    v4l2::register("android-pc test camera", "testcam", "platform:testcam", "devices/platform/testcam", formats, Box::new(cam));
    let mic = TestMic { st: Mutex::new(MicState { running: false, start_ns: 0, sent: 0, armed: 0 }), waker: Arc::new(Waker(Once::new())) };
    sound::register_capture("devices/platform/testmic", "TestMic", "testmic", "android-pc test microphone", "android-pc test microphone (testcam=1)", Box::new(mic));
}

#[test_case]
fn testcam_frames_through_v4l2_assembly() {
    // A YUYV frame survives packetizing and frame sync.
    let f = yuyv_frame(64, 48, 5);
    assert_eq!(f.len(), 64 * 48 * 2);
    assert_eq!((f[0], f[4], f[8]), (235, 16, 235));
    let mut sync = v4l2::FrameSync::default();
    let mut got = Vec::new();
    let mut frames = 0;
    for (fid, n) in [(false, 4u32), (true, 5), (false, 6)] {
        packetize(&yuyv_frame(64, 48, n), fid, 0, |p| {
            let pl = v4l2::parse_payload(p).unwrap();
            match sync.step(&pl) {
                v4l2::Step::NewFrame => {
                    frames += 1;
                    got.clear();
                    got.extend_from_slice(pl.data);
                }
                v4l2::Step::Continue => got.extend_from_slice(pl.data),
                v4l2::Step::Skip => {}
            }
        });
    }
    // The first frame is the one the stream joined in the middle of.
    assert_eq!(frames, 2);
    assert_eq!(got, yuyv_frame(64, 48, 6));
    assert_eq!((tone(0), tone(12), tone(24), tone(36)), (-8000, 0, 8000, 0));
    assert_eq!(&JPEG[..2], &[0xff, 0xd8]);
}
