//! Binder driver state: processes (one per open of a binder device),
//! threads, nodes, references, death notifications, transactions and each
//! process's buffer allocator.
//!
//! Everything lives in one table behind one lock ([`lock`]). Objects refer
//! to each other by id; a work item whose object has gone away is skipped
//! when it is dequeued. Rules that keep the lock deadlock-free:
//! - no user memory is touched while it is held (callers copy in before
//!   and copy out after), so the address-space lock is never taken under it;
//! - open files are never dropped under it: they go to the "grave" and are
//!   dropped (or closed, for descriptors) by [`Guard`] after unlocking,
//!   because the last close of a binder file takes the lock again.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use spin::{Mutex, MutexGuard};

use super::abi::*;
use crate::errno::*;
use crate::fs::file::OpenFile;
use crate::fs::poll::{POLLIN, WaitQueue};
use crate::mm::shmem::Shmem;
use crate::sched;

pub type Id = u64;

/// Sleep key namespace of binder threads: (WAIT_KEY, thread key).
pub const WAIT_KEY: u64 = 0x4249_4e44_0000_0000;

pub const LOOPER_REGISTERED: u32 = 1;
pub const LOOPER_ENTERED: u32 = 2;
pub const LOOPER_EXITED: u32 = 4;
pub const LOOPER_INVALID: u32 = 8;
pub const LOOPER_POLL: u32 = 0x10;

/// What can sit in a thread's or a process's todo list.
#[derive(Clone, Copy, Debug)]
pub enum Work {
    Txn(Id),
    /// BR_TRANSACTION_COMPLETE.
    Complete,
    /// An error from this thread's own command (BR_FAILED_REPLY, ...);
    /// further write commands are skipped until it is read.
    ReturnError(u32),
    /// The reply to this thread's call failed (BR_DEAD_REPLY, ...).
    ReplyError(u32),
    /// The node's reference state must be reported to its owner.
    Node(Id),
    Death(Id),
}

pub struct Thread {
    /// Sleep key (WAIT_KEY, key).
    pub key: u64,
    pub looper: u32,
    pub todo: VecDeque<Work>,
    /// Top of the transaction stack (incoming or outgoing).
    pub stack: Option<Id>,
    /// Return from the next read without waiting (new threads).
    pub need_return: bool,
    /// Sleeping until process work arrives, and not yet picked for any.
    pub waiting_proc: bool,
    pub error_pending: bool,
    /// binder_extended_error: (id, command, param).
    pub ee: (u32, u32, i32),
}

impl Thread {
    fn new(key: u64) -> Thread {
        Thread {
            key,
            looper: 0,
            todo: VecDeque::new(),
            stack: None,
            need_return: true,
            waiting_proc: false,
            error_pending: false,
            ee: (0, BR_OK, 0),
        }
    }

    /// Whether this thread takes work from the process todo list.
    pub fn proc_work_ok(&self) -> bool {
        self.stack.is_none() && self.todo.is_empty() && self.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0
    }
}

pub struct Node {
    /// None once the owning process is gone (a dead node).
    pub owner: Option<Id>,
    pub ptr: u64,
    pub cookie: u64,
    /// Strong references from other processes' refs.
    pub internal_strong: u32,
    /// References held by the kernel for the owner (in-flight buffers,
    /// unacknowledged BR_INCREFS/BR_ACQUIRE).
    pub local_strong: u32,
    pub local_weak: u32,
    /// What the owner has been told (BR_ACQUIRE / BR_INCREFS sent).
    pub has_strong: bool,
    pub has_weak: bool,
    pub pending_strong: bool,
    pub pending_weak: bool,
    pub accept_fds: bool,
    pub sec_ctx: bool,
    /// A Work::Node for it is queued somewhere.
    pub queued: bool,
    /// A oneway transaction to it is being processed; later ones wait in
    /// `async_todo` until its buffer is freed.
    pub has_async: bool,
    pub async_todo: VecDeque<Id>,
    /// Processes holding a ref to it.
    pub refs: BTreeSet<Id>,
}

