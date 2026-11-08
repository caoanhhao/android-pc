//! Android binder IPC, ABI-compatible with Linux binder (protocol
//! version 8, 64-bit), and binderfs.
//!
//! - Devices: every binder device is a context with its own context
//!   manager. `binder`, `hwbinder` and `vndbinder` (major [`BINDER_MAJOR`],
//!   minors 0-2) exist from boot; `BINDER_CTL_ADD` on `binder-control`
//!   creates more. A `binder` file system (binderfs, mounted by Android's
//!   init at /dev/binderfs) holds the default devices, `binder-control` and
//!   an empty `binder_logs/`. All binderfs mounts share the devices (one
//!   IPC namespace); devices added through one mount appear in that mount.
//! - Each open creates a binder process (`state::Proc`); its last close
//!   releases it: calls it was serving fail with BR_DEAD_REPLY, its nodes
//!   die (death notifications go out), its refs and buffers are dropped.
//! - mmap (read-only, at most 4 MiB used, once per open): a shmem object
//!   mapped shared and read-only into the process; the kernel writes
//!   transaction data straight into its pages.
//! - `protocol`: BINDER_WRITE_READ (commands, transactions, object
//!   translation, the read loop); `state`: the object model.

mod abi;
mod protocol;
mod state;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use abi::*;
use state::{Id, LOOPER_POLL, Mapping, lock};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLIN, POLLRDNORM, WaitQueue};
use crate::fs::tmpfs::TmpInode;
use crate::fs::vfs::{S_IFCHR, S_IFDIR, encode_dev};
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;
use crate::mm::user::{Backing, PROT_WRITE};
use crate::process::current;

pub const BINDER_MAJOR: u32 = 244;
/// Minor of `binder-control` (device minors stay below it).
const CONTROL_MINOR: u32 = 1023;
/// Buffer space used per process (larger mappings are not used beyond it).
pub const MAX_MAP: u64 = 4 << 20;
pub const BINDERFS_SUPER_MAGIC: u64 = 0x6c6f_6f70;

/// Device names by minor (context).
static DEVICES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn devices() -> spin::MutexGuard<'static, Vec<Vec<u8>>> {
    let mut d = DEVICES.lock();
    if d.is_empty() {
        for n in [&b"binder"[..], b"hwbinder", b"vndbinder"] {
            d.push(n.to_vec());
        }
    }
    d
}

/// Open a binder character device node.
pub fn open(minor: u32) -> KResult<Box<dyn FileOps>> {
    if minor == CONTROL_MINOR {
        return Ok(Box::new(ControlFile));
    }
    if minor as usize >= devices().len() {
        return Err(ENXIO);
    }
    let id = lock().new_proc(current().pid(), minor);
    Ok(Box::new(BinderFile { id }))
}

/// A new binderfs instance: the default devices, `binder-control` and
/// `binder_logs/`.
pub fn new_binderfs(mode: Option<u32>) -> Arc<TmpInode> {
    let root = TmpInode::new_fs(mode.unwrap_or(0o755));
    for (minor, name) in devices().iter().enumerate().take(3) {
        let _ = root.add(name, S_IFCHR | 0o600, encode_dev(BINDER_MAJOR, minor as u32), None);
    }
    let _ = root.add(b"binder-control", S_IFCHR | 0o600, encode_dev(BINDER_MAJOR, CONTROL_MINOR), None);
    let _ = root.add(b"binder_logs", S_IFDIR | 0o755, 0, None);
    root
}

/// An open binder device: one binder process.
struct BinderFile {
    id: Id,
}

impl Drop for BinderFile {
    fn drop(&mut self) {
        lock().proc_release(self.id);
    }
}

impl FileOps for BinderFile {
    fn ioctl(&self, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let tid = current().tid;
        let r = self.ioctl_thread(f, tid, cmd, arg);
        if let Some(th) = lock().thread_opt(self.id, tid) {
            th.need_return = false;
        }
        r
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let tid = current().tid;
        let mut s = lock();
        let th = s.thread(self.id, tid);
        th.looper |= LOOPER_POLL;
        let proc_work = th.proc_work_ok();
        if s.has_work(self.id, tid, proc_work) { POLLIN | POLLRDNORM } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(lock().proc(self.id).wq.clone())
    }

