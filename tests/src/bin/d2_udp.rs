//! D2: UDP and ICMP sockets, and the network outside the machine.
//! Loopback: 64 queued datagrams of growing size read back in order,
//! 60000-byte datagrams over IPv4 and IPv6 (MSG_TRUNC on a short
//! buffer), recvmsg source addresses, a connected socket ignoring other
//! senders, SO_RCVTIMEO, EAGAIN and poll; ICMP "ping" sockets
//! (SOCK_DGRAM + IPPROTO_ICMP/ICMPV6) echoing over 127.0.0.1 and ::1.
//! On the Rust kernel also eth0 on QEMU's user network: ping and DNS
//! (UDP to 10.0.2.3) through slirp, a 4000-byte datagram (IPv4
//! fragments) to the gateway, IPv6 from the router advertisement
//! (fe80:: link-local, a fec0::/64 SLAAC address, the default route in
//! /proc/net/ipv6_route) with ping6 and a refused TCP connect to fec0::2, the
//! SIOCETHTOOL queries, and the /proc/net statistics files.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("d2_udp", main);

const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;
const IPPROTO_ICMP: i32 = 1;
const IPPROTO_ICMPV6: i32 = 58;
const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;
const MSG_TRUNC: i32 = 0x20;
const MSG_DONTWAIT: i32 = 0x40;
const FIONREAD: u32 = 0x541b;
const SIOCETHTOOL: u32 = 0x8946;
const SIOCGIFHWADDR: u32 = 0x8927;

const LOOP4: [u8; 4] = [127, 0, 0, 1];
const LOOP6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn sock(domain: i32, ty: i32, proto: i32) -> i32 {
    (unsafe { syscall!(41, domain, ty | SOCK_CLOEXEC, proto) }) as i32
}

fn sin(a: [u8; 4], port: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&a);
    b
}

