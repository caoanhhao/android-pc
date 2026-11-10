//! Network interface ioctls (struct ifreq / ifconf) on any socket:
//! names, indexes, flags, addresses, MTU, hardware address, txqueuelen,
//! and the basic SIOCETHTOOL queries (driver info, link, settings,
//! permanent address).

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, Ipv4Address};

use super::inet::{AF_INET, IFF_UP, IfAddr, Net, RT_SCOPE_UNIVERSE, with_net, with_net_nopoll};
use crate::errno::*;
use crate::process::current;

const SIOCADDRT: u32 = 0x890b;
const SIOCDELRT: u32 = 0x890c;
const SIOCGIFNAME: u32 = 0x8910;
const SIOCGIFCONF: u32 = 0x8912;
const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCSIFFLAGS: u32 = 0x8914;
const SIOCGIFADDR: u32 = 0x8915;
const SIOCSIFADDR: u32 = 0x8916;
const SIOCGIFDSTADDR: u32 = 0x8917;
const SIOCGIFBRDADDR: u32 = 0x8919;
const SIOCSIFBRDADDR: u32 = 0x891a;
const SIOCGIFNETMASK: u32 = 0x891b;
const SIOCSIFNETMASK: u32 = 0x891c;
const SIOCGIFMETRIC: u32 = 0x891d;
const SIOCGIFMTU: u32 = 0x8921;
const SIOCSIFMTU: u32 = 0x8922;
const SIOCGIFHWADDR: u32 = 0x8927;
const SIOCSIFHWADDR: u32 = 0x8924;
const SIOCGIFINDEX: u32 = 0x8933;
const SIOCGIFTXQLEN: u32 = 0x8942;
const SIOCSIFTXQLEN: u32 = 0x8943;
const SIOCETHTOOL: u32 = 0x8946;
const SIOCGIFMAP: u32 = 0x8970;

const IFREQ: usize = 40;
const IFNAMSIZ: usize = 16;

fn sockaddr4(a: [u8; 4]) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..2].copy_from_slice(&AF_INET.to_le_bytes());
    b[4..8].copy_from_slice(&a);
    b
}

fn mask4(prefix: u8) -> [u8; 4] {
    (if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) }).to_be_bytes()
}

fn first_v4(addrs: &[IfAddr]) -> Option<(Ipv4Address, u8)> {
    addrs.iter().find_map(|a| match a.addr {
        IpAddress::Ipv4(v) => Some((v, a.prefix)),
        _ => None,
    })
}

fn ifname(b: &[u8]) -> &str {
    let n = &b[..IFNAMSIZ];
    let end = n.iter().position(|&c| c == 0).unwrap_or(IFNAMSIZ);
    core::str::from_utf8(&n[..end]).unwrap_or("")
}

