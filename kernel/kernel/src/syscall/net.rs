//! Socket syscalls (`crate::net`): socket/socketpair, bind/listen/connect/
//! accept, the send/recv family with ancillary data (SCM_RIGHTS,
//! SCM_CREDENTIALS), shutdown, get/setsockopt, getsockname/getpeername.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::fs::iovecs;
use crate::errno::*;
use crate::fs::file::OpenFile;
use crate::fs::vfs::PathRef;
use crate::net::unix::{Addr, Opt, Received, SCM_MAX_FD, SOCKADDR_UN_LEN, UnixSocket, socket_of};
use crate::net::*;
use crate::process::{current, signal};

const SO_DEBUG: i32 = 1;
const SO_REUSEADDR: i32 = 2;
const SO_TYPE: i32 = 3;
const SO_ERROR: i32 = 4;
const SO_DONTROUTE: i32 = 5;
const SO_BROADCAST: i32 = 6;
const SO_SNDBUF: i32 = 7;
const SO_RCVBUF: i32 = 8;
const SO_KEEPALIVE: i32 = 9;
const SO_OOBINLINE: i32 = 10;
const SO_PRIORITY: i32 = 12;
const SO_LINGER: i32 = 13;
const SO_REUSEPORT: i32 = 15;
const SO_PASSCRED: i32 = 16;
const SO_PEERCRED: i32 = 17;
const SO_RCVLOWAT: i32 = 18;
const SO_SNDLOWAT: i32 = 19;
const SO_RCVTIMEO_OLD: i32 = 20;
const SO_SNDTIMEO_OLD: i32 = 21;
const SO_ACCEPTCONN: i32 = 30;
const SO_PEERSEC: i32 = 31;
const SO_SNDBUFFORCE: i32 = 32;
const SO_RCVBUFFORCE: i32 = 33;
const SO_PASSSEC: i32 = 34;
const SO_PROTOCOL: i32 = 38;
const SO_DOMAIN: i32 = 39;
const SO_RCVTIMEO_NEW: i32 = 66;
const SO_SNDTIMEO_NEW: i32 = 67;

/// sizeof(struct msghdr) / (struct mmsghdr) on x86_64.
const MSGHDR_LEN: usize = 56;
const MMSGHDR_LEN: u64 = 64;
const CMSG_HDR: usize = 16;
/// Largest receive buffer copied through the kernel at once.
const RECV_MAX: usize = 4 << 20;

fn cmsg_align(n: usize) -> usize {
    (n + 7) & !7
}

fn sock(fd: i32) -> KResult<(Arc<OpenFile>, Arc<UnixSocket>)> {
    let f = current().process.files().get(fd)?;
    let s = socket_of(&f)?;
    Ok((f, s))
}

fn base() -> PathRef {
    current().process.cwd()
}

/// A user sockaddr of `len` bytes.
fn read_addr(ptr: u64, len: u32) -> KResult<Addr> {
    if len as usize > SOCKADDR_UN_LEN || len < 2 {
        return Err(EINVAL);
    }
    let mut b = vec![0u8; len as usize];
    current().mm().copy_in(ptr, &mut b)?;
    Addr::parse(&b)
}

/// Copy `addr` out to (ptr, *lenp): truncated to the buffer, real length
/// stored back.
fn write_addr(addr: &[u8], ptr: u64, lenp: u64) -> KResult<()> {
    if ptr == 0 || lenp == 0 {
        return Ok(());
    }
    let mm = current().mm();
    let mut lb = [0u8; 4];
    mm.copy_in(lenp, &mut lb)?;
    let len = i32::from_le_bytes(lb);
    if len < 0 {
        return Err(EINVAL);
    }
    let n = (len as usize).min(addr.len());
    mm.copy_out(ptr, &addr[..n])?;
    mm.copy_out(lenp, &(addr.len() as u32).to_le_bytes())
}

