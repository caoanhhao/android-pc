//! The ALSA PCM ABI (`/dev/snd/pcmC*D*p`, `SNDRV_PCM_IOCTL_*`) for
//! playback, as tinyalsa uses it: INFO, HW_REFINE/HW_PARAMS (interleaved
//! read/write access), SW_PARAMS, TTSTAMP, PREPARE, START, WRITEI_FRAMES,
//! SYNC_PTR (the status/control pages are not mmappable, so tinyalsa
//! falls back to it), HWSYNC, STATUS, DELAY, DRAIN, DROP, PAUSE, RESET,
//! HW_FREE.
//!
//! The driver ([`PcmHw`]) owns a DMA ring and reports the hardware read
//! position in it. The ring is split into the negotiated periods; frames
//! are copied from user space at `appl_ptr`, the hardware consumes them
//! at `hw_ptr`. Both count frames forever (modulo `boundary`, as in
//! Linux). While the stream runs a kernel timer (every tick, 10 ms) reads
//! the position, advances `hw_ptr` (the time since the last read resolves
//! ring wrap-arounds), zeroes what was consumed (so an underrun plays
//! silence, not the old ring), detects underruns (`hw_ptr` reaching
//! `appl_ptr`: the stream stops in XRUN, writes fail with EPIPE until
//! PREPARE) and wakes writers and DRAIN. A position that stops moving
//! for half a second (dead DMA) is treated as an underrun.
//!
//! The hardware ring is the buffer plus a pre-roll of two periods: after
//! each start the hardware first plays that much silence, and `hw_ptr`
//! only counts frames past it (the pre-roll is part of `delay`). The
//! Cuttlefish HAL writes a whole buffer per burst (4096 frames every
//! 85 ms into a 4096-frame buffer, sized for an emulator's host-side
//! queue); without the margin every burst would arrive just after the
//! buffer ran dry.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLERR, POLLOUT, POLLWRNORM, WaitQueue};
use crate::sched::{self, Wake, WaitMode, timer};
use crate::sync::IrqLock;

const ESTRPIPE: Errno = Errno(86);

pub const STATE_OPEN: i32 = 0;
pub const STATE_SETUP: i32 = 1;
pub const STATE_PREPARED: i32 = 2;
pub const STATE_RUNNING: i32 = 3;
pub const STATE_XRUN: i32 = 4;
pub const STATE_DRAINING: i32 = 5;
pub const STATE_PAUSED: i32 = 6;

pub const FORMAT_S16_LE: u32 = 2;
pub const FORMAT_S32_LE: u32 = 10;
const ACCESS_RW_INTERLEAVED: u32 = 3;
const SUBFORMAT_STD: u32 = 0;

// hw_params parameter indices: masks, then intervals from 8.
const P_ACCESS: usize = 0;
const P_FORMAT: usize = 1;
const P_SUBFORMAT: usize = 2;
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

// struct snd_pcm_hw_params layout (x86_64, 608 bytes).
const HW_PARAMS_SIZE: usize = 608;
const HWP_MASKS: usize = 4;
const HWP_INTERVALS: usize = 260;
const HWP_RMASK: usize = 512;
const HWP_CMASK: usize = 516;
const HWP_INFO: usize = 520;
const HWP_MSBITS: usize = 524;
const HWP_RATE_NUM: usize = 528;
const HWP_RATE_DEN: usize = 532;
const HWP_FIFO_SIZE: usize = 536;

const INFO_INTERLEAVED: u32 = 0x100;
const INFO_BLOCK_TRANSFER: u32 = 0x10000;
const INFO_PAUSE: u32 = 0x80000;

const SYNC_PTR_HWSYNC: u32 = 1;
const SYNC_PTR_APPL: u32 = 2;
const SYNC_PTR_AVAIL_MIN: u32 = 4;

const TSTAMP_TYPE_GETTIMEOFDAY: u32 = 0;

/// Hardware position not moving for this long while running: dead DMA.
const STALL_NS: u64 = 500_000_000;

/// What the hardware accepts.
#[derive(Clone, Debug)]
pub struct HwCaps {
    /// SNDRV_PCM_FORMAT_* bits.
    pub formats: u64,
    /// Supported rates, ascending.
    pub rates: Vec<u32>,
    pub channels_min: u32,
    pub channels_max: u32,
    /// Ring size limit (bytes) and the period alignment (bytes).
    pub max_buffer_bytes: u32,
    pub period_align: u32,
    pub periods_max: u32,
    /// Significant bits of a 32-bit sample.
    pub msbits32: u32,
}

/// The configuration hw_params settled on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub format: u32,
    pub channels: u32,
    pub rate: u32,
    pub period_frames: u32,
    pub periods: u32,
}

impl Params {
    pub fn sample_bytes(&self) -> u32 {
        if self.format == FORMAT_S16_LE { 2 } else { 4 }
    }

    pub fn frame_bytes(&self) -> u32 {
        self.sample_bytes() * self.channels
    }

    pub fn period_bytes(&self) -> u32 {
        self.period_frames * self.frame_bytes()
    }

    pub fn buffer_frames(&self) -> u32 {
        self.period_frames * self.periods
    }
}

/// A playback DMA engine.
pub trait PcmHw: Send + Sync {
    fn caps(&self) -> HwCaps;
    /// The ring: kernel address (direct map) and capacity in bytes.
    fn ring(&self) -> (*mut u8, usize);
    /// Program the hardware for `p` with a ring of `ring_frames` (the
    /// buffer plus the pre-roll; stopped, position 0); task context.
    fn prepare(&self, p: &Params, ring_frames: u32) -> KResult<()>;
    /// Start or stop the DMA (any context, no sleeping locks).
    fn trigger(&self, run: bool);
    /// Read position in the ring, bytes (any context).
    fn position(&self) -> u32;
    /// The stream is closed or its parameters freed; task context.
    fn release(&self);
}

