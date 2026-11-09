//! virtio over PCI: the modern (virtio 1.x, capabilities + MMIO) and the
//! legacy (transitional devices, I/O port BAR 0) transports, and split
//! virtqueues.
//!
//! Drivers run their queues by polling: interrupts are suppressed
//! (VRING_AVAIL_F_NO_INTERRUPT). virtio-blk and virtio-gpu spin on the
//! used ring (`submit_and_wait`); virtio-net keeps buffers posted (`add`)
//! and collects them later (`pop_used`).
//! Ring memory comes straight from the frame allocator (physically
//! contiguous, reached through the direct map).

pub mod blk;
pub mod gpu;
pub mod input;
pub mod net;

use core::sync::atomic::{Ordering, fence};
use x86_64::PhysAddr;
use x86_64::instructions::port::Port;

use super::pci::{self, Bar, CMD_BUS_MASTER, CMD_INTX_DISABLE, CMD_IO, CMD_MEMORY};
use crate::mm::paging::{map_mmio, phys_to_virt};
use crate::mm::{PAGE_SIZE, frame};

pub const VENDOR: u16 = 0x1af4;

pub const STATUS_ACKNOWLEDGE: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;
pub const STATUS_FAILED: u8 = 0x80;

pub const F_VERSION_1: u64 = 1 << 32;

const CAP_COMMON: u8 = 1;
const CAP_NOTIFY: u8 = 2;
const CAP_ISR: u8 = 3;
const CAP_DEVICE: u8 = 4;

/// Initialise every virtio PCI device we have a driver for.
pub fn init() {
    for d in pci::devices() {
        if d.vendor != VENDOR {
            continue;
        }
        // Modern ids are 0x1040 + device type; transitional 0x1000..0x103f.
        let ty = match d.device {
            0x1000..=0x103f => match d.device {
                0x1000 => 1,
                0x1001 => 2,
                _ => 0,
            },
            0x1040..=0x107f => d.device - 0x1040,
            _ => 0,
        };
        let sysdev = sysfs_device(&d, ty);
        if ty == 1 || ty == 2 || ty == 16 || ty == 18 {
            match Transport::new(&d) {
                Some(t) if ty == 1 => net::probe(t, sysdev),
                Some(t) if ty == 16 => gpu::probe(t, sysdev),
                Some(t) if ty == 18 => input::probe(t, sysdev),
                Some(t) => blk::probe(t, sysdev),
                None => log::warn!("virtio {:?}: no usable transport", d.addr),
            }
        }
    }
}

static NEXT_VIRTIO: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// sysfs directories of a virtio PCI function and of its virtio device
/// (`devices/pci0000:00/0000:00:04.0/virtio0`); returns the latter.
fn sysfs_device(d: &pci::Device, ty: u16) -> alloc::string::String {
    use crate::fs::sysfs;
    use alloc::format;
    let pci_dir = pci::sysfs_dir(d, "virtio-pci");
    let n = NEXT_VIRTIO.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let vdir = format!("{pci_dir}/virtio{n}");
    let driver = match ty {
        1 => "net",
        2 => "blk",
        16 => "gpu",
        18 => "input",
        _ => "unknown",
    };
    let vars = format!("DRIVER=virtio_{driver}\nVIRTIO_ID={ty}\n");
    sysfs::add_device(&vdir, "virtio", "bus/virtio", vars, false);
    sysfs::symlink("bus/virtio/devices", &format!("virtio{n}"), &sysfs::relative("bus/virtio/devices", &vdir));
    vdir
}

fn mmio_of(d: &pci::Device, bar: u8, off: u32) -> Option<u64> {
    match d.bars.get(bar as usize)? {
        Bar::Mem { addr, size } => Some(map_mmio(addr + off as u64, size - off as u64).as_u64()),
        _ => None,
    }
}

/// Register access for one device.
pub enum Transport {
    Modern { common: u64, notify: u64, notify_mul: u32, isr: u64, device: u64 },
    Legacy { io: u16 },
}

