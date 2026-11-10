//! The PCIe transport of Intel's AX210 family (AX210/AX211, "gen3"), as
//! Linux's iwlwifi pcie/ does it, polled instead of interrupt driven.
//!
//! - Bring-up: own the device (PCI_OWN_SET), software reset, APM init
//!   (INIT_DONE, MAC clock ready); MSI (not MSI-X) mode; interrupt causes
//!   are read from CSR_INT, which latches them without an interrupt line.
//! - Firmware load ("context info v2"): the firmware sections are copied
//!   to DRAM and listed in the peripheral scratch area, which with the RX
//!   rings, the command queue and the image loader (IML) is described by
//!   the context info; the driver writes their addresses to CSR and
//!   starts the CPU, the ROM loads everything by DMA. The firmware
//!   configures the RX hardware itself and raises the ALIVE cause; then
//!   the driver hands it receive buffers and the ALIVE notification
//!   arrives in one of them.
//! - Receive: one queue of 4 KiB buffers; free buffers are posted as
//!   transfer descriptors (rbid, address), the device fills completion
//!   descriptors (rbid) and advances a closed-index word in DRAM. One
//!   packet per buffer: `len_n_flags`, command header, payload.
//! - Commands: queue 0 of 256-byte TFDs (up to 25 buffer pointers; the
//!   first 20 bytes in a separate small buffer), 16-bit write pointer
//!   through HBUS_TARG_WRPTR, wide command header (command, group,
//!   sequence, length, version). The response carries the sequence.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::drivers::pci::{self, mmio_read32, mmio_write32, mmio_write64, wait_until};

// CSR registers.
pub const CSR_HW_IF_CONFIG_REG: u64 = 0x000;
const CSR_INT_COALESCING: u64 = 0x004;
pub const CSR_INT: u64 = 0x008;
const CSR_INT_MASK: u64 = 0x00c;
const CSR_RESET: u64 = 0x020;
pub const CSR_GP_CNTRL: u64 = 0x024;
pub const CSR_HW_REV: u64 = 0x028;
const CSR_GIO_REG: u64 = 0x03c;
const CSR_UCODE_DRV_GP1_CLR: u64 = 0x05c;
const CSR_MBOX_SET_REG: u64 = 0x088;
pub const CSR_HW_RF_ID: u64 = 0x09c;
const CSR_MAC_SHADOW_REG_CTRL: u64 = 0x0a8;
const CSR_LTR_LAST_MSG: u64 = 0x0dc;
const CSR_GIO_CHICKEN_BITS: u64 = 0x100;
const CSR_CTXT_INFO_ADDR: u64 = 0x118;
const CSR_IML_DATA_ADDR: u64 = 0x120;
const CSR_IML_SIZE_ADDR: u64 = 0x128;
const CSR_DBG_HPET_MEM_REG: u64 = 0x240;
const CSR_DBG_LINK_PWR_MGMT_REG: u64 = 0x250;
const CSR_MAC_ADDR_BASE: u64 = 0x380;
const CSR_MSIX_HW_INT_CAUSES_AD: u64 = 0x2808;
const HBUS_TARG_MEM_RADDR: u64 = 0x40c;
const HBUS_TARG_MEM_RDAT: u64 = 0x41c;
const HBUS_TARG_PRPH_WADDR: u64 = 0x444;
const HBUS_TARG_PRPH_RADDR: u64 = 0x448;
const HBUS_TARG_PRPH_WDAT: u64 = 0x44c;
const HBUS_TARG_PRPH_RDAT: u64 = 0x450;
const HBUS_TARG_WRPTR: u64 = 0x460;
const RFH_Q0_FRBDCB_WIDX_TRG: u64 = 0x1c80;

const HW_IF_CONFIG_PCI_OWN_SET: u32 = 0x0040_0000;
const HW_IF_CONFIG_HAP_WAKE: u32 = 0x0008_0000;
const HW_IF_CONFIG_WAKE_ME: u32 = 0x0800_0000;
const CTXT_INFO_AUTO_FUNC_BOOT_ENA: u32 = 1 << 1;
const MBOX_OS_ALIVE: u32 = 1 << 5;
const GP_CNTRL_MAC_CLOCK_READY: u32 = 0x1;
const GP_CNTRL_INIT_DONE: u32 = 0x4;
const GP_CNTRL_MAC_ACCESS_REQ: u32 = 0x8;
const GP_CNTRL_GOING_TO_SLEEP: u32 = 0x10;
pub const GP_CNTRL_HW_RF_KILL_SW: u32 = 0x0800_0000;
const RESET_SW_RESET: u32 = 0x80;
const RESET_LINK_PWR_MGMT_DISABLED: u32 = 0x8000_0000;
const GIO_L0S_DISABLED: u32 = 0x2;
const GIO_CHICKEN_L1A_NO_L0S_RX: u32 = 0x0080_0000;
const HPET_MEM_REG_VAL: u32 = 0xffff_0000;
const UCODE_SW_BIT_RFKILL: u32 = 0x2;
const UCODE_DRV_GP1_BIT_CMD_BLOCKED: u32 = 0x4;
const MSIX_HW_INT_CAUSES_IML: u32 = 1 << 1;

