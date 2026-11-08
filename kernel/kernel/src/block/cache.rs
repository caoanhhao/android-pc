//! Page cache of one block device: 4 KiB pages of the device, keyed by
//! page index, in frames from the frame allocator (the kernel heap is too
//! small for disk data). Write-through, so cached pages are never dirty
//! and uncached reads (`BlockDev::read_direct`) stay coherent with it.
//!
//! Each device keeps at most [`MAX_PAGES`] pages (fewer when free memory
//! runs low); when full, the least recently used quarter is dropped.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::PhysFrame;

use super::{Backend, SECTOR};
use crate::errno::*;
use crate::mm::paging::phys_to_virt;
use crate::mm::{PAGE_SIZE, frame};

const PAGE: usize = PAGE_SIZE as usize;
pub const MAX_PAGES: usize = 8192;
/// Below this many free frames the cache stops growing.
const LOW_FRAMES: usize = 16384;

struct Entry {
    frame: PhysFrame,
    stamp: u64,
}

struct Inner {
    pages: BTreeMap<u64, Entry>,
    clock: u64,
}

pub struct Cache {
    inner: Mutex<Inner>,
}

fn page_mut(f: PhysFrame) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f.start_address()).as_mut_ptr(), PAGE) }
}

impl Inner {
    fn evict(&mut self) {
        let mut stamps: Vec<(u64, u64)> = self.pages.iter().map(|(&k, e)| (e.stamp, k)).collect();
        stamps.sort_unstable();
        for &(_, k) in stamps.iter().take(stamps.len().div_ceil(4)) {
            if let Some(e) = self.pages.remove(&k) {
                frame::free(e.frame);
            }
        }
    }

    /// The cached page `idx`, read from the device if missing.
    fn page(&mut self, dev: &dyn Backend, idx: u64) -> KResult<PhysFrame> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(e) = self.pages.get_mut(&idx) {
            e.stamp = clock;
            return Ok(e.frame);
        }
        if self.pages.len() >= MAX_PAGES || (frame::stats().1 < LOW_FRAMES && !self.pages.is_empty()) {
            self.evict();
        }
        let f = frame::alloc_zeroed().ok_or(ENOMEM)?;
        let size = dev.size();
        let start = idx * PAGE_SIZE;
        // Device sizes are whole sectors; the last page may be partial.
        let len = size.saturating_sub(start).min(PAGE_SIZE) as usize;
        if len > 0
            && let Err(e) = dev.read(start, &mut page_mut(f)[..len])
        {
            frame::free(f);
            return Err(e);
        }
        self.pages.insert(idx, Entry { frame: f, stamp: clock });
        Ok(f)
    }
}

impl Cache {
    pub fn new() -> Cache {
        Cache { inner: Mutex::new(Inner { pages: BTreeMap::new(), clock: 0 }) }
    }

    /// Read any byte range (inside the device) through the cache.
    pub fn read(&self, dev: &dyn Backend, off: u64, buf: &mut [u8]) -> KResult<()> {
        let end = off.checked_add(buf.len() as u64).ok_or(EINVAL)?;
        if end > dev.size() {
            return Err(EIO);
        }
        let mut inner = self.inner.lock();
        let mut done = 0;
        while done < buf.len() {
            let pos = off + done as u64;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE - in_page).min(buf.len() - done);
            let f = inner.page(dev, pos / PAGE_SIZE)?;
            buf[done..done + n].copy_from_slice(&page_mut(f)[in_page..in_page + n]);
            done += n;
        }
        Ok(())
    }

    /// Write through to the device; partial sectors are read-modify-written.
    pub fn write(&self, dev: &dyn Backend, off: u64, data: &[u8]) -> KResult<()> {
        let end = off.checked_add(data.len() as u64).ok_or(EINVAL)?;
        if end > dev.size() {
            return Err(ENOSPC);
        }
        let mut inner = self.inner.lock();
        let mut done = 0;
        while done < data.len() {
            let pos = off + done as u64;
            let idx = pos / PAGE_SIZE;
            let in_page = (pos % PAGE_SIZE) as usize;
            let n = (PAGE - in_page).min(data.len() - done);
            // Sector-aligned span of this page touched by the write.
            let s0 = in_page / SECTOR as usize * SECTOR as usize;
            let s1 = (in_page + n).next_multiple_of(SECTOR as usize);
            let partial = s0 != in_page || s1 != in_page + n;
            if partial || inner.pages.contains_key(&idx) {
                let f = inner.page(dev, idx)?;
                let p = page_mut(f);
                p[in_page..in_page + n].copy_from_slice(&data[done..done + n]);
                dev.write(idx * PAGE_SIZE + s0 as u64, &p[s0..s1])?;
            } else {
                dev.write(pos, &data[done..done + n])?;
            }
            done += n;
        }
        Ok(())
    }

    /// Drop every cached page (BLKFLSBUF, after the device changed).
    pub fn invalidate(&self) {
        let pages = core::mem::take(&mut self.inner.lock().pages);
        for e in pages.values() {
            frame::free(e.frame);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().pages.len()
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.invalidate();
    }
}
