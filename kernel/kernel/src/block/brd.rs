//! RAM disks (Linux's brd): `ram0` .. `ramN-1`, block major 1, created at
//! boot when the command line asks for them (`brd.rd_nr=N`, size
//! `brd.rd_size=KiB`, default 64 MiB). Pages are allocated on first write;
//! unwritten ones read as zeros.
//!
//! The AOSP init boot uses two as the devices of its tmpfs /metadata and
//! /data in the first-stage fstab (first-stage init waits for a block
//! device per non-logical fstab entry).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::sync::Arc;
use core::any::Any;
use spin::Mutex;

use super::{Backend, BlockDev};
use crate::errno::*;

pub const RAMDISK_MAJOR: u32 = 1;
const PAGE: u64 = 4096;

struct Ram {
    size: u64,
    pages: Mutex<BTreeMap<u64, Box<[u8; PAGE as usize]>>>,
}

impl Ram {
    /// Visit the pieces of [off, off + len) page by page.
    fn each(&self, off: u64, len: usize, mut f: impl FnMut(u64, usize, usize, usize)) -> KResult<()> {
        let end = off.checked_add(len as u64).filter(|&e| e <= self.size).ok_or(EIO)?;
        let mut pos = off;
        while pos < end {
            let page = pos / PAGE;
            let in_page = (pos % PAGE) as usize;
            let n = ((PAGE as usize) - in_page).min((end - pos) as usize);
            f(page, in_page, (pos - off) as usize, n);
            pos += n as u64;
        }
        Ok(())
    }
}

impl Backend for Ram {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        let pages = self.pages.lock();
        self.each(off, buf.len(), |page, at, bufoff, n| match pages.get(&page) {
            Some(p) => buf[bufoff..bufoff + n].copy_from_slice(&p[at..at + n]),
            None => buf[bufoff..bufoff + n].fill(0),
        })
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        let mut pages = self.pages.lock();
        self.each(off, data.len(), |page, at, bufoff, n| {
            let p = pages.entry(page).or_insert_with(|| Box::new([0; PAGE as usize]));
            p[at..at + n].copy_from_slice(&data[bufoff..bufoff + n]);
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn param(name: &str) -> Option<u64> {
    crate::boot::cmdline()
        .split_ascii_whitespace()
        .take_while(|w| *w != "--")
        .find_map(|w| w.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
}

/// Create the RAM disks the command line asks for.
pub fn init() {
    let count = param("brd.rd_nr").unwrap_or(0).min(16) as u32;
    let size = param("brd.rd_size").unwrap_or(64 * 1024) * 1024;
    for n in 0..count {
        let ram = Arc::new(Ram { size, pages: Mutex::new(BTreeMap::new()) });
        super::register(Arc::new(BlockDev::new(format!("ram{n}"), RAMDISK_MAJOR, n, ram)));
    }
}