pub const INT_ALIVE: u32 = 1 << 0;
pub const INT_RESET_DONE: u32 = 1 << 2;
pub const INT_RF_KILL: u32 = 1 << 7;
pub const INT_SW_ERR: u32 = 1 << 25;
pub const INT_HW_ERR: u32 = 1 << 29;
pub const INT_FH_RX: u32 = 1 << 31;

// Peripheral registers (through HBUS_TARG_PRPH_*); UMAC ones are offset.
const UMAC_PRPH_OFFSET: u32 = 0x30_0000;
pub const UREG_CPU_INIT_RUN: u32 = 0xa0_5c44;
pub const UREG_DOORBELL_TO_ISR6: u32 = 0xa0_5c04;
pub const UREG_DOORBELL_TO_ISR6_PNVM: u32 = 1 << 20;
const UREG_CHICK: u32 = 0xa0_5c00;
const UREG_CHICK_MSI_ENABLE: u32 = 1 << 24;
const HPM_DEBUG: u32 = 0xa0_3440;
const PERSISTENCE_BIT: u32 = 1 << 12;
const PREG_PRPH_WPROT_22000: u32 = 0xa0_4d00;
const PREG_WFPM_ACCESS: u32 = 1 << 12;
pub const UMAG_SB_CPU_1_STATUS: u32 = 0xa0_38c0;
pub const UMAG_SB_CPU_2_STATUS: u32 = 0xa0_38c4;
pub const WFPM_ARC1_PD_NOTIFICATION: u32 = 0xa0_3044;

// Context info / scratch flags.
const PRPH_SCRATCH_IMR_DEBUG_EN: u32 = 1 << 1;
const PRPH_SCRATCH_RB_SIZE_4K: u32 = 1 << 16;
const PRPH_SCRATCH_MTR_MODE: u32 = 1 << 17;
const PRPH_MTR_FORMAT_256B: u32 = 0xc_0000;

pub const RXQ_SIZE: usize = 512;
const RB_SIZE: usize = 4096;
pub const CMDQ_SIZE: usize = 128;
const TFD_SIZE: usize = 256;
const FIRST_TB_SIZE: usize = 20;
const FIRST_TB_SLOT: usize = 64;
const SEQ_RX_FRAME: u16 = 0x8000;
/// Size of the peripheral scratch area up to its DRAM map (what the ROM
/// reads without the extended FSEQ entries) and in all.
const PRPH_SCRATCH_SIZE: usize = 1724;
const PRPH_SCRATCH_DRAM_END: usize = 1660;

/// Zeroed DMA memory that does not cross a 4 GiB boundary.
pub struct Dma {
    pub phys: u64,
    ptr: *mut u8,
    pub len: usize,
}

// The memory belongs to the device and this driver only.
unsafe impl Send for Dma {}

impl Dma {
    pub fn alloc(len: usize) -> Option<Dma> {
        let pages = len.div_ceil(4096).max(1);
        for _ in 0..4 {
            let (phys, ptr) = pci::dma_alloc(pages)?;
            let end = phys + (pages * 4096) as u64 - 1;
            if phys >> 32 == end >> 32 {
                return Some(Dma { phys, ptr, len: pages * 4096 });
            }
        }
        None
    }

    pub fn from(data: &[u8]) -> Option<Dma> {
        let d = Dma::alloc(data.len())?;
        d.bytes()[..data.len()].copy_from_slice(data);
        Some(d)
    }

    #[allow(clippy::mut_from_ref)]
    pub fn bytes(&self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn r16(&self, off: usize) -> u16 {
        unsafe { core::ptr::read_volatile(self.ptr.add(off) as *const u16) }
    }

    pub fn w16(&self, off: usize, v: u16) {
        unsafe { core::ptr::write_volatile(self.ptr.add(off) as *mut u16, v) }
    }

    pub fn w32(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.ptr.add(off) as *mut u32, v) }
    }

    pub fn w64(&self, off: usize, v: u64) {
        unsafe { core::ptr::write_unaligned(self.ptr.add(off) as *mut u64, v) }
    }
}

