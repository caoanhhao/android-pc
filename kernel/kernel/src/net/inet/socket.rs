//! AF_INET / AF_INET6 sockets on top of smoltcp's TCP and UDP sockets.
//!
//! - TCP: `connect` makes one smoltcp socket; `listen` keeps a small pool
//!   of smoltcp sockets listening on the endpoint (smoltcp accepts one
//!   connection per socket), refilled after every poll, and `accept`
//!   takes an established one out of the pool. A closed connection is
//!   left in the stack as an orphan until its FIN handshake ends.
//! - UDP: one smoltcp socket, created when the socket gets a port.
//! - ICMP "ping" sockets (SOCK_DGRAM + IPPROTO_ICMP/ICMPV6): one smoltcp
//!   ICMP socket bound to the echo identifier, which is the socket's port;
//!   echo requests go out with that identifier, replies come back.
//! - Raw sockets exist for their options (netfilter's IPT_SO_* live on
//!   raw sockets) but carry no traffic.
//!
//! A connection reset by the peer (or timed out) reports ECONNRESET once,
//! through recv/send and SO_ERROR, then end of file / EPIPE.
//!
//! AF_INET6 sockets are dual-stack: IPv4 peers appear as v4-mapped
//! addresses. Ports are reserved in [`Tables`] (bind conflicts, ephemeral
//! ports). Lock order: a socket's state, then the stack (`NET`).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{icmp, tcp, udp};
use smoltcp::time::Duration;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
use spin::Mutex;

use super::{AF_INET, AF_INET6, Net, parse_sockaddr, sockaddr_in, with_net, with_net_nopoll};
use crate::errno::*;
use crate::fs::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLERR, POLLHUP, POLLRDHUP, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::fs::vfs::{AnonInode, Inode, S_IFSOCK};
use crate::net::{MSG_DONTWAIT, MSG_PEEK, MSG_WAITALL, SOCK_DGRAM, SOCK_RAW, SOCK_STREAM};
use crate::sched::{self, Wake, WaitMode};

pub const IPPROTO_ICMP: u32 = 1;
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_UDP: u32 = 17;
pub const IPPROTO_ICMPV6: u32 = 58;

const TCP_BUF: usize = 64 * 1024;
const UDP_BUF: usize = 64 * 1024;
const UDP_SLOTS: usize = 64;
/// Sockets kept listening per listening socket (smoltcp takes one
/// connection per socket; `Net::poll` refills the pool every
/// `LISTEN_POOL` received packets, so a burst of SYNs is not refused).
pub const LISTEN_POOL: usize = 8;
const ICMP_BUF: usize = 16 * 1024;
const ICMP_SLOTS: usize = 32;
const ORPHAN_TIMEOUT_NS: u64 = 60_000_000_000;
const EPHEMERAL: core::ops::Range<u16> = 32768..61000;

struct PortUser {
    sock: u64,
    addr: Option<IpAddress>,
    reuse: bool,
    listening: bool,
}

struct Pool {
    ep: IpListenEndpoint,
    handles: Vec<SocketHandle>,
    backlog: usize,
}

/// Socket bookkeeping kept with the stack (under `NET`).
#[derive(Default)]
pub struct Tables {
    ports: BTreeMap<(u32, u16), Vec<PortUser>>,
    next_port: u16,
    pools: BTreeMap<u64, Pool>,
    orphans: Vec<(SocketHandle, u64)>,
}

impl Tables {
    fn conflict(&self, proto: u32, port: u16, addr: Option<IpAddress>, reuse: bool, listening: bool, me: u64) -> bool {
        self.ports.get(&(proto, port)).is_some_and(|v| {
            v.iter().any(|u| {
                u.sock != me
                    && (u.addr.is_none() || addr.is_none() || u.addr == addr)
                    && !(u.reuse && reuse && !(u.listening && listening))
            })
        })
    }

    fn reserve(&mut self, proto: u32, port: u16, addr: Option<IpAddress>, reuse: bool, me: u64) {
        self.ports.entry((proto, port)).or_default().push(PortUser { sock: me, addr, reuse, listening: false });
    }

    fn release(&mut self, me: u64) {
        self.ports.retain(|_, v| {
            v.retain(|u| u.sock != me);
            !v.is_empty()
        });
    }

    fn set_listening(&mut self, me: u64) {
        for v in self.ports.values_mut() {
            for u in v.iter_mut().filter(|u| u.sock == me) {
                u.listening = true;
            }
        }
    }

    fn ephemeral(&mut self, proto: u32, addr: Option<IpAddress>, me: u64) -> KResult<u16> {
        let span = EPHEMERAL.end - EPHEMERAL.start;
        if self.next_port == 0 {
            let mut b = [0u8; 2];
            crate::random::fill(&mut b);
            self.next_port = u16::from_le_bytes(b) % span;
        }
        for _ in 0..span {
            let p = EPHEMERAL.start + self.next_port % span;
            self.next_port = (self.next_port + 1) % span;
            if !self.ports.contains_key(&(proto, p)) {
                self.reserve(proto, p, addr, false, me);
                return Ok(p);
            }
        }
        Err(EADDRINUSE)
    }
}

fn new_tcp() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; TCP_BUF]), tcp::SocketBuffer::new(vec![0u8; TCP_BUF]));
    s.set_nagle_enabled(false);
    s.set_ack_delay(None);
    s
}

