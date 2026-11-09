//! IPv6 on the cards: neighbour discovery and stateless autoconfiguration
//! (the part of IPv6 smoltcp's `Medium::Ip` interface leaves out).
//!
//! Every card gets a link-local address (fe80::/64 + EUI-64 of its MAC)
//! when it is probed and sends a router solicitation once it is
//! configured. Neighbour solicitations for our addresses are answered,
//! advertisements fill a per-card neighbour cache, and packets waiting
//! for a neighbour are queued meanwhile, as for ARP. A router
//! advertisement makes its sender the default router and, for each
//! autonomous /64 prefix, adds prefix + EUI-64 as an address
//! ([`RaEvent`], applied by `Net::poll`). QEMU's user network answers with
//! fec0::/64 and router fe80::2.

use alloc::vec::Vec;
use smoltcp::wire::Ipv6Address;

use super::{Dev, ETHERTYPE_IPV6, MAX_PENDING};

const ICMPV6: u8 = 58;
const RS: u8 = 133;
const RA: u8 = 134;
const NS: u8 = 135;
const NA: u8 = 136;
const OPT_SOURCE_LL: u8 = 1;
const OPT_TARGET_LL: u8 = 2;
const OPT_PREFIX: u8 = 3;
const ND_TTL_NS: u64 = 300_000_000_000;

/// What a router advertisement asked for.
pub struct RaEvent {
    pub link: u32,
    pub router: Ipv6Address,
    pub lifetime: u16,
    /// Autonomous /64 prefixes: the address to add.
    pub addrs: Vec<Ipv6Address>,
}

pub fn is_link_local(a: &Ipv6Address) -> bool {
    let o = a.octets();
    o[0] == 0xfe && o[1] & 0xc0 == 0x80
}

pub fn is_multicast(a: &Ipv6Address) -> bool {
    a.octets()[0] == 0xff
}

/// prefix[0..8] + modified EUI-64 of `mac`.
pub fn eui64(prefix: [u8; 8], mac: [u8; 6]) -> Ipv6Address {
    let mut o = [0u8; 16];
    o[..8].copy_from_slice(&prefix);
    o[8..11].copy_from_slice(&[mac[0] ^ 2, mac[1], mac[2]]);
    o[11..13].copy_from_slice(&[0xff, 0xfe]);
    o[13..16].copy_from_slice(&mac[3..6]);
    Ipv6Address::from(o)
}

pub fn link_local(mac: [u8; 6]) -> Ipv6Address {
    eui64([0xfe, 0x80, 0, 0, 0, 0, 0, 0], mac)
}

fn multicast_mac(a: &Ipv6Address) -> [u8; 6] {
    let o = a.octets();
    [0x33, 0x33, o[12], o[13], o[14], o[15]]
}

fn solicited_node(a: &Ipv6Address) -> Ipv6Address {
    let o = a.octets();
    Ipv6Address::from([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff, o[13], o[14], o[15]])
}

fn v6(b: &[u8]) -> Ipv6Address {
    Ipv6Address::from(<[u8; 16]>::try_from(&b[..16]).unwrap())
}

/// An IPv6 packet carrying an ICMPv6 message (checksum filled in).
fn icmp_packet(src: Ipv6Address, dst: Ipv6Address, msg: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(40 + msg.len());
    p.extend_from_slice(&[0x60, 0, 0, 0]);
    p.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    p.extend_from_slice(&[ICMPV6, 255]);
    p.extend_from_slice(&src.octets());
    p.extend_from_slice(&dst.octets());
    let start = p.len();
    p.extend_from_slice(msg);
    let mut sum: u32 = 0;
    let mut add = |b: &[u8]| {
        for c in b.chunks(2) {
            sum += u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]) as u32;
        }
    };
    add(&src.octets());
    add(&dst.octets());
    add(&(msg.len() as u32).to_be_bytes());
    add(&[0, 0, 0, ICMPV6]);
    add(msg);
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let ck = !(sum as u16);
    p[start + 2..start + 4].copy_from_slice(&ck.to_be_bytes());
    p
}

impl Dev {
    /// The card's link-local address (or any IPv6 address it has).
    fn source6(&self, i: usize) -> Option<Ipv6Address> {
        let p = &self.ports[i];
        p.v6.iter().map(|a| a.0).find(is_link_local).or(p.v6.first().map(|a| a.0))
    }

    fn send6(&mut self, i: usize, mac: [u8; 6], pkt: &[u8]) {
        self.send_frame(i, mac, ETHERTYPE_IPV6, pkt);
    }

