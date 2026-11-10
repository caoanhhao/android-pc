//! NETLINK_ROUTE: links, addresses, routes, rules and neighbours.
//!
//! Links, addresses and the routing table are the stack's
//! (`net::inet::Net`): dumps list them, RTM_NEWADDR/DELADDR, NEWROUTE/
//! DELROUTE and NEWLINK/SETLINK (name, MTU, flags) change them, and the changes
//! are multicast to the RTNLGRP_LINK / *_IFADDR / *_ROUTE groups. Policy
//! rules are stored and dumped back but not used; the neighbour table
//! dumps empty. Route lookups (RTM_GETROUTE without NLM_F_DUMP) answer
//! with the link the stack would use.

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, Ipv4Address, Ipv6Address};

use super::inet::{
    AF_INET, AF_INET6, IFF_LOWER_UP, IFF_RUNNING, IFF_UP, IfAddr, Link, Net, RT_SCOPE_HOST, RT_SCOPE_LINK,
    RT_SCOPE_UNIVERSE, RT_TABLE_MAIN, Route, Stats, addr_bytes, with_net, with_net_nopoll,
};
use super::netlink::{self, NLM_F_ACK, NLM_F_DUMP, NLMSG_HDRLEN};
use crate::errno::*;

pub const NETLINK_ROUTE: u32 = 0;

const RTM_NEWLINK: u16 = 16;
const RTM_DELLINK: u16 = 17;
const RTM_GETLINK: u16 = 18;
const RTM_SETLINK: u16 = 19;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;
const RTM_NEWNEIGH: u16 = 28;
const RTM_DELNEIGH: u16 = 29;
const RTM_GETNEIGH: u16 = 30;
const RTM_GETNETCONF: u16 = 82;
const RTM_NEWRULE: u16 = 32;
const RTM_DELRULE: u16 = 33;
const RTM_GETRULE: u16 = 34;

const NLM_F_MULTI: u16 = 2;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;
const NLMSG_DONE: u16 = 3;

const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_QDISC: u16 = 6;
const IFLA_STATS: u16 = 7;
const IFLA_TXQLEN: u16 = 13;
const IFLA_OPERSTATE: u16 = 16;
const IFLA_LINKMODE: u16 = 17;
const IFLA_STATS64: u16 = 23;
const IFLA_GROUP: u16 = 27;
const IFLA_PROMISCUITY: u16 = 30;
const IFLA_NUM_TX_QUEUES: u16 = 31;
const IFLA_NUM_RX_QUEUES: u16 = 32;
const IFLA_CARRIER: u16 = 33;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;
const IFA_CACHEINFO: u16 = 6;
const IFA_FLAGS: u16 = 8;
const IFA_F_PERMANENT: u32 = 0x80;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PRIORITY: u16 = 6;
const RTA_PREFSRC: u16 = 7;
const RTA_TABLE: u16 = 15;

const RTN_UNICAST: u8 = 1;
const RTN_LOCAL: u8 = 2;
const RTPROT_KERNEL: u8 = 2;
const RTPROT_BOOT: u8 = 3;

/// Multicast group masks (1 << (RTNLGRP_* - 1)).
const GRP_LINK: u32 = 1 << 0;
const GRP_IPV4_IFADDR: u32 = 1 << 4;
const GRP_IPV4_ROUTE: u32 = 1 << 6;
const GRP_IPV6_IFADDR: u32 = 1 << 8;
const GRP_IPV6_ROUTE: u32 = 1 << 10;

/// Datagrams of a dump are kept under this size.
const DUMP_CHUNK: usize = 8192;

fn attr(out: &mut Vec<u8>, ty: u16, data: &[u8]) {
    let len = 4 + data.len();
    out.extend_from_slice(&(len as u16).to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(4), 0);
}

/// The attributes after a fixed header: (type, data).
fn attrs(b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut v = Vec::new();
    let mut off = 0;
    while off + 4 <= b.len() {
        let len = u16::from_le_bytes([b[off], b[off + 1]]) as usize;
        let ty = u16::from_le_bytes([b[off + 2], b[off + 3]]) & 0x3fff;
        if len < 4 || off + len > b.len() {
            break;
        }
        v.push((ty, &b[off + 4..off + len]));
        off += len.next_multiple_of(4);
    }
    v
}

