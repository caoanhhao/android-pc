//! AF_PACKET sockets: raw Ethernet frames of the cards, what Android's
//! NetworkStack needs for DHCP and ARP once EthernetService manages eth0
//! (it clears the interface's address and asks QEMU's DHCP server).
//!
//! Frames a card receives are copied, before the IP stack sees them, to
//! every packet socket bound to that card (or to all, ifindex 0) whose
//! protocol matches (or ETH_P_ALL) and whose classic BPF filter
//! (SO_ATTACH_FILTER) accepts them. SOCK_RAW sockets get and send whole
//! frames, SOCK_DGRAM ones the payload (the header comes from
//! sockaddr_ll). Outgoing frames of the stack itself are not looped back
//! to packet sockets, and there is no PACKET_MMAP or fanout.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLIN, POLLOUT, POLLRDNORM, POLLWRNORM, WaitQueue};
use crate::fs::vfs::{AnonInode, Inode, S_IFSOCK};
use crate::sched::{self, Wake, WaitMode};

pub const AF_PACKET: u16 = 17;
pub const ETH_P_ALL: u16 = 0x0003;
const SOCKADDR_LL_LEN: usize = 20;
const QUEUE_MAX: usize = 128;
const PACKET_HOST: u8 = 0;
const PACKET_BROADCAST: u8 = 1;
const PACKET_MULTICAST: u8 = 2;
const PACKET_OTHERHOST: u8 = 3;

/// A received frame and where it came from.
struct Frame {
    data: Vec<u8>,
    ifindex: u32,
}

struct Inner {
    /// Ethertype (host order), ETH_P_ALL, or 0: receive nothing.
    proto: u16,
    ifindex: u32,
    filter: Option<Vec<Insn>>,
    queue: VecDeque<Frame>,
}

pub struct PacketSocket {
    pub ty: u32,
    pub inode: Arc<dyn Inode>,
    inner: Mutex<Inner>,
    wq: Arc<WaitQueue>,
}

static SOCKETS: Mutex<Vec<Weak<PacketSocket>>> = Mutex::new(Vec::new());

#[derive(Clone, Copy)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// Run a classic BPF program over `p`: the number of bytes to keep
/// (0 drops). Out-of-bounds loads and invalid code drop the packet.
fn run_filter(prog: &[Insn], p: &[u8], ifindex: u32) -> u32 {
    let (mut a, mut x) = (0u32, 0u32);
    let mut mem = [0u32; 16];
    let mut pc = 0;
    // Linux's special offsets: SKF_NET_OFF + n is n bytes into the network
    // header (after the 14-byte Ethernet header), SKF_LL_OFF + n into the
    // link-layer header; SKF_AD_OFF + n is ancillary data: the protocol,
    // the packet type (wpa_supplicant's filter drops PACKET_OTHERHOST)
    // and the interface index.
    const SKF_NET_OFF: i32 = -0x10_0000;
    const SKF_LL_OFF: i32 = -0x20_0000;
    const SKF_AD_OFF: i32 = -0x1000;
    let pkttype = if p[0..6] == [0xff; 6] {
        PACKET_BROADCAST
    } else if p[0] & 1 != 0 {
        PACKET_MULTICAST
    } else {
        // The stack only passes on frames for the card's own address.
        PACKET_HOST
    };
    let load = |off: u32, size: u32| -> Option<u32> {
        let off = off as i32;
        if (SKF_AD_OFF..SKF_AD_OFF + 0x100).contains(&off) {
            return match off - SKF_AD_OFF {
                0 => Some(u16::from_be_bytes([p[12], p[13]]) as u32),
                4 => Some(pkttype as u32),
                8 => Some(ifindex),
                _ => None,
            };
        }
        let o = if off >= 0 {
            off as usize
        } else if off >= SKF_NET_OFF {
            14 + (off - SKF_NET_OFF) as usize
        } else if off >= SKF_LL_OFF {
            (off - SKF_LL_OFF) as usize
        } else {
            return None;
        };
        let s = size as usize;
        let b = p.get(o..o.checked_add(s)?)?;
        Some(b.iter().fold(0u32, |v, &c| (v << 8) | c as u32))
    };
    while pc < prog.len() {
        let i = prog[pc];
        pc += 1;
        let size = match i.code & 0x18 {
            0x00 => 4,
            0x08 => 2,
            _ => 1,
        };
        match i.code & 0x07 {
            0x00 | 0x01 => {
                // LD / LDX
                let v = match i.code & 0xe0 {
                    0x00 => Some(i.k),
                    0x20 => load(i.k, size),
                    0x40 => load(x.wrapping_add(i.k), size),
                    0x60 => mem.get(i.k as usize).copied(),
                    0x80 => Some(p.len() as u32),
                    0xa0 => load(i.k, 1).map(|b| (b & 0xf) * 4), // MSH
                    _ => None,
                };
                let Some(v) = v else { return 0 };
                if i.code & 0x07 == 0 { a = v } else { x = v }
            }
            0x02 => *mem.get_mut(i.k as usize).unwrap_or(&mut 0) = a,
            0x03 => *mem.get_mut(i.k as usize).unwrap_or(&mut 0) = x,
            0x04 => {
                let b = if i.code & 0x08 != 0 { x } else { i.k };
                a = match i.code & 0xf0 {
                    0x00 => a.wrapping_add(b),
                    0x10 => a.wrapping_sub(b),
                    0x20 => a.wrapping_mul(b),
                    0x30 => match a.checked_div(b) {
                        Some(v) => v,
                        None => return 0,
                    },
                    0x40 => a | b,
                    0x50 => a & b,
                    0x60 => a.wrapping_shl(b),
                    0x70 => a.wrapping_shr(b),
                    0x80 => a.wrapping_neg(),
                    0x90 => match a.checked_rem(b) {
                        Some(v) => v,
                        None => return 0,
                    },
                    0xa0 => a ^ b,
                    _ => return 0,
                };
            }
            0x05 => {
                let b = if i.code & 0x08 != 0 { x } else { i.k };
                let taken = match i.code & 0xf0 {
                    0x00 => {
                        pc += i.k as usize;
                        continue;
                    }
                    0x10 => a == b,
                    0x20 => a > b,
                    0x30 => a >= b,
                    0x40 => a & b != 0,
                    _ => return 0,
                };
                pc += if taken { i.jt } else { i.jf } as usize;
            }
            0x06 => return if i.code & 0x18 == 0x10 { a } else { i.k },
            _ => {
                // MISC: TAX / TXA.
                if i.code & 0xf8 == 0 { x = a } else { a = x }
            }
        }
    }
    0
}

