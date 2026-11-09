//! C3: AF_INET / AF_INET6 networking, as adbd, netd and iptables use it.
//! TCP over loopback (blocking and non-blocking connect, accept4, data
//! both ways, 1 MiB through a child process, shutdown/EOF, ECONNREFUSED,
//! EADDRINUSE, getsockname/getpeername, epoll readiness), dual-stack
//! AF_INET6 (v4-mapped peers, ::1), UDP (sendto/recvfrom with source
//! address, connected sockets, MSG_PEEK, MSG_TRUNC, FIONREAD);
//! rtnetlink dumps (links, addresses, routes) and the interface ioctls
//! on lo; on the Rust kernel also eth0 (virtio-net: a TCP connect through
//! QEMU's user network to the gateway), address changes over rtnetlink
//! with their multicast notification, and the iptables/ip6tables socket
//! options (IPT_SO_GET_INFO, GET_ENTRIES, SET_REPLACE, revisions).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_net", main);

const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;
const AF_NETLINK: i32 = 16;
const SOCK_RAW: i32 = 3;
const IPPROTO_RAW: i32 = 255;
const SOL_IP: i32 = 0;
const SOL_IPV6: i32 = 41;
const SO_ERROR: i32 = 4;
const SO_REUSEADDR: i32 = 2;
const MSG_PEEK: i32 = 2;
const MSG_TRUNC: i32 = 0x20;
const MSG_DONTWAIT: i32 = 0x40;
const EINPROGRESS: i32 = 115;
const EADDRNOTAVAIL: i32 = 99;
const FIONREAD: u32 = 0x541b;
const SIOCGIFNAME: u32 = 0x8910;
const SIOCGIFCONF: u32 = 0x8912;
const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCGIFADDR: u32 = 0x8915;
const SIOCGIFNETMASK: u32 = 0x891b;
const SIOCGIFMTU: u32 = 0x8921;
const SIOCGIFHWADDR: u32 = 0x8927;
const SIOCGIFINDEX: u32 = 0x8933;
const IFF_UP: u16 = 1;
const IFF_LOOPBACK: u16 = 8;

const CONNECT: usize = 42;
const ACCEPT4: usize = 288;
const SENDTO: usize = 44;
const RECVFROM: usize = 45;
const SHUTDOWN: usize = 48;
const BIND: usize = 49;
const GETSOCKNAME: usize = 51;
const GETPEERNAME: usize = 52;

fn sock3(domain: i32, ty: i32, proto: i32) -> isize {
    unsafe { syscall!(41, domain, ty, proto) }
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

const LOOP4: [u8; 4] = [127, 0, 0, 1];
const ANY6: [u8; 16] = [0; 16];
const LOOP6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const MAPPED_LOOP4: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 127, 0, 0, 1];

fn bind_raw(fd: i32, a: &[u8]) -> isize {
    unsafe { syscall!(BIND, fd, a.as_ptr(), a.len()) }
}

fn connect_raw(fd: i32, a: &[u8]) -> isize {
    unsafe { syscall!(CONNECT, fd, a.as_ptr(), a.len()) }
}

fn accept(fd: i32, peer: &mut [u8]) -> isize {
    let mut len = peer.len() as u32;
    unsafe { syscall!(ACCEPT4, fd, peer.as_mut_ptr(), &mut len as *mut u32, SOCK_CLOEXEC) }
}

fn name_of(nr: usize, fd: i32) -> ([u8; 28], u32) {
    let mut a = [0u8; 28];
    let mut len = 28u32;
    let r = unsafe { syscall!(nr, fd, a.as_mut_ptr(), &mut len as *mut u32) };
    if r < 0 {
        return ([0; 28], 0);
    }
    (a, len)
}

fn port_of(a: &[u8]) -> u16 {
    u16::from_be_bytes([a[2], a[3]])
}

fn sendto(fd: i32, b: &[u8], to: &[u8]) -> isize {
    unsafe { syscall!(SENDTO, fd, b.as_ptr(), b.len(), 0, to.as_ptr(), to.len()) }
}

fn recvfrom(fd: i32, b: &mut [u8], flags: i32, from: &mut [u8]) -> isize {
    let mut len = from.len() as u32;
    unsafe { syscall!(RECVFROM, fd, b.as_mut_ptr(), b.len(), flags, from.as_mut_ptr(), &mut len as *mut u32) }
}

fn int_opt(fd: i32, level: i32, name: i32) -> i32 {
    let mut v = 0i32;
    let mut len = 4u32;
    getsockopt(fd, level, name, &mut v as *mut i32 as *mut u8, &mut len);
    v
}