fn sin6(a: [u8; 16], port: u16) -> [u8; 28] {
    let mut b = [0u8; 28];
    b[..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[8..24].copy_from_slice(&a);
    b
}

fn bind(fd: i32, a: &[u8]) -> isize {
    unsafe { syscall!(49, fd, a.as_ptr(), a.len()) }
}

fn connect(fd: i32, a: &[u8]) -> isize {
    unsafe { syscall!(42, fd, a.as_ptr(), a.len()) }
}

fn local_port(fd: i32) -> u16 {
    let mut b = [0u8; 28];
    let mut len = 28u32;
    unsafe { syscall!(51, fd, b.as_mut_ptr(), &mut len as *mut u32) };
    u16::from_be_bytes([b[2], b[3]])
}

fn sendto(fd: i32, b: &[u8], to: &[u8]) -> isize {
    unsafe { syscall!(44, fd, b.as_ptr(), b.len(), 0, to.as_ptr(), to.len()) }
}

fn recvfrom(fd: i32, b: &mut [u8], flags: i32, from: &mut [u8]) -> isize {
    let mut len = from.len() as u32;
    unsafe { syscall!(45, fd, b.as_mut_ptr(), b.len(), flags, from.as_mut_ptr(), &mut len as *mut u32) }
}

fn readable(fd: i32, ms: i32) -> bool {
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    poll(&mut p, ms) == 1 && p[0].revents & POLLIN != 0
}

fn udp_loopback() {
    let a = sock(AF_INET, SOCK_DGRAM, 0);
    let b = sock(AF_INET, SOCK_DGRAM, 0);
    check_ok!(bind(a, &sin(LOOP4, 0)), "udp_bind");
    let pa = local_port(a);
    // 64 datagrams queue up and come back in order, each whole.
    let mut buf = [0u8; 2048];
    let mut sent = 0;
    for i in 0..64usize {
        for (j, x) in buf[..i * 20 + 1].iter_mut().enumerate() {
            *x = (i + j) as u8;
        }
        if sendto(b, &buf[..i * 20 + 1], &sin(LOOP4, pa)) == (i * 20 + 1) as isize {
            sent += 1;
        }
    }
    check_eq!(sent, 64, "udp_send_64");
    let mut n = 0i32;
    ioctl(a, FIONREAD, &mut n as *mut i32 as usize);
    check_eq!(n, 1, "udp_fionread_first");
    let mut good = 0;
    let mut from = [0u8; 16];
    for i in 0..64usize {
        let r = recvfrom(a, &mut buf, MSG_DONTWAIT, &mut from);
        if r == (i * 20 + 1) as isize && buf[..r as usize].iter().enumerate().all(|(j, &x)| x == (i + j) as u8) {
            good += 1;
        }
    }
    check_eq!(good, 64, "udp_recv_64_in_order");
    check_err!(recvfrom(a, &mut buf, MSG_DONTWAIT, &mut from), EAGAIN, "udp_drained");
    check!(!readable(a, 0), "udp_not_readable");

    // Large datagrams, then truncation.
    let big = [0xa5u8; 60000];
    check_eq!(sendto(b, &big, &sin(LOOP4, pa)), 60000, "udp_send_60000");
    let mut rb = [0u8; 65536];
    check!(readable(a, 2000), "udp_big_readable");
    let r = recvfrom(a, &mut rb, 0, &mut from);
    check!(r == 60000 && rb[..60000].iter().all(|&x| x == 0xa5), "udp_recv_60000", "{r}");
    sendto(b, &big[..3000], &sin(LOOP4, pa));
    let mut small = [0u8; 100];
    check_eq!(recvfrom(a, &mut small, MSG_TRUNC, &mut from), 3000, "udp_msg_trunc");
    check_err!(sendto(b, b"x", &sin(LOOP4, 0)), EINVAL, "udp_send_port_0");

    // A connected socket only hears from its peer.
    let c = sock(AF_INET, SOCK_DGRAM, 0);
    bind(c, &sin(LOOP4, 0));
    let pc = local_port(c);
    check_ok!(connect(c, &sin(LOOP4, pa)), "udp_connect");
    sendto(b, b"stranger", &sin(LOOP4, pc));
    sendto(a, b"friend", &sin(LOOP4, pc));
    check!(readable(c, 2000), "udp_connected_readable");
    let r = recv(c, &mut buf, MSG_DONTWAIT);
    check!(r == 6 && &buf[..6] == b"friend", "udp_connected_filters", "{r}");
    check_err!(recv(c, &mut buf, MSG_DONTWAIT), EAGAIN, "udp_stranger_dropped");

    // SO_RCVTIMEO: a blocking receive gives up.
    let tv = [0i64, 150_000];
    setsockopt(c, SOL_SOCKET, SO_RCVTIMEO, tv.as_ptr() as *const u8, 16);
    let t0 = now_ns();
    check_err!(recv(c, &mut buf, 0), EAGAIN, "udp_rcvtimeo");
    let dt = (now_ns() - t0) / 1_000_000;
    check!((100..2000).contains(&dt), "udp_rcvtimeo_elapsed", "{dt} ms");
    close(c);
    close(a);
    close(b);

    // IPv6: a big datagram over ::1 and the source address in recvmsg.
    let a6 = sock(AF_INET6, SOCK_DGRAM, 0);
    let b6 = sock(AF_INET6, SOCK_DGRAM, 0);
    check_ok!(bind(a6, &sin6(LOOP6, 0)), "udp6_bind");
    let p6 = local_port(a6);
    check_eq!(sendto(b6, &big, &sin6(LOOP6, p6)), 60000, "udp6_send_60000");
    let mut name = [0u8; 28];
    let mut iov = Iovec { base: rb.as_mut_ptr(), len: rb.len() };
    let mut m = Msghdr { name: name.as_mut_ptr(), namelen: 28, iov: &mut iov, iovlen: 1, control: core::ptr::null_mut(), controllen: 0, flags: 0 };
    check!(readable(a6, 2000), "udp6_readable");
    let r = recvmsg(a6, &mut m, 0);
    check!(r == 60000 && m.namelen == 28 && name[8..24] == LOOP6 && u16::from_be_bytes([name[2], name[3]]) == local_port(b6), "udp6_recvmsg_source", "{r}");
    close(a6);
    close(b6);
}

/// One echo request on a ping socket; true if the matching reply came.
fn ping(v6: bool, dst: &[u8], seq: u16, payload: &[u8], ms: i32) -> bool {
    let fd = if v6 { sock(AF_INET6, SOCK_DGRAM, IPPROTO_ICMPV6) } else { sock(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) };
    if fd < 0 {
        return false;
    }
    let mut req = [0u8; 64];
    req[0] = if v6 { 128 } else { 8 };
    req[6..8].copy_from_slice(&seq.to_be_bytes());
    req[8..8 + payload.len()].copy_from_slice(payload);
    let len = 8 + payload.len();
    let ok = sendto(fd, &req[..len], dst) == len as isize && {
        let mut got = false;
        let deadline = now_ns() + ms as i64 * 1_000_000;
        while !got && now_ns() < deadline {
            if !readable(fd, 200) {
                continue;
            }
            let mut b = [0u8; 128];
            let mut from = [0u8; 28];
            let r = recvfrom(fd, &mut b, MSG_DONTWAIT, &mut from);
            got = r == len as isize && b[0] == if v6 { 129 } else { 0 } && b[6..8] == seq.to_be_bytes() && &b[8..len] == payload;
        }
        got
    };
    close(fd);
    ok
}

fn icmp_loopback() {
    let probe = sock(AF_INET, SOCK_DGRAM, IPPROTO_ICMP);
    if probe < 0 {
        check!(!is_rust_kernel(), "ping_socket", "{}", errname(probe as isize));
        skip("ping", "no ping sockets (net.ipv4.ping_group_range)");
        return;
    }
    // Only echo requests may be sent.
    let mut bad = [0u8; 8];
    bad[0] = 13;
    check_err!(sendto(probe, &bad, &sin(LOOP4, 0)), EINVAL, "ping_socket_not_echo");
    close(probe);
    check!(ping(false, &sin(LOOP4, 0), 1, b"d2-ping-4", 2000), "ping_127_0_0_1");
    check!(ping(true, &sin6(LOOP6, 0), 2, b"d2-ping-6", 2000), "ping6_loopback");
}

/// A DNS query for "localhost" A to `server`; true if an answer with
/// our id came back (any rcode: it shows the resolver was reached).
fn dns(server: &[u8], v6: bool) -> bool {
    let fd = sock(if v6 { AF_INET6 } else { AF_INET }, SOCK_DGRAM, 0);
    let q: [u8; 27] = [
        0xd2, 0x42, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 9, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 0, 0, 1, 0, 1,
    ];
    let mut ok = false;
    for _ in 0..3 {
        sendto(fd, &q, server);
        if readable(fd, 2000) {
            let mut b = [0u8; 512];
            let r = recv(fd, &mut b, MSG_DONTWAIT);
            ok = r >= 12 && b[0] == 0xd2 && b[1] == 0x42 && b[2] & 0x80 != 0;
            break;
        }
    }
    close(fd);
    ok
}

fn file_has(path: &core::ffi::CStr, needle: &[u8]) -> bool {
    let mut b = [0u8; 8192];
    let n = read_file(path.as_ptr() as *const u8, &mut b);
    n > 0 && b[..n as usize].windows(needle.len()).any(|w| w == needle)
}

fn ifreq(name: &[u8]) -> [u8; 40] {
    let mut r = [0u8; 40];
    r[..name.len()].copy_from_slice(name);
    r
}

fn ethtool(fd: i32, ifname: &[u8], data: &mut [u8]) -> isize {
    let mut r = ifreq(ifname);
    r[16..24].copy_from_slice(&(data.as_mut_ptr() as u64).to_le_bytes());
    ioctl(fd, SIOCETHTOOL, r.as_mut_ptr() as usize)
}

fn eth0() {
    // IPv4 through slirp.
    check!(ping(false, &sin([10, 0, 2, 2], 0), 3, b"gateway", 3000), "ping_gateway");
    check!(dns(&sin([10, 0, 2, 3], 53), false), "dns_via_slirp");
    let u = sock(AF_INET, SOCK_DGRAM, 0);
    check_eq!(sendto(u, &[1u8; 4000], &sin([10, 0, 2, 2], 9)), 4000, "udp_fragmented_send");
    close(u);

    // IPv6 from the router advertisement (sent when eth0 was set up).
    let mut global = false;
    for _ in 0..50 {
        if file_has(c"/proc/net/if_inet6", b"fec0000000000000") {
            global = true;
            break;
        }
        sleep_ms(100);
    }
    check!(global, "slaac_fec0_address");
    check!(file_has(c"/proc/net/if_inet6", b"fe80000000000000"), "link_local_address");
    check!(file_has(c"/proc/net/ipv6_route", b"00000000000000000000000000000000 00 "), "ipv6_default_route");
    let gw6 = [0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
    check!(ping(true, &sin6(gw6, 0), 4, b"gateway6", 3000), "ping6_gateway");
    let t = sock(AF_INET6, SOCK_STREAM, 0);
    check_err!(connect(t, &sin6(gw6, 1)), ECONNREFUSED, "tcp6_to_gateway_refused");
    close(t);

    // ethtool.
    let fd = sock(AF_INET, SOCK_DGRAM, 0);
    let mut d = [0u8; 196];
    d[..4].copy_from_slice(&3u32.to_le_bytes());
    check_ok!(ethtool(fd, b"eth0", &mut d), "ethtool_gdrvinfo");
    check!(cfield(&d[4..36]) == b"virtio_net" && cfield(&d[100..132]) == b"0000:00:03.0", "ethtool_drvinfo_values", "{} {}", Bytes(cfield(&d[4..36])), Bytes(cfield(&d[100..132])));
    let mut v = [0u8; 8];
    v[..4].copy_from_slice(&0xau32.to_le_bytes());
    check!(ethtool(fd, b"eth0", &mut v) == 0 && v[4] == 1, "ethtool_glink");
    let mut pa = [0u8; 16];
    pa[..4].copy_from_slice(&0x20u32.to_le_bytes());
    pa[4..8].copy_from_slice(&8u32.to_le_bytes());
    let mut hw = ifreq(b"eth0");
    ioctl(fd, SIOCGIFHWADDR, hw.as_mut_ptr() as usize);
    check!(ethtool(fd, b"eth0", &mut pa) == 0 && pa[4] == 6 && pa[8..14] == hw[18..24], "ethtool_permaddr");
    let mut c = [0u8; 44];
    c[..4].copy_from_slice(&1u32.to_le_bytes());
    check_ok!(ethtool(fd, b"eth0", &mut c), "ethtool_gset");
    d[..4].copy_from_slice(&3u32.to_le_bytes());
    check_err!(ethtool(fd, b"lo", &mut d), EOPNOTSUPP, "ethtool_lo_drvinfo");
    check_err!(ethtool(fd, b"nosuch0", &mut d), ENODEV, "ethtool_no_device");
    close(fd);

    // /proc/net statistics.
    check!(file_has(c"/proc/net/snmp", b"Ip: Forwarding"), "proc_net_snmp");
    check!(file_has(c"/proc/net/sockstat", b"TCP: inuse"), "proc_net_sockstat");
    check!(file_has(c"/proc/net/dev", b"eth0:"), "proc_net_dev");
    check!(file_has(c"/proc/net/protocols", b"TCP"), "proc_net_protocols");
}

fn main() {
    udp_loopback();
    icmp_loopback();
    if is_rust_kernel() {
        eth0();
    } else {
        skip("eth0", "the host's network differs");
    }
}
