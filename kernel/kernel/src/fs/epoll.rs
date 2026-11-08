//! epoll: an interest list of (fd, file) items and a ready list.
//!
//! Each item registers an [`ItemWaiter`] on its file's wait queue; a
//! wake-up puts the item on the ready list (if its events match) and wakes
//! the epoll's own queue (epoll_wait sleepers, and pollers or outer epolls
//! watching this epoll fd). `epoll_wait` takes items off the ready list,
//! polls each file and reports those with events; level-triggered items go
//! back on the list, edge-triggered ones wait for the next wake-up,
//! EPOLLONESHOT items are disabled until EPOLL_CTL_MOD.
//!
//! Items hold the file weakly: once the last descriptor (or other
//! reference) of a file is closed it disappears from every interest list
//! (dropped lazily, the next time the epoll looks at it). Nested epolls
//! work; loops and nesting deeper than 4 are refused (ELOOP).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;

use super::file::{FileOps, O_RDWR, OpenFile};
use super::poll::{POLLERR, POLLHUP, POLLIN, POLLOUT, READ_EVENTS, WaitQueue, Waiter};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};
use crate::sync::IrqLock;

pub const EPOLL_CTL_ADD: u32 = 1;
pub const EPOLL_CTL_DEL: u32 = 2;
pub const EPOLL_CTL_MOD: u32 = 3;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP: u32 = 1 << 29;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;
const PRIVATE_BITS: u32 = EPOLLWAKEUP | EPOLLONESHOT | EPOLLET | EPOLLEXCLUSIVE;
const EXCLUSIVE_OK_BITS: u32 = POLLIN | POLLOUT | POLLERR | POLLHUP | EPOLLWAKEUP | EPOLLET | EPOLLEXCLUSIVE;
const MAX_NESTS: usize = 4;

/// (fd, file address): an fd closed and reused for another file is a
/// different item.
type Key = (i32, usize);

struct Item {
    file: Weak<OpenFile>,
    /// Requested events (always with ERR|HUP) and flags; only PRIVATE_BITS
    /// left = disabled (ONESHOT fired).
    events: u32,
    data: u64,
    queued: bool,
    reg: Option<(Arc<WaitQueue>, u64)>,
}

#[derive(Default)]
struct Inner {
    items: BTreeMap<Key, Item>,
    ready: VecDeque<Key>,
}

pub struct Epoll {
    me: Weak<Epoll>,
    inner: IrqLock<Inner>,
    wq: Arc<WaitQueue>,
}

struct ItemWaiter {
    ep: Weak<Epoll>,
    key: Key,
}

impl Waiter for ItemWaiter {
    fn notify(&self, events: u32) {
        if let Some(ep) = self.ep.upgrade() {
            ep.mark_ready(self.key, events);
        }
    }
}

fn key_of(fd: i32, f: &Arc<OpenFile>) -> Key {
    (fd, Arc::as_ptr(f) as usize)
}

/// The epoll behind an open file, if it is one.
pub fn epoll_of(f: &OpenFile) -> Option<&Arc<Epoll>> {
    f.ops.as_any().downcast_ref::<EpollFile>().map(|e| &e.0)
}

impl Epoll {
    fn mark_ready(&self, key: Key, events: u32) {
        {
            let mut g = self.inner.lock();
            let inner = &mut *g;
            let Some(it) = inner.items.get_mut(&key) else { return };
            if it.events & !PRIVATE_BITS == 0 || (events != 0 && events & it.events == 0) || it.queued {
                return;
            }
            it.queued = true;
            inner.ready.push_back(key);
        }
        self.wq.wake(POLLIN);
    }

    /// Drop items whose file is gone; returns their wait registrations.
    fn purge(inner: &mut Inner) -> Vec<(Arc<WaitQueue>, u64)> {
        let dead: Vec<Key> = inner.items.iter().filter(|(_, it)| it.file.strong_count() == 0).map(|(k, _)| *k).collect();
        let mut regs = Vec::new();
        for k in dead {
            if let Some(it) = inner.items.remove(&k) {
                regs.extend(it.reg);
            }
        }
        inner.ready.retain(|k| inner.items.contains_key(k));
        regs
    }