pub struct PcmDevice {
    pub card: u32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub hw: Arc<dyn PcmHw>,
    busy: AtomicBool,
    /// Rate limit of underrun messages.
    xrun_log: AtomicU64,
    pub xruns: AtomicU64,
}

impl PcmDevice {
    pub fn new(card: u32, device: u32, id: &str, name: &str, hw: Arc<dyn PcmHw>) -> PcmDevice {
        PcmDevice { card, device, id: id.into(), name: name.into(), hw, busy: AtomicBool::new(false), xrun_log: AtomicU64::new(0), xruns: AtomicU64::new(0) }
    }
}

#[derive(Default)]
struct Runtime {
    state: i32,
    params: Option<Params>,
    buffer: u64,
    frame_bytes: u64,
    hw_ptr: u64,
    appl_ptr: u64,
    boundary: u64,
    avail_min: u64,
    start_threshold: u64,
    stop_threshold: u64,
    tstamp_type: u32,
    /// Pre-roll (frames of silence played before the buffer's first
    /// frame), the hardware ring (buffer + pre-roll) and the frames the
    /// hardware consumed since PREPARE.
    pad: u64,
    ring: u64,
    consumed: u64,
    /// Last position read (frames in the ring) and when.
    last_pos: u64,
    last_ns: u64,
    /// When the position last moved (stall detection).
    moved_ns: u64,
    tstamp_ns: u64,
    trigger_ns: u64,
    avail_max: u64,
    timer: Option<u64>,
}

impl Runtime {
    /// Room for writing: buffer - queued (negative after an underrun: 0).
    fn avail(&self) -> u64 {
        let queued = (self.appl_ptr as i128 - self.hw_ptr as i128).rem_euclid(self.boundary.max(1) as i128) as u64;
        if queued > self.boundary / 2 {
            // hw_ptr ran past appl_ptr (underrun).
            return self.buffer + (self.boundary - queued);
        }
        self.buffer.saturating_sub(queued)
    }

    fn queued(&self) -> i64 {
        let q = (self.appl_ptr as i128 - self.hw_ptr as i128).rem_euclid(self.boundary.max(1) as i128) as u64;
        if q > self.boundary / 2 { -((self.boundary - q) as i64) } else { q as i64 }
    }
}

pub struct Substream {
    dev: Arc<PcmDevice>,
    rt: IrqLock<Runtime>,
    wq: Arc<WaitQueue>,
    me: Weak<Substream>,
}

pub fn open(dev: Arc<PcmDevice>) -> KResult<Box<dyn FileOps>> {
    if dev.busy.swap(true, Ordering::AcqRel) {
        return Err(EBUSY);
    }
    let s = Arc::new_cyclic(|me| Substream { dev, rt: IrqLock::new(Runtime::default()), wq: WaitQueue::new(), me: me.clone() });
    Ok(Box::new(PcmFile(s)))
}

fn tstamp_bytes(ns: u64, ty: u32) -> [u8; 16] {
    let t = if ty == TSTAMP_TYPE_GETTIMEOFDAY { (crate::time::boot_epoch_ns() + ns as i128) as i64 } else { ns as i64 };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&t.div_euclid(1_000_000_000).to_le_bytes());
    b[8..].copy_from_slice(&t.rem_euclid(1_000_000_000).to_le_bytes());
    b
}

impl Substream {
    /// Read the hardware position and advance hw_ptr; handles underrun,
    /// end of drain and stalls. With the runtime locked.
    fn update(&self, rt: &mut Runtime) {
        if !matches!(rt.state, STATE_RUNNING | STATE_DRAINING) {
            return;
        }
        let Some(p) = rt.params else { return };
        let now = crate::time::monotonic_ns();
        let buf = rt.ring;
        let pos = (self.dev.hw.position() as u64 / rt.frame_bytes) % buf;
        let mut delta = (pos + buf - rt.last_pos) % buf;
        // Whole laps the position may have made since the last read.
        let est = (now - rt.last_ns) as u128 * p.rate as u128 / 1_000_000_000;
        let est = est as u64;
        if est > delta + buf / 2 {
            delta += (est - delta + buf / 2) / buf * buf;
        }
        if delta > 0 {
            self.silence(rt.consumed % buf, delta.min(buf), rt);
            rt.consumed += delta;
            rt.hw_ptr = rt.consumed.saturating_sub(rt.pad) % rt.boundary;
            rt.moved_ns = now;
        }
        rt.last_pos = pos;
        rt.last_ns = now;
        rt.tstamp_ns = now;
        let avail = rt.avail();
        rt.avail_max = rt.avail_max.max(avail);
        if rt.state == STATE_DRAINING && rt.queued() <= 0 {
            self.dev.hw.trigger(false);
            rt.state = STATE_SETUP;
            log::info!("snd: pcmC{}D{}p drained", self.dev.card, self.dev.device);
        } else if rt.state == STATE_RUNNING && avail >= rt.stop_threshold {
            self.xrun(rt, "underrun");
        } else if now - rt.moved_ns > STALL_NS {
            self.xrun(rt, "DMA position stalled");
        }
    }

    fn xrun(&self, rt: &mut Runtime, why: &str) {
        self.dev.hw.trigger(false);
        rt.state = STATE_XRUN;
        let n = self.dev.xruns.fetch_add(1, Ordering::Relaxed) + 1;
        if super::ratelimit(&self.dev.xrun_log, 2000) {
            log::warn!(
                "snd: pcmC{}D{}p {why} (#{n}): hw {} appl {} pos {} bytes",
                self.dev.card, self.dev.device, rt.hw_ptr, rt.appl_ptr, self.dev.hw.position()
            );
        }
    }

    /// Zero `n` consumed frames of the ring from frame `at` (wrapping).
    fn silence(&self, at: u64, n: u64, rt: &Runtime) {
        let (ring, _) = self.dev.hw.ring();
        let fb = rt.frame_bytes as usize;
        let first = n.min(rt.ring - at) as usize;
        unsafe {
            core::ptr::write_bytes(ring.add(at as usize * fb), 0, first * fb);
            core::ptr::write_bytes(ring, 0, (n as usize - first) * fb);
        }
    }

