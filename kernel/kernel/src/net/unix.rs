//! AF_UNIX sockets: SOCK_STREAM, SOCK_DGRAM and SOCK_SEQPACKET.
//!
//! Every socket has a receive queue of [`Message`]s (data, passed files,
//! sender credentials and address). Senders append to the *peer's* queue
//! and sleep on the peer's wait queue while it is full; receivers sleep on
//! their own. Stream reads coalesce messages (stopping after one that
//! carries files, and between different writers when SO_PASSCRED is on);
//! datagram and seqpacket reads return one message each.
//!
//! Connection: `connect` creates the server-side socket at once (Linux's
//! "embryo"), links it with the client and queues it on the listener;
//! `accept` takes it from there. Addresses are filesystem paths (an
//! S_IFSOCK inode created by bind; found again through a table keyed by
//! the inode), abstract names (leading NUL; a global table) or none.
//!
//! Locks: one socket lock at a time, except a listener's lock around the
//! fresh (still private) embryo's; a socket lock may be held while taking
//! a name table lock, never the other way round. Files leave a queue (and
//! may be closed, releasing other sockets) only after the lock is dropped.
//! Files passed in a message that is never received are closed when the
//! receiving socket is released; reference cycles through in-flight files
//! (a socket sent over itself) are not collected (Linux has a GC for them).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::*;
use crate::errno::*;
use crate::fs::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLERR, POLLHUP, POLLRDHUP, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::fs::vfs::{self, AnonInode, Inode, PathRef, S_IFSOCK};
use crate::sched::{self, Wake, WaitMode};

/// Default SO_SNDBUF / SO_RCVBUF (Linux's net.core.wmem_default).
pub const DEFAULT_BUF: usize = 212_992;
const MIN_SNDBUF: usize = 4608;
const MIN_RCVBUF: usize = 2304;
/// Per-message overhead counted against the buffer (keeps tiny messages
/// from queueing without bound).
const MSG_OVERHEAD: usize = 256;
/// Datagrams queued on a socket from senders it is not connected to
/// (Android's init sets net.unix.max_dgram_qlen to 600).
const MAX_DGRAM_QLEN: usize = 600;
/// listen() backlog limit (net.core.somaxconn).
const SOMAXCONN: usize = 4096;
/// Most files in one SCM_RIGHTS message (Linux SCM_MAX_FD).
pub const SCM_MAX_FD: usize = 253;
pub const SHUT_RCV: u8 = 1;
pub const SHUT_SEND: u8 = 2;
pub const SHUT_MASK: u8 = 3;

/// A socket address, as given to bind/connect.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Addr {
    Unnamed,
    /// Filesystem path (without the terminating NUL).
    Path(Vec<u8>),
    /// Abstract name: sun_path including the leading NUL.
    Abstract(Vec<u8>),
}

/// sizeof(struct sockaddr_un).
pub const SOCKADDR_UN_LEN: usize = 110;

impl Addr {
    /// Parse a user sockaddr_un of the given length (EINVAL if it is not a
    /// valid AF_UNIX address). Length 2 (family only) is `Unnamed`.
    pub fn parse(b: &[u8]) -> KResult<Addr> {
        if b.len() < 2 || b.len() > SOCKADDR_UN_LEN || u16::from_le_bytes([b[0], b[1]]) != AF_UNIX {
            return Err(EINVAL);
        }
        let p = &b[2..];
        Ok(match p.first() {
            None => Addr::Unnamed,
            Some(0) => Addr::Abstract(p.to_vec()),
            Some(_) => Addr::Path(p[..p.iter().position(|&c| c == 0).unwrap_or(p.len())].to_vec()),
        })
    }

    /// The sockaddr_un getsockname/getpeername/recvfrom report.
    pub fn to_sockaddr(&self) -> Vec<u8> {
        let mut v = AF_UNIX.to_le_bytes().to_vec();
        match self {
            Addr::Unnamed => {}
            Addr::Path(p) => {
                v.extend_from_slice(p);
                v.push(0);
            }
            Addr::Abstract(n) => v.extend_from_slice(n),
        }
        v
    }

    /// /proc/net/unix form ("@name" for abstract names).
    fn display(&self) -> String {
        match self {
            Addr::Unnamed => String::new(),
            Addr::Path(p) => String::from_utf8_lossy(p).into_owned(),
            Addr::Abstract(n) => {
                let mut s = String::from("@");
                s.push_str(&String::from_utf8_lossy(&n[1..]).replace('\0', "@"));
                s
            }
        }
    }
}

/// One queued message (a datagram, a seqpacket record or a stream chunk).
pub struct Message {
    pub data: Vec<u8>,
    pub fds: Vec<Arc<OpenFile>>,
    pub cred: Ucred,
    pub from: Option<Addr>,
}

/// What a receive produced.
#[derive(Default)]
pub struct Received {
    /// Bytes copied into the buffer.
    pub n: usize,
    /// Full length of the (first) message; > n when a datagram was cut.
    pub len: usize,
    pub fds: Vec<Arc<OpenFile>>,
    pub cred: Option<Ucred>,
    pub from: Option<Addr>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Unconnected,
    Listening,
    Connected,
}

