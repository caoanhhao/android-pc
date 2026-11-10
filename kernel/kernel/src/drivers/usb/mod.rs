//! USB: the xHCI host controller (PCI class 0c.03.30), polled; HID
//! boot-protocol keyboards and mice ([`hid`]), which become evdev input
//! devices; hubs ([`hub`]); and mass storage ([`storage`]: Bulk-Only
//! Transport with SCSI commands), which becomes sd disks.
//!
//! Bring-up: BIOS hand-off (USB legacy support capability), halt and
//! reset, device context base array (with scratchpad buffers if the
//! controller wants them), a command ring and one event ring segment on
//! interrupter 0 (interrupts stay off: [`tick`] polls the event ring),
//! run. Root ports are powered when the controller has port power
//! control, and USB3 links are given up to a second to train. Then each
//! connected root port is reset (USB2; USB3 ports enable themselves) and
//! its device enumerated: Enable Slot, Address Device (route string, and
//! the transaction translator of a high-speed hub for low/full-speed
//! devices behind one), the device and configuration descriptors and
//! strings over EP0 control transfers, SET_CONFIGURATION, then by class:
//! a hub's ports are powered, reset and enumerated in turn (five tiers
//! deep at most); a mass storage interface (SCSI, Bulk-Only) gets its two
//! bulk endpoints configured and is probed as a disk; every HID interface
//! of the boot subclass gets SET_PROTOCOL(boot) + SET_IDLE, Configure
//! Endpoint for its interrupt IN endpoint, and a ring of Normal TRBs kept
//! posted. Devices present at boot only (no hot-plug). Failures are
//! logged and the device skipped, never fatal.
//!
//! Each returned interrupt transfer is a boot report, decoded by [`hid`]
//! into events of an input device named like Linux's ("QEMU QEMU USB
//! Keyboard", bus 0x03, the device's vendor/product), below
//! `.../0000:00:0d.0/usb1/1-1/1-1:1.0/0003:VVVV:PPPP.000N/input/inputM`
//! (`1-1.2` for port 2 of a hub on root port 1). The timer tick notices a
//! new event TRB (as virtio-input does with its used ring) and the soft
//! interrupt ([`softirq`]) delivers it. Disk I/O runs synchronously on
//! the caller with the controller locked, dispatching the HID events it
//! meets meanwhile.

pub mod hid;
pub mod hub;
pub mod storage;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use spin::Mutex;

use super::input::{self, InputDev};
use super::pci::{self, CMD_BUS_MASTER, CMD_INTX_DISABLE, CMD_MEMORY, mmio_read32, mmio_write32, mmio_write64, wait_until};

// Capability registers.
const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;
const HCC_AC64: u32 = 1 << 0;
const HCC_PPC: u32 = 1 << 3;
// Operational registers.
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC: u64 = 0x400;
// Interrupter 0 (runtime registers).
const IR0_IMAN: u64 = 0x20;
const IR0_ERSTSZ: u64 = 0x28;
const IR0_ERSTBA: u64 = 0x30;
const IR0_ERDP: u64 = 0x38;

const CMD_RS: u32 = 1;
const CMD_HCRST: u32 = 2;
const STS_HCH: u32 = 1;
const STS_CNR: u32 = 1 << 11;

const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
const PORT_PRC: u32 = 1 << 21;
/// Port link state "Polling" (a USB3 link still training).
const PLS_POLLING: u32 = 7;
/// PORTSC bits written back unchanged (read-only and read/write-sticky
/// ones; the change bits are write-1-to-clear and PED write-1-to-disable).
const PORT_KEEP: u32 = 0x0e00_0000 | 0x4000_0000 | 0xc000 | 0x3c00 | 0x200 | 0x1e0 | 0x8 | 0x1;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_STOP_EP: u32 = 15;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;

const TRB_ISP: u32 = 1 << 2;
const TRB_CH: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;

const CC_SUCCESS: u32 = 1;
const CC_STALL: u32 = 6;
const CC_SHORT_PACKET: u32 = 13;

// xHCI port speeds (PORTSC, slot context).
const SPEED_FULL: u32 = 1;
const SPEED_LOW: u32 = 2;
const SPEED_HIGH: u32 = 3;
const SPEED_SUPER: u32 = 4;

/// TRBs per ring page; the last one is the link back.
const RING_TRBS: usize = 256;
/// Interrupt transfers kept posted per endpoint.
const POSTED: usize = 16;
/// Bytes of report buffer per ring slot.
const REPORT_BUF: usize = 64;
const TIMEOUT_MS: u64 = 1000;
const MAX_CTRLS: usize = 8;

static NEXT_BUS: AtomicU32 = AtomicU32::new(1);
static NEXT_HID: AtomicU32 = AtomicU32::new(1);

fn trb_type(t: &[u32; 4]) -> u32 {
    (t[3] >> 10) & 0x3f
}

fn trb_ptr(t: &[u32; 4]) -> u64 {
    t[0] as u64 | (t[1] as u64) << 32
}

/// A producer ring (command or transfer) of one page.
struct Ring {
    phys: u64,
    ptr: *mut u32,
    idx: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Option<Ring> {
        let (phys, ptr) = pci::dma_alloc(1)?;
        let ptr = ptr as *mut u32;
        unsafe {
            let link = ptr.add((RING_TRBS - 1) * 4);
            *link = phys as u32;
            *link.add(1) = (phys >> 32) as u32;
            *link.add(3) = TRB_LINK << 10 | 2; // toggle cycle
        }
        Some(Ring { phys, ptr, idx: 0, cycle: 1 })
    }

    /// Enqueue a TRB (its cycle bit is filled in); returns its address.
    fn push(&mut self, t: [u32; 4]) -> u64 {
        let addr = self.phys + self.idx as u64 * 16;
        unsafe {
            let p = self.ptr.add(self.idx * 4);
            for i in 0..3 {
                core::ptr::write_volatile(p.add(i), t[i]);
            }
            fence(Ordering::SeqCst);
            core::ptr::write_volatile(p.add(3), (t[3] & !1) | self.cycle);
        }
        self.idx += 1;
        if self.idx == RING_TRBS - 1 {
            // The link carries the chain bit of a TD that wraps.
            unsafe {
                let link = self.ptr.add(self.idx * 4 + 3);
                core::ptr::write_volatile(link, TRB_LINK << 10 | 2 | (t[3] & TRB_CH) | self.cycle);
            }
            self.idx = 0;
            self.cycle ^= 1;
        }
        addr
    }

    /// Where the next TRB goes, with the cycle state (Set TR Dequeue).
    fn enqueue_ptr(&self) -> u64 {
        self.phys + self.idx as u64 * 16 | self.cycle as u64
    }
}

