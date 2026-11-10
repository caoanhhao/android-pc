//! USB Bluetooth controllers (class e0/01/01, interface 0): HCI commands
//! as class requests on EP0, events from the interrupt IN endpoint, ACL
//! data on the bulk pair. The isochronous interface (SCO audio) is left
//! alone, so voice calls over Bluetooth do not work; A2DP and LE do.
//!
//! Both IN endpoints keep [`POSTED`] transfers of one packet each queued,
//! like the HID endpoints; their bytes go through a [`Reassembler`] cut at
//! the HCI header lengths, and whole packets to the Bluetooth core
//! ([`crate::drivers::bluetooth::deliver`]). Outgoing commands and ACL
//! packets are synchronous transfers on the caller (the HAL's write).
//!
//! Intel controllers (vendor 8087) are brought to their operational
//! firmware at probe ([`intel::setup`]) with the transfers of this file:
//! in the bootloader, Secure Send commands go over bulk OUT and events
//! may come on bulk IN too. The probe logs each stage, so a photo of the
//! screen tells where it stopped.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{
    CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, CTRLS, Iface, RING_TRBS, Ring, SPEED_SUPER, TIMEOUT_MS, TRB_CONFIGURE_EP, TRB_IOC, TRB_ISP,
    TRB_NORMAL, TRB_TRANSFER_EVENT, UsbDev, Xhci, dci_of, ep_interval, pci, trb_ptr, trb_type, wait_until,
};
use crate::drivers::bluetooth::{self, Controller, H4_ACL, H4_CMD, H4_EVENT, Reassembler, intel};
use crate::errno::*;

/// Transfers kept posted per IN endpoint.
const POSTED: usize = 32;
/// Bytes per posted transfer slot (at least the max packet size).
const SLOT: usize = 1024;
/// Events kept for the setup at most.
const PENDING_MAX: usize = 64;

/// An IN endpoint with transfers kept posted.
pub(super) struct InEp {
    dci: u32,
    ring: Ring,
    bufs: (u64, *mut u8),
    mps: u32,
    outstanding: usize,
}

impl InEp {
    fn refill(&mut self) -> bool {
        let mut added = false;
        while self.outstanding < POSTED {
            let buf = self.bufs.0 + (self.ring.idx * SLOT) as u64;
            self.ring.push([buf as u32, (buf >> 32) as u32, self.mps, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP]);
            self.outstanding += 1;
            added = true;
        }
        added
    }
}

pub(super) struct Bt {
    /// EP0; taken out while a control transfer runs.
    dev: Option<UsbDev>,
    slot: u32,
    intr: InEp,
    bulk_in: InEp,
    out_dci: u32,
    out_ring: Ring,
    /// One page for an outgoing ACL packet or Secure Send command.
    out_buf: (u64, *mut u8),
    events: Reassembler,
    acl: Reassembler,
    /// Events arriving on bulk IN in the bootloader.
    boot_events: Reassembler,
    bootloader: bool,
    /// Probing: events are kept in `pending` for [`intel::setup`].
    setup: bool,
    pending: VecDeque<Vec<u8>>,
    label: String,
    errors: u64,
}

unsafe impl Send for Bt {}

impl Bt {
    pub(super) fn label(&self) -> &str {
        &self.label
    }
}

