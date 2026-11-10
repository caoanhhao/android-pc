//! Capture PCMs (E8): `/dev/snd/pcmC<c>D<d>c` (116:32c+24+d) on the
//! cards of [`super`], for USB audio class microphones
//! (`drivers::usb::uac`) and the test microphone (`testcam=1`).
//!
//! Unlike playback (`pcm`, a DMA ring the hardware reads), a capture
//! source pushes packets as they arrive ([`CapturePcm::push`]), in its own
//! format; they are converted on arrival to what the reader configured:
//! S16_LE, 1 or 2 channels, 8 to 48 kHz (24/32-bit to 16, mono/stereo,
//! rate: a box filter when decimating by 2 or more, linear interpolation
//! otherwise), so AOSP's "Built-In Mic" at 16 kHz mono works on a 48 kHz
//! stereo webcam microphone. Ioctls as tinyalsa and alsa-lib use them:
//! PVERSION, USER_PVERSION, INFO, TTSTAMP, HW_REFINE, HW_PARAMS (choosing
//! the minimum of what is left open, like Linux), HW_FREE, SW_PARAMS
//! (boundary returned), STATUS(_EXT), DELAY, HWSYNC, SYNC_PTR (status and
//! control are not mmapped), PREPARE, RESET, START (a read in the
//! PREPARED state starts too), DROP, DRAIN, READI_FRAMES; read(2) too. A
//! ring overflow is an overrun (EPIPE until PREPARE) unless the stop
//! threshold is above the buffer size, when the oldest frames are
//! dropped. The source streams from the first START until DROP, HW_FREE
//! or close. One opener at a time (EBUSY), as one substream.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLERR, POLLIN, POLLRDNORM, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

const RATES: [u32; 7] = [8000, 11025, 16000, 22050, 32000, 44100, 48000];
const MIN_PERIOD: u64 = 16;
const MAX_PERIOD: u64 = 32768;
const MAX_BUFFER: u64 = 65536;

// PCM states.
const STATE_OPEN: i32 = 0;
const STATE_SETUP: i32 = 1;
const STATE_PREPARED: i32 = 2;
const STATE_RUNNING: i32 = 3;
const STATE_XRUN: i32 = 4;

const FORMAT_S16_LE: u32 = 2;
const ACCESS_RW_INTERLEAVED: u32 = 3;
const INFO_INTERLEAVED: u32 = 0x100;
const INFO_BLOCK_TRANSFER: u32 = 0x1_0000;
const SYNC_PTR_HWSYNC: u32 = 1;
const SYNC_PTR_APPL: u32 = 2;
const SYNC_PTR_AVAIL_MIN: u32 = 4;

const fn ioc(dir: u32, ty: u8, nr: u32, size: u32) -> u32 {
    dir << 30 | size << 16 | (ty as u32) << 8 | nr
}

const R: u32 = 2;
const W: u32 = 1;
const PCM_PVERSION: u32 = ioc(R, b'A', 0x00, 4);
const PCM_INFO: u32 = ioc(R, b'A', 0x01, 288);
const PCM_TSTAMP: u32 = ioc(W, b'A', 0x02, 4);
const PCM_TTSTAMP: u32 = ioc(W, b'A', 0x03, 4);
const PCM_USER_PVERSION: u32 = ioc(W, b'A', 0x04, 4);
const PCM_HW_REFINE: u32 = ioc(R | W, b'A', 0x10, 608);
const PCM_HW_PARAMS: u32 = ioc(R | W, b'A', 0x11, 608);
const PCM_HW_FREE: u32 = ioc(0, b'A', 0x12, 0);
const PCM_SW_PARAMS: u32 = ioc(R | W, b'A', 0x13, 136);
const PCM_STATUS: u32 = ioc(R, b'A', 0x20, 152);
const PCM_DELAY: u32 = ioc(R, b'A', 0x21, 8);
const PCM_HWSYNC: u32 = ioc(0, b'A', 0x22, 0);
const PCM_SYNC_PTR: u32 = ioc(R | W, b'A', 0x23, 136);
const PCM_STATUS_EXT: u32 = ioc(R | W, b'A', 0x24, 152);
const PCM_PREPARE: u32 = ioc(0, b'A', 0x40, 0);
const PCM_RESET: u32 = ioc(0, b'A', 0x41, 0);
const PCM_START: u32 = ioc(0, b'A', 0x42, 0);
const PCM_DROP: u32 = ioc(0, b'A', 0x43, 0);
const PCM_DRAIN: u32 = ioc(0, b'A', 0x44, 0);
const PCM_READI_FRAMES: u32 = ioc(R, b'A', 0x51, 24);

