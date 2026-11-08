//! I/O multiplexing and event files: poll, ppoll, select, pselect6,
//! epoll_*, eventfd2, timerfd_*, signalfd4.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::errno::*;
use crate::fs::epoll::{self, Epoll};
use crate::fs::file::{O_CLOEXEC, O_NONBLOCK, OpenFile};
use crate::fs::poll::*;
use crate::fs::signalfd::SignalFd;
use crate::fs::timerfd::{self, TFD_TIMER_ABSTIME, TFD_TIMER_CANCEL_ON_SET, TimerFdFile};
use crate::fs::{eventfd, signalfd};
use crate::mm::user::AddressSpace;
use crate::process::{current, signal};
use crate::time::{self, NSEC_PER_SEC};

use super::time::read_timespec;

/// Most descriptors one poll/select call may name (RLIMIT_NOFILE).
const MAX_FDS: usize = 1024;
const SIGSET_SIZE: u64 = 8;

fn files_get(fd: i32) -> KResult<Arc<OpenFile>> {
    current().process.files().get(fd)
}

fn install(f: Arc<OpenFile>, cloexec: bool) -> KResult<u64> {
    Ok(current().process.files().alloc(f, cloexec, 0)? as u64)
}

/// Monotonic deadline for a relative timeout in ns (Some(0) = do not wait).
fn deadline_after(ns: u64) -> Option<u64> {
    if ns == 0 { Some(0) } else { Some(time::monotonic_ns().saturating_add(ns)) }
}

/// Time left until `deadline` (for the updated timeouts of ppoll/select).
fn remaining(deadline: Option<u64>) -> u64 {
    match deadline {
        Some(d) => d.saturating_sub(time::monotonic_ns()),
        None => 0,
    }
}