impl Xhci {
    /// A transfer event on a Bluetooth endpoint, if it is one.
    pub(super) fn bt_dispatch(&mut self, ev: &[u32; 4]) -> bool {
        let (slot, dci) = (ev[3] >> 24, (ev[3] >> 16) & 0x1f);
        let Some(i) = self.bts.iter().position(|b| b.slot == slot && (b.intr.dci == dci || b.bulk_in.dci == dci || b.out_dci == dci)) else {
            return false;
        };
        let b = &mut self.bts[i];
        if dci == b.out_dci {
            // A late OUT completion (its wait timed out): nothing to do.
            return true;
        }
        let is_intr = dci == b.intr.dci;
        let ep = if is_intr { &mut b.intr } else { &mut b.bulk_in };
        let code = ev[2] >> 24;
        let trb = trb_ptr(ev);
        let idx = (trb.wrapping_sub(ep.ring.phys) / 16) as usize;
        let ours = trb >= ep.ring.phys && idx < RING_TRBS - 1;
        if ours {
            ep.outstanding = ep.outstanding.saturating_sub(1);
        }
        if code != CC_SUCCESS && code != CC_SHORT_PACKET {
            self.bt_error(i, dci, code);
            return true;
        }
        let mut pkts: Vec<(u8, Vec<u8>)> = Vec::new();
        if ours {
            let len = ep.mps.saturating_sub(ev[2] & 0xff_ffff) as usize;
            let data = unsafe { core::slice::from_raw_parts(ep.bufs.1.add(idx * SLOT), len.min(SLOT)) };
            if is_intr {
                b.events.feed(data, |p| pkts.push((H4_EVENT, p.to_vec())));
            } else if b.bootloader {
                b.boot_events.feed(data, |p| pkts.push((H4_EVENT, p.to_vec())));
            } else {
                b.acl.feed(data, |p| pkts.push((H4_ACL, p.to_vec())));
            }
        }
        let ep = if is_intr { &mut b.intr } else { &mut b.bulk_in };
        if ep.refill() {
            self.ring_doorbell(slot, dci);
        }
        let b = &mut self.bts[i];
        for (kind, p) in pkts {
            if b.setup {
                if kind == H4_EVENT && b.pending.len() < PENDING_MAX {
                    b.pending.push_back(p);
                }
            } else {
                bluetooth::deliver(kind, &p);
            }
        }
        true
    }

    /// A failed IN transfer: recover a halted endpoint (as for HID) and
    /// queue the transfers afresh.
    fn bt_error(&mut self, i: usize, dci: u32, code: u32) {
        let slot = self.bts[i].slot;
        self.bts[i].errors += 1;
        let errors = self.bts[i].errors;
        let state = self.ep_state(slot, dci);
        if errors <= 5 || errors % 100 == 0 {
            log::warn!(
                "bluetooth: {}: endpoint {dci} transfer error, completion code {code}, state {state} ({errors} errors)",
                self.bts[i].label
            );
        }
        let b = &mut self.bts[i];
        let is_intr = dci == b.intr.dci;
        if is_intr { b.events.reset() } else { b.acl.reset() }
        if state == 2 || state == 4 {
            let enq = if is_intr { b.intr.ring.enqueue_ptr() } else { b.bulk_in.ring.enqueue_ptr() };
            self.recover_ep(slot, dci, enq, false);
            if code == CC_STALL
                && let Some(mut d) = self.bts[i].dev.take()
            {
                self.control_quiet(&mut d, 0x02, 1, 0, (dci / 2) as u16 | 0x80, 0, true);
                self.bts[i].dev = Some(d);
            }
            let b = &mut self.bts[i];
            if is_intr { b.intr.outstanding = 0 } else { b.bulk_in.outstanding = 0 }
        }
        let b = &mut self.bts[i];
        if is_intr { b.intr.refill() } else { b.bulk_in.refill() };
        self.ring_doorbell(slot, dci);
    }

    /// Send an HCI command (opcode, length, parameters) on EP0.
    fn bt_command(&mut self, i: usize, pkt: &[u8]) -> bool {
        if pkt.len() > 4096 {
            return false;
        }
        unsafe { core::ptr::copy_nonoverlapping(pkt.as_ptr(), self.data.1, pkt.len()) };
        let Some(mut d) = self.bts[i].dev.take() else { return false };
        // Class request to the device, bRequest 0 (Bluetooth spec, USB transport).
        let ok = self.control(&mut d, 0x20, 0, 0, 0, pkt.len() as u16);
        self.bts[i].dev = Some(d);
        ok
    }

