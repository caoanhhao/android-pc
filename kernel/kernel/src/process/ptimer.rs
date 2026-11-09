//! POSIX per-process timers (timer_create & co.) and the interval timer
//! behind setitimer(ITIMER_REAL) / alarm. Expiry runs in the timer
//! interrupt, which cannot take the process table lock: it queues the
//! timer, and the signal is sent from task context (`run_due`, called on
//! the way back to user mode and from the idle loop). Overruns are
//! counted while a signal is still queued. ITIMER_VIRTUAL and ITIMER_PROF
//! are stored and read back, never fire (no CPU time accounting).

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::signal::{self, SigInfo};
use super::{TABLE, current};
use crate::errno::*;
use crate::sched::timer::{self, TimerCallback};
use crate::sync::IrqLock;
use crate::syscall::time::{CLOCK_BOOTTIME, CLOCK_BOOTTIME_ALARM, CLOCK_MONOTONIC, CLOCK_REALTIME, CLOCK_REALTIME_ALARM};
use crate::time::{self, NSEC_PER_SEC};

const SIGEV_SIGNAL: i32 = 0;
const SIGEV_NONE: i32 = 1;
const SIGEV_THREAD_ID: i32 = 4;
const SI_TIMER: i32 = -2;
const TIMER_ABSTIME: u32 = 1;
const SIGALRM: u32 = 14;
/// The id of a process's ITIMER_REAL in the table.
const ITIMER_ID: i32 = -1;
const MAX_TIMERS: usize = 1024;

#[derive(Default)]
struct State {
    /// Next expiry (monotonic ns), 0 = disarmed.
    expires: u64,
    interval: u64,
    timer: Option<u64>,
    /// Expiries while the previous signal was still queued.
    overrun: u32,
    queued: bool,
}

struct PTimer {
    pid: u32,
    id: i32,
    clock: u32,
    notify: i32,
    signo: u32,
    value: u64,
    /// SIGEV_THREAD_ID target.
    tid: u32,
    me: Weak<PTimer>,
    st: IrqLock<State>,
}

static TIMERS: Mutex<BTreeMap<(u32, i32), Arc<PTimer>>> = Mutex::new(BTreeMap::new());
static DUE: IrqLock<Vec<Weak<PTimer>>> = IrqLock::new(Vec::new());
static ANY_DUE: AtomicBool = AtomicBool::new(false);

impl PTimer {
    fn arm(&self, s: &mut State) {
        if let Some(id) = s.timer.take() {
            timer::cancel(id);
        }
        if s.expires != 0 {
            s.timer = Some(timer::arm(s.expires, self.me.clone()));
        }
    }

    /// (left, interval) in ns.
    fn get(&self) -> (u64, u64) {
        let now = time::monotonic_ns();
        let s = self.st.lock();
        (if s.expires == 0 { 0 } else { s.expires.saturating_sub(now).max(1) }, s.interval)
    }

    fn set(&self, flags: u32, value: u64, interval: u64) -> (u64, u64) {
        let old = self.get();
        let now = time::monotonic_ns();
        let expires = if value == 0 {
            0
        } else if flags & TIMER_ABSTIME == 0 {
            now.saturating_add(value)
        } else if matches!(self.clock, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
            (value as i128 - time::boot_epoch_ns()).clamp(1, u64::MAX as i128) as u64
        } else {
            value.max(1)
        };
        let mut s = self.st.lock();
        s.expires = expires;
        s.interval = if expires == 0 { 0 } else { interval };
        s.overrun = 0;
        self.arm(&mut s);
        old
    }
}

impl TimerCallback for PTimer {
    fn fire(&self, id: u64) {
        {
            let mut s = self.st.lock();
            if s.timer != Some(id) {
                return;
            }
            s.timer = None;
            let now = time::monotonic_ns();
            if s.interval > 0 {
                let missed = (now - s.expires) / s.interval;
                s.expires += (missed + 1) * s.interval;
                if s.queued {
                    s.overrun = s.overrun.saturating_add(missed as u32 + 1);
                } else {
                    s.overrun = s.overrun.saturating_add(missed as u32);
                }
            } else {
                s.expires = 0;
            }
            self.arm(&mut s);
            if s.queued || self.notify == SIGEV_NONE {
                return;
            }
            s.queued = true;
        }
        DUE.lock().push(self.me.clone());
        ANY_DUE.store(true, Ordering::Release);
    }
}