fn read_sigmask(mm: &AddressSpace, addr: u64) -> KResult<u64> {
    let mut b = [0u8; 8];
    mm.copy_in(addr, &mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Run `f` with the temporary signal mask at `sigmask` (if non-NULL).
fn with_sigmask<T>(sigmask: Option<u64>, f: impl FnOnce() -> KResult<T>) -> KResult<T> {
    if let Some(m) = sigmask {
        signal::begin_temp_mask(m);
    }
    let r = f();
    if sigmask.is_some() {
        signal::end_temp_mask(r.as_ref().err() == Some(&EINTR));
    }
    r
}

// ---------------------------------------------------------------------------
// poll / ppoll

fn poll_common(fds: u64, nfds: u64, deadline: Option<u64>) -> KResult<u64> {
    let nfds = nfds as usize;
    if nfds > MAX_FDS {
        return Err(EINVAL);
    }
    let mm = current().mm();
    let mut raw = vec![0u8; nfds * 8];
    mm.copy_in(fds, &mut raw)?;
    let mut entries: Vec<PollEntry> = raw
        .chunks(8)
        .map(|c| {
            let fd = i32::from_le_bytes(c[0..4].try_into().unwrap());
            let events = u16::from_le_bytes(c[4..6].try_into().unwrap()) as u32;
            let file = if fd >= 0 { files_get(fd).ok() } else { None };
            PollEntry { fd, file, events, revents: 0 }
        })
        .collect();
    let n = do_poll(&mut entries, deadline)?;
    for (c, e) in raw.chunks_mut(8).zip(&entries) {
        c[6..8].copy_from_slice(&(e.revents as u16).to_le_bytes());
    }
    mm.copy_out(fds, &raw)?;
    Ok(n as u64)
}

pub fn sys_poll(fds: u64, nfds: u64, timeout_ms: i32) -> KResult<u64> {
    let deadline = (timeout_ms >= 0).then(|| deadline_after(timeout_ms as u64 * 1_000_000)).flatten();
    poll_common(fds, nfds, deadline)
}

pub fn sys_ppoll(fds: u64, nfds: u64, tsp: u64, sigmask: u64, sigsetsize: u64) -> KResult<u64> {
    let mm = current().mm();
    let deadline = if tsp != 0 {
        let (s, ns) = read_timespec(&mm, tsp)?;
        deadline_after(s.saturating_mul(NSEC_PER_SEC).saturating_add(ns))
    } else {
        None
    };
    let mask = if sigmask != 0 {
        if sigsetsize != SIGSET_SIZE {
            return Err(EINVAL);
        }
        Some(read_sigmask(&mm, sigmask)?)
    } else {
        None
    };
    let r = with_sigmask(mask, || poll_common(fds, nfds, deadline));
    if tsp != 0 && deadline != Some(0) {
        let _ = write_ns(&mm, tsp, remaining(deadline), false);
    }
    r
}

/// Write a timespec (or, with `timeval`, a timeval) holding `ns`.
fn write_ns(mm: &AddressSpace, addr: u64, ns: u64, timeval: bool) -> KResult<()> {
    let sec = ns / NSEC_PER_SEC;
    let frac = if timeval { (ns % NSEC_PER_SEC) / 1000 } else { ns % NSEC_PER_SEC };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&sec.to_le_bytes());
    b[8..].copy_from_slice(&frac.to_le_bytes());
    mm.copy_out(addr, &b)
}

// ---------------------------------------------------------------------------
// select / pselect6

const SELECT_IN: u32 = POLLRDNORM | POLLRDBAND | POLLIN | POLLHUP | POLLERR;
const SELECT_OUT: u32 = POLLWRBAND | POLLWRNORM | POLLOUT | POLLERR;
const SELECT_EX: u32 = POLLPRI;

fn select_common(nfds: i32, sets: [u64; 3], deadline: Option<u64>) -> KResult<u64> {
    if nfds < 0 {
        return Err(EINVAL);
    }
    let nfds = (nfds as usize).min(MAX_FDS);
    let mm = current().mm();
    let bytes = nfds.div_ceil(64) * 8;
    let mut bits = [vec![0u8; bytes], vec![0u8; bytes], vec![0u8; bytes]];
    for (b, &addr) in bits.iter_mut().zip(&sets) {
        if addr != 0 {
            mm.copy_in(addr, b)?;
        }
    }
    let is_set = |b: &[u8], fd: usize| b[fd / 8] & (1 << (fd % 8)) != 0;
    let mut entries = Vec::new();
    for fd in 0..nfds {
        let want = [SELECT_IN, SELECT_OUT, SELECT_EX]
            .iter()
            .zip(&bits)
            .filter(|(_, b)| is_set(b, fd))
            .fold(0, |m, (ev, _)| m | ev);
        if want != 0 {
            let file = files_get(fd as i32).map_err(|_| EBADF)?;
            entries.push(PollEntry { fd: fd as i32, file: Some(file), events: want, revents: 0 });
        }
    }
    do_poll(&mut entries, deadline)?;
    let mut out = [vec![0u8; bytes], vec![0u8; bytes], vec![0u8; bytes]];
    let mut count = 0;
    for e in &entries {
        let fd = e.fd as usize;
        for (i, ev) in [SELECT_IN, SELECT_OUT, SELECT_EX].iter().enumerate() {
            if is_set(&bits[i], fd) && e.revents & ev != 0 {
                out[i][fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
        }
    }
    for (b, &addr) in out.iter().zip(&sets) {
        if addr != 0 {
            mm.copy_out(addr, b)?;
        }
    }
    Ok(count)
}

pub fn sys_select(nfds: i32, r: u64, w: u64, e: u64, tvp: u64) -> KResult<u64> {
    let mm = current().mm();
    let deadline = if tvp != 0 {
        let mut b = [0u8; 16];
        mm.copy_in(tvp, &mut b)?;
        let sec = i64::from_le_bytes(b[..8].try_into().unwrap());
        let usec = i64::from_le_bytes(b[8..].try_into().unwrap());
        if sec < 0 || usec < 0 {
            return Err(EINVAL);
        }
        // Linux normalizes usec >= 1e6 into seconds.
        let ns = (sec as u64).saturating_mul(NSEC_PER_SEC).saturating_add((usec as u64).saturating_mul(1000));
        deadline_after(ns)
    } else {
        None
    };
    let r = select_common(nfds, [r, w, e], deadline);
    if tvp != 0 && deadline != Some(0) {
        let _ = write_ns(&mm, tvp, remaining(deadline), true);
    }
    r
}

pub fn sys_pselect6(nfds: i32, r: u64, w: u64, e: u64, tsp: u64, sig: u64) -> KResult<u64> {
    let mm = current().mm();
    let deadline = if tsp != 0 {
        let (s, ns) = read_timespec(&mm, tsp)?;
        deadline_after(s.saturating_mul(NSEC_PER_SEC).saturating_add(ns))
    } else {
        None
    };
    // The 6th argument points to { const sigset_t *ss; size_t ss_len; }.
    let mask = if sig != 0 {
        let mut b = [0u8; 16];
        mm.copy_in(sig, &mut b)?;
        let ss = u64::from_le_bytes(b[..8].try_into().unwrap());
        let len = u64::from_le_bytes(b[8..].try_into().unwrap());
        if ss != 0 {
            if len != SIGSET_SIZE {
                return Err(EINVAL);
            }
            Some(read_sigmask(&mm, ss)?)
        } else {
            None
        }
    } else {
        None
    };
    let res = with_sigmask(mask, || select_common(nfds, [r, w, e], deadline));
    if tsp != 0 && deadline != Some(0) {
        let _ = write_ns(&mm, tsp, remaining(deadline), false);
    }
    res
}

// ---------------------------------------------------------------------------
// epoll

pub fn sys_epoll_create1(flags: u32) -> KResult<u64> {
    if flags & !O_CLOEXEC != 0 {
        return Err(EINVAL);
    }
    install(epoll::new(), flags & O_CLOEXEC != 0)
}

pub fn sys_epoll_create(size: i32) -> KResult<u64> {
    if size <= 0 {
        return Err(EINVAL);
    }
    sys_epoll_create1(0)
}

fn epoll_file(epfd: i32) -> KResult<(Arc<OpenFile>, Arc<Epoll>)> {
    let f = files_get(epfd)?;
    let ep = epoll::epoll_of(&f).ok_or(EINVAL)?.clone();
    Ok((f, ep))
}

pub fn sys_epoll_ctl(epfd: i32, op: u32, fd: i32, event: u64) -> KResult<u64> {
    let (ef, ep) = epoll_file(epfd)?;
    let target = files_get(fd)?;
    let (events, data) = if op != epoll::EPOLL_CTL_DEL {
        let mut b = [0u8; 12];
        current().mm().copy_in(event, &mut b)?;
        (u32::from_le_bytes(b[..4].try_into().unwrap()), u64::from_le_bytes(b[4..].try_into().unwrap()))
    } else {
        (0, 0)
    };
    if Arc::ptr_eq(&ef, &target) {
        return Err(EINVAL);
    }
    ep.ctl(op, fd, &target, events, data)?;
    Ok(0)
}

fn epoll_wait_common(epfd: i32, events: u64, maxevents: i32, deadline: Option<u64>, sigmask: u64, size: u64) -> KResult<u64> {
    if maxevents <= 0 || maxevents as usize > (i32::MAX as usize) / 12 {
        return Err(EINVAL);
    }
    let (_f, ep) = epoll_file(epfd)?;
    let mm = current().mm();
    let mask = if sigmask != 0 {
        if size != SIGSET_SIZE {
            return Err(EINVAL);
        }
        Some(read_sigmask(&mm, sigmask)?)
    } else {
        None
    };
    let out = with_sigmask(mask, || ep.wait(maxevents as usize, deadline))?;
    let mut b = vec![0u8; out.len() * 12];
    for (c, (ev, data)) in b.chunks_mut(12).zip(&out) {
        c[..4].copy_from_slice(&ev.to_le_bytes());
        c[4..].copy_from_slice(&data.to_le_bytes());
    }
    mm.copy_out(events, &b)?;
    Ok(out.len() as u64)
}

fn ms_deadline(timeout_ms: i32) -> Option<u64> {
    (timeout_ms >= 0).then(|| deadline_after(timeout_ms as u64 * 1_000_000)).flatten()
}

pub fn sys_epoll_wait(epfd: i32, events: u64, maxevents: i32, timeout_ms: i32) -> KResult<u64> {
    epoll_wait_common(epfd, events, maxevents, ms_deadline(timeout_ms), 0, 0)
}

pub fn sys_epoll_pwait(epfd: i32, events: u64, maxevents: i32, timeout_ms: i32, sigmask: u64, size: u64) -> KResult<u64> {
    epoll_wait_common(epfd, events, maxevents, ms_deadline(timeout_ms), sigmask, size)
}

pub fn sys_epoll_pwait2(epfd: i32, events: u64, maxevents: i32, tsp: u64, sigmask: u64, size: u64) -> KResult<u64> {
    let deadline = if tsp != 0 {
        let (s, ns) = read_timespec(&current().mm(), tsp)?;
        deadline_after(s.saturating_mul(NSEC_PER_SEC).saturating_add(ns))
    } else {
        None
    };
    epoll_wait_common(epfd, events, maxevents, deadline, sigmask, size)
}

// ---------------------------------------------------------------------------
// eventfd, timerfd, signalfd

const EFD_FLAGS: u32 = eventfd::EFD_SEMAPHORE | O_NONBLOCK | O_CLOEXEC;

pub fn sys_eventfd2(initval: u32, flags: u32) -> KResult<u64> {
    if flags & !EFD_FLAGS != 0 {
        return Err(EINVAL);
    }
    install(eventfd::new(initval, flags), flags & O_CLOEXEC != 0)
}

pub fn sys_timerfd_create(clock: u32, flags: u32) -> KResult<u64> {
    if !timerfd::valid_clock(clock) || flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    install(timerfd::new(clock, flags), flags & O_CLOEXEC != 0)
}

fn timerfd_of(fd: i32) -> KResult<Arc<timerfd::TimerFd>> {
    let f = files_get(fd)?;
    let t = f.ops.as_any().downcast_ref::<TimerFdFile>().ok_or(EINVAL)?;
    Ok(t.0.clone())
}

/// struct itimerspec { it_interval, it_value } as (interval, value) ns.
fn write_itimerspec(mm: &AddressSpace, addr: u64, value: u64, interval: u64) -> KResult<()> {
    write_ns(mm, addr, interval, false)?;
    write_ns(mm, addr + 16, value, false)
}

pub fn sys_timerfd_settime(fd: i32, flags: u32, new: u64, old: u64) -> KResult<u64> {
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return Err(EINVAL);
    }
    let mm = current().mm();
    let (is, ins) = read_timespec(&mm, new)?;
    let (vs, vns) = read_timespec(&mm, new + 16)?;
    let t = timerfd_of(fd)?;
    let ns = |s: u64, n: u64| s.saturating_mul(NSEC_PER_SEC).saturating_add(n);
    let (ov, oi) = t.set(flags, ns(vs, vns), ns(is, ins));
    if old != 0 {
        write_itimerspec(&mm, old, ov, oi)?;
    }
    Ok(0)
}

pub fn sys_timerfd_gettime(fd: i32, cur: u64) -> KResult<u64> {
    let (v, i) = timerfd_of(fd)?.get();
    write_itimerspec(&current().mm(), cur, v, i)?;
    Ok(0)
}

pub fn sys_signalfd4(fd: i32, mask: u64, size: u64, flags: u32) -> KResult<u64> {
    if size != SIGSET_SIZE || flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let m = read_sigmask(&current().mm(), mask)? & !(signal::bit(signal::SIGKILL) | signal::bit(signal::SIGSTOP));
    if fd == -1 {
        return install(signalfd::new(m, flags), flags & O_CLOEXEC != 0);
    }
    let f = files_get(fd)?;
    let s = f.ops.as_any().downcast_ref::<SignalFd>().ok_or(EINVAL)?;
    s.mask.store(m, Ordering::Relaxed);
    // Pollers re-check against the new mask.
    current().process.signalfd_wq.wake(0);
    Ok(fd as u64)
}