    fn unregister(regs: Vec<(Arc<WaitQueue>, u64)>) {
        for (q, id) in regs {
            q.remove(id);
        }
    }

    /// Epolls this one watches.
    fn children(&self) -> Vec<Arc<Epoll>> {
        let files: Vec<Arc<OpenFile>> = self.inner.lock().items.values().filter_map(|it| it.file.upgrade()).collect();
        files.iter().filter_map(|f| epoll_of(f).cloned()).collect()
    }

    /// Longest chain of nested epolls below this one, or None if `target`
    /// is reachable (a loop).
    fn depth_avoiding(&self, target: &Epoll, limit: usize) -> Option<usize> {
        if core::ptr::eq(self, target) {
            return None;
        }
        if limit == 0 {
            return Some(usize::MAX / 2);
        }
        let mut d = 0;
        for c in self.children() {
            d = d.max(c.depth_avoiding(target, limit - 1)? + 1);
        }
        Some(d)
    }

    pub fn ctl(&self, op: u32, fd: i32, file: &Arc<OpenFile>, events: u32, data: u64) -> KResult<()> {
        let key = key_of(fd, file);
        if events & EPOLLEXCLUSIVE != 0
            && (op == EPOLL_CTL_MOD
                || (op == EPOLL_CTL_ADD && (epoll_of(file).is_some() || events & !EXCLUSIVE_OK_BITS != 0)))
        {
            return Err(EINVAL);
        }
        let events = events | POLLERR | POLLHUP;
        match op {
            EPOLL_CTL_ADD => {
                let queue = file.ops.wait_queue(file).ok_or(EPERM)?;
                if let Some(inner_ep) = epoll_of(file) {
                    // Loops are refused, and chains longer than MAX_NESTS.
                    let d = inner_ep.depth_avoiding(self, MAX_NESTS + 1).ok_or(ELOOP)?;
                    if d + 1 > MAX_NESTS {
                        return Err(ELOOP);
                    }
                }
                let dead = {
                    let mut g = self.inner.lock();
                    let dead = Self::purge(&mut g);
                    if g.items.contains_key(&key) {
                        drop(g);
                        Self::unregister(dead);
                        return Err(EEXIST);
                    }
                    g.items.insert(key, Item { file: Arc::downgrade(file), events, data, queued: false, reg: None });
                    dead
                };
                Self::unregister(dead);
                let id = queue.add(Arc::new(ItemWaiter { ep: self.me.clone(), key }));
                let stale = {
                    let mut g = self.inner.lock();
                    match g.items.get_mut(&key) {
                        Some(it) => {
                            it.reg = Some((queue.clone(), id));
                            false
                        }
                        None => true, // deleted meanwhile
                    }
                };
                if stale {
                    queue.remove(id);
                }
            }
            EPOLL_CTL_MOD => {
                let mut g = self.inner.lock();
                let it = g.items.get_mut(&key).ok_or(ENOENT)?;
                if it.events & EPOLLEXCLUSIVE != 0 {
                    return Err(EINVAL);
                }
                it.events = events;
                it.data = data;
            }
            EPOLL_CTL_DEL => {
                let it = {
                    let mut g = self.inner.lock();
                    let it = g.items.remove(&key).ok_or(ENOENT)?;
                    g.ready.retain(|k| *k != key);
                    it
                };
                Self::unregister(it.reg.into_iter().collect());
                return Ok(());
            }
            _ => return Err(EINVAL),
        }
        // ADD / MOD: queue the item at once if already ready.
        if file.poll() & events != 0 {
            self.mark_ready(key, 0);
        }
        Ok(())
    }