    fn arm(&self, rt: &mut Runtime) {
        if let Some(id) = rt.timer.take() {
            timer::cancel(id);
        }
        if matches!(rt.state, STATE_RUNNING | STATE_DRAINING) {
            let me: Weak<dyn timer::TimerCallback> = self.me.clone();
            rt.timer = Some(timer::arm(crate::time::monotonic_ns() + 1, me));
        }
    }

    fn start(&self, rt: &mut Runtime) {
        let now = crate::time::monotonic_ns();
        rt.state = STATE_RUNNING;
        rt.last_pos = (self.dev.hw.position() as u64 / rt.frame_bytes) % rt.ring;
        rt.last_ns = now;
        rt.moved_ns = now;
        rt.trigger_ns = now;
        rt.tstamp_ns = now;
        self.dev.hw.trigger(true);
        self.arm(rt);
    }

    fn stop(&self, rt: &mut Runtime, state: i32) {
        if matches!(rt.state, STATE_RUNNING | STATE_DRAINING | STATE_PAUSED) {
            self.dev.hw.trigger(false);
        }
        rt.state = state;
        rt.trigger_ns = crate::time::monotonic_ns();
        self.arm(rt);
    }

    fn hw_refine(&self, b: &mut [u8], install: bool) -> KResult<()> {
        let caps = self.dev.hw.caps();
        let p = refine(&caps, b, install)?;
        if let Some(p) = p {
            {
                let rt = self.rt.lock();
                if matches!(rt.state, STATE_RUNNING | STATE_DRAINING | STATE_PAUSED) {
                    return Err(EBADFD);
                }
            }
            let mut rt = self.rt.lock();
            rt.params = Some(p);
            rt.buffer = p.buffer_frames() as u64;
            rt.pad = (2 * p.period_frames as u64).min(rt.buffer);
            rt.ring = rt.buffer + rt.pad;
            rt.frame_bytes = p.frame_bytes() as u64;
            rt.boundary = boundary(rt.buffer);
            rt.hw_ptr = 0;
            rt.appl_ptr = 0;
            rt.avail_min = p.period_frames as u64;
            rt.start_threshold = 1;
            rt.stop_threshold = rt.buffer;
            rt.state = STATE_SETUP;
            log::info!(
                "snd: pcmC{}D{}p hw_params {} Hz, {} ch, {}, {} x {} frames",
                self.dev.card,
                self.dev.device,
                p.rate,
                p.channels,
                if p.format == FORMAT_S16_LE { "S16_LE" } else { "S32_LE" },
                p.periods,
                p.period_frames
            );
        }
        Ok(())
    }

    fn sw_params(&self, b: &mut [u8; 136]) -> KResult<()> {
        let rd = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let mut rt = self.rt.lock();
        if rt.state == STATE_OPEN {
            return Err(EBADFD);
        }
        let tstamp_type = u32::from_le_bytes(b[76..80].try_into().unwrap());
        let proto = u32::from_le_bytes(b[72..76].try_into().unwrap());
        rt.avail_min = rd(16).max(1);
        rt.start_threshold = rd(32);
        rt.stop_threshold = rd(40);
        // tstamp_type is valid from protocol 2.0.12 on.
        if proto >= 0x0002000c && tstamp_type <= 2 {
            rt.tstamp_type = tstamp_type;
        }
        let boundary = rt.boundary;
        b[64..72].copy_from_slice(&boundary.to_le_bytes());
        Ok(())
    }

    fn prepare(&self) -> KResult<()> {
        let p = {
            let mut rt = self.rt.lock();
            match rt.state {
                STATE_OPEN => return Err(EBADFD),
                STATE_RUNNING | STATE_DRAINING | STATE_PAUSED => self.stop(&mut rt, STATE_SETUP),
                _ => {}
            }
            rt.params.ok_or(EBADFD)?
        };
        let ring_frames = self.rt.lock().ring;
        self.dev.hw.prepare(&p, ring_frames as u32)?;
        let (ring, _) = self.dev.hw.ring();
        unsafe { core::ptr::write_bytes(ring, 0, (ring_frames * p.frame_bytes() as u64) as usize) };
        let mut rt = self.rt.lock();
        rt.consumed = 0;
        rt.hw_ptr = 0;
        rt.appl_ptr = 0;
        rt.avail_max = 0;
        rt.state = STATE_PREPARED;
        Ok(())
    }

