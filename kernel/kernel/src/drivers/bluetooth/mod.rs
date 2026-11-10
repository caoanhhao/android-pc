//! Bluetooth: one HCI controller, handed to Android's Bluetooth HAL as an
//! H4 byte stream on `/dev/hvc5`.
//!
//! The vendor HAL of this AOSP build (Cuttlefish's
//! `android.hardware.bluetooth-service.cuttlefish`, in the
//! `com.google.cf.bt` APEX) opens the serial device that
//! `vendor.ser.bt-uart` names (`/dev/hvc5`), switches it to raw mode,
//! drains it, and speaks H4 over it: a packet-type byte (1 command, 2
//! ACL, 3 SCO, 4 event, 5 ISO) before each HCI packet. On Cuttlefish the
//! other end is rootcanal on the host; here it is a real controller:
//!
//! - a USB one ([`crate::drivers::usb::btusb`]: commands on EP0, events on
//!   the interrupt IN endpoint, ACL on the bulk pair), with Intel's
//!   firmware download done at probe ([`intel`]);
//! - or a virtio-console port carrying H4 already
//!   ([`crate::drivers::virtio::console`]), which is how the QEMU test
//!   connects the host's rootcanal (`tools/run-qemu.sh --aosp-bt-test`).
//!
//! The first controller found becomes the HAL's. Writes to the port are
//! cut into packets ([`H4Parser`]) and sent; packets from the controller
//! queue up for `read` (whole packets, dropped while nobody has the port
//! open, and the oldest dropped past [`RX_MAX_PACKETS`]). Without a
//! controller `/dev/hvc5` stays the silent stand-in of [`super::hvc`]
//! (Android then never sees Bluetooth come up, as before).
//!
//! Logging (rate-limited, also on the framebuffer): the first packets
//! each way, failed commands, and a counters line at most every
//! [`STATUS_SECS`] seconds.

pub mod intel;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::{Mutex, Once};

use crate::errno::*;
use crate::fs::poll::{POLLIN, POLLOUT, POLLRDNORM, POLLWRNORM, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

/// The hvc minor the HAL opens (`vendor.ser.bt-uart=/dev/hvc5`).
pub const HVC_MINOR: u32 = 5;

pub const H4_CMD: u8 = 1;
pub const H4_ACL: u8 = 2;
pub const H4_SCO: u8 = 3;
pub const H4_EVENT: u8 = 4;
pub const H4_ISO: u8 = 5;

/// Packets queued for the HAL at most (a few seconds of LE advertising
/// reports while it is busy).
const RX_MAX_PACKETS: usize = 1024;
/// Packets logged in full at the start, each way.
const SHOW_PACKETS: u64 = 24;
/// Failed commands logged.
const SHOW_FAILURES: u64 = 20;
const STATUS_SECS: u64 = 10;

/// A controller the HAL's packets go to.
pub trait Controller: Send + Sync {
    /// Send one HCI packet (without the H4 type byte).
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()>;
    /// "usb1-14 8087:0033 Intel AX211", for the log.
    fn describe(&self) -> String;
}

static CTRL: Once<Arc<dyn Controller>> = Once::new();

struct Rx {
    /// H4-framed packets for `read`; `off` bytes of the first are read.
    q: VecDeque<Vec<u8>>,
    off: usize,
}

static RX: Mutex<Rx> = Mutex::new(Rx { q: VecDeque::new(), off: 0 });
/// Bytes written by the HAL not yet making a whole packet.
static TX: Mutex<H4Parser> = Mutex::new(H4Parser::new());
static OPEN: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Stats {
    cmds: AtomicU64,
    acl_out: AtomicU64,
    events: AtomicU64,
    acl_in: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicU64,
    other: AtomicU64,
}

static STATS: Stats = Stats {
    cmds: AtomicU64::new(0),
    acl_out: AtomicU64::new(0),
    events: AtomicU64::new(0),
    acl_in: AtomicU64::new(0),
    dropped: AtomicU64::new(0),
    failed: AtomicU64::new(0),
    other: AtomicU64::new(0),
};
static LAST_STATUS: Mutex<(u64, [u64; 7])> = Mutex::new((0, [0; 7]));

fn wq() -> &'static Arc<WaitQueue> {
    static WQ: Once<Arc<WaitQueue>> = Once::new();
    WQ.call_once(WaitQueue::new)
}