fn new_socket_args(domain: u32, ty: u32, protocol: u32) -> KResult<(u32, bool, bool)> {
    if ty & !(SOCK_TYPE_MASK | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    if domain >= AF_MAX {
        return Err(EAFNOSUPPORT);
    }
    let base = ty & SOCK_TYPE_MASK;
    if base >= SOCK_MAX {
        return Err(EINVAL);
    }
    if domain != AF_UNIX as u32 {
        return Err(EAFNOSUPPORT);
    }
    if protocol != 0 && protocol != AF_UNIX as u32 {
        return Err(EPROTONOSUPPORT);
    }
    let base = match base {
        SOCK_STREAM | SOCK_SEQPACKET | SOCK_DGRAM => base,
        SOCK_RAW => SOCK_DGRAM,
        _ => return Err(ESOCKTNOSUPPORT),
    };
    Ok((base, ty & SOCK_NONBLOCK != 0, ty & SOCK_CLOEXEC != 0))
}

pub fn sys_socket(domain: u32, ty: u32, protocol: u32) -> KResult<u64> {
    let (base, nonblock, cloexec) = new_socket_args(domain, ty, protocol)?;
    let f = UnixSocket::new(base).open_file(nonblock);
    Ok(current().process.files().alloc(f, cloexec, 0)? as u64)
}

pub fn sys_socketpair(domain: u32, ty: u32, protocol: u32, sv: u64) -> KResult<u64> {
    let (base, nonblock, cloexec) = new_socket_args(domain, ty, protocol)?;
    let (a, b) = UnixSocket::pair(base, Ucred::current());
    let files = current().process.files();
    let fa = files.alloc(a.open_file(nonblock), cloexec, 0)?;
    let fb = match files.alloc(b.open_file(nonblock), cloexec, 0) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = files.close(fa);
            return Err(e);
        }
    };
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&fa.to_le_bytes());
    out[4..].copy_from_slice(&fb.to_le_bytes());
    if let Err(e) = current().mm().copy_out(sv, &out) {
        let _ = files.close(fa);
        let _ = files.close(fb);
        return Err(e);
    }
    Ok(0)
}

pub fn sys_bind(fd: i32, addr: u64, len: u32) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    let a = read_addr(addr, len)?;
    let umask = current().process.fs.lock().umask;
    s.bind(a, &base(), umask)?;
    Ok(0)
}

pub fn sys_listen(fd: i32, backlog: i32) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    s.listen(backlog, Ucred::current())?;
    Ok(0)
}

pub fn sys_connect(fd: i32, addr: u64, len: u32) -> KResult<u64> {
    let (f, s) = sock(fd)?;
    // AF_UNSPEC dissolves a datagram association.
    if len >= 2 && s.ty == SOCK_DGRAM {
        let mut fam = [0u8; 2];
        current().mm().copy_in(addr, &mut fam)?;
        if u16::from_le_bytes(fam) == AF_UNSPEC {
            s.connect(None, &base(), Ucred::current(), f.nonblock())?;
            return Ok(0);
        }
    }
    let a = read_addr(addr, len)?;
    if a == Addr::Unnamed {
        return Err(EINVAL);
    }
    s.connect(Some(a), &base(), Ucred::current(), f.nonblock())?;
    Ok(0)
}

pub fn sys_accept4(fd: i32, addr: u64, lenp: u64, flags: u32) -> KResult<u64> {
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let (f, s) = sock(fd)?;
    let c = s.accept(f.nonblock())?;
    let peer = c.peer_addr().unwrap_or(Addr::Unnamed).to_sockaddr();
    let nf = c.open_file(flags & SOCK_NONBLOCK != 0);
    write_addr(&peer, addr, lenp)?;
    Ok(current().process.files().alloc(nf, flags & SOCK_CLOEXEC != 0, 0)? as u64)
}

pub fn sys_getsockname(fd: i32, addr: u64, lenp: u64) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    write_addr(&s.local_addr().to_sockaddr(), addr, lenp)?;
    Ok(0)
}

pub fn sys_getpeername(fd: i32, addr: u64, lenp: u64) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    write_addr(&s.peer_addr()?.to_sockaddr(), addr, lenp)?;
    Ok(0)
}

pub fn sys_shutdown(fd: i32, how: u32) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    s.shutdown(how)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// send / recv