fn new_icmp() -> icmp::Socket<'static> {
    icmp::Socket::new(
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; ICMP_SLOTS], vec![0u8; ICMP_BUF]),
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; ICMP_SLOTS], vec![0u8; ICMP_BUF]),
    )
}

fn new_udp() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_SLOTS], vec![0u8; UDP_BUF]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; UDP_SLOTS], vec![0u8; UDP_BUF]),
    )
}

/// Housekeeping after each poll: finished orphans leave the stack,
/// listening pools are topped up. True if anything changed.
pub(super) fn after_poll(n: &mut Net) -> bool {
    let now = crate::time::monotonic_ns();
    let mut changed = false;
    let mut i = 0;
    while i < n.sock.orphans.len() {
        let (h, deadline) = n.sock.orphans[i];
        let s = n.sockets.get_mut::<tcp::Socket>(h);
        // Data for a closed socket: reset (sent by the next poll, then
        // the socket goes), as Linux does.
        if s.recv_queue() > 0 && s.state() != tcp::State::Closed {
            s.abort();
            changed = true;
            i += 1;
            continue;
        }
        if s.state() == tcp::State::Closed || now >= deadline {
            s.abort();
            n.sockets.remove(h);
            n.sock.orphans.swap_remove(i);
        } else {
            i += 1;
        }
    }
    let ids: Vec<u64> = n.sock.pools.keys().copied().collect();
    for id in ids {
        let pool = n.sock.pools.get_mut(&id).unwrap();
        let ep = pool.ep;
        // Sockets reset during the handshake listen again.
        for &h in &pool.handles {
            let s = n.sockets.get_mut::<tcp::Socket>(h);
            if s.state() == tcp::State::Closed {
                let _ = s.listen(ep);
            }
        }
        let listening = pool.handles.iter().filter(|&&h| n.sockets.get::<tcp::Socket>(h).state() == tcp::State::Listen).count();
        let pending = pool.handles.len() - listening;
        if listening < LISTEN_POOL && pending < pool.backlog {
            for _ in listening..LISTEN_POOL {
                let mut s = new_tcp();
                if s.listen(ep).is_ok() {
                    pool.handles.push(n.sockets.add(s));
                }
            }
            changed = true;
        }
    }
    changed
}

static REGISTRY: Mutex<Vec<Weak<InetSocket>>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Wake every socket's waiters (after a poll that changed state). The
/// events are left unknown: callers may hold a socket's state lock.
pub fn wake_all() {
    let socks: Vec<Arc<InetSocket>> = REGISTRY.lock().iter().filter_map(Weak::upgrade).collect();
    for s in socks {
        s.wq.wake(0);
    }
}

enum Tcp {
    Idle,
    Listening(u64),
    Conn(SocketHandle),
}

enum Kind {
    Tcp(Tcp),
    Udp(Option<SocketHandle>),
    Icmp(Option<SocketHandle>),
    Raw,
}

struct State {
    kind: Kind,
    bound: Option<(Option<IpAddress>, u16)>,
    /// UDP default destination.
    remote: Option<IpEndpoint>,
    connecting: bool,
    error: Option<Errno>,
    shut_rd: bool,
    shut_wr: bool,
    /// The connection was reset; reported (ECONNRESET) or not yet.
    reset: Option<bool>,
    reuse: bool,
    rcvtimeo: u64,
    sndtimeo: u64,
    /// Options stored and read back but not acted on.
    opts: BTreeMap<(i32, i32), Vec<u8>>,
}

pub struct InetSocket {
    pub id: u64,
    pub family: u16,
    pub ty: u32,
    pub protocol: u32,
    pub wq: Arc<WaitQueue>,
    inode: Arc<dyn Inode>,
    st: Mutex<State>,
}

pub struct Received {
    pub data: Vec<u8>,
    /// Real datagram length (MSG_TRUNC).
    pub len: usize,
    pub from: Option<Vec<u8>>,
}

/// The port namespace of a socket (ICMP echo identifiers are ports too).
fn proto_of(ty: u32, protocol: u32) -> u32 {
    match ty {
        SOCK_STREAM => IPPROTO_TCP,
        SOCK_DGRAM if protocol == IPPROTO_ICMP || protocol == IPPROTO_ICMPV6 => IPPROTO_ICMP,
        _ => IPPROTO_UDP,
    }
}

impl InetSocket {
    pub fn new(family: u16, ty: u32, protocol: u32) -> KResult<Arc<InetSocket>> {
        let (kind, protocol) = match ty {
            SOCK_STREAM if protocol == 0 || protocol == IPPROTO_TCP => (Kind::Tcp(Tcp::Idle), IPPROTO_TCP),
            SOCK_DGRAM if protocol == 0 || protocol == IPPROTO_UDP => (Kind::Udp(None), IPPROTO_UDP),
            SOCK_DGRAM if protocol == IPPROTO_ICMP && family == AF_INET || protocol == IPPROTO_ICMPV6 && family == AF_INET6 => {
                (Kind::Icmp(None), protocol)
            }
            SOCK_DGRAM if protocol == IPPROTO_ICMP || protocol == IPPROTO_ICMPV6 => return Err(EPROTONOSUPPORT),
            SOCK_RAW if protocol < 256 => (Kind::Raw, protocol),
            SOCK_STREAM | SOCK_DGRAM | SOCK_RAW => return Err(EPROTONOSUPPORT),
            _ => return Err(ESOCKTNOSUPPORT),
        };
        let s = Arc::new(InetSocket {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            family,
            ty,
            protocol,
            wq: WaitQueue::new(),
            inode: AnonInode::new(S_IFSOCK | 0o777, 0),
            st: Mutex::new(State {
                kind,
                bound: None,
                remote: None,
                connecting: false,
                error: None,
                shut_rd: false,
                shut_wr: false,
                reset: None,
                reuse: false,
                rcvtimeo: 0,
                sndtimeo: 0,
                opts: BTreeMap::new(),
            }),
        });
        let mut r = REGISTRY.lock();
        r.retain(|w| w.strong_count() > 0);
        r.push(Arc::downgrade(&s));
        Ok(s)
    }

