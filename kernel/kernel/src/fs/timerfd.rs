//! timerfd: a timer whose expirations are counted and read as a u64.
//!
//! Deadlines are kept on the monotonic clock: CLOCK_REALTIME absolute
//! times are converted with the boot epoch (the wall clock never jumps
//! here: there is no settimeofday yet). BOOTTIME equals MONOTONIC (no
//! suspend). Expirations are computed from the clock when the timer is
//! read or polled; a kernel timer (`sched::timer`) wakes pollers and
//! blocked readers at each expiry.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use core::any::Any;

use super::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use super::poll::{POLLIN, READ_EVENTS, WaitQueue};
use crate::errno::*;
use crate::sched::timer::{self, TimerCallback};
use crate::sched::{self, Wake, WaitMode};
use crate::sync::IrqLock;
use crate::syscall::time::{CLOCK_BOOTTIME, CLOCK_BOOTTIME_ALARM, CLOCK_MONOTONIC, CLOCK_REALTIME, CLOCK_REALTIME_ALARM};
use crate::time;

pub const TFD_TIMER_ABSTIME: u32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: u32 = 2;

#[derive(Default)]
struct State {
    /// Next expiry (monotonic ns), 0 = disarmed.
    expires: u64,
    interval: u64,
    /// Expirations not read yet.
    ticks: u64,
    /// The armed kernel timer.
    timer: Option<u64>,
    settime_flags: u32,
}

impl State {
    /// Count the expirations up to `now`.
    fn advance(&mut self, now: u64) {
        if self.expires == 0 || now < self.expires {
            return;
        }
        match (now - self.expires).checked_div(self.interval) {
            Some(late) => {
                let n = late + 1;
                self.ticks = self.ticks.saturating_add(n);
                self.expires += n * self.interval;
            }
            None => {
                self.ticks = self.ticks.saturating_add(1);
                self.expires = 0;
            }
        }
    }
}

pub struct TimerFd {
    clock: u32,
    me: Weak<TimerFd>,
    st: IrqLock<State>,
    wq: Arc<WaitQueue>,
}

impl TimerFd {
    fn arm(&self, s: &mut State) {
        if let Some(id) = s.timer.take() {
            timer::cancel(id);
        }
        if s.expires != 0 {
            s.timer = Some(timer::arm(s.expires, self.me.clone()));
        }
    }

    /// (value, interval) in ns: time to the next expiry, 0 if disarmed.
    pub fn get(&self) -> (u64, u64) {
        let now = time::monotonic_ns();
        let mut s = self.st.lock();
        s.advance(now);
        let left = if s.expires == 0 { 0 } else { s.expires - now };
        (left, s.interval)
    }

    /// timerfd_settime; `value` is relative unless TFD_TIMER_ABSTIME (then
    /// on this timer's clock). Returns the old (value, interval).
    pub fn set(&self, flags: u32, value: u64, interval: u64) -> (u64, u64) {
        let old = self.get();
        let now = time::monotonic_ns();
        let expires = if value == 0 {
            0
        } else if flags & TFD_TIMER_ABSTIME == 0 {
            now.saturating_add(value)
        } else if matches!(self.clock, CLOCK_REALTIME | CLOCK_REALTIME_ALARM) {
            // Realtime -> monotonic; a time in the past expires at once.
            (value as i128 - time::boot_epoch_ns()).clamp(1, u64::MAX as i128) as u64
        } else {
            value
        };
        {
            let mut s = self.st.lock();
            s.expires = expires;
            s.interval = if expires == 0 { 0 } else { interval };
            s.ticks = 0;
            s.settime_flags = flags;
            s.advance(now);
            self.arm(&mut s);
        }
        self.wq.wake(0);
        old
    }
}

impl TimerCallback for TimerFd {
    fn fire(&self, id: u64) {
        let ready = {
            let mut s = self.st.lock();
            if s.timer != Some(id) {
                return;
            }
            s.timer = None;
            s.advance(time::monotonic_ns());
            self.arm(&mut s);
            s.ticks > 0
        };
        if ready {
            self.wq.wake(POLLIN);
        }
    }
}

impl Drop for TimerFd {
    fn drop(&mut self) {
        if let Some(id) = self.st.lock().timer.take() {
            timer::cancel(id);
        }
    }
}

impl FileOps for TimerFd {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let nonblock = f.nonblock();
        let mut got = 0;
        let mut again = false;
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            let mut s = self.st.lock();
            s.advance(time::monotonic_ns());
            got = core::mem::take(&mut s.ticks);
            again = got == 0 && nonblock;
            got > 0 || nonblock
        });
        match w {
            _ if again => Err(EAGAIN),
            Wake::Ready => {
                buf[..8].copy_from_slice(&got.to_ne_bytes());
                Ok(8)
            }
            _ => Err(ERESTARTSYS),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let mut s = self.st.lock();
        s.advance(time::monotonic_ns());
        if s.ticks > 0 { READ_EVENTS } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[timerfd]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        let (value, interval) = self.get();
        let (ticks, flags) = {
            let s = self.st.lock();
            (s.ticks, s.settime_flags)
        };
        let ts = |ns: u64| format!("({}, {})", ns / time::NSEC_PER_SEC, ns % time::NSEC_PER_SEC);
        format!(
            "clockid: {}\nticks: {}\nsettime flags: 0{:o}\nit_value: {}\nit_interval: {}\n",
            self.clock,
            ticks,
            flags,
            ts(value),
            ts(interval)
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn valid_clock(clock: u32) -> bool {
    matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM | CLOCK_BOOTTIME_ALARM)
}

/// A new timerfd file; `flags` may hold O_NONBLOCK.
pub fn new(clock: u32, flags: u32) -> Arc<OpenFile> {
    let ops = Arc::new_cyclic(|me| TimerFd { clock, me: me.clone(), st: IrqLock::new(State::default()), wq: WaitQueue::new() });
    OpenFile::new(super::anon_inode(), None, Box::new(TimerFdFile(ops)), O_RDWR | (flags & O_NONBLOCK))
}

/// The file operations hold the timer through an Arc so kernel timers can
/// keep a weak reference to it.
pub struct TimerFdFile(pub Arc<TimerFd>);

impl FileOps for TimerFdFile {
    fn read(&self, f: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        self.0.read(f, pos, buf)
    }

    fn poll(&self, f: &OpenFile) -> u32 {
        self.0.poll(f)
    }

    fn wait_queue(&self, f: &OpenFile) -> Option<Arc<WaitQueue>> {
        self.0.wait_queue(f)
    }

    fn anon_name(&self) -> Option<&'static str> {
        self.0.anon_name()
    }

    fn fdinfo(&self, f: &OpenFile) -> String {
        self.0.fdinfo(f)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
