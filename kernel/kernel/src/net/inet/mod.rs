//! IPv4/IPv6: the network stack behind AF_INET and AF_INET6 sockets.
//!
//! TCP, UDP and IP itself are smoltcp's. There is one smoltcp interface
//! with `Medium::Ip` that holds the addresses of every link; [`Dev`] sits
//! below it and does what smoltcp's single interface cannot:
//! - packets to a local address (127/8, ::1, any link's address) go
//!   straight back into the receive queue: that is `lo`;
//! - IPv4 packets to anywhere else leave through a card (virtio-net,
//!   `eth0`, `eth1`, ...) in an Ethernet frame: the card whose subnet holds
//!   the destination, else the default route's, with the next hop's MAC
//!   from a small per-card ARP cache (ARP requests and replies are handled
//!   here too);
//! - IPv6 packets likewise, with neighbour discovery instead of ARP and
//!   addresses and the default router from router advertisements
//!   (`ndisc`).
//!
//! The links, their addresses and the routing table are kernel state
//! here, changed through rtnetlink and the interface ioctls
//! (`net::rtnetlink`, `net::ifreq`); only the addresses and the default
//! route are mirrored into smoltcp. The last card starts up with QEMU
//! user-mode networking's addresses ([`configure_static`]): 10.0.2.15/24,
//! default route via 10.0.2.2.
//!
//! The stack runs when sockets use it (every send/connect/close polls it)
//! and from a soft interrupt: the timer tick ([`tick`]) notices received
//! frames or a due smoltcp timer and the next return to user mode or idle
//! loop ([`softirq`]) polls. After a poll that changed socket state, every
//! socket's wait queue is woken.

pub mod ndisc;
pub mod netfilter;
pub mod socket;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use smoltcp::iface::{Config, Interface, PollResult, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};
use spin::Mutex;

pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;

pub const IFF_UP: u32 = 0x1;
pub const IFF_BROADCAST: u32 = 0x2;
pub const IFF_LOOPBACK: u32 = 0x8;
pub const IFF_RUNNING: u32 = 0x40;
pub const IFF_MULTICAST: u32 = 0x1000;
pub const IFF_LOWER_UP: u32 = 0x10000;

pub const ARPHRD_ETHER: u16 = 1;
pub const ARPHRD_LOOPBACK: u16 = 772;

pub const RT_SCOPE_UNIVERSE: u8 = 0;
pub const RT_SCOPE_LINK: u8 = 253;
pub const RT_SCOPE_HOST: u8 = 254;
pub const RT_TABLE_MAIN: u32 = 254;
pub const RT_TABLE_LOCAL: u32 = 255;

/// A network card below the stack (virtio-net).
pub trait EthDevice: Send {
    fn mac(&self) -> [u8; 6];
    /// The next received Ethernet frame.
    fn recv(&mut self) -> Option<Vec<u8>>;
    /// Queue a frame; false when it was dropped.
    fn send(&mut self, frame: &[u8]) -> bool;
    /// Whether `send` has room for a frame now.
    fn can_send(&mut self) -> bool;
}

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_dropped: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IfAddr {
    pub addr: IpAddress,
    pub prefix: u8,
    pub scope: u8,
    pub flags: u32,
}

pub struct Link {
    pub index: u32,
    pub name: String,
    pub mac: [u8; 6],
    pub mtu: u32,
    pub flags: u32,
    pub arptype: u16,
    pub addrs: Vec<IfAddr>,
    pub txqlen: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Route {
    pub dst: IpAddress,
    pub dst_len: u8,
    pub gateway: Option<IpAddress>,
    pub oif: u32,
    pub table: u32,
    pub metric: u32,
    pub protocol: u8,
    pub scope: u8,
    pub ty: u8,
    pub prefsrc: Option<IpAddress>,
}

impl Route {
    pub fn family(&self) -> u16 {
        match self.dst {
            IpAddress::Ipv4(_) => AF_INET,
            IpAddress::Ipv6(_) => AF_INET6,
        }
    }
}

const LO: u32 = 1;

/// One network card.
struct Port {
    link: u32,
    eth: Box<dyn EthDevice>,
    mac: [u8; 6],
    /// Snapshot of the link's state (`Net::sync`).
    up: bool,
    v4: Vec<(Ipv4Address, u8)>,
    v6: Vec<(Ipv6Address, u8)>,
    stats: Stats,
    /// PCI address of the card ("0000:00:03.0", ethtool's bus_info).
    bus: String,
}

fn mask(p: u8) -> u32 {
    if p == 0 { 0 } else { u32::MAX << (32 - p as u32) }
}

fn on_subnet(v4: &[(Ipv4Address, u8)], a: Ipv4Address) -> bool {
    v4.iter().any(|&(ip, p)| u32::from_be_bytes(a.octets()) & mask(p) == u32::from_be_bytes(ip.octets()) & mask(p))
}

fn subnet_broadcast(v4: &[(Ipv4Address, u8)], a: Ipv4Address) -> bool {
    let a = u32::from_be_bytes(a.octets());
    v4.iter().any(|&(ip, p)| p < 31 && a & mask(p) == u32::from_be_bytes(ip.octets()) & mask(p) && a | mask(p) == u32::MAX)
}

/// The device under smoltcp: loopback plus the cards (see the module
/// docs).
pub struct Dev {
    rx: VecDeque<Vec<u8>>,
    ports: Vec<Port>,
    /// Snapshot of the configuration (`Net::sync`).
    local: Vec<IpAddress>,
    /// The main table's IPv4 default route: (link, gateway).
    gateway: Option<(u32, Ipv4Address)>,
    arp: BTreeMap<(u32, Ipv4Address), ([u8; 6], u64)>,
    arp_asked: BTreeMap<(u32, Ipv4Address), u64>,
    pending: VecDeque<(u32, Ipv4Address, Vec<u8>)>,
    /// IPv6 neighbours, like `arp` / `arp_asked` / `pending`.
    nd: BTreeMap<(u32, Ipv6Address), ([u8; 6], u64)>,
    nd_asked: BTreeMap<(u32, Ipv6Address), u64>,
    pending6: VecDeque<(u32, Ipv6Address, Vec<u8>)>,
    /// The main table's IPv6 default route: (link, router).
    gateway6: Option<(u32, Ipv6Address)>,
    /// Router advertisements for `Net::poll` to apply.
    ra: Vec<ndisc::RaEvent>,
    pub lo_stats: Stats,
    /// Packets smoltcp may still take in this `Interface::poll`.
    rx_budget: usize,
}

const ETH_HDR: usize = 14;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV6: u16 = 0x86dd;
const ARP_TTL_NS: u64 = 300_000_000_000;
const MAX_PENDING: usize = 64;

fn v4(b: &[u8]) -> Ipv4Address {
    Ipv4Address::new(b[0], b[1], b[2], b[3])
}

impl Dev {
    fn is_local(&self, a: IpAddress) -> bool {
        match a {
            IpAddress::Ipv4(v) => v.is_loopback() || self.local.contains(&a) || v.is_unspecified(),
            IpAddress::Ipv6(v) => v.is_loopback() || self.local.contains(&a) || v.is_unspecified(),
        }
    }