pub struct Ref {
    pub node: Id,
    pub strong: u32,
    pub weak: u32,
    pub death: Option<Id>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeathKind {
    /// BR_DEAD_BINDER when the node dies.
    Dead,
    /// BR_DEAD_BINDER was (or will be) sent and the client cleared the
    /// notification: BR_CLEAR_DEATH_NOTIFICATION_DONE after
    /// BC_DEAD_BINDER_DONE.
    DeadAndClear,
    /// BR_CLEAR_DEATH_NOTIFICATION_DONE.
    Clear,
}

pub struct Death {
    pub proc: Id,
    pub cookie: u64,
    pub kind: DeathKind,
    pub queued: bool,
    /// BR_DEAD_BINDER delivered, waiting for BC_DEAD_BINDER_DONE.
    pub delivered: bool,
}

pub struct Txn {
    /// Sending thread of a synchronous call (None for oneway calls and
    /// replies, or once the sender is gone).
    pub from: Option<(Id, u32)>,
    pub sender_pid: u32,
    pub to_proc: Id,
    /// Thread processing it (set when a synchronous call is read).
    pub to_thread: Option<u32>,
    /// The sender's previous stack top.
    pub from_parent: Option<Id>,
    /// The receiver's previous stack top.
    pub to_parent: Option<Id>,
    pub code: u32,
    pub flags: u32,
    pub reply: bool,
    pub node: Option<Id>,
    /// Offset of its buffer in `to_proc`'s mapping.
    pub buf: Option<u64>,
}

impl Txn {
    pub fn sync_call(&self) -> bool {
        !self.reply && self.flags & TF_ONE_WAY == 0
    }
}

/// Something the buffer holds a reference on, dropped when it is freed.
pub enum Held {
    /// A local reference on a node of the buffer's owner.
    Node { node: Id, strong: bool },
    Ref { desc: u32, strong: bool },
}

pub struct FdFixup {
    /// Offset of the 32-bit fd slot in the buffer.
    pub off: u64,
    pub file: Arc<OpenFile>,
    pub fda: bool,
}

pub struct Buffer {
    pub size: u64,
    pub data_size: u64,
    pub offsets_size: u64,
    pub txn: Option<Id>,
    /// Handed to user space (BC_FREE_BUFFER allowed).
    pub user_free: bool,
    pub oneway: bool,
    pub async_node: Option<Id>,
    pub held: Vec<Held>,
    /// Files to install in the receiver when it reads the transaction.
    pub fds: Vec<FdFixup>,
    /// Descriptors installed from fd arrays, closed with the buffer.
    pub fda_fds: Vec<i32>,
    /// Offset of the security context string.
    pub secctx: Option<u64>,
}

/// The process's mmap()ed buffer area and its allocator.
pub struct Mapping {
    pub shmem: Arc<Shmem>,
    pub base: u64,
    /// offset -> length of free ranges.
    free: BTreeMap<u64, u64>,
    pub used: BTreeMap<u64, Buffer>,
    /// Oneway transactions may use at most half of the area.
    pub async_free: u64,
}

pub fn align8(n: u64) -> u64 {
    (n + 7) & !7
}

impl Mapping {
    pub fn new(shmem: Arc<Shmem>, base: u64, size: u64) -> Mapping {
        let mut free = BTreeMap::new();
        free.insert(0, size);
        Mapping { shmem, base, free, used: BTreeMap::new(), async_free: size / 2 }
    }

    /// Best-fit allocation of `size` bytes (rounded up to 8, at least 8).
    pub fn alloc(&mut self, size: u64) -> Option<u64> {
        let size = align8(size.max(8));
        let (&off, &len) = self.free.iter().filter(|&(_, &l)| l >= size).min_by_key(|&(_, &l)| l)?;
        self.free.remove(&off);
        if len > size {
            self.free.insert(off + size, len - size);
        }
        Some(off)
    }

