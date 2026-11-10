//! GPT partition tables and the partition backend.
//!
//! Only the primary header (LBA 1) is read; CRCs are not checked. A
//! partition's number is its slot in the entry array plus one (Linux's
//! numbering), its name the UTF-16 entry name (PARTNAME, by-name links).
//! Disks without a GPT have no partitions (no MBR support).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use super::{Backend, BlockDev, SECTOR};
use crate::errno::*;

pub struct PartInfo {
    pub number: u32,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: String,
}

fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// The partitions of `disk` (empty without a GPT header).
pub fn parse(disk: &BlockDev) -> KResult<Vec<PartInfo>> {
    let sectors = disk.size() / SECTOR;
    if sectors < 3 {
        return Ok(Vec::new());
    }
    let mut hdr = [0u8; 512];
    disk.read(SECTOR, &mut hdr)?;
    if &hdr[..8] != b"EFI PART" {
        return Ok(Vec::new());
    }
    let entries_lba = le64(&hdr, 72);
    let count = le32(&hdr, 80).min(256) as usize;
    let esize = le32(&hdr, 84) as usize;
    if !(128..=4096).contains(&esize) || entries_lba >= sectors {
        return Err(EINVAL);
    }
    let mut table = vec![0u8; count * esize];
    let bytes = (table.len() as u64).min((sectors - entries_lba) * SECTOR) as usize;
    disk.read(entries_lba * SECTOR, &mut table[..bytes])?;
    let mut out = Vec::new();
    for i in 0..count {
        let e = &table[i * esize..(i + 1) * esize];
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let (first, last) = (le64(e, 32), le64(e, 40));
        if first > last || last >= sectors {
            log::warn!("gpt: {}: entry {} out of range", disk.name, i + 1);
            continue;
        }
        let units: Vec<u16> = e[56..128]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let name: String = char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect();
        out.push(PartInfo { number: i as u32 + 1, first_lba: first, last_lba: last, name });
    }
    Ok(out)
}

/// The disk GUID of `disk`'s GPT header (on-disk byte order).
pub fn disk_guid(disk: &BlockDev) -> Option<[u8; 16]> {
    let mut hdr = [0u8; 512];
    disk.read(SECTOR, &mut hdr).ok()?;
    (&hdr[..8] == b"EFI PART").then(|| hdr[56..72].try_into().unwrap())
}

/// A slice of the parent disk.
pub struct Partition {
    pub disk: Arc<dyn Backend>,
    pub start: u64,
    pub len: u64,
}

impl Backend for Partition {
    fn size(&self) -> u64 {
        self.len
    }
    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        if off + buf.len() as u64 > self.len {
            return Err(EIO);
        }
        self.disk.read(self.start + off, buf)
    }
    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        if off + data.len() as u64 > self.len {
            return Err(ENOSPC);
        }
        self.disk.write(self.start + off, data)
    }
    fn flush(&self) -> KResult<()> {
        self.disk.flush()
    }
    fn read_only(&self) -> bool {
        self.disk.read_only()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test_case]
fn gpt_parse_and_partition_io_through_cache() {
    use super::RamDisk;
    use spin::Mutex;
    // 1 MiB disk: GPT header at LBA 1, entries at LBA 2, one partition
    // "sys" covering LBAs 64..127 (slot 2, so it is partition 2).
    let mut img = alloc::vec![0u8; 1 << 20];
    img[512..520].copy_from_slice(b"EFI PART");
    img[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
    img[512 + 80..512 + 84].copy_from_slice(&4u32.to_le_bytes());
    img[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
    let e = 1024 + 128;
    img[e..e + 16].fill(0xaa);
    img[e + 32..e + 40].copy_from_slice(&64u64.to_le_bytes());
    img[e + 40..e + 48].copy_from_slice(&127u64.to_le_bytes());
    for (i, c) in "sys".encode_utf16().enumerate() {
        img[e + 56 + 2 * i..e + 58 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    img[64 * 512 + 100] = 0x42;
    let ram = Arc::new(RamDisk { data: Mutex::new(img) });
    let disk = BlockDev::new("ram0".into(), 1, 0, ram.clone());
    let parts = parse(&disk).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!((parts[0].number, parts[0].first_lba, parts[0].last_lba), (2, 64, 127));
    assert_eq!(parts[0].name, "sys");
    let part = BlockDev::new("ram0p2".into(), 1, 2, Arc::new(Partition { disk: ram.clone(), start: 64 * 512, len: 64 * 512 }));
    assert_eq!(part.size(), 32 * 1024);
    let mut b = [0u8; 3];
    part.read(99, &mut b).unwrap();
    assert_eq!(b, [0, 0x42, 0]);
    // Unaligned write-through, visible to the raw disk and after a flush.
    part.write(4090, b"crosses-a-page").unwrap();
    assert_eq!(&ram.data.lock()[64 * 512 + 4090..64 * 512 + 4104], b"crosses-a-page");
    part.cache.invalidate();
    let mut c = [0u8; 14];
    part.read(4090, &mut c).unwrap();
    assert_eq!(&c, b"crosses-a-page");
    assert!(part.cache.len() > 0);
    // Bounds.
    assert_eq!(part.read(32 * 1024 - 1, &mut b), Err(EIO));
    assert_eq!(part.write(32 * 1024, b"x"), Err(ENOSPC));
}