/// The event ring (one segment of one page).
struct EventRing {
    phys: u64,
    ptr: *mut u32,
    idx: usize,
    cycle: u32,
}

impl EventRing {
    fn peek_addr(&self) -> u64 {
        self.ptr as u64 + self.idx as u64 * 16
    }

    fn pop(&mut self) -> Option<[u32; 4]> {
        let p = unsafe { self.ptr.add(self.idx * 4) };
        let dw3 = unsafe { core::ptr::read_volatile(p.add(3)) };
        if dw3 & 1 != self.cycle {
            return None;
        }
        fence(Ordering::SeqCst);
        let t = unsafe { [core::ptr::read_volatile(p), core::ptr::read_volatile(p.add(1)), core::ptr::read_volatile(p.add(2)), dw3] };
        self.idx += 1;
        if self.idx == RING_TRBS {
            self.idx = 0;
            self.cycle ^= 1;
        }
        Some(t)
    }
}

/// One HID interface's interrupt IN endpoint.
struct HidEp {
    slot: u32,
    dci: u32,
    ring: Ring,
    /// REPORT_BUF bytes per ring slot.
    bufs: (u64, *mut u8),
    mps: u32,
    kind: hid::Kind,
    prev: [u8; 8],
    dev: Arc<InputDev>,
    /// Reports logged so far (the first few show what the device sends).
    shown: u8,
}

impl HidEp {
    /// Post a Normal TRB for the next ring slot.
    fn post(&mut self) {
        let buf = self.bufs.0 + (self.ring.idx * REPORT_BUF) as u64;
        self.ring.push([buf as u32, (buf >> 32) as u32, self.mps, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP]);
    }

    /// A transfer event for this endpoint: decode the report, re-post.
    fn complete(&mut self, ev: &[u32; 4]) {
        let code = ev[2] >> 24;
        let trb = trb_ptr(ev);
        if code == CC_SUCCESS || code == CC_SHORT_PACKET {
            let slot = (trb.wrapping_sub(self.ring.phys) / 16) as usize;
            let len = self.mps.saturating_sub(ev[2] & 0xff_ffff) as usize;
            if slot < RING_TRBS {
                let r = unsafe { core::slice::from_raw_parts(self.bufs.1.add(slot * REPORT_BUF), len.min(REPORT_BUF)) };
                if self.shown < 3 {
                    self.shown += 1;
                    log::info!("xhci: slot {} DCI {} {:?} report {:02x?}", self.slot, self.dci, self.kind, r);
                }
                let dev = &self.dev;
                let mut pause = false;
                let mut emit = |t: u16, c: u16, v: i32| {
                    pause |= t == input::EV_KEY && c == hid::KEY_PAUSE && v == 1;
                    dev.event(t, c, v)
                };
                let mut sent = false;
                match self.kind {
                    hid::Kind::Keyboard => {
                        if hid::keyboard(&self.prev, r, &mut emit) {
                            self.prev = [0; 8];
                            let n = r.len().min(8);
                            self.prev[..n].copy_from_slice(&r[..n]);
                            sent = true;
                        }
                    }
                    hid::Kind::Mouse => {
                        self.prev[0] = hid::mouse(self.prev[0], r, &mut emit);
                        sent = true;
                    }
                }
                if sent {
                    dev.event(input::EV_SYN, input::SYN_REPORT, 0);
                }
                // Pause/Break shows or hides the kernel log on screen.
                if pause {
                    crate::drivers::fbcon::toggle_overlay();
                }
            }
        } else {
            log::warn!("xhci: slot {} endpoint {}: transfer error {code}", self.slot, self.dci);
        }
        self.post();
    }
}

struct Xhci {
    index: usize,
    op: u64,
    rt: u64,
    db: u64,
    /// Context size (32 or 64 bytes).
    csz: usize,
    ports: u32,
    dcbaa: *mut u64,
    cmd: Ring,
    ev: EventRing,
    /// Control transfer data (one page).
    data: (u64, *mut u8),
    hids: Vec<HidEp>,
    /// Mass storage devices; taken out while a command runs on one.
    disks: Vec<Option<alloc::boxed::Box<storage::Msd>>>,
    /// Disks found during bring-up, registered once the controller is
    /// reachable through [`CTRLS`].
    new_disks: Vec<storage::NewDisk>,
    pci_dir: String,
    bus: u32,
    /// USB major revision of each root port (index = port number), from
    /// the Supported Protocol capabilities; 0 if unlisted.
    proto: Vec<u8>,
}

unsafe impl Send for Xhci {}

/// Where a device hangs: its root port, route string and hub tier, and the
/// transaction translator (hub slot, hub port) serving it if it is a
/// low/full-speed device behind a high-speed hub.
#[derive(Clone)]
struct Topo {
    port: u32,
    route: u32,
    depth: u32,
    tt: Option<(u32, u32)>,
    /// "3" or "3.1.2": the port path in sysfs names (1-3.1.2).
    path: String,
}

/// An addressed device.
struct UsbDev {
    slot: u32,
    speed: u32,
    topo: Topo,
    input: (u64, *mut u8),
    ep0: Ring,
    mps0: u32,
    /// Ports and think time (TTT) if the device is a hub.
    hub: Option<(u32, u32)>,
}

unsafe impl Send for UsbDev {}

/// Endpoint context index of an endpoint address (DCI).
fn dci_of(addr: u8) -> u32 {
    (addr as u32 & 0xf) * 2 + (addr as u32 >> 7)
}

/// The Interval field for an interrupt endpoint: 2^Interval * 125 us.
fn ep_interval(speed: u32, b_interval: u8) -> u32 {
    match speed {
        // Full/low speed: bInterval in frames (1 ms).
        SPEED_FULL | SPEED_LOW => {
            let micro = (b_interval.max(1) as u32) * 8;
            (31 - micro.leading_zeros()).clamp(3, 10)
        }
        // High/super speed: 2^(bInterval - 1) microframes.
        _ => (b_interval.clamp(1, 16) - 1) as u32,
    }
}

fn ok(b: bool) -> &'static str {
    if b { "ok" } else { "failed" }
}

fn speed_name(speed: u32) -> &'static str {
    match speed {
        SPEED_FULL => "full",
        SPEED_LOW => "low",
        SPEED_HIGH => "high",
        SPEED_SUPER => "super",
        5.. => "super+",
        _ => "unknown",
    }
}