    fn free_range(&mut self, mut off: u64, mut len: u64) {
        if let Some((&n, &nl)) = self.free.range(off + len..).next()
            && n == off + len
        {
            self.free.remove(&n);
            len += nl;
        }
        if let Some((&p, &pl)) = self.free.range(..off).next_back()
            && p + pl == off
        {
            self.free.remove(&p);
            off = p;
            len += pl;
        }
        self.free.insert(off, len);
    }
}

pub struct Proc {
    pub pid: u32,
    /// Device context (minor number): binder, hwbinder, vndbinder, ...
    pub ctx: u32,
    pub threads: BTreeMap<u32, Thread>,
    /// User pointer -> node.
    pub nodes: BTreeMap<u64, Id>,
    pub refs: BTreeMap<u32, Ref>,
    pub desc_of: BTreeMap<Id, u32>,
    pub todo: VecDeque<Work>,
    /// Woken on new work: pollers and epoll.
    pub wq: Arc<WaitQueue>,
    pub map: Option<Mapping>,
    pub max_threads: u32,
    /// BR_SPAWN_LOOPER sent, BC_REGISTER_LOOPER not yet received.
    pub requested: u32,
    pub started: u32,
    pub delivered: Vec<Id>,
    pub spam_detect: bool,
}

pub enum Grave {
    File(Arc<OpenFile>),
    /// A descriptor of the current process to close.
    Fd(i32),
}

pub struct State {
    next: u64,
    pub procs: BTreeMap<Id, Proc>,
    pub nodes: BTreeMap<Id, Node>,
    pub txns: BTreeMap<Id, Txn>,
    pub deaths: BTreeMap<Id, Death>,
    /// Context manager node of each context.
    pub mgr: BTreeMap<u32, Id>,
    pub grave: Vec<Grave>,
}

static STATE: Mutex<State> = Mutex::new(State {
    next: 0,
    procs: BTreeMap::new(),
    nodes: BTreeMap::new(),
    txns: BTreeMap::new(),
    deaths: BTreeMap::new(),
    mgr: BTreeMap::new(),
    grave: Vec::new(),
});

/// The locked state; drops the grave after unlocking.
pub struct Guard(Option<MutexGuard<'static, State>>);

pub fn lock() -> Guard {
    Guard(Some(STATE.lock()))
}

impl Deref for Guard {
    type Target = State;
    fn deref(&self) -> &State {
        self.0.as_ref().unwrap()
    }
}

impl DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut State {
        self.0.as_mut().unwrap()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let grave = core::mem::take(&mut self.0.as_mut().unwrap().grave);
        self.0 = None;
        for g in grave {
            match g {
                Grave::File(f) => drop(f),
                Grave::Fd(fd) => {
                    let _ = crate::process::current().process.files().close(fd);
                }
            }
        }
    }
}

impl State {
    pub fn next_id(&mut self) -> Id {
        self.next += 1;
        self.next
    }

    pub fn new_proc(&mut self, pid: u32, ctx: u32) -> Id {
        let id = self.next_id();
        self.procs.insert(
            id,
            Proc {
                pid,
                ctx,
                threads: BTreeMap::new(),
                nodes: BTreeMap::new(),
                refs: BTreeMap::new(),
                desc_of: BTreeMap::new(),
                todo: VecDeque::new(),
                wq: WaitQueue::new(),
                map: None,
                max_threads: 0,
                requested: 0,
                started: 0,
                delivered: Vec::new(),
                spam_detect: false,
            },
        );
        id
    }

    pub fn proc(&mut self, p: Id) -> &mut Proc {
        self.procs.get_mut(&p).expect("binder proc")
    }

    /// The thread `tid` of `p`, created on first use.
    pub fn thread(&mut self, p: Id, tid: u32) -> &mut Thread {
        if !self.proc(p).threads.contains_key(&tid) {
            let key = self.next_id();
            self.proc(p).threads.insert(tid, Thread::new(key));
        }
        self.proc(p).threads.get_mut(&tid).unwrap()
    }

