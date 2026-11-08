//! Readiness: poll event bits, wait queues with wake-up callbacks, and the
//! generic poll loop behind poll/ppoll/select/pselect6.
//!
//! Every file that can block has a [`WaitQueue`] (returned by
//! `FileOps::wait_queue`) and reports its state with `FileOps::poll`.
//! Waking a queue does two things:
//! - wakes tasks sleeping in `sched::wait_until(queue.key(), ...)` (blocked
//!   read/write on the object itself);
//! - calls every registered [`Waiter`] (a poll()ing task, an epoll
//!   interest item, ...). Callbacks may run in interrupt context (timerfd
//!   expiry), so they only take `IrqLock`s.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::file::OpenFile;
use crate::errno::*;
use crate::sched::{self, Wake, WaitKey, WaitMode};
use crate::sync::IrqLock;

pub const POLLIN: u32 = 0x1;
pub const POLLPRI: u32 = 0x2;
pub const POLLOUT: u32 = 0x4;
pub const POLLERR: u32 = 0x8;
pub const POLLHUP: u32 = 0x10;
pub const POLLNVAL: u32 = 0x20;
pub const POLLRDNORM: u32 = 0x40;
pub const POLLRDBAND: u32 = 0x80;
pub const POLLWRNORM: u32 = 0x100;
pub const POLLWRBAND: u32 = 0x200;
#[allow(dead_code)] // sockets (B4b)
pub const POLLRDHUP: u32 = 0x2000;

/// What a file without its own poll method reports (Linux DEFAULT_POLLMASK).
pub const DEFAULT_POLLMASK: u32 = POLLIN | POLLOUT | POLLRDNORM | POLLWRNORM;
pub const READ_EVENTS: u32 = POLLIN | POLLRDNORM;
pub const WRITE_EVENTS: u32 = POLLOUT | POLLWRNORM;

const WAIT_QUEUE: u64 = 0x5751_0000_0000_0000;
const WAIT_POLL: u64 = 0x504f_4c4c_0000_0000;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A wake-up callback registered on a wait queue.
pub trait Waiter: Send + Sync {
    /// `events` hints at what changed (0 = unknown); may run in interrupt
    /// context.
    fn notify(&self, events: u32);
}

pub struct WaitQueue {
    id: u64,
    entries: IrqLock<Vec<(u64, Arc<dyn Waiter>)>>,
}

impl WaitQueue {
    pub fn new() -> Arc<WaitQueue> {
        Arc::new(WaitQueue { id: next_id(), entries: IrqLock::new(Vec::new()) })
    }

    /// Sleep key for tasks blocked on the object itself.
    pub fn key(&self) -> WaitKey {
        (WAIT_QUEUE, self.id)
    }

    /// Register a callback; returns the id for [`WaitQueue::remove`].
    pub fn add(&self, w: Arc<dyn Waiter>) -> u64 {
        let id = next_id();
        self.entries.lock().push((id, w));
        id
    }

    pub fn remove(&self, id: u64) {
        let old = {
            let mut e = self.entries.lock();
            e.iter().position(|(i, _)| *i == id).map(|p| e.remove(p))
        };
        drop(old);
    }

    /// Wake sleepers and run the callbacks (outside the queue lock).
    pub fn wake(&self, events: u32) {
        sched::wake_all(self.key());
        let ws: Vec<Arc<dyn Waiter>> = {
            let e = self.entries.lock();
            if e.is_empty() {
                return;
            }
            e.iter().map(|(_, w)| w.clone()).collect()
        };
        for w in ws {
            w.notify(events);
        }
    }
}

/// Waiter that wakes one sleep key (a task in poll/select).
struct KeyWaker(WaitKey);

impl Waiter for KeyWaker {
    fn notify(&self, _: u32) {
        sched::wake_all(self.0);
    }
}

/// A task waiting on several wait queues at once.
pub struct Poller {
    key: WaitKey,
    waker: Arc<dyn Waiter>,
    regs: Vec<(Arc<WaitQueue>, u64)>,
}

impl Poller {
    pub fn new() -> Poller {
        let key = (WAIT_POLL, next_id());
        Poller { key, waker: Arc::new(KeyWaker(key)), regs: Vec::new() }
    }

    /// Get woken when `f` changes state.
    pub fn watch(&mut self, f: &OpenFile) {
        if let Some(q) = f.ops.wait_queue(f)
            && !self.regs.iter().any(|(r, _)| Arc::ptr_eq(r, &q))
        {
            let id = q.add(self.waker.clone());
            self.regs.push((q, id));
        }
    }

    /// Sleep until `ready()` (re-checked after every wake-up), the
    /// deadline, or a signal.
    pub fn wait(&self, deadline: Option<u64>, ready: impl FnMut() -> bool) -> Wake {
        sched::wait_until(self.key, deadline, WaitMode::Interruptible, ready)
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        for (q, id) in self.regs.drain(..) {
            q.remove(id);
        }
    }
}

/// One pollfd: the file (None = POLLNVAL, or ignored when `fd < 0`),
/// requested events, result.
pub struct PollEntry {
    pub fd: i32,
    pub file: Option<Arc<OpenFile>>,
    pub events: u32,
    pub revents: u32,
}

fn scan(entries: &mut [PollEntry]) -> usize {
    let mut n = 0;
    for e in entries.iter_mut() {
        e.revents = match &e.file {
            _ if e.fd < 0 => 0,
            None => POLLNVAL,
            Some(f) => f.poll() & (e.events | POLLERR | POLLHUP),
        };
        if e.revents != 0 {
            n += 1;
        }
    }
    n
}

/// The poll loop: fills `revents`, returns how many entries are ready
/// (0 on timeout) or EINTR.
pub fn do_poll(entries: &mut [PollEntry], deadline: Option<u64>) -> KResult<usize> {
    let mut poller = Poller::new();
    if deadline != Some(0) {
        for f in entries.iter().filter_map(|e| e.file.as_ref()) {
            poller.watch(f);
        }
    }
    let mut n = 0;
    match poller.wait(deadline, || {
        n = scan(entries);
        n > 0
    }) {
        Wake::Ready => Ok(n),
        Wake::Timeout => Ok(0),
        Wake::Interrupted => Err(EINTR),
    }
}