impl Transport {
    /// Prefer the modern interface; fall back to legacy I/O ports.
    pub fn new(d: &pci::Device) -> Option<Transport> {
        let (mut common, mut notify, mut isr, mut device, mut mul) = (None, None, None, None, 0);
        for off in d.capabilities(pci::CAP_VENDOR) {
            let a = d.addr;
            let ty = a.read8(off + 3);
            let bar = a.read8(off + 4);
            let offset = a.read32(off + 8);
            let addr = mmio_of(d, bar, offset);
            match ty {
                CAP_COMMON if common.is_none() => common = addr,
                CAP_NOTIFY if notify.is_none() => {
                    notify = addr;
                    mul = a.read32(off + 16);
                }
                CAP_ISR if isr.is_none() => isr = addr,
                CAP_DEVICE if device.is_none() => device = addr,
                _ => {}
            }
        }
        if let (Some(common), Some(notify), Some(isr), Some(device)) = (common, notify, isr, device) {
            d.enable(CMD_MEMORY | CMD_BUS_MASTER | CMD_INTX_DISABLE);
            return Some(Transport::Modern { common, notify, notify_mul: mul, isr, device });
        }
        if let Bar::Io(io) = d.bars[0] {
            d.enable(CMD_IO | CMD_BUS_MASTER | CMD_INTX_DISABLE);
            return Some(Transport::Legacy { io });
        }
        None
    }

    pub fn is_modern(&self) -> bool {
        matches!(self, Transport::Modern { .. })
    }

    fn rd<T: Copy>(addr: u64) -> T {
        unsafe { core::ptr::read_volatile(addr as *const T) }
    }

    fn wr<T: Copy>(addr: u64, v: T) {
        unsafe { core::ptr::write_volatile(addr as *mut T, v) }
    }

    pub fn status(&self) -> u8 {
        match *self {
            Transport::Modern { common, .. } => Self::rd(common + 20),
            Transport::Legacy { io } => unsafe { Port::<u8>::new(io + 18).read() },
        }
    }

    pub fn set_status(&self, s: u8) {
        match *self {
            Transport::Modern { common, .. } => Self::wr(common + 20, s),
            Transport::Legacy { io } => unsafe { Port::<u8>::new(io + 18).write(s) },
        }
    }

    /// Reset, then ACKNOWLEDGE | DRIVER.
    pub fn begin_init(&self) {
        self.set_status(0);
        while self.status() != 0 {
            core::hint::spin_loop();
        }
        self.set_status(STATUS_ACKNOWLEDGE);
        self.set_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    }

    pub fn device_features(&self) -> u64 {
        match *self {
            Transport::Modern { common, .. } => {
                Self::wr::<u32>(common, 0);
                let lo = Self::rd::<u32>(common + 4) as u64;
                Self::wr::<u32>(common, 1);
                lo | (Self::rd::<u32>(common + 4) as u64) << 32
            }
            Transport::Legacy { io } => unsafe { Port::<u32>::new(io).read() as u64 },
        }
    }

    /// Write the driver features and (modern) check FEATURES_OK sticks.
    pub fn set_features(&self, f: u64) -> bool {
        match *self {
            Transport::Modern { common, .. } => {
                Self::wr::<u32>(common + 8, 0);
                Self::wr::<u32>(common + 12, f as u32);
                Self::wr::<u32>(common + 8, 1);
                Self::wr::<u32>(common + 12, (f >> 32) as u32);
                let s = self.status();
                self.set_status(s | STATUS_FEATURES_OK);
                self.status() & STATUS_FEATURES_OK != 0
            }
            Transport::Legacy { io } => {
                unsafe { Port::<u32>::new(io + 4).write(f as u32) };
                true
            }
        }
    }

    pub fn driver_ok(&self) {
        let s = self.status();
        self.set_status(s | STATUS_DRIVER_OK);
    }

    pub fn fail(&self) {
        let s = self.status();
        self.set_status(s | STATUS_FAILED);
    }

    /// Device-specific configuration bytes.
    pub fn config_read32(&self, off: u16) -> u32 {
        match *self {
            Transport::Modern { device, .. } => Self::rd(device + off as u64),
            // Legacy without MSI-X: device config starts at 20.
            Transport::Legacy { io } => unsafe { Port::<u32>::new(io + 20 + off).read() },
        }
    }

    pub fn config_read8(&self, off: u16) -> u8 {
        match *self {
            Transport::Modern { device, .. } => Self::rd(device + off as u64),
            Transport::Legacy { io } => unsafe { Port::<u8>::new(io + 20 + off).read() },
        }
    }