/// Poll `snap` until it has not changed for `stable` ms, after at least
/// `min` and at most `max` ms; whether it settled. Real devices connect
/// some time after their port gets power (the controller reset may have
/// cut it), so ports are only looked at once their state stops moving.
fn settle<T: PartialEq>(min: u64, max: u64, stable: u64, mut snap: impl FnMut() -> T) -> bool {
    let start = crate::time::monotonic_ns();
    let ms = |t: u64| (crate::time::monotonic_ns() - t) / 1_000_000;
    let mut last = snap();
    let mut since = crate::time::monotonic_ns();
    loop {
        wait_until(10, || false);
        let now = snap();
        if now != last {
            last = now;
            since = crate::time::monotonic_ns();
        }
        if ms(start) >= min && ms(since) >= stable {
            return true;
        }
        if ms(start) >= max {
            return false;
        }
    }
}

/// Slot context dwords 0-2: route string, speed, hub flag, context
/// entries; root port, hub port count; TT hub slot and port (low/full
/// speed behind a high-speed hub), TT think time (hubs).
fn slot_words(t: &Topo, speed: u32, hub: Option<(u32, u32)>, entries: u32) -> [u32; 3] {
    let (is_hub, ports, ttt) = match hub {
        Some((n, ttt)) => (1, n, ttt),
        None => (0, 0, 0),
    };
    let (tt_slot, tt_port) = t.tt.unwrap_or((0, 0));
    [
        t.route & 0xf_ffff | speed << 20 | is_hub << 26 | entries << 27,
        t.port << 16 | ports << 24,
        tt_slot | tt_port << 8 | ttt << 16,
    ]
}

/// The boot-protocol keyboard and mouse interfaces of a configuration
/// (every one, whatever its number: a receiver often has a keyboard on
/// interface 0 and its mouse on 2) with their interrupt IN endpoint
/// (address, max packet size, bInterval).
fn boot_hid_interfaces(ifaces: &[Iface]) -> Vec<(u8, hid::Kind, (u8, u16, u8))> {
    let mut found = Vec::new();
    for f in ifaces.iter().filter(|f| f.alt == 0 && f.class.0 == 3 && f.class.1 == 1) {
        let kind = match f.class.2 {
            1 => hid::Kind::Keyboard,
            2 => hid::Kind::Mouse,
            _ => continue,
        };
        if let Some(&(addr, _, mps, interval, _)) = f.eps.iter().find(|e| e.0 & 0x80 != 0 && e.1 & 3 == 3) {
            found.push((f.num, kind, (addr, mps, interval)));
        }
    }
    found
}

/// An interface of the active configuration and its endpoints
/// (address, attributes, max packet size, interval, SuperSpeed burst).
struct Iface {
    num: u8,
    alt: u8,
    class: (u8, u8, u8),
    eps: Vec<(u8, u8, u16, u8, u8)>,
}

/// The interfaces listed in a configuration descriptor.
fn parse_config(cfg: &[u8]) -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    let mut i = 0;
    while i + 2 <= cfg.len() {
        let (len, ty) = (cfg[i] as usize, cfg[i + 1]);
        if len < 2 || i + len > cfg.len() {
            break;
        }
        let d = &cfg[i..i + len];
        match ty {
            4 if len >= 9 => out.push(Iface { num: d[2], alt: d[3], class: (d[5], d[6], d[7]), eps: Vec::new() }),
            5 if len >= 7 => {
                if let Some(f) = out.last_mut() {
                    f.eps.push((d[2], d[3], u16::from_le_bytes([d[4], d[5]]) & 0x7ff, d[6], 0));
                }
            }
            // SuperSpeed endpoint companion: bMaxBurst.
            0x30 if len >= 6 => {
                if let Some(e) = out.last_mut().and_then(|f| f.eps.last_mut()) {
                    e.4 = d[2].min(15);
                }
            }
            _ => {}
        }
        i += len;
    }
    out
}

impl Xhci {
    fn ring_doorbell(&self, slot: u32, target: u32) {
        fence(Ordering::SeqCst);
        mmio_write32(self.db, slot as u64 * 4, target);
    }

    fn update_erdp(&self) {
        mmio_write64(self.rt, IR0_ERDP, (self.ev.phys + self.ev.idx as u64 * 16) | 8);
        NEXT_EVENT[self.index].store(self.ev.peek_addr() | self.ev.cycle as u64, Ordering::Release);
    }

    /// Handle one event that nobody is waiting for.
    fn dispatch(&mut self, ev: &[u32; 4]) {
        if trb_type(ev) == TRB_TRANSFER_EVENT {
            let (slot, dci) = (ev[3] >> 24, (ev[3] >> 16) & 0x1f);
            if let Some(h) = self.hids.iter_mut().find(|h| h.slot == slot && h.dci == dci) {
                h.complete(ev);
            }
        }
    }

    /// Poll the event ring for up to `ms` until `want` matches an event
    /// (others are dispatched).
    fn wait_event(&mut self, ms: u64, mut want: impl FnMut(&[u32; 4]) -> bool) -> Option<[u32; 4]> {
        let mut found = None;
        wait_until(ms, || {
            while let Some(ev) = self.ev.pop() {
                if want(&ev) {
                    found = Some(ev);
                    break;
                }
                self.dispatch(&ev);
            }
            found.is_some()
        });
        self.update_erdp();
        found
    }

    /// Run a command; returns the completion event if it succeeded.
    fn command(&mut self, t: [u32; 4]) -> Option<[u32; 4]> {
        let r = self.command_code(t);
        match r {
            Ok(e) => Some(e),
            Err(0) => {
                log::warn!("xhci: command {} timed out", trb_type(&t));
                None
            }
            Err(code) => {
                log::warn!("xhci: command {} failed: completion code {code}", trb_type(&t));
                None
            }
        }
    }

    /// Run a command: the completion event, or its completion code (0 for
    /// a timeout).
    fn command_code(&mut self, t: [u32; 4]) -> Result<[u32; 4], u32> {
        let addr = self.cmd.push(t);
        self.ring_doorbell(0, 0);
        let ev = self.wait_event(TIMEOUT_MS, |e| trb_type(e) == TRB_COMMAND_COMPLETION && trb_ptr(e) == addr);
        match ev {
            Some(e) if e[2] >> 24 == CC_SUCCESS => Ok(e),
            Some(e) => Err(e[2] >> 24),
            None => Err(0),
        }
    }

    fn ctx(&self, base: *mut u8, i: usize) -> *mut u32 {
        unsafe { base.add(i * self.csz) as *mut u32 }
    }

    /// Clear the input context and set its add flags.
    fn input_ctx(&self, d: &UsbDev, add: u32) {
        unsafe { core::ptr::write_bytes(d.input.1, 0, 4096) };
        unsafe { *self.ctx(d.input.1, 0).add(1) = add };
    }