/// A packet from the device: a command response or a notification.
pub struct Packet {
    pub group: u8,
    pub cmd: u8,
    pub seq: u16,
    pub data: Vec<u8>,
}

impl Packet {
    pub fn is(&self, group: u8, cmd: u8) -> bool {
        self.cmd == cmd && (self.group == group || group <= 1 && self.group <= 1)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CmdError {
    QueueFull,
    Timeout,
    /// The firmware reported an error (CSR_INT SW/HW error).
    FwError,
    NoMemory,
}

const TFD_QUEUE_BC_SIZE_AX210: usize = 1024;

/// A data queue: TFDs, byte-count table, first-TB buffers and one page
/// per entry for the command, header and payload.
pub struct TxQueue {
    pub id: u16,
    n: usize,
    tfds: Dma,
    bc: Dma,
    first: Dma,
    bufs: Vec<Dma>,
    pub write: u16,
    read: u16,
}

impl TxQueue {
    fn tfdq_phys(&self) -> u64 {
        self.tfds.phys
    }

    /// The firmware finished the frame at `idx` (TX response sequence).
    pub fn reclaim(&mut self, idx: u8) {
        let done = (idx as u16).wrapping_add(1).wrapping_sub(self.read) & 0xff;
        if done <= self.write.wrapping_sub(self.read) {
            self.read = self.read.wrapping_add(done);
        }
    }

    pub fn space(&self) -> bool {
        (self.write.wrapping_sub(self.read) as usize) < self.n - 2
    }
}

/// Memory the ROM reads while booting (freed never: the device keeps
/// paging sections and the scratch area for its lifetime).
struct Boot {
    ctxt: Dma,
    scratch: Dma,
    _prph_info: Dma,
    _iml: Dma,
    _sections: Vec<Dma>,
}

pub struct Trans {
    mmio: u64,
    pub hw_rev: u32,
    pub hw_rf_id: u32,
    // Receive queue.
    rx_free: Dma,
    rx_used: Dma,
    rb_stts: Dma,
    rx_bufs: Vec<Dma>,
    rx_read: usize,
    rx_write: usize,
    rx_stocked: bool,
    // Command queue.
    cmd_tfds: Dma,
    cmd_first: Dma,
    cmd_bufs: Vec<Dma>,
    cmd_write: u16,
    boot: Option<Boot>,
    _pnvm: Vec<Dma>,
    /// Packets received while waiting for a command response.
    pub pending: VecDeque<Packet>,
    pub fw_error: bool,
}

impl Trans {
    pub fn new(d: &pci::Device) -> Result<Trans, &'static str> {
        let (mmio, _) = pci::map_bar(d, 0).ok_or("no MMIO BAR 0")?;
        d.enable(pci::CMD_MEMORY | pci::CMD_BUS_MASTER | pci::CMD_INTX_DISABLE);
        let a = |n| Dma::alloc(n).ok_or("out of DMA memory");
        let mut rx_bufs = Vec::with_capacity(RXQ_SIZE);
        for _ in 0..RXQ_SIZE {
            rx_bufs.push(a(RB_SIZE)?);
        }
        let mut cmd_bufs = Vec::with_capacity(CMDQ_SIZE);
        for _ in 0..CMDQ_SIZE {
            cmd_bufs.push(a(4096)?);
        }
        let t = Trans {
            mmio,
            hw_rev: mmio_read32(mmio, CSR_HW_REV),
            hw_rf_id: 0,
            rx_free: a(RXQ_SIZE * 16)?,
            rx_used: a(RXQ_SIZE * 32)?,
            rb_stts: a(64)?,
            rx_bufs,
            rx_read: 0,
            rx_write: 0,
            rx_stocked: false,
            cmd_tfds: a(CMDQ_SIZE * TFD_SIZE)?,
            cmd_first: a(CMDQ_SIZE * FIRST_TB_SLOT)?,
            cmd_bufs,
            cmd_write: 0,
            boot: None,
            _pnvm: Vec::new(),
            pending: VecDeque::new(),
            fw_error: false,
        };
        if t.hw_rev == u32::MAX {
            return Err("device does not answer (CSR_HW_REV all ones)");
        }
        Ok(t)
    }

    pub fn r32(&self, off: u64) -> u32 {
        mmio_read32(self.mmio, off)
    }

    pub fn w32(&self, off: u64, v: u32) {
        mmio_write32(self.mmio, off, v)
    }

    fn set_bits(&self, off: u64, bits: u32) {
        self.w32(off, self.r32(off) | bits);
    }

    fn clear_bits(&self, off: u64, bits: u32) {
        self.w32(off, self.r32(off) & !bits);
    }

    /// Wait until `(reg & mask) == bits`.
    fn poll_bits(&self, off: u64, bits: u32, mask: u32, us: u64) -> bool {
        wait_until(us.div_ceil(1000), || self.r32(off) & mask == bits)
    }

    pub fn grab_nic_access(&self) -> bool {
        self.set_bits(CSR_GP_CNTRL, GP_CNTRL_MAC_ACCESS_REQ);
        self.poll_bits(CSR_GP_CNTRL, GP_CNTRL_MAC_CLOCK_READY, GP_CNTRL_MAC_CLOCK_READY | GP_CNTRL_GOING_TO_SLEEP, 15_000)
    }

    pub fn release_nic_access(&self) {
        self.clear_bits(CSR_GP_CNTRL, GP_CNTRL_MAC_ACCESS_REQ);
    }

    pub fn read_prph(&self, addr: u32) -> u32 {
        if !self.grab_nic_access() {
            return u32::MAX;
        }
        self.w32(HBUS_TARG_PRPH_RADDR, (addr & 0x00ff_ffff) | (3 << 24));
        let v = self.r32(HBUS_TARG_PRPH_RDAT);
        self.release_nic_access();
        v
    }

    pub fn write_prph(&self, addr: u32, v: u32) {
        if !self.grab_nic_access() {
            return;
        }
        self.w32(HBUS_TARG_PRPH_WADDR, (addr & 0x00ff_ffff) | (3 << 24));
        self.w32(HBUS_TARG_PRPH_WDAT, v);
        self.release_nic_access();
    }

    pub fn read_umac_prph(&self, addr: u32) -> u32 {
        self.read_prph(addr + UMAC_PRPH_OFFSET)
    }

    pub fn write_umac_prph(&self, addr: u32, v: u32) {
        self.write_prph(addr + UMAC_PRPH_OFFSET, v)
    }

    /// `n` words of device memory (the firmware's error tables).
    pub fn read_mem(&self, addr: u32, n: usize) -> Vec<u32> {
        let mut v = Vec::with_capacity(n);
        if !self.grab_nic_access() {
            return v;
        }
        self.w32(HBUS_TARG_MEM_RADDR, addr);
        for _ in 0..n {
            v.push(self.r32(HBUS_TARG_MEM_RDAT));
        }
        self.release_nic_access();
        v
    }

    /// The MAC address fused into the device (strap, else OTP).
    pub fn mac_address(&self) -> [u8; 6] {
        let flip = |a0: u32, a1: u32| {
            let (b0, b1) = (a0.to_le_bytes(), a1.to_le_bytes());
            [b0[3], b0[2], b0[1], b0[0], b1[1], b1[0]]
        };
        let strap = flip(self.r32(CSR_MAC_ADDR_BASE + 8), self.r32(CSR_MAC_ADDR_BASE + 12));
        if strap != [0; 6] && strap != [0xff; 6] && strap[0] & 1 == 0 {
            return strap;
        }
        flip(self.r32(CSR_MAC_ADDR_BASE), self.r32(CSR_MAC_ADDR_BASE + 4))
    }

    fn set_hw_ready(&self) -> bool {
        self.set_bits(CSR_HW_IF_CONFIG_REG, HW_IF_CONFIG_PCI_OWN_SET);
        let ok = self.poll_bits(CSR_HW_IF_CONFIG_REG, HW_IF_CONFIG_PCI_OWN_SET, HW_IF_CONFIG_PCI_OWN_SET, 50);
        if ok {
            self.set_bits(CSR_MBOX_SET_REG, MBOX_OS_ALIVE);
        }
        ok
    }

    /// Take ownership of the device from the platform (ME).
    fn prepare_card_hw(&self) -> Result<(), &'static str> {
        if self.set_hw_ready() {
            return Ok(());
        }
        self.set_bits(CSR_DBG_LINK_PWR_MGMT_REG, RESET_LINK_PWR_MGMT_DISABLED);
        for _ in 0..10 {
            self.set_bits(CSR_HW_IF_CONFIG_REG, HW_IF_CONFIG_WAKE_ME);
            if wait_until(150, || self.set_hw_ready()) {
                return Ok(());
            }
            wait_until(25, || false);
        }
        Err("could not take the device (PCI_OWN_SET)")
    }

