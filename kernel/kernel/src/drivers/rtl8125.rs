//! Realtek RTL8125 2.5 GbE (PCI 10ec:8125, Killer E3000 10ec:3000):
//! one receive and one transmit ring, polled.
//!
//! Register setup follows Linux's r8169 (`r8169_main.c`, mac versions 61
//! = RTL8125A and 63 = RTL8125B, told apart by the XID in TxConfig): the
//! chip's own init (`rtl_hw_init_8125`), a reset, the MAC address from the
//! backup registers (MAC0_BKP, else MAC0), then on bring-up the PHY (power
//! up, Linux's RTL8125B parameter writes through the GPHY OCP window,
//! soft reset, auto-negotiation advertising 10/100/1000/2500 full duplex)
//! and the MAC (`rtl_hw_start_8125*`: ePHY values, the MAC OCP registers,
//! descriptor rings, Rx/Tx enable, receive filter). No rtl_nic firmware
//! patch is loaded (Linux applies one when the file is there; the chip
//! links up without it). ASPM and clock requests stay off, as Linux leaves
//! them when the OS does not own ASPM.
//!
//! Interrupts stay masked (no MSI support in the kernel yet): like
//! virtio-net, the timer tick checks the next receive descriptor's owner
//! bit ([`rx_hint`]) and the network stack collects frames with
//! [`EthDevice::recv`]. Link state comes from the PHYstatus register,
//! polled a few times a second through [`EthDevice::carrier`]: the stack
//! turns it into IFF_LOWER_UP/IFF_RUNNING, sysfs `carrier`/`operstate`
//! and an RTM_NEWLINK, which is what Android's EthernetTracker waits for
//! before it starts DHCP. Descriptors use the classic 16-byte format
//! (Linux disables the 8125's new one too); every receive descriptor has
//! its own 2 KiB buffer, transmit copies frames into per-descriptor
//! bounce buffers (padded to 60 bytes; no checksum offload, smoltcp
//! computes checksums).
//!
//! The kernel log (framebuffer too) gets the probe, chip, XID and MAC,
//! every link change with speed and duplex, and a counter line every
//! [`STATUS_SECS`] seconds when the counters moved, so a photo of the
//! screen shows what happened on a real PC.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::pci::{self, CMD_BUS_MASTER, CMD_INTX_DISABLE, CMD_MEMORY, wait_until};
use crate::net::inet::EthDevice;

const VENDOR_REALTEK: u16 = 0x10ec;
const DEVICE_IDS: [u16; 2] = [0x8125, 0x3000];

// MAC registers (r8169's names).
const MAC0: u64 = 0x00;
const MAC4: u64 = 0x04;
const MAR0: u64 = 0x08;
const TX_DESC_LO: u64 = 0x20;
const TX_DESC_HI: u64 = 0x24;
const INT_CFG0: u64 = 0x34;
const CHIP_CMD: u64 = 0x37;
const INTR_MASK: u64 = 0x38;
const INTR_STATUS: u64 = 0x3c;
const TX_CONFIG: u64 = 0x40;
const RX_CONFIG: u64 = 0x44;
const CFG9346: u64 = 0x50;
const CONFIG1: u64 = 0x52;
const CONFIG2: u64 = 0x53;
const CONFIG3: u64 = 0x54;
const CONFIG5: u64 = 0x56;
const CSIDR: u64 = 0x64;
const CSIAR: u64 = 0x68;
const PHY_STATUS: u64 = 0x6c;
const INT_CFG1: u64 = 0x7a;
const EPHYAR: u64 = 0x80;
const TX_POLL: u64 = 0x90;
const OCPDR: u64 = 0xb0;
const GPHY_OCP: u64 = 0xb8;
const MCU: u64 = 0xd3;
const RX_MAX_SIZE: u64 = 0xda;
const CPLUS_CMD: u64 = 0xe0;
const INTR_MITIGATE: u64 = 0xe2;
const RX_DESC_LO: u64 = 0xe4;
const RX_DESC_HI: u64 = 0xe8;
const MISC: u64 = 0xf0;
const MAC0_BKP: u64 = 0x19e0;
const EEE_TXIDLE_TIMER: u64 = 0x6048;

const CMD_RESET: u8 = 0x10;
const CMD_RX_ENB: u8 = 0x08;
const CMD_TX_ENB: u8 = 0x04;
const STOP_REQ: u8 = 0x80;
const CFG9346_UNLOCK: u8 = 0xc0;
const CFG9346_LOCK: u8 = 0x00;
const RDY_TO_L23: u8 = 1 << 1;
const CLK_REQ_EN: u8 = 1 << 7;
const ASPM_EN: u8 = 1 << 0;
const NOW_IS_OOB: u8 = 1 << 7;
const RXTX_EMPTY: u8 = 0x30;
const LINK_LIST_RDY: u8 = 1 << 1;
const RXDV_GATED_EN: u32 = 1 << 19;
const OCP_FLAG: u32 = 1 << 31;

// RxConfig.
const ACCEPT_BROADCAST: u32 = 0x08;
const ACCEPT_MULTICAST: u32 = 0x04;
const ACCEPT_MY_PHYS: u32 = 0x02;
const RX_CONFIG_ACCEPT_MASK: u32 = 0x3f;
const RX_DMA_BURST: u32 = 7 << 8;
const RX_PAUSE_SLOT_ON: u32 = 1 << 11;
const RX_VLAN_8125: u32 = 3 << 22;
const RX_FETCH_DFLT_8125: u32 = 8 << 27;
// TxConfig: unlimited DMA burst, shortest inter-frame gap.
const TX_CONFIG_VAL: u32 = 7 << 8 | 3 << 24;
// CPlusCmd bits kept from the firmware (Normal_mode | INTT), no Rx VLAN
// stripping or checksum reporting.
const CPCMD_KEEP: u16 = 1 << 13 | 3;