/// Handle `cmd` if it is an interface ioctl (ENOTTY otherwise).
pub fn ioctl(cmd: u32, arg: u64) -> KResult<u64> {
    match cmd {
        SIOCGIFCONF => return ifconf(arg),
        SIOCADDRT | SIOCDELRT => return Ok(0),
        SIOCETHTOOL => return ethtool(arg),
        SIOCGIFNAME | SIOCGIFFLAGS | SIOCSIFFLAGS | SIOCGIFADDR | SIOCSIFADDR | SIOCGIFDSTADDR | SIOCGIFBRDADDR
        | SIOCSIFBRDADDR | SIOCGIFNETMASK | SIOCSIFNETMASK | SIOCGIFMETRIC | SIOCGIFMTU | SIOCSIFMTU | SIOCGIFHWADDR
        | SIOCGIFINDEX | SIOCGIFTXQLEN | SIOCSIFTXQLEN | SIOCGIFMAP | SIOCSIFHWADDR => {}
        _ => return Err(ENOTTY),
    }
    let mm = current().mm();
    let mut r = [0u8; IFREQ];
    mm.copy_in(arg, &mut r)?;
    let mut changed = false;
    let out = with_net_nopoll(|n| -> KResult<Option<[u8; IFREQ]>> {
        if cmd == SIOCGIFNAME {
            let idx = u32::from_le_bytes(r[16..20].try_into().unwrap());
            let l = n.link(idx).ok_or(ENODEV)?;
            let mut o = r;
            o[..IFNAMSIZ].fill(0);
            o[..l.name.len()].copy_from_slice(l.name.as_bytes());
            return Ok(Some(o));
        }
        let idx = n.link_by_name(ifname(&r)).ok_or(ENODEV)?.index;
        if cmd == SIOCSIFHWADDR {
            if u16::from_le_bytes([r[16], r[17]]) != super::inet::ARPHRD_ETHER {
                return Err(EINVAL);
            }
            n.set_mac(idx, r[18..24].try_into().unwrap())?;
            changed = true;
            return Ok(None);
        }
        let mut o = r;
        let l = n.link_mut(idx).unwrap();
        let v4 = first_v4(&l.addrs);
        match cmd {
            SIOCGIFFLAGS => o[16..18].copy_from_slice(&(l.flags as u16).to_le_bytes()),
            SIOCSIFFLAGS => {
                let f = u16::from_le_bytes([r[16], r[17]]) as u32;
                let keep = l.flags & !0xffff;
                let mut nf = keep | (f & 0xffff);
                if l.arptype != super::inet::ARPHRD_LOOPBACK {
                    nf = if f & IFF_UP != 0 { nf | 0x10000 | 0x40 } else { nf & !(0x10000 | 0x40) };
                }
                l.flags = nf;
                changed = true;
                return Ok(None);
            }
            SIOCGIFADDR => o[16..32].copy_from_slice(&sockaddr4(v4.ok_or(EADDRNOTAVAIL)?.0.octets())),
            SIOCGIFDSTADDR => o[16..32].copy_from_slice(&sockaddr4(v4.ok_or(EADDRNOTAVAIL)?.0.octets())),
            SIOCGIFNETMASK => o[16..32].copy_from_slice(&sockaddr4(mask4(v4.ok_or(EADDRNOTAVAIL)?.1))),
            SIOCGIFBRDADDR => {
                let (a, p) = v4.ok_or(EADDRNOTAVAIL)?;
                let m = u32::from_be_bytes(mask4(p));
                o[16..32].copy_from_slice(&sockaddr4((u32::from_be_bytes(a.octets()) | !m).to_be_bytes()));
            }
            SIOCSIFADDR | SIOCSIFNETMASK => {
                if u16::from_le_bytes([r[16], r[17]]) != AF_INET {
                    return Err(EINVAL);
                }
                let val: [u8; 4] = r[20..24].try_into().unwrap();
                set_v4(n, idx, cmd, val);
                changed = true;
                return Ok(None);
            }
            SIOCSIFBRDADDR => return Ok(None),
            SIOCGIFMETRIC => o[16..20].copy_from_slice(&0u32.to_le_bytes()),
            SIOCGIFMTU => o[16..20].copy_from_slice(&l.mtu.to_le_bytes()),
            SIOCSIFMTU => {
                let mtu = u32::from_le_bytes(r[16..20].try_into().unwrap());
                if !(68..=65536).contains(&mtu) {
                    return Err(EINVAL);
                }
                l.mtu = mtu;
                return Ok(None);
            }
            SIOCGIFHWADDR => {
                o[16..18].copy_from_slice(&l.arptype.to_le_bytes());
                o[18..24].copy_from_slice(&l.mac);
                o[24..32].fill(0);
            }
            SIOCGIFINDEX => o[16..20].copy_from_slice(&idx.to_le_bytes()),
            SIOCGIFTXQLEN => o[16..20].copy_from_slice(&l.txqlen.to_le_bytes()),
            SIOCSIFTXQLEN => {
                l.txqlen = u32::from_le_bytes(r[16..20].try_into().unwrap());
                return Ok(None);
            }
            SIOCGIFMAP => o[16..40].fill(0),
            _ => return Err(ENOTTY),
        }
        Ok(Some(o))
    })?;
    if changed {
        with_net(|n| n.sync());
        super::rtnetlink::notify_links();
    }
    if let Some(o) = out {
        mm.copy_out(arg, &o)?;
    }
    Ok(0)
}

const ETHTOOL_GSET: u32 = 0x1;
const ETHTOOL_GDRVINFO: u32 = 0x3;
const ETHTOOL_GLINK: u32 = 0xa;
const ETHTOOL_GPERMADDR: u32 = 0x20;

