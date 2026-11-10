//! USB mass storage: the Bulk-Only Transport (class 8, SCSI transparent
//! subclass 6, protocol 0x50) with SCSI block commands, LUN 0.
//!
//! Each command is a 31-byte Command Block Wrapper on the bulk OUT
//! endpoint, the data stage (at most [`MAX_SECTORS`] sectors through a
//! bounce buffer, split into Normal TRBs at 64 KiB boundaries), and the
//! 13-byte Command Status Wrapper from the bulk IN endpoint. A stalled
//! endpoint is reset on the controller (Reset Endpoint, Set TR Dequeue)
//! and on the device (CLEAR_FEATURE(ENDPOINT_HALT)); a transport error, a
//! timeout or a phase error does the full Bulk-Only reset recovery
//! (Bulk-Only Mass Storage Reset, then both halts cleared). A failed
//! command's sense data comes from REQUEST SENSE.
//!
//! Probe: INQUIRY (a direct-access device), TEST UNIT READY until the
//! medium is ready (up to 10 s, REQUEST SENSE in between, which also
//! clears the unit attention after reset), READ CAPACITY(10) or (16) for
//! disks past 2 TiB. Only 512-byte logical blocks are supported. I/O is
//! READ(10)/WRITE(10), or READ(16)/WRITE(16) past LBA 2^32, retried
//! three times; SYNCHRONIZE CACHE(10) on flush (ignored when the device
//! does not know it). The disk is an sd device (8:16n) whose sysfs path
//! is the one Linux's usb-storage gives it,
//! `.../0000:00:14.0/usb1/1-3/1-3:1.0/host0/target0:0:0/0:0:0:0/block/sda`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::{
    CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, CTRLS, Iface, Ring, SPEED_SUPER, TIMEOUT_MS, TRB_CH, TRB_CONFIGURE_EP, TRB_IOC, TRB_ISP,
    TRB_NORMAL, TRB_TRANSFER_EVENT, UsbDev, Xhci, dci_of, pci, trb_ptr, trb_type, wait_until,
};
use crate::block::{self, Backend, BlockDev, SECTOR};
use crate::errno::*;

/// Sectors per command (Linux's usb-storage default, 120 KiB).
const MAX_SECTORS: usize = 240;
const BUF_BYTES: usize = MAX_SECTORS * SECTOR as usize;
/// Time for one command's data and status (slow sticks, first access).
const DATA_TIMEOUT_MS: u64 = 20_000;
const CSW_OFF: u64 = 64;
const SENSE_NOT_READY: u8 = 2;
const SENSE_UNIT_ATTENTION: u8 = 6;
const ASC_NO_MEDIUM: u8 = 0x3a;
/// A transport failure instead of a sense key.
const TRANSPORT: u8 = 0xff;

pub(super) struct BulkEp {
    addr: u8,
    dci: u32,
    mps: u32,
    ring: Ring,
}

pub(super) struct Msd {
    dev: UsbDev,
    iface: u8,
    ep_in: BulkEp,
    ep_out: BulkEp,
    /// CBW at 0, CSW at [`CSW_OFF`] (one page).
    cmd: (u64, *mut u8),
    /// The data stage's bounce buffer ([`BUF_BYTES`]).
    buf: (u64, *mut u8),
    tag: u32,
}

unsafe impl Send for Msd {}

/// A disk found at bring-up, registered once the controller is in
/// [`CTRLS`].
pub(super) struct NewDisk {
    idx: usize,
    sectors: u64,
    /// The interface's sysfs directory.
    iface_dir: String,
    vendor: String,
    model: String,
}

