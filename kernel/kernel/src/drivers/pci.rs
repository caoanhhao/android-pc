//! PCI bus: configuration space through the legacy 0xCF8/0xCFC ports
//! (enough for QEMU's q35 and i440fx), device enumeration, BARs and
//! capability lists. BARs are used as the firmware programmed them.

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xcf8;
const CONFIG_DATA: u16 = 0xcfc;

pub const CMD_IO: u16 = 1;
pub const CMD_MEMORY: u16 = 2;
pub const CMD_BUS_MASTER: u16 = 4;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;

pub const CAP_VENDOR: u8 = 0x09;

static CONFIG_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Address {
    fn port_addr(&self, off: u8) -> u32 {
        0x8000_0000
            | (self.bus as u32) << 16
            | (self.dev as u32) << 11
            | (self.func as u32) << 8
            | (off as u32 & 0xfc)
    }

    pub fn read32(&self, off: u8) -> u32 {
        let _g = CONFIG_LOCK.lock();
        unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(self.port_addr(off));
            Port::<u32>::new(CONFIG_DATA).read()
        }
    }

    pub fn write32(&self, off: u8, val: u32) {
        let _g = CONFIG_LOCK.lock();
        unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(self.port_addr(off));
            Port::<u32>::new(CONFIG_DATA).write(val);
        }
    }

    pub fn read16(&self, off: u8) -> u16 {
        (self.read32(off) >> ((off & 2) * 8)) as u16
    }

    pub fn read8(&self, off: u8) -> u8 {
        (self.read32(off) >> ((off & 3) * 8)) as u8
    }

    pub fn write16(&self, off: u8, val: u16) {
        let shift = (off & 2) * 8;
        let old = self.read32(off) & !(0xffff << shift);
        self.write32(off, old | (val as u32) << shift);
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Bar {
    None,
    Io(u16),
    Mem { addr: u64, size: u64 },
}

#[derive(Clone, Debug)]
pub struct Device {
    pub addr: Address,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bars: [Bar; 6],
}

impl Device {
    pub fn enable(&self, bits: u16) {
        let cmd = self.addr.read16(4);
        self.addr.write16(4, cmd | bits);
    }

    /// Offsets of the capabilities with id `id`.
    pub fn capabilities(&self, id: u8) -> Vec<u8> {
        let mut out = Vec::new();
        if self.addr.read16(6) & (1 << 4) == 0 {
            return out;
        }
        let mut off = self.addr.read8(0x34) & 0xfc;
        let mut guard = 0;
        while off != 0 && guard < 48 {
            if self.addr.read8(off) == id {
                out.push(off);
            }
            off = self.addr.read8(off + 1) & 0xfc;
            guard += 1;
        }
        out
    }
}

/// Read BAR `i` (sizing it by writing all ones with decoding disabled).
fn read_bar(a: Address, i: usize) -> (Bar, bool) {
    let off = 0x10 + 4 * i as u8;
    let lo = a.read32(off);
    if lo & 1 == 1 {
        return (if lo & !3 == 0 { Bar::None } else { Bar::Io((lo & 0xfffc) as u16) }, false);
    }
    let is64 = (lo >> 1) & 3 == 2;
    let cmd = a.read16(4);
    a.write16(4, cmd & !(CMD_IO | CMD_MEMORY));
    a.write32(off, 0xffff_ffff);
    let size_lo = a.read32(off) & !0xf;
    a.write32(off, lo);
    let (base, size) = if is64 {
        let hi = a.read32(off + 4);
        a.write32(off + 4, 0xffff_ffff);
        let size_hi = a.read32(off + 4);
        a.write32(off + 4, hi);
        let mask = (size_hi as u64) << 32 | size_lo as u64;
        (((hi as u64) << 32) | (lo & !0xf) as u64, (!mask).wrapping_add(1))
    } else {
        ((lo & !0xf) as u64, (!(size_lo as u64) & 0xffff_ffff).wrapping_add(1))
    };
    a.write16(4, cmd);
    if base == 0 || size == 0 {
        return (Bar::None, is64);
    }
    (Bar::Mem { addr: base, size }, is64)
}

fn probe(a: Address) -> Option<Device> {
    let id = a.read32(0);
    if id & 0xffff == 0xffff {
        return None;
    }
    let class = a.read32(8);
    let mut bars = [Bar::None; 6];
    if a.read8(0x0e) & 0x7f == 0 {
        let mut i = 0;
        while i < 6 {
            let (b, is64) = read_bar(a, i);
            bars[i] = b;
            i += if is64 { 2 } else { 1 };
        }
    }
    Some(Device {
        addr: a,
        vendor: id as u16,
        device: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
        bars,
    })
}

static DEVICES: Mutex<Vec<Device>> = Mutex::new(Vec::new());

/// Enumerate every bus (brute force: 256 buses x 32 slots).
pub fn init() {
    let mut found = Vec::new();
    for bus in 0..=255u8 {
        for dev in 0..32u8 {
            let a = Address { bus, dev, func: 0 };
            let Some(d) = probe(a) else { continue };
            let multi = a.read8(0x0e) & 0x80 != 0;
            found.push(d);
            if multi {
                for func in 1..8 {
                    if let Some(d) = probe(Address { bus, dev, func }) {
                        found.push(d);
                    }
                }
            }
        }
    }
    for d in &found {
        log::debug!(
            "pci {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}",
            d.addr.bus, d.addr.dev, d.addr.func, d.vendor, d.device, d.class, d.subclass
        );
    }
    log::info!("pci: {} functions", found.len());
    *DEVICES.lock() = found;
}

pub fn devices() -> Vec<Device> {
    DEVICES.lock().clone()
}

/// Probe one function again (a device a quirk just made visible) and add
/// it to the list if it is new.
pub fn rescan_function(a: Address) -> Option<Device> {
    let d = probe(a)?;
    let mut devs = DEVICES.lock();
    if !devs.iter().any(|x| x.addr == a) {
        log::debug!("pci {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x} (rescanned)", a.bus, a.dev, a.func, d.vendor, d.device, d.class, d.subclass);
        devs.push(d.clone());
    }
    Some(d)
}

/// The sysfs directory of a PCI function (`devices/pci0000:00/0000:00:04.0`,
/// relative to /sys), created with its uevent (DRIVER=`driver`), ids and
/// the /sys/bus/pci/devices link.
pub fn sysfs_dir(d: &Device, driver: &str) -> alloc::string::String {
    use crate::fs::sysfs;
    use alloc::format;
    let a = d.addr;
    let slot = format!("0000:{:02x}:{:02x}.{}", a.bus, a.dev, a.func);
    let pci_dir = format!("devices/pci0000:{:02x}/{slot}", a.bus);
    let vars = format!(
        "DRIVER={driver}\nPCI_CLASS={:X}{:02X}{:02X}\nPCI_ID={:04X}:{:04X}\nPCI_SLOT_NAME={slot}\n",
        d.class, d.subclass, d.prog_if, d.vendor, d.device
    );
    sysfs::add_device(&pci_dir, "pci", "bus/pci", vars, false);
    sysfs::set(&pci_dir, "vendor", &format!("0x{:04x}\n", d.vendor));
    sysfs::set(&pci_dir, "device", &format!("0x{:04x}\n", d.device));
    sysfs::set(&pci_dir, "class", &format!("0x{:02x}{:02x}{:02x}\n", d.class, d.subclass, d.prog_if));
    sysfs::symlink("bus/pci/devices", &slot, &sysfs::relative("bus/pci/devices", &pci_dir));
    pci_dir
}

/// Zeroed, physically contiguous memory for bus-master DMA: `pages`
/// frames, returned as (physical address, pointer through the direct map).
pub fn dma_alloc(pages: usize) -> Option<(u64, *mut u8)> {
    let phys = crate::mm::frame::alloc_contiguous(pages, 1)?;
    let ptr = crate::mm::paging::phys_to_virt(phys).as_mut_ptr::<u8>();
    unsafe { core::ptr::write_bytes(ptr, 0, pages * crate::mm::PAGE_SIZE as usize) };
    Some((phys.as_u64(), ptr))
}

/// The memory BAR `i`, mapped uncached: (virtual address, size).
pub fn map_bar(d: &Device, i: usize) -> Option<(u64, u64)> {
    match d.bars.get(i)? {
        Bar::Mem { addr, size } => Some((crate::mm::paging::map_mmio(*addr, *size).as_u64(), *size)),
        _ => None,
    }
}

/// 32-bit MMIO register access (`base` from [`map_bar`]).
pub fn mmio_read32(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

pub fn mmio_write32(base: u64, off: u64, v: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
}

/// 64-bit registers as two dword accesses, low half first.
pub fn mmio_read64(base: u64, off: u64) -> u64 {
    mmio_read32(base, off) as u64 | (mmio_read32(base, off + 4) as u64) << 32
}

pub fn mmio_write64(base: u64, off: u64, v: u64) {
    mmio_write32(base, off, v as u32);
    mmio_write32(base, off + 4, (v >> 32) as u32);
}

/// Spin until `f` holds or `ms` milliseconds pass (TSC clock; usable
/// before the scheduler runs). Returns whether `f` held.
pub fn wait_until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let start = crate::time::monotonic_ns();
    loop {
        if f() {
            return true;
        }
        if crate::time::monotonic_ns().saturating_sub(start) > ms * 1_000_000 {
            return f();
        }
        core::hint::spin_loop();
    }
}