/// Files and credentials from a control buffer.
fn parse_cmsgs(ctl: &[u8]) -> KResult<(Vec<Arc<OpenFile>>, Option<Ucred>)> {
    let files = current().process.files();
    let mut fds = Vec::new();
    let mut cred = None;
    let mut off = 0;
    while off + CMSG_HDR <= ctl.len() {
        let len = u64::from_le_bytes(ctl[off..off + 8].try_into().unwrap()) as usize;
        let level = i32::from_le_bytes(ctl[off + 8..off + 12].try_into().unwrap());
        let ty = i32::from_le_bytes(ctl[off + 12..off + 16].try_into().unwrap());
        if len < CMSG_HDR || len > ctl.len() - off {
            return Err(EINVAL);
        }
        if level != SOL_SOCKET {
            return Err(EINVAL);
        }
        let data = &ctl[off + CMSG_HDR..off + len];
        match ty {
            SCM_RIGHTS => {
                if data.len() % 4 != 0 || fds.len() + data.len() / 4 > SCM_MAX_FD {
                    return Err(EINVAL);
                }
                for c in data.chunks(4) {
                    fds.push(files.get(i32::from_le_bytes(c.try_into().unwrap()))?);
                }
            }
            SCM_CREDENTIALS => {
                if data.len() != 12 {
                    return Err(EINVAL);
                }
                let w = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
                cred = Some(Ucred { pid: w(0), uid: w(4), gid: w(8) });
            }
            _ => return Err(EINVAL),
        }
        off += cmsg_align(len);
    }
    Ok((fds, cred))
}

/// Gather the iovecs into one buffer.
fn gather(iov: u64, cnt: u64) -> KResult<Vec<u8>> {
    let mm = current().mm();
    let mut data = Vec::new();
    for (base, len) in iovecs(iov, cnt)? {
        let start = data.len();
        data.try_reserve(len as usize).map_err(|_| ENOMEM)?;
        data.resize(start + len as usize, 0);
        mm.copy_in(base, &mut data[start..])?;
    }
    Ok(data)
}

fn scatter(iov: &[(u64, u64)], data: &[u8]) -> KResult<()> {
    let mm = current().mm();
    let mut done = 0;
    for &(base, len) in iov {
        if done >= data.len() {
            break;
        }
        let n = (len as usize).min(data.len() - done);
        mm.copy_out(base, &data[done..done + n])?;
        done += n;
    }
    Ok(())
}

fn do_send(fd: i32, data: &[u8], flags: u32, to: Option<Addr>, fds: Vec<Arc<OpenFile>>, cred: Option<Ucred>) -> KResult<u64> {
    let (f, s) = sock(fd)?;
    if flags & MSG_OOB != 0 {
        return Err(EOPNOTSUPP);
    }
    let to = to.map(|a| (a, base()));
    let nonblock = f.nonblock() || flags & MSG_DONTWAIT != 0;
    let r = s.send(data, fds, cred.unwrap_or_else(Ucred::current), to, nonblock);
    if r == Err(EPIPE) && flags & MSG_NOSIGNAL == 0 {
        signal::send_to_current(signal::SIGPIPE);
    }
    Ok(r? as u64)
}

pub fn sys_sendto(fd: i32, buf: u64, len: u64, flags: u32, addr: u64, alen: u32) -> KResult<u64> {
    sock(fd)?;
    let to = if addr != 0 && alen != 0 { Some(read_addr(addr, alen)?) } else { None };
    let len = (len as usize).min(i32::MAX as usize);
    let mut data = Vec::new();
    data.try_reserve_exact(len).map_err(|_| ENOMEM)?;
    data.resize(len, 0);
    current().mm().copy_in(buf, &mut data)?;
    do_send(fd, &data, flags, to, Vec::new(), None)
}