    pub fn open_file(self: Arc<Self>, nonblock: bool) -> Arc<OpenFile> {
        let inode = self.inode.clone();
        OpenFile::new(inode, None, Box::new(InetFile(self)), O_RDWR | if nonblock { O_NONBLOCK } else { 0 })
    }

    pub fn ino(&self) -> u64 {
        self.inode.meta().ino
    }

    fn is_tcp(&self) -> bool {
        self.protocol == IPPROTO_TCP && self.ty == SOCK_STREAM
    }

    fn sockaddr(&self, ep: (IpAddress, u16)) -> Vec<u8> {
        sockaddr_in(ep, self.family)
    }

    fn unspecified(&self) -> IpAddress {
        if self.family == AF_INET6 { IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED) } else { IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED) }
    }

    /// A user sockaddr for this socket's family.
    fn parse(&self, b: &[u8]) -> KResult<(IpAddress, u16)> {
        let (fam, addr, port) = parse_sockaddr(b)?;
        if fam != self.family {
            return Err(if self.family == AF_INET { EAFNOSUPPORT } else { EINVAL });
        }
        Ok((addr, port))
    }

    /// Block until `ready` produces a result.
    fn wait<T>(&self, nonblock: bool, timeo: u64, mut ready: impl FnMut() -> Option<KResult<T>>) -> KResult<T> {
        let mut res = None;
        let deadline = (timeo != 0).then(|| crate::time::monotonic_ns().saturating_add(timeo));
        let w = sched::wait_until(self.wq.key(), deadline, WaitMode::Interruptible, || {
            match ready() {
                Some(r) => {
                    res = Some(r);
                    true
                }
                None => nonblock,
            }
        });
        match (w, res) {
            (_, Some(r)) => r,
            (Wake::Ready, None) | (Wake::Timeout, None) => Err(EAGAIN),
            (Wake::Interrupted, None) => Err(if timeo != 0 { EINTR } else { ERESTARTSYS }),
        }
    }

    pub fn bind(&self, b: &[u8]) -> KResult<()> {
        let (addr, port) = self.parse(b)?;
        let mut st = self.st.lock();
        if st.bound.is_some() {
            return Err(EINVAL);
        }
        if matches!(st.kind, Kind::Raw) {
            st.bound = Some((None, 0));
            return Ok(());
        }
        let addr = (!addr.is_unspecified()).then_some(addr);
        let proto = proto_of(self.ty, self.protocol);
        let reuse = st.reuse;
        let port = with_net_nopoll(|n| {
            if let Some(a) = addr
                && !n.dev.is_local(a)
            {
                return Err(EADDRNOTAVAIL);
            }
            if port == 0 {
                return n.sock.ephemeral(proto, addr, self.id);
            }
            if n.sock.conflict(proto, port, addr, reuse, false, self.id) {
                return Err(EADDRINUSE);
            }
            n.sock.reserve(proto, port, addr, reuse, self.id);
            Ok(port)
        })?;
        st.bound = Some((addr, port));
        match st.kind {
            Kind::Udp(None) => {
                with_net_nopoll(|n| self.ensure_udp(&mut st, n))?;
            }
            Kind::Icmp(None) => {
                with_net_nopoll(|n| self.ensure_icmp(&mut st, n))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Give an unbound socket an ephemeral port.
    fn autobind(&self, st: &mut State, n: &mut Net) -> KResult<(Option<IpAddress>, u16)> {
        if let Some(b) = st.bound {
            return Ok(b);
        }
        let port = n.sock.ephemeral(proto_of(self.ty, self.protocol), None, self.id)?;
        st.bound = Some((None, port));
        Ok((None, port))
    }

    pub fn listen(&self, backlog: i32) -> KResult<()> {
        let mut st = self.st.lock();
        match st.kind {
            Kind::Tcp(Tcp::Idle) => {}
            Kind::Tcp(Tcp::Listening(id)) => {
                let b = backlog.clamp(1, 4096) as usize;
                with_net_nopoll(|n| {
                    if let Some(p) = n.sock.pools.get_mut(&id) {
                        p.backlog = b;
                    }
                });
                return Ok(());
            }
            Kind::Tcp(Tcp::Conn(_)) => return Err(EINVAL),
            _ => return Err(EOPNOTSUPP),
        }
        let id = self.id;
        let st = &mut *st;
        with_net(|n| {
            let (addr, port) = self.autobind(st, n)?;
            if n.sock.conflict(IPPROTO_TCP, port, addr, false, true, id) {
                return Err(EADDRINUSE);
            }
            n.sock.set_listening(id);
            let ep = IpListenEndpoint { addr, port };
            n.sock.pools.insert(id, Pool { ep, handles: Vec::new(), backlog: backlog.clamp(1, 4096) as usize });
            st.kind = Kind::Tcp(Tcp::Listening(id));
            Ok(())
        })
    }

    pub fn accept(&self, nonblock: bool) -> KResult<(Arc<InetSocket>, Vec<u8>)> {
        let (id, timeo) = {
            let st = self.st.lock();
            match st.kind {
                Kind::Tcp(Tcp::Listening(id)) => (id, st.rcvtimeo),
                _ => return Err(EINVAL),
            }
        };
        let h = self.wait(nonblock, timeo, || {
            with_net_nopoll(|n| {
                let Some(pool) = n.sock.pools.get_mut(&id) else {
                    return Some(Err(EINVAL));
                };
                let pos = pool.handles.iter().position(|&h| {
                    let s = n.sockets.get::<tcp::Socket>(h);
                    !matches!(s.state(), tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed)
                })?;
                Some(Ok(pool.handles.remove(pos)))
            })
        })?;
        let c = InetSocket::new(self.family, SOCK_STREAM, IPPROTO_TCP)?;
        let peer = with_net(|n| {
            let s = n.sockets.get::<tcp::Socket>(h);
            let (l, r) = (s.local_endpoint(), s.remote_endpoint());
            if let Some(l) = l {
                n.sock.reserve(IPPROTO_TCP, l.port, Some(l.addr), true, c.id);
            }
            r
        });
        {
            let mut cs = c.st.lock();
            cs.kind = Kind::Tcp(Tcp::Conn(h));
        }
        let peer = peer.map_or_else(|| self.sockaddr((self.unspecified(), 0)), |r| self.sockaddr((r.addr, r.port)));
        Ok((c, peer))
    }

    pub fn connect(&self, b: &[u8], nonblock: bool) -> KResult<()> {
        // AF_UNSPEC dissolves a datagram association.
        if b.len() >= 2 && u16::from_le_bytes([b[0], b[1]]) == crate::net::AF_UNSPEC && !self.is_tcp() {
            self.st.lock().remote = None;
            return Ok(());
        }
        let (mut addr, port) = self.parse(b)?;
        if addr.is_unspecified() {
            addr = match addr {
                IpAddress::Ipv4(_) => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::LOCALHOST),
                IpAddress::Ipv6(_) => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::LOCALHOST),
            };
        }
        let remote = IpEndpoint::new(addr, port);
        let mut g = self.st.lock();
        let st = &mut *g;
        match st.kind {
            Kind::Udp(_) => {
                with_net_nopoll(|n| self.ensure_udp(st, n))?;
                st.remote = Some(remote);
                return Ok(());
            }
            Kind::Icmp(_) => {
                with_net_nopoll(|n| self.ensure_icmp(st, n))?;
                st.remote = Some(remote);
                return Ok(());
            }
            Kind::Raw => {
                st.remote = Some(remote);
                return Ok(());
            }
            Kind::Tcp(Tcp::Idle) => {}
            Kind::Tcp(Tcp::Listening(_)) => return Err(EINVAL),
            Kind::Tcp(Tcp::Conn(_)) => return Err(if st.connecting { EALREADY } else { EISCONN }),
        }
        if port == 0 {
            return Err(ECONNREFUSED);
        }
        let h = with_net(|n| {
            let (laddr, lport) = self.autobind(st, n)?;
            let laddr = laddr.or_else(|| n.source_for(remote.addr));
            let mut s = new_tcp();
            s.connect(n.iface.context(), remote, IpListenEndpoint { addr: laddr, port: lport }).map_err(|_| ENETUNREACH)?;
            Ok(n.sockets.add(s))
        })?;
        st.kind = Kind::Tcp(Tcp::Conn(h));
        st.connecting = true;
        let timeo = st.sndtimeo;
        drop(g);
        // The SYN goes out (and on loopback the handshake completes).
        with_net(|_| ());
        if nonblock {
            return match self.connect_result(h) {
                Some(r) => r,
                None => Err(EINPROGRESS),
            };
        }
        self.wait(false, timeo, || self.connect_result(h))
    }

    /// The outcome of a pending connect, once known.
    fn connect_result(&self, h: SocketHandle) -> Option<KResult<()>> {
        let state = with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).state());
        let mut st = self.st.lock();
        match state {
            tcp::State::SynSent | tcp::State::SynReceived => None,
            tcp::State::Closed if st.connecting => {
                st.connecting = false;
                st.error = None;
                Some(Err(ECONNREFUSED))
            }
            _ => {
                st.connecting = false;
                Some(Ok(()))
            }
        }
    }

    fn ensure_icmp(&self, st: &mut State, n: &mut Net) -> KResult<SocketHandle> {
        if let Kind::Icmp(Some(h)) = st.kind {
            return Ok(h);
        }
        let (_, ident) = self.autobind(st, n)?;
        let mut s = new_icmp();
        s.bind(icmp::Endpoint::Ident(ident)).map_err(|_| EINVAL)?;
        let h = n.sockets.add(s);
        st.kind = Kind::Icmp(Some(h));
        Ok(h)
    }

    fn ensure_udp(&self, st: &mut State, n: &mut Net) -> KResult<SocketHandle> {
        if let Kind::Udp(Some(h)) = st.kind {
            return Ok(h);
        }
        let (addr, port) = self.autobind(st, n)?;
        let mut s = new_udp();
        s.bind(IpListenEndpoint { addr, port }).map_err(|_| EINVAL)?;
        let h = n.sockets.add(s);
        st.kind = Kind::Udp(Some(h));
        Ok(h)
    }

    pub fn send(&self, data: &[u8], flags: u32, to: Option<&[u8]>, nonblock: bool) -> KResult<usize> {
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let mut st = self.st.lock();
        if st.shut_wr {
            return Err(EPIPE);
        }
        match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => {
                let timeo = st.sndtimeo;
                drop(st);
                self.tcp_send(h, data, nonblock, timeo)
            }
            Kind::Tcp(_) => Err(if to.is_some() { EOPNOTSUPP } else { ENOTCONN }),
            Kind::Icmp(_) => {
                let dest = match to {
                    Some(b) => self.parse(b)?.0,
                    None => st.remote.ok_or(EDESTADDRREQ)?.addr,
                };
                let request = if self.family == AF_INET6 { 128 } else { 8 };
                if data.len() < 8 || data[0] != request || data[1] != 0 {
                    return Err(EINVAL);
                }
                if data.len() > ICMP_BUF {
                    return Err(EMSGSIZE);
                }
                let h = with_net_nopoll(|n| self.ensure_icmp(&mut st, n))?;
                let ident = st.bound.map_or(0, |b| b.1);
                let timeo = st.sndtimeo;
                drop(st);
                let mut pkt = data.to_vec();
                pkt[4..6].copy_from_slice(&ident.to_be_bytes());
                self.wait(nonblock, timeo, || {
                    with_net(|n| match n.sockets.get_mut::<icmp::Socket>(h).send_slice(&pkt, dest) {
                        Ok(()) => Some(Ok(data.len())),
                        Err(icmp::SendError::BufferFull) => None,
                        Err(icmp::SendError::Unaddressable) => Some(Err(ENETUNREACH)),
                    })
                })
            }
            Kind::Raw => {
                if to.is_none() && st.remote.is_none() {
                    return Err(EDESTADDRREQ);
                }
                Ok(data.len())
            }
            Kind::Udp(_) => {
                let dest = match to {
                    Some(b) => {
                        let (a, p) = self.parse(b)?;
                        IpEndpoint::new(a, p)
                    }
                    None => st.remote.ok_or(EDESTADDRREQ)?,
                };
                if dest.port == 0 {
                    return Err(EINVAL);
                }
                let dest = if dest.addr.is_unspecified() {
                    IpEndpoint::new(
                        match dest.addr {
                            IpAddress::Ipv4(_) => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::LOCALHOST),
                            IpAddress::Ipv6(_) => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::LOCALHOST),
                        },
                        dest.port,
                    )
                } else {
                    dest
                };
                if data.len() > UDP_BUF {
                    return Err(EMSGSIZE);
                }
                let h = with_net_nopoll(|n| self.ensure_udp(&mut st, n))?;
                let local = st.bound.and_then(|b| b.0);
                let timeo = st.sndtimeo;
                drop(st);
                self.wait(nonblock, timeo, || {
                    with_net(|n| {
                        let src = local.or_else(|| n.source_for(dest.addr));
                        let s = n.sockets.get_mut::<udp::Socket>(h);
                        let mut meta = udp::UdpMetadata::from(dest);
                        meta.local_address = src;
                        match s.send_slice(data, meta) {
                            Ok(()) => Some(Ok(data.len())),
                            Err(udp::SendError::BufferFull) => None,
                            Err(udp::SendError::Unaddressable) => Some(Err(ENETUNREACH)),
                        }
                    })
                })
            }
        }
    }

    /// Whether a connection is gone without our close (reset or timed
    /// out); the first caller to see it gets ECONNRESET.
    fn take_reset(&self, state: tcp::State) -> Option<Errno> {
        let mut st = self.st.lock();
        if state != tcp::State::Closed || st.shut_wr || st.connecting {
            return None;
        }
        match st.reset {
            Some(true) => None,
            _ => {
                st.reset = Some(true);
                st.error = None;
                Some(ECONNRESET)
            }
        }
    }

    fn tcp_send(&self, h: SocketHandle, data: &[u8], nonblock: bool, timeo: u64) -> KResult<usize> {
        let mut done = 0;
        loop {
            let r = self.wait(nonblock, timeo, || {
                with_net(|n| {
                    let s = n.sockets.get_mut::<tcp::Socket>(h);
                    match s.state() {
                        tcp::State::SynSent | tcp::State::SynReceived => return None,
                        st if !s.may_send() => return Some(Err(self.take_reset(st).unwrap_or(EPIPE))),
                        _ => {}
                    }
                    match s.send_slice(&data[done..]) {
                        Ok(0) => None,
                        Ok(k) => Some(Ok(k)),
                        Err(_) => Some(Err(EPIPE)),
                    }
                })
            });
            match r {
                Ok(k) => {
                    done += k;
                    if done == data.len() || nonblock {
                        return Ok(done);
                    }
                }
                Err(_) if done > 0 => return Ok(done),
                Err(e) => return Err(e),
            }
        }
    }

    pub fn recv(&self, len: usize, flags: u32, nonblock: bool) -> KResult<Received> {
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let peek = flags & MSG_PEEK != 0;
        let st = self.st.lock();
        let timeo = st.rcvtimeo;
        let shut_rd = st.shut_rd;
        match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => {
                drop(st);
                let mut out = Vec::new();
                loop {
                    let want = len - out.len();
                    let r = self.wait(nonblock || !out.is_empty() && flags & MSG_WAITALL == 0, timeo, || {
                        with_net(|n| {
                            let s = n.sockets.get_mut::<tcp::Socket>(h);
                            if matches!(s.state(), tcp::State::SynSent | tcp::State::SynReceived) {
                                return None;
                            }
                            let mut buf = vec![0u8; want.min(s.recv_queue().max(1))];
                            let r = if peek { s.peek_slice(&mut buf) } else { s.recv_slice(&mut buf) };
                            match r {
                                Ok(0) if s.may_recv() && !shut_rd => None,
                                Ok(k) => {
                                    buf.truncate(k);
                                    Some(Ok(buf))
                                }
                                Err(tcp::RecvError::InvalidState) => match self.take_reset(s.state()) {
                                    Some(e) => Some(Err(e)),
                                    None => Some(Ok(Vec::new())),
                                },
                                Err(tcp::RecvError::Finished) => Some(Ok(Vec::new())),
                            }
                        })
                    });
                    match r {
                        Ok(b) => {
                            let eof = b.is_empty();
                            out.extend_from_slice(&b);
                            if eof || out.len() >= len || peek || flags & MSG_WAITALL == 0 {
                                break;
                            }
                        }
                        Err(_) if !out.is_empty() => break,
                        Err(e) => return Err(e),
                    }
                }
                let n = out.len();
                Ok(Received { data: out, len: n, from: None })
            }
            Kind::Tcp(_) => Err(ENOTCONN),
            Kind::Raw | Kind::Icmp(None) => {
                drop(st);
                self.wait(nonblock, timeo, || None)
            }
            Kind::Icmp(Some(h)) => {
                drop(st);
                let fam = self.family;
                let request = if fam == AF_INET6 { 128 } else { 8 };
                self.wait(nonblock, timeo, || {
                    with_net(|n| {
                        let s = n.sockets.get_mut::<icmp::Socket>(h);
                        loop {
                            let (data, from) = {
                                let (d, a) = s.recv().ok()?;
                                (d.to_vec(), a)
                            };
                            // Our own requests (to a local address) are
                            // not for us; only replies are.
                            if data.first() == Some(&request) {
                                continue;
                            }
                            let full = data.len();
                            let mut data = data;
                            data.truncate(len);
                            return Some(Ok(Received { data, len: full, from: Some(sockaddr_in((from, 0), fam)) }));
                        }
                    })
                })
            }
            Kind::Udp(h) => {
                let remote = st.remote;
                drop(st);
                let Some(h) = h else {
                    return self.wait(nonblock, timeo, || None);
                };
                let fam = self.family;
                self.wait(nonblock, timeo, || {
                    with_net(|n| {
                        let s = n.sockets.get_mut::<udp::Socket>(h);
                        loop {
                            let (data, meta) = if peek {
                                let (d, m) = s.peek().ok()?;
                                (d.to_vec(), *m)
                            } else {
                                let (d, m) = s.recv().ok()?;
                                (d.to_vec(), m)
                            };
                            if let Some(r) = remote
                                && r != meta.endpoint
                            {
                                if peek {
                                    let _ = s.recv();
                                }
                                continue;
                            }
                            let full = data.len();
                            let mut data = data;
                            data.truncate(len);
                            let from = sockaddr_in((meta.endpoint.addr, meta.endpoint.port), fam);
                            return Some(Ok(Received { data, len: full, from: Some(from) }));
                        }
                    })
                })
            }
        }
    }

    pub fn shutdown(&self, how: u32) -> KResult<()> {
        if how > 2 {
            return Err(EINVAL);
        }
        let mut st = self.st.lock();
        let h = match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => Some(h),
            Kind::Tcp(Tcp::Listening(_)) => None,
            Kind::Tcp(Tcp::Idle) => return Err(ENOTCONN),
            _ if st.remote.is_none() => return Err(ENOTCONN),
            _ => None,
        };
        if how != 1 {
            st.shut_rd = true;
        }
        if how != 0 {
            st.shut_wr = true;
            if let Some(h) = h {
                with_net(|n| n.sockets.get_mut::<tcp::Socket>(h).close());
            }
        }
        drop(st);
        self.wq.wake(self.poll());
        Ok(())
    }

    pub fn local_name(&self) -> Vec<u8> {
        let st = self.st.lock();
        let ep = match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).local_endpoint()).map(|e| (e.addr, e.port)),
            _ => None,
        };
        let ep = ep.unwrap_or_else(|| match st.bound {
            Some((a, p)) => (a.unwrap_or(self.unspecified()), p),
            None => (self.unspecified(), 0),
        });
        self.sockaddr(ep)
    }

    pub fn peer_name(&self) -> KResult<Vec<u8>> {
        let st = self.st.lock();
        let ep = match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).remote_endpoint()),
            _ => st.remote,
        };
        if st.connecting {
            return Err(ENOTCONN);
        }
        let ep = ep.ok_or(ENOTCONN)?;
        Ok(self.sockaddr((ep.addr, ep.port)))
    }

    pub fn poll(&self) -> u32 {
        let mut st = self.st.lock();
        let mut m = match st.kind {
            Kind::Raw => WRITE_EVENTS,
            Kind::Udp(None) | Kind::Icmp(None) => WRITE_EVENTS,
            Kind::Icmp(Some(h)) => with_net_nopoll(|n| {
                let s = n.sockets.get::<icmp::Socket>(h);
                let mut m = 0;
                if s.can_recv() {
                    m |= READ_EVENTS;
                }
                if s.can_send() {
                    m |= WRITE_EVENTS;
                }
                m
            }),
            Kind::Udp(Some(h)) => with_net_nopoll(|n| {
                let s = n.sockets.get::<udp::Socket>(h);
                let mut m = 0;
                if s.can_recv() {
                    m |= READ_EVENTS;
                }
                if s.can_send() {
                    m |= WRITE_EVENTS;
                }
                m
            }),
            Kind::Tcp(Tcp::Idle) => WRITE_EVENTS | POLLHUP,
            Kind::Tcp(Tcp::Listening(id)) => with_net_nopoll(|n| {
                let ready = n.sock.pools.get(&id).is_some_and(|p| {
                    p.handles.iter().any(|&h| {
                        !matches!(
                            n.sockets.get::<tcp::Socket>(h).state(),
                            tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed
                        )
                    })
                });
                if ready { READ_EVENTS } else { 0 }
            }),
            Kind::Tcp(Tcp::Conn(h)) => {
                let (state, can_recv, may_recv, can_send, may_send) = with_net_nopoll(|n| {
                    let s = n.sockets.get::<tcp::Socket>(h);
                    (s.state(), s.can_recv(), s.may_recv(), s.can_send(), s.may_send())
                });
                match state {
                    tcp::State::SynSent | tcp::State::SynReceived => 0,
                    tcp::State::Closed if st.connecting => {
                        st.connecting = false;
                        st.error = Some(ECONNREFUSED);
                        WRITE_EVENTS | POLLERR | POLLHUP
                    }
                    _ => {
                        st.connecting = false;
                        let mut m = 0;
                        if can_recv || !may_recv {
                            m |= READ_EVENTS;
                        }
                        // The peer's FIN is in (even with data left to read).
                        let fin = matches!(
                            state,
                            tcp::State::CloseWait | tcp::State::LastAck | tcp::State::Closing | tcp::State::TimeWait | tcp::State::Closed
                        );
                        if !may_recv || fin {
                            m |= POLLRDHUP;
                        }
                        if can_send && may_send {
                            m |= WRITE_EVENTS;
                        }
                        if matches!(state, tcp::State::Closed | tcp::State::TimeWait) {
                            m |= POLLHUP | READ_EVENTS;
                        }
                        if state == tcp::State::Closed && !st.shut_wr && st.reset.is_none() {
                            // Reset: SO_ERROR says so until recv/send does.
                            st.reset = Some(false);
                            st.error = Some(ECONNRESET);
                        }
                        if st.reset == Some(false) {
                            m |= POLLERR;
                        }
                        m
                    }
                }
            }
        };
        if st.shut_rd {
            m |= READ_EVENTS | POLLRDHUP;
        }
        if st.error.is_some() {
            m |= POLLERR;
        }
        m
    }

    pub fn inq(&self) -> usize {
        let st = self.st.lock();
        match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).recv_queue()),
            Kind::Udp(Some(h)) => with_net_nopoll(|n| n.sockets.get_mut::<udp::Socket>(h).peek().map_or(0, |(d, _)| d.len())),
            _ => 0,
        }
    }

    pub fn outq(&self) -> usize {
        let st = self.st.lock();
        match st.kind {
            Kind::Tcp(Tcp::Conn(h)) => with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).send_queue()),
            Kind::Udp(Some(h)) => with_net_nopoll(|n| n.sockets.get::<udp::Socket>(h).send_queue()),
            _ => 0,
        }
    }

    pub fn listening(&self) -> bool {
        matches!(self.st.lock().kind, Kind::Tcp(Tcp::Listening(_)))
    }

    pub fn take_error(&self) -> Option<Errno> {
        let mut st = self.st.lock();
        if st.reset == Some(false) {
            st.reset = Some(true);
        }
        st.error.take()
    }

    pub fn set_reuse(&self, on: bool) {
        self.st.lock().reuse = on;
    }

    pub fn set_timeouts(&self, rcv: Option<u64>, snd: Option<u64>) {
        let mut st = self.st.lock();
        if let Some(r) = rcv {
            st.rcvtimeo = r;
        }
        if let Some(s) = snd {
            st.sndtimeo = s;
        }
    }

    pub fn timeouts(&self) -> (u64, u64) {
        let st = self.st.lock();
        (st.rcvtimeo, st.sndtimeo)
    }

    /// TCP_NODELAY / SO_KEEPALIVE on a connection.
    pub fn tcp_tune(&self, nodelay: Option<bool>, keepalive: Option<bool>) {
        let st = self.st.lock();
        if let Kind::Tcp(Tcp::Conn(h)) = st.kind {
            with_net_nopoll(|n| {
                let s = n.sockets.get_mut::<tcp::Socket>(h);
                if let Some(d) = nodelay {
                    s.set_nagle_enabled(!d);
                }
                if let Some(k) = keepalive {
                    s.set_keep_alive(k.then(|| Duration::from_secs(75)));
                }
            });
        }
    }

    /// The TCP state number Linux uses (TCP_INFO, /proc/net/tcp).
    pub fn tcp_state(&self) -> u8 {
        let st = self.st.lock();
        match st.kind {
            Kind::Tcp(Tcp::Listening(_)) => 10,
            Kind::Tcp(Tcp::Conn(h)) => match with_net_nopoll(|n| n.sockets.get::<tcp::Socket>(h).state()) {
                tcp::State::Established => 1,
                tcp::State::SynSent => 2,
                tcp::State::SynReceived => 3,
                tcp::State::FinWait1 => 4,
                tcp::State::FinWait2 => 5,
                tcp::State::TimeWait => 6,
                tcp::State::Closed => 7,
                tcp::State::CloseWait => 8,
                tcp::State::LastAck => 9,
                tcp::State::Listen => 10,
                tcp::State::Closing => 11,
            },
            _ => 7,
        }
    }

    pub fn store_opt(&self, level: i32, name: i32, val: &[u8]) {
        self.st.lock().opts.insert((level, name), val.to_vec());
    }

    pub fn stored_opt(&self, level: i32, name: i32) -> Option<Vec<u8>> {
        self.st.lock().opts.get(&(level, name)).cloned()
    }

    /// The last close: connections finish in the background, everything
    /// else leaves the stack.
    fn release(&self) {
        let (kind, linger0) = {
            let mut st = self.st.lock();
            // SO_LINGER {on, 0 s}: close resets the connection.
            let linger0 = st.opts.get(&(crate::net::SOL_SOCKET, 13)).is_some_and(|v| v.len() >= 8 && v[..4] != [0; 4] && v[4..8] == [0; 4]);
            (core::mem::replace(&mut st.kind, Kind::Raw), linger0)
        };
        with_net(|n| {
            n.sock.release(self.id);
            match kind {
                Kind::Tcp(Tcp::Conn(h)) => {
                    let s = n.sockets.get_mut::<tcp::Socket>(h);
                    // As on Linux, unread data (or SO_LINGER 0) makes
                    // close send a reset instead of a FIN.
                    if linger0 || s.recv_queue() > 0 {
                        s.abort();
                        n.sock.orphans.push((h, 0));
                        return;
                    }
                    s.close();
                    n.sock.orphans.push((h, crate::time::monotonic_ns() + ORPHAN_TIMEOUT_NS));
                }
                Kind::Tcp(Tcp::Listening(id)) => {
                    if let Some(p) = n.sock.pools.remove(&id) {
                        for h in p.handles {
                            n.sockets.get_mut::<tcp::Socket>(h).abort();
                            n.sock.orphans.push((h, 0));
                        }
                    }
                }
                Kind::Udp(Some(h)) | Kind::Icmp(Some(h)) => {
                    n.sockets.remove(h);
                }
                _ => {}
            }
        });
    }
}