/// Make `c` the HAL's controller (the first one wins).
pub fn register(c: Arc<dyn Controller>) {
    let name = c.describe();
    if CTRL.is_completed() {
        log::warn!("bluetooth: {name}: a controller is already in use, ignored");
        return;
    }
    CTRL.call_once(|| c);
    log::info!("bluetooth: {name} is hci0, on /dev/hvc{HVC_MINOR} (H4) for the Bluetooth HAL");
}

pub fn present() -> bool {
    CTRL.is_completed()
}

/// Header and payload length of an H4 packet type, from its header:
/// (header bytes, payload length given the header).
fn header_len(kind: u8) -> Option<usize> {
    match kind {
        H4_CMD => Some(3),
        H4_ACL | H4_ISO => Some(4),
        H4_SCO => Some(3),
        H4_EVENT => Some(2),
        _ => None,
    }
}

fn payload_len(kind: u8, h: &[u8]) -> usize {
    match kind {
        H4_CMD | H4_SCO => h[2] as usize,
        H4_ACL => u16::from_le_bytes([h[2], h[3]]) as usize,
        H4_ISO => (u16::from_le_bytes([h[2], h[3]]) & 0x3fff) as usize,
        _ => h[1] as usize,
    }
}

/// Cuts an H4 byte stream into packets.
pub struct H4Parser {
    kind: u8,
    buf: Vec<u8>,
}

impl H4Parser {
    pub const fn new() -> H4Parser {
        H4Parser { kind: 0, buf: Vec::new() }
    }

    /// Feed bytes; each whole packet goes to `out(kind, packet)`. A bad
    /// type byte is skipped (and counted in the return value).
    pub fn feed(&mut self, mut data: &[u8], mut out: impl FnMut(u8, &[u8])) -> usize {
        let mut bad = 0;
        while !data.is_empty() {
            if self.kind == 0 {
                if header_len(data[0]).is_none() {
                    bad += 1;
                } else {
                    self.kind = data[0];
                }
                data = &data[1..];
                continue;
            }
            let hl = header_len(self.kind).unwrap();
            let want = if self.buf.len() < hl { hl } else { hl + payload_len(self.kind, &self.buf) };
            let n = (want - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.buf.len() == hl && payload_len(self.kind, &self.buf) > 0 {
                continue;
            }
            if self.buf.len() >= hl && self.buf.len() == hl + payload_len(self.kind, &self.buf) {
                out(self.kind, &self.buf);
                self.buf.clear();
                self.kind = 0;
            }
        }
        bad
    }
}

/// Cuts the packets of one USB endpoint's byte stream (events or ACL
/// data, without type bytes) at their header lengths.
pub struct Reassembler {
    kind: u8,
    buf: Vec<u8>,
}

impl Reassembler {
    pub const fn new(kind: u8) -> Reassembler {
        Reassembler { kind, buf: Vec::new() }
    }

