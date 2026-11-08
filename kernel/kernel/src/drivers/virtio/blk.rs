//! virtio-blk: one polled request queue per disk, one request in flight.
//!
//! Requests go through a bounce buffer (header + status page, then up to
//! [`MAX_XFER`] bytes of data, physically contiguous), so callers may pass
//! any kernel buffer. Disks are named vda, vdb, ... in PCI order and
//! registered with the block layer (which scans their GPT).

use alloc::format;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use super::{Buf, F_VERSION_1, Transport, Virtqueue};
use crate::block::{self, Backend, BlockDev, SECTOR};
use crate::errno::*;
use crate::mm::paging::phys_to_virt;
use crate::mm::{PAGE_SIZE, frame};

const F_SIZE_MAX: u64 = 1 << 1;
const F_SEG_MAX: u64 = 1 << 2;
const F_RO: u64 = 1 << 5;
const F_BLK_SIZE: u64 = 1 << 6;
const F_FLUSH: u64 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;

/// Largest transfer per request.
const MAX_XFER: usize = 128 * 1024;
const DMA_PAGES: usize = 1 + MAX_XFER / PAGE_SIZE as usize;

static NEXT_DISK: AtomicU32 = AtomicU32::new(0);

struct State {
    transport: Transport,
    queue: Virtqueue,
    dma: PhysAddr,
}

pub struct VirtioBlk {
    state: Mutex<State>,
    capacity: u64,
    ro: bool,
    flush: bool,
}

fn dma_ptr(base: PhysAddr, off: usize) -> *mut u8 {
    unsafe { phys_to_virt(base).as_mut_ptr::<u8>().add(off) }
}

impl VirtioBlk {
    /// One request: header, optional data (device-writable for reads),
    /// status. Data is at most MAX_XFER bytes, already in the bounce buffer
    /// for writes.
    fn request(&self, st: &mut State, ty: u32, sector: u64, len: usize) -> KResult<()> {
        let hdr = dma_ptr(st.dma, 0);
        unsafe {
            core::ptr::write_unaligned(hdr as *mut u32, ty);
            core::ptr::write_unaligned(hdr.add(4) as *mut u32, 0);
            core::ptr::write_unaligned(hdr.add(8) as *mut u64, sector);
            *hdr.add(16) = 0xff;
        }
        let base = st.dma.as_u64();
        let mut bufs = alloc::vec![Buf { phys: base, len: 16, write: false }];
        if len > 0 {
            bufs.push(Buf { phys: base + PAGE_SIZE, len: len as u32, write: ty == T_IN });
        }
        bufs.push(Buf { phys: base + 16, len: 1, write: true });
        st.queue.submit_and_wait(&bufs).ok_or(EIO)?;
        st.transport.ack_isr();
        match unsafe { *hdr.add(16) } {
            0 => Ok(()),
            s => {
                log::warn!("virtio-blk: request type {ty} sector {sector} failed (status {s})");
                Err(EIO)
            }
        }
    }
}

impl Backend for VirtioBlk {
    fn size(&self) -> u64 {
        self.capacity * SECTOR
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        if off % SECTOR != 0 || buf.len() as u64 % SECTOR != 0 || off + buf.len() as u64 > self.size() {
            return Err(EINVAL);
        }
        let mut st = self.state.lock();
        let mut done = 0;
        while done < buf.len() {
            let n = (buf.len() - done).min(MAX_XFER);
            self.request(&mut st, T_IN, (off + done as u64) / SECTOR, n)?;
            let src = dma_ptr(st.dma, PAGE_SIZE as usize);
            unsafe { core::ptr::copy_nonoverlapping(src, buf[done..].as_mut_ptr(), n) };
            done += n;
        }
        Ok(())
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        if self.ro {
            return Err(EROFS);
        }
        if off % SECTOR != 0 || data.len() as u64 % SECTOR != 0 || off + data.len() as u64 > self.size() {
            return Err(EINVAL);
        }
        let mut st = self.state.lock();
        let mut done = 0;
        while done < data.len() {
            let n = (data.len() - done).min(MAX_XFER);
            let dst = dma_ptr(st.dma, PAGE_SIZE as usize);
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst, n) };
            self.request(&mut st, T_OUT, (off + done as u64) / SECTOR, n)?;
            done += n;
        }
        Ok(())
    }

    fn flush(&self) -> KResult<()> {
        if !self.flush || self.ro {
            return Ok(());
        }
        let mut st = self.state.lock();
        self.request(&mut st, T_FLUSH, 0, 0)
    }

    fn read_only(&self) -> bool {
        self.ro
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Bring up one virtio-blk device and register it as a disk.
pub fn probe(t: Transport, sysdev: alloc::string::String) {
    t.begin_init();
    let offered = t.device_features();
    let wanted = F_SIZE_MAX | F_SEG_MAX | F_RO | F_BLK_SIZE | F_FLUSH | if t.is_modern() { F_VERSION_1 } else { 0 };
    let features = offered & wanted;
    if t.is_modern() && features & F_VERSION_1 == 0 || !t.set_features(features) {
        log::warn!("virtio-blk: feature negotiation failed ({offered:#x})");
        t.fail();
        return;
    }
    let Some(queue) = t.setup_queue(0, 128) else {
        log::warn!("virtio-blk: no request queue");
        t.fail();
        return;
    };
    let Some(dma) = frame::alloc_contiguous(DMA_PAGES, 1) else {
        t.fail();
        return;
    };
    let capacity = t.config_read64(0);
    t.driver_ok();
    let idx = NEXT_DISK.fetch_add(1, Ordering::Relaxed);
    let name = format!("vd{}", (b'a' + (idx % 26) as u8) as char);
    let qsize = queue.size();
    let dev = VirtioBlk {
        state: Mutex::new(State { transport: t, queue, dma }),
        capacity,
        ro: features & F_RO != 0,
        flush: features & F_FLUSH != 0,
    };
    log::info!(
        "virtio-blk: {name}: {} sectors, queue {qsize}, {}{}",
        capacity,
        if dev.state.lock().transport.is_modern() { "modern" } else { "legacy" },
        if dev.ro { ", read-only" } else { "" }
    );
    let mut disk = BlockDev::new(name, block::VIRTBLK_MAJOR, idx * 16, Arc::new(dev));
    disk.sysdev = Some(sysdev);
    block::add_disk(Arc::new(disk));
}