impl Drop for PTimer {
    fn drop(&mut self) {
        if let Some(id) = self.st.lock().timer.take() {
            timer::cancel(id);
        }
    }
}

pub fn pending() -> bool {
    ANY_DUE.load(Ordering::Acquire)
}

/// Send the signals of the timers that expired (task context).
pub fn run_due() {
    ANY_DUE.store(false, Ordering::Release);
    let due: Vec<Weak<PTimer>> = core::mem::take(&mut *DUE.lock());
    for t in due.iter().filter_map(Weak::upgrade) {
        let overrun = {
            let mut s = t.st.lock();
            s.queued = false;
            core::mem::take(&mut s.overrun)
        };
        let mut info = SigInfo { signo: t.signo as i32, code: SI_TIMER, ..Default::default() };
        if t.id != ITIMER_ID {
            // si_timerid, si_overrun, si_value.
            info.pid = t.id;
            info.uid = overrun;
            info.value = Some(t.value);
        } else {
            info.code = signal::SI_KERNEL;
        }
        let task = (t.notify == SIGEV_THREAD_ID).then(|| TABLE.lock().tasks.get(&t.tid).cloned()).flatten();
        match task {
            Some(task) => signal::send_thread(&task, t.signo, info),
            None => {
                let _ = signal::send_process(t.pid, t.signo, info);
            }
        }
    }
}

/// A process went away (exit or exec): its timers go with it.
pub fn exit(pid: u32) {
    let gone: Vec<Arc<PTimer>> = {
        let mut m = TIMERS.lock();
        let keys: Vec<(u32, i32)> = m.range((pid, i32::MIN)..=(pid, i32::MAX)).map(|(k, _)| *k).collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    };
    drop(gone);
}

fn new_timer(pid: u32, id: i32, clock: u32, notify: i32, signo: u32, value: u64, tid: u32) -> Arc<PTimer> {
    Arc::new_cyclic(|me| PTimer { pid, id, clock, notify, signo, value, tid, me: me.clone(), st: IrqLock::new(State::default()) })
}

fn ns(s: u64, n: u64) -> u64 {
    s.saturating_mul(NSEC_PER_SEC).saturating_add(n)
}

fn get_timer(id: i32) -> KResult<Arc<PTimer>> {
    if id < 0 {
        return Err(EINVAL);
    }
    TIMERS.lock().get(&(current().pid(), id)).cloned().ok_or(EINVAL)
}

pub fn sys_timer_create(clock: u32, sevp: u64, idp: u64) -> KResult<u64> {
    if !matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM | CLOCK_BOOTTIME_ALARM | 2 | 3) {
        return Err(EINVAL);
    }
    let t = current();
    let pid = t.pid();
    let mm = t.mm();
    let (mut notify, mut signo, mut value, mut tid) = (SIGEV_SIGNAL, SIGALRM, 0u64, 0u32);
    if sevp != 0 {
        let mut b = [0u8; 20];
        mm.copy_in(sevp, &mut b)?;
        value = u64::from_le_bytes(b[..8].try_into().unwrap());
        signo = u32::from_le_bytes(b[8..12].try_into().unwrap());
        notify = i32::from_le_bytes(b[12..16].try_into().unwrap());
        tid = u32::from_le_bytes(b[16..20].try_into().unwrap());
        match notify {
            SIGEV_NONE => {}
            SIGEV_SIGNAL | SIGEV_THREAD_ID => {
                if signo == 0 || signo as usize > signal::NSIG {
                    return Err(EINVAL);
                }
                if notify == SIGEV_THREAD_ID && TABLE.lock().tasks.get(&tid).is_none_or(|x| x.pid() != pid) {
                    return Err(EINVAL);
                }
            }
            _ => return Err(EINVAL),
        }
    }
    let mut m = TIMERS.lock();
    if m.range((pid, 0)..=(pid, i32::MAX)).count() >= MAX_TIMERS {
        return Err(EAGAIN);
    }
    let id = (0..i32::MAX).find(|i| !m.contains_key(&(pid, *i))).ok_or(EAGAIN)?;
    if sevp == 0 {
        value = id as u64;
    }
    mm.copy_out(idp, &id.to_le_bytes())?;
    m.insert((pid, id), new_timer(pid, id, clock, notify, signo, value, tid));
    Ok(0)
}