    fn status(&self, ext: bool, b: &mut [u8; 152]) -> KResult<()> {
        let mut rt = self.rt.lock();
        self.update(&mut rt);
        let ty = rt.tstamp_type;
        let w64 = |b: &mut [u8; 152], o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());
        if !ext {
            b.fill(0);
        }
        b[0..4].copy_from_slice(&rt.state.to_le_bytes());
        b[8..24].copy_from_slice(&tstamp_bytes(rt.trigger_ns, ty));
        b[24..40].copy_from_slice(&tstamp_bytes(rt.tstamp_ns, ty));
        w64(b, 40, rt.appl_ptr);
        w64(b, 48, rt.hw_ptr);
        let delay = if matches!(rt.state, STATE_RUNNING | STATE_DRAINING | STATE_PAUSED | STATE_PREPARED) { rt.queued() + rt.pad as i64 } else { 0 };
        w64(b, 56, delay as u64);
        let avail = rt.avail();
        w64(b, 64, avail);
        w64(b, 72, rt.avail_max.max(avail));
        w64(b, 80, 0);
        b[88..92].copy_from_slice(&0i32.to_le_bytes());
        b[92..96].fill(0);
        b[96..112].copy_from_slice(&tstamp_bytes(rt.tstamp_ns, 1));
        b[112..128].copy_from_slice(&tstamp_bytes(rt.tstamp_ns, ty));
        rt.avail_max = avail;
        Ok(())
    }

    fn sync_ptr(&self, b: &mut [u8; 136]) -> KResult<()> {
        let flags = u32::from_le_bytes(b[0..4].try_into().unwrap());
        let appl = u64::from_le_bytes(b[72..80].try_into().unwrap());
        let avail_min = u64::from_le_bytes(b[80..88].try_into().unwrap());
        let mut rt = self.rt.lock();
        if flags & SYNC_PTR_HWSYNC != 0 {
            match rt.state {
                STATE_XRUN => return Err(EPIPE),
                STATE_OPEN | STATE_SETUP => return Err(EBADFD),
                _ => self.update(&mut rt),
            }
        }
        if flags & SYNC_PTR_APPL == 0 && rt.state != STATE_OPEN && rt.boundary > 0 {
            // Only forward movement within the buffer is valid.
            rt.appl_ptr = appl % rt.boundary;
        }
        if flags & SYNC_PTR_AVAIL_MIN == 0 {
            rt.avail_min = avail_min.max(1);
        }
        let ty = rt.tstamp_type;
        b[8..12].copy_from_slice(&rt.state.to_le_bytes());
        b[12..16].fill(0);
        b[16..24].copy_from_slice(&rt.hw_ptr.to_le_bytes());
        b[24..40].copy_from_slice(&tstamp_bytes(rt.tstamp_ns, ty));
        b[40..44].copy_from_slice(&0i32.to_le_bytes());
        b[48..64].copy_from_slice(&tstamp_bytes(rt.tstamp_ns, ty));
        b[72..80].copy_from_slice(&rt.appl_ptr.to_le_bytes());
        b[80..88].copy_from_slice(&rt.avail_min.to_le_bytes());
        Ok(())
    }

    /// WRITEI_FRAMES: copy `frames` frames from `buf`; returns the count.
    fn write(&self, f: &OpenFile, buf: u64, frames: u64) -> KResult<u64> {
        let mm = crate::process::current().mm();
        let mut done = 0u64;
        while done < frames {
            let mut res: KResult<()> = Ok(());
            let mut room = 0u64;
            let mut at = 0u64;
            let mut fb = 0u64;
            let mut buffer = 0u64;
            let want = (frames - done).min(self.rt.lock().avail_min.max(1));
            let ready = |res: &mut KResult<()>, room: &mut u64, at: &mut u64, fb: &mut u64, buffer: &mut u64| -> bool {
                let mut rt = self.rt.lock();
                self.update(&mut rt);
                match rt.state {
                    STATE_PREPARED | STATE_RUNNING | STATE_PAUSED => {}
                    STATE_XRUN => {
                        *res = Err(EPIPE);
                        return true;
                    }
                    7 => {
                        *res = Err(ESTRPIPE);
                        return true;
                    }
                    _ => {
                        *res = Err(EBADFD);
                        return true;
                    }
                }
                *room = rt.avail();
                *at = (rt.appl_ptr + rt.pad) % rt.ring;
                *fb = rt.frame_bytes;
                *buffer = rt.ring;
                if *room == 0 && rt.state == STATE_PREPARED {
                    // Full before the start threshold: start anyway.
                    self.start(&mut rt);
                }
                *room >= want || (f.nonblock() && *room > 0)
            };
            if !ready(&mut res, &mut room, &mut at, &mut fb, &mut buffer) {
                if f.nonblock() {
                    res = Err(EAGAIN);
                } else {
                    let w = sched::wait_until(self.wq.key(), Some(crate::time::monotonic_ns() + 1_000_000_000), WaitMode::Interruptible, || {
                        ready(&mut res, &mut room, &mut at, &mut fb, &mut buffer)
                    });
                    match w {
                        Wake::Ready => {}
                        Wake::Timeout => res = Err(EIO),
                        Wake::Interrupted => res = Err(ERESTARTSYS),
                    }
                }
            }
            if let Err(e) = res {
                return if done > 0 { Ok(done) } else { Err(e) };
            }
            // Copy into the ring (outside the lock: it never moves).
            let n = room.min(frames - done);
            let (ring, cap) = self.dev.hw.ring();
            let first = n.min(buffer - at);
            let ring = unsafe { core::slice::from_raw_parts_mut(ring, cap) };
            let src = buf + done * fb;
            mm.copy_in(src, &mut ring[(at * fb) as usize..((at + first) * fb) as usize])?;
            if n > first {
                mm.copy_in(src + first * fb, &mut ring[..((n - first) * fb) as usize])?;
            }
            let mut rt = self.rt.lock();
            if !matches!(rt.state, STATE_PREPARED | STATE_RUNNING | STATE_PAUSED) {
                return if done > 0 { Ok(done) } else { Err(EBADFD) };
            }
            rt.appl_ptr = (rt.appl_ptr + n) % rt.boundary;
            done += n;
            if rt.state == STATE_PREPARED && rt.queued() >= rt.start_threshold.max(1) as i64 {
                self.start(&mut rt);
            }
        }
        Ok(done)
    }

    fn drain(&self, f: &OpenFile) -> KResult<()> {
        {
            let mut rt = self.rt.lock();
            match rt.state {
                STATE_OPEN => return Err(EBADFD),
                STATE_PREPARED if rt.queued() > 0 => {
                    self.start(&mut rt);
                    rt.state = STATE_DRAINING;
                }
                STATE_RUNNING => rt.state = STATE_DRAINING,
                STATE_DRAINING => {}
                STATE_PAUSED => {
                    self.dev.hw.trigger(true);
                    rt.state = STATE_DRAINING;
                    rt.last_ns = crate::time::monotonic_ns();
                    rt.moved_ns = rt.last_ns;
                    self.arm(&mut rt);
                }
                _ => {
                    rt.state = STATE_SETUP;
                    return Ok(());
                }
            }
        }
        if f.nonblock() {
            return Err(EAGAIN);
        }
        loop {
            let w = sched::wait_until(self.wq.key(), Some(crate::time::monotonic_ns() + 1_000_000_000), WaitMode::Interruptible, || {
                let mut rt = self.rt.lock();
                self.update(&mut rt);
                rt.state != STATE_DRAINING
            });
            match w {
                Wake::Ready => break,
                Wake::Interrupted => return Err(ERESTARTSYS),
                Wake::Timeout => {}
            }
        }
        let mut rt = self.rt.lock();
        if rt.state == STATE_XRUN {
            rt.state = STATE_SETUP;
        }
        Ok(())
    }
}