    fn slot_ctx(&self, d: &UsbDev, entries: u32) {
        let s = self.ctx(d.input.1, 1);
        let w = slot_words(&d.topo, d.speed, d.hub, entries);
        unsafe {
            for (i, v) in w.iter().enumerate() {
                *s.add(i) = *v;
            }
        }
    }

    fn ep0_ctx(&self, d: &UsbDev) {
        let e = self.ctx(d.input.1, 2);
        let ring = d.ep0.enqueue_ptr();
        unsafe {
            *e.add(1) = 3 << 1 | 4 << 3 | d.mps0 << 16;
            *e.add(2) = ring as u32;
            *e.add(3) = (ring >> 32) as u32;
            *e.add(4) = 8;
        }
    }

    /// A control transfer on EP0; IN data lands in `self.data`, OUT data
    /// is taken from it.
    fn control(&mut self, d: &mut UsbDev, req_type: u8, req: u8, value: u16, index: u16, len: u16) -> bool {
        self.control_quiet(d, req_type, req, value, index, len, false)
    }

    fn control_quiet(&mut self, d: &mut UsbDev, req_type: u8, req: u8, value: u16, index: u16, len: u16, quiet: bool) -> bool {
        let input = req_type & 0x80 != 0;
        let trt = if len == 0 { 0 } else if input { 3 } else { 2 };
        d.ep0.push([
            req_type as u32 | (req as u32) << 8 | (value as u32) << 16,
            index as u32 | (len as u32) << 16,
            8,
            TRB_SETUP << 10 | TRB_IDT | trt << 16,
        ]);
        if len > 0 {
            d.ep0.push([self.data.0 as u32, (self.data.0 >> 32) as u32, len as u32, TRB_DATA << 10 | (input as u32) << 16]);
        }
        let status_in = len == 0 || !input;
        let last = d.ep0.push([0, 0, 0, TRB_STATUS << 10 | TRB_IOC | (status_in as u32) << 16]);
        self.ring_doorbell(d.slot, 1);
        let slot = d.slot;
        // The status stage's event, or an earlier one with an error.
        let ev = self.wait_event(TIMEOUT_MS * 2, |e| {
            trb_type(e) == TRB_TRANSFER_EVENT
                && e[3] >> 24 == slot
                && (e[3] >> 16) & 0x1f == 1
                && (trb_ptr(e) == last || !matches!(e[2] >> 24, CC_SUCCESS | CC_SHORT_PACKET))
        });
        let code = ev.map(|e| e[2] >> 24);
        match code {
            Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => true,
            _ => {
                if !quiet {
                    log::warn!("xhci: slot {slot}: control request {req:#x} failed ({code:?})");
                }
                // Clear the halt (or stop a timed-out request) and skip
                // what is left of the TD.
                self.recover_ep(slot, 1, d.ep0.enqueue_ptr(), code.is_none());
                false
            }
        }
    }

    /// Put an endpoint that halted (or whose transfer timed out) back in
    /// service with its ring's dequeue pointer past everything posted.
    fn recover_ep(&mut self, slot: u32, dci: u32, enqueue: u64, stop: bool) {
        if stop {
            let _ = self.command_code([0, 0, 0, TRB_STOP_EP << 10 | dci << 16 | slot << 24]);
        }
        // Fails with a context state error unless the endpoint halted.
        let _ = self.command_code([0, 0, 0, TRB_RESET_EP << 10 | dci << 16 | slot << 24]);
        let p = enqueue;
        if let Err(code) = self.command_code([p as u32, (p >> 32) as u32, 0, TRB_SET_TR_DEQUEUE << 10 | dci << 16 | slot << 24]) {
            log::warn!("xhci: slot {slot} endpoint {dci}: cannot reset the ring (code {code})");
        }
    }

