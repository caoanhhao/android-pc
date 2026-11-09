//! Futexes: FUTEX_WAIT/WAKE (+ the _BITSET variants and REQUEUE), and the
//! priority-inheritance lock ops (LOCK_PI, LOCK_PI2, TRYLOCK_PI,
//! UNLOCK_PI) that PTHREAD_PRIO_INHERIT mutexes use (audioserver). The PI
//! ops implement the futex word protocol (owner tid, FUTEX_WAITERS,
//! FUTEX_OWNER_DIED) but boost no priorities: the scheduler has none.
//! Unlocking hands the lock back to the word (0) and wakes one waiter,
//! which takes it with FUTEX_WAITERS set while others still sleep.
//!
//! A futex in a MAP_SHARED mapping (without FUTEX_PRIVATE_FLAG) is keyed by
//! (shmem object, offset), so processes mapping the same object at any
//! address meet on it; every other futex by (address space, user address).
//! Bitsets are accepted but every waiter matches.

use alloc::sync::Arc;

use super::current;
use crate::errno::*;
use crate::mm::user::AddressSpace;
use crate::sched::{self, Wake};

pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_REQUEUE: u32 = 3;
pub const FUTEX_CMP_REQUEUE: u32 = 4;
pub const FUTEX_WAIT_BITSET: u32 = 9;
pub const FUTEX_WAKE_BITSET: u32 = 10;
pub const FUTEX_LOCK_PI: u32 = 6;
pub const FUTEX_UNLOCK_PI: u32 = 7;
pub const FUTEX_TRYLOCK_PI: u32 = 8;
pub const FUTEX_LOCK_PI2: u32 = 13;
pub const FUTEX_PRIVATE_FLAG: u32 = 128;
const FUTEX_WAITERS: u32 = 0x8000_0000;
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;
const FUTEX_TID_MASK: u32 = 0x3fff_ffff;

/// Tasks sleeping in LOCK_PI, per futex.
static PI_WAITERS: spin::Mutex<alloc::collections::BTreeMap<sched::WaitKey, usize>> =
    spin::Mutex::new(alloc::collections::BTreeMap::new());

fn pi_waiters(k: sched::WaitKey) -> usize {
    PI_WAITERS.lock().get(&k).copied().unwrap_or(0)
}

/// Take the PI futex at `uaddr` if it is free (owner 0): Ok(true).
fn pi_try_take(mm: &AddressSpace, k: sched::WaitKey, uaddr: u64, me: u32) -> KResult<Option<u32>> {
    let v = read_u32(mm, uaddr)?;
    let owner = v & FUTEX_TID_MASK;
    let dead_owner = owner != 0 && !super::TABLE.lock().tasks.contains_key(&owner);
    if owner == 0 || dead_owner {
        let waiters = if pi_waiters(k) > 0 { FUTEX_WAITERS } else { 0 };
        let died = if dead_owner { FUTEX_OWNER_DIED } else { v & FUTEX_OWNER_DIED };
        if mm.cmpxchg_u32(uaddr, v, me | waiters | died)? == v {
            return Ok(None);
        }
        return Ok(Some(u32::MAX)); // raced: try again
    }
    if owner == me {
        return Err(EDEADLK);
    }
    Ok(Some(v))
}

fn lock_pi(mm: &Arc<AddressSpace>, uaddr: u64, private: bool, deadline: Option<u64>, try_only: bool) -> KResult<u64> {
    let me = current().tid;
    let k = key(mm, uaddr, private);
    loop {
        let v = match pi_try_take(mm, k, uaddr, me)? {
            None => return Ok(0),
            Some(u32::MAX) => continue,
            Some(v) => v,
        };
        if try_only {
            return Err(EAGAIN);
        }
        let want = v | FUTEX_WAITERS;
        if v & FUTEX_WAITERS == 0 && mm.cmpxchg_u32(uaddr, v, want)? != v {
            continue;
        }
        *PI_WAITERS.lock().entry(k).or_default() += 1;
        let w = sched::wait_on(k, deadline, || {
            if read_u32(mm, uaddr)? != want {
                return Err(EAGAIN);
            }
            Ok(())
        });
        {
            let mut m = PI_WAITERS.lock();
            if let Some(n) = m.get_mut(&k) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&k);
                }
            }
        }
        match w {
            Ok(Wake::Ready) | Err(EAGAIN) => continue,
            Ok(Wake::Timeout) => return Err(ETIMEDOUT),
            Ok(Wake::Interrupted) => return Err(if deadline.is_some() { EINTR } else { ERESTARTSYS }),
            Err(e) => return Err(e),
        }
    }
}

