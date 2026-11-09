//! NVMe (PCI class 01.08.02): one admin queue pair and one I/O queue
//! pair per controller, polled, one command in flight.
//!
//! Bring-up follows the spec: disable (CC.EN = 0, wait CSTS.RDY = 0),
//! admin queues in AQA/ASQ/ACQ, enable, IDENTIFY the controller (MDTS,
//! number of namespaces), create I/O completion and submission queue 1,
//! then IDENTIFY each active namespace. Every active namespace with 512-byte
//! logical blocks becomes a disk `nvme<C>n<N>` (Linux's names; partitions
//! `nvme0n1p1`, ...) with device numbers from blkext (259:n), as on Linux.
//! Transfers go through a bounce buffer (up to [`MAX_XFER`] bytes,
//! described by PRP1/PRP2 or a PRP list page), so any kernel buffer works.
//! Commands are matched to completions by the phase bit; the doorbells
//! use the controller's stride (CAP.DSTRD).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::pci::{self, CMD_BUS_MASTER, CMD_INTX_DISABLE, CMD_MEMORY, mmio_read32, mmio_read64, mmio_write32, mmio_write64, wait_until};
use crate::block::{self, Backend, BlockDev, SECTOR};
use crate::errno::*;

const REG_CAP: u64 = 0x00;
const REG_VS: u64 = 0x08;
const REG_CC: u64 = 0x14;
const REG_CSTS: u64 = 0x1c;
const REG_AQA: u64 = 0x24;
const REG_ASQ: u64 = 0x28;
const REG_ACQ: u64 = 0x30;
const DOORBELLS: u64 = 0x1000;

const CC_EN: u32 = 1;
/// I/O submission / completion entry sizes (2^6, 2^4).
const CC_IOSQES: u32 = 6 << 16;
const CC_IOCQES: u32 = 4 << 20;
const CSTS_RDY: u32 = 1;
const CSTS_CFS: u32 = 2;

const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

/// Entries per queue (one page of submission entries).
const QUEUE_LEN: u16 = 64;
const PAGE: usize = 4096;
/// Largest transfer per command (lowered to the controller's MDTS).
const MAX_XFER: usize = 128 * 1024;
/// Command timeout.
const TIMEOUT_MS: u64 = 5000;

static NEXT_CTRL: AtomicU32 = AtomicU32::new(0);

/// A submission/completion queue pair.
struct Queue {
    qid: u16,
    sq: (u64, *mut u8),
    cq: (u64, *mut u8),
    tail: u16,
    head: u16,
    phase: bool,
    cid: u16,
}

unsafe impl Send for Queue {}

impl Queue {
    fn new(qid: u16) -> Option<Queue> {
        Some(Queue { qid, sq: pci::dma_alloc(1)?, cq: pci::dma_alloc(1)?, tail: 0, head: 0, phase: true, cid: 0 })
    }
}

struct Ctrl {
    regs: u64,
    stride: u64,
    admin: Queue,
    io: Queue,
    /// Bounce buffer (MAX_XFER bytes) and the PRP list page after it.
    buf: (u64, *mut u8),
    prp_list: (u64, *mut u8),
    max_xfer: usize,
}

unsafe impl Send for Ctrl {}

/// A command's dwords 10..15 and data length (in the bounce buffer).
struct Cmd {
    opcode: u8,
    nsid: u32,
    cdw: [u32; 6],
    len: usize,
    /// PRP1 given directly (queue creation) instead of the bounce buffer.
    prp: u64,
}

/// PRP1/PRP2 for `len` bytes at the physically contiguous `buf`: the
/// second page directly, or a list (written into `list`) of every page
/// after the first.
fn prps(buf: u64, len: usize, list_phys: u64, list: &mut [u64]) -> (u64, u64) {
    let pages = len.div_ceil(PAGE);
    match pages {
        0 | 1 => (buf, 0),
        2 => (buf, buf + PAGE as u64),
        n => {
            for (i, e) in list.iter_mut().take(n - 1).enumerate() {
                *e = buf + ((i + 1) * PAGE) as u64;
            }
            (buf, list_phys)
        }
    }
}