    fn port(&mut self, link: u32) -> Option<&mut Port> {
        self.ports.iter_mut().find(|p| p.link == link)
    }

    /// (port index, next hop) for an IPv4 destination off this machine.
    fn route4(&self, dst: Ipv4Address) -> Option<(usize, Ipv4Address)> {
        let usable = |p: &Port| p.up && !p.v4.is_empty();
        if let Some(i) = self.ports.iter().position(|p| usable(p) && on_subnet(&p.v4, dst)) {
            return Some((i, dst));
        }
        let (link, gw) = self.gateway?;
        let i = self.ports.iter().position(|p| p.link == link && usable(p))?;
        Some((i, gw))
    }

    /// An IP packet from smoltcp.
    fn output(&mut self, pkt: Vec<u8>) {
        let dst = match pkt.first().map(|b| b >> 4) {
            Some(4) if pkt.len() >= 20 => IpAddress::Ipv4(v4(&pkt[16..20])),
            Some(6) if pkt.len() >= 40 => IpAddress::Ipv6(Ipv6Address::from(<[u8; 16]>::try_from(&pkt[24..40]).unwrap())),
            _ => return,
        };
        if self.is_local(dst) {
            self.lo_stats.tx_packets += 1;
            self.lo_stats.tx_bytes += pkt.len() as u64;
            self.lo_stats.rx_packets += 1;
            self.lo_stats.rx_bytes += pkt.len() as u64;
            self.rx.push_back(pkt);
            return;
        }
        let dst = match dst {
            IpAddress::Ipv4(d) => d,
            IpAddress::Ipv6(d) => return self.output6(d, pkt),
        };
        if dst.is_multicast() || dst.is_broadcast() || self.ports.iter().any(|p| subnet_broadcast(&p.v4, dst)) {
            let o = dst.octets();
            let mac = if dst.is_multicast() { [0x01, 0, 0x5e, o[1] & 0x7f, o[2], o[3]] } else { [0xff; 6] };
            let Some(i) = self.ports.iter().position(|p| p.up && (dst.is_multicast() || dst.is_broadcast() || subnet_broadcast(&p.v4, dst)))
            else {
                return;
            };
            self.send_ip(i, mac, &pkt);
            return;
        }
        let Some((i, hop)) = self.route4(dst) else {
            return;
        };
        let link = self.ports[i].link;
        let now = crate::time::monotonic_ns();
        match self.arp.get(&(link, hop)) {
            Some(&(mac, exp)) if exp > now => self.send_ip(i, mac, &pkt),
            _ => {
                if self.pending.len() >= MAX_PENDING {
                    self.pending.pop_front();
                }
                self.pending.push_back((link, hop, pkt));
                let asked = self.arp_asked.get(&(link, hop)).copied().unwrap_or(0);
                if asked == 0 || now >= asked + 1_000_000_000 {
                    self.arp_asked.insert((link, hop), now);
                    if let Some(p) = self.arp_packet(i, 1, [0; 6], hop) {
                        self.send_frame(i, [0xff; 6], ETHERTYPE_ARP, &p);
                    }
                }
            }
        }
    }

    /// A frame from a packet socket out of the card that is link
    /// `ifindex` (`fill_src`: put the card's address in the header).
    pub fn send_raw(&mut self, ifindex: u32, mut frame: Vec<u8>, fill_src: bool) -> crate::errno::KResult<()> {
        let port = self.ports.iter_mut().find(|p| p.link == ifindex).ok_or(crate::errno::ENXIO)?;
        if !port.up {
            return Err(crate::errno::ENETDOWN);
        }
        if fill_src {
            frame[6..12].copy_from_slice(&port.mac);
        }
        if port.eth.send(&frame) {
            port.stats.tx_packets += 1;
            port.stats.tx_bytes += frame.len() as u64;
        } else {
            port.stats.tx_dropped += 1;
        }
        Ok(())
    }

    fn send_frame(&mut self, i: usize, dst: [u8; 6], ty: u16, payload: &[u8]) {

        let port = &mut self.ports[i];
        let mut f = Vec::with_capacity(ETH_HDR + payload.len());
        f.extend_from_slice(&dst);
        f.extend_from_slice(&port.mac);
        f.extend_from_slice(&ty.to_be_bytes());
        f.extend_from_slice(payload);
        if port.eth.send(&f) {
            port.stats.tx_packets += 1;
            port.stats.tx_bytes += f.len() as u64;
        } else {
            port.stats.tx_dropped += 1;
        }
    }