enum Bot {
    /// CSW status 1: the device has sense data.
    Failed,
    /// Anything else (reset recovery done).
    Transport,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The CDB of a READ/WRITE of `count` sectors at `lba`.
fn rw_cdb(write: bool, lba: u64, count: u16) -> ([u8; 16], usize) {
    let mut c = [0u8; 16];
    if lba <= u32::MAX as u64 {
        c[0] = if write { 0x2a } else { 0x28 };
        c[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        c[7..9].copy_from_slice(&count.to_be_bytes());
        (c, 10)
    } else {
        c[0] = if write { 0x8a } else { 0x88 };
        c[2..10].copy_from_slice(&lba.to_be_bytes());
        c[10..14].copy_from_slice(&(count as u32).to_be_bytes());
        (c, 16)
    }
}

fn ascii(b: &[u8]) -> String {
    b.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { ' ' }).collect::<String>().trim().into()
}

impl Xhci {
    /// A bulk transfer of `len` bytes at `phys`: the bytes moved, or the
    /// completion code (0: timed out). The endpoint is usable again
    /// afterwards except for a device-side stall (CC_STALL), which the
    /// caller clears with CLEAR_FEATURE.
    fn bulk(&mut self, slot: u32, ep: &mut BulkEp, phys: u64, len: usize, ms: u64) -> Result<usize, u32> {
        let input = ep.addr & 0x80 != 0;
        let mut pieces = Vec::new();
        let mut off = 0;
        while off < len {
            let a = phys + off as u64;
            let n = (len - off).min(0x10000 - (a & 0xffff) as usize);
            pieces.push((a, n));
            off += n;
        }
        let mut trbs: Vec<(u64, usize)> = Vec::with_capacity(pieces.len());
        let mut done = 0;
        for (i, &(a, n)) in pieces.iter().enumerate() {
            let last = i + 1 == pieces.len();
            done += n;
            // TD Size: packets still to come after this TRB.
            let td_size = if last { 0 } else { ((len - done) as u32).div_ceil(ep.mps).min(31) };
            let flags = TRB_NORMAL << 10 | if last { TRB_IOC } else { TRB_CH } | if input { TRB_ISP } else { 0 };
            let t = ep.ring.push([a as u32, (a >> 32) as u32, n as u32 | td_size << 17, flags]);
            trbs.push((t, n));
        }
        self.ring_doorbell(slot, ep.dci);
        let dci = ep.dci;
        let ev = self.wait_event(ms, |e| {
            trb_type(e) == TRB_TRANSFER_EVENT
                && e[3] >> 24 == slot
                && (e[3] >> 16) & 0x1f == dci
                && trbs.iter().any(|t| t.0 == trb_ptr(e))
        });
        let Some(e) = ev else {
            log::warn!("usb-storage: slot {slot} endpoint {dci}: transfer of {len} bytes timed out");
            self.recover_ep(slot, dci, ep.ring.enqueue_ptr(), true);
            return Err(0);
        };
        let code = e[2] >> 24;
        match code {
            CC_SUCCESS | CC_SHORT_PACKET => {
                let k = trbs.iter().position(|t| t.0 == trb_ptr(&e)).unwrap_or(0);
                let before: usize = trbs[..k].iter().map(|t| t.1).sum();
                Ok(before + trbs[k].1.saturating_sub((e[2] & 0xff_ffff) as usize))
            }
            _ => {
                if code != CC_STALL {
                    log::warn!("usb-storage: slot {slot} endpoint {dci}: transfer error {code}");
                }
                self.recover_ep(slot, dci, ep.ring.enqueue_ptr(), false);
                Err(code)
            }
        }
    }

    /// Clear a halted bulk endpoint on the device too.
    fn clear_halt(&mut self, m: &mut Msd, input: bool) {
        let ep = if input { &m.ep_in } else { &m.ep_out };
        let (dci, enq, addr) = (ep.dci, ep.ring.enqueue_ptr(), ep.addr);
        self.recover_ep(m.dev.slot, dci, enq, false);
        self.control(&mut m.dev, 0x02, 1, 0, addr as u16, 0);
    }

    /// Bulk-Only reset recovery.
    fn bot_reset(&mut self, m: &mut Msd) {
        log::warn!("usb-storage: slot {}: reset recovery", m.dev.slot);
        self.control(&mut m.dev, 0x21, 0xff, 0, m.iface as u16, 0);
        self.clear_halt(m, true);
        self.clear_halt(m, false);
    }