    pub fn thread_opt(&mut self, p: Id, tid: u32) -> Option<&mut Thread> {
        self.procs.get_mut(&p)?.threads.get_mut(&tid)
    }

    pub fn has_work(&self, p: Id, tid: u32, proc_work: bool) -> bool {
        let Some(pr) = self.procs.get(&p) else { return true };
        let Some(t) = pr.threads.get(&tid) else { return true };
        !t.todo.is_empty() || t.need_return || (proc_work && !pr.todo.is_empty())
    }

    pub fn buffer(&mut self, p: Id, off: u64) -> Option<&mut Buffer> {
        self.procs.get_mut(&p)?.map.as_mut()?.used.get_mut(&off)
    }

    // -----------------------------------------------------------------
    // Work queues

    pub fn queue_proc(&mut self, p: Id, w: Work) {
        let Some(pr) = self.procs.get_mut(&p) else { return };
        pr.todo.push_back(w);
        // Wake one idle looper; it stops counting as idle so the next
        // work item picks another one.
        if let Some(t) = pr.threads.values_mut().find(|t| t.waiting_proc && t.proc_work_ok()) {
            t.waiting_proc = false;
            sched::wake_all((WAIT_KEY, t.key));
        }
        pr.wq.wake(POLLIN);
    }

    pub fn queue_thread(&mut self, p: Id, tid: u32, w: Work, wake: bool) {
        let Some(pr) = self.procs.get_mut(&p) else { return };
        let Some(t) = pr.threads.get_mut(&tid) else { return };
        if let Work::ReturnError(_) = w {
            t.error_pending = true;
        }
        t.todo.push_back(w);
        if wake {
            sched::wake_all((WAIT_KEY, t.key));
            pr.wq.wake(POLLIN);
        }
    }

    // -----------------------------------------------------------------
    // Nodes and references

    pub fn new_node(&mut self, p: Id, ptr: u64, cookie: u64, flags: u32) -> Id {
        let id = self.next_id();
        self.nodes.insert(
            id,
            Node {
                owner: Some(p),
                ptr,
                cookie,
                internal_strong: 0,
                local_strong: 0,
                local_weak: 0,
                has_strong: false,
                has_weak: false,
                pending_strong: false,
                pending_weak: false,
                accept_fds: flags & FLAT_BINDER_FLAG_ACCEPTS_FDS != 0,
                sec_ctx: flags & FLAT_BINDER_FLAG_TXN_SECURITY_CTX != 0,
                queued: false,
                has_async: false,
                async_todo: VecDeque::new(),
                refs: BTreeSet::new(),
            },
        );
        self.proc(p).nodes.insert(ptr, id);
        id
    }

    /// Queue a node-state report for the owner: on the given thread of
    /// the owner (the one sending the object), else on the owner's todo.
    fn queue_node(&mut self, n: Id, target: Option<(Id, u32)>) {
        let Some(node) = self.nodes.get_mut(&n) else { return };
        let Some(owner) = node.owner else { return };
        if node.queued {
            return;
        }
        node.queued = true;
        match target {
            Some((p, tid)) if p == owner && self.thread_opt(p, tid).is_some() => {
                self.queue_thread(p, tid, Work::Node(n), false)
            }
            _ => self.queue_proc(owner, Work::Node(n)),
        }
    }

    pub fn inc_node(&mut self, n: Id, strong: bool, internal: bool, target: Option<(Id, u32)>) {
        let Some(node) = self.nodes.get_mut(&n) else { return };
        let report = if strong {
            if internal {
                node.internal_strong += 1;
            } else {
                node.local_strong += 1;
            }
            !node.has_strong
        } else {
            if !internal {
                node.local_weak += 1;
            }
            !node.has_weak
        };
        if report {
            self.queue_node(n, target);
        }
    }