impl timer::TimerCallback for Substream {
    fn fire(&self, id: u64) {
        {
            let mut rt = self.rt.lock();
            if rt.timer != Some(id) {
                return;
            }
            rt.timer = None;
            self.update(&mut rt);
            self.arm(&mut rt);
        }
        self.wq.wake(POLLOUT);
    }
}

/// Largest buffer * 2^k that keeps pointer differences in an i64.
fn boundary(buffer: u64) -> u64 {
    let mut b = buffer.max(1);
    while b * 2 <= (i64::MAX as u64 / 2) - buffer {
        b *= 2;
    }
    b
}

/// An snd_interval: min, max, open ends, integer, empty.
#[derive(Clone, Copy, Debug)]
struct Interval {
    min: u32,
    max: u32,
}

fn read_interval(b: &[u8], i: usize) -> Interval {
    let o = HWP_INTERVALS + (i - P_SAMPLE_BITS) * 12;
    let r = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (mut min, mut max, flags) = (r(o), r(o + 4), r(o + 8));
    // Open ends of integer parameters close inwards.
    if flags & 1 != 0 {
        min = min.saturating_add(1);
    }
    if flags & 2 != 0 && max > 0 {
        max -= 1;
    }
    if flags & 8 != 0 {
        return Interval { min: 1, max: 0 };
    }
    Interval { min, max }
}

fn write_interval(b: &mut [u8], i: usize, v: Interval) {
    let o = HWP_INTERVALS + (i - P_SAMPLE_BITS) * 12;
    b[o..o + 4].copy_from_slice(&v.min.to_le_bytes());
    b[o + 4..o + 8].copy_from_slice(&v.max.to_le_bytes());
    let flags: u32 = 4 | if v.min > v.max { 8 } else { 0 };
    b[o + 8..o + 12].copy_from_slice(&flags.to_le_bytes());
}