pub struct InetFile(pub Arc<InetSocket>);

const FIONREAD: u32 = 0x541b;
const SIOCOUTQ: u32 = 0x5411;
const SIOCATMARK: u32 = 0x8905;

impl FileOps for InetFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let r = self.0.recv(buf.len(), 0, f.nonblock())?;
        buf[..r.data.len()].copy_from_slice(&r.data);
        Ok(r.data.len())
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let r = self.0.send(data, 0, None, f.nonblock());
        if r == Err(EPIPE) {
            crate::process::signal::send_to_current(crate::process::signal::SIGPIPE);
        }
        r
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let v = match cmd {
            FIONREAD => self.0.inq(),
            SIOCOUTQ => self.0.outq(),
            SIOCATMARK => 0,
            _ => return crate::net::ifreq::ioctl(cmd, arg),
        };
        crate::process::current().mm().copy_out(arg, &(v as i32).to_le_bytes())?;
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

impl Drop for InetFile {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub fn inet_of(f: &OpenFile) -> Option<Arc<InetSocket>> {
    f.ops.as_any().downcast_ref::<InetFile>().map(|s| s.0.clone())
}

/// /proc/net/{tcp,tcp6,udp,udp6}.
pub fn proc_net(tcp: bool, v6: bool) -> String {
    use alloc::format;
    let mut out = String::from(if v6 {
        "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n"
    } else {
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n"
    });
    let socks: Vec<Arc<InetSocket>> = REGISTRY.lock().iter().filter_map(Weak::upgrade).collect();
    let hex = |b: &[u8]| -> String {
        let mut s = String::new();
        for c in b.chunks(4) {
            let w = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            s.push_str(&format!("{w:08X}"));
        }
        s
    };
    let mut i = 0;
    for s in socks {
        if (s.protocol == IPPROTO_TCP) != tcp || s.ty == SOCK_RAW || (s.family == AF_INET6) != v6 {
            continue;
        }
        let ep = |a: Vec<u8>| -> String {
            let port = u16::from_be_bytes([a[2], a[3]]);
            let addr = if v6 { &a[8..24] } else { &a[4..8] };
            format!("{}:{port:04X}", hex(addr))
        };
        let local = ep(s.local_name());
        let remote = ep(s.peer_name().unwrap_or_else(|_| s.sockaddr((s.unspecified(), 0))));
        let st = if tcp { s.tcp_state() } else { 7 };
        out.push_str(&format!(
            "{i:4}: {local} {remote} {st:02X} {:08X}:{:08X} 00:00000000 00000000     0        0 {} 1 0000000000000000 100 0 0 10 0\n",
            s.outq(),
            s.inq(),
            s.ino()
        ));
        i += 1;
    }
    out
}

/// Open (TCP, UDP) sockets of one family, for /proc/net/sockstat{,6}.
pub fn counts(v6: bool) -> (usize, usize) {
    let socks: Vec<Arc<InetSocket>> = REGISTRY.lock().iter().filter_map(Weak::upgrade).collect();
    let fam = |s: &&Arc<InetSocket>| (s.family == AF_INET6) == v6;
    let tcp = socks.iter().filter(fam).filter(|s| s.is_tcp()).count();
    let udp = socks.iter().filter(fam).filter(|s| s.ty == SOCK_DGRAM && s.protocol == IPPROTO_UDP).count();
    (tcp, udp)
}

/// Families the socket syscalls route here.
pub fn is_inet(domain: u32) -> bool {
    domain == AF_INET as u32 || domain == AF_INET6 as u32
}