    /// An IPv4 packet out of card `i`, fragmented to its 1500-byte MTU
    /// (smoltcp sets DF on everything; TCP stays within the peer's MSS,
    /// so what is too big here is a datagram, which Linux fragments).
    fn send_ip(&mut self, i: usize, mac: [u8; 6], pkt: &[u8]) {
        const MTU: usize = 1500;
        let hl = ((pkt[0] & 0xf) as usize) * 4;
        if pkt.len() <= MTU || hl < 20 || pkt.len() < hl {
            return self.send_frame(i, mac, ETHERTYPE_IPV4, pkt);
        }
        let data = &pkt[hl..];
        let step = (MTU - hl) & !7;
        let mut off = 0;
        while off < data.len() {
            let n = step.min(data.len() - off);
            let more = off + n < data.len();
            let mut f = Vec::with_capacity(hl + n);
            f.extend_from_slice(&pkt[..hl]);
            f.extend_from_slice(&data[off..off + n]);
            f[2..4].copy_from_slice(&((hl + n) as u16).to_be_bytes());
            let frag = (off / 8) as u16 | if more { 0x2000 } else { 0 };
            f[6..8].copy_from_slice(&frag.to_be_bytes());
            f[10..12].fill(0);
            let mut sum: u32 = f[..hl].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as u32).sum();
            while sum >> 16 != 0 {
                sum = (sum & 0xffff) + (sum >> 16);
            }
            f[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
            self.send_frame(i, mac, ETHERTYPE_IPV4, &f);
            off += n;
        }
    }

    fn arp_packet(&self, i: usize, op: u16, tha: [u8; 6], tpa: Ipv4Address) -> Option<[u8; 28]> {
        let port = &self.ports[i];
        let spa = port.v4.iter().find(|&&(ip, p)| on_subnet(&[(ip, p)], tpa)).or(port.v4.first())?.0;
        let mut p = [0u8; 28];
        p[0..2].copy_from_slice(&1u16.to_be_bytes());
        p[2..4].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        p[4] = 6;
        p[5] = 4;
        p[6..8].copy_from_slice(&op.to_be_bytes());
        p[8..14].copy_from_slice(&port.mac);
        p[14..18].copy_from_slice(&spa.octets());
        p[18..24].copy_from_slice(&tha);
        p[24..28].copy_from_slice(&tpa.octets());
        Some(p)
    }

    /// Frames from the cards: IP packets go to the receive queue, ARP is
    /// answered and learned from here. Cards that are down drop them.
    fn pull_eth(&mut self) {
        for i in 0..self.ports.len() {
            let mut budget = 256;
            while budget > 0 {
                budget -= 1;
                let port = &mut self.ports[i];
                let Some(f) = port.eth.recv() else {
                    break;
                };
                if f.len() < ETH_HDR || !port.up {
                    continue;
                }
                let dst: [u8; 6] = f[0..6].try_into().unwrap();
                if dst != port.mac && dst[0] & 1 == 0 {
                    continue;
                }
                port.stats.rx_packets += 1;
                port.stats.rx_bytes += f.len() as u64;
                crate::net::packet::deliver(port.link, &f);
                match u16::from_be_bytes([f[12], f[13]]) {
                    ETHERTYPE_IPV4 => self.rx.push_back(f[ETH_HDR..].to_vec()),
                    ETHERTYPE_IPV6 => {
                        if !self.ndisc_input(i, &f[ETH_HDR..]) {
                            self.rx.push_back(f[ETH_HDR..].to_vec());
                        }
                    }
                    ETHERTYPE_ARP => self.arp_input(i, &f[ETH_HDR..]),
                    _ => {}
                }
            }
        }
    }

    fn arp_input(&mut self, i: usize, p: &[u8]) {
        if p.len() < 28 || p[4] != 6 || p[5] != 4 {
            return;
        }
        let op = u16::from_be_bytes([p[6], p[7]]);
        let sha: [u8; 6] = p[8..14].try_into().unwrap();
        let (spa, tpa) = (v4(&p[14..18]), v4(&p[24..28]));
        let link = self.ports[i].link;
        let ours = self.ports[i].v4.iter().any(|&(ip, _)| ip == tpa);
        if ours || self.arp.contains_key(&(link, spa)) {
            self.arp.insert((link, spa), (sha, crate::time::monotonic_ns() + ARP_TTL_NS));
            let (ready, rest): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|(l, h, _)| *l == link && *h == spa);
            self.pending = rest.into();
            for (_, _, pkt) in ready {
                self.send_ip(i, sha, &pkt);
            }
        }
        if op == 1
            && ours
            && let Some(r) = self.arp_packet(i, 2, sha, spa)
        {
            self.send_frame(i, sha, ETHERTYPE_ARP, &r);
        }
    }
}

pub struct DevRx(Vec<u8>);
pub struct DevTx<'a>(&'a mut Dev);

impl phy::RxToken for DevRx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for DevTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = alloc::vec![0u8; len];
        let r = f(&mut buf);
        self.0.output(buf);
        r
    }
}

impl Device for Dev {
    type RxToken<'a> = DevRx;
    type TxToken<'a> = DevTx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(DevRx, DevTx<'_>)> {
        if self.rx_budget == 0 {
            return None;
        }
        let p = self.rx.pop_front()?;
        self.rx_budget -= 1;
        Some((DevRx(p), DevTx(self)))
    }

    fn transmit(&mut self, _: Instant) -> Option<DevTx<'_>> {
        // Backpressure: with a card's ring full smoltcp keeps the data
        // and retries on the next poll, instead of a dropped frame and a
        // retransmission timeout.
        if self.ports.iter_mut().any(|p| p.up && !p.eth.can_send()) {
            return None;
        }
        Some(DevTx(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ip;
        // lo's MTU: loopback carries 64 KiB segments and datagrams; what
        // leaves through a card is fragmented to its MTU (`send_ip`).
        c.max_transmission_unit = 65535;
        c
    }
}

