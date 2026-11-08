//! AF_NETLINK sockets.
//!
//! - NETLINK_KOBJECT_UEVENT: the kernel multicasts device events (group 1)
//!   to every bound socket: when a device is registered or removed, and
//!   when "add"/"change"/... is written to a sysfs `uevent` file (how
//!   ueventd and first-stage init "regenerate" events for existing
//!   devices). The payload is Linux's: "ACTION@DEVPATH\0ACTION=...\0
//!   DEVPATH=...\0SUBSYSTEM=...\0<uevent file lines>\0SEQNUM=n\0".
//! - Every other protocol (ROUTE, SELINUX, AUDIT, ...) is a stub: binds
//!   work, requests to the kernel are answered with NLMSG_DONE (dumps) or
//!   an NLMSG_ERROR ack (error 0 when NLM_F_ACK is set, else EOPNOTSUPP),
//!   and nothing is ever multicast.
//!
//! Messages between user sockets (unicast to a port id) are delivered.
//! Received messages carry the sender's sockaddr_nl and, with SO_PASSCRED,
//! SCM_CREDENTIALS (the kernel's are pid 0, uid 0, gid 0), which
//! libcutils' uevent_kernel_recv insists on.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use super::Ucred;
use crate::errno::*;
use crate::fs::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLERR, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::fs::vfs::{AnonInode, Inode, S_IFSOCK};
use crate::sched::{self, Wake, WaitMode};

pub const AF_NETLINK: u16 = 16;
pub const NETLINK_KOBJECT_UEVENT: u32 = 15;
/// Highest protocol number + 1 (Linux MAX_LINKS).
const MAX_LINKS: u32 = 32;
pub const SOCKADDR_NL_LEN: usize = 12;

const NLMSG_HDRLEN: usize = 16;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const NLM_F_MULTI: u16 = 2;

/// Default receive buffer (bytes queued before messages are dropped).
const DEFAULT_RCVBUF: usize = 212_992;

struct Msg {
    data: Vec<u8>,
    portid: u32,
    groups: u32,
    cred: Ucred,
}

struct Inner {
    portid: u32,
    groups: u32,
    queue: VecDeque<Msg>,
    queued: usize,
    rcvbuf: usize,
    passcred: bool,
    rcvtimeo: u64,
    /// ENOBUFS once after messages were dropped.
    overrun: bool,
}

pub struct NetlinkSocket {
    pub protocol: u32,
    pub ty: u32,
    pub wq: Arc<WaitQueue>,
    inode: Arc<dyn Inode>,
    inner: Mutex<Inner>,
}

static SOCKETS: Mutex<Vec<Weak<NetlinkSocket>>> = Mutex::new(Vec::new());
static NEXT_PORT: AtomicU32 = AtomicU32::new(0x8000_0000);
static SEQNUM: AtomicU64 = AtomicU64::new(1);

/// A sockaddr_nl.
pub fn sockaddr(portid: u32, groups: u32) -> [u8; SOCKADDR_NL_LEN] {
    let mut b = [0u8; SOCKADDR_NL_LEN];
    b[..2].copy_from_slice(&AF_NETLINK.to_le_bytes());
    b[4..8].copy_from_slice(&portid.to_le_bytes());
    b[8..].copy_from_slice(&groups.to_le_bytes());
    b
}

/// (port id, groups) of a user sockaddr_nl.
pub fn parse_addr(b: &[u8]) -> KResult<(u32, u32)> {
    if b.len() < SOCKADDR_NL_LEN {
        return Err(EINVAL);
    }
    if u16::from_le_bytes([b[0], b[1]]) != AF_NETLINK {
        return Err(EINVAL);
    }
    Ok((u32::from_le_bytes(b[4..8].try_into().unwrap()), u32::from_le_bytes(b[8..12].try_into().unwrap())))
}

pub struct Received {
    pub data: Vec<u8>,
    pub len: usize,
    pub addr: [u8; SOCKADDR_NL_LEN],
    pub cred: Option<Ucred>,
}