impl PacketSocket {
    pub fn new(ty: u32, proto_be: u16) -> Arc<PacketSocket> {
        let s = Arc::new(PacketSocket {
            ty,
            inode: AnonInode::new(S_IFSOCK | 0o777, 0),
            inner: Mutex::new(Inner { proto: u16::from_be(proto_be), ifindex: 0, filter: None, queue: VecDeque::new() }),
            wq: WaitQueue::new(),
        });
        let mut all = SOCKETS.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&s));
        s
    }

    pub fn open_file(self: Arc<Self>, nonblock: bool) -> Arc<OpenFile> {
        let inode = self.inode.clone();
        OpenFile::new(inode, None, Box::new(PacketFile(self)), O_RDWR | if nonblock { O_NONBLOCK } else { 0 })
    }

    /// bind(sockaddr_ll): protocol and interface (0: any).
    pub fn bind(&self, proto_be: u16, ifindex: u32) {
        let mut i = self.inner.lock();
        if proto_be != 0 {
            i.proto = u16::from_be(proto_be);
        }
        i.ifindex = ifindex;
        i.queue.clear();
    }

    pub fn attach_filter(&self, prog: Option<Vec<(u16, u8, u8, u32)>>) {
        self.inner.lock().filter = prog.map(|v| v.into_iter().map(|(code, jt, jf, k)| Insn { code, jt, jf, k }).collect());
    }

    pub fn local_addr(&self) -> Vec<u8> {
        let i = self.inner.lock();
        sockaddr_ll(i.proto, i.ifindex, 0, &[0; 6])
    }

    /// Receive one frame: (bytes copied, full length, sockaddr_ll of the
    /// sender).
    pub fn recv(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> KResult<(usize, usize, Vec<u8>)> {
        let mut out = None;
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            let mut i = self.inner.lock();
            let f = if peek { i.queue.front().map(|f| Frame { data: f.data.clone(), ifindex: f.ifindex }) } else { i.queue.pop_front() };
            match f {
                Some(f) => {
                    out = Some(f);
                    true
                }
                None => nonblock,
            }
        });
        let Some(f) = out else {
            return Err(if w == Wake::Ready { EAGAIN } else { ERESTARTSYS });
        };
        let src: [u8; 6] = f.data[6..12].try_into().unwrap();
        let ethertype = u16::from_be_bytes([f.data[12], f.data[13]]);
        let dst = &f.data[0..6];
        let pkttype = if dst == [0xff; 6] {
            PACKET_BROADCAST
        } else if dst[0] & 1 != 0 {
            PACKET_MULTICAST
        } else {
            let ours = super::inet::with_net_nopoll(|n| n.link(f.ifindex).is_some_and(|l| l.mac == *dst));
            if ours { PACKET_HOST } else { PACKET_OTHERHOST }
        };
        let mut addr = sockaddr_ll(ethertype, f.ifindex, 1, &src);
        addr[10] = pkttype;
        let data = if self.ty == super::SOCK_DGRAM { &f.data[14..] } else { &f.data[..] };
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok((n, data.len(), addr))
    }

    /// Send: a whole frame (SOCK_RAW) or a payload with the header built
    /// from `to` (SOCK_DGRAM), out of interface `to.ifindex` (or the
    /// bound one).
    pub fn send(&self, data: &[u8], to: Option<(u16, u32, [u8; 6])>) -> KResult<usize> {
        let (proto, bound) = {
            let i = self.inner.lock();
            (i.proto, i.ifindex)
        };
        let (proto, ifindex, dst) = match to {
            Some((p, idx, mac)) => (if p != 0 { u16::from_be(p) } else { proto }, if idx != 0 { idx } else { bound }, mac),
            None => (proto, bound, [0xff; 6]),
        };
        if ifindex == 0 {
            return Err(ENXIO);
        }
        let frame = if self.ty == super::SOCK_DGRAM {
            let mut f = Vec::with_capacity(14 + data.len());
            f.extend_from_slice(&dst);
            f.extend_from_slice(&[0; 6]); // the card's address, filled in below
            f.extend_from_slice(&proto.to_be_bytes());
            f.extend_from_slice(data);
            f
        } else {
            if data.len() < 14 {
                return Err(EINVAL);
            }
            data.to_vec()
        };
        if frame.len() > 1514 {
            return Err(EMSGSIZE);
        }
        super::inet::with_net(|n| n.dev.send_raw(ifindex, frame, self.ty == super::SOCK_DGRAM))?;
        Ok(data.len())
    }
}