    /// Send a packet on bulk OUT and wait for it to go.
    fn bt_bulk_out(&mut self, i: usize, pkt: &[u8]) -> bool {
        if pkt.len() > 4096 {
            return false;
        }
        let b = &mut self.bts[i];
        unsafe { core::ptr::copy_nonoverlapping(pkt.as_ptr(), b.out_buf.1, pkt.len()) };
        let a = b.out_buf.0;
        let t = b.out_ring.push([a as u32, (a >> 32) as u32, pkt.len() as u32, TRB_NORMAL << 10 | TRB_IOC]);
        let (slot, dci) = (b.slot, b.out_dci);
        self.ring_doorbell(slot, dci);
        let ev = self.wait_event(TIMEOUT_MS, |e| {
            trb_type(e) == TRB_TRANSFER_EVENT && e[3] >> 24 == slot && (e[3] >> 16) & 0x1f == dci && trb_ptr(e) == t
        });
        match ev.map(|e| e[2] >> 24) {
            Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => true,
            code => {
                self.bts[i].errors += 1;
                log::warn!("bluetooth: {}: bulk OUT of {} bytes failed ({code:?})", self.bts[i].label, pkt.len());
                let enq = self.bts[i].out_ring.enqueue_ptr();
                self.recover_ep(slot, dci, enq, code.is_none());
                if code == Some(CC_STALL)
                    && let Some(mut d) = self.bts[i].dev.take()
                {
                    self.control_quiet(&mut d, 0x02, 1, 0, (dci / 2) as u16, 0, true);
                    self.bts[i].dev = Some(d);
                }
                false
            }
        }
    }

    /// Bind a configured Bluetooth device: its endpoints, then (Intel)
    /// the firmware; the controller is handed to the Bluetooth core once
    /// the xHCI is registered ([`register`]).
    pub(super) fn btusb(&mut self, d: UsbDev, f: &Iface, ids: (u16, u16), name: &str) {
        let label = format!("usb{}-{} {:04x}:{:04x}", self.bus, d.topo.path, ids.0, ids.1);
        log::info!("bluetooth: {label}: {name}, {} speed", super::speed_name(d.speed));
        let find = |ty: u8, input: bool| f.eps.iter().find(|e| e.1 & 3 == ty && (e.0 & 0x80 != 0) == input).copied();
        let (Some(ei), Some(bi), Some(bo)) = (find(3, true), find(2, true), find(2, false)) else {
            log::warn!("bluetooth: {label}: interface 0 lacks the interrupt IN / bulk IN / bulk OUT endpoints");
            return;
        };
        let slot = d.slot;
        let dcis = [dci_of(ei.0), dci_of(bi.0), dci_of(bo.0)];
        self.input_ctx(&d, 1 | 1 << dcis[0] | 1 << dcis[1] | 1 << dcis[2]);
        self.slot_ctx(&d, *dcis.iter().max().unwrap());
        let mut rings = Vec::new();
        for (e, ty) in [(ei, 7u32), (bi, 6), (bo, 2)] {
            let Some(ring) = Ring::new() else { return };
            let mps = (e.2 as u32).clamp(8, SLOT as u32);
            let burst = if d.speed >= SPEED_SUPER { e.4 as u32 } else { 0 };
            let c = self.ctx(d.input.1, dci_of(e.0) as usize + 1);
            let tr = ring.enqueue_ptr();
            unsafe {
                *c = if ty == 7 { ep_interval(d.speed, e.3) << 16 } else { 0 };
                *c.add(1) = 3 << 1 | ty << 3 | burst << 8 | mps << 16;
                *c.add(2) = tr as u32;
                *c.add(3) = (tr >> 32) as u32;
                *c.add(4) = if ty == 7 { mps | mps << 16 } else { 1024 };
            }
            rings.push((ring, mps));
        }
        if let Err(code) = self.command_code([d.input.0 as u32, (d.input.0 >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | slot << 24]) {
            log::warn!("bluetooth: {label}: Configure Endpoint failed (completion code {code})");
            return;
        }
        let (Some(ib), Some(bb), Some(ob)) =
            (pci::dma_alloc(RING_TRBS * SLOT / 4096), pci::dma_alloc(RING_TRBS * SLOT / 4096), pci::dma_alloc(1))
        else {
            log::warn!("bluetooth: {label}: out of memory");
            return;
        };
        let (out_ring, _) = rings.pop().unwrap();
        let (bring, bmps) = rings.pop().unwrap();
        let (iring, imps) = rings.pop().unwrap();
        let mut b = Bt {
            dev: Some(d),
            slot,
            intr: InEp { dci: dcis[0], ring: iring, bufs: ib, mps: imps, outstanding: 0 },
            bulk_in: InEp { dci: dcis[1], ring: bring, bufs: bb, mps: bmps, outstanding: 0 },
            out_dci: dcis[2],
            out_ring,
            out_buf: ob,
            events: Reassembler::new(H4_EVENT),
            acl: Reassembler::new(H4_ACL),
            boot_events: Reassembler::new(H4_EVENT),
            bootloader: false,
            setup: true,
            pending: VecDeque::new(),
            label: label.clone(),
            errors: 0,
        };
        b.intr.refill();
        b.bulk_in.refill();
        self.ring_doorbell(slot, dcis[0]);
        self.ring_doorbell(slot, dcis[1]);
        let i = self.bts.len();
        self.bts.push(b);
        log::info!(
            "bluetooth: {label}: endpoints: events {:02x} ({imps} B), ACL in {:02x} out {:02x} ({bmps} B)",
            ei.0,
            bi.0,
            bo.0
        );
        if ids.0 == 0x8087 {
            let t0 = crate::time::monotonic_ns();
            match intel::setup(&mut UsbHci { x: self, i }, intel::firmware) {
                Ok(o) => log::info!(
                    "bluetooth: {label}: Intel controller ready in {} ms (firmware build {}){}, address {}",
                    (crate::time::monotonic_ns() - t0) / 1_000_000,
                    o.version.build_num,
                    if o.downloaded { " (firmware downloaded)" } else { "" },
                    o.bdaddr.map_or("unknown".into(), |a| intel::bdaddr_str(&a))
                ),
                Err(e) => log::error!("bluetooth: {label}: Intel setup failed: {e}"),
            }
        }
        let b = &mut self.bts[i];
        b.bootloader = false;
        b.setup = false;
        b.pending.clear();
        self.new_bts.push(i);
    }
}

/// The xHCI side of [`intel::setup`].
struct UsbHci<'a> {
    x: &'a mut Xhci,
    i: usize,
}