pub struct Net {
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    pub dev: Dev,
    pub links: Vec<Link>,
    pub routes: Vec<Route>,
    /// Policy routing rules (rtnetlink RTM_NEWRULE payloads), stored only.
    pub rules: Vec<(u16, Vec<u8>)>,
    pub sock: socket::Tables,
}

pub static NET: Mutex<Option<Net>> = Mutex::new(None);
static PENDING: AtomicBool = AtomicBool::new(false);
static NEXT_POLL: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn now() -> Instant {
    Instant::from_micros((crate::time::monotonic_ns() / 1000) as i64)
}

fn v4cidr(a: [u8; 4]) -> IpAddress {
    IpAddress::Ipv4(Ipv4Address::new(a[0], a[1], a[2], a[3]))
}

/// Create the stack with `lo` (before any card is probed).
pub fn init() {
    let mut dev = Dev {
        rx: VecDeque::new(),
        ports: Vec::new(),
        local: Vec::new(),
        gateway: None,
        arp: BTreeMap::new(),
        arp_asked: BTreeMap::new(),
        pending: VecDeque::new(),
        nd: BTreeMap::new(),
        nd_asked: BTreeMap::new(),
        pending6: VecDeque::new(),
        gateway6: None,
        ra: Vec::new(),
        lo_stats: Stats::default(),
        rx_budget: 0,
    };
    let mut cfg = Config::new(HardwareAddress::Ip);
    let mut seed = [0u8; 8];
    crate::random::fill(&mut seed);
    cfg.random_seed = u64::from_le_bytes(seed);
    let iface = Interface::new(cfg, &mut dev, now());
    let lo = Link {
        index: LO,
        name: String::from("lo"),
        mac: [0; 6],
        mtu: 65536,
        flags: IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_LOWER_UP,
        arptype: ARPHRD_LOOPBACK,
        addrs: alloc::vec![
            IfAddr { addr: v4cidr([127, 0, 0, 1]), prefix: 8, scope: RT_SCOPE_HOST, flags: 0x80 },
            IfAddr { addr: IpAddress::Ipv6(Ipv6Address::LOCALHOST), prefix: 128, scope: RT_SCOPE_HOST, flags: 0x80 },
        ],
        txqlen: 1000,
    };
    let mut n = Net {
        iface,
        sockets: SocketSet::new(Vec::new()),
        dev,
        links: alloc::vec![lo],
        routes: Vec::new(),
        rules: Vec::new(),
        sock: socket::Tables::default(),
    };
    n.sync();
    *NET.lock() = Some(n);
    crate::fs::sysfs::add_device("devices/virtual/net/lo", "net", "class/net", String::from("INTERFACE=lo\nIFINDEX=1\n"), false);
    sysfs_link_files("devices/virtual/net/lo", 1, [0; 6], 65536, ARPHRD_LOOPBACK);
}

fn sysfs_link_files(dir: &str, index: u32, mac: [u8; 6], mtu: u32, ty: u16) {
    use crate::fs::sysfs;
    use alloc::format;
    let m = format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    sysfs::set(dir, "ifindex", &format!("{index}\n"));
    sysfs::set(dir, "iflink", &format!("{index}\n"));
    sysfs::set(dir, "address", &m);
    sysfs::set(dir, "mtu", &format!("{mtu}\n"));
    sysfs::set(dir, "type", &format!("{ty}\n"));
    sysfs::set(dir, "operstate", if ty == ARPHRD_LOOPBACK { "unknown\n" } else { "up\n" });
    sysfs::set(dir, "carrier", "1\n");
    sysfs::set(dir, "flags", "0x1003\n");
    sysfs::set(dir, "tx_queue_len", "1000\n");
}

/// A card was probed: it becomes eth<N> (up, no address yet).
pub fn add_ethernet(dev: Box<dyn EthDevice>, sysdev: String) {
    let mac = dev.mac();
    let (index, name) = {
        let mut g = NET.lock();
        let Some(n) = g.as_mut() else {
            return;
        };
        let index = n.links.iter().map(|l| l.index).max().unwrap_or(LO) + 1;
        let name = alloc::format!("eth{}", n.dev.ports.len());
        let bus = sysdev.split('/').find(|c| c.starts_with("0000:")).unwrap_or("").into();
        n.dev.ports.push(Port { link: index, eth: dev, mac, up: false, v4: Vec::new(), v6: Vec::new(), stats: Stats::default(), bus });
        let ll = IpAddress::Ipv6(ndisc::link_local(mac));
        n.links.push(Link {
            index,
            name: name.clone(),
            mac,
            mtu: 1500,
            flags: IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST | IFF_LOWER_UP,
            arptype: ARPHRD_ETHER,
            addrs: alloc::vec![IfAddr { addr: ll, prefix: 64, scope: RT_SCOPE_LINK, flags: 0x80 }],
            txqlen: 1000,
        });
        n.routes.push(Route {
            dst: IpAddress::Ipv6(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0)),
            dst_len: 64,
            gateway: None,
            oif: index,
            table: RT_TABLE_MAIN,
            metric: 256,
            protocol: 2,
            scope: RT_SCOPE_UNIVERSE,
            ty: 1,
            prefsrc: None,
        });
        n.sync();
        (index, name)
    };
    let dir = alloc::format!("{sysdev}/net/{name}");
    crate::fs::sysfs::add_device(&dir, "net", "class/net", alloc::format!("INTERFACE={name}\nIFINDEX={index}\n"), false);
    sysfs_link_files(&dir, index, mac, 1500, ARPHRD_ETHER);
}

