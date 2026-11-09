//! virtio-gpu, 2D only: one polled control queue, one request in flight.
//!
//! The DRM driver (`drivers::drm`) keeps buffers in guest memory and
//! shows one on the scanout by making it a host resource
//! (RESOURCE_CREATE_2D + ATTACH_BACKING with the buffer's pages), pointing
//! the scanout at it (SET_SCANOUT) and copying it to the host on every
//! update (TRANSFER_TO_HOST_2D + RESOURCE_FLUSH), as Linux's virtio-gpu
//! does for dumb buffers. No virgl, no blobs, no cursor queue.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::{Mutex, Once};
use x86_64::PhysAddr;

use super::{Buf, F_VERSION_1, Transport, Virtqueue};
use crate::errno::*;
use crate::mm::paging::phys_to_virt;
use crate::mm::{PAGE_SIZE, frame};

const F_EDID: u64 = 1 << 1;

const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
const CMD_GET_EDID: u32 = 0x010a;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_EDID: u32 = 0x1104;

const HDR: usize = 24;
/// Request buffer: header + up to (REQ_PAGES * 4096 - 32) / 16 backing
/// entries; the response follows on its own page.
const REQ_PAGES: usize = 16;
const MAX_ENTRIES: usize = (REQ_PAGES * PAGE_SIZE as usize - 32) / 16;

/// One scanout as GET_DISPLAY_INFO reports it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scanout {
    pub width: u32,
    pub height: u32,
    pub enabled: bool,
}

struct Queue {
    transport: Transport,
    q: Virtqueue,
    /// REQ_PAGES request pages, then one response page.
    dma: PhysAddr,
}

pub struct VirtioGpu {
    queue: Mutex<Queue>,
    pub scanouts: Vec<Scanout>,
    /// Scanout 0's EDID block(s), when the host provides one.
    pub edid: Option<Vec<u8>>,
    next_res: AtomicU32,
}

static GPU: Once<VirtioGpu> = Once::new();

/// The first virtio-gpu device, once probed.
pub fn get() -> Option<&'static VirtioGpu> {
    GPU.get()
}

fn wr32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// A rectangle (x, y, width, height) in resource pixels.
pub type Rect = (u32, u32, u32, u32);

fn put_rect(b: &mut [u8], off: usize, r: Rect) {
    wr32(b, off, r.0);
    wr32(b, off + 4, r.1);
    wr32(b, off + 8, r.2);
    wr32(b, off + 12, r.3);
}

impl Queue {
    fn req(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(self.dma).as_mut_ptr::<u8>(), REQ_PAGES * PAGE_SIZE as usize) }
    }

    fn resp(&self) -> &[u8] {
        let p = phys_to_virt(self.dma + (REQ_PAGES as u64) * PAGE_SIZE);
        unsafe { core::slice::from_raw_parts(p.as_ptr::<u8>(), PAGE_SIZE as usize) }
    }

    /// Send the `len`-byte request already in the buffer (header type
    /// `ty`); returns the response type.
    fn submit(&mut self, ty: u32, len: usize, resp_len: usize) -> u32 {
        let r = self.req();
        r[..HDR].fill(0);
        wr32(r, 0, ty);
        let base = self.dma.as_u64();
        let resp = base + (REQ_PAGES as u64) * PAGE_SIZE;
        let bufs = [Buf { phys: base, len: len as u32, write: false }, Buf { phys: resp, len: resp_len as u32, write: true }];
        // Clear the response type so a failed request never reads as OK.
        unsafe { core::ptr::write_volatile(phys_to_virt(PhysAddr::new(resp)).as_mut_ptr::<u32>(), 0) };
        if self.q.submit_and_wait(&bufs).is_none() {
            return 0;
        }
        self.transport.ack_isr();
        rd32(self.resp(), 0)
    }

    fn simple(&mut self, ty: u32, len: usize) -> KResult<()> {
        match self.submit(ty, len, HDR) {
            RESP_OK_NODATA => Ok(()),
            r => {
                log::warn!("virtio-gpu: command {ty:#x} failed ({r:#x})");
                Err(EIO)
            }
        }
    }
}

impl VirtioGpu {
    /// Create a host resource of `width` x `height` pixels in virtio
    /// format `format` backed by `frames` (in order, 4 KiB each); returns
    /// its id.
    pub fn create_resource(&self, format: u32, width: u32, height: u32, frames: &[PhysAddr]) -> KResult<u32> {
        // Merge physically contiguous pages into one entry each.
        let mut runs: Vec<(u64, u32)> = Vec::new();
        for f in frames {
            match runs.last_mut() {
                Some((a, l)) if *a + *l as u64 == f.as_u64() && *l < 0x4000_0000 => *l += PAGE_SIZE as u32,
                _ => runs.push((f.as_u64(), PAGE_SIZE as u32)),
            }
        }
        if runs.len() > MAX_ENTRIES {
            return Err(ENOMEM);
        }
        let id = self.next_res.fetch_add(1, Ordering::Relaxed);
        let mut q = self.queue.lock();
        let r = q.req();
        wr32(r, HDR, id);
        wr32(r, HDR + 4, format);
        wr32(r, HDR + 8, width);
        wr32(r, HDR + 12, height);
        q.simple(CMD_RESOURCE_CREATE_2D, HDR + 16)?;
        let r = q.req();
        wr32(r, HDR, id);
        wr32(r, HDR + 4, runs.len() as u32);
        for (i, (addr, len)) in runs.iter().enumerate() {
            let o = HDR + 8 + i * 16;
            r[o..o + 8].copy_from_slice(&addr.to_le_bytes());
            wr32(r, o + 8, *len);
            wr32(r, o + 12, 0);
        }
        if let Err(e) = q.simple(CMD_RESOURCE_ATTACH_BACKING, HDR + 8 + runs.len() * 16) {
            let r = q.req();
            wr32(r, HDR, id);
            wr32(r, HDR + 4, 0);
            let _ = q.simple(CMD_RESOURCE_UNREF, HDR + 8);
            return Err(e);
        }
        Ok(id)
    }