fn tcp_loopback() {
    let srv = sock3(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    check!(srv >= 0, "tcp_socket", "{}", errname(srv));
    let srv = srv as i32;
    check_ok!(bind_raw(srv, &sin(LOOP4, 0)), "tcp_bind_ephemeral");
    let (a, alen) = name_of(GETSOCKNAME, srv);
    let port = port_of(&a);
    check!(alen == 16 && port != 0 && a[4..8] == LOOP4, "tcp_getsockname", "len={alen} port={port}");
    check_ok!(listen(srv, 4), "tcp_listen");
    // A second socket cannot take the port.
    let other = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_err!(bind_raw(other, &sin(LOOP4, port)), EADDRINUSE, "tcp_bind_in_use");
    close(other);
    // Nothing pending yet.
    let mut pfd = [Pollfd { fd: srv, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 0), 0, "listen_not_readable");

    let cli = sock3(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0) as i32;
    check_ok!(connect_raw(cli, &sin(LOOP4, port)), "tcp_connect");
    let (ca, _) = name_of(GETSOCKNAME, cli);
    let (cp, _) = name_of(GETPEERNAME, cli);
    check!(ca[4..8] == LOOP4 && port_of(&ca) != 0 && port_of(&ca) != port, "client_local_name");
    check!(cp[4..8] == LOOP4 && port_of(&cp) == port, "client_peer_name");
    let mut pfd = [Pollfd { fd: srv, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 2000), 1, "listen_readable");
    let mut peer = [0u8; 16];
    let c = accept(srv, &mut peer);
    check!(c >= 0, "tcp_accept", "{}", errname(c));
    let c = c as i32;
    check!(peer[4..8] == LOOP4 && port_of(&peer) == port_of(&ca), "accept_peer_addr");

    check_eq!(write(cli, b"hello"), 5, "tcp_write");
    let mut buf = [0u8; 64];
    check_eq!(read(c, &mut buf), 5, "tcp_read");
    check!(&buf[..5] == b"hello", "tcp_data");
    check_eq!(send(c, b"world!", 0), 6, "tcp_send_back");
    let mut pfd = [Pollfd { fd: cli, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 2000), 1, "client_readable");
    let mut n = 0i32;
    ioctl(cli, FIONREAD, &mut n as *mut i32 as usize);
    check_eq!(n, 6, "tcp_fionread");
    check_eq!(recv(cli, &mut buf, MSG_PEEK), 6, "tcp_peek");
    check_eq!(recv(cli, &mut buf, 0), 6, "tcp_recv");
    check!(&buf[..6] == b"world!", "tcp_data_back");
    check_err!(recv(cli, &mut buf, MSG_DONTWAIT), EAGAIN, "tcp_recv_eagain");

    // EOF after shutdown(SHUT_WR).
    check_ok!(unsafe { syscall!(SHUTDOWN, cli, 1) }, "tcp_shutdown_wr");
    let mut pfd = [Pollfd { fd: c, events: POLLIN, revents: 0 }];
    poll(&mut pfd, 2000);
    check_eq!(read(c, &mut buf), 0, "tcp_eof");
    check_eq!(write(c, b"late"), 4, "tcp_half_close_write");
    check_eq!(read(cli, &mut buf), 4, "tcp_half_close_read");
    close(c);
    close(cli);

    // 1 MiB from a child, checked byte by byte.
    let cli = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_ok!(connect_raw(cli, &sin(LOOP4, port)), "bulk_connect");
    let c = accept(srv, &mut peer) as i32;
    let pid = fork();
    if pid == 0 {
        let mut chunk = [0u8; 8192];
        let mut sent = 0usize;
        while sent < 1 << 20 {
            for (i, b) in chunk.iter_mut().enumerate() {
                *b = ((sent + i) % 251) as u8;
            }
            let r = write(cli, &chunk);
            if r <= 0 {
                exit(1);
            }
            sent += r as usize;
        }
        exit(0);
    }
    close(cli);
    let mut got = 0usize;
    let mut good = true;
    let mut big = [0u8; 4096];
    loop {
        let r = read(c, &mut big);
        if r <= 0 {
            break;
        }
        for i in 0..r as usize {
            if big[i] != ((got + i) % 251) as u8 {
                good = false;
            }
        }
        got += r as usize;
    }
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    check!(got == 1 << 20 && good, "bulk_1mib", "got {got} good={good}");
    check!(wifexited(st) && wexitstatus(st) == 0, "bulk_writer_ok");
    close(c);
    close(srv);

    // Refused: nothing listens on the port any more.
    let cli = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_err!(connect_raw(cli, &sin(LOOP4, port)), ECONNREFUSED, "tcp_refused");
    close(cli);
    let cli = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_err!(bind_raw(cli, &sin([192, 0, 2, 77], 0)), EADDRNOTAVAIL, "bind_foreign_addr");
    close(cli);
}

fn tcp_nonblocking() {
    let srv = sock3(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0) as i32;
    bind_raw(srv, &sin(LOOP4, 0));
    listen(srv, 8);
    let (a, _) = name_of(GETSOCKNAME, srv);
    let port = port_of(&a);
    check_err!(accept(srv, &mut [0u8; 16]), EAGAIN, "accept_nonblock_eagain");
    let cli = sock3(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0) as i32;
    let r = connect_raw(cli, &sin(LOOP4, port));
    check!(r == 0 || r == -(EINPROGRESS as isize), "connect_nonblock", "{}", r);
    let mut pfd = [Pollfd { fd: cli, events: POLLOUT, revents: 0 }];
    check_eq!(poll(&mut pfd, 2000), 1, "connect_pollout");
    check_eq!(int_opt(cli, SOL_SOCKET, SO_ERROR), 0, "connect_so_error_0");
    // epoll: the listener reports the pending connection.
    let ep = epoll_create1(EPOLL_CLOEXEC) as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, srv, EPOLLIN, 7);
    let mut evs = [EpollEvent { events: 0, data: 0 }; 4];
    let n = epoll_wait(ep, &mut evs, 2000);
    check!(n == 1 && evs[0].data == 7, "epoll_listen_ready");
    let c = accept(srv, &mut [0u8; 16]);
    check!(c >= 0, "accept_after_epoll");
    let c = c as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, c, EPOLLIN, 9);
    write(cli, b"x");
    let n = epoll_wait(ep, &mut evs, 2000);
    check!(n >= 1 && evs[..n as usize].iter().any(|e| e.data == 9), "epoll_data_ready");
    close(ep);
    close(c);
    close(cli);
    // Refused, seen through SO_ERROR.
    close(srv);
    let cli = sock3(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0) as i32;
    let r = connect_raw(cli, &sin(LOOP4, port));
    if r == -(EINPROGRESS as isize) {
        let mut pfd = [Pollfd { fd: cli, events: POLLOUT, revents: 0 }];
        poll(&mut pfd, 2000);
        check_eq!(int_opt(cli, SOL_SOCKET, SO_ERROR), ECONNREFUSED, "nonblock_refused_so_error");
    } else {
        check_eq!(r, -(ECONNREFUSED as isize), "nonblock_refused_so_error");
    }
    close(cli);
}