    /// One Bulk-Only command; the data bytes moved.
    fn bot(&mut self, m: &mut Msd, cdb: &[u8], len: usize, input: bool, ms: u64) -> Result<usize, Bot> {
        m.tag = m.tag.wrapping_add(1);
        let mut cbw = [0u8; 31];
        cbw[..4].copy_from_slice(b"USBC");
        cbw[4..8].copy_from_slice(&m.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        cbw[12] = if input { 0x80 } else { 0 };
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);
        unsafe { core::ptr::copy_nonoverlapping(cbw.as_ptr(), m.cmd.1, 31) };
        let slot = m.dev.slot;
        match self.bulk(slot, &mut m.ep_out, m.cmd.0, 31, TIMEOUT_MS * 5) {
            Ok(31) => {}
            _ => {
                self.bot_reset(m);
                return Err(Bot::Transport);
            }
        }
        let mut got = 0;
        if len > 0 {
            let ep = if input { &mut m.ep_in } else { &mut m.ep_out };
            match self.bulk(slot, ep, m.buf.0, len, ms) {
                Ok(n) => got = n,
                Err(CC_STALL) => self.clear_halt(m, input),
                Err(_) => {
                    self.bot_reset(m);
                    return Err(Bot::Transport);
                }
            }
        }
        // The status, read once more after clearing a stall.
        let mut ok = false;
        for attempt in 0..2 {
            match self.bulk(slot, &mut m.ep_in, m.cmd.0 + CSW_OFF, 13, ms) {
                Ok(13) => {
                    ok = true;
                    break;
                }
                Err(CC_STALL) if attempt == 0 => self.clear_halt(m, true),
                _ => break,
            }
        }
        let csw = unsafe { core::slice::from_raw_parts(m.cmd.1.add(CSW_OFF as usize), 13) };
        if !ok || &csw[..4] != b"USBS" || csw[4..8] != m.tag.to_le_bytes() || csw[12] > 1 {
            log::warn!("usb-storage: slot {slot}: command {:#04x}: bad status ({:02x?})", cdb[0], csw);
            self.bot_reset(m);
            return Err(Bot::Transport);
        }
        if csw[12] == 1 {
            return Err(Bot::Failed);
        }
        Ok(got)
    }

    /// REQUEST SENSE: (sense key, additional sense code).
    fn sense(&mut self, m: &mut Msd) -> (u8, u8) {
        match self.bot(m, &[0x03, 0, 0, 0, 18, 0], 18, true, TIMEOUT_MS * 5) {
            Ok(n) if n >= 14 => {
                let s = unsafe { core::slice::from_raw_parts(m.buf.1, 18) };
                (s[2] & 0xf, s[12])
            }
            _ => (TRANSPORT, 0),
        }
    }

    /// A SCSI command: the data bytes moved, or the sense key and code.
    fn scsi(&mut self, m: &mut Msd, cdb: &[u8], len: usize, input: bool) -> Result<usize, (u8, u8)> {
        match self.bot(m, cdb, len, input, DATA_TIMEOUT_MS) {
            Ok(n) => Ok(n),
            Err(Bot::Failed) => Err(self.sense(m)),
            Err(Bot::Transport) => Err((TRANSPORT, 0)),
        }
    }

    /// Read or write sectors through the bounce buffer, with retries.
    fn disk_rw(&mut self, m: &mut Msd, write: bool, lba: u64, count: usize, mut each: impl FnMut(*mut u8)) -> KResult<()> {
        let len = count * SECTOR as usize;
        let (cdb, n) = rw_cdb(write, lba, count as u16);
        let mut last = (0, 0);
        for _ in 0..3 {
            if write {
                each(m.buf.1);
            }
            match self.scsi(m, &cdb[..n], len, !write) {
                Ok(got) if got == len => {
                    if !write {
                        each(m.buf.1);
                    }
                    return Ok(());
                }
                Ok(got) => last = (0, got as u8),
                Err(e) => last = e,
            }
        }
        log::warn!(
            "usb-storage: slot {}: {} of {count} sectors at {lba} failed (sense {:#x}/{:#x})",
            m.dev.slot,
            if write { "write" } else { "read" },
            last.0,
            last.1
        );
        Err(EIO)
    }

