//! /dev/ashmem (misc, named `ashmem<boot_id>` by ueventd): Android shared
//! memory regions. Each open file is one region: ASHMEM_SET_NAME and
//! ASHMEM_SET_SIZE before the first mmap, then mmap maps a shmem object of
//! that size (shared between every process that maps the fd).
//! ASHMEM_SET_PROT_MASK only removes rights (a shared writable mapping
//! after PROT_WRITE was dropped is EPERM). Pinning is accepted and never
//! purges anything (no memory pressure handling).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;
use crate::mm::user::{PROT_EXEC, PROT_READ, PROT_WRITE};
use crate::process::current;

pub const ASHMEM_MINOR: u32 = 126;

const NAME_LEN: usize = 256;
const NAME_PREFIX: &str = "dev/ashmem/";

const fn iow(nr: u32, size: u32) -> u32 {
    (1 << 30) | (size << 16) | (0x77 << 8) | nr
}
const fn ior(nr: u32, size: u32) -> u32 {
    (2 << 30) | (size << 16) | (0x77 << 8) | nr
}
const fn io(nr: u32) -> u32 {
    (0x77 << 8) | nr
}

const ASHMEM_SET_NAME: u32 = iow(1, NAME_LEN as u32);
const ASHMEM_GET_NAME: u32 = ior(2, NAME_LEN as u32);
const ASHMEM_SET_SIZE: u32 = iow(3, 8);
const ASHMEM_GET_SIZE: u32 = io(4);
const ASHMEM_SET_PROT_MASK: u32 = iow(5, 8);
const ASHMEM_GET_PROT_MASK: u32 = io(6);
const ASHMEM_PIN: u32 = iow(7, 8);
const ASHMEM_UNPIN: u32 = iow(8, 8);
const ASHMEM_GET_PIN_STATUS: u32 = io(9);
const ASHMEM_PURGE_ALL_CACHES: u32 = io(10);
const ASHMEM_GET_FILE_ID: u32 = ior(11, 8);

const ASHMEM_NOT_PURGED: u64 = 0;
const ASHMEM_IS_PINNED: u64 = 1;

struct Region {
    name: Vec<u8>,
    size: u64,
    prot_mask: u32,
    /// Created by the first mmap; name and size are fixed from then on.
    obj: Option<Arc<Shmem>>,
}

struct Ashmem(Mutex<Region>);

impl FileOps for Ashmem {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mut r = self.0.lock();
        let mm = current().mm();
        match cmd {
            ASHMEM_SET_NAME => {
                if r.obj.is_some() {
                    return Err(EINVAL);
                }
                drop(r);
                let name = mm.read_cstr(arg, NAME_LEN - NAME_PREFIX.len() - 1)?;
                self.0.lock().name = name;
                Ok(0)
            }
            ASHMEM_GET_NAME => {
                let mut b = if r.name.is_empty() { b"dev/ashmem".to_vec() } else { r.name.clone() };
                b.push(0);
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            ASHMEM_SET_SIZE => {
                if r.obj.is_some() {
                    return Err(EINVAL);
                }
                r.size = arg;
                Ok(0)
            }
            ASHMEM_GET_SIZE => Ok(r.size),
            ASHMEM_SET_PROT_MASK => {
                let m = arg as u32;
                if m & !r.prot_mask != 0 {
                    return Err(EINVAL);
                }
                r.prot_mask = m;
                Ok(0)
            }
            ASHMEM_GET_PROT_MASK => Ok(r.prot_mask as u64),
            ASHMEM_PIN | ASHMEM_UNPIN => {
                let mut b = [0u8; 8];
                mm.copy_in(arg, &mut b)?;
                let (off, len) = (u32::from_ne_bytes(b[..4].try_into().unwrap()), u32::from_ne_bytes(b[4..].try_into().unwrap()));
                if r.obj.is_none() || !(off as u64).is_multiple_of(PAGE_SIZE) || !(len as u64).is_multiple_of(PAGE_SIZE) {
                    return Err(EINVAL);
                }
                Ok(ASHMEM_NOT_PURGED)
            }
            ASHMEM_GET_PIN_STATUS => {
                if r.obj.is_none() {
                    return Err(EINVAL);
                }
                Ok(ASHMEM_IS_PINNED)
            }
            ASHMEM_PURGE_ALL_CACHES => Ok(0),
            ASHMEM_GET_FILE_ID => {
                let id = r.obj.as_ref().map_or(0, |o| Arc::as_ptr(o) as u64);
                mm.copy_out(arg, &id.to_ne_bytes())?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn mmap_shmem(&self, _: &OpenFile, shared_write: bool, pgoff: u64) -> KResult<Option<(Arc<Shmem>, u64)>> {
        let mut r = self.0.lock();
        if r.size == 0 {
            return Err(EINVAL);
        }
        if shared_write && r.prot_mask & PROT_WRITE == 0 {
            return Err(EPERM);
        }
        let size = r.size.div_ceil(PAGE_SIZE) * PAGE_SIZE;
        let obj = r.obj.get_or_insert_with(|| Shmem::new(size)).clone();
        Ok(Some((obj, pgoff)))
    }

    fn read(&self, _: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let r = self.0.lock();
        let Some(obj) = &r.obj else {
            return Ok(0);
        };
        let n = obj.read(*pos, buf);
        *pos += n as u64;
        Ok(n)
    }

    fn lseek_ok(&self) -> bool {
        true
    }

    fn link_name(&self) -> Option<Vec<u8>> {
        let r = self.0.lock();
        let name = if r.name.is_empty() { String::from("dev/ashmem") } else { format!("{NAME_PREFIX}{}", String::from_utf8_lossy(&r.name)) };
        Some(format!("/{name} (deleted)").into_bytes())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(Ashmem(Mutex::new(Region {
        name: Vec::new(),
        size: 0,
        prot_mask: PROT_READ | PROT_WRITE | PROT_EXEC,
        obj: None,
    }))))
}