struct Inner {
    state: State,
    addr: Option<Addr>,
    /// Filesystem binding: the socket inode (keeps the table key alive).
    bound_inode: Option<Arc<dyn Inode>>,
    peer: Option<Arc<UnixSocket>>,
    /// SO_PEERCRED; for a listener its own credentials from listen().
    peer_cred: Option<Ucred>,
    backlog: usize,
    /// Listener: connections not accepted yet.
    pending: VecDeque<Arc<UnixSocket>>,
    queue: VecDeque<Message>,
    /// Stream: bytes of the first message already read.
    head_off: usize,
    /// Unread data bytes (+ overhead per message) in `queue`.
    queued: usize,
    shutdown: u8,
    released: bool,
    /// Pending SO_ERROR.
    err: Option<Errno>,
    passcred: bool,
    sndbuf: usize,
    rcvbuf: usize,
    /// SO_RCVTIMEO / SO_SNDTIMEO in ns (0 = none).
    rcvtimeo: u64,
    sndtimeo: u64,
}

pub struct UnixSocket {
    pub ty: u32,
    pub inode: Arc<dyn Inode>,
    pub wq: Arc<WaitQueue>,
    inner: Mutex<Inner>,
}

/// Every live socket by inode number (/proc/net/unix).
static ALL: Mutex<BTreeMap<u64, Weak<UnixSocket>>> = Mutex::new(BTreeMap::new());
/// Bound abstract names.
static ABSTRACT: Mutex<BTreeMap<Vec<u8>, Weak<UnixSocket>>> = Mutex::new(BTreeMap::new());
/// Bound filesystem sockets by inode address.
static PATHS: Mutex<BTreeMap<usize, Weak<UnixSocket>>> = Mutex::new(BTreeMap::new());

fn inode_key(i: &Arc<dyn Inode>) -> usize {
    Arc::as_ptr(i) as *const () as usize
}

fn deadline(timeo: u64) -> Option<u64> {
    (timeo != 0).then(|| crate::time::monotonic_ns().saturating_add(timeo))
}

/// The error for an interrupted wait: restartable unless a timeout is set
/// (Linux sock_intr_errno).
fn intr(timeo: u64) -> Errno {
    if timeo != 0 { EINTR } else { ERESTARTSYS }
}

fn is(a: &Option<Arc<UnixSocket>>, b: &UnixSocket) -> bool {
    a.as_ref().is_some_and(|a| core::ptr::eq(Arc::as_ptr(a), b))
}

impl UnixSocket {
    pub fn new(ty: u32) -> Arc<UnixSocket> {
        let s = Arc::new(UnixSocket {
            ty,
            inode: AnonInode::new(S_IFSOCK | 0o777, 0),
            wq: WaitQueue::new(),
            inner: Mutex::new(Inner {
                state: State::Unconnected,
                addr: None,
                bound_inode: None,
                peer: None,
                peer_cred: None,
                backlog: 0,
                pending: VecDeque::new(),
                queue: VecDeque::new(),
                head_off: 0,
                queued: 0,
                shutdown: 0,
                released: false,
                err: None,
                passcred: false,
                sndbuf: DEFAULT_BUF,
                rcvbuf: DEFAULT_BUF,
                rcvtimeo: 0,
                sndtimeo: 0,
            }),
        });
        ALL.lock().insert(s.ino(), Arc::downgrade(&s));
        s
    }

    pub fn ino(&self) -> u64 {
        self.inode.meta().ino
    }

    /// socketpair: two connected sockets, each with the caller's credentials.
    pub fn pair(ty: u32, cred: Ucred) -> (Arc<UnixSocket>, Arc<UnixSocket>) {
        let (a, b) = (UnixSocket::new(ty), UnixSocket::new(ty));
        for (x, y) in [(&a, &b), (&b, &a)] {
            let mut s = x.inner.lock();
            s.state = State::Connected;
            s.peer = Some(y.clone());
            s.peer_cred = Some(cred);
        }
        (a, b)
    }

    fn connection_oriented(&self) -> bool {
        self.ty != SOCK_DGRAM
    }

    pub fn queued(&self) -> usize {
        self.inner.lock().queued
    }

    /// The open file for this socket.
    pub fn open_file(self: Arc<Self>, nonblock: bool) -> Arc<OpenFile> {
        let inode = self.inode.clone();
        OpenFile::new(inode, None, Box::new(SocketFile(self)), O_RDWR | if nonblock { O_NONBLOCK } else { 0 })
    }

    // -----------------------------------------------------------------------
    // Names