    /// A configured mass storage device: set up its bulk endpoints and
    /// probe it as a disk.
    pub(super) fn storage(&mut self, mut d: UsbDev, f: &Iface, usbdev: &str, name: &str) {
        let bulk = |input: bool| f.eps.iter().find(|e| e.1 & 3 == 2 && (e.0 & 0x80 != 0) == input).copied();
        let (Some(ein), Some(eout)) = (bulk(true), bulk(false)) else {
            log::warn!("usb-storage: {name}: no bulk endpoint pair");
            return;
        };
        let slot = d.slot;
        let (din, dout) = (dci_of(ein.0), dci_of(eout.0));
        self.input_ctx(&d, 1 | 1 << din | 1 << dout);
        self.slot_ctx(&d, din.max(dout));
        let mut eps = Vec::new();
        for (e, ty) in [(ein, 6u32), (eout, 2u32)] {
            let Some(ring) = Ring::new() else { return };
            let mps = (e.2 as u32).max(8);
            let burst = if d.speed >= SPEED_SUPER { e.4 as u32 } else { 0 };
            let c = self.ctx(d.input.1, dci_of(e.0) as usize + 1);
            let tr = ring.enqueue_ptr();
            unsafe {
                *c = 0;
                *c.add(1) = 3 << 1 | ty << 3 | burst << 8 | mps << 16;
                *c.add(2) = tr as u32;
                *c.add(3) = (tr >> 32) as u32;
                *c.add(4) = 3072;
            }
            eps.push(BulkEp { addr: e.0, dci: dci_of(e.0), mps, ring });
        }
        if self.command([d.input.0 as u32, (d.input.0 >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | slot << 24]).is_none() {
            log::warn!("usb-storage: {name}: cannot configure the bulk endpoints");
            return;
        }
        let (Some(cmd), Some(buf)) = (pci::dma_alloc(1), pci::dma_alloc(BUF_BYTES / 4096)) else {
            log::warn!("usb-storage: {name}: out of memory");
            return;
        };
        // GET_MAX_LUN: only LUN 0 is used; many devices stall it.
        self.control_quiet(&mut d, 0xa1, 0xfe, 0, f.num as u16, 1, true);
        let ep_out = eps.pop().unwrap();
        let ep_in = eps.pop().unwrap();
        let mut m = Msd { dev: d, iface: f.num, ep_in, ep_out, cmd, buf, tag: 0 };

        let mut inq = None;
        for _ in 0..3 {
            if let Ok(n) = self.scsi(&mut m, &[0x12, 0, 0, 0, 36, 0], 36, true)
                && n >= 32
            {
                inq = Some(unsafe { core::slice::from_raw_parts(m.buf.1, 36) }.to_vec());
                break;
            }
        }
        let Some(inq) = inq else {
            log::warn!("usb-storage: {name}: INQUIRY failed");
            return;
        };
        if inq[0] & 0x1f != 0 {
            log::info!("usb-storage: {name}: SCSI device type {:#x}, not a disk", inq[0] & 0x1f);
            return;
        }
        let (vendor, model) = (ascii(&inq[8..16]), ascii(&inq[16..32]));
        let mut ready = false;
        let mut no_medium = 0;
        wait_until(10_000, || {
            match self.scsi(&mut m, &[0; 6], 0, false) {
                Ok(_) => ready = true,
                Err((SENSE_NOT_READY, ASC_NO_MEDIUM)) => no_medium += 1,
                Err(_) => {}
            }
            if !ready && no_medium < 5 {
                wait_until(100, || false);
            }
            ready || no_medium >= 5
        });
        if !ready {
            log::warn!("usb-storage: {name}: {vendor} {model}: {}", if no_medium > 0 { "no medium" } else { "not ready after 10 s" });
            return;
        }
        let mut cap = None;
        for _ in 0..3 {
            match self.scsi(&mut m, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8, true) {
                Ok(8) => {
                    let b = unsafe { core::slice::from_raw_parts(m.buf.1, 8) };
                    let (last, bs) = (be32(b), be32(&b[4..]));
                    if last != u32::MAX {
                        cap = Some((last as u64 + 1, bs));
                        break;
                    }
                    let mut c16 = [0u8; 16];
                    c16[0] = 0x9e;
                    c16[1] = 0x10;
                    c16[13] = 32;
                    if let Ok(n) = self.scsi(&mut m, &c16, 32, true)
                        && n >= 12
                    {
                        let b = unsafe { core::slice::from_raw_parts(m.buf.1, 12) };
                        let last = u64::from_be_bytes(b[..8].try_into().unwrap());
                        cap = Some((last + 1, be32(&b[8..])));
                        break;
                    }
                }
                Err((SENSE_UNIT_ATTENTION, _)) => continue,
                _ => {}
            }
        }
        let Some((sectors, bs)) = cap else {
            log::warn!("usb-storage: {name}: {vendor} {model}: READ CAPACITY failed");
            return;
        };
        if bs != SECTOR as u32 {
            log::warn!("usb-storage: {name}: {vendor} {model}: {bs}-byte blocks not supported");
            return;
        }
        log::info!(
            "usb-storage: usb{}-{}: {vendor} {model}, {sectors} sectors ({} MiB)",
            self.bus,
            m.dev.topo.path,
            sectors >> 11
        );
        let iface_dir = format!("{usbdev}/{}-{}:1.{}", self.bus, m.dev.topo.path, f.num);
        let idx = self.disks.len();
        self.disks.push(Some(Box::new(m)));
        self.new_disks.push(NewDisk { idx, sectors, iface_dir, vendor, model });
    }
}

/// An sd disk on a USB mass storage device.
pub struct UsbDisk {
    ctrl: usize,
    idx: usize,
    sectors: u64,
}

impl UsbDisk {
    fn rw(&self, write: bool, off: u64, len: usize, mut each: impl FnMut(*mut u8, usize, usize)) -> KResult<()> {
        if off % SECTOR != 0 || len as u64 % SECTOR != 0 || off + len as u64 > self.size() {
            return Err(EINVAL);
        }
        let mut ctrls = CTRLS.lock();
        let x = ctrls.get_mut(self.ctrl).ok_or(EIO)?;
        let mut m = x.disks.get_mut(self.idx).and_then(Option::take).ok_or(EIO)?;
        let mut done = 0;
        let mut r = Ok(());
        while done < len {
            let n = (len - done).min(BUF_BYTES);
            let lba = (off + done as u64) / SECTOR;
            r = x.disk_rw(&mut m, write, lba, n / SECTOR as usize, |b| each(b, done, n));
            if r.is_err() {
                break;
            }
            done += n;
        }
        x.disks[self.idx] = Some(m);
        r
    }
}

impl Backend for UsbDisk {
    fn size(&self) -> u64 {
        self.sectors * SECTOR
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let dst = buf.as_mut_ptr();
        self.rw(false, off, buf.len(), |b, at, n| unsafe { core::ptr::copy_nonoverlapping(b, dst.add(at), n) })
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        self.rw(true, off, data.len(), |b, at, n| unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(at), b, n) })
    }