// struct snd_pcm_hw_params: interval parameters 8 (SAMPLE_BITS) to 19.
const P_SAMPLE_BITS: usize = 8;
const P_FRAME_BITS: usize = 9;
const P_CHANNELS: usize = 10;
const P_RATE: usize = 11;
const P_PERIOD_TIME: usize = 12;
const P_PERIOD_SIZE: usize = 13;
const P_PERIOD_BYTES: usize = 14;
const P_PERIODS: usize = 15;
const P_BUFFER_TIME: usize = 16;
const P_BUFFER_SIZE: usize = 17;
const P_BUFFER_BYTES: usize = 18;
const P_TICK_TIME: usize = 19;

/// The device's sample format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HwFormat {
    pub rate: u32,
    pub channels: u32,
    /// Bytes per sample (2, 3 or 4; little-endian signed).
    pub bytes: u32,
}

/// Where the samples come from.
pub trait CaptureSource: Send + Sync {
    /// Start delivering samples (in `format()`) to `pcm.push()`.
    fn start(&self, pcm: &Arc<CapturePcm>) -> KResult<()>;
    fn stop(&self, pcm: &Arc<CapturePcm>);
    fn format(&self) -> HwFormat;
    /// Called before a reader looks at what was captured (test sources
    /// generate what is due).
    fn pump(&self, _pcm: &Arc<CapturePcm>) {}
}

/// Device samples to S16 frames at another rate and channel count.
#[derive(Clone, Debug)]
pub struct Converter {
    hw: HwFormat,
    out_rate: u32,
    out_ch: u32,
    acc: u64,
    prev: [i32; 2],
    sum: [i64; 2],
    count: i64,
}

impl Converter {
    pub fn new(hw: HwFormat, out_rate: u32, out_ch: u32) -> Converter {
        Converter { hw, out_rate, out_ch, acc: 0, prev: [0; 2], sum: [0; 2], count: 0 }
    }