fn tcp_dual_stack() {
    let srv = sock3(AF_INET6, SOCK_STREAM, 0);
    check!(srv >= 0, "tcp6_socket", "{}", errname(srv));
    let srv = srv as i32;
    let on = 1i32;
    setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &on as *const i32 as *const u8, 4);
    check_ok!(bind_raw(srv, &sin6(ANY6, 0)), "tcp6_bind_any");
    check_ok!(listen(srv, 4), "tcp6_listen");
    let (a, alen) = name_of(GETSOCKNAME, srv);
    let port = port_of(&a);
    check!(alen == 28 && port != 0, "tcp6_getsockname");
    // An IPv4 client: the server sees a v4-mapped peer.
    let cli = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_ok!(connect_raw(cli, &sin(LOOP4, port)), "v4_to_dual_connect");
    let mut peer = [0u8; 28];
    let c = accept(srv, &mut peer);
    check!(c >= 0 && peer[8..24] == MAPPED_LOOP4, "v4_mapped_peer");
    write(cli, b"m4");
    let mut b = [0u8; 8];
    check_eq!(read(c as i32, &mut b), 2, "v4_mapped_data");
    close(c as i32);
    close(cli);
    // An IPv6 client over ::1.
    let cli = sock3(AF_INET6, SOCK_STREAM, 0) as i32;
    check_ok!(connect_raw(cli, &sin6(LOOP6, port)), "v6_connect_loopback");
    let c = accept(srv, &mut peer);
    check!(c >= 0 && peer[8..24] == LOOP6, "v6_peer");
    let (cp, _) = name_of(GETPEERNAME, cli);
    check!(cp[8..24] == LOOP6 && port_of(&cp) == port, "v6_getpeername");
    write(c as i32, b"six");
    check_eq!(read(cli, &mut b), 3, "v6_data");
    close(c as i32);
    close(cli);
    // An AF_INET6 client to a v4-mapped address.
    let cli = sock3(AF_INET6, SOCK_STREAM, 0) as i32;
    check_ok!(connect_raw(cli, &sin6(MAPPED_LOOP4, port)), "v6_socket_to_v4_mapped");
    close(cli);
    close(srv);
}