/// After probing: the last card gets QEMU user-mode networking's static
/// configuration (10.0.2.15/24, default route via 10.0.2.2), like Linux's
/// `ip=` autoconfiguration (which also leaves the card up: Cuttlefish's
/// vendor init then fails to rename eth0 to buried_eth0, as renaming an
/// up link is EBUSY, and eth0 stays usable for adb).
pub fn configure_static() {
    let mut g = NET.lock();
    let Some(n) = g.as_mut() else {
        return;
    };
    let Some(index) = n.dev.ports.last().map(|p| p.link) else {
        return;
    };
    let me = v4cidr([10, 0, 2, 15]);
    n.link_mut(index).unwrap().addrs.push(IfAddr { addr: me, prefix: 24, scope: RT_SCOPE_UNIVERSE, flags: 0x80 });
    n.routes.push(Route {
        dst: v4cidr([10, 0, 2, 0]),
        dst_len: 24,
        gateway: None,
        oif: index,
        table: RT_TABLE_MAIN,
        metric: 0,
        protocol: 2,
        scope: RT_SCOPE_LINK,
        ty: 1,
        prefsrc: Some(me),
    });
    n.routes.push(Route {
        dst: v4cidr([0, 0, 0, 0]),
        dst_len: 0,
        gateway: Some(v4cidr([10, 0, 2, 2])),
        oif: index,
        table: RT_TABLE_MAIN,
        metric: 0,
        protocol: 4,
        scope: RT_SCOPE_UNIVERSE,
        ty: 1,
        prefsrc: None,
    });
    n.sync();
    // IPv6 addresses and router come from the router advertisement.
    let i = n.dev.ports.len() - 1;
    n.dev.router_solicit(i);
    log::info!("net: {} is 10.0.2.15/24, default route via 10.0.2.2", n.link(index).unwrap().name);
}

impl Net {
    pub fn link(&self, index: u32) -> Option<&Link> {
        self.links.iter().find(|l| l.index == index)
    }