// PHYstatus (16 bits on the 8125).
const PHY_LINK: u16 = 0x02;
const PHY_FULL_DUP: u16 = 0x01;
const PHY_10: u16 = 0x04;
const PHY_100: u16 = 0x08;
const PHY_1000: u16 = 0x10;
const PHY_2500: u16 = 0x400;
const PHY_TX_FLOW: u16 = 0x40;
const PHY_RX_FLOW: u16 = 0x20;

// Standard PHY registers through the OCP window.
const OCP_STD_PHY_BASE: u32 = 0xa400;
const MII_BMCR: u32 = 0;
const MII_ADVERTISE: u32 = 4;
const MII_CTRL1000: u32 = 9;
const BMCR_RESET: u16 = 0x8000;
const BMCR_ANENABLE: u16 = 0x1000;
const BMCR_PDOWN: u16 = 0x0800;
const BMCR_ISOLATE: u16 = 0x0400;
const BMCR_ANRESTART: u16 = 0x0200;

// Descriptor bits (first dword).
pub const DESC_OWN: u32 = 1 << 31;
pub const RING_END: u32 = 1 << 30;
pub const FIRST_FRAG: u32 = 1 << 29;
pub const LAST_FRAG: u32 = 1 << 28;
/// Receive error summary (the CRC/runt/watchdog bits say which).
pub const RX_RES: u32 = 1 << 21;
const RX_LEN_MASK: u32 = 0x3fff;
const ETH_FCS: usize = 4;
const ETH_ZLEN: usize = 60;

const BUF_SIZE: usize = 2048;
const RX_DESCS: usize = 256;
const TX_DESCS: usize = 128;
const MAX_CARDS: usize = 4;
/// Counter lines at most this often (and only when something moved).
const STATUS_SECS: u64 = 5;

/// Per card: virtual address of the next receive descriptor's opts1, for
/// the interrupt-context check in [`rx_hint`].
static RX_NEXT: [AtomicU64; MAX_CARDS] = [const { AtomicU64::new(0) }; MAX_CARDS];
static CARDS: AtomicU64 = AtomicU64::new(0);

/// Whether a received frame waits on any card (callable from the timer
/// interrupt).
pub fn rx_hint() -> bool {
    (0..CARDS.load(Ordering::Acquire) as usize).any(|i| {
        let a = RX_NEXT[i].load(Ordering::Acquire);
        a != 0 && unsafe { core::ptr::read_volatile(a as *const u32) } & DESC_OWN == 0
    })
}

/// One 16-byte descriptor (r8169's TxDesc / RxDesc).
#[repr(C)]
pub struct Desc {
    pub opts1: u32,
    pub opts2: u32,
    pub addr: u64,
}

/// What [`Rings::rx_pop`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Rx {
    Frame(Vec<u8>),
    /// RX_RES set: the descriptor's status.
    Error(u32),
    /// A frame spread over several descriptors (bigger than a buffer).
    Fragment,
}

/// Counters for the status line.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_busy: u64,
}

/// The descriptor rings and their buffers (DMA memory), without the
/// registers: what the unit tests drive, playing the NIC.
pub struct Rings {
    rx: *mut Desc,
    tx: *mut Desc,
    rx_phys: u64,
    tx_phys: u64,
    rx_n: usize,
    tx_n: usize,
    rx_buf: (u64, *mut u8),
    tx_buf: (u64, *mut u8),
    /// Next receive descriptor to look at.
    rx_cur: usize,
    /// Transmit descriptors handed out / reclaimed (free-running).
    tx_cur: usize,
    tx_dirty: usize,
    pub counters: Counters,
}

unsafe impl Send for Rings {}

impl Rings {
    pub fn new(rx_n: usize, tx_n: usize) -> Option<Rings> {
        let pages = |b: usize| b.div_ceil(crate::mm::PAGE_SIZE as usize);
        let (rx_phys, rx) = pci::dma_alloc(pages(rx_n * 16))?;
        let (tx_phys, tx) = pci::dma_alloc(pages(tx_n * 16))?;
        let rx_buf = pci::dma_alloc(pages(rx_n * BUF_SIZE))?;
        let tx_buf = pci::dma_alloc(pages(tx_n * BUF_SIZE))?;
        let mut r = Rings {
            rx: rx.cast(),
            tx: tx.cast(),
            rx_phys,
            tx_phys,
            rx_n,
            tx_n,
            rx_buf,
            tx_buf,
            rx_cur: 0,
            tx_cur: 0,
            tx_dirty: 0,
            counters: Counters::default(),
        };
        r.reset();
        Some(r)
    }

    /// Every receive descriptor owned by the NIC, the transmit ring empty.
    pub fn reset(&mut self) {
        for i in 0..self.rx_n {
            let d = self.rx_desc(i);
            unsafe { core::ptr::write_volatile(&raw mut (*d).addr, self.rx_buf.0 + (i * BUF_SIZE) as u64) };
            self.rx_give(i);
        }
        for i in 0..self.tx_n {
            let d = self.tx_desc(i);
            unsafe {
                core::ptr::write_volatile(&raw mut (*d).opts1, if i == self.tx_n - 1 { RING_END } else { 0 });
                core::ptr::write_volatile(&raw mut (*d).opts2, 0);
                core::ptr::write_volatile(&raw mut (*d).addr, self.tx_buf.0 + (i * BUF_SIZE) as u64);
            }
        }
        self.rx_cur = 0;
        self.tx_cur = 0;
        self.tx_dirty = 0;
    }

    pub fn rx_desc(&self, i: usize) -> *mut Desc {
        unsafe { self.rx.add(i) }
    }

    pub fn tx_desc(&self, i: usize) -> *mut Desc {
        unsafe { self.tx.add(i) }
    }