    fn get_descriptor(&mut self, d: &mut UsbDev, ty: u8, idx: u8, lang: u16, len: u16) -> Option<Vec<u8>> {
        if !self.control(d, 0x80, 6, (ty as u16) << 8 | idx as u16, lang, len) {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(self.data.1, len as usize) }.to_vec())
    }

    fn string(&mut self, d: &mut UsbDev, idx: u8) -> String {
        if idx == 0 {
            return String::new();
        }
        let Some(s) = self.get_descriptor(d, 3, idx, 0x0409, 255) else { return String::new() };
        let n = (s[0] as usize).min(s.len());
        let units: Vec<u16> = s[2.min(n)..n].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    }

    fn portsc(&self, port: u32) -> u32 {
        mmio_read32(self.op, OP_PORTSC + 0x10 * (port as u64 - 1))
    }

    /// Reset (if needed) and enumerate the device on root port `port`.
    fn attach_root(&mut self, port: u32) {
        let reg = OP_PORTSC + 0x10 * (port as u64 - 1);
        let sc0 = mmio_read32(self.op, reg);
        if sc0 & PORT_CCS == 0 {
            return;
        }
        let usb = self.proto.get(port as usize).copied().unwrap_or(0);
        log::info!(
            "xhci: usb{} port {port} (USB {}): connected, PORTSC {sc0:#010x}, {}",
            self.bus,
            if usb == 0 { "?".into() } else { format!("{usb}") },
            if sc0 & PORT_PED != 0 { "enabled" } else { "to reset" }
        );
        for attempt in 0..3 {
            // USB2 ports need a reset to enable; a USB3 port enables
            // itself, but a retry resets it too (warm-reset like).
            let sc = mmio_read32(self.op, reg);
            if sc & PORT_PED == 0 || attempt > 0 {
                mmio_write32(self.op, reg, (sc & PORT_KEEP) | PORT_PR);
                let op = self.op;
                if !wait_until(500, || mmio_read32(op, reg) & PORT_PRC != 0) {
                    log::warn!("xhci: usb{} port {port}: reset timed out ({:#x})", self.bus, mmio_read32(op, reg));
                    continue;
                }
                // Reset recovery (TRSTRCY), more on a retry.
                wait_until(if attempt == 0 { 10 } else { 100 }, || false);
            }
            let sc = mmio_read32(self.op, reg);
            mmio_write32(self.op, reg, (sc & PORT_KEEP) | (sc & 0x00fe_0000));
            if sc & PORT_CCS == 0 {
                log::warn!("xhci: usb{} port {port}: device gone ({sc:#x})", self.bus);
                return;
            }
            if sc & PORT_PED == 0 {
                log::warn!("xhci: usb{} port {port}: not enabled after reset ({sc:#x})", self.bus);
                continue;
            }
            let speed = (sc >> 10) & 0xf;
            let topo = Topo { port, route: 0, depth: 0, tt: None, path: format!("{port}") };
            if self.enumerate(topo, speed) {
                return;
            }
            log::warn!("xhci: usb{} port {port}: enumeration failed (try {}), resetting the port", self.bus, attempt + 1);
        }
    }

    /// Give back a slot whose device could not be set up.
    fn disable_slot(&mut self, slot: u32) {
        let _ = self.command_code([0, 0, 0, TRB_DISABLE_SLOT << 10 | slot << 24]);
        unsafe { *self.dcbaa.add(slot as usize) = 0 };
    }

    /// Address the device at `topo` and set it up by class. False if it
    /// could not be addressed or described (the caller may reset the port
    /// and try again); a device of no supported class still counts as done.
    fn enumerate(&mut self, topo: Topo, speed: u32) -> bool {
        let path = topo.path.clone();
        let name = format!("usb{}-{path}", self.bus);
        let Some(ev) = self.command([0, 0, 0, TRB_ENABLE_SLOT << 10]) else {
            log::warn!("xhci: {name}: no free slot");
            return false;
        };
        let slot = ev[3] >> 24;
        let (Some(out), Some(input), Some(ep0)) = (pci::dma_alloc(1), pci::dma_alloc(1), Ring::new()) else {
            log::warn!("xhci: {name}: out of memory");
            return true;
        };
        unsafe { *self.dcbaa.add(slot as usize) = out.0 };
        let mps0 = match speed {
            SPEED_HIGH => 64,
            SPEED_SUPER.. => 512,
            _ => 8,
        };
        let tt = topo.tt;
        let mut d = UsbDev { slot, speed, topo, input, ep0, mps0, hub: None };
        log::info!(
            "xhci: {name}: {} speed, slot {slot}, route {:#x}, root port {}{}",
            speed_name(speed),
            d.topo.route,
            d.topo.port,
            tt.map_or(String::new(), |(s, p)| format!(", TT hub slot {s} port {p}"))
        );
        self.input_ctx(&d, 0b11);
        self.slot_ctx(&d, 1);
        self.ep0_ctx(&d);
        if let Err(code) = self.command_code([input.0 as u32, (input.0 >> 32) as u32, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24]) {
            log::warn!("xhci: {name}: Address Device failed (completion code {code})");
            self.disable_slot(slot);
            return false;
        }
        // Address recovery (TRSTRCY) before the first request.
        wait_until(10, || false);
        let Some(dd) = self.get_descriptor(&mut d, 1, 0, 0, 8) else {
            log::warn!("xhci: {name}: no device descriptor");
            self.disable_slot(slot);
            return false;
        };
        let mps = if speed >= SPEED_SUPER { 1u32 << dd[7].min(9) } else { dd[7] as u32 };
        if mps != d.mps0 && mps >= 8 {
            log::info!("xhci: {name}: EP0 max packet {mps}");
            d.mps0 = mps;
            self.input_ctx(&d, 0b10);
            let e = self.ctx(d.input.1, 2);
            unsafe { *e.add(1) = 3 << 1 | 4 << 3 | mps << 16 };
            self.command([input.0 as u32, (input.0 >> 32) as u32, 0, TRB_EVALUATE_CTX << 10 | slot << 24]);
        }
        let descs = (|| {
            let dd = self.get_descriptor(&mut d, 1, 0, 0, 18)?;
            let cfg = self.get_descriptor(&mut d, 2, 0, 0, 9)?;
            let total = u16::from_le_bytes([cfg[2], cfg[3]]).clamp(9, 4096);
            let cfg = self.get_descriptor(&mut d, 2, 0, 0, total)?;
            Some((dd, cfg))
        })();
        let Some((dd, cfg)) = descs else {
            log::warn!("xhci: {name}: descriptors unreadable");
            self.disable_slot(slot);
            return false;
        };
        let dev_class = dd[4];
        let vendor = u16::from_le_bytes([dd[8], dd[9]]);
        let product = u16::from_le_bytes([dd[10], dd[11]]);
        let version = u16::from_le_bytes([dd[12], dd[13]]);
        let (i_mfr, i_prod, i_serial) = (dd[14], dd[15], dd[16]);
        let manufacturer = self.string(&mut d, i_mfr);
        let prod_name = self.string(&mut d, i_prod);
        let serial = self.string(&mut d, i_serial);
        log::info!("xhci: {name}: {vendor:04x}:{product:04x} class {dev_class:#04x} \"{manufacturer}\" \"{prod_name}\"");
        let usbdev = format!("{}/usb{}/{}-{path}", self.pci_dir, self.bus, self.bus);
        crate::fs::sysfs::set(&usbdev, "idVendor", &format!("{vendor:04x}\n"));
        crate::fs::sysfs::set(&usbdev, "idProduct", &format!("{product:04x}\n"));
        crate::fs::sysfs::set(&usbdev, "manufacturer", &format!("{manufacturer}\n"));
        crate::fs::sysfs::set(&usbdev, "product", &format!("{prod_name}\n"));
        crate::fs::sysfs::set(&usbdev, "speed", &format!("{}\n", [0, 12, 1, 480, 5000, 10000][speed.min(5) as usize]));
        if !serial.is_empty() {
            crate::fs::sysfs::set(&usbdev, "serial", &format!("{serial}\n"));
        }

        let ifaces = parse_config(&cfg);
        for f in &ifaces {
            let eps: Vec<String> = f.eps.iter().map(|e| format!("{:02x}/{}B/{}", e.0, e.2, e.3)).collect();
            log::info!(
                "xhci: {name}: if{} alt {} class {:02x}/{:02x}/{:02x} endpoints [{}]",
                f.num,
                f.alt,
                f.class.0,
                f.class.1,
                f.class.2,
                eps.join(" ")
            );
        }
        let is_hub = dev_class == 9 || ifaces.iter().any(|f| f.class.0 == 9);
        let msd = ifaces.iter().position(|f| f.alt == 0 && f.class == (8, 6, 0x50));
        let hid_any = ifaces.iter().any(|f| f.alt == 0 && f.class.0 == 3 && f.class.1 == 1 && matches!(f.class.2, 1 | 2));
        if !is_hub && msd.is_none() && !hid_any {
            if ifaces.iter().any(|f| f.class.0 == 8) {
                log::warn!("xhci: {name}: mass storage without SCSI Bulk-Only (UAS-only?), not supported");
            } else {
                log::info!("xhci: {name}: no hub, disk, keyboard or mouse interface, left unconfigured");
            }
            return true;
        }
        if !self.control(&mut d, 0x00, 9, cfg[5] as u16, 0, 0) {
            log::warn!("xhci: {name}: SET_CONFIGURATION {} failed", cfg[5]);
            self.disable_slot(slot);
            return false;
        }
        if is_hub {
            self.hub(d, &usbdev);
        } else if let Some(i) = msd {
            let f = &ifaces[i];
            let name = format!("{manufacturer} {prod_name}");
            self.storage(d, f, &usbdev, name.trim());
        } else {
            let name = match (manufacturer.is_empty(), prod_name.is_empty()) {
                (false, false) => format!("{manufacturer} {prod_name}"),
                (true, false) => prod_name.clone(),
                (false, true) => manufacturer.clone(),
                (true, true) => format!("HID {vendor:04x}:{product:04x}"),
            };
            self.hid(d, &ifaces, &usbdev, &name, [vendor, product, version]);
        }
        true
    }

    /// Set up the boot-protocol keyboards and mice of a configured device.
    fn hid(&mut self, mut d: UsbDev, ifaces: &[Iface], usbdev: &str, name: &str, ids: [u16; 3]) {
        let [vendor, product, version] = ids;
        let slot = d.slot;
        let found = boot_hid_interfaces(ifaces);
        if found.is_empty() {
            log::info!("xhci: {name}: no interrupt endpoint, left unconfigured");
            return;
        }
        let mut add = 1u32;
        let mut max_dci = 1;
        for &(num, kind, (addr, _, _)) in &found {
            let proto = self.control(&mut d, 0x21, 0x0b, 0, num as u16, 0); // SET_PROTOCOL(boot)
            let idle = self.control_quiet(&mut d, 0x21, 0x0a, 0, num as u16, 0, true); // SET_IDLE(0)
            log::info!("xhci: {name}: if{num} {kind:?} endpoint {addr:02x}: SET_PROTOCOL(boot) {}, SET_IDLE {}", ok(proto), ok(idle));
            add |= 1 << dci_of(addr);
            max_dci = max_dci.max(dci_of(addr));
        }
        self.input_ctx(&d, add);
        self.slot_ctx(&d, max_dci);
        let mut eps = Vec::new();
        for &(num, kind, (addr, mps, interval)) in &found {
            let dci = dci_of(addr);
            let (Some(ring), Some(bufs)) = (Ring::new(), pci::dma_alloc(RING_TRBS * REPORT_BUF / 4096)) else { return };
            let mps = (mps as u32).clamp(1, REPORT_BUF as u32);
            let e = self.ctx(d.input.1, dci as usize + 1);
            let tr = ring.enqueue_ptr();
            unsafe {
                *e = ep_interval(d.speed, interval) << 16;
                *e.add(1) = 3 << 1 | 7 << 3 | mps << 16;
                *e.add(2) = tr as u32;
                *e.add(3) = (tr >> 32) as u32;
                *e.add(4) = mps | mps << 16;
            }
            eps.push((num, kind, dci, ring, bufs, mps));
        }
        if let Err(code) = self.command_code([d.input.0 as u32, (d.input.0 >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | slot << 24]) {
            log::warn!("xhci: {name}: Configure Endpoint failed (completion code {code})");
            return;
        }
        let path = &d.topo.path;
        for (num, kind, dci, ring, bufs, mps) in eps {
            let n = NEXT_HID.fetch_add(1, Ordering::Relaxed);
            let parent = format!("{usbdev}/{}-{path}:1.{num}/0003:{vendor:04X}:{product:04X}.{n:04X}", self.bus);
            let mut caps = hid::caps(kind, name);
            caps.id = [0x03, vendor, product, version];
            let slot_name = self.pci_dir.rsplit('/').next().unwrap_or("");
            caps.phys = format!("usb-{slot_name}-{path}/input{num}");
            let dev = match input::register(caps, &parent, None) {
                Ok(dev) => dev,
                Err(e) => {
                    log::warn!("xhci: {name}: {e:?}");
                    continue;
                }
            };
            let mut h = HidEp { slot, dci, ring, bufs, mps, kind, prev: [0; 8], dev, shown: 0 };
            for _ in 0..POSTED {
                h.post();
            }
            self.ring_doorbell(slot, dci);
            log::info!("xhci: {name}: if{num} bound as {kind:?} (slot {slot}, DCI {dci}, {mps}-byte reports)");
            self.hids.push(h);
        }
    }
}