fn read_mask(b: &[u8], i: usize) -> u64 {
    let o = HWP_MASKS + i * 32;
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn write_mask(b: &mut [u8], i: usize, v: u64) {
    let o = HWP_MASKS + i * 32;
    b[o..o + 32].fill(0);
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

fn isect(a: Interval, lo: u32, hi: u32) -> Interval {
    Interval { min: a.min.max(lo), max: a.max.min(hi) }
}

fn div_up(a: u64, b: u64) -> u64 {
    a.div_ceil(b.max(1))
}

/// HW_REFINE / HW_PARAMS on the raw struct `b`: narrow every parameter
/// to what `caps` allows (EINVAL if something becomes empty) and, when
/// `install`, choose one configuration (the smallest format, channels,
/// rate, period size and count left) and return it.
fn refine(caps: &HwCaps, b: &mut [u8], install: bool) -> KResult<Option<Params>> {
    let access = read_mask(b, P_ACCESS) & (1 << ACCESS_RW_INTERLEAVED);
    let mut formats = read_mask(b, P_FORMAT) & caps.formats;
    let subformat = read_mask(b, P_SUBFORMAT) & (1 << SUBFORMAT_STD);
    // Sample bits narrow the formats.
    let sbits = read_interval(b, P_SAMPLE_BITS);
    for (f, bits) in [(FORMAT_S16_LE, 16u32), (FORMAT_S32_LE, 32)] {
        if bits < sbits.min || bits > sbits.max {
            formats &= !(1 << f);
        }
    }
    if access == 0 || formats == 0 || subformat == 0 {
        return Err(EINVAL);
    }
    let fbits_of = |f: u32| if f == FORMAT_S16_LE { 16 } else { 32 };
    let fmt_list: Vec<u32> = [FORMAT_S16_LE, FORMAT_S32_LE].into_iter().filter(|f| formats & (1 << f) != 0).collect();
    let sample_bits = Interval { min: fbits_of(fmt_list[0]), max: fbits_of(*fmt_list.last().unwrap()) };

    let mut channels = isect(read_interval(b, P_CHANNELS), caps.channels_min, caps.channels_max);
    let fbits = read_interval(b, P_FRAME_BITS);
    channels = isect(channels, div_up(fbits.min as u64, sample_bits.max as u64) as u32, fbits.max / sample_bits.min.max(1));
    let r = read_interval(b, P_RATE);
    let rates: Vec<u32> = caps.rates.iter().copied().filter(|&x| x >= r.min && x <= r.max).collect();
    if channels.min > channels.max || rates.is_empty() {
        return Err(EINVAL);
    }
    let rate = Interval { min: rates[0], max: *rates.last().unwrap() };

    // Sizes in frames. Frame size range in bytes:
    let fbytes_min = (sample_bits.min / 8 * channels.min) as u64;
    let fbytes_max = (sample_bits.max / 8 * channels.max) as u64;
    let align = caps.period_align as u64;
    // Smallest period: one alignment unit at the largest frame size.
    let mut psize = isect(read_interval(b, P_PERIOD_SIZE), div_up(align, fbytes_max).max(16) as u32, (caps.max_buffer_bytes as u64 / 2 / fbytes_min) as u32);
    let mut periods = isect(read_interval(b, P_PERIODS), 2, caps.periods_max);
    let mut bsize = isect(read_interval(b, P_BUFFER_SIZE), 0, (caps.max_buffer_bytes as u64 / fbytes_min) as u32);
    // Byte limits.
    let pbytes = read_interval(b, P_PERIOD_BYTES);
    psize = isect(psize, div_up(pbytes.min as u64, fbytes_max) as u32, (pbytes.max as u64 / fbytes_min).min(u32::MAX as u64) as u32);
    let bbytes = read_interval(b, P_BUFFER_BYTES);
    bsize = isect(bsize, div_up(bbytes.min as u64, fbytes_max) as u32, (bbytes.max as u64 / fbytes_min).min(u32::MAX as u64) as u32);
    // Times (us) only once the rate is known.
    if rate.min == rate.max {
        let fr = |t: u32| t as u64 * rate.min as u64 / 1_000_000;
        let pt = read_interval(b, P_PERIOD_TIME);
        psize = isect(psize, div_up(pt.min as u64 * rate.min as u64, 1_000_000) as u32, fr(pt.max).min(u32::MAX as u64) as u32);
        let bt = read_interval(b, P_BUFFER_TIME);
        bsize = isect(bsize, div_up(bt.min as u64 * rate.min as u64, 1_000_000) as u32, fr(bt.max).min(u32::MAX as u64) as u32);
    }
    // buffer = period * periods, both ways.
    for _ in 0..2 {
        bsize = isect(bsize, psize.min.saturating_mul(periods.min), psize.max.saturating_mul(periods.max));
        psize = isect(psize, div_up(bsize.min as u64, periods.max as u64) as u32, bsize.max / periods.min.max(1));
        periods = isect(periods, div_up(bsize.min as u64, psize.max as u64) as u32, bsize.max / psize.min.max(1));
    }
    if psize.min > psize.max || periods.min > periods.max || bsize.min > bsize.max {
        return Err(EINVAL);
    }

    let mut chosen = None;
    let (mut formats, mut channels, mut rate, mut psize, mut periods, mut bsize) = (formats, channels, rate, psize, periods, bsize);
    let mut sample_bits = sample_bits;
    if install {
        let format = fmt_list[0];
        let ch = channels.min;
        let fbytes = (fbits_of(format) / 8 * ch) as u64;
        // Period: the smallest allowed size whose byte length is aligned.
        let unit = (align / gcd(align, fbytes)) as u32;
        let mut pf = psize.min.div_ceil(unit) * unit;
        let np = periods.min.max(div_up(bsize.min as u64, pf as u64) as u32);
        if pf > psize.max || np > periods.max || (pf as u64 * np as u64 * fbytes) > caps.max_buffer_bytes as u64 {
            // Fall back to the largest aligned period that fits.
            pf = (psize.max / unit) * unit;
            if pf == 0 || (pf as u64 * np as u64 * fbytes) > caps.max_buffer_bytes as u64 {
                return Err(EINVAL);
            }
        }
        let p = Params { format, channels: ch, rate: rate.min, period_frames: pf, periods: np };
        formats = 1 << format;
        sample_bits = Interval { min: fbits_of(format), max: fbits_of(format) };
        channels = Interval { min: ch, max: ch };
        rate = Interval { min: p.rate, max: p.rate };
        psize = Interval { min: pf, max: pf };
        periods = Interval { min: np, max: np };
        bsize = Interval { min: pf * np, max: pf * np };
        chosen = Some(p);
    }
    let fbmin = sample_bits.min * channels.min;
    let fbmax = sample_bits.max * channels.max;
    let fbyte = |bits: u32| bits as u64 / 8;
    let sat = |v: u64| v.min(u32::MAX as u64) as u32;
    write_mask(b, P_ACCESS, access);
    write_mask(b, P_FORMAT, formats);
    write_mask(b, P_SUBFORMAT, subformat);
    write_interval(b, P_SAMPLE_BITS, sample_bits);
    write_interval(b, P_FRAME_BITS, Interval { min: fbmin, max: fbmax });
    write_interval(b, P_CHANNELS, channels);
    write_interval(b, P_RATE, rate);
    write_interval(b, P_PERIOD_SIZE, psize);
    write_interval(b, P_PERIOD_BYTES, Interval { min: sat(psize.min as u64 * fbyte(fbmin)), max: sat(psize.max as u64 * fbyte(fbmax)) });
    write_interval(b, P_PERIODS, periods);
    write_interval(b, P_BUFFER_SIZE, bsize);
    write_interval(b, P_BUFFER_BYTES, Interval { min: sat(bsize.min as u64 * fbyte(fbmin)), max: sat(bsize.max as u64 * fbyte(fbmax)) });
    let t = |f: u32, r: u32| sat(f as u64 * 1_000_000 / r.max(1) as u64);
    write_interval(b, P_PERIOD_TIME, Interval { min: t(psize.min, rate.max), max: t(psize.max, rate.min) });
    write_interval(b, P_BUFFER_TIME, Interval { min: t(bsize.min, rate.max), max: t(bsize.max, rate.min) });
    write_interval(b, P_TICK_TIME, Interval { min: 0, max: 0 });
    b[HWP_RMASK..HWP_RMASK + 4].copy_from_slice(&0u32.to_le_bytes());
    b[HWP_CMASK..HWP_CMASK + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    b[HWP_INFO..HWP_INFO + 4].copy_from_slice(&(INFO_INTERLEAVED | INFO_BLOCK_TRANSFER | INFO_PAUSE).to_le_bytes());
    let msbits = if formats == 1 << FORMAT_S32_LE { caps.msbits32 } else if formats == 1 << FORMAT_S16_LE { 16 } else { 0 };
    b[HWP_MSBITS..HWP_MSBITS + 4].copy_from_slice(&msbits.to_le_bytes());
    let (num, den) = if rate.min == rate.max { (rate.min, 1u32) } else { (0, 0) };
    b[HWP_RATE_NUM..HWP_RATE_NUM + 4].copy_from_slice(&num.to_le_bytes());
    b[HWP_RATE_DEN..HWP_RATE_DEN + 4].copy_from_slice(&den.to_le_bytes());
    b[HWP_FIFO_SIZE..HWP_FIFO_SIZE + 8].copy_from_slice(&0u64.to_le_bytes());
    Ok(chosen)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

pub struct PcmFile(Arc<Substream>);

fn copy_out(arg: u64, b: &[u8]) -> KResult<()> {
    crate::process::current().mm().copy_out(arg, b)
}

fn copy_in(arg: u64, b: &mut [u8]) -> KResult<()> {
    crate::process::current().mm().copy_in(arg, b)
}

/// struct snd_pcm_info (288 bytes).
pub fn pcm_info(dev: &PcmDevice) -> [u8; 288] {
    let mut b = [0u8; 288];
    b[0..4].copy_from_slice(&dev.device.to_le_bytes());
    b[12..16].copy_from_slice(&dev.card.to_le_bytes());
    let put = |b: &mut [u8; 288], o: usize, len: usize, s: &str| {
        let n = s.len().min(len - 1);
        b[o..o + n].copy_from_slice(&s.as_bytes()[..n]);
    };
    put(&mut b, 16, 64, &dev.id);
    put(&mut b, 80, 80, &dev.name);
    put(&mut b, 160, 32, "subdevice #0");
    b[200..204].copy_from_slice(&1u32.to_le_bytes());
    let avail = if dev.busy.load(Ordering::Relaxed) { 0u32 } else { 1 };
    b[204..208].copy_from_slice(&avail.to_le_bytes());
    b
}

impl FileOps for PcmFile {
    fn ioctl(&self, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let s = &self.0;
        if (cmd >> 8) & 0xff != b'A' as u32 {
            return Err(ENOTTY);
        }
        match cmd & 0xff {
            // PVERSION: 2.0.15
            0x00 => copy_out(arg, &0x0002000fi32.to_le_bytes()).map(|_| 0),
            0x01 => copy_out(arg, &pcm_info(&s.dev)).map(|_| 0),
            // TSTAMP, USER_PVERSION
            0x02 | 0x04 => Ok(0),
            // TTSTAMP
            0x03 => {
                let mut v = [0u8; 4];
                copy_in(arg, &mut v)?;
                let ty = u32::from_le_bytes(v);
                if ty > 2 {
                    return Err(EINVAL);
                }
                s.rt.lock().tstamp_type = ty;
                Ok(0)
            }
            // HW_REFINE, HW_PARAMS
            n @ (0x10 | 0x11) => {
                let mut b = [0u8; HW_PARAMS_SIZE];
                copy_in(arg, &mut b)?;
                s.hw_refine(&mut b, n == 0x11)?;
                copy_out(arg, &b).map(|_| 0)
            }
            // HW_FREE
            0x12 => {
                {
                    let mut rt = s.rt.lock();
                    if matches!(rt.state, STATE_RUNNING | STATE_DRAINING | STATE_PAUSED) {
                        return Err(EBADFD);
                    }
                    rt.state = STATE_OPEN;
                    rt.params = None;
                }
                s.dev.hw.release();
                Ok(0)
            }
            0x13 => {
                let mut b = [0u8; 136];
                copy_in(arg, &mut b)?;
                s.sw_params(&mut b)?;
                copy_out(arg, &b).map(|_| 0)
            }
            // STATUS, STATUS_EXT
            n @ (0x20 | 0x24) => {
                let mut b = [0u8; 152];
                if n == 0x24 {
                    copy_in(arg, &mut b)?;
                }
                s.status(n == 0x24, &mut b)?;
                copy_out(arg, &b).map(|_| 0)
            }
            // DELAY
            0x21 => {
                let mut rt = s.rt.lock();
                s.update(&mut rt);
                let d = match rt.state {
                    STATE_RUNNING | STATE_DRAINING | STATE_PREPARED | STATE_PAUSED => rt.queued() + rt.pad as i64,
                    STATE_XRUN => return Err(EPIPE),
                    _ => return Err(EBADFD),
                };
                drop(rt);
                copy_out(arg, &d.to_le_bytes()).map(|_| 0)
            }
            // HWSYNC
            0x22 => {
                let mut rt = s.rt.lock();
                match rt.state {
                    STATE_XRUN => Err(EPIPE),
                    STATE_OPEN | STATE_SETUP => Err(EBADFD),
                    _ => {
                        s.update(&mut rt);
                        Ok(0)
                    }
                }
            }
            0x23 => {
                let mut b = [0u8; 136];
                copy_in(arg, &mut b)?;
                s.sync_ptr(&mut b)?;
                copy_out(arg, &b).map(|_| 0)
            }
            // PREPARE
            0x40 => s.prepare().map(|_| 0),
            // RESET
            0x41 => {
                let mut rt = s.rt.lock();
                s.update(&mut rt);
                rt.appl_ptr = rt.hw_ptr;
                Ok(0)
            }
            // START
            0x42 => {
                let mut rt = s.rt.lock();
                if rt.state != STATE_PREPARED {
                    return Err(EBADFD);
                }
                s.start(&mut rt);
                Ok(0)
            }
            // DROP
            0x43 => {
                let mut rt = s.rt.lock();
                if rt.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                if rt.state != STATE_SETUP {
                    s.stop(&mut rt, STATE_SETUP);
                }
                drop(rt);
                s.wq.wake(POLLOUT | POLLERR);
                Ok(0)
            }
            0x44 => s.drain(f).map(|_| 0),
            // PAUSE
            0x45 => {
                let push = arg != 0;
                let mut rt = s.rt.lock();
                match (rt.state, push) {
                    (STATE_RUNNING, true) => {
                        s.update(&mut rt);
                        s.dev.hw.trigger(false);
                        rt.state = STATE_PAUSED;
                        s.arm(&mut rt);
                    }
                    (STATE_PAUSED, false) => {
                        rt.state = STATE_RUNNING;
                        rt.last_ns = crate::time::monotonic_ns();
                        rt.moved_ns = rt.last_ns;
                        s.dev.hw.trigger(true);
                        s.arm(&mut rt);
                    }
                    _ => return Err(EBADFD),
                }
                Ok(0)
            }
            // RESUME (nothing suspends): not supported, like no INFO_RESUME.
            0x47 => Err(ENOSYS),
            // XRUN
            0x48 => {
                let mut rt = s.rt.lock();
                match rt.state {
                    STATE_RUNNING | STATE_PREPARED | STATE_PAUSED => {
                        s.stop(&mut rt, STATE_XRUN);
                        Ok(0)
                    }
                    _ => Err(EBADFD),
                }
            }
            // WRITEI_FRAMES: struct snd_xferi { result, buf, frames }
            0x50 => {
                let mut x = [0u8; 24];
                copy_in(arg, &mut x)?;
                let buf = u64::from_le_bytes(x[8..16].try_into().unwrap());
                let frames = u64::from_le_bytes(x[16..24].try_into().unwrap());
                let r = s.write(f, buf, frames);
                let n = *r.as_ref().unwrap_or(&0);
                copy_out(arg, &n.to_le_bytes())?;
                r.map(|_| 0)
            }
            // LINK/UNLINK: single substreams only.
            0x60 | 0x61 => Err(EINVAL),
            _ => Err(ENOTTY),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let s = &self.0;
        let mut rt = s.rt.lock();
        s.update(&mut rt);
        match rt.state {
            STATE_RUNNING | STATE_PREPARED | STATE_PAUSED if rt.avail() >= rt.avail_min => POLLOUT | POLLWRNORM,
            STATE_RUNNING | STATE_PREPARED | STATE_PAUSED | STATE_DRAINING => 0,
            _ => POLLOUT | POLLWRNORM | POLLERR,
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
        let s = &self.0;
        let had_params = {
            let mut rt = s.rt.lock();
            s.stop(&mut rt, STATE_OPEN);
            rt.params.take().is_some()
        };
        if had_params {
            s.dev.hw.release();
        }
        s.dev.busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> HwCaps {
        HwCaps {
            formats: 1 << FORMAT_S16_LE | 1 << FORMAT_S32_LE,
            rates: alloc::vec![44100, 48000, 96000],
            channels_min: 2,
            channels_max: 2,
            max_buffer_bytes: 256 * 1024,
            period_align: 128,
            periods_max: 32,
            msbits32: 24,
        }
    }

    /// An "any" hw_params, as snd_pcm_hw_params_any / tinyalsa's param_init.
    fn any() -> [u8; HW_PARAMS_SIZE] {
        let mut b = [0u8; HW_PARAMS_SIZE];
        for i in 0..3 {
            b[HWP_MASKS + i * 32..HWP_MASKS + i * 32 + 32].fill(0xff);
        }
        for i in P_SAMPLE_BITS..=P_TICK_TIME {
            let o = HWP_INTERVALS + (i - P_SAMPLE_BITS) * 12;
            b[o + 4..o + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        b
    }

    fn set_int(b: &mut [u8], i: usize, v: u32) {
        write_interval(b, i, Interval { min: v, max: v });
    }

    #[test_case]
    fn pcm_hw_params_tinyalsa() {
        // What pcm_set_config sends for 48 kHz stereo S16, 4 x 1024.
        let mut b = any();
        write_mask(&mut b, P_FORMAT, 1 << FORMAT_S16_LE);
        write_mask(&mut b, P_ACCESS, 1 << ACCESS_RW_INTERLEAVED);
        write_interval(&mut b, P_PERIOD_SIZE, Interval { min: 1024, max: u32::MAX });
        set_int(&mut b, P_CHANNELS, 2);
        set_int(&mut b, P_PERIODS, 4);
        set_int(&mut b, P_RATE, 48000);
        let p = refine(&caps(), &mut b, true).unwrap().unwrap();
        assert_eq!(p, Params { format: FORMAT_S16_LE, channels: 2, rate: 48000, period_frames: 1024, periods: 4 });
        assert_eq!(read_interval(&b, P_BUFFER_BYTES).min, 16384);
        // An odd period is rounded up to 128-byte periods (32 frames).
        let mut b = any();
        write_mask(&mut b, P_FORMAT, 1 << FORMAT_S16_LE);
        write_interval(&mut b, P_PERIOD_SIZE, Interval { min: 1000, max: u32::MAX });
        set_int(&mut b, P_PERIODS, 2);
        set_int(&mut b, P_RATE, 44100);
        let p = refine(&caps(), &mut b, true).unwrap().unwrap();
        assert_eq!((p.period_frames, p.rate), (1024, 44100));
        // Unsupported rate and channel count.
        let mut b = any();
        set_int(&mut b, P_RATE, 22050);
        assert_eq!(refine(&caps(), &mut b, true), Err(EINVAL));
        let mut b = any();
        set_int(&mut b, P_CHANNELS, 6);
        assert_eq!(refine(&caps(), &mut b, false), Err(EINVAL));
        // Refining "any" keeps ranges.
        let mut b = any();
        assert_eq!(refine(&caps(), &mut b, false), Ok(None));
        let r = read_interval(&b, P_RATE);
        assert_eq!((r.min, r.max), (44100, 96000));
    }

    #[test_case]
    fn pcm_pointer_math() {
        let mut rt = Runtime { buffer: 4096, boundary: boundary(4096), ..Default::default() };
        assert_eq!(rt.avail(), 4096);
        rt.appl_ptr = 3000;
        assert_eq!((rt.avail(), rt.queued()), (1096, 3000));
        rt.hw_ptr = 3500;
        assert_eq!((rt.avail(), rt.queued()), (4596, -500));
        assert!(rt.boundary % 4096 == 0 && rt.boundary > 1 << 60);
    }
}