    /// Bind to `addr` (`Unnamed` = autobind). Paths are created relative to
    /// `base` with permissions `0777 & !umask`.
    pub fn bind(self: &Arc<Self>, addr: Addr, base: &PathRef, umask: u32) -> KResult<()> {
        let mut s = self.inner.lock();
        if s.addr.is_some() {
            return Err(EINVAL);
        }
        match addr {
            Addr::Unnamed => {
                s.addr = Some(self.autobind_name());
            }
            Addr::Abstract(ref n) => {
                let mut t = ABSTRACT.lock();
                if t.get(n).is_some_and(|w| w.strong_count() > 0) {
                    return Err(EADDRINUSE);
                }
                t.insert(n.clone(), Arc::downgrade(self));
                s.addr = Some(addr);
            }
            Addr::Path(ref p) => {
                let (dir, name, trailing) = vfs::lookup_parent(base, p)?;
                if trailing || name == b"." || name == b".." {
                    return Err(EADDRINUSE);
                }
                let node = vfs::create(&dir, &name, S_IFSOCK | (0o777 & !umask), 0).map_err(|e| {
                    if e == EEXIST { EADDRINUSE } else { e }
                })?;
                let inode = node.inode().clone();
                PATHS.lock().insert(inode_key(&inode), Arc::downgrade(self));
                s.bound_inode = Some(inode);
                s.addr = Some(addr);
            }
        }
        Ok(())
    }