    pub fn rx_buf(&self, i: usize) -> *mut u8 {
        unsafe { self.rx_buf.1.add(i * BUF_SIZE) }
    }

    pub fn tx_buf(&self, i: usize) -> *mut u8 {
        unsafe { self.tx_buf.1.add(i * BUF_SIZE) }
    }

    /// Hand receive descriptor `i` (back) to the NIC (`rtl8169_mark_to_asic`).
    fn rx_give(&mut self, i: usize) {
        let d = self.rx_desc(i);
        let eor = if i == self.rx_n - 1 { RING_END } else { 0 };
        unsafe {
            core::ptr::write_volatile(&raw mut (*d).opts2, 0);
            core::sync::atomic::fence(Ordering::Release);
            core::ptr::write_volatile(&raw mut (*d).opts1, DESC_OWN | eor | BUF_SIZE as u32);
        }
    }

    /// Address of the next receive descriptor's opts1 (for [`rx_hint`]).
    pub fn rx_next_addr(&self) -> u64 {
        self.rx_desc(self.rx_cur) as u64
    }

    /// The next completed receive descriptor, given back to the NIC.
    pub fn rx_pop(&mut self) -> Option<Rx> {
        let i = self.rx_cur;
        let d = self.rx_desc(i);
        let status = unsafe { core::ptr::read_volatile(&raw const (*d).opts1) };
        if status & DESC_OWN != 0 {
            return None;
        }
        core::sync::atomic::fence(Ordering::Acquire);
        let r = if status & RX_RES != 0 {
            self.counters.rx_errors += 1;
            Rx::Error(status)
        } else if status & (FIRST_FRAG | LAST_FRAG) != FIRST_FRAG | LAST_FRAG {
            self.counters.rx_dropped += 1;
            Rx::Fragment
        } else {
            let len = ((status & RX_LEN_MASK) as usize).saturating_sub(ETH_FCS).min(BUF_SIZE);
            self.counters.rx_packets += 1;
            self.counters.rx_bytes += len as u64;
            Rx::Frame(unsafe { core::slice::from_raw_parts(self.rx_buf(i), len) }.to_vec())
        };
        self.rx_give(i);
        self.rx_cur = (i + 1) % self.rx_n;
        Some(r)
    }

    /// Reclaim the transmit descriptors the NIC finished.
    pub fn tx_reclaim(&mut self) {
        while self.tx_dirty != self.tx_cur {
            let d = self.tx_desc(self.tx_dirty % self.tx_n);
            if unsafe { core::ptr::read_volatile(&raw const (*d).opts1) } & DESC_OWN != 0 {
                break;
            }
            self.tx_dirty = self.tx_dirty.wrapping_add(1);
        }
    }

    pub fn tx_free(&self) -> usize {
        self.tx_n - self.tx_cur.wrapping_sub(self.tx_dirty)
    }

    /// Queue one frame (copied, padded to the minimum length); false when
    /// it is too big or the ring is full.
    pub fn tx_push(&mut self, frame: &[u8]) -> bool {
        self.tx_reclaim();
        if frame.len() > BUF_SIZE || self.tx_free() == 0 {
            self.counters.tx_busy += 1;
            return false;
        }
        let i = self.tx_cur % self.tx_n;
        let len = frame.len().max(ETH_ZLEN);
        let p = self.tx_buf(i);
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), p, frame.len());
            core::ptr::write_bytes(p.add(frame.len()), 0, len - frame.len());
            let d = self.tx_desc(i);
            core::ptr::write_volatile(&raw mut (*d).opts2, 0);
            core::sync::atomic::fence(Ordering::Release);
            let eor = if i == self.tx_n - 1 { RING_END } else { 0 };
            core::ptr::write_volatile(&raw mut (*d).opts1, DESC_OWN | eor | FIRST_FRAG | LAST_FRAG | len as u32);
        }
        self.tx_cur = self.tx_cur.wrapping_add(1);
        self.counters.tx_packets += 1;
        self.counters.tx_bytes += frame.len() as u64;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chip {
    /// RTL8125A (mac version 61).
    A,
    /// RTL8125B (mac version 63).
    B,
}

impl Chip {
    fn name(self) -> &'static str {
        match self {
            Chip::A => "RTL8125A",
            Chip::B => "RTL8125B",
        }
    }
}

/// Link state from PHYstatus: (up, speed in Mb/s, full duplex).
fn link_of(st: u16) -> (bool, u32, bool) {
    let speed = if st & PHY_2500 != 0 {
        2500
    } else if st & PHY_1000 != 0 {
        1000
    } else if st & PHY_100 != 0 {
        100
    } else if st & PHY_10 != 0 {
        10
    } else {
        0
    };
    (st & PHY_LINK != 0, speed, st & PHY_FULL_DUP != 0)
}

pub struct Rtl8125 {
    regs: u64,
    func: u8,
    chip: Chip,
    mac: [u8; 6],
    rings: Rings,
    card: usize,
    name: alloc::string::String,
    link: bool,
    /// Last status line: (time, counters).
    shown: (u64, Counters),
}

impl Rtl8125 {
    fn r8(&self, off: u64) -> u8 {
        unsafe { core::ptr::read_volatile((self.regs + off) as *const u8) }
    }
    fn w8(&self, off: u64, v: u8) {
        unsafe { core::ptr::write_volatile((self.regs + off) as *mut u8, v) }
    }
    fn r16(&self, off: u64) -> u16 {
        unsafe { core::ptr::read_volatile((self.regs + off) as *const u16) }
    }
    fn w16(&self, off: u64, v: u16) {
        unsafe { core::ptr::write_volatile((self.regs + off) as *mut u16, v) }
    }
    fn r32(&self, off: u64) -> u32 {
        pci::mmio_read32(self.regs, off)
    }
    fn w32(&self, off: u64, v: u32) {
        pci::mmio_write32(self.regs, off, v)
    }

