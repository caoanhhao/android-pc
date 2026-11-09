//! USB: the xHCI host controller (PCI class 0c.03.30), polled, and HID
//! boot-protocol keyboards and mice on its root ports ([`hid`]), which
//! become evdev input devices.
//!
//! Bring-up: BIOS hand-off (USB legacy support capability), halt and
//! reset, device context base array (with scratchpad buffers if the
//! controller wants them), a command ring and one event ring segment on
//! interrupter 0 (interrupts stay off: [`tick`] polls the event ring),
//! run. Then each connected root port is reset (USB2; USB3 ports enable
//! themselves) and its device enumerated: Enable Slot, Address Device,
//! the device and configuration descriptors and strings over EP0 control
//! transfers, SET_CONFIGURATION, and for every HID interface of the boot
//! subclass SET_PROTOCOL(boot) + SET_IDLE, Configure Endpoint for its
//! interrupt IN endpoint, and a ring of Normal TRBs kept posted. Hubs and
//! hot-plug are not supported (devices present at boot on root ports).
//!
//! Each returned interrupt transfer is a boot report, decoded by [`hid`]
//! into events of an input device named like Linux's ("QEMU QEMU USB
//! Keyboard", bus 0x03, the device's vendor/product), below
//! `.../0000:00:0d.0/usb1/1-1/1-1:1.0/0003:VVVV:PPPP.000N/input/inputM`.
//! The timer tick notices a new event TRB (as virtio-input does with its
//! used ring) and the soft interrupt ([`softirq`]) delivers it.

pub mod hid;

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
const PORT_PRC: u32 = 1 << 21;
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
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;

const TRB_IOC: u32 = 1 << 5;
const TRB_ISP: u32 = 1 << 2;
const TRB_IDT: u32 = 1 << 6;

const CC_SUCCESS: u32 = 1;
const CC_STALL: u32 = 6;
const CC_SHORT_PACKET: u32 = 13;

/// TRBs per ring page; the last one is the link back.
const RING_TRBS: usize = 256;
/// Interrupt transfers kept posted per endpoint.
const POSTED: usize = 16;
/// Bytes of report buffer per ring slot.
const REPORT_BUF: usize = 64;
const TIMEOUT_MS: u64 = 1000;
const MAX_CTRLS: usize = 4;

static NEXT_BUS: AtomicU32 = AtomicU32::new(1);
static NEXT_HID: AtomicU32 = AtomicU32::new(1);