/// SIOCETHTOOL: `ifr_data` points at a command-specific struct whose
/// first u32 is the command. Cards look like Linux's virtio_net (speed
/// and duplex unknown); `lo` only answers GLINK.
fn ethtool(arg: u64) -> KResult<u64> {
    let mm = current().mm();
    let mut r = [0u8; IFREQ];
    mm.copy_in(arg, &mut r)?;
    let data = u64::from_le_bytes(r[16..24].try_into().unwrap());
    let mut c = [0u8; 4];
    mm.copy_in(data, &mut c)?;
    let cmd = u32::from_le_bytes(c);
    let (up, mac, bus) = with_net_nopoll(|n| {
        let l = n.link_by_name(ifname(&r)).ok_or(ENODEV)?;
        Ok::<_, Errno>((l.flags & IFF_UP != 0, l.mac, n.bus_info(l.index).map(alloc::string::String::from)))
    })?;
    match cmd {
        ETHTOOL_GLINK => {
            let mut v = [0u8; 8];
            v[..4].copy_from_slice(&cmd.to_le_bytes());
            v[4..].copy_from_slice(&(up as u32).to_le_bytes());
            mm.copy_out(data, &v)?;
        }
        ETHTOOL_GDRVINFO => {
            let bus = bus.ok_or(EOPNOTSUPP)?;
            let mut v = [0u8; 196];
            v[..4].copy_from_slice(&cmd.to_le_bytes());
            let mut put = |off: usize, s: &str| v[off..off + s.len().min(31)].copy_from_slice(&s.as_bytes()[..s.len().min(31)]);
            put(4, "virtio_net");
            put(36, "1.0.0");
            put(100, &bus);
            mm.copy_out(data, &v)?;
        }
        ETHTOOL_GSET => {
            bus.ok_or(EOPNOTSUPP)?;
            let mut v = [0u8; 44];
            v[..4].copy_from_slice(&cmd.to_le_bytes());
            v[12..14].copy_from_slice(&0xffffu16.to_le_bytes()); // SPEED_UNKNOWN (low half)
            v[14] = 0xff; // DUPLEX_UNKNOWN
            v[15] = 0xef; // PORT_OTHER
            v[28..30].copy_from_slice(&0xffffu16.to_le_bytes()); // speed_hi
            mm.copy_out(data, &v)?;
        }
        ETHTOOL_GPERMADDR => {
            bus.ok_or(EOPNOTSUPP)?;
            let mut h = [0u8; 8];
            mm.copy_in(data, &mut h)?;
            let size = u32::from_le_bytes(h[4..8].try_into().unwrap());
            h[4..8].copy_from_slice(&6u32.to_le_bytes());
            mm.copy_out(data, &h)?;
            if size < 6 {
                return Err(E2BIG);
            }
            mm.copy_out(data + 8, &mac)?;
        }
        _ => return Err(EOPNOTSUPP),
    }
    Ok(0)
}

fn set_v4(n: &mut Net, idx: u32, cmd: u32, val: [u8; 4]) {
    let l = n.link_mut(idx).unwrap();
    let cur = l.addrs.iter().position(|a| matches!(a.addr, IpAddress::Ipv4(_)));
    let (addr, prefix) = match (cmd, cur) {
        (SIOCSIFADDR, Some(i)) => (Ipv4Address::from(val), l.addrs[i].prefix),
        (SIOCSIFADDR, None) => (Ipv4Address::from(val), if val[0] < 128 { 8 } else if val[0] < 192 { 16 } else { 24 }),
        (_, Some(i)) => {
            let IpAddress::Ipv4(a) = l.addrs[i].addr else { unreachable!() };
            (a, u32::from_be_bytes(val).leading_ones() as u8)
        }
        (_, None) => return,
    };
    let a = IfAddr { addr: IpAddress::Ipv4(addr), prefix, scope: RT_SCOPE_UNIVERSE, flags: 0x80 };
    match cur {
        Some(i) if addr.is_unspecified() => {
            l.addrs.remove(i);
        }
        Some(i) => l.addrs[i] = a,
        None if !addr.is_unspecified() => l.addrs.push(a),
        None => {}
    }
}

/// SIOCGIFCONF: the IPv4 addresses, one ifreq each.
fn ifconf(arg: u64) -> KResult<u64> {
    let mm = current().mm();
    let mut c = [0u8; 16];
    mm.copy_in(arg, &mut c)?;
    let len = i32::from_le_bytes(c[0..4].try_into().unwrap()).max(0) as usize;
    let buf = u64::from_le_bytes(c[8..16].try_into().unwrap());
    let reqs: Vec<[u8; IFREQ]> = with_net_nopoll(|n| {
        let mut v = Vec::new();
        for l in &n.links {
            for a in &l.addrs {
                if let IpAddress::Ipv4(ip) = a.addr {
                    let mut r = [0u8; IFREQ];
                    r[..l.name.len()].copy_from_slice(l.name.as_bytes());
                    r[16..32].copy_from_slice(&sockaddr4(ip.octets()));
                    v.push(r);
                }
            }
        }
        v
    });
    let total = if buf == 0 {
        reqs.len() * IFREQ
    } else {
        let fit = (len / IFREQ).min(reqs.len());
        let mut out = vec![0u8; fit * IFREQ];
        for (i, r) in reqs.iter().take(fit).enumerate() {
            out[i * IFREQ..(i + 1) * IFREQ].copy_from_slice(r);
        }
        mm.copy_out(buf, &out)?;
        out.len()
    };
    mm.copy_out(arg, &(total as i32).to_le_bytes())?;
    Ok(0)
}
