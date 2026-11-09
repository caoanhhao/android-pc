//! Open file description locks (F_OFD_SETLK, F_OFD_SETLKW, F_OFD_GETLK):
//! byte-range read/write locks on an inode, owned by the open file
//! description that took them and dropped when it is closed.
//!
//! Users: netd and libbpf_android's `bpfLock`, which lock byte `map id`
//! of the bpf maps' shared anonymous inode to make map access exclusive.
//! Classic per-process POSIX locks (F_SETLK) are still always granted.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::file::OpenFile;
use super::poll::WaitQueue;
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};

pub const F_RDLCK: i16 = 0;
pub const F_WRLCK: i16 = 1;
pub const F_UNLCK: i16 = 2;

#[derive(Clone, Copy)]
struct Lock {
    /// The owning open file description (its address).
    owner: usize,
    ty: i16,
    start: u64,
    /// Exclusive; u64::MAX: to the end of the file.
    end: u64,
}

/// Locks per inode (keyed by the inode's address).
static LOCKS: Mutex<BTreeMap<usize, Vec<Lock>>> = Mutex::new(BTreeMap::new());

/// Woken whenever a lock is released (F_OFD_SETLKW waiters recheck).
fn released() -> &'static Arc<WaitQueue> {
    static WQ: spin::Once<Arc<WaitQueue>> = spin::Once::new();
    WQ.call_once(WaitQueue::new)
}

fn inode_key(f: &OpenFile) -> usize {
    Arc::as_ptr(&f.inode) as *const () as usize
}

fn owner_key(f: &OpenFile) -> usize {
    f as *const OpenFile as usize
}

/// A `struct flock` request.
pub struct Request {
    pub ty: i16,
    pub start: u64,
    pub end: u64,
}

impl Request {
    /// Decode `struct flock` (l_type, l_whence, l_start, l_len, l_pid)
    /// against `f`'s position and size.
    pub fn parse(f: &OpenFile, b: &[u8; 32]) -> KResult<Request> {
        let ty = i16::from_ne_bytes([b[0], b[1]]);
        let whence = i16::from_ne_bytes([b[2], b[3]]);
        let start = i64::from_ne_bytes(b[8..16].try_into().unwrap());
        let len = i64::from_ne_bytes(b[16..24].try_into().unwrap());
        let pid = i32::from_ne_bytes(b[24..28].try_into().unwrap());
        if !matches!(ty, F_RDLCK | F_WRLCK | F_UNLCK) || pid != 0 {
            return Err(EINVAL);
        }
        let base: i64 = match whence {
            0 => 0,
            1 => f.pos() as i64,
            2 => f.ops.size(f) as i64,
            _ => return Err(EINVAL),
        };
        let mut s = base.checked_add(start).ok_or(EOVERFLOW)?;
        let e = if len == 0 {
            u64::MAX
        } else if len > 0 {
            s.checked_add(len).ok_or(EOVERFLOW)? as u64
        } else {
            let e = s;
            s = s.checked_add(len).ok_or(EINVAL)?;
            e as u64
        };
        if s < 0 {
            return Err(EINVAL);
        }
        Ok(Request { ty, start: s as u64, end: e })
    }
}

fn conflicts(l: &Lock, owner: usize, r: &Request) -> bool {
    l.owner != owner && l.start < r.end && r.start < l.end && (l.ty == F_WRLCK || r.ty == F_WRLCK)
}

/// F_OFD_GETLK: the first lock that would block `r`, as (type, start, end).
pub fn test(f: &OpenFile, r: &Request) -> Option<(i16, u64, u64)> {
    let locks = LOCKS.lock();
    let owner = owner_key(f);
    locks.get(&inode_key(f))?.iter().find(|l| conflicts(l, owner, r)).map(|l| (l.ty, l.start, l.end))
}

/// Try to apply `r` for `f`; false on conflict.
fn try_set(f: &OpenFile, r: &Request) -> bool {
    let mut locks = LOCKS.lock();
    let owner = owner_key(f);
    let key = inode_key(f);
    let list = locks.entry(key).or_default();
    if r.ty != F_UNLCK && list.iter().any(|l| conflicts(l, owner, r)) {
        return false;
    }
    // Cut the range out of this owner's locks, then add the new one.
    let mut out = Vec::with_capacity(list.len() + 2);
    for l in list.drain(..) {
        if l.owner != owner || l.end <= r.start || r.end <= l.start {
            out.push(l);
            continue;
        }
        if l.start < r.start {
            out.push(Lock { end: r.start, ..l });
        }
        if r.end < l.end {
            out.push(Lock { start: r.end, ..l });
        }
    }
    if r.ty != F_UNLCK {
        out.push(Lock { owner, ty: r.ty, start: r.start, end: r.end });
    }
    if out.is_empty() {
        locks.remove(&key);
    } else {
        *list = out;
    }
    true
}

/// F_OFD_SETLK (`wait` false: EAGAIN on conflict) and F_OFD_SETLKW.
pub fn set(f: &OpenFile, r: &Request, wait: bool) -> KResult<()> {
    match r.ty {
        F_RDLCK if !f.readable() => return Err(EBADF),
        F_WRLCK if !f.writable() => return Err(EBADF),
        _ => {}
    }
    if try_set(f, r) {
        if r.ty == F_UNLCK {
            released().wake(0);
        }
        return Ok(());
    }
    if !wait {
        return Err(EAGAIN);
    }
    match sched::wait_until(released().key(), None, WaitMode::Interruptible, || try_set(f, r)) {
        Wake::Ready => Ok(()),
        _ => Err(ERESTARTSYS),
    }
}

/// The open file description `f` is going away: drop its locks.
pub fn release_all(f: &OpenFile) {
    let owner = owner_key(f);
    let mut locks = LOCKS.lock();
    let Some(list) = locks.get_mut(&inode_key(f)) else { return };
    let before = list.len();
    list.retain(|l| l.owner != owner);
    let changed = list.len() != before;
    if list.is_empty() {
        locks.remove(&inode_key(f));
    }
    drop(locks);
    if changed {
        released().wake(0);
    }
}