fn udp() {
    let a = sock3(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0) as i32;
    let b = sock3(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0) as i32;
    check!(a >= 0 && b >= 0, "udp_sockets");
    check_ok!(bind_raw(a, &sin(LOOP4, 0)), "udp_bind");
    let (aa, _) = name_of(GETSOCKNAME, a);
    let pa = port_of(&aa);
    check_eq!(sendto(b, b"dgram-1", &sin(LOOP4, pa)), 7, "udp_sendto");
    check_eq!(sendto(b, b"dgram-two", &sin(LOOP4, pa)), 9, "udp_sendto_2");
    let (ba, _) = name_of(GETSOCKNAME, b);
    check!(port_of(&ba) != 0, "udp_autobind");
    let mut pfd = [Pollfd { fd: a, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 2000), 1, "udp_readable");
    let mut n = 0i32;
    ioctl(a, FIONREAD, &mut n as *mut i32 as usize);
    check_eq!(n, 7, "udp_fionread_first_dgram");
    let mut buf = [0u8; 64];
    let mut from = [0u8; 16];
    check_eq!(recvfrom(a, &mut buf, MSG_PEEK, &mut from), 7, "udp_peek");
    check_eq!(recvfrom(a, &mut buf, 0, &mut from), 7, "udp_recvfrom");
    check!(&buf[..7] == b"dgram-1" && from[4..8] == LOOP4 && port_of(&from) == port_of(&ba), "udp_source_addr");
    // Truncation: the rest of the datagram is dropped, MSG_TRUNC tells.
    let mut small = [0u8; 4];
    check_eq!(recvfrom(a, &mut small, MSG_TRUNC, &mut from), 9, "udp_msg_trunc_len");
    check_err!(recvfrom(a, &mut buf, MSG_DONTWAIT, &mut from), EAGAIN, "udp_empty");
    // Connected sockets: send/recv without addresses.
    check_ok!(connect_raw(b, &sin(LOOP4, pa)), "udp_connect");
    check_ok!(connect_raw(a, &ba[..16]), "udp_connect_back");
    check_eq!(send(b, b"ping", 0), 4, "udp_send_connected");
    check_eq!(recv(a, &mut buf, 0), 4, "udp_recv_connected");
    check_eq!(send(a, b"pong", 0), 4, "udp_reply");
    check_eq!(recv(b, &mut buf, 0), 4, "udp_reply_recv");
    let (bp, _) = name_of(GETPEERNAME, b);
    check!(port_of(&bp) == pa, "udp_getpeername");
    close(a);
    close(b);
    // Dual-stack UDP: an IPv6 socket receives from an IPv4 sender.
    let s6 = sock3(AF_INET6, SOCK_DGRAM, 0) as i32;
    check_ok!(bind_raw(s6, &sin6(ANY6, 0)), "udp6_bind");
    let (a6, _) = name_of(GETSOCKNAME, s6);
    let s4 = sock3(AF_INET, SOCK_DGRAM, 0) as i32;
    check_eq!(sendto(s4, b"46", &sin(LOOP4, port_of(&a6))), 2, "udp_v4_to_v6");
    let mut from6 = [0u8; 28];
    check_eq!(recvfrom(s6, &mut buf, 0, &mut from6), 2, "udp6_recv");
    check!(from6[8..24] == MAPPED_LOOP4, "udp6_mapped_source");
    close(s4);
    close(s6);
}

// --- rtnetlink ---------------------------------------------------------------

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_GETROUTE: u16 = 26;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;