    // OCP windows: MAC registers and the internal PHY.
    fn mac_ocp_write(&self, reg: u32, v: u16) {
        self.w32(OCPDR, OCP_FLAG | reg << 15 | v as u32);
    }
    fn mac_ocp_read(&self, reg: u32) -> u16 {
        self.w32(OCPDR, reg << 15);
        self.r32(OCPDR) as u16
    }
    fn mac_ocp_modify(&self, reg: u32, mask: u16, set: u16) {
        let v = self.mac_ocp_read(reg);
        self.mac_ocp_write(reg, (v & !mask) | set);
    }
    fn phy_ocp_write(&self, reg: u32, v: u16) {
        self.w32(GPHY_OCP, OCP_FLAG | reg << 15 | v as u32);
        wait_until(1, || self.r32(GPHY_OCP) & OCP_FLAG == 0);
    }
    fn phy_ocp_read(&self, reg: u32) -> u16 {
        self.w32(GPHY_OCP, reg << 15);
        wait_until(1, || self.r32(GPHY_OCP) & OCP_FLAG != 0);
        self.r32(GPHY_OCP) as u16
    }
    /// OCP address of PHY register `reg` on `page` (r8168g_mdio_*):
    /// page 0 is the standard registers, others hold 0x10..0x17.
    fn phy_addr(page: u32, reg: u32) -> u32 {
        if page == 0 { OCP_STD_PHY_BASE + reg * 2 } else { (page << 4) + (reg - 0x10) * 2 }
    }
    fn phy_read(&self, page: u32, reg: u32) -> u16 {
        self.phy_ocp_read(Self::phy_addr(page, reg))
    }
    fn phy_write(&self, page: u32, reg: u32, v: u16) {
        self.phy_ocp_write(Self::phy_addr(page, reg), v)
    }
    fn phy_modify(&self, page: u32, reg: u32, mask: u16, set: u16) {
        let v = self.phy_read(page, reg);
        self.phy_write(page, reg, (v & !mask) | set);
    }
    /// r8168g_phy_param: indirect PHY parameter through 0xa436/0xa438.
    fn phy_param(&self, parm: u16, mask: u16, v: u16) {
        self.phy_write(0xa43, 0x13, parm);
        self.phy_modify(0xa43, 0x14, mask, v);
    }

    fn ephy_write(&self, reg: u32, v: u16) {
        self.w32(EPHYAR, OCP_FLAG | (reg & 0x1f) << 16 | v as u32);
        wait_until(1, || self.r32(EPHYAR) & OCP_FLAG == 0);
        delay_us(10);
    }
    fn ephy_read(&self, reg: u32) -> u16 {
        self.w32(EPHYAR, (reg & 0x1f) << 16);
        wait_until(1, || self.r32(EPHYAR) & OCP_FLAG != 0);
        self.r32(EPHYAR) as u16
    }
    fn csi_write(&self, addr: u32, v: u32) {
        self.w32(CSIDR, v);
        self.w32(CSIAR, OCP_FLAG | (addr & 0xfff) | 0xf000 | (self.func as u32) << 16);
        wait_until(1, || self.r32(CSIAR) & OCP_FLAG == 0);
    }
    fn csi_read(&self, addr: u32) -> u32 {
        self.w32(CSIAR, (addr & 0xfff) | 0xf000 | (self.func as u32) << 16);
        if wait_until(1, || self.r32(CSIAR) & OCP_FLAG != 0) { self.r32(CSIDR) } else { !0 }
    }

    fn unlock(&self) {
        self.w8(CFG9346, CFG9346_UNLOCK);
    }
    fn lock(&self) {
        self.w8(CFG9346, CFG9346_LOCK);
    }

    fn init_rxcfg(&self) {
        let v = RX_FETCH_DFLT_8125 | RX_DMA_BURST | if self.chip == Chip::B { RX_PAUSE_SLOT_ON } else { 0 };
        self.w32(RX_CONFIG, v);
    }

    fn irq_mask_and_ack(&self) {
        self.w32(INTR_MASK, 0);
        self.w32(INTR_STATUS, !0);
        self.r8(CHIP_CMD);
    }

    fn wait_ll_fifo(&self) {
        wait_until(5, || self.r8(MCU) & LINK_LIST_RDY != 0);
    }

    /// rtl_enable_rxdvgate: gate the receive path and let the FIFOs drain.
    fn enable_rxdvgate(&self) {
        self.w32(MISC, self.r32(MISC) | RXDV_GATED_EN);
        delay_us(2000);
        if self.chip == Chip::B {
            self.w8(CHIP_CMD, self.r8(CHIP_CMD) | STOP_REQ);
        }
        wait_until(5, || self.r8(MCU) & RXTX_EMPTY == RXTX_EMPTY);
        if self.chip == Chip::B {
            wait_until(5, || self.r16(INTR_MITIGATE) & 0x0103 == 0x0103);
        }
    }

    /// rtl_hw_init_8125.
    fn hw_init(&self) {
        self.enable_rxdvgate();
        self.w8(CHIP_CMD, self.r8(CHIP_CMD) & !(CMD_TX_ENB | CMD_RX_ENB));
        delay_us(1000);
        self.w8(MCU, self.r8(MCU) & !NOW_IS_OOB);
        self.mac_ocp_modify(0xe8de, 1 << 14, 0);
        self.wait_ll_fifo();
        self.mac_ocp_write(0xc0aa, 0x07d0);
        self.mac_ocp_write(0xc0a6, 0x0150);
        self.mac_ocp_write(0xc01e, 0x5555);
        self.wait_ll_fifo();
    }

    fn hw_reset(&self) -> bool {
        self.w8(CHIP_CMD, CMD_RESET);
        wait_until(10, || self.r8(CHIP_CMD) & CMD_RESET == 0)
    }