fn sendmsg_one(fd: i32, msg: u64, flags: u32) -> KResult<u64> {
    let mm = current().mm();
    let mut h = [0u8; MSGHDR_LEN];
    mm.copy_in(msg, &mut h)?;
    let q = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let (name, namelen) = (q(0), u32::from_le_bytes(h[8..12].try_into().unwrap()));
    let (iov, iovlen, control, controllen) = (q(16), q(24), q(32), q(40));
    sock(fd)?;
    let to = if name != 0 && namelen != 0 { Some(read_addr(name, namelen)?) } else { None };
    let (fds, cred) = if controllen >= CMSG_HDR as u64 {
        if controllen > i32::MAX as u64 {
            return Err(ENOBUFS);
        }
        let mut ctl = vec![0u8; controllen as usize];
        mm.copy_in(control, &mut ctl)?;
        parse_cmsgs(&ctl)?
    } else {
        (Vec::new(), None)
    };
    let data = gather(iov, iovlen)?;
    do_send(fd, &data, flags, to, fds, cred)
}

pub fn sys_sendmsg(fd: i32, msg: u64, flags: u32) -> KResult<u64> {
    sendmsg_one(fd, msg, flags)
}

pub fn sys_sendmmsg(fd: i32, vec: u64, vlen: u32, flags: u32) -> KResult<u64> {
    let mut n = 0u64;
    for i in 0..vlen.min(1024) as u64 {
        let m = vec + i * MMSGHDR_LEN;
        match sendmsg_one(fd, m, flags) {
            Ok(len) => {
                current().mm().copy_out(m + MSGHDR_LEN as u64, &(len as u32).to_le_bytes())?;
                n += 1;
            }
            Err(e) if n == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(n)
}

fn do_recv(fd: i32, len: usize, flags: u32) -> KResult<(Arc<UnixSocket>, Vec<u8>, Received)> {
    let (f, s) = sock(fd)?;
    if flags & MSG_OOB != 0 {
        return Err(EOPNOTSUPP);
    }
    let mut buf = vec![0u8; len.min(RECV_MAX)];
    let nonblock = f.nonblock() || flags & MSG_DONTWAIT != 0;
    let r = s.recv(&mut buf, flags, nonblock)?;
    Ok((s, buf, r))
}

/// The result of a receive: the real datagram size with MSG_TRUNC.
fn recv_result(s: &UnixSocket, r: &Received, flags: u32) -> u64 {
    if flags & MSG_TRUNC != 0 && s.ty != SOCK_STREAM { r.len as u64 } else { r.n as u64 }
}

pub fn sys_recvfrom(fd: i32, buf: u64, len: u64, flags: u32, addr: u64, alen: u64) -> KResult<u64> {
    let (s, data, r) = do_recv(fd, len as usize, flags)?;
    current().mm().copy_out(buf, &data[..r.n])?;
    if addr != 0 {
        let a = r.from.as_ref().map_or(Vec::new(), Addr::to_sockaddr);
        write_addr(&a, addr, alen)?;
    }
    Ok(recv_result(&s, &r, flags))
}

/// Build the control data for a received message; installs passed files.
/// Returns (bytes, MSG_CTRUNC or 0).
fn build_cmsgs(s: &UnixSocket, r: Received, room: usize, flags: u32) -> (Vec<u8>, u32) {
    let mut out = Vec::new();
    let mut trunc = 0;
    let put = |out: &mut Vec<u8>, trunc: &mut u32, ty: i32, data: &[u8]| {
        let left = room - out.len();
        if left < CMSG_HDR {
            *trunc = MSG_CTRUNC;
            return;
        }
        let mut c = Vec::with_capacity(CMSG_HDR + data.len());
        c.extend_from_slice(&((CMSG_HDR + data.len()) as u64).to_le_bytes());
        c.extend_from_slice(&SOL_SOCKET.to_le_bytes());
        c.extend_from_slice(&ty.to_le_bytes());
        c.extend_from_slice(data);
        if c.len() > left {
            *trunc = MSG_CTRUNC;
        }
        c.resize(cmsg_align(c.len()).min(left), 0);
        out.extend_from_slice(&c);
    };
    if s.passcred()
        && let Some(c) = r.cred
    {
        put(&mut out, &mut trunc, SCM_CREDENTIALS, &c.to_bytes());
    }
    if !r.fds.is_empty() {
        let left = room - out.len();
        let max = left.saturating_sub(CMSG_HDR) / 4;
        let files = current().process.files();
        let cloexec = flags & MSG_CMSG_CLOEXEC != 0;
        let mut fds = Vec::new();
        let mut rest = r.fds.into_iter();
        for f in rest.by_ref().take(max) {
            match files.alloc(f, cloexec, 0) {
                Ok(fd) => fds.extend_from_slice(&fd.to_le_bytes()),
                Err(_) => break,
            }
        }
        if rest.next().is_some() || left < CMSG_HDR {
            trunc = MSG_CTRUNC;
        }
        if !fds.is_empty() {
            put(&mut out, &mut trunc, SCM_RIGHTS, &fds);
        }
        // Files that did not fit are closed when `rest` drops.
    }
    (out, trunc)
}

fn recvmsg_one(fd: i32, msg: u64, flags: u32) -> KResult<u64> {
    let mm = current().mm();
    let mut h = [0u8; MSGHDR_LEN];
    mm.copy_in(msg, &mut h)?;
    let q = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let (name, namelen) = (q(0), u32::from_le_bytes(h[8..12].try_into().unwrap()) as i32);
    let (iov, iovlen, control, controllen) = (q(16), q(24), q(32), q(40));
    let iov = iovecs(iov, iovlen)?;
    let total: u64 = iov.iter().map(|&(_, l)| l).sum();
    let (s, data, r) = do_recv(fd, total.min(isize::MAX as u64) as usize, flags)?;
    scatter(&iov, &data[..r.n])?;
    let mut mflags = 0;
    if s.ty != SOCK_STREAM && r.len > r.n {
        mflags |= MSG_TRUNC;
    }
    let ret = recv_result(&s, &r, flags);
    if name != 0 && namelen >= 0 {
        let a = r.from.as_ref().map_or(Vec::new(), Addr::to_sockaddr);
        let n = (namelen as usize).min(a.len());
        mm.copy_out(name, &a[..n])?;
        mm.copy_out(msg + 8, &(a.len() as u32).to_le_bytes())?;
    } else {
        mm.copy_out(msg + 8, &0u32.to_le_bytes())?;
    }
    let room = if control == 0 { 0 } else { controllen.min(i32::MAX as u64) as usize };
    let (ctl, trunc) = build_cmsgs(&s, r, room, flags);
    mflags |= trunc;
    if !ctl.is_empty() {
        mm.copy_out(control, &ctl)?;
    }
    mm.copy_out(msg + 40, &(ctl.len() as u64).to_le_bytes())?;
    mm.copy_out(msg + 48, &mflags.to_le_bytes())?;
    Ok(ret)
}

pub fn sys_recvmsg(fd: i32, msg: u64, flags: u32) -> KResult<u64> {
    recvmsg_one(fd, msg, flags)
}

pub fn sys_recvmmsg(fd: i32, vec: u64, vlen: u32, flags: u32, _timeout: u64) -> KResult<u64> {
    let mut n = 0u64;
    let mut fl = flags & !MSG_WAITFORONE;
    for i in 0..vlen.min(1024) as u64 {
        let m = vec + i * MMSGHDR_LEN;
        match recvmsg_one(fd, m, fl) {
            Ok(len) => {
                current().mm().copy_out(m + MSGHDR_LEN as u64, &(len as u32).to_le_bytes())?;
                n += 1;
            }
            Err(e) if n == 0 => return Err(e),
            Err(_) => break,
        }
        if flags & MSG_WAITFORONE != 0 {
            fl |= MSG_DONTWAIT;
        }
    }
    Ok(n)
}

/// read/readv on a socket: one receive scattered over the iovecs.
pub fn readv(f: &OpenFile, s: &UnixSocket, iov: &[(u64, u64)]) -> KResult<u64> {
    let total: u64 = iov.iter().map(|&(_, l)| l).sum();
    let mut buf = vec![0u8; (total as usize).min(RECV_MAX)];
    let r = s.recv(&mut buf, 0, f.nonblock())?;
    scatter(iov, &buf[..r.n])?;
    Ok(r.n as u64)
}

// ---------------------------------------------------------------------------
// Options

fn timeval_ns(b: &[u8]) -> KResult<u64> {
    if b.len() < 16 {
        return Err(EINVAL);
    }
    let sec = i64::from_le_bytes(b[..8].try_into().unwrap());
    let usec = i64::from_le_bytes(b[8..16].try_into().unwrap());
    if !(0..1_000_000).contains(&usec) {
        return Err(EDOM);
    }
    if sec < 0 {
        return Ok(0);
    }
    Ok((sec as u64).saturating_mul(1_000_000_000).saturating_add(usec as u64 * 1000))
}

fn ns_timeval(ns: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
    b[8..].copy_from_slice(&(((ns % 1_000_000_000) / 1000) as i64).to_le_bytes());
    b
}

pub fn sys_setsockopt(fd: i32, level: i32, name: i32, val: u64, len: u32) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    if level != SOL_SOCKET {
        return Err(EOPNOTSUPP);
    }
    let mut b = vec![0u8; (len as usize).min(64)];
    current().mm().copy_in(val, &mut b)?;
    let int = || -> KResult<i32> {
        if b.len() < 4 {
            return Err(EINVAL);
        }
        Ok(i32::from_le_bytes(b[..4].try_into().unwrap()))
    };
    match name {
        SO_PASSCRED => s.set_opt(Opt::PassCred(int()? != 0)),
        SO_SNDBUF | SO_SNDBUFFORCE => s.set_opt(Opt::SndBuf(int()?.max(0) as usize)),
        SO_RCVBUF | SO_RCVBUFFORCE => s.set_opt(Opt::RcvBuf(int()?.max(0) as usize)),
        SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => s.set_opt(Opt::RcvTimeo(timeval_ns(&b)?)),
        SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => s.set_opt(Opt::SndTimeo(timeval_ns(&b)?)),
        SO_DEBUG | SO_REUSEADDR | SO_DONTROUTE | SO_BROADCAST | SO_KEEPALIVE | SO_OOBINLINE | SO_PRIORITY
        | SO_REUSEPORT | SO_RCVLOWAT | SO_PASSSEC => {
            int()?;
        }
        SO_LINGER => {
            if b.len() < 8 {
                return Err(EINVAL);
            }
        }
        _ => return Err(ENOPROTOOPT),
    }
    Ok(0)
}

pub fn sys_getsockopt(fd: i32, level: i32, name: i32, val: u64, lenp: u64) -> KResult<u64> {
    let (_, s) = sock(fd)?;
    let mm = current().mm();
    let mut lb = [0u8; 4];
    mm.copy_in(lenp, &mut lb)?;
    let len = i32::from_le_bytes(lb);
    if len < 0 {
        return Err(EINVAL);
    }
    if level != SOL_SOCKET {
        return Err(EOPNOTSUPP);
    }
    let int = |v: i64| (v as i32).to_le_bytes().to_vec();
    let v: Vec<u8> = match name {
        SO_TYPE => int(s.ty as i64),
        SO_DOMAIN => int(AF_UNIX as i64),
        SO_PROTOCOL => int(0),
        SO_ERROR => int(s.take_error().map_or(0, |e| e.0 as i64)),
        SO_SNDBUF => int(s.sndbuf() as i64),
        SO_RCVBUF => int(s.rcvbuf() as i64),
        SO_PASSCRED => int(s.passcred() as i64),
        SO_ACCEPTCONN => int(s.listening() as i64),
        SO_PEERCRED => s.peer_cred().to_bytes().to_vec(),
        SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => ns_timeval(s.timeouts().0).to_vec(),
        SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => ns_timeval(s.timeouts().1).to_vec(),
        SO_RCVLOWAT | SO_SNDLOWAT => int(1),
        SO_DEBUG | SO_REUSEADDR | SO_DONTROUTE | SO_BROADCAST | SO_KEEPALIVE | SO_OOBINLINE | SO_PRIORITY
        | SO_REUSEPORT | SO_PASSSEC => int(0),
        SO_LINGER => vec![0u8; 8],
        SO_PEERSEC => return Err(ENOPROTOOPT),
        _ => return Err(ENOPROTOOPT),
    };
    let n = (len as usize).min(v.len());
    mm.copy_out(val, &v[..n])?;
    mm.copy_out(lenp, &(n as u32).to_le_bytes())?;
    Ok(0)
}

