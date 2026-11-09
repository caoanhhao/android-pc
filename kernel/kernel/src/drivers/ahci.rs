//! AHCI SATA (PCI class 01.06.01): ATA disks, polled, one command slot
//! per port.
//!
//! The HBA is switched to AHCI mode (GHC.AE) and left as the firmware
//! configured it otherwise. Each implemented port with a device present
//! (SSTS.DET = 3) and an ATA signature (ATAPI drives such as QEMU's -cdrom
//! are skipped) is stopped, given a command list, a received-FIS area and
//! one command table, and started again. IDENTIFY DEVICE gives the size
//! (LBA48 when supported) and model; I/O is READ/WRITE DMA EXT through a
//! bounce buffer of [`MAX_XFER`] bytes (one PRD entry), and FLUSH CACHE
//! EXT. Completion is polled on PxCI; errors (PxIS.TFES, TFD.ERR) fail
//! the request with EIO after a port restart.
//!
//! Disks are sda, sdb, ... (8:16n, partitions 8:16n+1..15 then blkext),
//! with the sysfs path Linux's libata gives them
//! (`.../0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0/block/sda`).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::pci::{self, CMD_BUS_MASTER, CMD_INTX_DISABLE, CMD_MEMORY, mmio_read32, mmio_write32, wait_until};
use crate::block::{self, Backend, BlockDev, SECTOR};
use crate::errno::*;

const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0c;
const GHC_AE: u32 = 1 << 31;
const CAP_S64A: u32 = 1 << 31;

const PX_CLB: u64 = 0x00;
const PX_FB: u64 = 0x08;
const PX_IS: u64 = 0x10;
const PX_IE: u64 = 0x14;
const PX_CMD: u64 = 0x18;
const PX_TFD: u64 = 0x20;
const PX_SIG: u64 = 0x24;
const PX_SSTS: u64 = 0x28;
const PX_SERR: u64 = 0x30;
const PX_CI: u64 = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const IS_TFES: u32 = 1 << 30;
const TFD_ERR: u32 = 1 << 0;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BSY: u32 = 1 << 7;

const SIG_ATA: u32 = 0x0000_0101;

const ATA_IDENTIFY: u8 = 0xec;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE_EXT: u8 = 0xea;

const MAX_XFER: usize = 128 * 1024;
const TIMEOUT_MS: u64 = 5000;

static NEXT_DISK: AtomicU32 = AtomicU32::new(0);

/// One port's DMA memory (one page: command list at 0, received FISes at
/// 1 KiB, the command table at 2 KiB) and the bounce buffer.
struct Port {
    regs: u64,
    mem: (u64, *mut u8),
    buf: (u64, *mut u8),
}

unsafe impl Send for Port {}

const RFIS_OFF: u64 = 1024;
const TABLE_OFF: u64 = 2048;

impl Port {
    fn r(&self, off: u64) -> u32 {
        mmio_read32(self.regs, off)
    }

    fn w(&self, off: u64, v: u32) {
        mmio_write32(self.regs, off, v)
    }

    fn stop(&self) -> bool {
        self.w(PX_CMD, self.r(PX_CMD) & !CMD_ST);
        let ok = wait_until(500, || self.r(PX_CMD) & CMD_CR == 0);
        self.w(PX_CMD, self.r(PX_CMD) & !CMD_FRE);
        ok && wait_until(500, || self.r(PX_CMD) & CMD_FR == 0)
    }

    fn start(&self) -> bool {
        self.w(PX_SERR, 0xffff_ffff);
        self.w(PX_IS, 0xffff_ffff);
        self.w(PX_CMD, self.r(PX_CMD) | CMD_FRE | CMD_POD | CMD_SUD);
        if !wait_until(1000, || self.r(PX_TFD) & (TFD_BSY | TFD_DRQ) == 0) {
            return false;
        }
        self.w(PX_CMD, self.r(PX_CMD) | CMD_ST);
        true
    }