    fn sample(&self, b: &[u8]) -> i32 {
        match self.hw.bytes {
            2 => i16::from_le_bytes([b[0], b[1]]) as i32,
            3 => (i32::from_le_bytes([0, b[0], b[1], b[2]])) >> 16,
            _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) >> 16,
        }
    }

    fn emit(&self, f: [i32; 2], out: &mut Vec<i16>) {
        let c = |v: i32| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        if self.out_ch == 1 {
            out.push(c((f[0] + f[1]) / 2));
        } else {
            out.push(c(f[0]));
            out.push(c(f[1]));
        }
    }

    /// Convert whole device frames of `data`, appending samples to `out`.
    pub fn push(&mut self, data: &[u8], out: &mut Vec<i16>) {
        let step = (self.hw.bytes * self.hw.channels) as usize;
        if step == 0 {
            return;
        }
        let (rin, rout) = (self.hw.rate as u64, self.out_rate as u64);
        for fr in data.chunks_exact(step) {
            let l = self.sample(fr);
            let r = if self.hw.channels > 1 { self.sample(&fr[self.hw.bytes as usize..]) } else { l };
            let cur = [l, r];
            if rout * 2 <= rin {
                self.sum[0] += l as i64;
                self.sum[1] += r as i64;
                self.count += 1;
                self.acc += rout;
                if self.acc >= rin {
                    self.acc -= rin;
                    let avg = [(self.sum[0] / self.count) as i32, (self.sum[1] / self.count) as i32];
                    self.emit(avg, out);
                    self.sum = [0; 2];
                    self.count = 0;
                }
            } else {
                while self.acc < rout {
                    let t = self.acc as i64;
                    let y = |k: usize| self.prev[k] + ((cur[k] - self.prev[k]) as i64 * t / rout as i64) as i32;
                    let f = [y(0), y(1)];
                    self.emit(f, out);
                    self.acc += rin;
                }
                self.acc -= rout;
                self.prev = cur;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Params {
    rate: u32,
    channels: u32,
    period: u64,
    buffer: u64,
}

struct PState {
    state: i32,
    open: bool,
    params: Option<Params>,
    conv: Option<Converter>,
    boundary: u64,
    avail_min: u64,
    start_threshold: u64,
    stop_threshold: u64,
    tstamp_type: u32,
    /// Frames captured and read since the stream was opened.
    hw_ptr: u64,
    appl_ptr: u64,
    /// Captured samples not read yet.
    ring: VecDeque<i16>,
    /// When hw_ptr last moved, and when the stream started (ns).
    tstamp_ns: u64,
    trigger_ns: u64,
    source_running: bool,
    overruns: u64,
    scratch: Vec<i16>,
    /// Status line bookkeeping: (time, hw_ptr), lines printed.
    mark: (u64, u64),
    lines: u32,
}

pub struct CapturePcm {
    pub card: u32,
    pub name: String,
    source: Box<dyn CaptureSource>,
    st: Mutex<PState>,
    pub wq: Arc<WaitQueue>,
}


fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn get32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn get64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
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

fn timespec(b: &mut [u8], off: usize, ns: u64) {
    put64(b, off, ns / 1_000_000_000);
    put64(b, off + 8, ns % 1_000_000_000);
}

/// A closed interval of a hw_params parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Iv {
    min: u64,
    max: u64,
}

impl Iv {
    fn and(&mut self, lo: u64, hi: u64) {
        self.min = self.min.max(lo);
        self.max = self.max.min(hi);
    }

    fn one(v: u64) -> Iv {
        Iv { min: v, max: v }
    }
}

const IV_OFF: usize = 260;

fn iv_get(b: &[u8], p: usize) -> Iv {
    let o = IV_OFF + (p - P_SAMPLE_BITS) * 12;
    let flags = get32(b, o + 8);
    let mut iv = Iv { min: get32(b, o) as u64, max: get32(b, o + 4) as u64 };
    if flags & 1 != 0 {
        iv.min += 1;
    }
    if flags & 2 != 0 {
        iv.max = iv.max.saturating_sub(1);
    }
    iv
}

fn iv_put(b: &mut [u8], p: usize, iv: Iv) {
    let o = IV_OFF + (p - P_SAMPLE_BITS) * 12;
    put32(b, o, iv.min.min(u32::MAX as u64) as u32);
    put32(b, o + 4, iv.max.min(u32::MAX as u64) as u32);
    // integer; not open, not empty.
    put32(b, o + 8, 4);
}

/// Refine a struct snd_pcm_hw_params in place (`choose`: also pick one
/// value for each parameter, as HW_PARAMS does). The chosen rate,
/// channels, period and buffer sizes.
fn refine(b: &mut [u8; 608], choose: bool) -> KResult<Params> {
    // Masks: ACCESS at 4, FORMAT at 36, SUBFORMAT at 68 (bit n of word n/32).
    let mask_has = |b: &[u8], off: usize, bit: u32| get32(b, off + (bit as usize / 32) * 4) & (1 << (bit % 32)) != 0;
    if !mask_has(b, 4, ACCESS_RW_INTERLEAVED) || !mask_has(b, 36, FORMAT_S16_LE) || !mask_has(b, 68, 0) {
        return Err(EINVAL);
    }
    for (off, bit) in [(4usize, ACCESS_RW_INTERLEAVED), (36, FORMAT_S16_LE), (68, 0)] {
        b[off..off + 32].fill(0);
        put32(b, off + (bit as usize / 32) * 4, 1 << (bit % 32));
    }
    let mut v: [Iv; 12] = core::array::from_fn(|i| iv_get(b, P_SAMPLE_BITS + i));
    let ix = |p: usize| p - P_SAMPLE_BITS;
    const US: u64 = 1_000_000;
    for round in 0..3 {
        let [sb, fb, ch, rate, pt, ps, pb, pe, bt, bs, bb, _tick] = &mut v;
        sb.and(16, 16);
        ch.and(1, 2);
        fb.and(16 * ch.min, 16 * ch.max);
        ch.and(fb.min.div_ceil(16), fb.max / 16);
        // Rates from the list only.
        rate.and(RATES[0] as u64, RATES[RATES.len() - 1] as u64);
        let lo = RATES.iter().map(|&r| r as u64).find(|&r| r >= rate.min);
        let hi = RATES.iter().map(|&r| r as u64).rev().find(|&r| r <= rate.max);
        match (lo, hi) {
            (Some(lo), Some(hi)) if lo <= hi => *rate = Iv { min: lo, max: hi },
            _ => return Err(EINVAL),
        }
        let (fbytes_min, fbytes_max) = (fb.min / 8, fb.max / 8);
        ps.and(MIN_PERIOD, MAX_PERIOD);
        pb.and(ps.min * fbytes_min, ps.max.saturating_mul(fbytes_max));
        ps.and(pb.min.div_ceil(fbytes_max.max(1)), pb.max / fbytes_min.max(1));
        pt.and((ps.min * US).div_ceil(rate.max), ps.max.saturating_mul(US) / rate.min);
        ps.and((pt.min * rate.min).div_ceil(US), pt.max.saturating_mul(rate.max) / US);
        pe.and(2, 1024);
        bs.and(ps.min * pe.min, (ps.max.saturating_mul(pe.max)).min(MAX_BUFFER));
        pe.and(bs.min.div_ceil(ps.max.max(1)), bs.max / ps.min.max(1));
        ps.and(bs.min.div_ceil(pe.max.max(1)), bs.max / pe.min.max(1));
        bb.and(bs.min * fbytes_min, bs.max.saturating_mul(fbytes_max));
        bs.and(bb.min.div_ceil(fbytes_max.max(1)), bb.max / fbytes_min.max(1));
        bt.and((bs.min * US).div_ceil(rate.max), bs.max.saturating_mul(US) / rate.min);
        bs.and((bt.min * rate.min).div_ceil(US), bt.max.saturating_mul(rate.max) / US);
        if v.iter().take(11).any(|i| i.min > i.max) {
            return Err(EINVAL);
        }
        if choose && round == 1 {
            // Linux's choice order: channels, rate, then the smallest
            // period and the fewest periods left.
            let c = v[ix(P_CHANNELS)].min;
            let r = v[ix(P_RATE)].min;
            let p = v[ix(P_PERIOD_SIZE)].min;
            let n = v[ix(P_PERIODS)].min.max(v[ix(P_BUFFER_SIZE)].min.div_ceil(p));
            v[ix(P_CHANNELS)] = Iv::one(c);
            v[ix(P_RATE)] = Iv::one(r);
            v[ix(P_PERIOD_SIZE)] = Iv::one(p);
            v[ix(P_PERIODS)] = Iv::one(n);
            v[ix(P_FRAME_BITS)] = Iv::one(16 * c);
            v[ix(P_PERIOD_BYTES)] = Iv::one(p * c * 2);
            v[ix(P_PERIOD_TIME)] = Iv::one(p * US / r);
            v[ix(P_BUFFER_SIZE)] = Iv::one(p * n);
            v[ix(P_BUFFER_BYTES)] = Iv::one(p * n * c * 2);
            v[ix(P_BUFFER_TIME)] = Iv::one(p * n * US / r);
            if p * n > MAX_BUFFER {
                return Err(EINVAL);
            }
            break;
        }
    }
    v[ix(P_TICK_TIME)] = Iv { min: 0, max: 0 };
    for (i, iv) in v.iter().enumerate() {
        iv_put(b, P_SAMPLE_BITS + i, *iv);
    }
    put32(b, 512, 0); // rmask
    put32(b, 516, 0); // cmask
    put32(b, 520, INFO_INTERLEAVED | INFO_BLOCK_TRANSFER);
    put32(b, 524, 16); // msbits
    let rate = v[ix(P_RATE)];
    let (num, den) = if rate.min == rate.max { (rate.min as u32, 1) } else { (0, 0) };
    put32(b, 528, num);
    put32(b, 532, den);
    put64(b, 536, 0);
    Ok(Params {
        rate: v[ix(P_RATE)].min as u32,
        channels: v[ix(P_CHANNELS)].min as u32,
        period: v[ix(P_PERIOD_SIZE)].min,
        buffer: v[ix(P_BUFFER_SIZE)].min,
    })
}

/// Open of a capture node.
pub fn open(pcm: Arc<CapturePcm>) -> KResult<Box<dyn FileOps>> {
    let mut st = pcm.st.lock();
    if st.open {
        return Err(EBUSY);
    }
    st.open = true;
    st.state = STATE_OPEN;
    st.params = None;
    st.tstamp_type = 0;
    drop(st);
    Ok(Box::new(PcmFile(pcm)))
}

impl CapturePcm {
    pub fn new(card: u32, name: &str, source: Box<dyn CaptureSource>) -> Arc<CapturePcm> {
        Arc::new(CapturePcm {
            card,
            name: name.into(),
            source,
            st: Mutex::new(PState {
                state: STATE_OPEN,
                open: false,
                params: None,
                conv: None,
                boundary: 0,
                avail_min: 1,
                start_threshold: 1,
                stop_threshold: u64::MAX,
                tstamp_type: 0,
                hw_ptr: 0,
                appl_ptr: 0,
                ring: VecDeque::new(),
                tstamp_ns: 0,
                trigger_ns: 0,
                source_running: false,
                overruns: 0,
                scratch: Vec::new(),
                mark: (0, 0),
                lines: 0,
            }),
            wq: WaitQueue::new(),
        })
    }

    pub fn format(&self) -> HwFormat {
        self.source.format()
    }

    /// Device samples arriving (any context but interrupts).
    pub fn push(&self, data: &[u8]) {
        let mut guard = self.st.lock();
        let st: &mut PState = &mut guard;
        if st.state != STATE_RUNNING {
            return;
        }
        let (Some(conv), Some(p)) = (st.conv.as_mut(), st.params) else { return };
        st.scratch.clear();
        conv.push(data, &mut st.scratch);
        let frames = (st.scratch.len() / p.channels as usize) as u64;
        if frames == 0 {
            return;
        }
        st.ring.extend(st.scratch.iter().copied());
        st.hw_ptr += frames;
        let now = crate::time::monotonic_ns();
        st.tstamp_ns = now;
        let avail = st.hw_ptr - st.appl_ptr;
        if avail > p.buffer {
            if st.stop_threshold <= p.buffer {
                st.state = STATE_XRUN;
                st.overruns += 1;
                if st.overruns <= 3 || st.overruns.is_multiple_of(100) {
                    log::warn!("sound: card {}: capture overrun ({} so far)", self.card, st.overruns);
                }
            } else {
                let drop = avail - p.buffer;
                st.ring.drain(..(drop * p.channels as u64) as usize);
                st.appl_ptr += drop;
            }
        }
        // A status line after the first second, then every ten.
        let (t0, h0) = st.mark;
        if t0 == 0 {
            st.mark = (now, st.hw_ptr);
            st.lines = 0;
        } else if now - t0 >= if st.lines == 0 { 1_000_000_000 } else { 10_000_000_000 } {
            log::info!(
                "sound: card {}: capturing {} Hz {} ch: {} frames/s, {} overruns",
                self.card,
                p.rate,
                p.channels,
                (st.hw_ptr - h0) * 1_000_000_000 / (now - t0),
                st.overruns
            );
            st.lines += 1;
            st.mark = (now, st.hw_ptr);
        }
        let wake = st.hw_ptr - st.appl_ptr >= st.avail_min || st.state == STATE_XRUN;
        drop(guard);
        if wake {
            self.wq.wake(POLLIN | POLLRDNORM);
        }
    }

    fn start(self: &Arc<Self>) -> KResult<()> {
        let need_source = {
            let mut st = self.st.lock();
            if st.state != STATE_PREPARED {
                return Err(EBADFD);
            }
            st.state = STATE_RUNNING;
            st.trigger_ns = crate::time::monotonic_ns();
            st.mark = (0, 0);
            !core::mem::replace(&mut st.source_running, true)
        };
        if need_source && let Err(e) = self.source.start(self) {
            let mut st = self.st.lock();
            st.state = STATE_PREPARED;
            st.source_running = false;
            log::warn!("sound: card {}: cannot start capture ({e:?})", self.card);
            return Err(EIO);
        }
        Ok(())
    }

    /// Stop the device stream; the PCM goes to `state`.
    fn stop(self: &Arc<Self>, state: i32) {
        let running = {
            let mut st = self.st.lock();
            if st.state > STATE_OPEN {
                st.state = state;
            }
            core::mem::replace(&mut st.source_running, false)
        };
        if running {
            self.source.stop(self);
        }
        self.wq.wake(POLLERR);
    }

    fn prepare(&self) -> KResult<()> {
        let mut st = self.st.lock();
        let p = st.params.ok_or(EBADFD)?;
        if st.state == STATE_OPEN {
            return Err(EBADFD);
        }
        st.ring.clear();
        st.appl_ptr = st.hw_ptr;
        st.conv = Some(Converter::new(self.source.format(), p.rate, p.channels));
        st.state = STATE_PREPARED;
        Ok(())
    }

    /// struct snd_pcm_info (288 bytes).
    pub(super) fn info(&self) -> [u8; 288] {
        let mut b = [0u8; 288];
        put32(&mut b, 8, 1); // capture
        put32(&mut b, 12, self.card);
        put_str(&mut b, 16, 64, "USB Audio");
        put_str(&mut b, 80, 80, &self.name);
        put_str(&mut b, 160, 32, "subdevice #0");
        put32(&mut b, 200, 1);
        put32(&mut b, 204, 1);
        b
    }

    fn now_ts(st: &PState, ns: u64) -> u64 {
        if st.tstamp_type == 0 && ns != 0 { (crate::time::boot_epoch_ns() + ns as i128) as u64 } else { ns }
    }

    /// Copy up to `frames` captured frames out; how many.
    fn take(&self, st: &mut PState, frames: u64) -> Vec<i16> {
        let ch = st.params.map_or(2, |p| p.channels) as u64;
        let n = frames.min(st.hw_ptr - st.appl_ptr);
        st.appl_ptr += n;
        st.ring.drain(..(n * ch) as usize).collect()
    }

    /// READI_FRAMES / read(2): read `frames` frames to user `buf`.
    fn read_frames(self: &Arc<Self>, nonblock: bool, buf: u64, frames: u64) -> KResult<u64> {
        let ch = {
            let st = self.st.lock();
            match st.state {
                STATE_PREPARED | STATE_RUNNING => {}
                STATE_XRUN => return Err(EPIPE),
                _ => return Err(EBADFD),
            }
            st.params.ok_or(EBADFD)?.channels as u64
        };
        if self.st.lock().state == STATE_PREPARED {
            self.start()?;
        }
        let mut done = 0u64;
        while done < frames {
            let mut res: KResult<Vec<i16>> = Ok(Vec::new());
            let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
                self.source.pump(self);
                let mut st = self.st.lock();
                match st.state {
                    STATE_RUNNING => {}
                    STATE_XRUN => {
                        res = Err(EPIPE);
                        return true;
                    }
                    _ => {
                        res = Err(EBADFD);
                        return true;
                    }
                }
                let s = self.take(&mut st, frames - done);
                let got = !s.is_empty();
                res = Ok(s);
                got || nonblock
            });
            match res {
                Ok(s) if !s.is_empty() => {
                    let bytes: Vec<u8> = s.iter().flat_map(|v| v.to_le_bytes()).collect();
                    copy_out(buf + done * ch * 2, &bytes)?;
                    done += s.len() as u64 / ch;
                }
                Ok(_) if done > 0 => return Ok(done),
                Ok(_) if matches!(w, Wake::Interrupted) => return Err(ERESTARTSYS),
                Ok(_) => return Err(EAGAIN),
                Err(e) if done > 0 && e != EPIPE => return Ok(done),
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    /// struct snd_pcm_status (152 bytes).
    fn status(&self) -> [u8; 152] {
        let st = self.st.lock();
        let mut b = [0u8; 152];
        let bd = st.boundary.max(1);
        put32(&mut b, 0, st.state as u32);
        timespec(&mut b, 8, Self::now_ts(&st, st.trigger_ns));
        timespec(&mut b, 24, Self::now_ts(&st, crate::time::monotonic_ns()));
        put64(&mut b, 40, st.appl_ptr % bd);
        put64(&mut b, 48, st.hw_ptr % bd);
        let avail = st.hw_ptr - st.appl_ptr;
        put64(&mut b, 56, avail);
        put64(&mut b, 64, avail);
        put64(&mut b, 72, avail);
        timespec(&mut b, 96, Self::now_ts(&st, st.tstamp_ns));
        b
    }

    fn sync_ptr(&self, b: &mut [u8; 136]) {
        let flags = get32(b, 0);
        let mut st = self.st.lock();
        let bd = st.boundary.max(1);
        if flags & SYNC_PTR_APPL == 0 {
            // Take the reader's appl_ptr (it only moves forward).
            let user = get64(b, 72) % bd;
            let delta = (user + bd - st.appl_ptr % bd) % bd;
            if delta > 0 && delta <= st.hw_ptr - st.appl_ptr {
                let ch = st.params.map_or(2, |p| p.channels) as u64;
                st.ring.drain(..(delta * ch) as usize);
                st.appl_ptr += delta;
            }
        }
        if flags & SYNC_PTR_AVAIL_MIN == 0 {
            st.avail_min = get64(b, 80).max(1);
        }
        put32(b, 8, st.state as u32);
        put64(b, 16, st.hw_ptr % bd);
        let ts = Self::now_ts(&st, st.tstamp_ns);
        timespec(b, 24, ts);
        put32(b, 40, 0);
        timespec(b, 48, ts);
        put64(b, 72, st.appl_ptr % bd);
        put64(b, 80, st.avail_min);
    }

    fn ioctl(self: &Arc<Self>, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            PCM_PVERSION => copy_out(arg, &0x0002_000fu32.to_le_bytes()).map(|_| 0),
            PCM_USER_PVERSION | PCM_TSTAMP => Ok(0),
            PCM_TTSTAMP => {
                let mut b = [0u8; 4];
                copy_in(arg, &mut b)?;
                let t = get32(&b, 0);
                if t > 2 {
                    return Err(EINVAL);
                }
                self.st.lock().tstamp_type = t;
                Ok(0)
            }
            PCM_INFO => copy_out(arg, &self.info()).map(|_| 0),
            PCM_HW_REFINE | PCM_HW_PARAMS => {
                let mut b = [0u8; 608];
                copy_in(arg, &mut b)?;
                if cmd == PCM_HW_PARAMS {
                    let st = self.st.lock();
                    if st.state >= STATE_RUNNING && st.state != STATE_XRUN {
                        return Err(EBADFD);
                    }
                }
                let p = refine(&mut b, cmd == PCM_HW_PARAMS)?;
                if cmd == PCM_HW_PARAMS {
                    let mut st = self.st.lock();
                    st.params = Some(p);
                    st.state = STATE_SETUP;
                    let mut bd = p.buffer;
                    while bd * 2 <= (i64::MAX as u64) - p.buffer {
                        bd *= 2;
                    }
                    st.boundary = bd;
                    st.avail_min = p.period;
                    st.start_threshold = 1;
                    st.stop_threshold = p.buffer;
                    st.ring.clear();
                    st.appl_ptr = st.hw_ptr;
                    log::info!(
                        "sound: card {}: hw_params {} Hz, {} ch, period {} frames, buffer {} frames",
                        self.card,
                        p.rate,
                        p.channels,
                        p.period,
                        p.buffer
                    );
                }
                copy_out(arg, &b)?;
                Ok(0)
            }
            PCM_HW_FREE => {
                self.stop(STATE_OPEN);
                let mut st = self.st.lock();
                st.state = STATE_OPEN;
                st.params = None;
                Ok(0)
            }
            PCM_SW_PARAMS => {
                let mut b = [0u8; 136];
                copy_in(arg, &mut b)?;
                let mut st = self.st.lock();
                if st.params.is_none() {
                    return Err(EBADFD);
                }
                st.avail_min = get64(&b, 16).max(1);
                st.start_threshold = get64(&b, 32);
                st.stop_threshold = get64(&b, 40);
                put64(&mut b, 64, st.boundary);
                drop(st);
                copy_out(arg, &b)?;
                Ok(0)
            }
            PCM_STATUS => copy_out(arg, &self.status()).map(|_| 0),
            PCM_STATUS_EXT => copy_out(arg, &self.status()).map(|_| 0),
            PCM_DELAY => {
                let st = self.st.lock();
                if st.state == STATE_XRUN {
                    return Err(EPIPE);
                }
                let d = st.hw_ptr - st.appl_ptr;
                drop(st);
                copy_out(arg, &d.to_le_bytes()).map(|_| 0)
            }
            PCM_HWSYNC => {
                self.source.pump(self);
                if self.st.lock().state == STATE_XRUN { Err(EPIPE) } else { Ok(0) }
            }
            PCM_SYNC_PTR => {
                let mut b = [0u8; 136];
                copy_in(arg, &mut b)?;
                if get32(&b, 0) & SYNC_PTR_HWSYNC != 0 {
                    self.source.pump(self);
                }
                self.sync_ptr(&mut b);
                copy_out(arg, &b)?;
                Ok(0)
            }
            PCM_PREPARE => self.prepare().map(|_| 0),
            PCM_RESET => {
                let mut st = self.st.lock();
                st.ring.clear();
                st.appl_ptr = st.hw_ptr;
                Ok(0)
            }
            PCM_START => self.start().map(|_| 0),
            PCM_DROP | PCM_DRAIN => {
                if self.st.lock().state == STATE_OPEN {
                    return Err(EBADFD);
                }
                self.stop(STATE_SETUP);
                Ok(0)
            }
            PCM_READI_FRAMES => {
                let mut b = [0u8; 24];
                copy_in(arg, &mut b)?;
                let n = self.read_frames(f.nonblock(), get64(&b, 8), get64(&b, 16))?;
                put64(&mut b, 0, n);
                copy_out(arg, &b)?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }
}

pub struct PcmFile(Arc<CapturePcm>);

impl FileOps for PcmFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        // read(2) on a PCM: whole frames into a kernel buffer.
        let ch = self.0.st.lock().params.ok_or(EBADFD)?.channels as usize;
        let frames = buf.len() / (ch * 2);
        let mut out = Vec::new();
        {
            let mut st = self.0.st.lock();
            if st.state == STATE_XRUN {
                return Err(EPIPE);
            }
            if st.state == STATE_PREPARED {
                drop(st);
                self.0.start()?;
                st = self.0.st.lock();
            }
            let avail = st.hw_ptr - st.appl_ptr;
            if avail == 0 && f.nonblock() {
                return Err(EAGAIN);
            }
            if avail > 0 {
                out = self.0.take(&mut st, frames as u64);
            }
        }
        if out.is_empty() {
            // Block for data like READI_FRAMES does (one wait).
            let wq = self.0.wq.clone();
            let pcm = self.0.clone();
            let w = sched::wait_until(wq.key(), None, WaitMode::Interruptible, || {
                pcm.source.pump(&pcm);
                let mut st = pcm.st.lock();
                if st.state != STATE_RUNNING {
                    return true;
                }
                out = pcm.take(&mut st, frames as u64);
                !out.is_empty()
            });
            if out.is_empty() {
                return match (w, self.0.st.lock().state) {
                    (_, STATE_XRUN) => Err(EPIPE),
                    (Wake::Interrupted, _) => Err(ERESTARTSYS),
                    _ => Err(EBADFD),
                };
            }
        }
        for (i, v) in out.iter().enumerate() {
            buf[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        Ok(out.len() * 2)
    }

    fn ioctl(&self, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        self.0.ioctl(f, cmd, arg)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        self.0.source.pump(&self.0);
        let st = self.0.st.lock();
        match st.state {
            STATE_RUNNING if st.hw_ptr - st.appl_ptr >= st.avail_min => POLLIN | POLLRDNORM,
            STATE_RUNNING | STATE_PREPARED => 0,
            _ => POLLERR,
        }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for PcmFile {
    fn drop(&mut self) {
        self.0.stop(STATE_OPEN);
        let mut st = self.0.st.lock();
        st.open = false;
        st.state = STATE_OPEN;
        st.params = None;
    }
}

#[test_case]
fn capture_ioctl_numbers() {
    // Values from Linux's <sound/asound.h> on x86_64.
    assert_eq!(PCM_INFO, 0x8120_4101);
    assert_eq!(PCM_HW_PARAMS, 0xc260_4111);
    assert_eq!(PCM_SW_PARAMS, 0xc088_4113);
    assert_eq!(PCM_SYNC_PTR, 0xc088_4123);
    assert_eq!(PCM_STATUS, 0x8098_4120);
    assert_eq!(PCM_READI_FRAMES, 0x8018_4151);
    assert_eq!(PCM_PREPARE, 0x4140);
}

#[cfg(test)]
fn hw_params_any() -> [u8; 608] {
    let mut b = [0u8; 608];
    b[4..260].fill(0xff);
    for i in 0..12 {
        put32(&mut b, IV_OFF + i * 12 + 4, u32::MAX);
    }
    b
}

#[test_case]
fn alsa_hw_params_refine() {
    // tinyalsa's pcm_open for the HAL's built-in mic: 16 kHz mono,
    // period >= 320 frames, 2 periods.
    let mut b = hw_params_any();
    iv_put(&mut b, P_CHANNELS, Iv::one(1));
    iv_put(&mut b, P_RATE, Iv::one(16000));
    iv_put(&mut b, P_PERIODS, Iv::one(2));
    iv_put(&mut b, P_PERIOD_SIZE, Iv { min: 320, max: u32::MAX as u64 });
    let p = refine(&mut b, true).unwrap();
    assert_eq!((p.rate, p.channels, p.period, p.buffer), (16000, 1, 320, 640));
    assert_eq!(iv_get(&b, P_PERIOD_BYTES), Iv::one(640));
    assert_eq!(iv_get(&b, P_BUFFER_TIME), Iv::one(40_000));
    assert_eq!(get32(&b, IV_OFF + (P_PERIOD_SIZE - 8) * 12 + 8) & 4, 4);
    // Refining everything open: the hardware's ranges.
    let mut b = hw_params_any();
    refine(&mut b, false).unwrap();
    assert_eq!(iv_get(&b, P_CHANNELS), Iv { min: 1, max: 2 });
    assert_eq!(iv_get(&b, P_RATE), Iv { min: 8000, max: 48000 });
    assert_eq!(iv_get(&b, P_SAMPLE_BITS), Iv::one(16));
    // 6 channels, 96 kHz, 24-bit: refused.
    for (p, v) in [(P_CHANNELS, 6u64), (P_RATE, 96000)] {
        let mut b = hw_params_any();
        iv_put(&mut b, p, Iv::one(v));
        assert!(refine(&mut b, false).is_err());
    }
    let mut b = hw_params_any();
    b[36..68].fill(0);
    b[36] = 1 << 6; // S24_LE only
    assert!(refine(&mut b, false).is_err());
}

#[test_case]
fn alsa_convert() {
    let hw = HwFormat { rate: 48000, channels: 2, bytes: 2 };
    // 48 frames of a ramp in both channels (left = n, right = -n).
    let mut data = Vec::new();
    for n in 0..48i16 {
        data.extend_from_slice(&(n * 100).to_le_bytes());
        data.extend_from_slice(&(-n * 100).to_le_bytes());
    }
    let mut out = Vec::new();
    Converter::new(hw, 48000, 2).push(&data, &mut out);
    assert_eq!(out.len(), 96);
    assert_eq!(&out[..4], &[0, 0, 0, 0]);
    assert_eq!(&out[4..6], &[100, -100]);
    // To mono 16 kHz: 16 frames, each the average of three, L+R = 0.
    let mut out = Vec::new();
    Converter::new(hw, 16000, 1).push(&data, &mut out);
    assert_eq!(out.len(), 16);
    assert!(out.iter().all(|&v| v == 0));
    // Left only: averages 100 (0,1,2), 400, ...
    let mut left = Vec::new();
    for n in 0..48i16 {
        left.extend_from_slice(&(n * 100).to_le_bytes());
    }
    let mut out = Vec::new();
    Converter::new(HwFormat { rate: 48000, channels: 1, bytes: 2 }, 16000, 2).push(&left, &mut out);
    assert_eq!(&out[..4], &[100, 100, 400, 400]);
    // 44.1 kHz from 48 kHz: 441 frames per 480.
    let mut out = Vec::new();
    let mut c = Converter::new(hw, 44100, 2);
    for _ in 0..10 {
        c.push(&data, &mut out);
    }
    assert_eq!(out.len() / 2, 441);
    // 24-bit samples keep their top 16 bits.
    let mut out = Vec::new();
    Converter::new(HwFormat { rate: 48000, channels: 1, bytes: 3 }, 48000, 1).push(&[0x00, 0x34, 0x12, 0x00, 0x00, 0x80], &mut out);
    assert_eq!(out, alloc::vec![0, 0x1234]);
}