    fn read_mac(&self, base: u64) -> [u8; 6] {
        core::array::from_fn(|i| self.r8(base + i as u64))
    }

    fn set_mac(&self) {
        let m = self.mac;
        self.unlock();
        self.w32(MAC4, u16::from_le_bytes([m[4], m[5]]) as u32);
        self.r8(CHIP_CMD);
        self.w32(MAC0, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
        self.r8(CHIP_CMD);
        self.lock();
    }

    /// The PHY: powered up, Linux's parameters, soft reset, then
    /// auto-negotiation with everything up to 2.5G advertised.
    fn phy_init(&self) {
        // genphy_soft_reset (phy_init_hw), then resume: clear power-down.
        self.phy_soft_reset();
        self.phy_modify(0, MII_BMCR, BMCR_PDOWN, 0);
        delay_us(20_000);
        if self.chip == Chip::B {
            // rtl8125b_hw_phy_config (minus the firmware patch).
            self.phy_modify(0xa44, 0x11, 0, 0x0800);
            self.phy_modify(0xac4, 0x13, 0x00f0, 0x0090);
            self.phy_modify(0xad3, 0x10, 0x0003, 0x0001);
            self.phy_write(0xb87, 0x16, 0x80f5);
            self.phy_write(0xb87, 0x17, 0x760e);
            self.phy_write(0xb87, 0x16, 0x8107);
            self.phy_write(0xb87, 0x17, 0x360e);
            self.phy_write(0xb87, 0x16, 0x8551);
            self.phy_modify(0xb87, 0x17, 0xff00, 0x0800);
            self.phy_modify(0xbf0, 0x10, 0xe000, 0xa000);
            self.phy_modify(0xbf4, 0x13, 0x0f00, 0x0300);
            for p in (0x8044..=0x807a).step_by(6) {
                self.phy_param(p, 0xffff, 0x2417);
            }
            self.phy_modify(0xa4c, 0x15, 0, 0x0040);
            self.phy_modify(0xbf8, 0x12, 0xe000, 0xa000);
            // rtl8125_legacy_force_mode, rtl8168g_disable_aldps,
            // rtl8125b_config_eee_phy.
            self.phy_modify(0xa5b, 0x12, 1 << 15, 0);
            self.phy_modify(0xa43, 0x10, 1 << 2, 0);
            self.phy_modify(0xa6d, 0x12, 0x0001, 0);
            self.phy_modify(0xa6d, 0x14, 0x0010, 0);
            self.phy_modify(0xa42, 0x14, 0x0080, 0);
            self.phy_modify(0xa4a, 0x11, 0x0200, 0);
        } else {
            self.phy_modify(0xa43, 0x10, 1 << 2, 0);
        }
        self.phy_soft_reset();
    }

    fn phy_soft_reset(&self) {
        self.phy_modify(0, MII_BMCR, BMCR_ISOLATE, BMCR_RESET);
        if !wait_until(600, || self.phy_read(0, MII_BMCR) & BMCR_RESET == 0) {
            log::warn!("rtl8125: {}: PHY reset did not finish", self.name);
        }
    }

    /// rtl822x_config_aneg + genphy: advertise 10/100 half and full with
    /// pause, 1000 full, 2.5G full; restart auto-negotiation.
    fn phy_start_aneg(&self) {
        self.phy_write(0, MII_ADVERTISE, 0x0de1);
        self.phy_modify(0, MII_CTRL1000, 0x0300, 0x0200);
        self.phy_modify(0xa5d, 0x12, 0x0180, 0x0080);
        self.phy_modify(0, MII_BMCR, BMCR_ISOLATE | BMCR_PDOWN, BMCR_ANENABLE | BMCR_ANRESTART);
    }

    /// rtl_hw_start for the 8125 family.
    fn hw_start(&self) {
        self.unlock();
        // ASPM and clock requests off (rtl_hw_aspm_clkreq_enable(false)).
        self.mac_ocp_modify(0xe092, 0x00ff, 0);
        self.w8(CONFIG2, self.r8(CONFIG2) & !CLK_REQ_EN);
        self.w8(CONFIG5, self.r8(CONFIG5) & !ASPM_EN);
        self.w16(CPLUS_CMD, self.r16(CPLUS_CMD) & CPCMD_KEEP);
        self.w16(EEE_TXIDLE_TIMER, (1500 + 14 + 0x20) as u16);
        // rtl_hw_start_8125: no interrupt coalescing.
        self.w8(INT_CFG0, 0);
        let end = if self.chip == Chip::A { 0xb00 } else { 0xa80 };
        for r in (0xa00..end).step_by(4) {
            self.w32(r, 0);
        }
        if self.chip == Chip::B {
            self.w16(INT_CFG1, 0);
        }
        // ASPM entry latency (config space 0x70f, through CSI: the
        // legacy config mechanism only reaches 256 bytes).
        let csi = self.csi_read(0x070c);
        if csi != !0 {
            self.csi_write(0x070c, (csi & 0x00ff_ffff) | 0x27 << 24);
        }
        let ephy: &[(u32, u16, u16)] = match self.chip {
            Chip::A => &[
                (0x04, 0xffff, 0xd000),
                (0x0a, 0xffff, 0x8653),
                (0x23, 0xffff, 0xab66),
                (0x20, 0xffff, 0x9455),
                (0x21, 0xffff, 0x99ff),
                (0x29, 0xffff, 0xfe04),
                (0x44, 0xffff, 0xd000),
                (0x4a, 0xffff, 0x8653),
                (0x63, 0xffff, 0xab66),
                (0x60, 0xffff, 0x9455),
                (0x61, 0xffff, 0x99ff),
                (0x69, 0xffff, 0xfe04),
            ],
            Chip::B => &[
                (0x0b, 0xffff, 0xa908),
                (0x1e, 0xffff, 0x20eb),
                (0x4b, 0xffff, 0xa908),
                (0x5e, 0xffff, 0x20eb),
                (0x22, 0x0030, 0x0020),
                (0x62, 0x0030, 0x0020),
            ],
        };
        for &(reg, mask, bits) in ephy {
            let v = self.ephy_read(reg);
            self.ephy_write(reg, (v & !mask) | bits);
        }
        self.hw_start_common();
        // rtl_enable_exit_l1.
        self.mac_ocp_modify(0xc0ac, 0, 0x1f80);
        self.w16(RX_MAX_SIZE, BUF_SIZE as u16);
        let (rx, tx) = (self.rings.rx_phys, self.rings.tx_phys);
        self.w32(TX_DESC_HI, (tx >> 32) as u32);
        self.w32(TX_DESC_LO, tx as u32);
        self.w32(RX_DESC_HI, (rx >> 32) as u32);
        self.w32(RX_DESC_LO, rx as u32);
        self.lock();
        self.r8(CHIP_CMD);
        self.w8(CHIP_CMD, CMD_TX_ENB | CMD_RX_ENB);
        self.init_rxcfg();
        self.w32(TX_CONFIG, TX_CONFIG_VAL);
        // Receive filter: own address, broadcast, every multicast (IPv6
        // neighbour discovery); no error frames, no VLAN stripping.
        self.w32(MAR0 + 4, !0);
        self.w32(MAR0, !0);
        let rc = self.r32(RX_CONFIG) & !(RX_CONFIG_ACCEPT_MASK | RX_VLAN_8125);
        self.w32(RX_CONFIG, rc | ACCEPT_BROADCAST | ACCEPT_MY_PHYS | ACCEPT_MULTICAST);
        // Polled: interrupts stay masked.
        self.irq_mask_and_ack();
    }

    /// rtl_hw_start_8125_common.
    fn hw_start_common(&self) {
        self.w8(CONFIG3, self.r8(CONFIG3) & !RDY_TO_L23);
        self.w16(0x382, 0x221b);
        self.w8(0x4500, 0);
        self.w16(0x4800, 0);
        // UPS off.
        self.mac_ocp_modify(0xd40a, 0x0010, 0);
        self.w8(CONFIG1, self.r8(CONFIG1) & !0x10);
        self.mac_ocp_write(0xc140, 0xffff);
        self.mac_ocp_write(0xc142, 0xffff);
        self.mac_ocp_modify(0xd3e2, 0x0fff, 0x03a9);
        self.mac_ocp_modify(0xd3e4, 0x00ff, 0);
        self.mac_ocp_modify(0xe860, 0, 0x0080);
        // The classic descriptor format.
        self.mac_ocp_modify(0xeb58, 0x0001, 0);
        self.mac_ocp_modify(0xe614, 0x0700, if self.chip == Chip::B { 0x0200 } else { 0x0300 });
        self.mac_ocp_modify(0xe63e, 0x0c30, if self.chip == Chip::B { 0 } else { 0x0020 });
        self.mac_ocp_modify(0xc0b4, 0, 0x000c);
        self.mac_ocp_modify(0xeb6a, 0x00ff, 0x0033);
        self.mac_ocp_modify(0xeb50, 0x03e0, 0x0040);
        self.mac_ocp_modify(0xe056, 0x00f0, 0x0030);
        self.mac_ocp_modify(0xe040, 0x1000, 0);
        self.mac_ocp_modify(0xea1c, 0x0003, 0x0001);
        self.mac_ocp_modify(0xea1c, 0x0004, 0);
        self.mac_ocp_modify(0xe0c0, 0x4f0f, 0x4403);
        self.mac_ocp_modify(0xe052, 0x0080, 0x0068);
        self.mac_ocp_modify(0xd430, 0x0fff, 0x047f);
        self.mac_ocp_modify(0xea1c, 0x0004, 0);
        self.mac_ocp_modify(0xeb54, 0, 0x0001);
        delay_us(1);
        self.mac_ocp_modify(0xeb54, 0x0001, 0);
        self.w16(0x1880, self.r16(0x1880) & !0x0030);
        self.mac_ocp_write(0xe098, 0xc302);
        wait_until(10, || self.mac_ocp_read(0xe00e) & (1 << 13) == 0);
        // EEE in the MAC (Linux's default).
        self.mac_ocp_modify(0xe040, 0, 0x0003);
        if self.chip == Chip::A {
            self.mac_ocp_modify(0xeb62, 0, 0x0006);
        }
        // rtl_disable_rxdvgate.
        self.w32(MISC, self.r32(MISC) & !RXDV_GATED_EN);
    }

    /// rtl8169_up: PHY, then a reset of the MAC (rtl8169_cleanup) and
    /// rtl_hw_start with fresh rings, then auto-negotiation.
    fn up(&mut self) {
        self.phy_init();
        self.irq_mask_and_ack();
        self.w32(RX_CONFIG, self.r32(RX_CONFIG) & !RX_CONFIG_ACCEPT_MASK);
        self.enable_rxdvgate();
        if !self.hw_reset() {
            log::warn!("rtl8125: {}: MAC reset did not finish", self.name);
        }
        self.rings.reset();
        self.hw_start();
        self.phy_start_aneg();
        RX_NEXT[self.card].store(self.rings.rx_next_addr(), Ordering::Release);
    }

    fn status_line(&mut self, force: bool) {
        let now = crate::time::monotonic_ns();
        let c = self.rings.counters;
        if !force && (c == self.shown.1 || now.saturating_sub(self.shown.0) < STATUS_SECS * 1_000_000_000) {
            return;
        }
        log::info!(
            "rtl8125: {}: rx {} pkts {} B, {} err {} drop; tx {} pkts {} B, {} busy",
            self.name,
            c.rx_packets,
            c.rx_bytes,
            c.rx_errors,
            c.rx_dropped,
            c.tx_packets,
            c.tx_bytes,
            c.tx_busy
        );
        self.shown = (now, c);
    }
}

impl EthDevice for Rtl8125 {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn driver(&self) -> &'static str {
        "r8169"
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            let r = self.rings.rx_pop();
            RX_NEXT[self.card].store(self.rings.rx_next_addr(), Ordering::Release);
            match r? {
                Rx::Frame(f) => return Some(f),
                Rx::Error(st) => {
                    if self.rings.counters.rx_errors <= 8 {
                        log::warn!("rtl8125: {}: receive error, status {st:#010x}", self.name);
                    }
                }
                Rx::Fragment => {}
            }
        }
    }

    fn can_send(&mut self) -> bool {
        self.rings.tx_reclaim();
        self.rings.tx_free() > 0
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if !self.rings.tx_push(frame) {
            return false;
        }
        self.w16(TX_POLL, 1);
        true
    }

    fn carrier(&mut self) -> Option<bool> {
        // Events are polled, not interrupts: keep the status register clear.
        let ev = self.r32(INTR_STATUS);
        if ev != 0 {
            self.w32(INTR_STATUS, ev);
        }
        let st = self.r16(PHY_STATUS);
        let (up, speed, full) = link_of(st);
        if up != self.link {
            self.link = up;
            if up {
                let physr = self.phy_read(0xa43, 0x12);
                let flow = match (st & PHY_RX_FLOW != 0, st & PHY_TX_FLOW != 0) {
                    (true, true) => ", flow control rx/tx",
                    (true, false) => ", flow control rx",
                    (false, true) => ", flow control tx",
                    _ => "",
                };
                log::info!(
                    "rtl8125: {}: link up, {speed} Mb/s {}-duplex{flow} (PHYstatus {st:#06x}, PHYSR {physr:#06x})",
                    self.name,
                    if full { "full" } else { "half" }
                );
            } else {
                log::info!("rtl8125: {}: link down (PHYstatus {st:#06x})", self.name);
            }
            self.status_line(true);
        } else {
            self.status_line(false);
        }
        Some(up)
    }
}