fn sockaddr_ll(proto: u16, ifindex: u32, hatype: u16, addr: &[u8; 6]) -> Vec<u8> {
    let mut b = alloc::vec![0u8; SOCKADDR_LL_LEN];
    b[0..2].copy_from_slice(&AF_PACKET.to_le_bytes());
    b[2..4].copy_from_slice(&proto.to_be_bytes());
    b[4..8].copy_from_slice(&ifindex.to_le_bytes());
    b[8..10].copy_from_slice(&hatype.to_le_bytes());
    b[11] = 6;
    b[12..18].copy_from_slice(addr);
    b
}

/// A frame card `ifindex` received (called before the IP stack, under
/// the network lock: only this module's locks are taken).
pub fn deliver(ifindex: u32, frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let socks: Vec<Arc<PacketSocket>> = {
        let all = SOCKETS.lock();
        if all.is_empty() {
            return;
        }
        all.iter().filter_map(Weak::upgrade).collect()
    };
    for s in socks {
        let mut i = s.inner.lock();
        if (i.ifindex != 0 && i.ifindex != ifindex) || (i.proto != ETH_P_ALL && i.proto != ethertype) || i.proto == 0 {
            continue;
        }
        let keep = match &i.filter {
            Some(p) => run_filter(p, frame, ifindex) as usize,
            None => frame.len(),
        };
        if keep == 0 || i.queue.len() >= QUEUE_MAX {
            continue;
        }
        i.queue.push_back(Frame { data: frame[..keep.min(frame.len())].to_vec(), ifindex });
        drop(i);
        s.wq.wake(POLLIN);
    }
}

struct PacketFile(Arc<PacketSocket>);

impl FileOps for PacketFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        self.0.recv(buf, f.nonblock(), false).map(|(n, _, _)| n)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        self.0.send(data, None)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let ready = if self.0.inner.lock().queue.is_empty() { 0 } else { POLLIN | POLLRDNORM };
        ready | POLLOUT | POLLWRNORM
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    /// The interface ioctls (wpa_supplicant's l2_packet asks for the
    /// index and address of its interface on the packet socket).
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        super::ifreq::ioctl(cmd, arg)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn packet_of(f: &OpenFile) -> Option<Arc<PacketSocket>> {
    f.ops.as_any().downcast_ref::<PacketFile>().map(|p| p.0.clone())
}

/// Parse a sockaddr_ll: (protocol in network order, ifindex, address).
pub fn parse_sockaddr_ll(b: &[u8]) -> KResult<(u16, u32, [u8; 6])> {
    if b.len() < 12 || u16::from_le_bytes([b[0], b[1]]) != AF_PACKET {
        return Err(EINVAL);
    }
    let proto = u16::from_ne_bytes([b[2], b[3]]);
    let ifindex = u32::from_le_bytes(b[4..8].try_into().unwrap());
    let mut mac = [0u8; 6];
    if b.len() >= 18 {
        mac.copy_from_slice(&b[12..18]);
    }
    Ok((proto, ifindex, mac))
}