    /// Pick a free abstract name "\0xxxxx" and register it.
    fn autobind_name(self: &Arc<Self>) -> Addr {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let mut t = ABSTRACT.lock();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed) & 0xfffff;
            let mut name = alloc::vec![0u8];
            name.extend_from_slice(format!("{n:05x}").as_bytes());
            if t.get(&name).is_none_or(|w| w.strong_count() == 0) {
                t.insert(name.clone(), Arc::downgrade(self));
                return Addr::Abstract(name);
            }
        }
    }

    fn ensure_bound(self: &Arc<Self>) {
        let mut s = self.inner.lock();
        if s.addr.is_none() {
            s.addr = Some(self.autobind_name());
        }
    }

    pub fn local_addr(&self) -> Addr {
        self.inner.lock().addr.clone().unwrap_or(Addr::Unnamed)
    }

    pub fn peer_addr(&self) -> KResult<Addr> {
        let peer = {
            let s = self.inner.lock();
            if self.connection_oriented() && s.state != State::Connected {
                return Err(ENOTCONN);
            }
            s.peer.clone().ok_or(ENOTCONN)?
        };
        Ok(peer.local_addr())
    }

    /// The socket bound to `addr` (`base` for relative paths).
    fn lookup(addr: &Addr, base: &PathRef) -> KResult<Arc<UnixSocket>> {
        let found = match addr {
            Addr::Unnamed => return Err(EINVAL),
            Addr::Abstract(n) => ABSTRACT.lock().get(n).and_then(Weak::upgrade),
            Addr::Path(p) => {
                let node = vfs::lookup(base, p, true)?;
                if node.inode().meta().file_type() != S_IFSOCK {
                    return Err(ECONNREFUSED);
                }
                PATHS.lock().get(&inode_key(node.inode())).and_then(Weak::upgrade)
            }
        };
        let s = found.ok_or(ECONNREFUSED)?;
        if s.inner.lock().released {
            return Err(ECONNREFUSED);
        }
        Ok(s)
    }

    // -----------------------------------------------------------------------
    // Connections

    pub fn listen(&self, backlog: i32, cred: Ucred) -> KResult<()> {
        if !self.connection_oriented() {
            return Err(EOPNOTSUPP);
        }
        {
            let mut s = self.inner.lock();
            if s.addr.is_none() || s.state == State::Connected {
                return Err(EINVAL);
            }
            s.state = State::Listening;
            s.backlog = (backlog as u32 as usize).min(SOMAXCONN);
            s.peer_cred = Some(cred);
        }
        self.wq.wake(0);
        Ok(())
    }

    /// connect: stream/seqpacket sockets link up with a listener; datagram
    /// sockets set (or with `None`, clear) their default destination.
    pub fn connect(self: &Arc<Self>, addr: Option<Addr>, base: &PathRef, cred: Ucred, nonblock: bool) -> KResult<()> {
        if !self.connection_oriented() {
            return self.connect_dgram(addr, base);
        }
        let addr = addr.ok_or(EINVAL)?;
        let (passcred, timeo) = {
            let s = self.inner.lock();
            match s.state {
                State::Connected => return Err(EISCONN),
                State::Listening => return Err(EINVAL),
                State::Unconnected => {}
            }
            (s.passcred, s.sndtimeo)
        };
        if passcred {
            self.ensure_bound();
        }
        let target = UnixSocket::lookup(&addr, base)?;
        if target.ty != self.ty {
            return Err(EPROTOTYPE);
        }
        let embryo = UnixSocket::new(self.ty);
        let mut err = None;
        let mut listener_cred = None;
        let w = sched::wait_until(target.wq.key(), deadline(timeo), WaitMode::Interruptible, || {
            let mut t = target.inner.lock();
            if t.released || t.state != State::Listening || t.shutdown & SHUT_RCV != 0 {
                err = Some(ECONNREFUSED);
                return true;
            }
            if t.pending.len() > t.backlog {
                if nonblock {
                    err = Some(EAGAIN);
                    return true;
                }
                return false;
            }
            {
                let mut e = embryo.inner.lock();
                e.state = State::Connected;
                e.addr = t.addr.clone();
                e.peer = Some(self.clone());
                e.peer_cred = Some(cred);
                e.passcred = t.passcred;
            }
            listener_cred = t.peer_cred;
            t.pending.push_back(embryo.clone());
            true
        });
        if let Some(e) = err {
            embryo.release();
            return Err(e);
        }
        match w {
            Wake::Ready => {}
            Wake::Timeout => {
                embryo.release();
                return Err(EAGAIN);
            }
            Wake::Interrupted => {
                embryo.release();
                return Err(intr(timeo));
            }
        }
        {
            let mut s = self.inner.lock();
            s.state = State::Connected;
            s.peer = Some(embryo);
            s.peer_cred = listener_cred;
        }
        target.wq.wake(READ_EVENTS);
        Ok(())
    }

    fn connect_dgram(self: &Arc<Self>, addr: Option<Addr>, base: &PathRef) -> KResult<()> {
        let peer = match addr {
            None => None,
            Some(a) => {
                let t = UnixSocket::lookup(&a, base)?;
                if t.ty != self.ty {
                    return Err(EPROTOTYPE);
                }
                let tp = t.inner.lock().peer.clone();
                if tp.is_some() && !is(&tp, self) {
                    return Err(EPERM);
                }
                if self.inner.lock().passcred {
                    self.ensure_bound();
                }
                Some(t)
            }
        };
        let old = {
            let mut s = self.inner.lock();
            s.state = if peer.is_some() { State::Connected } else { State::Unconnected };
            core::mem::replace(&mut s.peer, peer)
        };
        drop(old);
        self.wq.wake(0);
        Ok(())
    }

    pub fn accept(&self, nonblock: bool) -> KResult<Arc<UnixSocket>> {
        if !self.connection_oriented() {
            return Err(EOPNOTSUPP);
        }
        let timeo = self.inner.lock().rcvtimeo;
        let mut res = Err(EAGAIN);
        let w = sched::wait_until(self.wq.key(), deadline(timeo), WaitMode::Interruptible, || {
            let mut s = self.inner.lock();
            if s.state != State::Listening {
                res = Err(EINVAL);
                return true;
            }
            if let Some(c) = s.pending.pop_front() {
                res = Ok(c);
                return true;
            }
            nonblock
        });
        match w {
            Wake::Ready => {}
            Wake::Timeout => return Err(EAGAIN),
            Wake::Interrupted => return Err(intr(timeo)),
        }
        // Connectors waiting for backlog room.
        self.wq.wake(0);
        res
    }

    pub fn shutdown(&self, how: u32) -> KResult<()> {
        if how > 2 {
            return Err(EINVAL);
        }
        let mode = ((how + 1) & 3) as u8;
        let peer = {
            let mut s = self.inner.lock();
            s.shutdown |= mode;
            s.peer.clone()
        };
        self.wq.wake(0);
        if let Some(p) = peer
            && self.connection_oriented()
        {
            let mut pm = 0;
            if mode & SHUT_RCV != 0 {
                pm |= SHUT_SEND;
            }
            if mode & SHUT_SEND != 0 {
                pm |= SHUT_RCV;
            }
            p.inner.lock().shutdown |= pm;
            p.wq.wake(if pm & SHUT_RCV != 0 { POLLRDHUP | READ_EVENTS } else { 0 });
        }
        Ok(())
    }

    /// The last file reference is gone (or an embryo is dropped): hang up
    /// on the peer, unregister the name, close files still queued.
    pub fn release(&self) {
        let (peer, queue, pending, addr, bound, unread) = {
            let mut s = self.inner.lock();
            if s.released {
                return;
            }
            s.released = true;
            s.shutdown = SHUT_MASK;
            s.queued = 0;
            let unread = !s.queue.is_empty();
            (
                s.peer.take(),
                core::mem::take(&mut s.queue),
                core::mem::take(&mut s.pending),
                s.addr.clone(),
                s.bound_inode.take(),
                unread,
            )
        };
        if let Some(Addr::Abstract(n)) = &addr {
            let mut t = ABSTRACT.lock();
            if t.get(n).is_some_and(|w| core::ptr::eq(w.as_ptr(), self)) {
                t.remove(n);
            }
        }
        if let Some(i) = &bound {
            PATHS.lock().remove(&inode_key(i));
        }
        if let Some(p) = &peer
            && self.connection_oriented()
        {
            {
                let mut ps = p.inner.lock();
                ps.shutdown = SHUT_MASK;
                if unread || !pending.is_empty() {
                    ps.err = Some(ECONNRESET);
                }
            }
            p.wq.wake(POLLHUP | POLLRDHUP | READ_EVENTS | WRITE_EVENTS);
        }
        self.wq.wake(POLLHUP | POLLRDHUP | READ_EVENTS | WRITE_EVENTS);
        for e in pending {
            e.release();
        }
        drop(queue);
        drop(peer);
    }

    // -----------------------------------------------------------------------
    // Data

    /// Send `data` (with files and credentials) to the peer or, for
    /// datagrams, to `to`. EPIPE is returned without raising SIGPIPE.
    pub fn send(
        self: &Arc<Self>,
        data: &[u8],
        fds: Vec<Arc<OpenFile>>,
        cred: Ucred,
        to: Option<(Addr, PathRef)>,
        nonblock: bool,
    ) -> KResult<usize> {
        let (shut, connected, passcred, sndbuf, timeo) = {
            let s = self.inner.lock();
            (s.shutdown & SHUT_SEND != 0, s.state == State::Connected, s.passcred, s.sndbuf, s.sndtimeo)
        };
        if self.ty == SOCK_DGRAM {
            let target = match &to {
                Some((a, base)) => {
                    let t = UnixSocket::lookup(a, base)?;
                    if t.ty != self.ty {
                        return Err(EPROTOTYPE);
                    }
                    t
                }
                None => self.inner.lock().peer.clone().ok_or(ENOTCONN)?,
            };
            if shut {
                return Err(EPIPE);
            }
            if data.len() + MSG_OVERHEAD > sndbuf {
                return Err(EMSGSIZE);
            }
            if passcred || target.inner.lock().passcred {
                self.ensure_bound();
            }
            let r = self.send_to(&target, data, fds, cred, true, nonblock, sndbuf, timeo);
            if r == Err(ECONNREFUSED) && to.is_none() {
                // The peer is gone: disconnect.
                let old = {
                    let mut s = self.inner.lock();
                    s.state = State::Unconnected;
                    s.peer.take()
                };
                drop(old);
            }
            return r;
        }
        if to.is_some() {
            return Err(if connected { EISCONN } else { EOPNOTSUPP });
        }
        if !connected {
            return Err(ENOTCONN);
        }
        if shut {
            return Err(EPIPE);
        }
        let peer = self.inner.lock().peer.clone().ok_or(ENOTCONN)?;
        if self.ty == SOCK_SEQPACKET {
            if data.len() + MSG_OVERHEAD > sndbuf {
                return Err(EMSGSIZE);
            }
            return self.send_to(&peer, data, fds, cred, true, nonblock, sndbuf, timeo);
        }
        if data.is_empty() {
            return Ok(0);
        }
        self.send_to(&peer, data, fds, cred, false, nonblock, sndbuf, timeo)
    }

    /// Queue `data` on `target`: in one message (`atomic`) or in chunks
    /// as buffer space allows (streams; files ride on the first chunk).
    #[allow(clippy::too_many_arguments)]
    fn send_to(
        self: &Arc<Self>,
        target: &Arc<UnixSocket>,
        data: &[u8],
        fds: Vec<Arc<OpenFile>>,
        cred: Ucred,
        atomic: bool,
        nonblock: bool,
        sndbuf: usize,
        timeo: u64,
    ) -> KResult<usize> {
        let from = self.inner.lock().addr.clone();
        let mut fds = Some(fds);
        let mut sent = 0;
        let dl = deadline(timeo);
        loop {
            let mut err = None;
            let mut pushed = false;
            let w = sched::wait_until(target.wq.key(), dl, WaitMode::Interruptible, || {
                if self.inner.lock().shutdown & SHUT_SEND != 0 && !core::ptr::eq(Arc::as_ptr(target), Arc::as_ptr(self)) {
                    err = Some(EPIPE);
                    return true;
                }
                let mut t = target.inner.lock();
                if t.released {
                    err = Some(if self.ty == SOCK_DGRAM { ECONNREFUSED } else { EPIPE });
                    return true;
                }
                if self.ty == SOCK_DGRAM && t.peer.is_some() && !is(&t.peer, self) {
                    err = Some(EPERM);
                    return true;
                }
                if t.shutdown & SHUT_RCV != 0 {
                    err = Some(EPIPE);
                    return true;
                }
                let want = data.len() - sent;
                let n = if atomic {
                    let full = !t.queue.is_empty()
                        && (t.queued + want + MSG_OVERHEAD > sndbuf
                            || (self.ty == SOCK_DGRAM && !is(&t.peer, self) && t.queue.len() >= MAX_DGRAM_QLEN));
                    (!full).then_some(want)
                } else {
                    Some(sndbuf.saturating_sub(t.queued + MSG_OVERHEAD).min(want)).filter(|&n| n > 0)
                };
                if let Some(n) = n {
                    t.queue.push_back(Message {
                        data: data[sent..sent + n].to_vec(),
                        fds: fds.take().unwrap_or_default(),
                        cred,
                        from: from.clone(),
                    });
                    t.queued += n + MSG_OVERHEAD;
                    sent += n;
                    pushed = true;
                    return true;
                }
                if nonblock {
                    err = Some(EAGAIN);
                    return true;
                }
                false
            });
            if pushed {
                target.wq.wake(READ_EVENTS);
            }
            if let Some(e) = err {
                return if sent > 0 { Ok(sent) } else { Err(e) };
            }
            match w {
                Wake::Ready => {}
                Wake::Timeout => return if sent > 0 { Ok(sent) } else { Err(EAGAIN) },
                Wake::Interrupted => return if sent > 0 { Ok(sent) } else { Err(intr(timeo)) },
            }
            if atomic || sent >= data.len() {
                return Ok(sent);
            }
        }
    }

    /// Receive into `buf`. MSG_PEEK, MSG_WAITALL and MSG_DONTWAIT (folded
    /// into `nonblock`) are handled here.
    pub fn recv(&self, buf: &mut [u8], flags: u32, nonblock: bool) -> KResult<Received> {
        let peek = flags & MSG_PEEK != 0;
        if self.ty != SOCK_STREAM {
            let r = self.recv_once(buf, peek, nonblock, false)?;
            self.wake_writers(peek);
            return Ok(r);
        }
        let mut total = Received::default();
        loop {
            let r = match self.recv_once(&mut buf[total.n..], peek, nonblock, true) {
                Ok(r) => r,
                Err(e) if total.n == 0 => return Err(e),
                Err(_) => break,
            };
            self.wake_writers(peek);
            if total.n == 0 {
                total.from = r.from.clone();
                total.cred = r.cred;
            }
            total.n += r.n;
            total.len = total.n;
            let done = r.n == 0 || !r.fds.is_empty() || r.cred.is_some();
            total.fds = r.fds;
            if done || peek || flags & MSG_WAITALL == 0 || total.n >= buf.len() {
                break;
            }
        }
        Ok(total)
    }

    fn wake_writers(&self, peek: bool) {
        if peek {
            return;
        }
        let peer = self.inner.lock().peer.clone();
        self.wq.wake(WRITE_EVENTS);
        if let Some(p) = peer {
            p.wq.wake(WRITE_EVENTS);
        }
    }

    /// Wait for one message (or, for streams, at least one byte) and take
    /// it; `Ok(n = 0)` at end of file.
    fn recv_once(&self, buf: &mut [u8], peek: bool, nonblock: bool, stream: bool) -> KResult<Received> {
        let timeo = self.inner.lock().rcvtimeo;
        let mut res = Err(EAGAIN);
        let w = sched::wait_until(self.wq.key(), deadline(timeo), WaitMode::Interruptible, || {
            let mut g = self.inner.lock();
            let s = &mut *g;
            if self.connection_oriented() && s.state != State::Connected {
                res = Err(if stream { EINVAL } else { ENOTCONN });
                return true;
            }
            if !s.queue.is_empty() {
                res = Ok(if !stream {
                    take_message(s, buf, peek)
                } else if peek {
                    peek_stream(s, buf)
                } else {
                    take_stream(s, buf)
                });
                return true;
            }
            if let Some(e) = s.err.take() {
                res = Err(e);
                return true;
            }
            if s.shutdown & SHUT_RCV != 0 {
                res = Ok(Received::default());
                return true;
            }
            nonblock
        });
        match w {
            Wake::Ready => res,
            Wake::Timeout => Err(EAGAIN),
            Wake::Interrupted => Err(intr(timeo)),
        }
    }

    // -----------------------------------------------------------------------
    // State queries

    pub fn poll(&self) -> u32 {
        let (mut m, peer, sndbuf) = {
            let s = self.inner.lock();
            if s.state == State::Listening {
                return if s.pending.is_empty() { 0 } else { READ_EVENTS };
            }
            let mut m = 0;
            if s.err.is_some() {
                m |= POLLERR;
            }
            if s.shutdown == SHUT_MASK {
                m |= POLLHUP;
            }
            if s.shutdown & SHUT_RCV != 0 {
                m |= POLLRDHUP | READ_EVENTS;
            }
            if !s.queue.is_empty() {
                m |= READ_EVENTS;
            }
            if self.connection_oriented() && s.state == State::Unconnected {
                m |= POLLHUP;
            }
            (m, s.peer.clone(), s.sndbuf)
        };
        match peer {
            Some(p) if !core::ptr::eq(Arc::as_ptr(&p), self) => {
                if p.queued() + MSG_OVERHEAD < sndbuf {
                    m |= WRITE_EVENTS;
                }
            }
            _ => m |= WRITE_EVENTS,
        }
        m
    }

    /// FIONREAD / SIOCINQ: all unread stream bytes, or the next datagram's
    /// size.
    pub fn inq(&self) -> KResult<usize> {
        let s = self.inner.lock();
        if s.state == State::Listening {
            return Err(EINVAL);
        }
        Ok(if self.ty == SOCK_DGRAM {
            s.queue.front().map_or(0, |m| m.data.len())
        } else {
            s.queue.iter().map(|m| m.data.len()).sum::<usize>() - s.head_off
        })
    }

    /// SIOCOUTQ: bytes sent but not yet read by the peer.
    pub fn outq(&self) -> KResult<usize> {
        let peer = {
            let s = self.inner.lock();
            if s.state == State::Listening {
                return Err(EINVAL);
            }
            s.peer.clone()
        };
        Ok(peer.map_or(0, |p| p.inq().unwrap_or(0)))
    }
}