pub fn sys_timer_settime(id: i32, flags: u32, new: u64, old: u64) -> KResult<u64> {
    if flags & !TIMER_ABSTIME != 0 {
        return Err(EINVAL);
    }
    let t = get_timer(id)?;
    let mm = current().mm();
    let (is, ins) = crate::syscall::time::read_timespec(&mm, new)?;
    let (vs, vns) = crate::syscall::time::read_timespec(&mm, new + 16)?;
    let (ov, oi) = t.set(flags, ns(vs, vns), ns(is, ins));
    if old != 0 {
        crate::syscall::poll::write_itimerspec(&mm, old, ov, oi)?;
    }
    Ok(0)
}

pub fn sys_timer_gettime(id: i32, cur: u64) -> KResult<u64> {
    let (v, i) = get_timer(id)?.get();
    crate::syscall::poll::write_itimerspec(&current().mm(), cur, v, i)?;
    Ok(0)
}

pub fn sys_timer_getoverrun(id: i32) -> KResult<u64> {
    Ok(get_timer(id)?.st.lock().overrun as u64)
}

pub fn sys_timer_delete(id: i32) -> KResult<u64> {
    get_timer(id)?;
    let t = TIMERS.lock().remove(&(current().pid(), id));
    drop(t);
    Ok(0)
}

/// Stored ITIMER_VIRTUAL / ITIMER_PROF values (never fire).
static OTHER_ITIMERS: Mutex<BTreeMap<(u32, u32), [u64; 2]>> = Mutex::new(BTreeMap::new());

fn read_timeval(b: &[u8]) -> KResult<u64> {
    let s = i64::from_le_bytes(b[..8].try_into().unwrap());
    let us = i64::from_le_bytes(b[8..16].try_into().unwrap());
    if s < 0 || !(0..1_000_000).contains(&us) {
        return Err(EINVAL);
    }
    Ok(ns(s as u64, us as u64 * 1000))
}

fn timeval_bytes(ns: u64, out: &mut [u8]) {
    // Round up to whole microseconds so an armed timer never reads 0.
    let us = ns.div_ceil(1000);
    out[..8].copy_from_slice(&((us / 1_000_000) as i64).to_le_bytes());
    out[8..16].copy_from_slice(&((us % 1_000_000) as i64).to_le_bytes());
}

/// setitimer/getitimer core: returns the old (value, interval).
fn itimer(which: u32, new: Option<(u64, u64)>) -> KResult<(u64, u64)> {
    let pid = current().pid();
    match which {
        0 => {
            let t = TIMERS
                .lock()
                .entry((pid, ITIMER_ID))
                .or_insert_with(|| new_timer(pid, ITIMER_ID, CLOCK_MONOTONIC, SIGEV_SIGNAL, SIGALRM, 0, 0))
                .clone();
            Ok(match new {
                Some((v, i)) => t.set(0, v, i),
                None => t.get(),
            })
        }
        1 | 2 => {
            let mut m = OTHER_ITIMERS.lock();
            let old = m.get(&(pid, which)).copied().unwrap_or([0, 0]);
            if let Some((v, i)) = new {
                m.insert((pid, which), [v, i]);
            }
            Ok((old[0], old[1]))
        }
        _ => Err(EINVAL),
    }
}

pub fn sys_setitimer(which: u32, new: u64, old: u64) -> KResult<u64> {
    let mm = current().mm();
    let val = if new != 0 {
        let mut b = [0u8; 32];
        mm.copy_in(new, &mut b)?;
        Some((read_timeval(&b[16..])?, read_timeval(&b[..16])?))
    } else {
        Some((0, 0))
    };
    let (ov, oi) = itimer(which, val)?;
    if old != 0 {
        let mut b = [0u8; 32];
        timeval_bytes(oi, &mut b[..16]);
        timeval_bytes(ov, &mut b[16..]);
        mm.copy_out(old, &b)?;
    }
    Ok(0)
}

pub fn sys_getitimer(which: u32, cur: u64) -> KResult<u64> {
    let (v, i) = itimer(which, None)?;
    let mut b = [0u8; 32];
    timeval_bytes(i, &mut b[..16]);
    timeval_bytes(v, &mut b[16..]);
    current().mm().copy_out(cur, &b)?;
    Ok(0)
}

pub fn sys_alarm(secs: u32) -> KResult<u64> {
    let (old, _) = itimer(0, Some((secs as u64 * NSEC_PER_SEC, 0)))?;
    // Remaining seconds, rounded (a pending alarm never reports 0).
    Ok(if old == 0 { 0 } else { old.div_ceil(NSEC_PER_SEC).max(1) })
}