impl NetlinkSocket {
    pub fn new(ty: u32, protocol: u32) -> KResult<Arc<NetlinkSocket>> {
        if protocol >= MAX_LINKS {
            return Err(EPROTONOSUPPORT);
        }
        let s = Arc::new(NetlinkSocket {
            protocol,
            ty,
            wq: WaitQueue::new(),
            inode: AnonInode::new(S_IFSOCK | 0o777, 0),
            inner: Mutex::new(Inner {
                portid: 0,
                groups: 0,
                queue: VecDeque::new(),
                queued: 0,
                rcvbuf: DEFAULT_RCVBUF,
                passcred: false,
                rcvtimeo: 0,
                overrun: false,
            }),
        });
        let mut all = SOCKETS.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&s));
        Ok(s)
    }

    pub fn open_file(self: Arc<Self>, nonblock: bool) -> Arc<OpenFile> {
        let inode = self.inode.clone();
        OpenFile::new(inode, None, Box::new(NetlinkFile(self)), O_RDWR | if nonblock { O_NONBLOCK } else { 0 })
    }

    fn port_in_use(&self, port: u32) -> bool {
        live().iter().any(|s| s.protocol == self.protocol && !core::ptr::eq(&**s, self) && s.inner.lock().portid == port)
    }

    /// Port id 0 = pick one (the process id if free, like Linux).
    pub fn bind(&self, portid: u32, groups: u32) -> KResult<()> {
        let cur = self.inner.lock().portid;
        let port = if portid != 0 {
            if cur != 0 && cur != portid {
                return Err(EINVAL);
            }
            if self.port_in_use(portid) {
                return Err(EADDRINUSE);
            }
            portid
        } else if cur != 0 {
            cur
        } else {
            self.autobind_port()
        };
        let mut s = self.inner.lock();
        s.portid = port;
        s.groups = groups;
        Ok(())
    }

    fn autobind_port(&self) -> u32 {
        let pid = crate::process::current().pid();
        if !self.port_in_use(pid) {
            return pid;
        }
        loop {
            let p = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
            if !self.port_in_use(p) {
                return p;
            }
        }
    }

    fn ensure_bound(&self) {
        if self.inner.lock().portid == 0 {
            let p = self.autobind_port();
            let mut s = self.inner.lock();
            if s.portid == 0 {
                s.portid = p;
            }
        }
    }

    pub fn local_addr(&self) -> [u8; SOCKADDR_NL_LEN] {
        let s = self.inner.lock();
        sockaddr(s.portid, s.groups)
    }

    pub fn set_passcred(&self, on: bool) {
        self.inner.lock().passcred = on;
    }

    pub fn passcred(&self) -> bool {
        self.inner.lock().passcred
    }

    pub fn set_rcvbuf(&self, n: usize) {
        self.inner.lock().rcvbuf = n.clamp(2048, 64 << 20);
    }

    pub fn rcvbuf(&self) -> usize {
        self.inner.lock().rcvbuf
    }

    pub fn set_rcvtimeo(&self, ns: u64) {
        self.inner.lock().rcvtimeo = ns;
    }

    pub fn rcvtimeo(&self) -> u64 {
        self.inner.lock().rcvtimeo
    }

    pub fn groups(&self) -> u32 {
        self.inner.lock().groups
    }

    /// Queue a message; false (and ENOBUFS for the reader) when full.
    fn deliver(&self, m: Msg) -> bool {
        {
            let mut s = self.inner.lock();
            if s.queued + m.data.len() > s.rcvbuf && !s.queue.is_empty() {
                s.overrun = true;
                drop(s);
                self.wq.wake(POLLERR);
                return false;
            }
            s.queued += m.data.len();
            s.queue.push_back(m);
        }
        self.wq.wake(READ_EVENTS);
        true
    }

    /// sendto/sendmsg: to the kernel (port 0) or to another socket.
    pub fn send(&self, data: &[u8], to: Option<(u32, u32)>) -> KResult<usize> {
        self.ensure_bound();
        let (dst, groups) = to.unwrap_or((0, 0));
        let me = self.inner.lock().portid;
        let cred = Ucred::current();
        if groups != 0 {
            for s in live() {
                if s.protocol == self.protocol && s.groups() & groups != 0 && !core::ptr::eq(&*s, self) {
                    s.deliver(Msg { data: data.to_vec(), portid: me, groups, cred });
                }
            }
        }
        if dst != 0 {
            let target = live().into_iter().find(|s| s.protocol == self.protocol && s.inner.lock().portid == dst);
            match target {
                Some(t) => {
                    t.deliver(Msg { data: data.to_vec(), portid: me, groups: 0, cred });
                }
                None if groups == 0 => return Err(ECONNREFUSED),
                None => {}
            }
            return Ok(data.len());
        }
        if groups == 0 {
            self.kernel_request(data);
        }
        Ok(data.len())
    }

    /// Answer requests sent to the kernel (no protocol is implemented).
    fn kernel_request(&self, data: &[u8]) {
        if self.protocol == NETLINK_KOBJECT_UEVENT {
            return;
        }
        let mut off = 0;
        while off + NLMSG_HDRLEN <= data.len() {
            let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            let flags = u16::from_le_bytes(data[off + 6..off + 8].try_into().unwrap());
            let seq = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap());
            if len < NLMSG_HDRLEN || off + len > data.len() {
                break;
            }
            let me = self.inner.lock().portid;
            if flags & NLM_F_REQUEST != 0 {
                let reply = if flags & NLM_F_DUMP == NLM_F_DUMP {
                    nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, me, &0i32.to_le_bytes())
                } else {
                    let err: i32 = if flags & NLM_F_ACK != 0 { 0 } else { -(EOPNOTSUPP.0 as i32) };
                    let mut p = err.to_le_bytes().to_vec();
                    p.extend_from_slice(&data[off..off + NLMSG_HDRLEN]);
                    nlmsg(NLMSG_ERROR, 0, seq, me, &p)
                };
                self.deliver(Msg { data: reply, portid: 0, groups: 0, cred: KERNEL_CRED });
            }
            off += (len + 3) & !3;
        }
    }

    pub fn recv(&self, len: usize, peek: bool, nonblock: bool) -> KResult<Received> {
        let timeo = self.inner.lock().rcvtimeo;
        let mut res = Err(EAGAIN);
        let deadline = (timeo != 0).then(|| crate::time::monotonic_ns().saturating_add(timeo));
        let w = sched::wait_until(self.wq.key(), deadline, WaitMode::Interruptible, || {
            let mut s = self.inner.lock();
            if s.overrun {
                s.overrun = false;
                res = Err(ENOBUFS);
                return true;
            }
            let passcred = s.passcred;
            let m = if peek { s.queue.front().map(|m| (m.data.clone(), m.portid, m.groups, m.cred)) } else {
                s.queue.pop_front().map(|m| (m.data, m.portid, m.groups, m.cred))
            };
            match m {
                Some((data, portid, groups, cred)) => {
                    if !peek {
                        s.queued -= data.len();
                    }
                    let full = data.len();
                    let mut data = data;
                    data.truncate(len);
                    res = Ok(Received { data, len: full, addr: sockaddr(portid, groups), cred: passcred.then_some(cred) });
                    true
                }
                None => nonblock,
            }
        });
        match w {
            Wake::Ready => res,
            Wake::Timeout => Err(EAGAIN),
            Wake::Interrupted => Err(if timeo != 0 { EINTR } else { ERESTARTSYS }),
        }
    }

    pub fn poll(&self) -> u32 {
        let s = self.inner.lock();
        let mut m = WRITE_EVENTS;
        if !s.queue.is_empty() {
            m |= READ_EVENTS;
        }
        if s.overrun {
            m |= POLLERR;
        }
        m
    }

    pub fn inq(&self) -> usize {
        self.inner.lock().queue.front().map_or(0, |m| m.data.len())
    }
}