    pub fn dec_node(&mut self, n: Id, strong: bool, internal: bool) {
        let Some(node) = self.nodes.get_mut(&n) else { return };
        if strong {
            if internal {
                node.internal_strong = node.internal_strong.saturating_sub(1);
            } else {
                node.local_strong = node.local_strong.saturating_sub(1);
            }
            if node.internal_strong > 0 || node.local_strong > 0 {
                return;
            }
        } else {
            if !internal {
                node.local_weak = node.local_weak.saturating_sub(1);
            }
            if node.local_weak > 0 || !node.refs.is_empty() {
                return;
            }
        }
        if node.owner.is_some() && (node.has_strong || node.has_weak) {
            self.queue_node(n, None);
        } else if node.refs.is_empty() && node.local_strong == 0 && node.local_weak == 0 {
            self.remove_node(n);
        }
    }

    pub fn remove_node(&mut self, n: Id) {
        if let Some(node) = self.nodes.remove(&n)
            && let Some(o) = node.owner
            && let Some(p) = self.procs.get_mut(&o)
            && p.nodes.get(&node.ptr) == Some(&n)
        {
            p.nodes.remove(&node.ptr);
        }
    }

    /// The descriptor of `p`'s ref to node `n`, created (with no counts)
    /// if needed. The context manager is always handle 0.
    pub fn ref_for_node(&mut self, p: Id, n: Id) -> u32 {
        if let Some(&d) = self.proc(p).desc_of.get(&n) {
            return d;
        }
        let ctx = self.proc(p).ctx;
        let mut d = if self.mgr.get(&ctx) == Some(&n) { 0 } else { 1 };
        let pr = self.proc(p);
        while pr.refs.contains_key(&d) {
            d += 1;
        }
        pr.refs.insert(d, Ref { node: n, strong: 0, weak: 0, death: None });
        pr.desc_of.insert(n, d);
        if let Some(node) = self.nodes.get_mut(&n) {
            node.refs.insert(p);
        }
        d
    }

    pub fn inc_ref(&mut self, p: Id, d: u32, strong: bool, target: Option<(Id, u32)>) -> KResult<()> {
        let r = self.proc(p).refs.get_mut(&d).ok_or(EINVAL)?;
        let n = r.node;
        let first = if strong {
            r.strong += 1;
            r.strong == 1
        } else {
            r.weak += 1;
            r.weak == 1
        };
        if first {
            self.inc_node(n, strong, true, target);
        }
        Ok(())
    }

    pub fn dec_ref(&mut self, p: Id, d: u32, strong: bool) -> KResult<()> {
        let r = self.procs.get_mut(&p).ok_or(EINVAL)?.refs.get_mut(&d).ok_or(EINVAL)?;
        let n = r.node;
        if strong {
            if r.strong == 0 {
                return Err(EINVAL);
            }
            r.strong -= 1;
            if r.strong == 0 {
                self.dec_node(n, true, true);
            }
        } else {
            if r.weak == 0 {
                return Err(EINVAL);
            }
            r.weak -= 1;
        }
        if self.proc(p).refs.get(&d).is_some_and(|r| r.strong == 0 && r.weak == 0) {
            self.cleanup_ref(p, d);
        }
        Ok(())
    }

    /// Drop `p`'s ref `d` with whatever counts it still has.
    pub fn cleanup_ref(&mut self, p: Id, d: u32) {
        let Some(pr) = self.procs.get_mut(&p) else { return };
        let Some(r) = pr.refs.remove(&d) else { return };
        pr.desc_of.remove(&r.node);
        if r.strong > 0 {
            self.dec_node(r.node, true, true);
        }
        if let Some(node) = self.nodes.get_mut(&r.node) {
            node.refs.remove(&p);
        }
        self.dec_node(r.node, false, true);
        if let Some(dt) = r.death {
            self.deaths.remove(&dt);
        }
    }

    /// Queue a death work item on its process (or one of its threads).
    pub fn queue_death(&mut self, d: Id, thread: Option<u32>) {
        let Some(dt) = self.deaths.get_mut(&d) else { return };
        if dt.queued {
            return;
        }
        dt.queued = true;
        let p = dt.proc;
        match thread {
            Some(tid) => self.queue_thread(p, tid, Work::Death(d), false),
            None => self.queue_proc(p, Work::Death(d)),
        }
    }