impl Ctrl {
    fn doorbell(&self, qid: u16, completion: bool) -> u64 {
        DOORBELLS + (2 * qid as u64 + completion as u64) * self.stride
    }

    /// Submit on the admin (`admin`) or I/O queue and wait for the
    /// completion; returns its dword 0.
    fn submit(&mut self, admin: bool, c: &Cmd) -> KResult<u32> {
        let (prp1, prp2) = if c.prp != 0 {
            (c.prp, 0)
        } else if c.len > 0 {
            let list = unsafe { core::slice::from_raw_parts_mut(self.prp_list.1 as *mut u64, PAGE / 8) };
            prps(self.buf.0, c.len, self.prp_list.0, list)
        } else {
            (0, 0)
        };
        let regs = self.regs;
        let (sq_db, cq_db) = {
            let qid = if admin { 0 } else { self.io.qid };
            (self.doorbell(qid, false), self.doorbell(qid, true))
        };
        let q = if admin { &mut self.admin } else { &mut self.io };
        q.cid = q.cid.wrapping_add(1);
        let cid = q.cid;
        let mut e = [0u32; 16];
        e[0] = c.opcode as u32 | (cid as u32) << 16;
        e[1] = c.nsid;
        e[6] = prp1 as u32;
        e[7] = (prp1 >> 32) as u32;
        e[8] = prp2 as u32;
        e[9] = (prp2 >> 32) as u32;
        e[10..16].copy_from_slice(&c.cdw);
        unsafe {
            let slot = q.sq.1.add(q.tail as usize * 64) as *mut u32;
            for (i, w) in e.iter().enumerate() {
                core::ptr::write_volatile(slot.add(i), *w);
            }
        }
        q.tail = (q.tail + 1) % QUEUE_LEN;
        core::sync::atomic::fence(Ordering::SeqCst);
        mmio_write32(regs, sq_db, q.tail as u32);
        let entry = unsafe { q.cq.1.add(q.head as usize * 16) as *const u32 };
        let phase = q.phase;
        let done = wait_until(TIMEOUT_MS, || unsafe { core::ptr::read_volatile(entry.add(3)) >> 16 & 1 == phase as u32 });
        if !done {
            log::warn!("nvme: command {:#x} timed out (CSTS {:#x})", c.opcode, mmio_read32(regs, REG_CSTS));
            return Err(EIO);
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        let (dw0, dw3) = unsafe { (core::ptr::read_volatile(entry), core::ptr::read_volatile(entry.add(3))) };
        q.head = (q.head + 1) % QUEUE_LEN;
        if q.head == 0 {
            q.phase = !q.phase;
        }
        mmio_write32(regs, cq_db, q.head as u32);
        let status = (dw3 >> 17) & 0x7fff;
        if dw3 & 0xffff != cid as u32 || status != 0 {
            log::warn!("nvme: command {:#x} failed: status {status:#x} cid {}", c.opcode, dw3 & 0xffff);
            return Err(EIO);
        }
        Ok(dw0)
    }

    fn identify(&mut self, cns: u32, nsid: u32) -> KResult<[u8; 4096]> {
        self.submit(true, &Cmd { opcode: ADMIN_IDENTIFY, nsid, cdw: [cns, 0, 0, 0, 0, 0], len: 4096, prp: 0 })?;
        let mut out = [0u8; 4096];
        unsafe { core::ptr::copy_nonoverlapping(self.buf.1, out.as_mut_ptr(), 4096) };
        Ok(out)
    }
}

/// One namespace: a disk.
pub struct Namespace {
    ctrl: Arc<Mutex<Ctrl>>,
    nsid: u32,
    sectors: u64,
}

impl Namespace {
    fn rw(&self, write: bool, off: u64, len: usize, mut each: impl FnMut(&mut Ctrl, usize, usize)) -> KResult<()> {
        if off % SECTOR != 0 || len as u64 % SECTOR != 0 || off + len as u64 > self.size() {
            return Err(EINVAL);
        }
        let mut c = self.ctrl.lock();
        let max = c.max_xfer;
        let mut done = 0;
        while done < len {
            let n = (len - done).min(max);
            if write {
                each(&mut c, done, n);
            }
            let lba = (off + done as u64) / SECTOR;
            let cdw = [lba as u32, (lba >> 32) as u32, (n as u64 / SECTOR - 1) as u32, 0, 0, 0];
            c.submit(false, &Cmd { opcode: if write { IO_WRITE } else { IO_READ }, nsid: self.nsid, cdw, len: n, prp: 0 })?;
            if !write {
                each(&mut c, done, n);
            }
            done += n;
        }
        Ok(())
    }
}

impl Backend for Namespace {
    fn size(&self) -> u64 {
        self.sectors * SECTOR
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let dst = buf.as_mut_ptr();
        self.rw(false, off, buf.len(), |c, at, n| unsafe { core::ptr::copy_nonoverlapping(c.buf.1, dst.add(at), n) })
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        self.rw(true, off, data.len(), |c, at, n| unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(at), c.buf.1, n) })
    }

    fn flush(&self) -> KResult<()> {
        self.ctrl.lock().submit(false, &Cmd { opcode: IO_FLUSH, nsid: self.nsid, cdw: [0; 6], len: 0, prp: 0 })?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Bring up every NVMe controller.
pub fn init() {
    for d in pci::devices() {
        if d.class == 0x01 && d.subclass == 0x08 && d.prog_if == 0x02 {
            if let Err(e) = probe(&d) {
                log::warn!("nvme {:?}: {e}", d.addr);
            }
        }
    }
}

fn probe(d: &pci::Device) -> Result<(), &'static str> {
    let (regs, _) = pci::map_bar(d, 0).ok_or("no MMIO BAR 0")?;
    d.enable(CMD_MEMORY | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    let cap = mmio_read64(regs, REG_CAP);
    let mqes = (cap & 0xffff) as u16 + 1;
    if mqes < QUEUE_LEN {
        return Err("queues too small");
    }
    let stride = 4u64 << ((cap >> 32) & 0xf);
    // CAP.TO is in 500 ms units.
    let timeout = ((cap >> 24) & 0xff).max(1) * 500;
    if mmio_read32(regs, REG_CC) & CC_EN != 0 {
        mmio_write32(regs, REG_CC, 0);
    }
    if !wait_until(timeout, || mmio_read32(regs, REG_CSTS) & CSTS_RDY == 0) {
        return Err("controller does not disable");
    }
    let admin = Queue::new(0).ok_or("out of memory")?;
    let io = Queue::new(1).ok_or("out of memory")?;
    let buf = pci::dma_alloc(MAX_XFER / PAGE).ok_or("out of memory")?;
    let prp_list = pci::dma_alloc(1).ok_or("out of memory")?;
    let aqa = (QUEUE_LEN as u32 - 1) << 16 | (QUEUE_LEN as u32 - 1);
    mmio_write32(regs, REG_AQA, aqa);
    mmio_write64(regs, REG_ASQ, admin.sq.0);
    mmio_write64(regs, REG_ACQ, admin.cq.0);
    mmio_write32(regs, REG_CC, CC_EN | CC_IOSQES | CC_IOCQES);
    if !wait_until(timeout, || mmio_read32(regs, REG_CSTS) & (CSTS_RDY | CSTS_CFS) != 0)
        || mmio_read32(regs, REG_CSTS) & CSTS_CFS != 0
    {
        return Err("controller does not become ready");
    }
    let mut c = Ctrl { regs, stride, admin, io, buf, prp_list, max_xfer: MAX_XFER };
    let id = c.identify(1, 0).map_err(|_| "IDENTIFY controller failed")?;
    // MDTS: 2^n minimum pages (CAP.MPSMIN, 4 KiB here); 0 = no limit.
    let mdts = id[77];
    if mdts != 0 {
        c.max_xfer = c.max_xfer.min(PAGE << mdts);
    }
    let nn = u32::from_le_bytes(id[516..520].try_into().unwrap());
    let model: String = String::from_utf8_lossy(&id[24..64]).trim().into();
    let serial: String = String::from_utf8_lossy(&id[4..24]).trim().into();
    let (cq_phys, sq_phys) = (c.io.cq.0, c.io.sq.0);
    let qsize = (QUEUE_LEN as u32 - 1) << 16 | 1;
    c.submit(true, &Cmd { opcode: ADMIN_CREATE_CQ, nsid: 0, cdw: [qsize, 1, 0, 0, 0, 0], len: 0, prp: cq_phys })
        .map_err(|_| "create I/O CQ failed")?;
    c.submit(true, &Cmd { opcode: ADMIN_CREATE_SQ, nsid: 0, cdw: [qsize, 1 << 16 | 1, 0, 0, 0, 0], len: 0, prp: sq_phys })
        .map_err(|_| "create I/O SQ failed")?;
    let idx = NEXT_CTRL.fetch_add(1, Ordering::Relaxed);
    let vs = mmio_read32(regs, REG_VS);
    log::info!(
        "nvme{idx}: {model} (serial {serial}), NVMe {}.{}, {nn} namespaces, max transfer {} KiB",
        vs >> 16,
        (vs >> 8) & 0xff,
        c.max_xfer / 1024
    );
    let pci_dir = pci::sysfs_dir(d, "nvme");
    let sysdev = format!("{pci_dir}/nvme/nvme{idx}");
    crate::fs::sysfs::set(&sysdev, "model", &format!("{model}\n"));
    crate::fs::sysfs::set(&sysdev, "serial", &format!("{serial}\n"));
    // The active namespace list (NVMe 1.1+), else every id up to NN.
    let mut ids: alloc::vec::Vec<u32> = match c.identify(2, 0) {
        Ok(list) if vs >= 0x1_0100 => list.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).take_while(|&n| n != 0).collect(),
        _ => (1..=nn.min(1024)).collect(),
    };
    ids.retain(|&n| n <= nn);
    let mut found = alloc::vec::Vec::new();
    for nsid in ids {
        let Ok(ns) = c.identify(0, nsid) else { continue };
        let nsze = u64::from_le_bytes(ns[0..8].try_into().unwrap());
        if nsze == 0 {
            continue;
        }
        let flbas = (ns[26] & 0xf) as usize;
        let lbads = ns[128 + 4 * flbas + 2];
        if lbads != 9 {
            log::warn!("nvme{idx}n{nsid}: {}-byte blocks not supported", 1u64 << lbads);
            continue;
        }
        found.push((nsid, nsze));
    }
    let ctrl = Arc::new(Mutex::new(c));
    for (nsid, sectors) in found {
        let name = format!("nvme{idx}n{nsid}");
        let ns = Namespace { ctrl: ctrl.clone(), nsid, sectors };
        let mut disk = BlockDev::new(name, block::BLKEXT_MAJOR, block::alloc_ext_minor(), Arc::new(ns));
        disk.sysdev = Some(sysdev.clone());
        block::add_disk(Arc::new(disk));
    }
    Ok(())
}

#[test_case]
fn nvme_prps() {
    let mut list = [0u64; 512];
    assert_eq!(prps(0x10000, 512, 0x9000, &mut list), (0x10000, 0));
    assert_eq!(prps(0x10000, 4096, 0x9000, &mut list), (0x10000, 0));
    assert_eq!(prps(0x10000, 8192, 0x9000, &mut list), (0x10000, 0x11000));
    assert_eq!(prps(0x10000, 3 * 4096 - 512, 0x9000, &mut list), (0x10000, 0x9000));
    assert_eq!(&list[..2], &[0x11000, 0x12000]);
    assert_eq!(list[2], 0);
}