const KERNEL_CRED: Ucred = Ucred { pid: 0, uid: 0, gid: 0 };

fn nlmsg(ty: u16, flags: u16, seq: u32, pid: u32, payload: &[u8]) -> Vec<u8> {
    let len = NLMSG_HDRLEN + payload.len();
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(&(len as u32).to_le_bytes());
    v.extend_from_slice(&ty.to_le_bytes());
    v.extend_from_slice(&flags.to_le_bytes());
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&pid.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn live() -> Vec<Arc<NetlinkSocket>> {
    SOCKETS.lock().iter().filter_map(Weak::upgrade).collect()
}

/// Broadcast a uevent to every NETLINK_KOBJECT_UEVENT socket bound to
/// group 1. `vars` are the lines of the device's `uevent` file.
pub fn send_uevent(action: &str, devpath: &str, subsystem: &str, vars: &str) {
    let seq = SEQNUM.fetch_add(1, Ordering::Relaxed);
    let mut msg = format!("{action}@{devpath}\0ACTION={action}\0DEVPATH={devpath}\0SUBSYSTEM={subsystem}\0").into_bytes();
    for l in vars.lines().filter(|l| !l.is_empty()) {
        msg.extend_from_slice(l.as_bytes());
        msg.push(0);
    }
    msg.extend_from_slice(format!("SEQNUM={seq}\0").as_bytes());
    for s in live() {
        if s.protocol == NETLINK_KOBJECT_UEVENT && s.groups() & 1 != 0 {
            s.deliver(Msg { data: msg.clone(), portid: 0, groups: 1, cred: KERNEL_CRED });
        }
    }
}

/// The last sequence number handed out (/sys/kernel/uevent_seqnum).
pub fn uevent_seqnum() -> u64 {
    SEQNUM.load(Ordering::Relaxed) - 1
}

pub struct NetlinkFile(pub Arc<NetlinkSocket>);

const FIONREAD: u32 = 0x541b;

impl FileOps for NetlinkFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let r = self.0.recv(buf.len(), false, f.nonblock())?;
        buf[..r.data.len()].copy_from_slice(&r.data);
        Ok(r.data.len())
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        self.0.send(data, None)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd != FIONREAD {
            return Err(ENOTTY);
        }
        crate::process::current().mm().copy_out(arg, &(self.0.inq() as i32).to_le_bytes())?;
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

pub fn netlink_of(f: &OpenFile) -> Option<Arc<NetlinkSocket>> {
    f.ops.as_any().downcast_ref::<NetlinkFile>().map(|s| s.0.clone())
}

/// /proc/net/netlink.
pub fn proc_net_netlink() -> String {
    let mut out = String::from("sk               Eth Pid        Groups   Rmem     Wmem     Dump  Locks    Drops    Inode\n");
    for (i, s) in live().iter().enumerate() {
        let g = s.inner.lock();
        out.push_str(&format!(
            "{:016x} {:<3} {:<10} {:08x} {:<8} {:<8} {:<5} {:<8} {:<8} {}\n",
            i,
            s.protocol,
            g.portid,
            g.groups,
            g.queued,
            0,
            0,
            2,
            0,
            s.inode.meta().ino
        ));
    }
    out
}