    fn mmap(&self, f: &Arc<OpenFile>, addr: u64, len: u64, prot: u32, fixed: bool, noreplace: bool, off: u64) -> KResult<u64> {
        if prot & PROT_WRITE != 0 {
            return Err(EPERM);
        }
        if off != 0 {
            return Err(EINVAL);
        }
        if lock().proc(self.id).map.is_some() {
            return Err(EBUSY);
        }
        let size = len.min(MAX_MAP).div_ceil(PAGE_SIZE) * PAGE_SIZE;
        let shmem = Shmem::new(size);
        let backing = Backing { obj: shmem.clone(), pgoff: 0, file: Some(f.clone()) };
        // Shared and never writable, whatever MAP_PRIVATE/SHARED says: the
        // pages must stay the kernel's.
        let mm = current().mm();
        let base = mm.mmap_object(addr, len, prot, true, false, backing, fixed, noreplace)?;
        let mut s = lock();
        let pr = s.proc(self.id);
        if pr.map.is_some() {
            drop(s);
            let _ = mm.munmap(base, len);
            return Err(EBUSY);
        }
        pr.map = Some(Mapping::new(shmem, base, size));
        Ok(base)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl BinderFile {
    fn ioctl_thread(&self, f: &OpenFile, tid: u32, cmd: u32, arg: u64) -> KResult<u64> {
        let p = self.id;
        let mm = current().mm();
        let read_u32 = || -> KResult<u32> {
            let mut b = [0u8; 4];
            mm.copy_in(arg, &mut b)?;
            Ok(u32::from_le_bytes(b))
        };
        lock().thread(p, tid);
        match cmd {
            BINDER_WRITE_READ => {
                let mut b = [0u8; BWR_SIZE];
                mm.copy_in(arg, &mut b)?;
                let (ws, mut wc, wb) = (rd64(&b, 0), rd64(&b, 8), rd64(&b, 16));
                let (rs, mut rc, rb) = (rd64(&b, 24), rd64(&b, 32), rd64(&b, 40));
                let mut res = Ok(());
                if ws > 0 {
                    res = protocol::write(p, tid, &mm, wb, ws, &mut wc);
                }
                if res.is_ok() && rs > rc {
                    match protocol::read(p, tid, f.nonblock(), rc == 0, (rs - rc) as usize) {
                        Ok(out) => match mm.copy_out(rb + rc, &out) {
                            Ok(()) => rc += out.len() as u64,
                            Err(e) => res = Err(e),
                        },
                        Err(e) => res = Err(e),
                    }
                }
                wr64(&mut b, 8, wc);
                wr64(&mut b, 32, rc);
                mm.copy_out(arg, &b)?;
                res.map(|_| 0)
            }
            BINDER_SET_MAX_THREADS => {
                let n = read_u32()?;
                lock().proc(p).max_threads = n;
                Ok(0)
            }
            BINDER_SET_CONTEXT_MGR | BINDER_SET_CONTEXT_MGR_EXT => {
                let (flags, ptr, cookie) = if cmd == BINDER_SET_CONTEXT_MGR_EXT {
                    let mut b = [0u8; 24];
                    mm.copy_in(arg, &mut b)?;
                    (rd32(&b, 4), rd64(&b, 8), rd64(&b, 16))
                } else {
                    (0, 0, 0)
                };
                let mut s = lock();
                let ctx = s.proc(p).ctx;
                if s.mgr.contains_key(&ctx) {
                    return Err(EBUSY);
                }
                if s.proc(p).nodes.contains_key(&ptr) {
                    return Err(EBUSY);
                }
                let n = s.new_node(p, ptr, cookie, flags);
                let node = s.nodes.get_mut(&n).unwrap();
                node.local_strong = 1;
                node.local_weak = 1;
                node.has_strong = true;
                node.has_weak = true;
                s.mgr.insert(ctx, n);
                Ok(0)
            }
            BINDER_THREAD_EXIT => {
                lock().thread_release(p, tid);
                Ok(0)
            }
            BINDER_VERSION => {
                mm.copy_out(arg, &PROTOCOL_VERSION.to_le_bytes())?;
                Ok(0)
            }
            BINDER_GET_NODE_DEBUG_INFO => {
                let mut b = [0u8; 24];
                mm.copy_in(arg, &mut b)?;
                let after = rd64(&b, 0);
                b = [0; 24];
                let mut s = lock();
                let next = s.proc(p).nodes.range(after.saturating_add(1)..).next().map(|(_, &n)| n);
                if let Some(n) = next.filter(|_| after != u64::MAX).and_then(|n| s.nodes.get(&n)) {
                    wr64(&mut b, 0, n.ptr);
                    wr64(&mut b, 8, n.cookie);
                    wr32(&mut b, 16, n.has_strong as u32);
                    wr32(&mut b, 20, n.has_weak as u32);
                }
                drop(s);
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            BINDER_GET_NODE_INFO_FOR_REF => {
                let mut b = [0u8; 24];
                mm.copy_in(arg, &mut b)?;
                if b[4..].iter().any(|&c| c != 0) {
                    return Err(EINVAL);
                }
                let handle = rd32(&b, 0);
                let mut s = lock();
                let ctx = s.proc(p).ctx;
                if s.mgr.get(&ctx).and_then(|m| s.nodes.get(m)).and_then(|n| n.owner) != Some(p) {
                    return Err(EPERM);
                }
                let n = s.proc(p).refs.get(&handle).ok_or(EINVAL)?.node;
                let node = s.nodes.get(&n).ok_or(EINVAL)?;
                wr32(&mut b, 4, node.local_strong + node.internal_strong);
                wr32(&mut b, 8, node.local_weak);
                drop(s);
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            BINDER_GET_EXTENDED_ERROR => {
                let (id, command, param) = core::mem::replace(&mut lock().thread(p, tid).ee, (0, BR_OK, 0));
                let mut b = [0u8; 12];
                wr32(&mut b, 0, id);
                wr32(&mut b, 4, command);
                wr32(&mut b, 8, param as u32);
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            BINDER_ENABLE_ONEWAY_SPAM_DETECTION => {
                let on = read_u32()? != 0;
                lock().proc(p).spam_detect = on;
                Ok(0)
            }
            // No freezer: nothing is ever frozen.
            BINDER_FREEZE | BINDER_SET_IDLE_TIMEOUT | BINDER_SET_IDLE_PRIORITY => Ok(0),
            BINDER_GET_FROZEN_INFO => {
                let mut b = [0u8; 12];
                mm.copy_in(arg, &mut b)?;
                b[4..].fill(0);
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }
}

/// binderfs `binder-control`: BINDER_CTL_ADD creates a device (a new
/// context) next to it.
struct ControlFile;

impl FileOps for ControlFile {
    fn ioctl(&self, f: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd != BINDER_CTL_ADD {
            return Err(EINVAL);
        }
        let mm = current().mm();
        let mut b = [0u8; BINDERFS_DEVICE_SIZE];
        mm.copy_in(arg, &mut b)?;
        let len = b[..256].iter().position(|&c| c == 0).ok_or(EINVAL)?;
        let name = &b[..len];
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(EINVAL);
        }
        let dir = f.path.as_ref().and_then(|p| p.dentry.parent()).ok_or(EINVAL)?;
        let dir = dir.inode.as_any().downcast_ref::<TmpInode>().ok_or(EINVAL)?;
        if dir.get(name).is_some() {
            return Err(EEXIST);
        }
        let minor = {
            let mut d = devices();
            if d.len() >= CONTROL_MINOR as usize {
                return Err(ENOSPC);
            }
            d.push(name.to_vec());
            (d.len() - 1) as u32
        };
        dir.add(name, S_IFCHR | 0o600, encode_dev(BINDER_MAJOR, minor), None)?;
        wr32(&mut b, 256, BINDER_MAJOR);
        wr32(&mut b, 260, minor);
        mm.copy_out(arg, &b)?;
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