    pub fn link_mut(&mut self, index: u32) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| l.index == index)
    }

    pub fn link_by_name(&self, name: &str) -> Option<&Link> {
        self.links.iter().find(|l| l.name == name)
    }

    /// The PCI address of a card's link (None for `lo`).
    pub fn bus_info(&self, index: u32) -> Option<&str> {
        self.dev.ports.iter().find(|p| p.link == index).map(|p| p.bus.as_str())
    }

    pub fn stats(&self, index: u32) -> Stats {
        if index == LO {
            return self.dev.lo_stats;
        }
        self.dev.ports.iter().find(|p| p.link == index).map_or(Stats::default(), |p| p.stats)
    }

    /// Mirror the links' addresses and the default route into smoltcp
    /// and the device after a configuration change.
    pub fn sync(&mut self) {
        let mut cidrs: Vec<IpCidr> = Vec::new();
        let mut local = Vec::new();
        for l in &self.links {
            let v4s: Vec<(Ipv4Address, u8)> = l
                .addrs
                .iter()
                .filter_map(|a| match a.addr {
                    IpAddress::Ipv4(v) => Some((v, a.prefix)),
                    _ => None,
                })
                .collect();
            let v6s: Vec<(Ipv6Address, u8)> = l
                .addrs
                .iter()
                .filter_map(|a| match a.addr {
                    IpAddress::Ipv6(v) => Some((v, a.prefix)),
                    _ => None,
                })
                .collect();
            for a in &l.addrs {
                local.push(a.addr);
                if l.flags & IFF_UP != 0 {
                    cidrs.push(IpCidr::new(a.addr, a.prefix));
                }
            }
            if let Some(p) = self.dev.port(l.index) {
                p.up = l.flags & IFF_UP != 0;
                p.v4 = v4s;
                p.v6 = v6s;
            }
        }
        // IPv4 first, cards before lo: smoltcp takes the first IPv4
        // address as the source of what it sends on its own (ICMP).
        cidrs.sort_by_key(|c| match c {
            IpCidr::Ipv4(v) if v.address().is_loopback() => 1,
            IpCidr::Ipv4(_) => 0,
            IpCidr::Ipv6(_) => 2,
        });
        self.iface.update_ip_addrs(|v| {
            v.clear();
            for c in cidrs.iter().take(8) {
                let _ = v.push(*c);
            }
        });
        self.dev.local = local;
        let gw = self
            .routes
            .iter()
            .filter(|r| r.dst_len == 0 && r.table == RT_TABLE_MAIN && r.family() == AF_INET)
            .find_map(|r| match r.gateway {
                Some(IpAddress::Ipv4(g)) => Some((r.oif, g)),
                _ => None,
            });
        self.dev.gateway = gw;
        let gw6 = self
            .routes
            .iter()
            .filter(|r| r.dst_len == 0 && r.table == RT_TABLE_MAIN && r.family() == AF_INET6)
            .find_map(|r| match r.gateway {
                Some(IpAddress::Ipv6(g)) => Some((r.oif, g)),
                _ => None,
            });
        self.dev.gateway6 = gw6;
        let gw = gw.map(|(_, g)| g);
        let routes = self.iface.routes_mut();
        routes.remove_default_ipv4_route();
        if let Some(g) = gw {
            let _ = routes.add_default_ipv4_route(g);
        }
        routes.remove_default_ipv6_route();
        if let Some((_, g)) = gw6 {
            let _ = routes.add_default_ipv6_route(g);
        }
    }

    /// Router advertisements: SLAAC addresses, their prefixes on link, the
    /// sender as default router (removed again by a zero lifetime).
    fn apply_ra(&mut self) {
        let events = core::mem::take(&mut self.dev.ra);
        for e in events {
            let Some(l) = self.link_mut(e.link) else { continue };
            for &a in &e.addrs {
                if !l.addrs.iter().any(|x| x.addr == IpAddress::Ipv6(a)) {
                    l.addrs.push(IfAddr { addr: IpAddress::Ipv6(a), prefix: 64, scope: RT_SCOPE_UNIVERSE, flags: 0 });
                    log::info!("net: {} got {a}/64 (SLAAC)", l.name);
                }
            }
            for &a in &e.addrs {
                let mut o = a.octets();
                o[8..].fill(0);
                let dst = IpAddress::Ipv6(Ipv6Address::from(o));
                if !self.routes.iter().any(|r| r.dst == dst && r.dst_len == 64 && r.oif == e.link) {
                    self.routes.push(Route {
                        dst,
                        dst_len: 64,
                        gateway: None,
                        oif: e.link,
                        table: RT_TABLE_MAIN,
                        metric: 256,
                        protocol: 9,
                        scope: RT_SCOPE_UNIVERSE,
                        ty: 1,
                        prefsrc: None,
                    });
                }
            }
            let is_default = |r: &Route| r.family() == AF_INET6 && r.dst_len == 0 && r.oif == e.link && r.protocol == 9;
            self.routes.retain(|r| !is_default(r));
            if e.lifetime > 0 {
                self.routes.push(Route {
                    dst: IpAddress::Ipv6(Ipv6Address::UNSPECIFIED),
                    dst_len: 0,
                    gateway: Some(IpAddress::Ipv6(e.router)),
                    oif: e.link,
                    table: RT_TABLE_MAIN,
                    metric: 1024,
                    protocol: 9,
                    scope: RT_SCOPE_UNIVERSE,
                    ty: 1,
                    prefsrc: None,
                });
            }
        }
        self.sync();
    }

    /// The source address for packets to `dst` from a socket not bound to
    /// one (smoltcp would take the first IPv4 address for everything): the
    /// destination itself if local, 127.0.0.1 for loopback, else the
    /// address of the link the destination is on or the default route's.
    pub fn source_for(&self, dst: IpAddress) -> Option<IpAddress> {
        let IpAddress::Ipv4(d) = dst else {
            return None;
        };
        if d.is_loopback() {
            return Some(IpAddress::Ipv4(Ipv4Address::LOCALHOST));
        }
        if self.dev.local.contains(&dst) {
            return Some(dst);
        }
        let port = match self.dev.route4(d) {
            Some((i, _)) => &self.dev.ports[i],
            None => self.dev.ports.iter().find(|p| !p.v4.is_empty())?,
        };
        let a = port.v4.iter().find(|&&(ip, p)| on_subnet(&[(ip, p)], d)).or(port.v4.first())?;
        Some(IpAddress::Ipv4(a.0))
    }

    /// Run the stack until it is idle; true if socket state may have
    /// changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        // A few packets at a time, topping up the listening pools in
        // between: each packet takes at most one listening socket.
        for _ in 0..256 {
            self.dev.pull_eth();
            self.dev.rx_budget = socket::LISTEN_POOL;
            if self.iface.poll(now(), &mut self.dev, &mut self.sockets) == PollResult::SocketStateChanged {
                changed = true;
            }
            if socket::after_poll(self) {
                changed = true;
            }
            if self.dev.rx.is_empty() && self.dev.rx_budget > 0 {
                break;
            }
            changed = true;
        }
        if !self.dev.rx.is_empty() {
            PENDING.store(true, Ordering::Relaxed);
        }
        if !self.dev.ra.is_empty() {
            self.apply_ra();
            changed = true;
        }
        let next = self.iface.poll_at(now(), &self.sockets).map_or(u64::MAX, |t| t.total_micros().max(0) as u64 * 1000);
        NEXT_POLL.store(next, Ordering::Relaxed);
        changed
    }
}

/// Run `f` on the stack, then poll it and wake sockets if needed.
pub fn with_net<R>(f: impl FnOnce(&mut Net) -> R) -> R {
    let (r, changed) = {
        let mut g = NET.lock();
        let n = g.as_mut().expect("network stack not initialised");
        let r = f(n);
        (r, n.poll())
    };
    if changed {
        socket::wake_all();
    }
    r
}

/// Run `f` on the stack without polling.
pub fn with_net_nopoll<R>(f: impl FnOnce(&mut Net) -> R) -> R {
    let mut g = NET.lock();
    f(g.as_mut().expect("network stack not initialised"))
}

/// From the timer interrupt (BSP): request a poll when frames arrived or
/// a smoltcp timer is due.
pub fn tick() {
    if crate::drivers::virtio::net::rx_hint() || crate::time::monotonic_ns() >= NEXT_POLL.load(Ordering::Relaxed) {
        PENDING.store(true, Ordering::Relaxed);
    }
}

/// Whether a deferred poll is due.
pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// The deferred poll (task context, no locks held: return to user mode
/// and the idle loop).
pub fn softirq() {
    if !PENDING.load(Ordering::Relaxed) {
        return;
    }
    let changed = {
        let Some(mut g) = NET.try_lock() else {
            return;
        };
        PENDING.store(false, Ordering::Relaxed);
        match g.as_mut() {
            Some(n) => n.poll(),
            None => false,
        }
    };
    if changed {
        socket::wake_all();
    }
}

