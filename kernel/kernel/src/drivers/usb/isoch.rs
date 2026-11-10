//! xHCI isochronous endpoints (E8): the streaming endpoints of USB video
//! (UVC) and audio (UAC) devices, IN or OUT, full/high/super speed.
//!
//! An endpoint has a transfer ring of several pages and one DMA buffer
//! slot per ring position. Each TD is one Isoch TRB covering one service
//! interval (ESIT): the endpoint's max packet size times its burst (high
//! speed "high-bandwidth" endpoints move up to three packets per
//! microframe: Max Burst = mult - 1, TBC 0, TLBPC = packets - 1). Every TD
//! has Start Isoch ASAP set: the controller puts it in the interval after
//! the previous one (or the next free one after an underrun), so frame IDs
//! never need computing ([`isoch_trb`] can encode one anyway).
//!
//! Completions: an IN TD raises an event when it comes back short (ISP),
//! fails, or carries IOC, which only every `ioc_every`-th TD does; a TD
//! the controller completed without an event moved its full length. The
//! TDs are kept in posting order, so an event for TD k also completes the
//! silent ones before it. A buffer whose TD never ran (a missed interval
//! the controller reported once for several) would look full: with
//! `sentinel` set, its first byte is zeroed when posted, and a "full" TD
//! still starting with 0 counts as empty (a UVC payload header is never
//! 0 bytes long). Each TD's bytes go to the endpoint's [`IsochSink`], in
//! order, from the soft interrupt (or a task waiting on the controller).
//!
//! Isochronous endpoints never halt: errors (missed service, transaction
//! errors, ring under/overruns) are counted, the TD is given to the sink
//! as empty with `err` set, and the ring is topped up again.
//!
//! [`Xhci::isoch_start`] adds the endpoint (Configure Endpoint, dropping an
//! older context of the same endpoint), then selects the alternate setting
//! (SET_INTERFACE), as Linux's usb_set_interface does; a bandwidth error
//! is returned so the caller can try a smaller alternate setting.
//! [`Xhci::isoch_stop`] stops and drops the endpoint and selects alternate
//! setting 0.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use super::{CC_SHORT_PACKET, CC_SUCCESS, SPEED_FULL, SPEED_LOW, SPEED_SUPER, TRB_CONFIGURE_EP, TRB_IOC, TRB_ISP, TRB_LINK, TRB_STOP_EP, UsbDev, Xhci, pci, trb_ptr};

pub(super) const TRB_ISOCH: u32 = 5;
const TRB_SIA: u32 = 1 << 31;
pub(super) const CC_RING_UNDERRUN: u32 = 14;
pub(super) const CC_RING_OVERRUN: u32 = 15;
pub(super) const CC_BANDWIDTH: u32 = 8;
pub(super) const CC_SECONDARY_BANDWIDTH: u32 = 35;
pub(super) const CC_MISSED_SERVICE: u32 = 23;
const CC_STOPPED: u32 = 26;
const CC_STOPPED_LENGTH_INVALID: u32 = 27;
const CC_STOPPED_SHORT: u32 = 28;
/// Bytes of DMA buffer allocated at a time (slots never cross 64 KiB).
const CHUNK: usize = 0x10000;

/// Where the data of an isochronous endpoint goes.
pub trait IsochSink: Send {
    /// One service interval's data (IN), in order; empty with `err` for a
    /// missed or failed interval.
    fn packet(&mut self, data: &[u8], err: bool);

    /// OUT endpoints: fill the next interval's buffer, return its length.
    fn fill(&mut self, _buf: &mut [u8]) -> usize {
        0
    }
}

/// An isochronous endpoint as its descriptors give it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IsochDesc {
    pub addr: u8,
    /// wMaxPacketSize as sent (bits 11-12: extra transactions per
    /// microframe on high speed).
    pub max_packet: u16,
    pub interval: u8,
    /// SuperSpeed companion bMaxBurst and Mult.
    pub ss_burst: u8,
    pub ss_mult: u8,
}