/// Socket options kept by the socket.
#[derive(Clone, Copy)]
pub enum Opt {
    PassCred(bool),
    SndBuf(usize),
    RcvBuf(usize),
    RcvTimeo(u64),
    SndTimeo(u64),
}

impl UnixSocket {
    pub fn set_opt(&self, o: Opt) {
        let mut s = self.inner.lock();
        match o {
            Opt::PassCred(v) => s.passcred = v,
            Opt::SndBuf(v) => s.sndbuf = v.saturating_mul(2).clamp(MIN_SNDBUF, i32::MAX as usize),
            Opt::RcvBuf(v) => s.rcvbuf = v.saturating_mul(2).clamp(MIN_RCVBUF, i32::MAX as usize),
            Opt::RcvTimeo(v) => s.rcvtimeo = v,
            Opt::SndTimeo(v) => s.sndtimeo = v,
        }
        drop(s);
        self.wq.wake(0);
    }

    pub fn passcred(&self) -> bool {
        self.inner.lock().passcred
    }

    pub fn sndbuf(&self) -> usize {
        self.inner.lock().sndbuf
    }

    pub fn rcvbuf(&self) -> usize {
        self.inner.lock().rcvbuf
    }

    pub fn timeouts(&self) -> (u64, u64) {
        let s = self.inner.lock();
        (s.rcvtimeo, s.sndtimeo)
    }