/// Spin for `us` microseconds.
fn delay_us(us: u64) {
    let end = crate::time::monotonic_ns() + us * 1000;
    while crate::time::monotonic_ns() < end {
        core::hint::spin_loop();
    }
}

/// Bring up every RTL8125.
pub fn init() {
    let mut found = 0;
    for d in pci::devices() {
        if d.vendor == VENDOR_REALTEK && DEVICE_IDS.contains(&d.device) {
            found += 1;
            if let Err(e) = probe(&d) {
                log::warn!("rtl8125: {:02x}:{:02x}.{}: {e}", d.addr.bus, d.addr.dev, d.addr.func);
            }
        }
    }
    if found == 0 {
        log::info!("rtl8125: no RTL8125 found");
    }
}

fn probe(d: &pci::Device) -> Result<(), &'static str> {
    let rev = d.addr.read8(8);
    log::info!(
        "rtl8125: probing {:02x}:{:02x}.{} {:04x}:{:04x} rev {rev:02x}",
        d.addr.bus,
        d.addr.dev,
        d.addr.func,
        d.vendor,
        d.device
    );
    let card = CARDS.load(Ordering::Acquire) as usize;
    if card >= MAX_CARDS {
        return Err("too many cards");
    }
    let bar = (0..6).find(|&i| matches!(d.bars[i], pci::Bar::Mem { .. })).ok_or("no memory BAR")?;
    d.enable(CMD_MEMORY | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    let (regs, _) = pci::map_bar(d, bar).ok_or("cannot map the BAR")?;
    let txconfig = pci::mmio_read32(regs, TX_CONFIG);
    if txconfig == !0 {
        return Err("registers read as all ones");
    }
    let xid = (txconfig >> 20) & 0xfcf;
    let chip = match xid & 0x7cf {
        0x641 => Chip::B,
        0x609 => Chip::A,
        _ => {
            log::warn!("rtl8125: unknown XID {xid:03x}, treating it as an RTL8125B");
            Chip::B
        }
    };
    let rings = Rings::new(RX_DESCS, TX_DESCS).ok_or("out of DMA memory")?;
    let mut dev = Rtl8125 {
        regs,
        func: d.addr.func,
        chip,
        mac: [0; 6],
        rings,
        card,
        name: alloc::format!("eth? ({:02x}:{:02x}.{})", d.addr.bus, d.addr.dev, d.addr.func),
        link: false,
        shown: (0, Counters::default()),
    };
    // rtl_init_one: Rx config, interrupts off, chip init, reset.
    dev.init_rxcfg();
    dev.irq_mask_and_ack();
    dev.hw_init();
    if !dev.hw_reset() {
        return Err("chip reset timed out");
    }
    let valid = |m: &[u8; 6]| m.iter().any(|&b| b != 0) && m[0] & 1 == 0;
    let mut mac = dev.read_mac(MAC0_BKP);
    let mut source = "MAC0_BKP";
    if !valid(&mac) {
        mac = dev.read_mac(MAC0);
        source = "MAC0";
    }
    if !valid(&mac) {
        crate::random::fill(&mut mac);
        mac[0] = (mac[0] & 0xfe) | 0x02;
        source = "random";
    }
    dev.mac = mac;
    dev.set_mac();
    let sysdev = pci::sysfs_dir(d, "r8169");
    dev.name = alloc::format!("eth{}", crate::net::inet::next_eth_index());
    log::info!(
        "rtl8125: {}: {} XID {xid:03x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ({source}), BAR{bar} {regs:#x}, polled, no firmware",
        dev.name,
        chip.name(),
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    dev.up();
    CARDS.store(card as u64 + 1, Ordering::Release);
    log::info!("rtl8125: {}: up, waiting for link (auto-negotiation)", dev.name);
    crate::net::inet::add_ethernet(Box::new(dev), sysdev);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts1(r: &Rings, i: usize, rx: bool) -> u32 {
        let d = if rx { r.rx_desc(i) } else { r.tx_desc(i) };
        unsafe { core::ptr::read_volatile(&raw const (*d).opts1) }
    }

    /// Play the NIC: complete receive descriptor `i` with `data`.
    fn nic_rx(r: &Rings, i: usize, data: &[u8], extra: u32) {
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), r.rx_buf(i), data.len()) };
        let eor = opts1(r, i, true) & RING_END;
        let d = r.rx_desc(i);
        unsafe { core::ptr::write_volatile(&raw mut (*d).opts1, eor | FIRST_FRAG | LAST_FRAG | extra | (data.len() + ETH_FCS) as u32) };
    }

    #[test_case]
    fn rtl8125_rx_ring() {
        let mut r = Rings::new(8, 4).unwrap();
        for i in 0..8 {
            let o = opts1(&r, i, true);
            assert_eq!(o, DESC_OWN | BUF_SIZE as u32 | if i == 7 { RING_END } else { 0 });
            let d = r.rx_desc(i);
            assert_eq!(unsafe { (*d).addr }, r.rx_buf.0 + (i * BUF_SIZE) as u64);
        }
        assert_eq!(r.rx_pop(), None);
        assert!(!hint(&r));
        // Three times around the ring.
        for n in 0..24usize {
            let i = n % 8;
            let frame: Vec<u8> = (0..60 + n).map(|b| (b + n) as u8).collect();
            nic_rx(&r, i, &frame, 0);
            assert!(hint(&r));
            assert_eq!(r.rx_pop(), Some(Rx::Frame(frame)));
            assert_eq!(opts1(&r, i, true), DESC_OWN | BUF_SIZE as u32 | if i == 7 { RING_END } else { 0 });
            assert_eq!(r.rx_pop(), None);
        }
        assert_eq!(r.counters.rx_packets, 24);
        // An error and a fragment are consumed and counted, not returned.
        nic_rx(&r, 0, &[0; 64], RX_RES | 1 << 19);
        assert!(matches!(r.rx_pop(), Some(Rx::Error(_))));
        nic_rx(&r, 1, &[0; 64], 0);
        let d = r.rx_desc(1);
        unsafe { core::ptr::write_volatile(&raw mut (*d).opts1, opts1(&r, 1, true) & !LAST_FRAG) };
        assert_eq!(r.rx_pop(), Some(Rx::Fragment));
        assert_eq!((r.counters.rx_errors, r.counters.rx_dropped), (1, 1));
        assert_eq!(opts1(&r, 0, true) & DESC_OWN, DESC_OWN);
    }

    fn hint(r: &Rings) -> bool {
        let opts1 = unsafe { core::ptr::read_volatile(r.rx_next_addr() as *const u32) };
        opts1 & DESC_OWN == 0
    }

    #[test_case]
    fn rtl8125_tx_ring() {
        let mut r = Rings::new(4, 4).unwrap();
        assert_eq!(r.tx_free(), 4);
        for n in 0..4 {
            assert!(r.tx_push(&[n as u8; 20]));
        }
        assert_eq!(r.tx_free(), 0);
        assert!(!r.tx_push(&[9; 20]));
        assert_eq!(r.counters.tx_busy, 1);
        for i in 0..4 {
            let o = opts1(&r, i, false);
            // Short frames are padded to 60 bytes.
            assert_eq!(o, DESC_OWN | FIRST_FRAG | LAST_FRAG | 60 | if i == 3 { RING_END } else { 0 });
            let b = unsafe { core::slice::from_raw_parts(r.tx_buf(i), 60) };
            assert!(b[..20].iter().all(|&x| x == i as u8) && b[20..].iter().all(|&x| x == 0));
        }
        // The NIC finishes the first two: they are reclaimed in order.
        for i in 0..2 {
            let d = r.tx_desc(i);
            unsafe { core::ptr::write_volatile(&raw mut (*d).opts1, opts1(&r, i, false) & !DESC_OWN) };
        }
        r.tx_reclaim();
        assert_eq!(r.tx_free(), 2);
        // Wrap: the next frames reuse descriptors 0 and 1.
        assert!(r.tx_push(&[0xaa; 1514]));
        assert_eq!(opts1(&r, 0, false), DESC_OWN | FIRST_FRAG | LAST_FRAG | 1514);
        assert!(r.tx_push(&[0xbb; 100]));
        assert!(!r.tx_push(&[0xcc; 100]));
        // Too big for a buffer.
        let d = r.tx_desc(2);
        unsafe { core::ptr::write_volatile(&raw mut (*d).opts1, opts1(&r, 2, false) & !DESC_OWN) };
        assert!(!r.tx_push(&[0; BUF_SIZE + 1]));
        assert!(r.tx_push(&[0; 64]));
        assert_eq!(r.counters.tx_packets, 7);
    }

    #[test_case]
    fn rtl8125_link_status() {
        assert_eq!(link_of(0x0000), (false, 0, false));
        assert_eq!(link_of(PHY_LINK | PHY_FULL_DUP | PHY_2500), (true, 2500, true));
        assert_eq!(link_of(PHY_LINK | PHY_FULL_DUP | PHY_1000 | PHY_TX_FLOW), (true, 1000, true));
        assert_eq!(link_of(PHY_LINK | PHY_100), (true, 100, false));
        assert_eq!(Rtl8125::phy_addr(0, MII_BMCR), 0xa400);
        assert_eq!(Rtl8125::phy_addr(0xa43, 0x12), 0xa434);
        assert_eq!(Rtl8125::phy_addr(0xb87, 0x17), 0xb87e);
    }
}