    fn flush(&self) -> KResult<()> {
        let mut ctrls = CTRLS.lock();
        let x = ctrls.get_mut(self.ctrl).ok_or(EIO)?;
        let mut m = x.disks.get_mut(self.idx).and_then(Option::take).ok_or(EIO)?;
        // SYNCHRONIZE CACHE(10); devices without a cache may reject it.
        let _ = x.scsi(&mut m, &[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, false);
        x.disks[self.idx] = Some(m);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Register a disk probed on controller `ctrl`.
pub(super) fn register(ctrl: usize, d: NewDisk) {
    let (n, name) = block::next_sd();
    let scsi = format!("{}/host{n}/target{n}:0:0/{n}:0:0:0", d.iface_dir);
    crate::fs::sysfs::set(&scsi, "vendor", &format!("{}\n", d.vendor));
    crate::fs::sysfs::set(&scsi, "model", &format!("{}\n", d.model));
    log::info!("usb-storage: {name}: {} {}", d.vendor, d.model);
    let disk = UsbDisk { ctrl, idx: d.idx, sectors: d.sectors };
    let mut bd = BlockDev::new(name, block::SCSI_DISK_MAJOR, n * 16, Arc::new(disk));
    bd.sysdev = Some(scsi);
    block::add_disk(Arc::new(bd));
}

#[test_case]
fn usb_storage_cdbs() {
    let (c, n) = rw_cdb(false, 0x1234_5678, 240);
    assert_eq!(n, 10);
    assert_eq!(c[..10], [0x28, 0, 0x12, 0x34, 0x56, 0x78, 0, 0, 240, 0]);
    let (c, n) = rw_cdb(true, 1 << 32, 8);
    assert_eq!(n, 16);
    assert_eq!(c[..16], [0x8a, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0]);
}