    pub fn listening(&self) -> bool {
        self.inner.lock().state == State::Listening
    }

    pub fn take_error(&self) -> Option<Errno> {
        self.inner.lock().err.take()
    }

    pub fn peer_cred(&self) -> Ucred {
        let s = self.inner.lock();
        if s.state == State::Listening { None } else { s.peer_cred }.unwrap_or(NO_CRED)
    }
}

/// Take the first message whole (datagram/seqpacket; the rest of a cut
/// message is dropped).
fn take_message(s: &mut Inner, buf: &mut [u8], peek: bool) -> Received {
    let m = s.queue.front().unwrap();
    let n = buf.len().min(m.data.len());
    buf[..n].copy_from_slice(&m.data[..n]);
    let mut r = Received { n, len: m.data.len(), fds: Vec::new(), cred: s.passcred.then_some(m.cred), from: m.from.clone() };
    if peek {
        r.fds = m.fds.clone();
    } else {
        let m = s.queue.pop_front().unwrap();
        s.queued -= m.data.len() + MSG_OVERHEAD;
        r.fds = m.fds;
    }
    r
}

/// Stream read: consume bytes across messages, stopping after one with
/// files and (with SO_PASSCRED) before one from another writer.
fn take_stream(s: &mut Inner, buf: &mut [u8]) -> Received {
    let mut r = Received::default();
    let mut first = true;
    while let Some(m) = s.queue.front_mut() {
        if !first && (r.n >= buf.len() || (s.passcred && r.cred != Some(m.cred))) {
            break;
        }
        if first {
            r.from = m.from.clone();
            r.cred = s.passcred.then_some(m.cred);
            first = false;
        }
        let avail = m.data.len() - s.head_off;
        let k = avail.min(buf.len() - r.n);
        buf[r.n..r.n + k].copy_from_slice(&m.data[s.head_off..s.head_off + k]);
        r.n += k;
        s.queued -= k;
        let has_fds = !m.fds.is_empty();
        if has_fds {
            r.fds = core::mem::take(&mut m.fds);
        }
        if k < avail {
            s.head_off += k;
            break;
        }
        s.head_off = 0;
        s.queued -= MSG_OVERHEAD;
        s.queue.pop_front();
        if has_fds {
            break;
        }
    }
    r.len = r.n;
    r
}