impl IsochDesc {
    pub fn mps(&self) -> u32 {
        (self.max_packet & 0x7ff) as u32
    }

    /// Packets per service interval (high-bandwidth transactions, or
    /// SuperSpeed bursts times Mult).
    pub fn packets(&self, speed: u32) -> u32 {
        if speed >= SPEED_SUPER {
            (self.ss_burst as u32 + 1) * (self.ss_mult as u32 + 1)
        } else {
            ((self.max_packet >> 11) & 3) as u32 + 1
        }
    }

    /// Bytes per service interval (Max ESIT Payload).
    pub fn esit_bytes(&self, speed: u32) -> u32 {
        self.mps() * self.packets(speed)
    }

    pub fn is_in(&self) -> bool {
        self.addr & 0x80 != 0
    }
}

/// The endpoint context's Interval for an isochronous endpoint (2^n x
/// 125 us): bInterval is an exponent at every speed (frames on full speed).
pub fn isoch_interval(speed: u32, b_interval: u8) -> u32 {
    let e = b_interval.clamp(1, 16) as u32 - 1;
    match speed {
        SPEED_FULL | SPEED_LOW => (e + 3).min(15),
        _ => e.min(15),
    }
}

/// Service intervals per second.
pub fn intervals_per_sec(speed: u32, b_interval: u8) -> u32 {
    8000 >> isoch_interval(speed, b_interval)
}

/// Endpoint context dwords 0, 1 and 4 of an isochronous endpoint.
pub fn isoch_ep_words(speed: u32, d: &IsochDesc) -> [u32; 3] {
    let ty = if d.is_in() { 5 } else { 1 };
    let (burst, mult) = if speed >= SPEED_SUPER {
        (d.ss_burst as u32, d.ss_mult as u32)
    } else {
        (d.packets(speed) - 1, 0)
    };
    let esit = d.esit_bytes(speed);
    [
        mult << 8 | isoch_interval(speed, d.interval) << 16 | (esit >> 16) << 24,
        // CErr 0: isochronous endpoints never retry.
        ty << 3 | burst << 8 | d.mps() << 16,
        esit.min(0xffff) | (esit & 0xffff) << 16,
    ]
}

/// An Isoch TRB for one TD of `len` bytes (packets of `mps`, `burst` + 1
/// per burst). `frame`: None for Start Isoch ASAP.
pub fn isoch_trb(buf: u64, len: u32, mps: u32, burst: u32, input: bool, ioc: bool, frame: Option<u32>) -> [u32; 4] {
    let packets = len.div_ceil(mps.max(1)).max(1);
    let tbc = packets.div_ceil(burst + 1) - 1;
    let tlbpc = (packets - 1) % (burst + 1);
    let when = match frame {
        None => TRB_SIA,
        Some(f) => (f & 0x7ff) << 20,
    };
    [
        buf as u32,
        (buf >> 32) as u32,
        len & 0x1_ffff,
        TRB_ISOCH << 10 | tbc << 7 | tlbpc << 16 | when | if input { TRB_ISP } else { 0 } | if ioc { TRB_IOC } else { 0 },
    ]
}

/// A transfer ring of `pages` contiguous pages; the last TRB links back.
struct IsoRing {
    phys: u64,
    ptr: *mut u32,
    len: usize,
    idx: usize,
    cycle: u32,
}

impl IsoRing {
    fn new(pages: usize) -> Option<IsoRing> {
        let (phys, ptr) = pci::dma_alloc(pages)?;
        let mut r = IsoRing { phys, ptr: ptr as *mut u32, len: pages * 256, idx: 0, cycle: 1 };
        r.reset();
        Some(r)
    }

