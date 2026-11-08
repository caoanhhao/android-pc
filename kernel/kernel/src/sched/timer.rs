//! Kernel timers: callbacks run from the BSP's timer interrupt once their
//! monotonic deadline (ns) has passed (10 ms granularity, never early).
//! Callbacks run in interrupt context: they may only take `IrqLock`s.

use alloc::sync::Weak;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::IrqLock;

pub trait TimerCallback: Send + Sync {
    /// `id` is what [`arm`] returned for this expiry.
    fn fire(&self, id: u64);
}

struct Timer {
    deadline: u64,
    id: u64,
    cb: Weak<dyn TimerCallback>,
}

static TIMERS: IrqLock<Vec<Timer>> = IrqLock::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Earliest armed deadline (u64::MAX when none): the tick's fast path.
static EARLIEST: AtomicU64 = AtomicU64::new(u64::MAX);

/// Call `cb.fire(id)` once `deadline` has passed (unless cancelled or the
/// owner is gone).
pub fn arm(deadline: u64, cb: Weak<dyn TimerCallback>) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut t = TIMERS.lock();
    t.push(Timer { deadline, id, cb });
    EARLIEST.fetch_min(deadline, Ordering::SeqCst);
    id
}

pub fn cancel(id: u64) {
    let old = {
        let mut t = TIMERS.lock();
        t.iter().position(|x| x.id == id).map(|i| t.swap_remove(i))
    };
    drop(old);
}

/// From the timer interrupt (BSP).
pub(super) fn run_expired() {
    let now = crate::time::monotonic_ns();
    if now < EARLIEST.load(Ordering::SeqCst) {
        return;
    }
    let expired: Vec<Timer> = {
        let mut t = TIMERS.lock();
        let mut out = Vec::new();
        let mut i = 0;
        while i < t.len() {
            if t[i].deadline <= now {
                out.push(t.swap_remove(i));
            } else {
                i += 1;
            }
        }
        EARLIEST.store(t.iter().map(|x| x.deadline).min().unwrap_or(u64::MAX), Ordering::SeqCst);
        out
    };
    for t in expired {
        if let Some(cb) = t.cb.upgrade() {
            cb.fire(t.id);
        }
    }
}