    /// Detach the backing pages and destroy a resource.
    pub fn destroy_resource(&self, id: u32) {
        let mut q = self.queue.lock();
        for cmd in [CMD_RESOURCE_DETACH_BACKING, CMD_RESOURCE_UNREF] {
            let r = q.req();
            wr32(r, HDR, id);
            wr32(r, HDR + 4, 0);
            let _ = q.simple(cmd, HDR + 8);
        }
    }

    /// Show `rect` of resource `res` on scanout `scanout` (res 0: off).
    pub fn set_scanout(&self, scanout: u32, res: u32, rect: Rect) -> KResult<()> {
        let mut q = self.queue.lock();
        let r = q.req();
        put_rect(r, HDR, rect);
        wr32(r, HDR + 16, scanout);
        wr32(r, HDR + 20, res);
        q.simple(CMD_SET_SCANOUT, HDR + 24)
    }

    /// Copy `rect` of a resource from its backing (rows `stride` bytes
    /// apart, `rect` starting `offset` bytes in) to the host, then make
    /// the host redraw it.
    pub fn transfer_and_flush(&self, res: u32, rect: Rect, offset: u64) -> KResult<()> {
        let mut q = self.queue.lock();
        let r = q.req();
        put_rect(r, HDR, rect);
        r[HDR + 16..HDR + 24].copy_from_slice(&offset.to_le_bytes());
        wr32(r, HDR + 24, res);
        wr32(r, HDR + 28, 0);
        q.simple(CMD_TRANSFER_TO_HOST_2D, HDR + 32)?;
        let r = q.req();
        put_rect(r, HDR, rect);
        wr32(r, HDR + 16, res);
        wr32(r, HDR + 20, 0);
        q.simple(CMD_RESOURCE_FLUSH, HDR + 24)
    }
}

fn display_info(q: &mut Queue) -> Vec<Scanout> {
    let resp_len = HDR + 16 * 24;
    if q.submit(CMD_GET_DISPLAY_INFO, HDR, resp_len) != RESP_OK_DISPLAY_INFO {
        return Vec::new();
    }
    let r = q.resp();
    (0..16)
        .map(|i| {
            let o = HDR + i * 24;
            Scanout { width: rd32(r, o + 8), height: rd32(r, o + 12), enabled: rd32(r, o + 16) != 0 }
        })
        .collect()
}

fn get_edid(q: &mut Queue, scanout: u32) -> Option<Vec<u8>> {
    let r = q.req();
    wr32(r, HDR, scanout);
    wr32(r, HDR + 4, 0);
    if q.submit(CMD_GET_EDID, HDR + 8, HDR + 8 + 1024) != RESP_OK_EDID {
        return None;
    }
    let r = q.resp();
    let size = (rd32(r, HDR) as usize).min(1024);
    (size >= 128).then(|| r[HDR + 8..HDR + 8 + size].to_vec())
}

/// Bring up a virtio-gpu device (the first one only) and register the DRM
/// device on top of it.
pub fn probe(t: Transport, sysdev: alloc::string::String) {
    if GPU.get().is_some() || !t.is_modern() {
        log::warn!("virtio-gpu: ignoring extra or legacy device");
        return;
    }
    t.begin_init();
    let offered = t.device_features();
    let features = offered & (F_VERSION_1 | F_EDID);
    if features & F_VERSION_1 == 0 || !t.set_features(features) {
        log::warn!("virtio-gpu: feature negotiation failed ({offered:#x})");
        t.fail();
        return;
    }
    let Some(q) = t.setup_queue(0, 64) else {
        log::warn!("virtio-gpu: no control queue");
        t.fail();
        return;
    };
    let Some(dma) = frame::alloc_contiguous(REQ_PAGES + 1, 1) else {
        t.fail();
        return;
    };
    let num_scanouts = t.config_read32(8).clamp(1, 16);
    t.driver_ok();
    let mut queue = Queue { transport: t, q, dma };
    let mut scanouts = display_info(&mut queue);
    scanouts.truncate(num_scanouts as usize);
    if scanouts.is_empty() {
        scanouts.push(Scanout::default());
    }
    // A disabled scanout still gets a usable mode (QEMU's default size).
    for s in &mut scanouts {
        if s.width == 0 || s.height == 0 {
            (s.width, s.height) = (1280, 800);
        }
    }
    let edid = if features & F_EDID != 0 { get_edid(&mut queue, 0) } else { None };
    log::info!(
        "virtio-gpu: {} scanout(s), scanout 0 {}x{}{}, edid {}",
        scanouts.len(),
        scanouts[0].width,
        scanouts[0].height,
        if scanouts[0].enabled { "" } else { " (disabled)" },
        edid.as_ref().map_or(0, |e| e.len())
    );
    GPU.call_once(|| VirtioGpu { queue: Mutex::new(queue), scanouts, edid, next_res: AtomicU32::new(1) });
    crate::drivers::drm::register(&sysdev);
}