fn nl_socket(groups: u32) -> i32 {
    let fd = sock3(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, 0) as i32;
    let mut a = [0u8; 12];
    a[..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    a[8..].copy_from_slice(&groups.to_le_bytes());
    bind_raw(fd, &a);
    fd
}

fn nl_request(fd: i32, ty: u16, flags: u16, seq: u32, body: &[u8]) {
    let mut m = [0u8; 256];
    let len = 16 + body.len();
    m[..4].copy_from_slice(&(len as u32).to_le_bytes());
    m[4..6].copy_from_slice(&ty.to_le_bytes());
    m[6..8].copy_from_slice(&(flags | 1).to_le_bytes());
    m[8..12].copy_from_slice(&seq.to_le_bytes());
    m[16..len].copy_from_slice(body);
    let mut k = [0u8; 12];
    k[..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    sendto(fd, &m[..len], &k);
}

/// Every message answering `seq` until NLMSG_DONE / NLMSG_ERROR, passed to
/// `f(type, payload)`. Returns the error code (0 for DONE / ack).
fn nl_collect(fd: i32, seq: u32, mut f: impl FnMut(u16, &[u8])) -> i32 {
    let mut buf = [0u8; 16384];
    for _ in 0..64 {
        let mut pfd = [Pollfd { fd, events: POLLIN, revents: 0 }];
        if poll(&mut pfd, 2000) != 1 {
            return -1000;
        }
        let n = recv(fd, &mut buf, 0);
        if n <= 0 {
            return -1001;
        }
        let mut off = 0;
        while off + 16 <= n as usize {
            let len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            let ty = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
            let s = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
            if len < 16 || off + len > n as usize {
                return -1002;
            }
            if s == seq {
                match ty {
                    NLMSG_DONE => return 0,
                    NLMSG_ERROR => return i32::from_le_bytes(buf[off + 16..off + 20].try_into().unwrap()),
                    _ => f(ty, &buf[off + 16..off + len]),
                }
            }
            off += (len + 3) & !3;
        }
    }
    -1003
}

fn attr<'a>(b: &'a [u8], want: u16) -> Option<&'a [u8]> {
    let mut off = 0;
    while off + 4 <= b.len() {
        let len = u16::from_le_bytes([b[off], b[off + 1]]) as usize;
        let ty = u16::from_le_bytes([b[off + 2], b[off + 3]]);
        if len < 4 || off + len > b.len() {
            return None;
        }
        if ty == want {
            return Some(&b[off + 4..off + len]);
        }
        off += (len + 3) & !3;
    }
    None
}

/// Index of lo from an RTM_GETLINK dump (0 if absent); also whether eth0
/// was listed.
fn rtnl_links() -> (i32, bool) {
    let fd = nl_socket(0);
    nl_request(fd, RTM_GETLINK, 0x300, 11, &[0u8; 16]);
    let mut lo = 0;
    let mut lo_flags = 0u32;
    let mut eth0 = false;
    let mut count = 0;
    let r = nl_collect(fd, 11, |ty, p| {
        if ty != RTM_NEWLINK || p.len() < 16 {
            return;
        }
        count += 1;
        let idx = i32::from_le_bytes(p[4..8].try_into().unwrap());
        let flags = u32::from_le_bytes(p[8..12].try_into().unwrap());
        match attr(&p[16..], 3).map(cfield) {
            Some(b"lo") => {
                lo = idx;
                lo_flags = flags;
            }
            Some(b"eth0") => eth0 = true,
            _ => {}
        }
    });
    check_eq!(r, 0, "rtnl_getlink_done");
    check!(lo > 0 && lo_flags & (IFF_UP | IFF_LOOPBACK) as u32 == (IFF_UP | IFF_LOOPBACK) as u32, "rtnl_link_lo",
        "lo={lo} flags={lo_flags:#x} links={count}");
    // A single link by index.
    let mut ifi = [0u8; 16];
    ifi[4..8].copy_from_slice(&lo.to_le_bytes());
    nl_request(fd, RTM_GETLINK, 0, 12, &ifi);
    let mut name_ok = false;
    let mut buf = [0u8; 4096];
    let n = recv(fd, &mut buf, 0);
    if n > 32 && u16::from_le_bytes([buf[4], buf[5]]) == RTM_NEWLINK {
        name_ok = attr(&buf[32..n as usize], 3).map(cfield) == Some(b"lo");
    }
    check!(name_ok, "rtnl_getlink_by_index");
    close(fd);
    (lo, eth0)
}