fn unlock_pi(mm: &Arc<AddressSpace>, uaddr: u64, private: bool) -> KResult<u64> {
    let me = current().tid;
    let k = key(mm, uaddr, private);
    loop {
        let v = read_u32(mm, uaddr)?;
        if v & FUTEX_TID_MASK != me {
            return Err(EPERM);
        }
        if mm.cmpxchg_u32(uaddr, v, 0)? == v {
            break;
        }
    }
    if pi_waiters(k) > 0 {
        sched::wake_n(k, 1);
    }
    Ok(0)
}
pub const FUTEX_CLOCK_REALTIME: u32 = 256;

fn key(mm: &Arc<AddressSpace>, uaddr: u64, private: bool) -> sched::WaitKey {
    let shared = if private { None } else { mm.shared_futex_key(uaddr) };
    shared.unwrap_or((Arc::as_ptr(mm) as u64, uaddr))
}

fn read_u32(mm: &AddressSpace, uaddr: u64) -> KResult<u32> {
    let mut b = [0u8; 4];
    mm.copy_in(uaddr, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// Wake up to `n` waiters on `uaddr` in `mm` (a shared futex if `uaddr`
/// is in shared memory, like Linux's clear_child_tid wake).
pub fn wake(mm: &Arc<AddressSpace>, uaddr: u64, n: usize) -> usize {
    sched::wake_n(key(mm, uaddr, false), n)
}

pub fn sys_futex(uaddr: u64, op: u32, val: u32, timeout: u64, uaddr2: u64, val3: u32) -> KResult<u64> {
    let cmd = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    if uaddr % 4 != 0 {
        return Err(EINVAL);
    }
    let mm = current().mm();
    let private = op & FUTEX_PRIVATE_FLAG != 0;
    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            if cmd == FUTEX_WAIT_BITSET && val3 == 0 {
                return Err(EINVAL);
            }
            let deadline = if timeout == 0 {
                None
            } else {
                let (s, ns) = crate::syscall::time::read_timespec(&mm, timeout)?;
                let t = s.saturating_mul(1_000_000_000).saturating_add(ns);
                Some(if cmd == FUTEX_WAIT {
                    // Relative timeout.
                    crate::time::monotonic_ns().saturating_add(t)
                } else if op & FUTEX_CLOCK_REALTIME != 0 {
                    // Absolute CLOCK_REALTIME -> monotonic.
                    let now_rt = crate::time::realtime_ns();
                    let delta = (t as i128 - now_rt).max(0) as u64;
                    crate::time::monotonic_ns().saturating_add(delta)
                } else {
                    t
                })
            };
            let w = sched::wait_on(key(&mm, uaddr, private), deadline, || {
                if read_u32(&mm, uaddr)? != val {
                    return Err(EAGAIN);
                }
                Ok(())
            })?;
            match w {
                Wake::Ready => Ok(0),
                Wake::Timeout => Err(ETIMEDOUT),
                Wake::Interrupted => Err(if deadline.is_some() { EINTR } else { ERESTARTSYS }),
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            if cmd == FUTEX_WAKE_BITSET && val3 == 0 {
                return Err(EINVAL);
            }
            Ok(sched::wake_n(key(&mm, uaddr, private), val as usize) as u64)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            if uaddr2 % 4 != 0 {
                return Err(EINVAL);
            }
            if cmd == FUTEX_CMP_REQUEUE && read_u32(&mm, uaddr)? != val3 {
                return Err(EAGAIN);
            }
            let woken = sched::wake_n(key(&mm, uaddr, private), val as usize);
            // For the requeue ops the timeout argument holds val2.
            let moved = sched::requeue(key(&mm, uaddr, private), key(&mm, uaddr2, private), timeout as u32 as usize);
            Ok((woken + moved) as u64)
        }
        FUTEX_LOCK_PI | FUTEX_LOCK_PI2 | FUTEX_TRYLOCK_PI => {
            // Absolute timeouts: CLOCK_REALTIME for LOCK_PI, LOCK_PI2's
            // clock is CLOCK_MONOTONIC unless FUTEX_CLOCK_REALTIME.
            let deadline = if timeout == 0 || cmd == FUTEX_TRYLOCK_PI {
                None
            } else {
                let (s, ns) = crate::syscall::time::read_timespec(&mm, timeout)?;
                let t = s.saturating_mul(1_000_000_000).saturating_add(ns);
                Some(if cmd == FUTEX_LOCK_PI || op & FUTEX_CLOCK_REALTIME != 0 {
                    let delta = (t as i128 - crate::time::realtime_ns()).max(0) as u64;
                    crate::time::monotonic_ns().saturating_add(delta)
                } else {
                    t
                })
            };
            lock_pi(&mm, uaddr, private, deadline, cmd == FUTEX_TRYLOCK_PI)
        }
        FUTEX_UNLOCK_PI => unlock_pi(&mm, uaddr, private),
        _ => Err(ENOSYS),
    }
}