/// The address families' sockaddr layouts.
pub fn sockaddr_in(ep: (IpAddress, u16), family: u16) -> Vec<u8> {
    let (addr, port) = ep;
    if family == AF_INET6 {
        let mut b = alloc::vec![0u8; 28];
        b[0..2].copy_from_slice(&AF_INET6.to_le_bytes());
        b[2..4].copy_from_slice(&port.to_be_bytes());
        let o = match addr {
            IpAddress::Ipv6(a) => a.octets(),
            IpAddress::Ipv4(a) if a.is_unspecified() => [0; 16],
            IpAddress::Ipv4(a) => a.to_ipv6_mapped().octets(),
        };
        b[8..24].copy_from_slice(&o);
        b
    } else {
        let mut b = alloc::vec![0u8; 16];
        b[0..2].copy_from_slice(&AF_INET.to_le_bytes());
        b[2..4].copy_from_slice(&port.to_be_bytes());
        if let IpAddress::Ipv4(a) = addr {
            b[4..8].copy_from_slice(&a.octets());
        }
        b
    }
}

/// Parse a sockaddr_in / sockaddr_in6 (v4-mapped IPv6 becomes IPv4).
/// Returns (family, address, port).
pub fn parse_sockaddr(b: &[u8]) -> crate::errno::KResult<(u16, IpAddress, u16)> {
    use crate::errno::*;
    if b.len() < 2 {
        return Err(EINVAL);
    }
    let fam = u16::from_le_bytes([b[0], b[1]]);
    match fam {
        AF_INET if b.len() >= 16 => Ok((AF_INET, IpAddress::Ipv4(v4(&b[4..8])), u16::from_be_bytes([b[2], b[3]]))),
        AF_INET6 if b.len() >= 24 => {
            let a = Ipv6Address::from(<[u8; 16]>::try_from(&b[8..24]).unwrap());
            let addr = match a.to_ipv4_mapped() {
                Some(v) => IpAddress::Ipv4(v),
                None => IpAddress::Ipv6(a),
            };
            Ok((AF_INET6, addr, u16::from_be_bytes([b[2], b[3]])))
        }
        AF_INET | AF_INET6 => Err(EINVAL),
        _ => Err(EAFNOSUPPORT),
    }
}

pub fn addr_bytes(a: &IpAddress) -> Vec<u8> {
    match a {
        IpAddress::Ipv4(v) => v.octets().to_vec(),
        IpAddress::Ipv6(v) => v.octets().to_vec(),
    }
}

/// Files in /proc/net served by [`proc_net_file`].
pub const NET_FILES: &[&str] = &[
    "arp",
    "dev",
    "dev_mcast",
    "icmp",
    "icmp6",
    "if_inet6",
    "igmp",
    "igmp6",
    "ip6_tables_names",
    "ip_tables_matches",
    "ip_tables_names",
    "ip_tables_targets",
    "ipv6_route",
    "netstat",
    "packet",
    "protocols",
    "raw",
    "raw6",
    "route",
    "snmp",
    "snmp6",
    "sockstat",
    "sockstat6",
    "tcp",
    "tcp6",
    "udp",
    "udp6",
];