    /// One ATA command in slot 0 with `len` bytes of data in the bounce
    /// buffer (written to the device when `write`).
    fn command(&self, cmd: u8, lba: u64, count: u16, len: usize, write: bool) -> KResult<()> {
        let mem = self.mem.1;
        unsafe {
            // Command header 0: FIS length 5 dwords, W, one PRD; table.
            let hdr = mem as *mut u32;
            let prdtl = if len > 0 { 1u32 } else { 0 };
            core::ptr::write_volatile(hdr, 5 | (write as u32) << 6 | prdtl << 16);
            core::ptr::write_volatile(hdr.add(1), 0);
            let table = self.mem.0 + TABLE_OFF;
            core::ptr::write_volatile(hdr.add(2), table as u32);
            core::ptr::write_volatile(hdr.add(3), (table >> 32) as u32);
            // Register host-to-device FIS.
            let fis = mem.add(TABLE_OFF as usize);
            core::ptr::write_bytes(fis, 0, 0x80 + 16);
            let f = [
                0x27u8,
                0x80,
                cmd,
                0,
                lba as u8,
                (lba >> 8) as u8,
                (lba >> 16) as u8,
                0x40,
                (lba >> 24) as u8,
                (lba >> 32) as u8,
                (lba >> 40) as u8,
                0,
                count as u8,
                (count >> 8) as u8,
                0,
                0,
            ];
            core::ptr::copy_nonoverlapping(f.as_ptr(), fis, f.len());
            if len > 0 {
                let prd = fis.add(0x80) as *mut u32;
                core::ptr::write_volatile(prd, self.buf.0 as u32);
                core::ptr::write_volatile(prd.add(1), (self.buf.0 >> 32) as u32);
                core::ptr::write_volatile(prd.add(3), (len as u32 - 1) & 0x3f_ffff);
            }
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        self.w(PX_IS, 0xffff_ffff);
        self.w(PX_CI, 1);
        let done = wait_until(TIMEOUT_MS, || self.r(PX_CI) & 1 == 0 || self.r(PX_IS) & IS_TFES != 0);
        let tfd = self.r(PX_TFD);
        if !done || self.r(PX_IS) & IS_TFES != 0 || tfd & TFD_ERR != 0 {
            log::warn!("ahci: command {cmd:#x} lba {lba} failed (TFD {tfd:#x}, IS {:#x})", self.r(PX_IS));
            // Clear the error: restart the port.
            self.stop();
            self.start();
            return Err(EIO);
        }
        Ok(())
    }
}

pub struct SataDisk {
    port: Mutex<Port>,
    sectors: u64,
}

impl SataDisk {
    fn rw(&self, write: bool, off: u64, len: usize, mut each: impl FnMut(*mut u8, usize, usize)) -> KResult<()> {
        if off % SECTOR != 0 || len as u64 % SECTOR != 0 || off + len as u64 > self.size() {
            return Err(EINVAL);
        }
        let p = self.port.lock();
        let mut done = 0;
        while done < len {
            let n = (len - done).min(MAX_XFER);
            if write {
                each(p.buf.1, done, n);
            }
            let lba = (off + done as u64) / SECTOR;
            let cmd = if write { ATA_WRITE_DMA_EXT } else { ATA_READ_DMA_EXT };
            p.command(cmd, lba, (n as u64 / SECTOR) as u16, n, write)?;
            if !write {
                each(p.buf.1, done, n);
            }
            done += n;
        }
        Ok(())
    }
}

impl Backend for SataDisk {
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
        self.port.lock().command(ATA_FLUSH_CACHE_EXT, 0, 0, 0, false)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An ATA IDENTIFY string: words of two bytes each, high byte first.
fn ata_string(id: &[u8]) -> String {
    let mut s = alloc::vec::Vec::with_capacity(id.len());
    for w in id.chunks(2) {
        s.push(w[1]);
        s.push(w[0]);
    }
    String::from_utf8_lossy(&s).trim().into()
}

/// Bring up every AHCI controller.
pub fn init() {
    for d in pci::devices() {
        if d.class == 0x01 && d.subclass == 0x06 && d.prog_if == 0x01 {
            if let Err(e) = probe(&d) {
                log::warn!("ahci {:?}: {e}", d.addr);
            }
        }
    }
}

fn probe(d: &pci::Device) -> Result<(), &'static str> {
    let (abar, _) = pci::map_bar(d, 5).ok_or("no ABAR (BAR 5)")?;
    d.enable(CMD_MEMORY | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    mmio_write32(abar, HBA_GHC, mmio_read32(abar, HBA_GHC) | GHC_AE);
    let cap = mmio_read32(abar, HBA_CAP);
    let pi = mmio_read32(abar, HBA_PI);
    log::info!("ahci: {:?}: {} ports (implemented {pi:#x}), 64-bit {}", d.addr, (cap & 0x1f) + 1, cap & CAP_S64A != 0);
    let mut pci_dir = None;
    for i in 0..32u32 {
        if pi & (1 << i) == 0 {
            continue;
        }
        let regs = abar + 0x100 + 0x80 * i as u64;
        let ssts = mmio_read32(regs, PX_SSTS);
        let sig = mmio_read32(regs, PX_SIG);
        if ssts & 0xf != 3 {
            continue;
        }
        if sig != SIG_ATA {
            log::info!("ahci: port {i}: signature {sig:#x} (not an ATA disk), skipped");
            continue;
        }
        let (Some(mem), Some(buf)) = (pci::dma_alloc(1), pci::dma_alloc(MAX_XFER / 4096)) else {
            return Err("out of memory");
        };
        if cap & CAP_S64A == 0 && (mem.0 >> 32 != 0 || buf.0 >> 32 != 0) {
            log::warn!("ahci: port {i}: DMA memory above 4 GiB on a 32-bit HBA");
            continue;
        }
        let port = Port { regs, mem, buf };
        if !port.stop() {
            log::warn!("ahci: port {i}: does not stop");
            continue;
        }
        port.w(PX_CLB, mem.0 as u32);
        port.w(PX_CLB + 4, (mem.0 >> 32) as u32);
        port.w(PX_FB, (mem.0 + RFIS_OFF) as u32);
        port.w(PX_FB + 4, ((mem.0 + RFIS_OFF) >> 32) as u32);
        port.w(PX_IE, 0);
        if !port.start() {
            log::warn!("ahci: port {i}: device stays busy");
            continue;
        }
        if port.command(ATA_IDENTIFY, 0, 0, 512, false).is_err() {
            continue;
        }
        let id = unsafe { core::slice::from_raw_parts(buf.1, 512) };
        let word = |n: usize| u16::from_le_bytes([id[2 * n], id[2 * n + 1]]) as u64;
        let lba48 = word(83) & (1 << 10) != 0;
        let sectors = if lba48 {
            word(100) | word(101) << 16 | word(102) << 32 | word(103) << 48
        } else {
            word(60) | word(61) << 16
        };
        let model = ata_string(&id[54..94]);
        let serial = ata_string(&id[20..40]);
        if !lba48 {
            log::warn!("ahci: port {i}: {model}: no LBA48, skipped");
            continue;
        }
        let n = NEXT_DISK.fetch_add(1, Ordering::Relaxed);
        let name = format!("sd{}", (b'a' + (n % 26) as u8) as char);
        log::info!("ahci: port {i}: {name}: {model} (serial {serial}), {sectors} sectors");
        let pci_dir = pci_dir.get_or_insert_with(|| pci::sysfs_dir(d, "ahci")).clone();
        let scsi = format!("{pci_dir}/ata{}/host{n}/target{n}:0:0/{n}:0:0:0", i + 1);
        crate::fs::sysfs::set(&scsi, "model", &format!("{model}\n"));
        crate::fs::sysfs::set(&scsi, "vendor", "ATA\n");
        let disk = SataDisk { port: Mutex::new(port), sectors };
        let mut bd = BlockDev::new(name, block::SCSI_DISK_MAJOR, n * 16, Arc::new(disk));
        bd.sysdev = Some(scsi);
        block::add_disk(Arc::new(bd));
    }
    Ok(())
}

#[test_case]
fn ahci_ata_string() {
    // "QEMU HARDDISK" as IDENTIFY stores it: byte pairs swapped, padded.
    assert_eq!(ata_string(b"EQUMH RADDSI K   "[..16].as_ref()), "QEMU HARDDISK");
}