static CTRLS: Mutex<Vec<Xhci>> = Mutex::new(Vec::new());
/// Per controller: virtual address of the next event TRB | its cycle bit.
static NEXT_EVENT: [AtomicU64; MAX_CTRLS] = [const { AtomicU64::new(0) }; MAX_CTRLS];
static COUNT: AtomicU32 = AtomicU32::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);

/// From the timer interrupt (BSP): is there a new event?
pub fn tick() {
    for i in 0..COUNT.load(Ordering::Acquire) as usize {
        let v = NEXT_EVENT[i].load(Ordering::Acquire);
        if v != 0 && unsafe { core::ptr::read_volatile(((v & !1) + 12) as *const u32) } & 1 == (v & 1) as u32 {
            PENDING.store(true, Ordering::Relaxed);
            return;
        }
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// Deliver the reports the controllers returned (task context).
pub fn softirq() {
    let Some(mut ctrls) = CTRLS.try_lock() else { return };
    PENDING.store(false, Ordering::Relaxed);
    for x in ctrls.iter_mut() {
        let mut any = false;
        while let Some(ev) = x.ev.pop() {
            x.dispatch(&ev);
            any = true;
        }
        if any {
            x.update_erdp();
        }
    }
}

/// Bring up every xHCI controller.
pub fn init() {
    for d in pci::devices() {
        if d.class == 0x0c && d.subclass == 0x03 && d.prog_if == 0x30 {
            if let Err(e) = probe(&d) {
                log::warn!("xhci {:?}: {e}", d.addr);
            }
        }
    }
}

fn probe(d: &pci::Device) -> Result<(), &'static str> {
    let index = COUNT.load(Ordering::Acquire) as usize;
    if index >= MAX_CTRLS {
        return Err("too many controllers");
    }
    let (base, _) = pci::map_bar(d, 0).ok_or("no MMIO BAR 0")?;
    d.enable(CMD_MEMORY | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    let caplen = (mmio_read32(base, 0) & 0xff) as u64;
    let hcs1 = mmio_read32(base, CAP_HCSPARAMS1);
    let hcs2 = mmio_read32(base, CAP_HCSPARAMS2);
    let hcc1 = mmio_read32(base, CAP_HCCPARAMS1);
    if caplen == 0 || hcs1 == u32::MAX {
        return Err("registers read as all ones (device absent or powered down)");
    }
    let op = base + caplen;
    let rt = base + (mmio_read32(base, CAP_RTSOFF) & !0x1f) as u64;
    let db = base + (mmio_read32(base, CAP_DBOFF) & !0x3) as u64;
    // BIOS hand-off (USB legacy support extended capability); the
    // Supported Protocol capabilities say which ports are USB2 or USB3.
    let mut proto = alloc::vec![0u8; (hcs1 >> 24) as usize + 1];
    let mut proto_log = Vec::new();
    let mut xecp = ((hcc1 >> 16) << 2) as u64;
    let mut guard = 0;
    while xecp != 0 && guard < 64 {
        let v = mmio_read32(base, xecp);
        if v & 0xff == 2 {
            let major = (v >> 24) as u8;
            let minor = (v >> 16) as u8;
            let ports = mmio_read32(base, xecp + 8);
            let (first, count) = ((ports & 0xff) as usize, ((ports >> 8) & 0xff) as usize);
            for p in first..(first + count).min(proto.len()) {
                proto[p] = major;
            }
            if count > 0 {
                proto_log.push(format!("{}-{} USB {major}.{}", first, first + count - 1, minor >> 4));
            }
        }
        if v & 0xff == 1 {
            mmio_write32(base, xecp, v | 1 << 24);
            if !wait_until(1000, || mmio_read32(base, xecp) & 1 << 16 == 0) {
                log::warn!("xhci: BIOS does not release the controller");
            }
            let ctl = mmio_read32(base, xecp + 4);
            mmio_write32(base, xecp + 4, ctl & 0xffff_0000 & !0x1fff_0000);
        }
        let next = ((v >> 8) & 0xff) as u64;
        xecp = if next == 0 { 0 } else { xecp + (next << 2) };
        guard += 1;
    }
    if !wait_until(1000, || mmio_read32(op, OP_USBSTS) & STS_CNR == 0) {
        return Err("controller not ready");
    }
    mmio_write32(op, OP_USBCMD, mmio_read32(op, OP_USBCMD) & !CMD_RS);
    if !wait_until(100, || mmio_read32(op, OP_USBSTS) & STS_HCH != 0) {
        return Err("controller does not halt");
    }
    mmio_write32(op, OP_USBCMD, CMD_HCRST);
    if !wait_until(1000, || mmio_read32(op, OP_USBCMD) & CMD_HCRST == 0 && mmio_read32(op, OP_USBSTS) & STS_CNR == 0) {
        return Err("reset timed out");
    }
    let max_slots = (hcs1 & 0xff).min(64);
    let ports = (hcs1 >> 24).max(1);
    mmio_write32(op, OP_CONFIG, max_slots);
    let dcbaa = pci::dma_alloc(1).ok_or("out of memory")?;
    if hcc1 & HCC_AC64 == 0 && dcbaa.0 >> 32 != 0 {
        log::warn!("xhci: 32-bit controller with DMA memory above 4 GiB, it may not work");
    }
    let scratch = ((hcs2 >> 27) & 0x1f) | ((hcs2 >> 21) & 0x1f) << 5;
    if scratch > 0 {
        let arr = pci::dma_alloc(1).ok_or("out of memory")?;
        for i in 0..scratch.min(512) as usize {
            let page = pci::dma_alloc(1).ok_or("out of memory")?;
            unsafe { *(arr.1 as *mut u64).add(i) = page.0 };
        }
        unsafe { *(dcbaa.1 as *mut u64) = arr.0 };
    }
    mmio_write64(op, OP_DCBAAP, dcbaa.0);
    let cmd = Ring::new().ok_or("out of memory")?;
    mmio_write64(op, OP_CRCR, cmd.phys | 1);
    let seg = pci::dma_alloc(1).ok_or("out of memory")?;
    let erst = pci::dma_alloc(1).ok_or("out of memory")?;
    unsafe {
        let e = erst.1 as *mut u32;
        *e = seg.0 as u32;
        *e.add(1) = (seg.0 >> 32) as u32;
        *e.add(2) = RING_TRBS as u32;
    }
    mmio_write32(rt, IR0_ERSTSZ, 1);
    mmio_write64(rt, IR0_ERDP, seg.0);
    mmio_write64(rt, IR0_ERSTBA, erst.0);
    mmio_write32(rt, IR0_IMAN, 1); // clear IP, interrupts off
    mmio_write32(op, OP_USBCMD, CMD_RS);
    if !wait_until(100, || mmio_read32(op, OP_USBSTS) & STS_HCH == 0) {
        return Err("controller does not start");
    }
    let bus = NEXT_BUS.fetch_add(1, Ordering::Relaxed);
    let pci_dir = pci::sysfs_dir(d, "xhci_hcd");
    let mut x = Xhci {
        index,
        op,
        rt,
        db,
        csz: if hcc1 & 4 != 0 { 64 } else { 32 },
        proto,
        ports,
        dcbaa: dcbaa.1 as *mut u64,
        cmd,
        ev: EventRing { phys: seg.0, ptr: seg.1 as *mut u32, idx: 0, cycle: 1 },
        data: pci::dma_alloc(1).ok_or("out of memory")?,
        hids: Vec::new(),
        disks: Vec::new(),
        new_disks: Vec::new(),
        pci_dir,
        bus,
    };
    log::info!(
        "xhci: {:?}: usb{bus}, {} ports ({}), {max_slots} slots, {}-byte contexts, port power {}",
        d.addr,
        x.ports,
        proto_log.join(", "),
        x.csz,
        if hcc1 & HCC_PPC != 0 { "switched" } else { "always on" }
    );
    // Power the ports if software controls their power (otherwise they
    // are on), let connections show and USB3 links train.
    if hcc1 & HCC_PPC != 0 {
        for port in 1..=x.ports {
            let sc = x.portsc(port);
            if sc & PORT_PP == 0 {
                mmio_write32(op, OP_PORTSC + 0x10 * (port as u64 - 1), (sc & PORT_KEEP) | PORT_PP);
            }
        }
    }
    // Devices show up some time after power (the reset may have cut it):
    // wait until no port's connection or link state has moved for 300 ms
    // (at least 500 ms, at most 3 s). A USB3 link in training shows
    // Polling with no connection yet (a USB2 port shows Polling with a
    // device waiting for its reset).
    let snap = |x: &Xhci| (1..=x.ports).map(|p| x.portsc(p) & (PORT_CCS | 0xf << 5)).collect::<Vec<u32>>();
    if !settle(500, 3000, 300, || snap(&x)) {
        log::warn!("xhci: usb{bus}: ports still changing after 3 s");
    }
    let training = |x: &Xhci| (1..=x.ports).any(|p| x.portsc(p) & (PORT_CCS | 0xf << 5) == PLS_POLLING << 5);
    if training(&x) {
        log::warn!("xhci: usb{bus}: a USB3 link is still training");
    }
    let connected: Vec<String> = (1..=x.ports).filter(|&p| x.portsc(p) & PORT_CCS != 0).map(|p| format!("{p}")).collect();
    log::info!("xhci: usb{bus}: devices on ports [{}]", connected.join(" "));
    for port in 1..=x.ports {
        x.attach_root(port);
    }
    x.update_erdp();
    let disks = core::mem::take(&mut x.new_disks);
    CTRLS.lock().push(x);
    COUNT.store(index as u32 + 1, Ordering::Release);
    for d in disks {
        storage::register(index, d);
    }
    Ok(())
}

#[test_case]
fn xhci_endpoint_math() {
    assert_eq!(dci_of(0x81), 3);
    assert_eq!(dci_of(0x02), 4);
    // Full speed, 10 ms -> 80 microframes -> 2^6.
    assert_eq!(ep_interval(1, 10), 6);
    assert_eq!(ep_interval(2, 1), 3);
    // High speed bInterval 7 -> 2^6 microframes.
    assert_eq!(ep_interval(3, 7), 6);
}

#[test_case]
fn usb_config_parse() {
    // A mass storage interface (SCSI, Bulk-Only) with two bulk endpoints
    // and SuperSpeed companions.
    let cfg = [
        9, 2, 44, 0, 1, 1, 0, 0x80, 50, //
        9, 4, 0, 0, 2, 8, 6, 0x50, 0, //
        7, 5, 0x81, 2, 0, 4, 0, 6, 0x30, 15, 0, 0, 0, //
        7, 5, 0x02, 2, 0, 4, 0, 6, 0x30, 3, 0, 0, 0,
    ];
    let f = parse_config(&cfg);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].class, (8, 6, 0x50));
    assert_eq!(f[0].eps.len(), 2);
    assert_eq!(f[0].eps[0], (0x81, 2, 1024, 0, 15));
    assert_eq!(f[0].eps[1], (0x02, 2, 1024, 0, 3));
}