fn trb_type(t: &[u32; 4]) -> u32 {
    (t[3] >> 10) & 0x3f
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
            unsafe {
                let link = self.ptr.add(self.idx * 4 + 3);
                core::ptr::write_volatile(link, TRB_LINK << 10 | 2 | self.cycle);
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
        let trb = ev[0] as u64 | (ev[1] as u64) << 32;
        if code == CC_SUCCESS || code == CC_SHORT_PACKET {
            let slot = (trb.wrapping_sub(self.ring.phys) / 16) as usize;
            let len = self.mps.saturating_sub(ev[2] & 0xff_ffff) as usize;
            if slot < RING_TRBS {
                let r = unsafe { core::slice::from_raw_parts(self.bufs.1.add(slot * REPORT_BUF), len.min(REPORT_BUF)) };
                let dev = &self.dev;
                let mut emit = |t: u16, c: u16, v: i32| dev.event(t, c, v);
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
    pci_dir: String,
    bus: u32,
}

unsafe impl Send for Xhci {}

/// A device being set up.
struct UsbDev {
    slot: u32,
    port: u32,
    speed: u32,
    input: (u64, *mut u8),
    ep0: Ring,
    mps0: u32,
}

/// Endpoint context index of an endpoint address (DCI).
fn dci_of(addr: u8) -> u32 {
    (addr as u32 & 0xf) * 2 + (addr as u32 >> 7)
}

/// The Interval field for an interrupt endpoint: 2^Interval * 125 us.
fn ep_interval(speed: u32, b_interval: u8) -> u32 {
    match speed {
        // Full/low speed: bInterval in frames (1 ms).
        1 | 2 => {
            let micro = (b_interval.max(1) as u32) * 8;
            (31 - micro.leading_zeros()).clamp(3, 10)
        }
        // High/super speed: 2^(bInterval - 1) microframes.
        _ => (b_interval.clamp(1, 16) - 1) as u32,
    }
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

    /// Poll the event ring until `want` matches an event (others are
    /// dispatched).
    fn wait_event(&mut self, mut want: impl FnMut(&[u32; 4]) -> bool) -> Option<[u32; 4]> {
        let mut found = None;
        wait_until(TIMEOUT_MS, || {
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
        let addr = self.cmd.push(t);
        self.ring_doorbell(0, 0);
        let ev = self.wait_event(|e| trb_type(e) == TRB_COMMAND_COMPLETION && (e[0] as u64 | (e[1] as u64) << 32) == addr);
        match ev {
            Some(e) if e[2] >> 24 == CC_SUCCESS => Some(e),
            Some(e) => {
                log::warn!("xhci: command {} failed: completion code {}", trb_type(&t), e[2] >> 24);
                None
            }
            None => {
                log::warn!("xhci: command {} timed out", trb_type(&t));
                None
            }
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
        unsafe {
            *s = d.speed << 20 | entries << 27;
            *s.add(1) = d.port << 16;
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

    /// A control transfer on EP0; IN data lands in `self.data`.
    fn control(&mut self, d: &mut UsbDev, req_type: u8, req: u8, value: u16, index: u16, len: u16) -> bool {
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
        d.ep0.push([0, 0, 0, TRB_STATUS << 10 | TRB_IOC | (status_in as u32) << 16]);
        self.ring_doorbell(d.slot, 1);
        let slot = d.slot;
        let ev = self.wait_event(|e| trb_type(e) == TRB_TRANSFER_EVENT && e[3] >> 24 == slot && (e[3] >> 16) & 0x1f == 1);
        let code = ev.map(|e| e[2] >> 24);
        match code {
            Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => true,
            _ => {
                log::warn!("xhci: slot {slot}: control request {req:#x} failed ({code:?})");
                if code == Some(CC_STALL) {
                    // Clear the halt and skip what is left of the TD.
                    self.command([0, 0, 0, TRB_RESET_EP << 10 | 1 << 16 | slot << 24]);
                    let p = d.ep0.enqueue_ptr();
                    self.command([p as u32, (p >> 32) as u32, 0, TRB_SET_TR_DEQUEUE << 10 | 1 << 16 | slot << 24]);
                }
                false
            }
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

    /// Reset (if needed) and enumerate the device on root port `port`.
    fn attach(&mut self, port: u32) {
        let reg = OP_PORTSC + 0x10 * (port as u64 - 1);
        let sc = mmio_read32(self.op, reg);
        if sc & PORT_CCS == 0 {
            return;
        }
        if sc & PORT_PED == 0 {
            mmio_write32(self.op, reg, (sc & PORT_KEEP) | PORT_PR);
            let op = self.op;
            if !wait_until(500, || mmio_read32(op, reg) & PORT_PRC != 0) {
                log::warn!("xhci: port {port}: reset timed out");
                return;
            }
        }
        let sc = mmio_read32(self.op, reg);
        mmio_write32(self.op, reg, (sc & PORT_KEEP) | (sc & 0x00fe_0000));
        if sc & PORT_PED == 0 {
            log::warn!("xhci: port {port}: not enabled after reset ({sc:#x})");
            return;
        }
        let speed = (sc >> 10) & 0xf;
        let Some(ev) = self.command([0, 0, 0, TRB_ENABLE_SLOT << 10]) else { return };
        let slot = ev[3] >> 24;
        let (Some(out), Some(input), Some(ep0)) = (pci::dma_alloc(1), pci::dma_alloc(1), Ring::new()) else { return };
        unsafe { *self.dcbaa.add(slot as usize) = out.0 };
        let mps0 = match speed {
            3 => 64,
            4.. => 512,
            _ => 8,
        };
        let mut d = UsbDev { slot, port, speed, input, ep0, mps0 };
        self.input_ctx(&d, 0b11);
        self.slot_ctx(&d, 1);
        self.ep0_ctx(&d);
        if self.command([input.0 as u32, (input.0 >> 32) as u32, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24]).is_none() {
            return;
        }
        let Some(dd) = self.get_descriptor(&mut d, 1, 0, 0, 8) else { return };
        let mps = if speed >= 4 { 1u32 << dd[7] } else { dd[7] as u32 };
        if mps != d.mps0 && mps >= 8 {
            d.mps0 = mps;
            self.input_ctx(&d, 0b10);
            let e = self.ctx(d.input.1, 2);
            unsafe { *e.add(1) = 3 << 1 | 4 << 3 | mps << 16 };
            self.command([input.0 as u32, (input.0 >> 32) as u32, 0, TRB_EVALUATE_CTX << 10 | slot << 24]);
        }
        let Some(dd) = self.get_descriptor(&mut d, 1, 0, 0, 18) else { return };
        let vendor = u16::from_le_bytes([dd[8], dd[9]]);
        let product = u16::from_le_bytes([dd[10], dd[11]]);
        let version = u16::from_le_bytes([dd[12], dd[13]]);
        let (i_mfr, i_prod) = (dd[14], dd[15]);
        let Some(cfg) = self.get_descriptor(&mut d, 2, 0, 0, 9) else { return };
        let total = u16::from_le_bytes([cfg[2], cfg[3]]).min(4096);
        let Some(cfg) = self.get_descriptor(&mut d, 2, 0, 0, total) else { return };
        let manufacturer = self.string(&mut d, i_mfr);
        let prod_name = self.string(&mut d, i_prod);
        log::info!("xhci: port {port}: slot {slot}, speed {speed}, {vendor:04x}:{product:04x} {manufacturer} {prod_name}");

        // Boot-protocol HID interfaces and their interrupt IN endpoints.
        let mut ifaces: Vec<(u8, hid::Kind, Option<(u8, u16, u8)>)> = Vec::new();
        let mut cur: Option<(u8, Option<hid::Kind>)> = None;
        let mut i = 0;
        while i + 2 <= cfg.len() {
            let (len, ty) = (cfg[i] as usize, cfg[i + 1]);
            if len < 2 || i + len > cfg.len() {
                break;
            }
            let desc = &cfg[i..i + len];
            if ty == 4 && len >= 9 {
                let kind = match (desc[5], desc[6], desc[7]) {
                    (3, 1, 1) => Some(hid::Kind::Keyboard),
                    (3, 1, 2) => Some(hid::Kind::Mouse),
                    _ => None,
                };
                cur = Some((desc[2], kind));
                if let Some(k) = kind {
                    ifaces.push((desc[2], k, None));
                }
            } else if ty == 5 && len >= 7 && desc[2] & 0x80 != 0 && desc[3] & 3 == 3 {
                if let Some((num, Some(_))) = cur
                    && let Some(f) = ifaces.iter_mut().find(|f| f.0 == num && f.2.is_none())
                {
                    f.2 = Some((desc[2], u16::from_le_bytes([desc[4], desc[5]]) & 0x7ff, desc[6]));
                }
            }
            i += len;
        }
        if ifaces.iter().all(|f| f.2.is_none()) {
            log::info!("xhci: port {port}: no boot keyboard or mouse, left unconfigured");
            return;
        }
        if !self.control(&mut d, 0x00, 9, cfg[5] as u16, 0, 0) {
            return;
        }
        let mut add = 1u32;
        let mut max_dci = 1;
        for &(num, _, ep) in &ifaces {
            let Some((addr, _, _)) = ep else { continue };
            self.control(&mut d, 0x21, 0x0b, 0, num as u16, 0); // SET_PROTOCOL(boot)
            self.control(&mut d, 0x21, 0x0a, 0, num as u16, 0); // SET_IDLE(0)
            add |= 1 << dci_of(addr);
            max_dci = max_dci.max(dci_of(addr));
        }
        self.input_ctx(&d, add);
        self.slot_ctx(&d, max_dci);
        let mut eps = Vec::new();
        for &(num, kind, ep) in &ifaces {
            let Some((addr, mps, interval)) = ep else { continue };
            let dci = dci_of(addr);
            let (Some(ring), Some(bufs)) = (Ring::new(), pci::dma_alloc(RING_TRBS * REPORT_BUF / 4096)) else { return };
            let mps = (mps as u32).clamp(1, REPORT_BUF as u32);
            let e = self.ctx(d.input.1, dci as usize + 1);
            let tr = ring.enqueue_ptr();
            unsafe {
                *e = ep_interval(speed, interval) << 16;
                *e.add(1) = 3 << 1 | 7 << 3 | mps << 16;
                *e.add(2) = tr as u32;
                *e.add(3) = (tr >> 32) as u32;
                *e.add(4) = mps | mps << 16;
            }
            eps.push((num, kind, dci, ring, bufs, mps));
        }
        if self.command([input.0 as u32, (input.0 >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | slot << 24]).is_none() {
            return;
        }
        let name = match (manufacturer.is_empty(), prod_name.is_empty()) {
            (false, false) => format!("{manufacturer} {prod_name}"),
            (true, false) => prod_name.clone(),
            (false, true) => manufacturer.clone(),
            (true, true) => format!("HID {vendor:04x}:{product:04x}"),
        };
        let usbdev = format!("{}/usb{}/{}-{port}", self.pci_dir, self.bus, self.bus);
        crate::fs::sysfs::set(&usbdev, "idVendor", &format!("{vendor:04x}\n"));
        crate::fs::sysfs::set(&usbdev, "idProduct", &format!("{product:04x}\n"));
        crate::fs::sysfs::set(&usbdev, "manufacturer", &format!("{manufacturer}\n"));
        crate::fs::sysfs::set(&usbdev, "product", &format!("{prod_name}\n"));
        for (num, kind, dci, ring, bufs, mps) in eps {
            let n = NEXT_HID.fetch_add(1, Ordering::Relaxed);
            let parent = format!("{usbdev}/{}-{port}:1.{num}/0003:{vendor:04X}:{product:04X}.{n:04X}", self.bus);
            let mut caps = hid::caps(kind, &name);
            caps.id = [0x03, vendor, product, version];
            let slot_name = self.pci_dir.rsplit('/').next().unwrap_or("");
            caps.phys = format!("usb-{slot_name}-{port}/input{num}");
            let dev = match input::register(caps, &parent, None) {
                Ok(dev) => dev,
                Err(e) => {
                    log::warn!("xhci: {name}: {e:?}");
                    continue;
                }
            };
            let mut h = HidEp { slot, dci, ring, bufs, mps, kind, prev: [0; 8], dev };
            for _ in 0..POSTED {
                h.post();
            }
            self.ring_doorbell(slot, dci);
            log::info!("xhci: {name}: {kind:?} on slot {slot} endpoint {dci}");
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
    let op = base + caplen;
    let rt = base + (mmio_read32(base, CAP_RTSOFF) & !0x1f) as u64;
    let db = base + (mmio_read32(base, CAP_DBOFF) & !0x3) as u64;
    // BIOS hand-off (USB legacy support extended capability).
    let mut xecp = ((hcc1 >> 16) << 2) as u64;
    let mut guard = 0;
    while xecp != 0 && guard < 64 {
        let v = mmio_read32(base, xecp);
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
    let max_slots = (hcs1 & 0xff).min(32);
    let ports = hcs1 >> 24;
    mmio_write32(op, OP_CONFIG, max_slots);
    let dcbaa = pci::dma_alloc(1).ok_or("out of memory")?;
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
        ports,
        dcbaa: dcbaa.1 as *mut u64,
        cmd,
        ev: EventRing { phys: seg.0, ptr: seg.1 as *mut u32, idx: 0, cycle: 1 },
        data: pci::dma_alloc(1).ok_or("out of memory")?,
        hids: Vec::new(),
        pci_dir,
        bus,
    };
    log::info!("xhci: {:?}: {} ports, {max_slots} slots, {}-byte contexts", d.addr, x.ports, x.csz);
    // Let USB3 links train and connections show before looking.
    wait_until(50, || false);
    for port in 1..=x.ports {
        x.attach(port);
    }
    x.update_erdp();
    CTRLS.lock().push(x);
    COUNT.store(index as u32 + 1, Ordering::Release);
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