    // -----------------------------------------------------------------
    // Transactions and buffers

    pub fn free_txn(&mut self, t: Id) {
        if let Some(tx) = self.txns.remove(&t)
            && let Some(off) = tx.buf
            && let Some(b) = self.buffer(tx.to_proc, off)
        {
            b.txn = None;
        }
    }

    /// Fail a synchronous call: its sender gets `code`. If the sender is
    /// gone, the call it was serving fails in turn.
    pub fn send_failed_reply(&mut self, mut t: Id, code: u32) {
        loop {
            let Some(tx) = self.txns.get(&t) else { return };
            let (from, from_parent) = (tx.from, tx.from_parent);
            if let Some((fp, ftid)) = from
                && let Some(th) = self.thread_opt(fp, ftid)
            {
                if th.stack == Some(t) {
                    th.stack = from_parent;
                }
                self.queue_thread(fp, ftid, Work::ReplyError(code), true);
                self.free_txn(t);
                return;
            }
            self.free_txn(t);
            match from_parent {
                Some(n) => t = n,
                None => return,
            }
        }
    }

    /// Drop a transaction that was never read: free its buffer and fail
    /// the call if someone waits for a reply.
    pub fn discard_txn(&mut self, t: Id, code: u32) {
        let Some(tx) = self.txns.get(&t) else { return };
        let (p, off, sync) = (tx.to_proc, tx.buf, tx.sync_call());
        if let Some(off) = off {
            self.release_buffer(p, off, false);
        }
        if sync {
            self.send_failed_reply(t, code);
        } else {
            self.free_txn(t);
        }
    }

    /// Free buffer `off` of `p`, dropping the references and files it
    /// holds. `current_owner`: called by `p`'s own process, so installed
    /// fd-array descriptors are closed too.
    pub fn release_buffer(&mut self, p: Id, off: u64, current_owner: bool) {
        let Some(m) = self.procs.get_mut(&p).and_then(|pr| pr.map.as_mut()) else { return };
        let Some(b) = m.used.remove(&off) else { return };
        m.free_range(off, b.size);
        if b.oneway {
            m.async_free += b.size;
        }
        if let Some(t) = b.txn
            && let Some(tx) = self.txns.get_mut(&t)
        {
            tx.buf = None;
        }
        if let Some(n) = b.async_node
            && let Some(node) = self.nodes.get_mut(&n)
        {
            match node.async_todo.pop_front() {
                Some(next) => {
                    if let Some(o) = node.owner {
                        self.queue_proc(o, Work::Txn(next));
                    }
                }
                None => node.has_async = false,
            }
        }
        for h in b.held {
            match h {
                Held::Node { node, strong } => self.dec_node(node, strong, false),
                Held::Ref { desc, strong } => {
                    let _ = self.dec_ref(p, desc, strong);
                }
            }
        }
        for f in b.fds {
            self.grave.push(Grave::File(f.file));
        }
        if current_owner {
            self.grave.extend(b.fda_fds.into_iter().map(Grave::Fd));
        }
    }

    // -----------------------------------------------------------------
    // Release

    /// BINDER_THREAD_EXIT, or the process going away: fail the call this
    /// thread was serving, detach it from calls it made, drop its work.
    pub fn thread_release(&mut self, p: Id, tid: u32) {
        let Some(th) = self.procs.get_mut(&p).and_then(|pr| pr.threads.remove(&tid)) else { return };
        let mut send_reply = None;
        let mut cur = th.stack;
        if let Some(t) = cur
            && self.txns.get(&t).is_some_and(|x| x.to_proc == p && x.to_thread == Some(tid))
        {
            send_reply = Some(t);
        }
        while let Some(t) = cur {
            let Some(tx) = self.txns.get_mut(&t) else { break };
            if tx.to_proc == p && tx.to_thread == Some(tid) {
                tx.to_thread = None;
                cur = tx.to_parent;
                if let Some(off) = tx.buf.take()
                    && let Some(b) = self.buffer(p, off)
                {
                    b.txn = None;
                }
            } else if tx.from == Some((p, tid)) {
                tx.from = None;
                cur = tx.from_parent;
            } else {
                break;
            }
        }
        if let Some(t) = send_reply {
            self.send_failed_reply(t, BR_DEAD_REPLY);
        }
        self.release_work(th.todo);
    }