fn peek_stream(s: &Inner, buf: &mut [u8]) -> Received {
    let mut r = Received::default();
    let mut off = s.head_off;
    for (i, m) in s.queue.iter().enumerate() {
        if i > 0 && (r.n >= buf.len() || (s.passcred && r.cred != Some(m.cred))) {
            break;
        }
        if i == 0 {
            r.from = m.from.clone();
            r.cred = s.passcred.then_some(m.cred);
        }
        let k = (m.data.len() - off).min(buf.len() - r.n);
        buf[r.n..r.n + k].copy_from_slice(&m.data[off..off + k]);
        r.n += k;
        off = 0;
        if !m.fds.is_empty() {
            r.fds = m.fds.clone();
            break;
        }
    }
    r.len = r.n;
    r
}

// ---------------------------------------------------------------------------
// The file

/// File operations of a socket; dropping it (the last close) releases the
/// socket.
pub struct SocketFile(pub Arc<UnixSocket>);

const FIONREAD: u32 = 0x541b;
const SIOCOUTQ: u32 = 0x5411;

impl FileOps for SocketFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        Ok(self.0.recv(buf, 0, f.nonblock())?.n)
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let r = self.0.send(data, Vec::new(), Ucred::current(), None, f.nonblock());
        if r == Err(EPIPE) {
            crate::process::signal::send_to_current(crate::process::signal::SIGPIPE);
        }
        r
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let n = match cmd {
            FIONREAD => self.0.inq()?,
            SIOCOUTQ => self.0.outq()?,
            _ => return Err(ENOTTY),
        };
        crate::process::current().mm().copy_out(arg, &(n as i32).to_le_bytes())?;
        Ok(0)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        self.0.poll()
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        ALL.lock().remove(&self.ino());
    }
}

