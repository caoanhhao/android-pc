//! Clocks and sleeping.

use crate::errno::*;
use crate::mm::user::AddressSpace;
use crate::process::current;
use crate::sched::{self, Wake, WaitMode};
use crate::time::{self, NSEC_PER_SEC};

pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: u32 = 2;
pub const CLOCK_THREAD_CPUTIME_ID: u32 = 3;
pub const CLOCK_MONOTONIC_RAW: u32 = 4;
pub const CLOCK_REALTIME_COARSE: u32 = 5;
pub const CLOCK_MONOTONIC_COARSE: u32 = 6;
pub const CLOCK_BOOTTIME: u32 = 7;
pub const CLOCK_REALTIME_ALARM: u32 = 8;
pub const CLOCK_BOOTTIME_ALARM: u32 = 9;
pub const CLOCK_TAI: u32 = 11;
const TIMER_ABSTIME: u32 = 1;
const WAIT_SLEEP: u64 = 0x534c_4545_5000_0000;

/// Read and validate a `struct timespec`: (seconds, nanoseconds).
pub fn read_timespec(mm: &AddressSpace, addr: u64) -> KResult<(u64, u64)> {
    let mut b = [0u8; 16];
    mm.copy_in(addr, &mut b)?;
    let sec = i64::from_le_bytes(b[..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(b[8..].try_into().unwrap());
    if sec < 0 || !(0..NSEC_PER_SEC as i64).contains(&nsec) {
        return Err(EINVAL);
    }
    Ok((sec as u64, nsec as u64))
}

fn write_timespec(mm: &AddressSpace, addr: u64, ns: i128) -> KResult<()> {
    let sec = ns.div_euclid(NSEC_PER_SEC as i128) as i64;
    let nsec = ns.rem_euclid(NSEC_PER_SEC as i128) as i64;
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&sec.to_le_bytes());
    b[8..].copy_from_slice(&nsec.to_le_bytes());
    mm.copy_out(addr, &b)
}

/// Current value of `clock` in ns. CPU-time clocks report time since boot
/// until per-task accounting exists.
fn clock_ns(clock: u32) -> KResult<i128> {
    Ok(match clock {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE | CLOCK_REALTIME_ALARM | CLOCK_TAI => time::realtime_ns(),
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME
        | CLOCK_BOOTTIME_ALARM | CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => {
            time::monotonic_ns() as i128
        }
        _ => return Err(EINVAL),
    })
}

pub fn sys_clock_gettime(clock: u32, ts: u64) -> KResult<u64> {
    let ns = clock_ns(clock)?;
    write_timespec(&current().mm(), ts, ns)?;
    Ok(0)
}

pub fn sys_clock_getres(clock: u32, ts: u64) -> KResult<u64> {
    clock_ns(clock)?;
    let res = match clock {
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => (NSEC_PER_SEC / time::HZ) as i128,
        _ => 1,
    };
    if ts != 0 {
        write_timespec(&current().mm(), ts, res)?;
    }
    Ok(0)
}

pub fn sys_gettimeofday(tv: u64, _tz: u64) -> KResult<u64> {
    if tv != 0 {
        let ns = time::realtime_ns();
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&((ns / NSEC_PER_SEC as i128) as i64).to_le_bytes());
        b[8..].copy_from_slice(&(((ns % NSEC_PER_SEC as i128) / 1000) as i64).to_le_bytes());
        current().mm().copy_out(tv, &b)?;
    }
    Ok(0)
}

pub fn sys_time(t: u64) -> KResult<u64> {
    let s = (time::realtime_ns() / NSEC_PER_SEC as i128) as i64;
    if t != 0 {
        current().mm().copy_out(t, &s.to_le_bytes())?;
    }
    Ok(s as u64)
}

/// Sleep until monotonic `deadline`. On a signal, returns the time left.
fn sleep_until(deadline: u64) -> Result<(), u64> {
    let tid = current().tid as u64;
    let w = sched::wait_until((WAIT_SLEEP, tid), Some(deadline), WaitMode::Interruptible, || {
        time::monotonic_ns() >= deadline
    });
    match w {
        Wake::Interrupted => Err(deadline.saturating_sub(time::monotonic_ns())),
        _ => Ok(()),
    }
}

pub fn sys_nanosleep(req: u64, rem: u64) -> KResult<u64> {
    let mm = current().mm();
    let (s, ns) = read_timespec(&mm, req)?;
    let deadline = time::monotonic_ns().saturating_add(s.saturating_mul(NSEC_PER_SEC).saturating_add(ns));
    match sleep_until(deadline) {
        Ok(()) => Ok(0),
        Err(left) => {
            if rem != 0 {
                write_timespec(&mm, rem, left as i128)?;
            }
            Err(EINTR)
        }
    }
}

pub fn sys_clock_nanosleep(clock: u32, flags: u32, req: u64, rem: u64) -> KResult<u64> {
    if matches!(clock, CLOCK_THREAD_CPUTIME_ID) {
        return Err(EINVAL);
    }
    let now = clock_ns(clock)?;
    let mm = current().mm();
    let (s, ns) = read_timespec(&mm, req)?;
    let t = s.saturating_mul(NSEC_PER_SEC).saturating_add(ns) as i128;
    let delta = if flags & TIMER_ABSTIME != 0 { (t - now).max(0) } else { t };
    let deadline = time::monotonic_ns().saturating_add(delta as u64);
    match sleep_until(deadline) {
        Ok(()) => Ok(0),
        Err(left) => {
            if rem != 0 && flags & TIMER_ABSTIME == 0 {
                write_timespec(&mm, rem, left as i128)?;
            }
            Err(EINTR)
        }
    }
}