    pub fn release_work(&mut self, works: VecDeque<Work>) {
        for w in works {
            match w {
                Work::Txn(t) => self.discard_txn(t, BR_DEAD_REPLY),
                Work::Death(d) => {
                    if let Some(dt) = self.deaths.get_mut(&d) {
                        dt.queued = false;
                        if dt.kind == DeathKind::Clear {
                            self.deaths.remove(&d);
                        }
                    }
                }
                Work::Node(n) => {
                    if let Some(node) = self.nodes.get_mut(&n) {
                        node.queued = false;
                    }
                }
                _ => {}
            }
        }
    }

    /// The owner of `n` is going away: the node becomes dead and every
    /// process with a death notification on it is told.
    fn node_release(&mut self, n: Id) {
        let Some(node) = self.nodes.get_mut(&n) else { return };
        node.queued = false;
        node.has_async = false;
        let pending = core::mem::take(&mut node.async_todo);
        if node.refs.is_empty() {
            self.nodes.remove(&n);
        } else {
            node.owner = None;
            node.local_strong = 0;
            node.local_weak = 0;
            let holders: Vec<Id> = node.refs.iter().copied().collect();
            for rp in holders {
                let death = self.procs.get(&rp).and_then(|pr| {
                    let d = pr.desc_of.get(&n)?;
                    pr.refs.get(d)?.death
                });
                if let Some(d) = death
                    && let Some(dt) = self.deaths.get_mut(&d)
                {
                    dt.kind = DeathKind::Dead;
                    self.queue_death(d, None);
                }
            }
        }
        for t in pending {
            self.discard_txn(t, BR_DEAD_REPLY);
        }
    }

    /// Last close of a binder file.
    pub fn proc_release(&mut self, p: Id) {
        let Some(pr) = self.procs.get(&p) else { return };
        let ctx = pr.ctx;
        if let Some(&m) = self.mgr.get(&ctx)
            && self.nodes.get(&m).and_then(|n| n.owner) == Some(p)
        {
            self.mgr.remove(&ctx);
        }
        let tids: Vec<u32> = self.proc(p).threads.keys().copied().collect();
        for tid in tids {
            self.thread_release(p, tid);
        }
        let nodes: Vec<Id> = core::mem::take(&mut self.proc(p).nodes).into_values().collect();
        for n in nodes {
            self.node_release(n);
        }
        let descs: Vec<u32> = self.proc(p).refs.keys().copied().collect();
        for d in descs {
            self.cleanup_ref(p, d);
        }
        let todo = core::mem::take(&mut self.proc(p).todo);
        self.release_work(todo);
        let offs: Vec<u64> = self.proc(p).map.as_ref().map(|m| m.used.keys().copied().collect()).unwrap_or_default();
        for off in offs {
            self.release_buffer(p, off, false);
        }
        self.procs.remove(&p);
    }
}

#[test_case]
fn binder_buffer_allocator_best_fit_and_coalescing() {
    let mut m = Mapping::new(Shmem::new(4096), 0x1000, 4096);
    let a = m.alloc(100).unwrap();
    let b = m.alloc(1).unwrap();
    let c = m.alloc(200).unwrap();
    assert_eq!((a, b, c), (0, 104, 112));
    // Free the middle one: a 8-byte hole that a small request reuses.
    m.free_range(b, 8);
    assert_eq!(m.alloc(8), Some(104));
    m.free_range(104, 8);
    m.free_range(a, 104);
    m.free_range(c, 200);
    // Everything merged back into one range.
    assert_eq!(m.free.len(), 1);
    assert_eq!(m.alloc(4096), Some(0));
    assert_eq!(m.alloc(8), None);
}