impl intel::Hci for UsbHci<'_> {
    fn send_cmd(&mut self, pkt: &[u8], bulk: bool) -> bool {
        if bulk { self.x.bt_bulk_out(self.i, pkt) } else { self.x.bt_command(self.i, pkt) }
    }

    fn next_event(&mut self, ms: u64) -> Option<Vec<u8>> {
        let (x, i) = (&mut *self.x, self.i);
        if x.bts[i].pending.is_empty() {
            wait_until(ms, || {
                while let Some(ev) = x.ev.pop() {
                    x.dispatch(&ev);
                }
                !x.bts[i].pending.is_empty()
            });
            x.update_erdp();
        }
        x.bts[i].pending.pop_front()
    }

    fn set_bootloader(&mut self, on: bool) {
        self.x.bts[self.i].bootloader = on;
    }

    fn progress(&mut self, stage: &str, pct: u32) {
        log::info!("bluetooth: {}: {stage} {pct}%", self.x.bts[self.i].label);
    }
}

/// A USB controller as the Bluetooth core sees it.
struct UsbBt {
    ctrl: usize,
    idx: usize,
    label: String,
}

impl Controller for UsbBt {
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()> {
        let mut ctrls = CTRLS.lock();
        let x = ctrls.get_mut(self.ctrl).ok_or(EIO)?;
        let ok = match kind {
            H4_CMD => x.bt_command(self.idx, pkt),
            H4_ACL => x.bt_bulk_out(self.idx, pkt),
            // SCO/ISO need the isochronous interface: dropped.
            _ => true,
        };
        if ok { Ok(()) } else { Err(EIO) }
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// Hand the controllers probed on xHCI `ctrl` to the Bluetooth core.
pub(super) fn register(ctrl: usize, idx: usize, label: String) {
    bluetooth::register(Arc::new(UsbBt { ctrl, idx, label }));
}