fn rtnl_addrs(lo: i32) {
    let fd = nl_socket(0);
    nl_request(fd, RTM_GETADDR, 0x300, 21, &[AF_INET as u8, 0, 0, 0, 0, 0, 0, 0]);
    let mut found = false;
    let mut v6 = false;
    let r = nl_collect(fd, 21, |ty, p| {
        if ty == RTM_NEWADDR && p.len() >= 8 {
            if p[0] as i32 == AF_INET6 {
                v6 = true;
            }
            let idx = i32::from_le_bytes(p[4..8].try_into().unwrap());
            if idx == lo && p[1] == 8 && attr(&p[8..], 2) == Some(&LOOP4[..]) {
                found = true;
            }
        }
    });
    check_eq!(r, 0, "rtnl_getaddr_done");
    check!(found && !v6, "rtnl_addr_127_0_0_1_family_filtered");
    nl_request(fd, RTM_GETROUTE, 0x300, 22, &[AF_INET as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut routes = 0;
    let r = nl_collect(fd, 22, |ty, _| {
        if ty == RTM_NEWROUTE {
            routes += 1;
        }
    });
    check!(r == 0 && routes > 0, "rtnl_getroute_dump", "r={r} routes={routes}");
    close(fd);
}

fn rtnl_change_addr(lo: i32) {
    // Listen for address notifications (RTMGRP_IPV4_IFADDR).
    let mon = nl_socket(0x10);
    let fd = nl_socket(0);
    let mut body = [0u8; 16];
    body[0] = AF_INET as u8;
    body[1] = 8;
    body[4..8].copy_from_slice(&lo.to_le_bytes());
    body[8..10].copy_from_slice(&8u16.to_le_bytes());
    body[10..12].copy_from_slice(&2u16.to_le_bytes()); // IFA_LOCAL
    body[12..16].copy_from_slice(&[127, 0, 0, 9]);
    nl_request(fd, RTM_NEWADDR, 0x4 | 0x400, 31, &body); // ACK | CREATE
    check_eq!(nl_collect(fd, 31, |_, _| {}), 0, "rtnl_newaddr_ack");
    let mut seen = false;
    let mut buf = [0u8; 4096];
    let mut pfd = [Pollfd { fd: mon, events: POLLIN, revents: 0 }];
    if poll(&mut pfd, 2000) == 1 {
        let n = recv(mon, &mut buf, 0);
        seen = n > 24 && u16::from_le_bytes([buf[4], buf[5]]) == RTM_NEWADDR && attr(&buf[24..n as usize], 2) == Some(&[127, 0, 0, 9][..]);
    }
    check!(seen, "rtnl_newaddr_multicast");
    nl_request(fd, RTM_DELADDR, 0x4, 32, &body);
    check_eq!(nl_collect(fd, 32, |_, _| {}), 0, "rtnl_deladdr_ack");
    nl_request(fd, RTM_DELADDR, 0x4, 33, &body);
    check_eq!(nl_collect(fd, 33, |_, _| {}), -EADDRNOTAVAIL, "rtnl_deladdr_missing");
    // Renaming a link that is up fails (Cuttlefish's rename_netiface
    // asks for eth0 -> buried_eth0 together with "down").
    let mut ifi = [0u8; 28];
    ifi[4..8].copy_from_slice(&lo.to_le_bytes());
    ifi[12..16].copy_from_slice(&1u32.to_le_bytes()); // ifi_change = IFF_UP, flags 0
    ifi[16..18].copy_from_slice(&12u16.to_le_bytes());
    ifi[18..20].copy_from_slice(&3u16.to_le_bytes()); // IFLA_IFNAME
    ifi[20..27].copy_from_slice(b"buried\0");
    nl_request(fd, 19, 0x4, 34, &ifi); // RTM_SETLINK
    check_eq!(nl_collect(fd, 34, |_, _| {}), -EBUSY, "rtnl_rename_up_link_ebusy");
    let mut r = ifreq(b"lo");
    let s4 = sock3(AF_INET, SOCK_DGRAM, 0) as i32;
    ioctl(s4, SIOCGIFFLAGS, r.as_mut_ptr() as usize);
    check!(u16::from_le_bytes([r[16], r[17]]) & IFF_UP != 0, "rtnl_rename_failed_link_still_up");
    close(s4);
    close(fd);
    close(mon);
}

fn ifreq(name: &[u8]) -> [u8; 40] {
    let mut r = [0u8; 40];
    r[..name.len()].copy_from_slice(name);
    r
}

fn ioctls(lo: i32) {
    let fd = sock3(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0) as i32;
    let mut r = ifreq(b"lo");
    check_ok!(ioctl(fd, SIOCGIFINDEX, r.as_mut_ptr() as usize), "siocgifindex");
    check_eq!(i32::from_le_bytes(r[16..20].try_into().unwrap()), lo, "ifindex_matches_netlink");
    let mut r = ifreq(b"");
    r[16..20].copy_from_slice(&lo.to_le_bytes());
    check_ok!(ioctl(fd, SIOCGIFNAME, r.as_mut_ptr() as usize), "siocgifname");
    check!(cfield(&r[..16]) == b"lo", "ifname_lo");
    let mut r = ifreq(b"lo");
    check_ok!(ioctl(fd, SIOCGIFFLAGS, r.as_mut_ptr() as usize), "siocgifflags");
    let fl = u16::from_le_bytes([r[16], r[17]]);
    check!(fl & IFF_UP != 0 && fl & IFF_LOOPBACK != 0, "lo_flags_up_loopback", "{fl:#x}");
    let mut r = ifreq(b"lo");
    check_ok!(ioctl(fd, SIOCGIFADDR, r.as_mut_ptr() as usize), "siocgifaddr");
    check!(r[20..24] == LOOP4, "lo_addr");
    let mut r = ifreq(b"lo");
    ioctl(fd, SIOCGIFNETMASK, r.as_mut_ptr() as usize);
    check!(r[20..24] == [255, 0, 0, 0], "lo_netmask");
    let mut r = ifreq(b"lo");
    check_ok!(ioctl(fd, SIOCGIFMTU, r.as_mut_ptr() as usize), "siocgifmtu");
    check!(i32::from_le_bytes(r[16..20].try_into().unwrap()) >= 1500, "lo_mtu");
    let mut r = ifreq(b"lo");
    check_ok!(ioctl(fd, SIOCGIFHWADDR, r.as_mut_ptr() as usize), "siocgifhwaddr");
    check_eq!(u16::from_le_bytes([r[16], r[17]]), 772, "lo_arphrd_loopback");
    let mut r = ifreq(b"nosuch0");
    check_err!(ioctl(fd, SIOCGIFINDEX, r.as_mut_ptr() as usize), ENODEV, "unknown_if_enodev");
    // SIOCGIFCONF lists lo's address.
    let mut reqs = [0u8; 40 * 8];
    let mut conf = [0u8; 16];
    conf[..4].copy_from_slice(&(reqs.len() as i32).to_le_bytes());
    conf[8..16].copy_from_slice(&(reqs.as_mut_ptr() as u64).to_le_bytes());
    check_ok!(ioctl(fd, SIOCGIFCONF, conf.as_mut_ptr() as usize), "siocgifconf");
    let len = i32::from_le_bytes(conf[..4].try_into().unwrap()) as usize;
    check!(len >= 40 && len % 40 == 0 && reqs[..len].chunks(40).any(|c| cfield(&c[..16]) == b"lo" && c[20..24] == LOOP4),
        "ifconf_lists_lo", "len={len}");
    close(fd);
}

fn eth0() {
    let fd = sock3(AF_INET, SOCK_DGRAM, 0) as i32;
    let mut r = ifreq(b"eth0");
    check_ok!(ioctl(fd, SIOCGIFADDR, r.as_mut_ptr() as usize), "eth0_addr_ioctl");
    check!(r[20..24] == [10, 0, 2, 15], "eth0_qemu_user_addr");
    close(fd);
    // Through virtio-net and ARP to QEMU's gateway, which forwards to the
    // host's loopback: port 1 is closed there, so the answer is a reset.
    let c = sock3(AF_INET, SOCK_STREAM, 0) as i32;
    check_err!(connect_raw(c, &sin([10, 0, 2, 2], 1)), ECONNREFUSED, "eth0_tcp_to_gateway_refused");
    close(c);
    let mut b = [0u8; 2048];
    let n = read_file(c"/proc/net/arp".as_ptr() as *const u8, &mut b);
    check!(n > 0 && b[..n as usize].windows(9).any(|w| w == b"10.0.2.2 "), "eth0_arp_entry");
    let n = read_file(c"/proc/net/dev".as_ptr() as *const u8, &mut b);
    check!(n > 0 && b[..n as usize].windows(5).any(|w| w == b"eth0:"), "proc_net_dev_eth0");
}

// --- iptables ----------------------------------------------------------------

const IPT_SO_SET_REPLACE: i32 = 64;
const IPT_SO_GET_INFO: i32 = 64;
const IPT_SO_GET_ENTRIES: i32 = 65;
const IPT_SO_GET_REVISION_MATCH: i32 = 66;

/// struct ipt_getinfo for `table`; (result, info bytes).
fn get_info(fd: i32, level: i32, table: &[u8]) -> (isize, [u8; 84]) {
    let mut info = [0u8; 84];
    info[..table.len()].copy_from_slice(table);
    let mut len = 84u32;
    let r = getsockopt(fd, level, IPT_SO_GET_INFO, info.as_mut_ptr(), &mut len);
    (r, info)
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn iptables(fd: i32, level: i32, v6: bool, tag: &str) {
    let entry = if v6 { 168 } else { 112 };
    let (r, info) = get_info(fd, level, b"filter");
    check!(r == 0, "ipt_get_info", "{tag}: {}", errname(r));
    let hooks = u32_at(&info, 32);
    let num = u32_at(&info, 76);
    let size = u32_at(&info, 80) as usize;
    // INPUT, FORWARD, OUTPUT with ACCEPT policies, then the ERROR entry.
    check!(hooks == 0b01110 && num == 4 && size == 3 * (entry + 40) + entry + 64, "ipt_filter_builtin",
        "{tag}: hooks={hooks:#x} num={num} size={size}");
    check!(u32_at(&info, 40) == 0 && u32_at(&info, 44) == (entry + 40) as u32, "ipt_hook_entry_forward", "{tag}");
    let (r, _) = get_info(fd, level, b"nosuchtable");
    check_eq!(r, -(ENOENT as isize), "ipt_unknown_table");

    // GET_ENTRIES, then SET_REPLACE with the same rules.
    let total = 40 + size;
    let buf = alloc_pages(65536);
    let ge = unsafe { core::slice::from_raw_parts_mut(buf, total) };
    ge[..6].copy_from_slice(b"filter");
    ge[32..36].copy_from_slice(&(size as u32).to_le_bytes());
    let mut len = total as u32;
    check_ok!(getsockopt(fd, level, IPT_SO_GET_ENTRIES, ge.as_mut_ptr(), &mut len), "ipt_get_entries");
    let blob_off = 40;
    let target_off = u16::from_le_bytes([ge[blob_off + if v6 { 140 } else { 88 }], ge[blob_off + if v6 { 141 } else { 89 }]]);
    check_eq!(target_off as usize, entry, "ipt_entry_target_offset");
    // struct ipt_replace + entries; old counters go to `counters`.
    let rep = unsafe { core::slice::from_raw_parts_mut(buf.add(32768), 96 + size) };
    let mut counters = [0xffu8; 16 * 4];
    rep[..6].copy_from_slice(b"filter");
    rep[32..36].copy_from_slice(&hooks.to_le_bytes());
    rep[36..40].copy_from_slice(&num.to_le_bytes());
    rep[40..44].copy_from_slice(&(size as u32).to_le_bytes());
    rep[44..64].copy_from_slice(&info[36..56]);
    rep[64..84].copy_from_slice(&info[56..76]);
    rep[84..88].copy_from_slice(&num.to_le_bytes());
    rep[88..96].copy_from_slice(&(counters.as_mut_ptr() as u64).to_le_bytes());
    rep[96..].copy_from_slice(&ge[40..40 + size]);
    check_ok!(setsockopt(fd, level, IPT_SO_SET_REPLACE, rep.as_ptr(), rep.len() as u32), "ipt_set_replace");
    check!(counters.iter().all(|&b| b == 0), "ipt_old_counters_returned");
    let (r, info2) = get_info(fd, level, b"filter");
    check!(r == 0 && info2[32..84] == info[32..84], "ipt_table_after_replace");
    // Extensions: "comment" match revision 0.
    let mut rev = [0u8; 30];
    rev[..7].copy_from_slice(b"comment");
    let mut len = 30u32;
    check!(getsockopt(fd, level, IPT_SO_GET_REVISION_MATCH, rev.as_mut_ptr(), &mut len) >= 0, "ipt_revision_match", "{tag}");
    munmap(buf, 65536);
}

fn netfilter() {
    let fd = sock3(AF_INET, SOCK_RAW | SOCK_CLOEXEC, IPPROTO_RAW);
    if fd == -(EPERM as isize) {
        skip("iptables", "raw sockets need CAP_NET_RAW");
        return;
    }
    check!(fd >= 0, "raw_socket", "{}", errname(fd));
    iptables(fd as i32, SOL_IP, false, "ipv4");
    close(fd as i32);
    let fd = sock3(AF_INET6, SOCK_RAW | SOCK_CLOEXEC, IPPROTO_RAW);
    check!(fd >= 0, "raw6_socket", "{}", errname(fd));
    iptables(fd as i32, SOL_IPV6, true, "ipv6");
    close(fd as i32);
}

fn main() {
    tcp_loopback();
    tcp_nonblocking();
    tcp_dual_stack();
    udp();
    let (lo, has_eth0) = rtnl_links();
    rtnl_addrs(lo);
    ioctls(lo);
    let mut b = [0u8; 4096];
    let n = read_file(c"/proc/net/dev".as_ptr() as *const u8, &mut b);
    check!(n > 0 && b[..n as usize].windows(3).any(|w| w == b"lo:"), "proc_net_dev_lo");
    if !is_rust_kernel() {
        skip("rtnl_change_addr", "needs CAP_NET_ADMIN on Linux");
        skip("eth0", "the host's interfaces differ");
        skip("iptables", "needs CAP_NET_ADMIN on Linux");
        return;
    }
    rtnl_change_addr(lo);
    if has_eth0 {
        eth0();
    } else {
        skip("eth0", "no virtio-net card");
    }
    netfilter();
}