    pub fn config_write8(&self, off: u16, v: u8) {
        match *self {
            Transport::Modern { device, .. } => Self::wr(device + off as u64, v),
            Transport::Legacy { io } => unsafe { Port::<u8>::new(io + 20 + off).write(v) },
        }
    }

    pub fn config_read64(&self, off: u16) -> u64 {
        self.config_read32(off) as u64 | (self.config_read32(off + 4) as u64) << 32
    }

    /// Set up queue `idx` (at most `max` entries) and return it.
    pub fn setup_queue(&self, idx: u16, max: u16) -> Option<Virtqueue> {
        match *self {
            Transport::Modern { common, notify, notify_mul, .. } => {
                Self::wr::<u16>(common + 22, idx);
                let dev_size = Self::rd::<u16>(common + 24);
                if dev_size == 0 {
                    return None;
                }
                let size = dev_size.min(max);
                let size = 1 << (15 - size.leading_zeros()); // power of two
                Self::wr::<u16>(common + 24, size);
                Self::wr::<u16>(common + 26, 0xffff); // no MSI-X vector
                let q = Virtqueue::new(size, idx)?;
                Self::wr::<u64>(common + 32, q.desc_phys());
                Self::wr::<u64>(common + 40, q.avail_phys());
                Self::wr::<u64>(common + 48, q.used_phys());
                let noff = Self::rd::<u16>(common + 30);
                Self::wr::<u16>(common + 28, 1);
                let mut q = q;
                q.notify = Notify::Mmio(notify + noff as u64 * notify_mul as u64);
                Some(q)
            }
            Transport::Legacy { io } => unsafe {
                Port::<u16>::new(io + 14).write(idx);
                let size = Port::<u16>::new(io + 12).read();
                if size == 0 {
                    return None;
                }
                // Legacy queues have the device's size and the 4 KiB layout.
                let mut q = Virtqueue::new(size, idx)?;
                Port::<u32>::new(io + 8).write((q.desc_phys() / PAGE_SIZE) as u32);
                q.notify = Notify::Port(io + 16);
                Some(q)
            },
        }
    }

    /// Acknowledge a (polled) interrupt, if any.
    pub fn ack_isr(&self) {
        match *self {
            Transport::Modern { isr, .. } => {
                let _ = Self::rd::<u8>(isr);
            }
            Transport::Legacy { io } => unsafe {
                Port::<u8>::new(io + 19).read();
            },
        }
    }
}

enum Notify {
    None,
    Mmio(u64),
    Port(u16),
}

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const AVAIL_F_NO_INTERRUPT: u16 = 1;

/// A split virtqueue (legacy layout: the used ring on the next 4 KiB
/// boundary, which modern devices accept too).
pub struct Virtqueue {
    size: u16,
    index: u16,
    base: PhysAddr,
    pages: usize,
    used_off: u64,
    free: alloc::vec::Vec<u16>,
    /// Descriptor ids of each chain in flight, by head id (`add`).
    chains: alloc::vec::Vec<alloc::vec::Vec<u16>>,
    avail_idx: u16,
    last_used: u16,
    notify: Notify,
}

/// One buffer of a request: physical address, length, device-writable.
#[derive(Clone, Copy)]
pub struct Buf {
    pub phys: u64,
    pub len: u32,
    pub write: bool,
}