/// The configuration descriptors of the real PC's devices (from Linux's
/// sysfs `descriptors`): a low-speed Microdia keyboard on root port 1,
/// and a full-speed 2.4G mouse receiver behind a high-speed hub on root
/// port 11 (1-11.3) with a boot keyboard on if0 and its boot mouse on if2.
#[cfg(test)]
const REAL_KEYBOARD_CFG: [u8; 59] = [
        0x09, 0x02, 0x3b, 0x00, 0x02, 0x01, 0x00, 0xa0, 0x32, 0x09, 0x04, 0x00, 0x00, 0x01, 0x03, 0x01,
        0x01, 0x00, 0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x4e, 0x00, 0x07, 0x05, 0x81, 0x03, 0x08,
        0x00, 0x0a, 0x09, 0x04, 0x01, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x09, 0x21, 0x00, 0x01, 0x00,
        0x01, 0x22, 0x85, 0x00, 0x07, 0x05, 0x82, 0x03, 0x08, 0x00, 0x0a,
];
#[cfg(test)]
const REAL_RECEIVER_CFG: [u8; 84] = [
        0x09, 0x02, 0x54, 0x00, 0x03, 0x01, 0x00, 0xa0, 0x31, 0x09, 0x04, 0x00, 0x00, 0x01, 0x03, 0x01,
        0x01, 0x00, 0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x39, 0x00, 0x07, 0x05, 0x81, 0x03, 0x08,
        0x00, 0x01, 0x09, 0x04, 0x01, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x09, 0x21, 0x11, 0x01, 0x00,
        0x01, 0x22, 0x96, 0x00, 0x07, 0x05, 0x82, 0x03, 0x11, 0x00, 0x01, 0x09, 0x04, 0x02, 0x00, 0x01,
        0x03, 0x01, 0x02, 0x00, 0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x57, 0x00, 0x07, 0x05, 0x83,
        0x03, 0x07, 0x00, 0x01,
];