fn ip_from(family: u16, b: &[u8]) -> Option<IpAddress> {
    match (family, b.len()) {
        (AF_INET, 4) => Some(IpAddress::Ipv4(Ipv4Address::new(b[0], b[1], b[2], b[3]))),
        (AF_INET6, 16) => Some(IpAddress::Ipv6(Ipv6Address::from(<[u8; 16]>::try_from(b).unwrap()))),
        _ => None,
    }
}

fn family_of(a: &IpAddress) -> u16 {
    match a {
        IpAddress::Ipv4(_) => AF_INET,
        IpAddress::Ipv6(_) => AF_INET6,
    }
}

fn link_payload(l: &Link, st: Stats) -> Vec<u8> {
    let mut p = Vec::with_capacity(512);
    p.push(0); // AF_UNSPEC
    p.push(0);
    p.extend_from_slice(&l.arptype.to_le_bytes());
    p.extend_from_slice(&(l.index as i32).to_le_bytes());
    p.extend_from_slice(&l.flags.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    let mut name = l.name.clone().into_bytes();
    name.push(0);
    attr(&mut p, IFLA_IFNAME, &name);
    attr(&mut p, IFLA_TXQLEN, &l.txqlen.to_le_bytes());
    let oper = if l.arptype == super::inet::ARPHRD_LOOPBACK {
        0
    } else if l.flags & IFF_UP != 0 {
        6
    } else {
        2
    };
    attr(&mut p, IFLA_OPERSTATE, &[oper]);
    attr(&mut p, IFLA_LINKMODE, &[0]);
    attr(&mut p, IFLA_MTU, &l.mtu.to_le_bytes());
    attr(&mut p, IFLA_GROUP, &0u32.to_le_bytes());
    attr(&mut p, IFLA_PROMISCUITY, &0u32.to_le_bytes());
    attr(&mut p, IFLA_NUM_TX_QUEUES, &1u32.to_le_bytes());
    attr(&mut p, IFLA_NUM_RX_QUEUES, &1u32.to_le_bytes());
    attr(&mut p, IFLA_CARRIER, &[(l.flags & IFF_LOWER_UP != 0) as u8]);
    attr(&mut p, IFLA_QDISC, if l.arptype == super::inet::ARPHRD_LOOPBACK { b"noqueue\0" } else { b"pfifo_fast\0" });
    attr(&mut p, IFLA_ADDRESS, &l.mac);
    attr(&mut p, IFLA_BROADCAST, if l.arptype == super::inet::ARPHRD_LOOPBACK { &[0u8; 6] } else { &[0xffu8; 6] });
    let s32: Vec<u8> = [st.rx_packets, st.tx_packets, st.rx_bytes, st.tx_bytes, 0, 0, 0, st.tx_dropped]
        .iter()
        .flat_map(|v| (*v as u32).to_le_bytes())
        .chain(core::iter::repeat_n(0u8, 16 * 4))
        .collect();
    attr(&mut p, IFLA_STATS, &s32);
    let s64: Vec<u8> = [st.rx_packets, st.tx_packets, st.rx_bytes, st.tx_bytes, 0, 0, 0, st.tx_dropped]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .chain(core::iter::repeat_n(0u8, 16 * 8))
        .collect();
    attr(&mut p, IFLA_STATS64, &s64);
    p
}

fn addr_payload(l: &Link, a: &IfAddr) -> Vec<u8> {
    let fam = family_of(&a.addr);
    let mut p = vec![fam as u8, a.prefix, a.flags as u8, a.scope];
    p.extend_from_slice(&l.index.to_le_bytes());
    let ab = addr_bytes(&a.addr);
    attr(&mut p, IFA_ADDRESS, &ab);
    if fam == AF_INET {
        attr(&mut p, IFA_LOCAL, &ab);
        if l.arptype != super::inet::ARPHRD_LOOPBACK && a.prefix < 31 {
            let m = if a.prefix == 0 { 0 } else { u32::MAX << (32 - a.prefix as u32) };
            let b = u32::from_be_bytes(ab[..4].try_into().unwrap()) | !m;
            attr(&mut p, IFA_BROADCAST, &b.to_be_bytes());
        }
        let mut name = l.name.clone().into_bytes();
        name.push(0);
        attr(&mut p, IFA_LABEL, &name);
    }
    let mut ci = Vec::new();
    ci.extend_from_slice(&u32::MAX.to_le_bytes());
    ci.extend_from_slice(&u32::MAX.to_le_bytes());
    ci.extend_from_slice(&0u32.to_le_bytes());
    ci.extend_from_slice(&0u32.to_le_bytes());
    attr(&mut p, IFA_CACHEINFO, &ci);
    attr(&mut p, IFA_FLAGS, &a.flags.to_le_bytes());
    p
}

fn route_payload(r: &Route) -> Vec<u8> {
    let table = if r.table > 255 { 252 } else { r.table as u8 };
    let mut p = vec![r.family() as u8, r.dst_len, 0, 0, table, r.protocol, r.scope, r.ty];
    p.extend_from_slice(&0u32.to_le_bytes());
    attr(&mut p, RTA_TABLE, &r.table.to_le_bytes());
    if r.dst_len > 0 {
        attr(&mut p, RTA_DST, &addr_bytes(&r.dst));
    }
    if r.metric != 0 {
        attr(&mut p, RTA_PRIORITY, &r.metric.to_le_bytes());
    }
    if let Some(s) = r.prefsrc {
        attr(&mut p, RTA_PREFSRC, &addr_bytes(&s));
    }
    if let Some(g) = r.gateway {
        attr(&mut p, RTA_GATEWAY, &addr_bytes(&g));
    }
    if r.oif != 0 {
        attr(&mut p, RTA_OIF, &r.oif.to_le_bytes());
    }
    p
}

/// Collects reply messages, packing dumps into datagrams.
struct Reply {
    seq: u32,
    pid: u32,
    out: Vec<Vec<u8>>,
    cur: Vec<u8>,
}

impl Reply {
    fn push(&mut self, ty: u16, flags: u16, payload: &[u8]) {
        let m = netlink::nlmsg(ty, flags, self.seq, self.pid, payload);
        if !self.cur.is_empty() && self.cur.len() + m.len() > DUMP_CHUNK {
            self.out.push(core::mem::take(&mut self.cur));
        }
        self.cur.extend_from_slice(&m);
    }

    fn done(mut self) -> Vec<Vec<u8>> {
        self.push(NLMSG_DONE, NLM_F_MULTI, &0i32.to_le_bytes());
        self.out.push(self.cur);
        self.out
    }
}

fn error_msg(err: i32, hdr: &[u8], seq: u32, pid: u32) -> Vec<u8> {
    let mut p = err.to_le_bytes().to_vec();
    p.extend_from_slice(&hdr[..NLMSG_HDRLEN]);
    netlink::nlmsg(2, if err == 0 { 0x100 } else { 0 }, seq, pid, &p)
}

/// Handle one request sent to the kernel; returns the datagrams to queue
/// on the sender, or None for types this module does not know.
pub fn request(msg: &[u8], pid: u32) -> Option<Vec<Vec<u8>>> {
    let ty = u16::from_le_bytes([msg[4], msg[5]]);
    let flags = u16::from_le_bytes([msg[6], msg[7]]);
    let seq = u32::from_le_bytes(msg[8..12].try_into().unwrap());
    let body = &msg[NLMSG_HDRLEN..];
    let dump = flags & NLM_F_DUMP == NLM_F_DUMP;
    let mut rep = Reply { seq, pid, out: Vec::new(), cur: Vec::new() };
    let result: KResult<()> = match ty {
        RTM_GETLINK if dump => {
            with_net_nopoll(|n| {
                for l in &n.links {
                    rep.push(RTM_NEWLINK, NLM_F_MULTI, &link_payload(l, n.stats(l.index)));
                }
            });
            return Some(rep.done());
        }
        RTM_GETADDR if dump => {
            let fam = body.first().copied().unwrap_or(0) as u16;
            with_net_nopoll(|n| {
                for l in &n.links {
                    for a in &l.addrs {
                        if fam == 0 || fam == family_of(&a.addr) {
                            rep.push(RTM_NEWADDR, NLM_F_MULTI, &addr_payload(l, a));
                        }
                    }
                }
            });
            return Some(rep.done());
        }
        RTM_GETROUTE if dump => {
            let fam = body.first().copied().unwrap_or(0) as u16;
            with_net_nopoll(|n| {
                for r in &n.routes {
                    if fam == 0 || fam == r.family() {
                        rep.push(RTM_NEWROUTE, NLM_F_MULTI, &route_payload(r));
                    }
                }
                for (fam_r, l, a) in local_routes(n) {
                    if fam == 0 || fam == fam_r {
                        let r = Route {
                            dst: a.addr,
                            dst_len: if fam_r == AF_INET { 32 } else { 128 },
                            gateway: None,
                            oif: l,
                            table: super::inet::RT_TABLE_LOCAL,
                            metric: 0,
                            protocol: RTPROT_KERNEL,
                            scope: RT_SCOPE_HOST,
                            ty: RTN_LOCAL,
                            prefsrc: Some(a.addr),
                        };
                        rep.push(RTM_NEWROUTE, NLM_F_MULTI, &route_payload(&r));
                    }
                }
            });
            return Some(rep.done());
        }
        RTM_GETRULE if dump => {
            let fam = body.first().copied().unwrap_or(0) as u16;
            with_net_nopoll(|n| {
                for (f, p) in &n.rules {
                    if fam == 0 || fam == *f {
                        rep.push(RTM_NEWRULE, NLM_F_MULTI, p);
                    }
                }
            });
            return Some(rep.done());
        }
        RTM_GETNEIGH | RTM_GETNETCONF if dump => return Some(rep.done()),
        RTM_GETLINK => match get_link(body) {
            Ok(p) => {
                rep.push(RTM_NEWLINK, 0, &p);
                rep.out.push(rep.cur);
                return Some(rep.out);
            }
            Err(e) => Err(e),
        },
        RTM_GETROUTE => match route_get(body) {
            Ok(p) => {
                rep.push(RTM_NEWROUTE, 0, &p);
                rep.out.push(rep.cur);
                return Some(rep.out);
            }
            Err(e) => Err(e),
        },
        RTM_NEWLINK if flags & NLM_F_CREATE != 0 && find_link(body).is_err() => Err(EOPNOTSUPP),
        RTM_NEWLINK | RTM_SETLINK => set_link(body),
        RTM_DELLINK => Err(EOPNOTSUPP),
        RTM_NEWADDR => new_addr(body, flags),
        RTM_DELADDR => del_addr(body),
        RTM_NEWROUTE => new_route(body, flags),
        RTM_DELROUTE => del_route(body),
        RTM_NEWRULE => {
            // Kept in priority order (FRA_PRIORITY), as dumps list them.
            let fam = body.first().copied().unwrap_or(0) as u16;
            let prio = rule_priority(body);
            with_net_nopoll(|n| {
                let at = n.rules.iter().position(|(_, p)| rule_priority(p) > prio).unwrap_or(n.rules.len());
                n.rules.insert(at, (fam, body.to_vec()));
            });
            Ok(())
        }
        RTM_DELRULE => with_net_nopoll(|n| match n.rules.iter().position(|(_, p)| p.as_slice() == body) {
            Some(i) => {
                n.rules.remove(i);
                Ok(())
            }
            None => Err(ENOENT),
        }),
        RTM_NEWNEIGH | RTM_DELNEIGH => Ok(()),
        _ => return None,
    };
    let err = match result {
        Ok(()) if flags & NLM_F_ACK == 0 => return Some(Vec::new()),
        Ok(()) => 0,
        Err(e) => -e.0,
    };
    Some(vec![error_msg(err, msg, seq, pid)])
}


/// A fib rule's FRA_PRIORITY (after the 12-byte fib_rule_hdr).
fn rule_priority(body: &[u8]) -> u32 {
    body.get(12..)
        .and_then(|a| attrs(a).into_iter().find(|(t, d)| *t == 6 && d.len() == 4))
        .map_or(0, |(_, d)| u32::from_le_bytes(d.try_into().unwrap()))
}

/// (family, link, address) of every local address (the "local" table).
fn local_routes(n: &Net) -> Vec<(u16, u32, IfAddr)> {
    let mut v = Vec::new();
    for l in &n.links {
        for a in &l.addrs {
            v.push((family_of(&a.addr), l.index, *a));
        }
    }
    v
}

/// The link an ifinfomsg names (index, else IFLA_IFNAME).
fn find_link(body: &[u8]) -> KResult<u32> {
    if body.len() < 16 {
        return Err(EINVAL);
    }
    let idx = i32::from_le_bytes(body[4..8].try_into().unwrap());
    with_net_nopoll(|n| {
        if idx > 0 {
            return n.link(idx as u32).map(|l| l.index).ok_or(ENODEV);
        }
        for (ty, d) in attrs(&body[16..]) {
            if ty == IFLA_IFNAME {
                let name = core::str::from_utf8(d).unwrap_or("").trim_end_matches('\0');
                return n.link_by_name(name).map(|l| l.index).ok_or(ENODEV);
            }
        }
        Err(EINVAL)
    })
}

fn get_link(body: &[u8]) -> KResult<Vec<u8>> {
    let idx = find_link(body)?;
    with_net_nopoll(|n| Ok(link_payload(n.link(idx).unwrap(), n.stats(idx))))
}

fn set_link(body: &[u8]) -> KResult<()> {
    let idx = find_link(body)?;
    let flags = u32::from_le_bytes(body[8..12].try_into().unwrap());
    let change = u32::from_le_bytes(body[12..16].try_into().unwrap());
    let a = attrs(&body[16..]);
    let mtu = a.iter().find(|(t, d)| *t == IFLA_MTU && d.len() == 4).map(|(_, d)| u32::from_le_bytes((*d).try_into().unwrap()));
    let mac: Option<[u8; 6]> = a.iter().find(|(t, d)| *t == IFLA_ADDRESS && d.len() == 6).map(|(_, d)| (*d).try_into().unwrap());
    let name = a
        .iter()
        .find(|(t, _)| *t == IFLA_IFNAME)
        .map(|(_, d)| alloc::string::String::from(core::str::from_utf8(d).unwrap_or("").trim_end_matches('\0')));
    // In Linux's order (do_setlink): the name, then the MTU, then the
    // flags. A rename needs the link down, so renaming an up link fails
    // with EBUSY and changes nothing.
    with_net(|n| {
        if let Some(new) = &name
            && n.link(idx).unwrap().name != *new
        {
            if new.is_empty() || new.len() >= 16 || new.contains('/') {
                return Err(EINVAL);
            }
            if n.link_by_name(new).is_some() {
                return Err(EEXIST);
            }
            if n.link(idx).unwrap().flags & IFF_UP != 0 {
                return Err(EBUSY);
            }
            n.link_mut(idx).unwrap().name = new.clone();
        }
        if let Some(m) = mac {
            n.set_mac(idx, m)?;
        }
        let l = n.link_mut(idx).unwrap();
        if let Some(m) = mtu {
            l.mtu = m;
        }
        if flags != 0 || change != 0 {
            let mask = if change == 0 { 0xffff } else { change };
            let mut nf = (l.flags & !mask) | (flags & mask);
            if l.arptype != super::inet::ARPHRD_LOOPBACK {
                nf = if nf & IFF_UP != 0 { nf | IFF_RUNNING | IFF_LOWER_UP } else { nf & !(IFF_RUNNING | IFF_LOWER_UP) };
            }
            l.flags = nf;
        }
        n.sync();
        Ok(())
    })?;
    notify_links();
    Ok(())
}

/// (link index, address) of an ifaddrmsg request.
fn parse_addr(body: &[u8]) -> KResult<(u32, IfAddr)> {
    if body.len() < 8 {
        return Err(EINVAL);
    }
    let fam = body[0] as u16;
    let idx = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let mut addr = None;
    let mut fl = body[2] as u32;
    for (ty, d) in attrs(&body[8..]) {
        match ty {
            IFA_LOCAL => addr = ip_from(fam, d),
            IFA_ADDRESS if addr.is_none() => addr = ip_from(fam, d),
            IFA_FLAGS if d.len() == 4 => fl = u32::from_le_bytes(d.try_into().unwrap()),
            _ => {}
        }
    }
    let addr = addr.ok_or(EINVAL)?;
    let scope = if body[3] != 0 {
        body[3]
    } else {
        match addr {
            IpAddress::Ipv4(a) if a.is_loopback() => RT_SCOPE_HOST,
            IpAddress::Ipv6(a) if a.is_loopback() => RT_SCOPE_HOST,
            IpAddress::Ipv6(a) if a.is_unicast_link_local() => RT_SCOPE_LINK,
            _ => RT_SCOPE_UNIVERSE,
        }
    };
    let max = if fam == AF_INET { 32 } else { 128 };
    if body[1] > max {
        return Err(EINVAL);
    }
    Ok((idx, IfAddr { addr, prefix: body[1], scope, flags: fl | IFA_F_PERMANENT }))
}

fn new_addr(body: &[u8], flags: u16) -> KResult<()> {
    let (idx, a) = parse_addr(body)?;
    let payload = with_net(|n| {
        let l = n.link_mut(idx).ok_or(ENODEV)?;
        match l.addrs.iter().position(|x| x.addr == a.addr) {
            Some(_) if flags & NLM_F_EXCL != 0 => return Err(EEXIST),
            Some(i) => l.addrs[i] = a,
            None => l.addrs.push(a),
        }
        let p = addr_payload(l, &a);
        n.sync();
        Ok(p)
    })?;
    let grp = if family_of(&a.addr) == AF_INET { GRP_IPV4_IFADDR } else { GRP_IPV6_IFADDR };
    netlink::multicast(NETLINK_ROUTE, grp, &netlink::nlmsg(RTM_NEWADDR, 0, 0, 0, &payload));
    Ok(())
}

fn del_addr(body: &[u8]) -> KResult<()> {
    let (idx, a) = parse_addr(body)?;
    let payload = with_net(|n| {
        let l = n.link_mut(idx).ok_or(ENODEV)?;
        let i = l.addrs.iter().position(|x| x.addr == a.addr).ok_or(EADDRNOTAVAIL)?;
        let old = l.addrs.remove(i);
        let p = addr_payload(l, &old);
        n.sync();
        Ok(p)
    })?;
    let grp = if family_of(&a.addr) == AF_INET { GRP_IPV4_IFADDR } else { GRP_IPV6_IFADDR };
    netlink::multicast(NETLINK_ROUTE, grp, &netlink::nlmsg(RTM_DELADDR, 0, 0, 0, &payload));
    Ok(())
}

fn parse_route(body: &[u8]) -> KResult<Route> {
    if body.len() < 12 {
        return Err(EINVAL);
    }
    let fam = body[0] as u16;
    if fam != AF_INET && fam != AF_INET6 {
        return Err(EAFNOSUPPORT);
    }
    let zero = if fam == AF_INET {
        IpAddress::Ipv4(Ipv4Address::UNSPECIFIED)
    } else {
        IpAddress::Ipv6(Ipv6Address::UNSPECIFIED)
    };
    let mut r = Route {
        dst: zero,
        dst_len: body[1],
        gateway: None,
        oif: 0,
        table: body[4] as u32,
        metric: 0,
        protocol: if body[5] == 0 { RTPROT_BOOT } else { body[5] },
        scope: body[6],
        ty: if body[7] == 0 { RTN_UNICAST } else { body[7] },
        prefsrc: None,
    };
    for (ty, d) in attrs(&body[12..]) {
        match ty {
            RTA_DST => r.dst = ip_from(fam, d).ok_or(EINVAL)?,
            RTA_GATEWAY => r.gateway = ip_from(fam, d),
            RTA_PREFSRC => r.prefsrc = ip_from(fam, d),
            RTA_OIF if d.len() == 4 => r.oif = u32::from_le_bytes(d.try_into().unwrap()),
            RTA_PRIORITY if d.len() == 4 => r.metric = u32::from_le_bytes(d.try_into().unwrap()),
            RTA_TABLE if d.len() == 4 => r.table = u32::from_le_bytes(d.try_into().unwrap()),
            _ => {}
        }
    }
    if r.table == 0 {
        r.table = RT_TABLE_MAIN;
    }
    Ok(r)
}

fn same_route(a: &Route, b: &Route) -> bool {
    a.dst == b.dst && a.dst_len == b.dst_len && a.table == b.table && a.metric == b.metric
}

fn new_route(body: &[u8], flags: u16) -> KResult<()> {
    let r = parse_route(body)?;
    with_net(|n| {
        if r.oif != 0 && n.link(r.oif).is_none() {
            return Err(ENODEV);
        }
        match n.routes.iter().position(|x| same_route(x, &r)) {
            Some(_) if flags & NLM_F_EXCL != 0 => return Err(EEXIST),
            Some(i) => n.routes[i] = r.clone(),
            None => n.routes.push(r.clone()),
        }
        n.sync();
        Ok(())
    })?;
    let grp = if r.family() == AF_INET { GRP_IPV4_ROUTE } else { GRP_IPV6_ROUTE };
    netlink::multicast(NETLINK_ROUTE, grp, &netlink::nlmsg(RTM_NEWROUTE, 0, 0, 0, &route_payload(&r)));
    Ok(())
}

fn del_route(body: &[u8]) -> KResult<()> {
    let r = parse_route(body)?;
    let old = with_net(|n| {
        let i = n
            .routes
            .iter()
            .position(|x| x.dst == r.dst && x.dst_len == r.dst_len && x.table == r.table && (r.oif == 0 || x.oif == r.oif))
            .ok_or(ESRCH)?;
        let old = n.routes.remove(i);
        n.sync();
        Ok(old)
    })?;
    let grp = if old.family() == AF_INET { GRP_IPV4_ROUTE } else { GRP_IPV6_ROUTE };
    netlink::multicast(NETLINK_ROUTE, grp, &netlink::nlmsg(RTM_DELROUTE, 0, 0, 0, &route_payload(&old)));
    Ok(())
}

/// RTM_GETROUTE for one destination (`ip route get`).
fn route_get(body: &[u8]) -> KResult<Vec<u8>> {
    let q = parse_route(body)?;
    let dst = q.dst;
    with_net_nopoll(|n| {
        let local = n.links.iter().find(|l| l.addrs.iter().any(|a| a.addr == dst)).map(|l| l.index);
        let loopback = match dst {
            IpAddress::Ipv4(a) => a.is_loopback(),
            IpAddress::Ipv6(a) => a.is_loopback(),
        };
        let (oif, gw, ty, table) = if loopback || local.is_some() {
            (1, None, RTN_LOCAL, super::inet::RT_TABLE_LOCAL)
        } else {
            // Longest prefix match in the main table.
            let best = n
                .routes
                .iter()
                .filter(|r| r.table == RT_TABLE_MAIN && r.family() == family_of(&dst) && prefix_match(&r.dst, r.dst_len, &dst))
                .max_by_key(|r| r.dst_len)
                .ok_or(ENETUNREACH)?;
            (best.oif, best.gateway, RTN_UNICAST, RT_TABLE_MAIN)
        };
        let src = n.link(oif).and_then(|l| l.addrs.iter().find(|a| family_of(&a.addr) == family_of(&dst)).map(|a| a.addr));
        let r = Route {
            dst,
            dst_len: if family_of(&dst) == AF_INET { 32 } else { 128 },
            gateway: gw,
            oif,
            table,
            metric: 0,
            protocol: 0,
            scope: RT_SCOPE_UNIVERSE,
            ty,
            prefsrc: src,
        };
        Ok(route_payload(&r))
    })
}

fn prefix_match(net: &IpAddress, len: u8, a: &IpAddress) -> bool {
    let (x, y) = (addr_bytes(net), addr_bytes(a));
    if x.len() != y.len() {
        return false;
    }
    let full = (len / 8) as usize;
    if x[..full] != y[..full] {
        return false;
    }
    let rem = len % 8;
    rem == 0 || (x[full] ^ y[full]) & (0xffu8 << (8 - rem)) == 0
}

/// Multicast RTM_NEWLINK for every link (after a flags change).
pub fn notify_links() {
    let msgs: Vec<Vec<u8>> =
        with_net_nopoll(|n| n.links.iter().map(|l| netlink::nlmsg(RTM_NEWLINK, 0, 0, 0, &link_payload(l, n.stats(l.index)))).collect());
    for m in msgs {
        netlink::multicast(NETLINK_ROUTE, GRP_LINK, &m);
    }
}