impl Virtqueue {
    fn new(size: u16, index: u16) -> Option<Virtqueue> {
        let n = size as u64;
        let used_off = (16 * n + 6 + 2 * n).next_multiple_of(PAGE_SIZE);
        let total = used_off + (6 + 8 * n).next_multiple_of(PAGE_SIZE);
        let pages = (total / PAGE_SIZE) as usize;
        let base = frame::alloc_contiguous(pages, 1)?;
        unsafe { core::ptr::write_bytes(phys_to_virt(base).as_mut_ptr::<u8>(), 0, total as usize) };
        let q = Virtqueue {
            size,
            index,
            base,
            pages,
            used_off,
            free: (0..size).rev().collect(),
            chains: (0..size).map(|_| alloc::vec::Vec::new()).collect(),
            avail_idx: 0,
            last_used: 0,
            notify: Notify::None,
        };
        q.wr16(q.avail_off(), AVAIL_F_NO_INTERRUPT);
        Some(q)
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    fn desc_phys(&self) -> u64 {
        self.base.as_u64()
    }

    fn avail_off(&self) -> u64 {
        16 * self.size as u64
    }

    fn avail_phys(&self) -> u64 {
        self.base.as_u64() + self.avail_off()
    }

    fn used_phys(&self) -> u64 {
        self.base.as_u64() + self.used_off
    }

    fn ptr(&self, off: u64) -> u64 {
        phys_to_virt(self.base).as_u64() + off
    }

    fn wr16(&self, off: u64, v: u16) {
        unsafe { core::ptr::write_volatile(self.ptr(off) as *mut u16, v) }
    }

    fn rd16(&self, off: u64) -> u16 {
        unsafe { core::ptr::read_volatile(self.ptr(off) as *const u16) }
    }

    /// Write a descriptor chain and make it available (no notify).
    fn push_chain(&mut self, bufs: &[Buf]) -> Option<alloc::vec::Vec<u16>> {
        if bufs.is_empty() || bufs.len() > self.free.len() {
            return None;
        }
        let ids: alloc::vec::Vec<u16> = (0..bufs.len()).map(|_| self.free.pop().unwrap()).collect();
        for (i, b) in bufs.iter().enumerate() {
            let d = self.ptr(16 * ids[i] as u64);
            let mut flags = if b.write { DESC_F_WRITE } else { 0 };
            let next = if i + 1 < bufs.len() {
                flags |= DESC_F_NEXT;
                ids[i + 1]
            } else {
                0
            };
            unsafe {
                core::ptr::write_volatile(d as *mut u64, b.phys);
                core::ptr::write_volatile((d + 8) as *mut u32, b.len);
                core::ptr::write_volatile((d + 12) as *mut u16, flags);
                core::ptr::write_volatile((d + 14) as *mut u16, next);
            }
        }
        let slot = self.avail_off() + 4 + 2 * (self.avail_idx % self.size) as u64;
        self.wr16(slot, ids[0]);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.wr16(self.avail_off() + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        Some(ids)
    }

    /// Tell the device new buffers are available.
    pub fn notify(&self) {
        match self.notify {
            Notify::Mmio(a) => unsafe { core::ptr::write_volatile(a as *mut u16, self.index) },
            Notify::Port(p) => unsafe { Port::<u16>::new(p).write(self.index) },
            Notify::None => {}
        }
    }

    /// The next (head id, bytes written) from the used ring, if any.
    fn next_used(&mut self) -> Option<(u16, u32)> {
        if self.rd16(self.used_off + 2) == self.last_used {
            return None;
        }
        fence(Ordering::SeqCst);
        let e = self.used_off + 4 + 8 * (self.last_used % self.size) as u64;
        let id = unsafe { core::ptr::read_volatile(self.ptr(e) as *const u32) } as u16;
        let len = unsafe { core::ptr::read_volatile((self.ptr(e) + 4) as *const u32) };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id, len))
    }

    /// Submit a descriptor chain and spin until the device returns it.
    /// Returns the number of bytes the device wrote.
    pub fn submit_and_wait(&mut self, bufs: &[Buf]) -> Option<u32> {
        let ids = self.push_chain(bufs)?;
        self.notify();
        loop {
            if let Some((_, len)) = self.next_used() {
                for id in ids {
                    self.free.push(id);
                }
                return Some(len);
            }
            core::hint::spin_loop();
        }
    }

    /// Make a chain available without waiting (call `notify` after a
    /// batch). Returns its head id, or None when the ring is full.
    pub fn add(&mut self, bufs: &[Buf]) -> Option<u16> {
        let ids = self.push_chain(bufs)?;
        let head = ids[0];
        self.chains[head as usize] = ids;
        Some(head)
    }

    /// A chain the device has finished with: (head id, bytes written).
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        let (head, len) = self.next_used()?;
        let ids = core::mem::take(&mut self.chains[head as usize]);
        self.free.extend(ids);
        Some((head, len))
    }

    /// Whether the device has returned buffers not yet popped; reads only
    /// the ring (safe from interrupt context through `used_idx_addr`).
    pub fn used_idx_addr(&self) -> u64 {
        self.ptr(self.used_off + 2)
    }

    pub fn last_used(&self) -> u16 {
        self.last_used
    }

}

impl Drop for Virtqueue {
    fn drop(&mut self) {
        for i in 0..self.pages as u64 {
            frame::free(x86_64::structures::paging::PhysFrame::containing_address(self.base + i * PAGE_SIZE));
        }
    }
}