#[test_case]
fn usb_real_keyboard_binding() {
    let f = parse_config(&REAL_KEYBOARD_CFG);
    assert_eq!(f.len(), 2);
    let b = boot_hid_interfaces(&f);
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].0, b[0].2), (0, (0x81, 8, 10)));
    assert!(matches!(b[0].1, hid::Kind::Keyboard));
    // Low speed, bInterval 10 frames -> 2^6 x 125 us (8 ms).
    assert_eq!(ep_interval(SPEED_LOW, 10), 6);
    // On root port 1: no route, no TT.
    let t = Topo { port: 1, route: 0, depth: 0, tt: None, path: "1".into() };
    assert_eq!(slot_words(&t, SPEED_LOW, None, 3), [SPEED_LOW << 20 | 3 << 27, 1 << 16, 0]);
}

#[test_case]
fn usb_real_receiver_binding() {
    let f = parse_config(&REAL_RECEIVER_CFG);
    assert_eq!(f.len(), 3);
    let b = boot_hid_interfaces(&f);
    // Both boot interfaces, the mouse being interface 2.
    assert_eq!(b.len(), 2);
    assert_eq!((b[0].0, b[0].2), (0, (0x81, 8, 1)));
    assert!(matches!(b[0].1, hid::Kind::Keyboard));
    assert_eq!((b[1].0, b[1].2), (2, (0x83, 7, 1)));
    assert!(matches!(b[1].1, hid::Kind::Mouse));
    // Full speed, bInterval 1 frame -> 2^3 x 125 us.
    assert_eq!(ep_interval(SPEED_FULL, 1), 3);
    // The single-TT high-speed hub on root port 11 (slot 2, 4 ports, TT
    // think time 1), the receiver on its port 3 through its TT.
    let hub = Topo { port: 11, route: 0, depth: 0, tt: None, path: "11".into() };
    assert_eq!(slot_words(&hub, SPEED_HIGH, Some((4, 1)), 1), [SPEED_HIGH << 20 | 1 << 26 | 1 << 27, 11 << 16 | 4 << 24, 1 << 16]);
    let tt = hub::child_tt(SPEED_HIGH, 2, None, 3, SPEED_FULL);
    assert_eq!(tt, Some((2, 3)));
    let rx = Topo { port: 11, route: 3, depth: 1, tt, path: "11.3".into() };
    assert_eq!(slot_words(&rx, SPEED_FULL, None, 7), [3 | SPEED_FULL << 20 | 7 << 27, 11 << 16, 2 | 3 << 8]);
    // A high-speed device behind it needs no TT; a full-speed one behind
    // a full-speed hub keeps the TT above.
    assert_eq!(hub::child_tt(SPEED_HIGH, 2, None, 2, SPEED_HIGH), None);
    assert_eq!(hub::child_tt(SPEED_FULL, 5, Some((2, 3)), 1, SPEED_LOW), Some((2, 3)));
}