    /// Take up to `max` ready events: (events, data).
    fn harvest(&self, max: usize) -> Vec<(u32, u64)> {
        let (batch, dead) = {
            let mut g = self.inner.lock();
            let dead = Self::purge(&mut g);
            let inner = &mut *g;
            let mut batch = Vec::new();
            while let Some(k) = inner.ready.pop_front() {
                if let Some(it) = inner.items.get_mut(&k) {
                    it.queued = false;
                    batch.push((k, it.file.clone(), it.events));
                }
            }
            (batch, dead)
        };
        Self::unregister(dead);
        // Poll the files without the lock.
        let mut found: Vec<(Key, u32)> = Vec::new();
        let mut leftover: Vec<Key> = Vec::new();
        for (k, file, events) in batch {
            if found.len() >= max {
                leftover.push(k);
                continue;
            }
            let Some(f) = file.upgrade() else { continue };
            let rev = f.poll() & events;
            if rev != 0 {
                found.push((k, rev));
            }
        }
        let mut out = Vec::new();
        let mut g = self.inner.lock();
        let inner = &mut *g;
        // Items not looked at stay first in line.
        for k in leftover.into_iter().rev() {
            if let Some(it) = inner.items.get_mut(&k)
                && !it.queued
            {
                it.queued = true;
                inner.ready.push_front(k);
            }
        }
        for (k, rev) in found {
            let Some(it) = inner.items.get_mut(&k) else { continue };
            if it.events & !PRIVATE_BITS == 0 {
                continue; // disabled by a concurrent epoll_wait
            }
            out.push((rev, it.data));
            if it.events & EPOLLONESHOT != 0 {
                it.events &= PRIVATE_BITS;
            } else if it.events & EPOLLET == 0 && !it.queued {
                it.queued = true;
                inner.ready.push_back(k);
            }
        }
        out
    }

    /// epoll_wait: ready events, or an empty list on timeout.
    pub fn wait(&self, max: usize, deadline: Option<u64>) -> KResult<Vec<(u32, u64)>> {
        let mut out = Vec::new();
        let w = sched::wait_until(self.wq.key(), deadline, WaitMode::Interruptible, || {
            out = self.harvest(max);
            !out.is_empty()
        });
        match w {
            Wake::Ready | Wake::Timeout => Ok(out),
            Wake::Interrupted => Err(EINTR),
        }
    }

    /// Readiness of the epoll fd itself: some queued item has events.
    fn poll_self(&self) -> u32 {
        let batch: Vec<(Weak<OpenFile>, u32)> = {
            let g = self.inner.lock();
            g.ready.iter().filter_map(|k| g.items.get(k)).map(|it| (it.file.clone(), it.events)).collect()
        };
        let ready = batch.iter().any(|(f, ev)| f.upgrade().is_some_and(|f| f.poll() & ev != 0));
        if ready { READ_EVENTS } else { 0 }
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        let regs: Vec<(Arc<WaitQueue>, u64)> =
            core::mem::take(&mut self.inner.lock().items).into_values().filter_map(|it| it.reg).collect();
        Self::unregister(regs);
    }
}

pub struct EpollFile(pub Arc<Epoll>);

impl FileOps for EpollFile {
    fn poll(&self, _: &OpenFile) -> u32 {
        self.0.poll_self()
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[eventpoll]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        let items: Vec<(Key, u32, u64, Option<Arc<OpenFile>>)> =
            self.0.inner.lock().items.iter().map(|(k, it)| (*k, it.events, it.data, it.file.upgrade())).collect();
        let mut s = String::new();
        for ((fd, _), events, data, f) in items {
            let Some(f) = f else { continue };
            let m = f.inode.meta();
            let _ = writeln!(
                s,
                "tfd: {fd:8} events: {events:8x} data: {data:16x}  pos:{} ino:{:x} sdev:{:x}",
                f.pos(),
                m.ino,
                m.dev
            );
        }
        s
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn new() -> Arc<OpenFile> {
    let ep = Arc::new_cyclic(|me| Epoll { me: me.clone(), inner: IrqLock::new(Inner::default()), wq: WaitQueue::new() });
    OpenFile::new(super::anon_inode(), None, Box::new(EpollFile(ep)), O_RDWR)
}
