//! Futexes: FUTEX_WAIT/WAKE (+ the _BITSET variants and REQUEUE).
//!
//! Every futex is keyed by (address space, user address), i.e. treated as
//! process-private. Shared futexes across processes need shared memory,
//! which arrives with MAP_SHARED in phase B3. Bitsets are accepted but
//! every waiter matches.

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
pub const FUTEX_PRIVATE_FLAG: u32 = 128;
pub const FUTEX_CLOCK_REALTIME: u32 = 256;

fn key(mm: &Arc<AddressSpace>, uaddr: u64) -> sched::WaitKey {
    (Arc::as_ptr(mm) as u64, uaddr)
}

fn read_u32(mm: &AddressSpace, uaddr: u64) -> KResult<u32> {
    let mut b = [0u8; 4];
    mm.copy_in(uaddr, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// Wake up to `n` waiters on `uaddr` in `mm`.
pub fn wake(mm: &Arc<AddressSpace>, uaddr: u64, n: usize) -> usize {
    sched::wake_n(key(mm, uaddr), n)
}

pub fn sys_futex(uaddr: u64, op: u32, val: u32, timeout: u64, uaddr2: u64, val3: u32) -> KResult<u64> {
    let cmd = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    if uaddr % 4 != 0 {
        return Err(EINVAL);
    }
    let mm = current().mm();
    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            if cmd == FUTEX_WAIT_BITSET && val3 == 0 {
                return Err(EINVAL);
            }
            let deadline = if timeout == 0 {
                None
            } else {
                let (s, ns) = crate::syscall::time::read_timespec(&mm, timeout)?;
                let t = s * 1_000_000_000 + ns;
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
            let w = sched::wait_on(key(&mm, uaddr), deadline, || {
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
            Ok(wake(&mm, uaddr, val as usize) as u64)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            if uaddr2 % 4 != 0 {
                return Err(EINVAL);
            }
            if cmd == FUTEX_CMP_REQUEUE && read_u32(&mm, uaddr)? != val3 {
                return Err(EAGAIN);
            }
            let woken = wake(&mm, uaddr, val as usize);
            // For the requeue ops the timeout argument holds val2.
            let moved = sched::requeue(key(&mm, uaddr), key(&mm, uaddr2), timeout as u32 as usize);
            Ok((woken + moved) as u64)
        }
        _ => Err(ENOSYS),
    }
}