/// The socket behind an open file (ENOTSOCK if it is not one).
pub fn socket_of(f: &OpenFile) -> KResult<Arc<UnixSocket>> {
    f.ops.as_any().downcast_ref::<SocketFile>().map(|s| s.0.clone()).ok_or(ENOTSOCK)
}

/// /proc/net/unix.
pub fn proc_net_unix() -> String {
    let socks: Vec<Arc<UnixSocket>> = ALL.lock().values().filter_map(Weak::upgrade).collect();
    let mut out = String::from("Num       RefCount Protocol Flags    Type St Inode Path\n");
    for (i, k) in socks.iter().enumerate() {
        let (flags, st, addr) = {
            let s = k.inner.lock();
            let st = match s.state {
                State::Connected => 3,
                _ => 1,
            };
            let flags = if s.state == State::Listening { 0x10000 } else { 0 };
            (flags, st, s.addr.as_ref().map(Addr::display))
        };
        out.push_str(&format!(
            "{:016x}: {:08X} {:08X} {:08X} {:04X} {:02X} {:5}",
            i,
            Arc::strong_count(k) - 1,
            0,
            flags,
            k.ty,
            st,
            k.ino()
        ));
        if let Some(a) = addr {
            out.push(' ');
            out.push_str(&a);
        }
        out.push('\n');
    }
    drop(socks);
    out
}

#[test_case]
fn unix_addr_parse() {
    let mut b = AF_UNIX.to_le_bytes().to_vec();
    assert_eq!(Addr::parse(&b), Ok(Addr::Unnamed));
    b.extend_from_slice(b"/tmp/s\0junk");
    assert_eq!(Addr::parse(&b), Ok(Addr::Path(b"/tmp/s".to_vec())));
    assert_eq!(Addr::Path(b"/tmp/s".to_vec()).to_sockaddr(), b"\x01\x00/tmp/s\x00");
    let ab = b"\x01\x00\x00name";
    assert_eq!(Addr::parse(ab), Ok(Addr::Abstract(b"\0name".to_vec())));
    assert_eq!(Addr::parse(ab).unwrap().to_sockaddr(), ab);
    assert_eq!(Addr::parse(b"\x02\x00x"), Err(EINVAL));
    assert_eq!(Addr::parse(&[1]), Err(EINVAL));
}

#[test_case]
fn unix_socket_queues() {
    // Kernel-level checks that need no current process (no blocking
    // waits): queue accounting, stream coalescing and peeking, message
    // boundaries, hang-up on release.
    let cred = Ucred { pid: 7, uid: 0, gid: 0 };
    let push = |t: &Arc<UnixSocket>, d: &[u8]| {
        let mut s = t.inner.lock();
        s.queue.push_back(Message { data: d.to_vec(), fds: Vec::new(), cred, from: None });
        s.queued += d.len() + MSG_OVERHEAD;
    };
    let (a, b) = UnixSocket::pair(SOCK_STREAM, cred);
    push(&b, b"hello");
    push(&b, b"world");
    assert_eq!(b.inq(), Ok(10));
    assert!(b.poll() & READ_EVENTS != 0);
    let mut buf = [0u8; 16];
    let r = peek_stream(&b.inner.lock(), &mut buf[..3]);
    assert_eq!(&buf[..r.n], b"hel");
    let r = take_stream(&mut b.inner.lock(), &mut buf[..7]);
    assert_eq!(&buf[..r.n], b"hellowo");
    assert_eq!(b.inq(), Ok(3));
    let r = take_stream(&mut b.inner.lock(), &mut buf);
    assert_eq!(&buf[..r.n], b"rld");
    assert_eq!(b.queued(), 0);
    assert_eq!(b.peer_cred(), cred);
    a.release();
    assert_eq!(b.poll() & POLLHUP, POLLHUP);
    b.release();
    drop((a, b));

    let (a, b) = UnixSocket::pair(SOCK_SEQPACKET, cred);
    push(&b, b"one");
    push(&b, b"0123456789");
    assert_eq!(b.inq(), Ok(13));
    assert_eq!(take_message(&mut b.inner.lock(), &mut buf, false).n, 3);
    let r = take_message(&mut b.inner.lock(), &mut buf[..4], false);
    assert_eq!((r.n, r.len), (4, 10));
    assert!(b.inner.lock().queue.is_empty());
    assert_eq!(b.queued(), 0);
    b.release();
    a.release();
    assert!(a.poll() & POLLHUP != 0);
}