pub fn proc_net_file(name: &str) -> String {
    use alloc::format;
    match name {
        "tcp" => socket::proc_net(true, false),
        "tcp6" => socket::proc_net(true, true),
        "udp" => socket::proc_net(false, false),
        "udp6" => socket::proc_net(false, true),
        "ip_tables_names" => netfilter::table_names(false),
        "ip6_tables_names" => netfilter::table_names(true),
        "ip_tables_matches" | "ip_tables_targets" => String::new(),
        "arp" => with_net_nopoll(|n| {
            let mut s = String::from("IP address       HW type     Flags       HW address            Mask     Device\n");
            for ((link, ip), (m, _)) in &n.dev.arp {
                s.push_str(&format!(
                    "{:<16} 0x1         0x2         {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}     *        {}\n",
                    alloc::string::ToString::to_string(ip),
                    m[0],
                    m[1],
                    m[2],
                    m[3],
                    m[4],
                    m[5],
                    n.link(*link).map_or("?", |l| l.name.as_str())
                ));
            }
            s
        }),
        "dev" => with_net_nopoll(|n| {
            let mut s = String::from(
                "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
            );
            for l in &n.links {
                let st = n.stats(l.index);
                s.push_str(&format!(
                    "{:>6}: {:>7} {:>7}    0    0    0     0          0         0 {:>8} {:>7}    0 {:>4}    0     0       0          0\n",
                    l.name, st.rx_bytes, st.rx_packets, st.tx_bytes, st.tx_packets, st.tx_dropped
                ));
            }
            s
        }),
        "if_inet6" => with_net_nopoll(|n| {
            let mut s = String::new();
            for l in &n.links {
                for a in &l.addrs {
                    if let IpAddress::Ipv6(v) = a.addr {
                        let hex: String = v.octets().iter().map(|b| format!("{b:02x}")).collect();
                        s.push_str(&format!("{hex} {:02x} {:02x} {:02x} {:02x} {:>8}\n", l.index, a.prefix, a.scope, a.flags, l.name));
                    }
                }
            }
            s
        }),
        "route" => with_net_nopoll(|n| {
            let mut s = String::from("Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
            for r in n.routes.iter().filter(|r| r.table == RT_TABLE_MAIN && r.family() == AF_INET) {
                let le = |a: &IpAddress| match a {
                    IpAddress::Ipv4(v) => u32::from_le_bytes(v.octets()),
                    _ => 0,
                };
                let mask = if r.dst_len == 0 { 0 } else { (u32::MAX << (32 - r.dst_len as u32)).swap_bytes() };
                let flags = 1 | if r.gateway.is_some() { 2 } else { 0 };
                let name = n.link(r.oif).map_or("*", |l| l.name.as_str());
                s.push_str(&format!(
                    "{name}\t{:08X}\t{:08X}\t{flags:04X}\t0\t0\t{}\t{mask:08X}\t0\t0\t0\n",
                    le(&r.dst),
                    r.gateway.as_ref().map_or(0, le),
                    r.metric
                ));
            }
            s
        }),
        "ipv6_route" => with_net_nopoll(|n| {
            let hex = |a: &IpAddress| -> String {
                match a {
                    IpAddress::Ipv6(v) => v.octets().iter().map(|b| format!("{b:02x}")).collect(),
                    _ => "0".repeat(32),
                }
            };
            let mut s = String::new();
            for r in n.routes.iter().filter(|r| r.family() == AF_INET6) {
                // RTF_UP | RTF_GATEWAY
                let flags = 1 | if r.gateway.is_some() { 2 } else { 0 };
                let name = n.link(r.oif).map_or("lo", |l| l.name.as_str());
                s.push_str(&format!(
                    "{} {:02x} {} 00 {} {:08x} 00000001 00000000 {flags:08x} {name:>8}\n",
                    hex(&r.dst),
                    r.dst_len,
                    "0".repeat(32),
                    r.gateway.as_ref().map_or_else(|| "0".repeat(32), hex),
                    r.metric
                ));
            }
            s
        }),
        "dev_mcast" => with_net_nopoll(|n| {
            let mut s = String::new();
            for l in n.links.iter().filter(|l| l.arptype == ARPHRD_ETHER) {
                s.push_str(&format!("{:<5}{:<15}1     0     01005e000001\n", l.index, l.name));
                s.push_str(&format!("{:<5}{:<15}1     0     333300000001\n", l.index, l.name));
            }
            s
        }),
        "igmp" => with_net_nopoll(|n| {
            let mut s = String::from("Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n");
            for l in &n.links {
                s.push_str(&format!("{}\t{:<10}:     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n", l.index, l.name));
            }
            s
        }),
        "igmp6" => with_net_nopoll(|n| {
            let mut s = String::new();
            for l in &n.links {
                s.push_str(&format!("{:<4} {:<15} ff020000000000000000000000000001     1 0000000C 0\n", l.index, l.name));
            }
            s
        }),
        "raw" | "icmp" => String::from("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n"),
        "raw6" | "icmp6" => String::from(
            "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n",
        ),
        "packet" => String::from("sk               RefCnt Type Proto  Iface R Rmem   User   Inode\n"),
        "protocols" => String::from(
            "protocol  size sockets  memory press maxhdr  slab module     cl co di ac io in de sh ss gs se re sp bi br ha uh gp em\n\
             PINGv6    1176      0      -1   NI       0   yes  kernel      y  y  y  n  n  y  n  n  y  y  y  y  n  y  y  y  y  y  n\n\
             UDPv6     1344      0      -1   NI       0   yes  kernel      y  y  y  n  y  y  y  n  y  y  y  y  n  n  n  y  y  y  n\n\
             TCPv6     2432      0      -1   no     320   yes  kernel      y  y  y  y  y  y  y  y  y  y  y  y  y  n  y  y  y  y  y\n\
             UNIX      1024      0      -1   NI       0   yes  kernel      n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n  n\n\
             PING       976      0      -1   NI       0   yes  kernel      y  y  y  n  n  y  n  n  y  y  y  y  n  y  y  y  y  y  n\n\
             UDP       1152      0      -1   NI       0   yes  kernel      y  y  y  n  y  y  y  n  y  y  y  y  y  n  n  y  y  y  n\n\
             TCP       2272      0      -1   no     320   yes  kernel      y  y  y  y  y  y  y  y  y  y  y  y  y  n  y  y  y  y  y\n",
        ),
        "snmp" => with_net_nopoll(|n| {
            let (mut rx, mut tx) = (0, 0);
            for l in &n.links {
                let st = n.stats(l.index);
                rx += st.rx_packets;
                tx += st.tx_packets;
            }
            format!(
                "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates\n\
                 Ip: 2 64 {rx} 0 0 0 0 0 {rx} {tx} 0 0 0 0 0 0 0 0 0\n\
                 Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs OutErrors OutRateLimitGlobal OutRateLimitHost OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps\n\
                 Icmp: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
                 Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n\
                 Tcp: 1 200 120000 -1 0 0 0 0 0 0 0 0 0 0 0\n\
                 Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\n\
                 Udp: 0 0 0 0 0 0 0 0 0\n\
                 UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\n\
                 UdpLite: 0 0 0 0 0 0 0 0 0\n"
            )
        }),
        "snmp6" => String::from(
            "Ip6InReceives                   \t0\nIp6OutRequests                  \t0\nIcmp6InMsgs                     \t0\nIcmp6OutMsgs                    \t0\nUdp6InDatagrams                 \t0\nUdp6OutDatagrams                \t0\n",
        ),
        "netstat" => String::from(
            "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled TW ListenOverflows ListenDrops\n\
             TcpExt: 0 0 0 0 0 0 0 0\n\
             IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets OutOctets\n\
             IpExt: 0 0 0 0 0 0 0 0\n",
        ),
        "sockstat" => {
            let (tcp, udp) = socket::counts(false);
            format!("sockets: used {}\nTCP: inuse {tcp} orphan 0 tw 0 alloc {tcp} mem 0\nUDP: inuse {udp} mem 0\nUDPLITE: inuse 0\nRAW: inuse 0\nFRAG: inuse 0 memory 0\n", tcp + udp)
        }
        "sockstat6" => {
            let (tcp, udp) = socket::counts(true);
            format!("TCP6: inuse {tcp}\nUDP6: inuse {udp}\nUDPLITE6: inuse 0\nRAW6: inuse 0\nFRAG6: inuse 0 memory 0\n")
        }
        _ => String::new(),
    }
}