    /// Empty the ring (the endpoint is not running).
    fn reset(&mut self) {
        unsafe {
            core::ptr::write_bytes(self.ptr, 0, self.len * 4);
            let link = self.ptr.add((self.len - 1) * 4);
            *link = self.phys as u32;
            *link.add(1) = (self.phys >> 32) as u32;
            *link.add(3) = TRB_LINK << 10 | 2;
        }
        self.idx = 0;
        self.cycle = 1;
    }

    /// Usable positions (all but the link).
    fn slots(&self) -> usize {
        self.len - 1
    }

    /// Enqueue a TRB; returns its position.
    fn push(&mut self, t: [u32; 4]) -> usize {
        let pos = self.idx;
        unsafe {
            let p = self.ptr.add(pos * 4);
            for i in 0..3 {
                core::ptr::write_volatile(p.add(i), t[i]);
            }
            fence(Ordering::SeqCst);
            core::ptr::write_volatile(p.add(3), (t[3] & !1) | self.cycle);
        }
        self.idx += 1;
        if self.idx == self.len - 1 {
            unsafe {
                let link = self.ptr.add(self.idx * 4 + 3);
                core::ptr::write_volatile(link, TRB_LINK << 10 | 2 | self.cycle);
            }
            self.idx = 0;
            self.cycle ^= 1;
        }
        pos
    }

    /// The position of a TRB address, if it is one of ours.
    fn pos_of(&self, addr: u64) -> Option<usize> {
        let off = addr.checked_sub(self.phys)?;
        let pos = (off / 16) as usize;
        (off % 16 == 0 && pos < self.len - 1).then_some(pos)
    }

    fn dequeue_ptr(&self) -> u64 {
        self.phys + self.idx as u64 * 16 | self.cycle as u64
    }
}

/// Counters of one endpoint, for the status line and tests.
#[derive(Clone, Copy, Default, Debug)]
pub struct IsochStats {
    pub tds: u64,
    pub bytes: u64,
    pub empty: u64,
    pub errors: u64,
    pub missed: u64,
    pub overruns: u64,
    pub last_error: u32,
}

pub(super) struct IsochEp {
    pub slot: u32,
    pub dci: u32,
    pub desc: IsochDesc,
    pub label: String,
    ring: IsoRing,
    /// DMA buffer chunks; ring position p uses bytes p * slot_bytes on.
    chunks: Vec<(u64, *mut u8)>,
    slot_bytes: usize,
    /// Bytes per TD (one service interval).
    td_len: u32,
    mps: u32,
    burst: u32,
    ioc_every: usize,
    sentinel: bool,
    /// TDs to keep posted.
    target: usize,
    /// Positions posted, oldest first.
    posted: VecDeque<usize>,
    posted_count: u64,
    pub active: bool,
    sink: Option<Box<dyn IsochSink>>,
    pub stats: IsochStats,
    /// The stats printed last.
    shown: (u64, u64, u64),
}

unsafe impl Send for IsochEp {}

impl IsochEp {
    fn buf(&self, pos: usize) -> (u64, *mut u8) {
        let off = pos * self.slot_bytes;
        let (phys, virt) = self.chunks[off / CHUNK];
        let in_chunk = (off % CHUNK) as u64;
        (phys + in_chunk, unsafe { virt.add(in_chunk as usize) })
    }

    /// Post one TD.
    fn post(&mut self) {
        let input = self.desc.is_in();
        let pos = self.ring.idx;
        let (phys, virt) = self.buf(pos);
        let mut len = self.td_len;
        if input {
            if self.sentinel {
                unsafe { *virt = 0 };
            }
        } else {
            let b = unsafe { core::slice::from_raw_parts_mut(virt, self.td_len as usize) };
            len = self.sink.as_mut().map_or(0, |s| s.fill(b)) as u32;
        }
        self.posted_count += 1;
        let ioc = !input || self.posted_count % self.ioc_every as u64 == 0;
        let t = isoch_trb(phys, len, self.mps, self.burst, input, ioc, None);
        let p = self.ring.push(t);
        self.posted.push_back(p);
    }