    fn sw_reset(&self) -> Result<(), &'static str> {
        self.set_bits(CSR_RESET, RESET_SW_RESET);
        wait_until(6, || false);
        self.prepare_card_hw()
    }

    fn clear_persistence_bit(&self) -> Result<(), &'static str> {
        let hpm = self.read_prph(HPM_DEBUG);
        if hpm != 0xa5a5_a5a0 && hpm != u32::MAX && hpm & PERSISTENCE_BIT != 0 {
            if self.read_prph(PREG_PRPH_WPROT_22000) & PREG_WFPM_ACCESS != 0 {
                return Err("persistence bit set and write protected");
            }
            self.write_prph(HPM_DEBUG, hpm & !PERSISTENCE_BIT);
        }
        Ok(())
    }

    fn apm_init(&self) -> Result<(), &'static str> {
        self.set_bits(CSR_GIO_CHICKEN_BITS, GIO_CHICKEN_L1A_NO_L0S_RX);
        self.set_bits(CSR_DBG_HPET_MEM_REG, HPET_MEM_REG_VAL);
        self.set_bits(CSR_HW_IF_CONFIG_REG, HW_IF_CONFIG_HAP_WAKE);
        self.set_bits(CSR_GIO_REG, GIO_L0S_DISABLED);
        self.set_bits(CSR_GP_CNTRL, GP_CNTRL_INIT_DONE);
        if !self.poll_bits(CSR_GP_CNTRL, GP_CNTRL_MAC_CLOCK_READY, GP_CNTRL_MAC_CLOCK_READY, 25_000) {
            return Err("MAC clock not ready");
        }
        Ok(())
    }

    /// iwl_trans_pcie_start_hw + the gen2 NIC init.
    pub fn start_hw(&mut self) -> Result<(), &'static str> {
        self.prepare_card_hw()?;
        self.clear_persistence_bit()?;
        self.sw_reset()?;
        self.apm_init()?;
        // MSI mode, not MSI-X: causes show in CSR_INT.
        self.write_umac_prph(UREG_CHICK, UREG_CHICK_MSI_ENABLE);
        self.w32(CSR_INT_MASK, 0);
        self.w32(CSR_INT, u32::MAX);
        self.hw_rf_id = self.r32(CSR_HW_RF_ID);
        Ok(())
    }

    pub fn hw_rfkill(&self) -> bool {
        self.r32(CSR_GP_CNTRL) & GP_CNTRL_HW_RF_KILL_SW == 0
    }

    /// Context info v2: describe the rings, the scratch area with the
    /// firmware sections and the IML, then start the CPU.
    pub fn load_fw(&mut self, fw: &super::fw::Firmware, imr: bool) -> Result<(), &'static str> {
        self.w32(CSR_UCODE_DRV_GP1_CLR, UCODE_SW_BIT_RFKILL);
        self.w32(CSR_UCODE_DRV_GP1_CLR, UCODE_DRV_GP1_BIT_CMD_BLOCKED);
        self.w32(CSR_INT, u32::MAX);
        // Interrupt coalescing timer: the default 2048 us (an 8-bit field).
        self.w32(CSR_INT_COALESCING, (self.r32(CSR_INT_COALESCING) & !0xff) | 0x40);
        // Shadow registers on.
        self.set_bits(CSR_MAC_SHADOW_REG_CTRL, 0x800f_ffff);
        self.reset_queues();

        let oom = "out of DMA memory";
        let (lmac, umac, paging) = fw.split();
        if lmac.len() > 64 || umac.len() > 64 || paging.len() > 64 || fw.iml.is_empty() {
            return Err("unexpected firmware layout");
        }
        let scratch = Dma::alloc(PRPH_SCRATCH_SIZE).ok_or(oom)?;
        let mut sections = Vec::new();
        for (base, list) in [(124usize, umac), (124 + 512, lmac), (124 + 1024, paging)] {
            for (i, s) in list.iter().enumerate() {
                let d = Dma::from(s.data).ok_or(oom)?;
                scratch.w64(base + i * 8, d.phys);
                sections.push(d);
            }
        }
        let mut flags = PRPH_SCRATCH_RB_SIZE_4K | PRPH_SCRATCH_MTR_MODE | PRPH_MTR_FORMAT_256B;
        if imr {
            flags |= PRPH_SCRATCH_IMR_DEBUG_EN;
        }
        scratch.w16(0, self.hw_rev as u16);
        scratch.w16(2, 0);
        scratch.w16(4, (PRPH_SCRATCH_SIZE / 4) as u16);
        scratch.w32(8, flags);
        scratch.w32(12, 0);
        scratch.w64(48, self.rx_free.phys);
        let prph_info = Dma::alloc(4096).ok_or(oom)?;
        let ctxt = Dma::alloc(104).ok_or(oom)?;
        ctxt.w64(8, prph_info.phys);
        ctxt.w64(16, self.rb_stts.phys);
        ctxt.w64(24, prph_info.phys + 2048);
        ctxt.w64(32, prph_info.phys + 3072);
        ctxt.w64(52, self.cmd_tfds.phys);
        ctxt.w64(60, self.rx_used.phys);
        ctxt.w16(68, (CMDQ_SIZE.trailing_zeros() - 3) as u16);
        ctxt.w16(70, RXQ_SIZE.trailing_zeros() as u16);
        ctxt.w64(88, scratch.phys);
        ctxt.w32(96, PRPH_SCRATCH_DRAM_END as u32);
        let iml = Dma::from(fw.iml).ok_or(oom)?;
        log::info!(
            "iwlwifi: loading firmware: {} LMAC, {} UMAC, {} paging sections, IML {} bytes",
            lmac.len(),
            umac.len(),
            paging.len(),
            fw.iml.len()
        );

        // Kick: only the ALIVE (and RX) causes matter now.
        self.w32(CSR_INT_MASK, INT_ALIVE | INT_FH_RX);
        mmio_write64(self.mmio, CSR_CTXT_INFO_ADDR, ctxt.phys);
        mmio_write64(self.mmio, CSR_IML_DATA_ADDR, iml.phys);
        self.w32(CSR_IML_SIZE_ADDR, fw.iml.len() as u32);
        self.set_bits(CSR_HW_IF_CONFIG_REG, CTXT_INFO_AUTO_FUNC_BOOT_ENA);
        // Integrated AX210: the ROM needs the bus kept busy until the IML
        // is loaded (Linux's iwl_pcie_spin_for_iml).
        self.w32(CSR_MSIX_HW_INT_CAUSES_AD, MSIX_HW_INT_CAUSES_IML);
        self.write_umac_prph(UREG_CPU_INIT_RUN, 1);
        let iml_ok = wait_until(100, || {
            let _ = self.r32(CSR_LTR_LAST_MSG);
            self.r32(CSR_MSIX_HW_INT_CAUSES_AD) & MSIX_HW_INT_CAUSES_IML != 0
        });
        log::info!("iwlwifi: IML {} (LTR_LAST_MSG {:#x})", if iml_ok { "loaded" } else { "not signalled" }, self.r32(CSR_LTR_LAST_MSG));
        self.boot = Some(Boot { ctxt, scratch, _prph_info: prph_info, _iml: iml, _sections: sections });
        Ok(())
    }

    /// Wait for the ALIVE cause (firmware running, RX hardware set up),
    /// then post the receive buffers.
    pub fn wait_alive_irq(&mut self, ms: u64) -> bool {
        let ok = wait_until(ms, || self.r32(CSR_INT) & (INT_ALIVE | INT_SW_ERR | INT_HW_ERR) != 0);
        let causes = self.r32(CSR_INT);
        self.w32(CSR_INT, causes & INT_ALIVE);
        if causes & (INT_SW_ERR | INT_HW_ERR) != 0 {
            self.fw_error = true;
        }
        // Post the buffers even without the cause: the notification
        // cannot arrive otherwise.
        self.restock();
        ok && causes & INT_ALIVE != 0
    }

    fn reset_queues(&mut self) {
        self.rx_read = 0;
        self.rx_write = 0;
        self.rx_stocked = false;
        self.rb_stts.w32(0, 0);
        self.rx_free.bytes().fill(0);
        self.rx_used.bytes().fill(0);
        self.cmd_tfds.bytes().fill(0);
        self.cmd_write = 0;
        self.pending.clear();
        self.fw_error = false;
    }

    fn post_rb(&mut self, i: usize) {
        let o = self.rx_write * 16;
        self.rx_free.w16(o, (i + 1) as u16);
        self.rx_free.w64(o + 8, self.rx_bufs[i].phys);
        self.rx_write = (self.rx_write + 1) % RXQ_SIZE;
    }

    fn restock(&mut self) {
        if self.rx_stocked {
            return;
        }
        for i in 0..RXQ_SIZE - 1 {
            self.post_rb(i);
        }
        self.rx_stocked = true;
        self.w32(RFH_Q0_FRBDCB_WIDX_TRG, (self.rx_write & !7) as u32);
    }

    /// Collect received packets.
    pub fn poll_rx(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        if !self.rx_stocked {
            return out;
        }
        let closed = self.rb_stts.r16(0) as usize % RXQ_SIZE;
        let mut reposted = false;
        while self.rx_read != closed {
            let rbid = self.rx_used.r16(self.rx_read * 32 + 4) as usize;
            self.rx_read = (self.rx_read + 1) % RXQ_SIZE;
            if rbid == 0 || rbid > RXQ_SIZE {
                log::warn!("iwlwifi: invalid receive buffer id {rbid}");
                continue;
            }
            let b = self.rx_bufs[rbid - 1].bytes();
            let lnf = u32::from_le_bytes(b[0..4].try_into().unwrap());
            let len = (lnf & 0x3fff) as usize;
            if lnf != 0x5555_0000 && (4..=RB_SIZE - 4).contains(&len) {
                out.push(Packet {
                    cmd: b[4],
                    group: b[5],
                    seq: u16::from_le_bytes([b[6], b[7]]),
                    data: b[8..4 + len].to_vec(),
                });
            }
            self.post_rb(rbid - 1);
            reposted = true;
        }
        if reposted {
            self.w32(RFH_Q0_FRBDCB_WIDX_TRG, (self.rx_write & !7) as u32);
        }
        out
    }

    /// Firmware error causes (latched; logged by the caller).
    pub fn take_error(&mut self) -> Option<u32> {
        let c = self.r32(CSR_INT);
        if c == u32::MAX {
            self.fw_error = true;
            return Some(c);
        }
        if c & (INT_SW_ERR | INT_HW_ERR) != 0 {
            self.w32(CSR_INT, c & (INT_SW_ERR | INT_HW_ERR));
            self.fw_error = true;
            return Some(c);
        }
        if c & INT_FH_RX != 0 {
            self.w32(CSR_INT, INT_FH_RX);
        }
        None
    }

    /// Queue a command; the sequence number it will be answered with.
    fn enqueue(&mut self, group: u8, cmd: u8, version: u8, data: &[u8]) -> Result<u16, CmdError> {
        let idx = self.cmd_write as usize % CMDQ_SIZE;
        let seq = self.cmd_write & 0xff;
        let size = 8 + data.len();
        if size > 4096 {
            return Err(CmdError::NoMemory);
        }
        let buf = &self.cmd_bufs[idx];
        let b = buf.bytes();
        b[0] = cmd;
        b[1] = group;
        b[2..4].copy_from_slice(&seq.to_le_bytes());
        b[4..6].copy_from_slice(&(data.len() as u16).to_le_bytes());
        b[6] = 0;
        b[7] = version;
        b[8..size].copy_from_slice(data);
        let tb0 = size.min(FIRST_TB_SIZE);
        let first = self.cmd_first.bytes();
        first[idx * FIRST_TB_SLOT..idx * FIRST_TB_SLOT + tb0].copy_from_slice(&b[..tb0]);
        let t = idx * TFD_SIZE;
        self.cmd_tfds.bytes()[t..t + TFD_SIZE].fill(0);
        self.cmd_tfds.w16(t + 2, tb0 as u16);
        self.cmd_tfds.w64(t + 4, self.cmd_first.phys + (idx * FIRST_TB_SLOT) as u64);
        let mut n = 1;
        if size > tb0 {
            self.cmd_tfds.w16(t + 12, (size - tb0) as u16);
            self.cmd_tfds.w64(t + 14, buf.phys + tb0 as u64);
            n = 2;
        }
        self.cmd_tfds.w16(t, n);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.cmd_write = self.cmd_write.wrapping_add(1);
        self.w32(HBUS_TARG_WRPTR, self.cmd_write as u32);
        Ok(seq)
    }

    /// Send a command and wait for its response (`ms` milliseconds).
    pub fn send_cmd(&mut self, group: u8, cmd: u8, version: u8, data: &[u8], ms: u64) -> Result<Packet, CmdError> {
        if self.fw_error {
            return Err(CmdError::FwError);
        }
        let seq = self.enqueue(group, cmd, version, data)?;
        let mut resp = None;
        let mut err = false;
        wait_until(ms, || {
            if self.take_error().is_some() {
                err = true;
                return true;
            }
            for p in self.poll_rx() {
                if p.seq & SEQ_RX_FRAME == 0 && p.seq >> 8 & 0x1f == 0 && p.seq & 0xff == seq && resp.is_none() {
                    resp = Some(p);
                } else {
                    self.pending.push_back(p);
                }
            }
            resp.is_some()
        });
        if err {
            return Err(CmdError::FwError);
        }
        resp.ok_or(CmdError::Timeout)
    }

    /// Wait for a notification (group, command); others stay pending.
    pub fn wait_notif(&mut self, group: u8, cmd: u8, ms: u64) -> Option<Packet> {
        if let Some(i) = self.pending.iter().position(|p| p.is(group, cmd)) {
            return self.pending.remove(i);
        }
        let mut found = None;
        wait_until(ms, || {
            if self.take_error().is_some() {
                return true;
            }
            for p in self.poll_rx() {
                if found.is_none() && p.is(group, cmd) {
                    found = Some(p);
                } else {
                    self.pending.push_back(p);
                }
            }
            found.is_some()
        });
        found
    }

    /// PNVM payloads into DRAM, listed in the scratch area (fragmented
    /// image: an array of addresses).
    pub fn set_pnvm(&mut self, chunks: &[&[u8]]) -> Result<(), &'static str> {
        let boot = self.boot.as_ref().ok_or("no context info")?;
        let desc = Dma::alloc(4096).ok_or("out of DMA memory")?;
        let mut total = 0;
        let mut mem = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            let d = Dma::from(c).ok_or("out of DMA memory")?;
            desc.w64(i * 8, d.phys);
            total += c.len();
            mem.push(d);
        }
        boot.scratch.w64(16, desc.phys);
        boot.scratch.w32(24, total as u32);
        mem.push(desc);
        self._pnvm = mem;
        Ok(())
    }

    /// Memory for a data queue of `n` TFDs (n a power of two <= 256).
    pub fn txq_new(n: usize) -> Option<TxQueue> {
        let mut bufs = Vec::with_capacity(n);
        for _ in 0..n {
            bufs.push(Dma::alloc(4096)?);
        }
        Some(TxQueue {
            id: 0,
            n,
            tfds: Dma::alloc(n * TFD_SIZE)?,
            bc: Dma::alloc(TFD_QUEUE_BC_SIZE_AX210 * 2)?,
            first: Dma::alloc(n * FIRST_TB_SLOT)?,
            bufs,
            write: 0,
            read: 0,
        })
    }

    /// SCD_QUEUE_CONFIG_CMD (v3) adding `q` for stations `sta_mask`, TID
    /// `tid`.
    pub fn txq_add_cmd(q: &TxQueue, sta_mask: u32, tid: u8) -> Vec<u8> {
        let mut c = alloc::vec![0u8; 36];
        c[4..8].copy_from_slice(&sta_mask.to_le_bytes());
        c[8] = tid;
        c[12..16].copy_from_slice(&1u32.to_le_bytes()); // TX_QUEUE_CFG_ENABLE_QUEUE
        c[16..20].copy_from_slice(&(q.n.trailing_zeros() - 3).to_le_bytes());
        c[20..28].copy_from_slice(&q.bc.phys.to_le_bytes());
        c[28..36].copy_from_slice(&q.tfdq_phys().to_le_bytes());
        c
    }

    /// Queue a TX_CMD: `cmd` is the TX command (24 bytes) followed by the
    /// 802.11 header, `payload` the rest of the frame. False if full.
    pub fn tx(&mut self, q: &mut TxQueue, cmd: &[u8], payload: &[u8]) -> bool {
        if q.write.wrapping_sub(q.read) as usize >= q.n - 2 || cmd.len() + 4 > 2048 || payload.len() > 2048 {
            return false;
        }
        let idx = q.write as usize % q.n;
        let b = q.bufs[idx].bytes();
        b[0] = 0x1c; // TX_CMD
        b[1] = 0;
        let seq = (q.id << 8) | (idx as u16 & 0xff);
        b[2..4].copy_from_slice(&seq.to_le_bytes());
        b[4..4 + cmd.len()].copy_from_slice(cmd);
        let head = (4 + cmd.len() + 3) & !3;
        b[4 + cmd.len()..head].fill(0);
        b[2048..2048 + payload.len()].copy_from_slice(payload);
        let f = q.first.bytes();
        f[idx * FIRST_TB_SLOT..idx * FIRST_TB_SLOT + FIRST_TB_SIZE].copy_from_slice(&b[..FIRST_TB_SIZE]);
        let t = idx * TFD_SIZE;
        q.tfds.bytes()[t..t + TFD_SIZE].fill(0);
        q.tfds.w16(t + 2, FIRST_TB_SIZE as u16);
        q.tfds.w64(t + 4, q.first.phys + (idx * FIRST_TB_SLOT) as u64);
        q.tfds.w16(t + 12, (head - FIRST_TB_SIZE) as u16);
        q.tfds.w64(t + 14, q.bufs[idx].phys + FIRST_TB_SIZE as u64);
        let mut n = 2;
        if !payload.is_empty() {
            q.tfds.w16(t + 22, payload.len() as u16);
            q.tfds.w64(t + 24, q.bufs[idx].phys + 2048);
            n = 3;
        }
        q.tfds.w16(t, n);
        // Byte count: the TX command's length field, fetch chunks.
        let len = u16::from_le_bytes([cmd[0], cmd[1]]);
        let chunks = (2 + n as usize * 10).div_ceil(64) as u16 - 1;
        q.bc.w16(idx * 2, len | (chunks << 14));
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        q.write = q.write.wrapping_add(1);
        self.w32(HBUS_TARG_WRPTR, q.write as u32 | ((q.id as u32) << 16));
        true
    }

    pub fn boot_ctxt_phys(&self) -> u64 {
        self.boot.as_ref().map_or(0, |b| b.ctxt.phys)
    }
}