    pub fn feed(&mut self, mut data: &[u8], mut out: impl FnMut(&[u8])) {
        let hl = header_len(self.kind).unwrap();
        while !data.is_empty() {
            let want = if self.buf.len() < hl { hl } else { hl + payload_len(self.kind, &self.buf) };
            let n = (want - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.buf.len() >= hl && self.buf.len() == hl + payload_len(self.kind, &self.buf) {
                out(&self.buf);
                self.buf.clear();
            }
        }
    }

    /// Forget a partial packet (after a transfer error).
    pub fn reset(&mut self) {
        self.buf.clear();
    }
}

/// HCI opcode as "OGF/OCF" text, e.g. 0x0c03 -> "0x03/0x003".
fn opcode_name(op: u16) -> String {
    alloc::format!("{:#04x}/{:#05x}", op >> 10, op & 0x3ff)
}

/// A packet from the controller: queue it for the HAL.
pub fn deliver(kind: u8, pkt: &[u8]) {
    let n = match kind {
        H4_EVENT => STATS.events.fetch_add(1, Ordering::Relaxed),
        H4_ACL => STATS.acl_in.fetch_add(1, Ordering::Relaxed),
        _ => STATS.other.fetch_add(1, Ordering::Relaxed),
    };
    if kind == H4_EVENT && n < SHOW_PACKETS {
        log::info!("bluetooth: event {:02x?}", &pkt[..pkt.len().min(24)]);
    }
    // Command Complete with a failure status (the first return byte).
    if kind == H4_EVENT && pkt.len() >= 6 && pkt[0] == 0x0e && pkt[5] != 0
        || kind == H4_EVENT && pkt.len() >= 6 && pkt[0] == 0x0f && pkt[2] != 0
    {
        let (op, status) = if pkt[0] == 0x0e {
            (u16::from_le_bytes([pkt[3], pkt[4]]), pkt[5])
        } else {
            (u16::from_le_bytes([pkt[4], pkt[5]]), pkt[2])
        };
        if STATS.failed.fetch_add(1, Ordering::Relaxed) < SHOW_FAILURES {
            log::warn!("bluetooth: command {} failed, status {status:#04x}", opcode_name(op));
        }
    }
    if OPEN.load(Ordering::Acquire) == 0 {
        STATS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut framed = Vec::with_capacity(pkt.len() + 1);
    framed.push(kind);
    framed.extend_from_slice(pkt);
    // The HAL waits for exactly `04 0e 04 01 03 0c 00` (Reset complete,
    // one command credit) before it calls the stack initialized.
    if framed.len() == 7 && framed[1] == 0x0e && framed[2] == 4 && framed[4..7] == [0x03, 0x0c, 0x00] {
        framed[3] = 1;
    }
    {
        let mut rx = RX.lock();
        while rx.q.len() >= RX_MAX_PACKETS {
            rx.q.pop_front();
            rx.off = 0;
            STATS.dropped.fetch_add(1, Ordering::Relaxed);
        }
        rx.q.push_back(framed);
    }
    wq().wake(POLLIN | POLLRDNORM);
    status();
}

/// The counters line, at most every STATUS_SECS seconds and only when
/// something changed.
fn status() {
    let now = crate::time::monotonic_ns() / 1_000_000_000;
    let Some(mut last) = LAST_STATUS.try_lock() else { return };
    if now < last.0 + STATUS_SECS {
        return;
    }
    let s = &STATS;
    let v = [&s.cmds, &s.acl_out, &s.events, &s.acl_in, &s.dropped, &s.failed, &s.other].map(|a| a.load(Ordering::Relaxed));
    if v == last.1 {
        return;
    }
    *last = (now, v);
    log::info!(
        "bluetooth: hci0: {} commands, {} ACL out, {} events, {} ACL in, {} failed commands, {} dropped (HAL {})",
        v[0],
        v[1],
        v[2],
        v[3],
        v[5],
        v[4],
        if OPEN.load(Ordering::Relaxed) > 0 { "attached" } else { "not attached" }
    );
}

/// `/dev/hvc5` opened (the HAL) or closed.
pub fn opened() {
    OPEN.fetch_add(1, Ordering::AcqRel);
}

pub fn closed() {
    if OPEN.fetch_sub(1, Ordering::AcqRel) == 1 {
        let mut rx = RX.lock();
        rx.q.clear();
        rx.off = 0;
        *TX.lock() = H4Parser::new();
    }
}

pub fn read(nonblock: bool, out: &mut [u8]) -> KResult<usize> {
    if out.is_empty() {
        return Ok(0);
    }
    let mut result = Err(EAGAIN);
    let wake = sched::wait_until(wq().key(), None, WaitMode::Interruptible, || {
        let mut rx = RX.lock();
        let mut n = 0;
        while n < out.len() {
            let off = rx.off;
            let Some(p) = rx.q.front() else { break };
            let k = (p.len() - off).min(out.len() - n);
            out[n..n + k].copy_from_slice(&p[off..off + k]);
            n += k;
            if off + k == p.len() {
                rx.q.pop_front();
                rx.off = 0;
            } else {
                rx.off = off + k;
            }
        }
        if n > 0 {
            result = Ok(n);
            return true;
        }
        nonblock
    });
    match wake {
        Wake::Ready => result,
        _ => Err(ERESTARTSYS),
    }
}

pub fn write(data: &[u8]) -> KResult<usize> {
    let Some(c) = CTRL.get() else { return Ok(data.len()) };
    let mut pkts: Vec<(u8, Vec<u8>)> = Vec::new();
    let bad = TX.lock().feed(data, |k, p| pkts.push((k, p.to_vec())));
    if bad > 0 {
        log::warn!("bluetooth: {bad} bytes of the HAL's stream are not H4, skipped");
    }
    for (kind, p) in pkts {
        let n = match kind {
            H4_CMD => STATS.cmds.fetch_add(1, Ordering::Relaxed),
            H4_ACL => STATS.acl_out.fetch_add(1, Ordering::Relaxed),
            _ => STATS.other.fetch_add(1, Ordering::Relaxed),
        };
        if kind == H4_CMD && n < SHOW_PACKETS {
            log::info!("bluetooth: command {} {:02x?}", opcode_name(u16::from_le_bytes([p[0], p[1]])), &p[2..p.len().min(20)]);
        }
        if let Err(e) = c.send(kind, &p) {
            log::warn!("bluetooth: sending a type {kind} packet failed ({e:?})");
            return Err(EIO);
        }
    }
    status();
    Ok(data.len())
}

pub fn poll() -> u32 {
    let ready = !RX.lock().q.is_empty();
    POLLOUT | POLLWRNORM | if ready { POLLIN | POLLRDNORM } else { 0 }
}

pub fn wait_queue() -> Arc<WaitQueue> {
    wq().clone()
}

#[test_case]
fn bluetooth_h4_parser() {
    let mut p = H4Parser::new();
    let mut got: Vec<(u8, Vec<u8>)> = Vec::new();
    // The HAL writes the type byte and the packet separately; a reset,
    // then an ACL packet split across writes, then a command with no
    // parameters (0 payload) right after.
    p.feed(&[H4_CMD], |k, b| got.push((k, b.to_vec())));
    p.feed(&[0x03, 0x0c, 0x00], |k, b| got.push((k, b.to_vec())));
    p.feed(&[H4_ACL, 0x01, 0x20, 0x03], |k, b| got.push((k, b.to_vec())));
    p.feed(&[0x00, 0xaa, 0xbb], |k, b| got.push((k, b.to_vec())));
    let bad = p.feed(&[0xcc, 0x07, H4_CMD, 0x01, 0x10, 0x00], |k, b| got.push((k, b.to_vec())));
    assert_eq!(bad, 1);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], (H4_CMD, alloc::vec![0x03, 0x0c, 0x00]));
    assert_eq!(got[1], (H4_ACL, alloc::vec![0x01, 0x20, 0x03, 0x00, 0xaa, 0xbb, 0xcc]));
    assert_eq!(got[2], (H4_CMD, alloc::vec![0x01, 0x10, 0x00]));
}

#[test_case]
fn bluetooth_reassembler() {
    // A 70-byte event arriving as 64 + 6 bytes, then two short events in
    // one transfer.
    let mut ev = alloc::vec![0x3eu8, 68];
    ev.extend((0..68).map(|i| i as u8));
    let mut r = Reassembler::new(H4_EVENT);
    let mut got: Vec<Vec<u8>> = Vec::new();
    r.feed(&ev[..64], |p| got.push(p.to_vec()));
    assert!(got.is_empty());
    r.feed(&ev[64..], |p| got.push(p.to_vec()));
    r.feed(&[0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00, 0xff, 0x01, 0x06], |p| got.push(p.to_vec()));
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], ev);
    assert_eq!(got[1], [0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00]);
    assert_eq!(got[2], [0xff, 0x01, 0x06]);
    let mut a = Reassembler::new(H4_ACL);
    let mut acl: Vec<Vec<u8>> = Vec::new();
    a.feed(&[0x01, 0x20, 0x02, 0x00, 0x11], |p| acl.push(p.to_vec()));
    a.feed(&[0x22], |p| acl.push(p.to_vec()));
    assert_eq!(acl, [alloc::vec![0x01, 0x20, 0x02, 0x00, 0x11, 0x22]]);
}