    /// Keep `target` TDs queued; whether any was added.
    fn refill(&mut self) -> bool {
        let mut added = false;
        while self.active && self.posted.len() < self.target {
            self.post();
            added = true;
        }
        added
    }

    /// Hand a completed TD at `pos` to the sink.
    fn deliver(&mut self, pos: usize, len: usize, err: bool) {
        let (_, virt) = self.buf(pos);
        let s = &mut self.stats;
        s.tds += 1;
        let mut data: &[u8] = if self.desc.is_in() && !err { unsafe { core::slice::from_raw_parts(virt, len.min(self.td_len as usize)) } } else { &[] };
        if self.sentinel && data.first() == Some(&0) {
            data = &[];
        }
        if data.is_empty() {
            s.empty += 1;
        }
        s.bytes += data.len() as u64;
        if let Some(sink) = self.sink.as_mut()
            && self.desc.is_in()
        {
            sink.packet(data, err);
        }
    }

    /// A transfer event for this endpoint; whether TDs were re-posted.
    pub fn complete(&mut self, ev: &[u32; 4]) -> bool {
        let code = ev[2] >> 24;
        let residual = (ev[2] & 0xff_ffff) as usize;
        match code {
            CC_RING_UNDERRUN | CC_RING_OVERRUN => {
                // The ring ran empty: no TD named.
                self.stats.overruns += 1;
                self.stats.last_error = code;
                return self.refill();
            }
            CC_STOPPED | CC_STOPPED_LENGTH_INVALID | CC_STOPPED_SHORT if !self.active => return false,
            _ => {}
        }
        let Some(pos) = self.ring.pos_of(trb_ptr(ev)) else {
            self.stats.errors += 1;
            self.stats.last_error = code;
            return self.refill();
        };
        let Some(k) = self.posted.iter().position(|&p| p == pos) else {
            return self.refill();
        };
        // The TDs before it completed in full without an event.
        for _ in 0..k {
            let p = self.posted.pop_front().unwrap();
            self.deliver(p, self.td_len as usize, false);
        }
        self.posted.pop_front();
        let ok = matches!(code, CC_SUCCESS | CC_SHORT_PACKET);
        if !ok {
            self.stats.errors += 1;
            self.stats.last_error = code;
            if code == CC_MISSED_SERVICE {
                self.stats.missed += 1;
            }
        }
        let len = (self.td_len as usize).saturating_sub(residual);
        self.deliver(pos, len, !ok);
        self.refill()
    }

    /// "if1 ep 82: 1234 TDs, 5 MB, ..." when something changed.
    pub fn status(&mut self) -> Option<String> {
        let s = self.stats;
        let now = (s.tds, s.errors, s.overruns);
        if now == self.shown {
            return None;
        }
        let d_tds = s.tds - self.shown.0;
        self.shown = now;
        Some(format!(
            "{}: {} TDs (+{d_tds}), {} KiB, {} empty, {} errors ({} missed, {} ring overruns, last code {}), {} posted",
            self.label,
            s.tds,
            s.bytes >> 10,
            s.empty,
            s.errors,
            s.missed,
            s.overruns,
            s.last_error,
            self.posted.len()
        ))
    }
}

/// How an isochronous stream is set up.
pub struct IsochParams {
    pub iface: u8,
    pub alt: u8,
    pub desc: IsochDesc,
    /// IN: TDs per IOC (UVC: 8; audio: 1).
    pub ioc_every: usize,
    pub sentinel: bool,
    /// Milliseconds of TDs kept posted.
    pub queue_ms: u32,
    pub label: String,
}