    /// Ask the routers on card `i` to advertise themselves.
    pub(super) fn router_solicit(&mut self, i: usize) {
        let Some(src) = self.source6(i) else { return };
        let mut msg = alloc::vec![RS, 0, 0, 0, 0, 0, 0, 0, OPT_SOURCE_LL, 1];
        msg.extend_from_slice(&self.ports[i].mac);
        let all_routers = Ipv6Address::from([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        let p = icmp_packet(src, all_routers, &msg);
        self.send6(i, multicast_mac(&all_routers), &p);
    }

    fn solicit(&mut self, i: usize, target: Ipv6Address) {
        let Some(src) = self.source6(i) else { return };
        let mut msg = alloc::vec![NS, 0, 0, 0, 0, 0, 0, 0];
        msg.extend_from_slice(&target.octets());
        msg.extend_from_slice(&[OPT_SOURCE_LL, 1]);
        msg.extend_from_slice(&self.ports[i].mac);
        let dst = solicited_node(&target);
        let p = icmp_packet(src, dst, &msg);
        self.send6(i, multicast_mac(&dst), &p);
    }

    /// (port index, next hop) for an IPv6 destination off this machine.
    fn route6(&self, dst: &Ipv6Address) -> Option<(usize, Ipv6Address)> {
        let usable = |p: &super::Port| p.up && !p.v6.is_empty();
        if is_link_local(dst) {
            return self.ports.iter().position(usable).map(|i| (i, *dst));
        }
        let on_link = |p: &super::Port| {
            p.v6.iter().any(|&(a, len)| {
                let (x, y) = (u128::from_be_bytes(a.octets()), u128::from_be_bytes(dst.octets()));
                !is_link_local(&a) && len > 0 && (x ^ y) >> (128 - len.min(128) as u32) == 0
            })
        };
        if let Some(i) = self.ports.iter().position(|p| usable(p) && on_link(p)) {
            return Some((i, *dst));
        }
        let (link, gw) = self.gateway6?;
        let i = self.ports.iter().position(|p| p.link == link && usable(p))?;
        Some((i, gw))
    }

    /// An IPv6 packet from smoltcp for another machine.
    pub(super) fn output6(&mut self, dst: Ipv6Address, pkt: Vec<u8>) {
        if is_multicast(&dst) {
            if let Some(i) = self.ports.iter().position(|p| p.up && !p.v6.is_empty()) {
                self.send6(i, multicast_mac(&dst), &pkt);
            }
            return;
        }
        let Some((i, hop)) = self.route6(&dst) else { return };
        let link = self.ports[i].link;
        let now = crate::time::monotonic_ns();
        match self.nd.get(&(link, hop)) {
            Some(&(mac, exp)) if exp > now => self.send6(i, mac, &pkt),
            _ => {
                if self.pending6.len() >= MAX_PENDING {
                    self.pending6.pop_front();
                }
                self.pending6.push_back((link, hop, pkt));
                let asked = self.nd_asked.get(&(link, hop)).copied().unwrap_or(0);
                if asked == 0 || now >= asked + 1_000_000_000 {
                    self.nd_asked.insert((link, hop), now);
                    self.solicit(i, hop);
                }
            }
        }
    }

    fn learn6(&mut self, i: usize, ip: Ipv6Address, mac: [u8; 6]) {
        let link = self.ports[i].link;
        self.nd.insert((link, ip), (mac, crate::time::monotonic_ns() + ND_TTL_NS));
        let (ready, rest): (Vec<_>, Vec<_>) = self.pending6.drain(..).partition(|(l, h, _)| *l == link && *h == ip);
        self.pending6 = rest.into();
        for (_, _, pkt) in ready {
            self.send6(i, mac, &pkt);
        }
    }

    /// Neighbour discovery messages from card `i` (`p`: the IPv6
    /// packet). True if the packet was one (and is consumed here).
    pub(super) fn ndisc_input(&mut self, i: usize, p: &[u8]) -> bool {
        if p.len() < 48 || p[6] != ICMPV6 || !(RS..=NA).contains(&p[40]) {
            return false;
        }
        let (src, msg) = (v6(&p[8..24]), &p[40..]);
        let lladdr = |opts: &[u8], want: u8| -> Option<[u8; 6]> {
            let mut o = opts;
            while o.len() >= 8 && o[1] != 0 {
                let len = o[1] as usize * 8;
                if o[0] == want && len >= 8 {
                    return o[2..8].try_into().ok();
                }
                o = o.get(len..)?;
            }
            None
        };
        match msg[0] {
            NS if msg.len() >= 24 => {
                let target = v6(&msg[8..24]);
                if !self.ports[i].v6.iter().any(|a| a.0 == target) {
                    return true;
                }
                let their = lladdr(&msg[24..], OPT_SOURCE_LL);
                if let Some(m) = their
                    && !src.is_unspecified()
                {
                    self.learn6(i, src, m);
                }
                let mut na = alloc::vec![NA, 0, 0, 0, if src.is_unspecified() { 0x20 } else { 0x60 }, 0, 0, 0];
                na.extend_from_slice(&target.octets());
                na.extend_from_slice(&[OPT_TARGET_LL, 1]);
                na.extend_from_slice(&self.ports[i].mac);
                let all_nodes = Ipv6Address::from([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
                let (dst, mac) = match their {
                    Some(m) if !src.is_unspecified() => (src, m),
                    _ => (all_nodes, multicast_mac(&all_nodes)),
                };
                let pkt = icmp_packet(target, dst, &na);
                self.send6(i, mac, &pkt);
            }
            NA if msg.len() >= 24 => {
                let target = v6(&msg[8..24]);
                if let Some(m) = lladdr(&msg[24..], OPT_TARGET_LL) {
                    self.learn6(i, target, m);
                }
            }
            RA if msg.len() >= 16 => {
                let lifetime = u16::from_be_bytes([msg[6], msg[7]]);
                if let Some(m) = lladdr(&msg[16..], OPT_SOURCE_LL) {
                    self.learn6(i, src, m);
                }
                let mut addrs = Vec::new();
                let mut o = &msg[16..];
                while o.len() >= 8 && o[1] != 0 {
                    let len = o[1] as usize * 8;
                    if o[0] == OPT_PREFIX && len == 32 && o.len() >= 32 && o[2] == 64 && o[3] & 0x40 != 0 {
                        addrs.push(eui64(o[16..24].try_into().unwrap(), self.ports[i].mac));
                    }
                    let Some(rest) = o.get(len..) else { break };
                    o = rest;
                }
                self.ra.push(RaEvent { link: self.ports[i].link, router: src, lifetime, addrs });
            }
            _ => {}
        }
        true
    }
}