impl Xhci {
    /// Configure Endpoint adding `dci` (and dropping its old context if it
    /// was enabled) or only dropping it; `enabled` is the device's set of
    /// enabled DCIs, updated on success.
    fn configure_isoch(&mut self, d: &UsbDev, enabled: &mut u32, dci: u32, add: Option<(&IsochDesc, u64)>) -> Result<(), u32> {
        let drop = if *enabled & 1 << dci != 0 { 1 << dci } else { 0 };
        let after = (*enabled & !(1 << dci)) | if add.is_some() { 1 << dci } else { 0 };
        let entries = 31 - (after | 1).leading_zeros();
        self.input_ctx(d, 1 | if add.is_some() { 1 << dci } else { 0 });
        unsafe { *self.ctx(d.input.1, 0) = drop };
        self.slot_ctx(d, entries.max(1));
        if let Some((desc, deq)) = add {
            let w = isoch_ep_words(d.speed, desc);
            let e = self.ctx(d.input.1, dci as usize + 1);
            unsafe {
                *e = w[0];
                *e.add(1) = w[1];
                *e.add(2) = deq as u32;
                *e.add(3) = (deq >> 32) as u32;
                *e.add(4) = w[2];
            }
        }
        let slot = d.slot;
        self.command_code([d.input.0 as u32, (d.input.0 >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | slot << 24])?;
        *enabled = after;
        Ok(())
    }

    /// Start streaming: configure the endpoint, select the alternate
    /// setting, queue TDs. Err: the Configure Endpoint completion code
    /// (bandwidth: try a smaller setting), or 0 if SET_INTERFACE failed.
    pub(super) fn isoch_start(&mut self, d: &mut UsbDev, enabled: &mut u32, p: IsochParams, sink: Box<dyn IsochSink>) -> Result<(), u32> {
        let speed = d.speed;
        let dci = super::dci_of(p.desc.addr);
        let td_len = p.desc.esit_bytes(speed);
        let per_sec = intervals_per_sec(speed, p.desc.interval);
        let target = ((per_sec * p.queue_ms / 1000) as usize).max(8);
        let slot_bytes = (td_len as usize).next_power_of_two().clamp(64, CHUNK);
        let i = match self.isochs.iter().position(|e| e.slot == d.slot && e.dci == dci) {
            Some(i) => i,
            None => {
                let pages = (target + 64).div_ceil(255);
                let ring = IsoRing::new(pages).ok_or(0u32)?;
                self.isochs.push(IsochEp {
                    slot: d.slot,
                    dci,
                    desc: p.desc,
                    label: p.label.clone(),
                    ring,
                    chunks: Vec::new(),
                    slot_bytes: 0,
                    td_len: 0,
                    mps: 0,
                    burst: 0,
                    ioc_every: 1,
                    sentinel: false,
                    target: 0,
                    posted: VecDeque::new(),
                    posted_count: 0,
                    active: false,
                    sink: None,
                    stats: IsochStats::default(),
                    shown: (0, 0, 0),
                });
                self.isochs.len() - 1
            }
        };
        {
            let e = &mut self.isochs[i];
            if target + 1 > e.ring.slots() {
                e.ring = IsoRing::new((target + 64).div_ceil(255)).ok_or(0u32)?;
            }
            let need = e.ring.len * slot_bytes;
            if slot_bytes > e.slot_bytes || need > e.chunks.len() * CHUNK {
                e.chunks.clear();
                for _ in 0..need.div_ceil(CHUNK) {
                    e.chunks.push(pci::dma_alloc(CHUNK / 4096).ok_or(0u32)?);
                }
            }
            e.slot_bytes = slot_bytes;
            e.desc = p.desc;
            e.label = p.label.clone();
            e.td_len = td_len;
            e.mps = p.desc.mps();
            e.burst = if speed >= SPEED_SUPER { p.desc.ss_burst as u32 } else { p.desc.packets(speed) - 1 };
            e.ioc_every = p.ioc_every.max(1);
            e.sentinel = p.sentinel;
            e.target = target.min(e.ring.slots() - 1);
            e.ring.reset();
            e.posted.clear();
            e.posted_count = 0;
            e.active = false;
        }
        let deq = self.isochs[i].ring.dequeue_ptr();
        self.configure_isoch(d, enabled, dci, Some((&p.desc, deq)))?;
        if !self.control(d, 0x01, 11, p.alt as u16, p.iface as u16, 0) {
            log::warn!("{}: SET_INTERFACE({}, {}) failed", p.label, p.iface, p.alt);
            let _ = self.configure_isoch(d, enabled, dci, None);
            return Err(0);
        }
        let e = &mut self.isochs[i];
        e.sink = Some(sink);
        e.active = true;
        e.refill();
        log::info!(
            "{}: streaming: if{} alt {}, endpoint {:02x} DCI {dci}, {} x {} bytes per interval ({} per s), {} TDs queued",
            p.label,
            p.iface,
            p.alt,
            p.desc.addr,
            e.burst + 1,
            e.mps,
            per_sec,
            e.posted.len()
        );
        self.ring_doorbell(d.slot, dci);
        Ok(())
    }

    /// Stop streaming on the endpoint `addr`: stop and drop it, select
    /// alternate setting 0 of `iface`.
    pub(super) fn isoch_stop(&mut self, d: &mut UsbDev, enabled: &mut u32, iface: u8, addr: u8) {
        let dci = super::dci_of(addr);
        let slot = d.slot;
        if let Some(e) = self.isochs.iter_mut().find(|e| e.slot == slot && e.dci == dci) {
            e.active = false;
        }
        if *enabled & 1 << dci != 0 {
            let _ = self.command_code([0, 0, 0, TRB_STOP_EP << 10 | dci << 16 | slot << 24]);
            if let Err(code) = self.configure_isoch(d, enabled, dci, None) {
                log::warn!("xhci: slot {slot}: dropping endpoint {addr:02x} failed (code {code})");
            }
        }
        if let Some(e) = self.isochs.iter_mut().find(|e| e.slot == slot && e.dci == dci) {
            e.posted.clear();
            e.sink = None;
        }
        self.control_quiet(d, 0x01, 11, 0, iface as u16, 0, true);
    }

    /// An event for an isochronous endpoint; false if it is not one.
    pub(super) fn isoch_event(&mut self, slot: u32, dci: u32, ev: &[u32; 4]) -> bool {
        let Some(e) = self.isochs.iter_mut().find(|e| e.slot == slot && e.dci == dci) else { return false };
        if e.complete(ev) {
            self.ring_doorbell(slot, dci);
        }
        true
    }

    /// Status lines of the streaming endpoints that moved.
    pub(super) fn isoch_status(&mut self) {
        for e in self.isochs.iter_mut().filter(|e| e.active) {
            if let Some(line) = e.status() {
                log::info!("{line}");
            }
        }
    }
}

#[test_case]
fn isoch_trb_encoding() {
    // The UGREEN camera's alt 11: 3 x 1024 bytes per microframe.
    let d = IsochDesc { addr: 0x82, max_packet: 0x1400, interval: 1, ss_burst: 0, ss_mult: 0 };
    assert_eq!((d.mps(), d.packets(3), d.esit_bytes(3)), (1024, 3, 3072));
    let t = isoch_trb(0x1000, 3072, 1024, 2, true, false, None);
    assert_eq!(t[2], 3072);
    // Type 5, TBC 0, TLBPC 2, ISP, SIA.
    assert_eq!(t[3], 5 << 10 | 2 << 16 | TRB_ISP | TRB_SIA);
    // Endpoint context: interval 0 (every microframe), isoch IN, Max
    // Burst 2, MPS 1024, ESIT payload 3072.
    assert_eq!(isoch_ep_words(3, &d), [0, 5 << 3 | 2 << 8 | 1024 << 16, 3072 | 3072 << 16]);
    // Its microphone: 192 bytes every 2^3 microframes (1 ms).
    let m = IsochDesc { addr: 0x83, max_packet: 192, interval: 4, ss_burst: 0, ss_mult: 0 };
    assert_eq!(isoch_interval(3, 4), 3);
    assert_eq!(intervals_per_sec(3, 4), 1000);
    assert_eq!(isoch_ep_words(3, &m)[1], 5 << 3 | 192 << 16);
    // Full speed bInterval 1 = 1 frame = 2^3 microframes.
    assert_eq!(isoch_interval(SPEED_FULL, 1), 3);
    // An OUT TD with IOC at a given frame: 100 bytes on a high-speed
    // endpoint with two 64-byte transactions per microframe (TLBPC 1).
    let t = isoch_trb(0x2000, 100, 64, 1, false, true, Some(0x123));
    assert_eq!(t[3], 5 << 10 | 0x123 << 20 | TRB_IOC | 1 << 16);
    // SuperSpeed: 2 bursts of 4 packets (burst 3), the last of 2.
    let t = isoch_trb(0, 6 * 1024, 1024, 3, true, false, None);
    assert_eq!((t[3] >> 7) & 3, 1);
    assert_eq!((t[3] >> 16) & 0xf, 1);
}

#[cfg(test)]
struct Collect(alloc::sync::Arc<spin::Mutex<Vec<(usize, bool)>>>);

#[cfg(test)]
impl IsochSink for Collect {
    fn packet(&mut self, data: &[u8], err: bool) {
        self.0.lock().push((data.len(), err));
    }
}

#[test_case]
fn isoch_completion_order() {
    // Eight TDs posted, IOC on every fourth: an event for the 3rd (short)
    // completes the first two in full; an event for the 8th (IOC) the
    // 4th to 7th, the 5th having been missed (sentinel byte still 0).
    let got = alloc::sync::Arc::new(spin::Mutex::new(Vec::new()));
    let desc = IsochDesc { addr: 0x81, max_packet: 512, interval: 1, ss_burst: 0, ss_mult: 0 };
    let mut e = IsochEp {
        slot: 1,
        dci: 3,
        desc,
        label: "test".into(),
        ring: IsoRing::new(1).unwrap(),
        chunks: alloc::vec![pci::dma_alloc(16).unwrap(); 2],
        slot_bytes: 512,
        td_len: 512,
        mps: 512,
        burst: 0,
        ioc_every: 4,
        sentinel: true,
        target: 8,
        posted: VecDeque::new(),
        posted_count: 0,
        active: true,
        sink: Some(Box::new(Collect(got.clone()))),
        stats: IsochStats::default(),
        shown: (0, 0, 0),
    };
    e.refill();
    assert_eq!(e.posted.len(), 8);
    // Each TD's buffer gets a header byte, except the 5th (pos 4).
    for pos in [0usize, 1, 2, 3, 5, 6, 7] {
        unsafe { *e.buf(pos).1 = 2 };
    }
    let ring_phys = e.ring.phys;
    let ev = |pos: usize, code: u32, residual: u32| {
        let a = ring_phys + pos as u64 * 16;
        [a as u32, (a >> 32) as u32, code << 24 | residual, 32 << 10 | 3 << 16 | 1 << 24]
    };
    let (e2, e7) = (ev(2, CC_SHORT_PACKET, 500), ev(7, CC_SUCCESS, 0));
    e.active = false; // nothing re-posted
    e.complete(&e2);
    e.complete(&e7);
    assert_eq!(*got.lock(), alloc::vec![(512, false), (512, false), (12, false), (512, false), (0, false), (512, false), (512, false), (512, false)]);
    assert_eq!(e.stats.empty, 1);
    // A missed service event names a TD: delivered empty with err.
    e.active = true;
    e.refill();
    let first = e.posted[0];
    e.complete(&ev(first, CC_MISSED_SERVICE, 0));
    assert_eq!(got.lock().last(), Some(&(0, true)));
    assert_eq!(e.stats.missed, 1);
}
