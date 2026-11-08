//! B4: AF_UNIX sockets — socketpair (stream/dgram/seqpacket), message
//! boundaries, MSG_PEEK/MSG_TRUNC, shutdown, EPIPE, bind/listen/connect/accept
//! on filesystem and abstract names, SO_PEERCRED, SCM_RIGHTS (fd passing),
//! SCM_CREDENTIALS. This is what Android's init, logd, property service,
//! zygote and adbd rely on.
#![no_std]
#![no_main]

use core::mem::size_of;
use rt::*;

rt::test_main!("b4_unix", main);

/// Control buffer large enough for a few fds; 8-byte aligned.
#[repr(C, align(8))]
struct Cmsg([u8; 64]);

const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000;
const CMSG_HDR: usize = size_of::<Cmsghdr>(); // 16

fn send_fds(sock: i32, data: &[u8], fds: &[i32]) -> isize {
    let mut cbuf = Cmsg([0; 64]);
    let payload = fds.len() * 4;
    unsafe {
        let h = cbuf.0.as_mut_ptr() as *mut Cmsghdr;
        (*h).len = CMSG_HDR + payload;
        (*h).level = SOL_SOCKET;
        (*h).ty = SCM_RIGHTS;
        for (i, &fd) in fds.iter().enumerate() {
            (cbuf.0.as_mut_ptr().add(CMSG_HDR + i * 4) as *mut i32).write_unaligned(fd);
        }
    }
    let mut iov = Iovec { base: data.as_ptr() as *mut u8, len: data.len() };
    let m = Msghdr {
        name: core::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: cbuf.0.as_mut_ptr(),
        controllen: (CMSG_HDR + payload + 7) & !7,
        flags: 0,
    };
    sendmsg(sock, &m, 0)
}

/// Receive data plus any SCM_RIGHTS fds; returns (bytes, fds, nfds, msg_flags).
fn recv_fds(sock: i32, data: &mut [u8], ctl_len: usize) -> (isize, [i32; 8], usize, i32) {
    let mut cbuf = Cmsg([0; 64]);
    let mut iov = Iovec { base: data.as_mut_ptr(), len: data.len() };
    let mut m = Msghdr {
        name: core::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: cbuf.0.as_mut_ptr(),
        controllen: ctl_len,
        flags: 0,
    };
    let r = recvmsg(sock, &mut m, MSG_CMSG_CLOEXEC);
    let mut out = [-1i32; 8];
    let mut n = 0;
    if r >= 0 && m.controllen >= CMSG_HDR {
        let h = cbuf.0.as_ptr() as *const Cmsghdr;
        unsafe {
            if (*h).level == SOL_SOCKET && (*h).ty == SCM_RIGHTS {
                n = ((*h).len - CMSG_HDR) / 4;
                for i in 0..n.min(8) {
                    out[i] = (cbuf.0.as_ptr().add(CMSG_HDR + i * 4) as *const i32).read_unaligned();
                }
            }
        }
    }
    (r, out, n, m.flags)
}

fn main() {
    let mut buf = [0u8; 256];

    // --- socketpair STREAM
    let mut sv = [0i32; 2];
    check_ok!(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, &mut sv), "socketpair_stream");
    let mut st = Stat::default();
    fstat(sv[0], &mut st);
    check_eq!(st.mode & S_IFMT, S_IFSOCK, "fstat_is_sock");
    let mut ty = 0i32;
    let mut len = 4u32;
    getsockopt(sv[0], SOL_SOCKET, SO_TYPE, &mut ty as *mut i32 as *mut u8, &mut len);
    check_eq!(ty, SOCK_STREAM, "so_type_stream");
    check_eq!(write(sv[0], b"hello"), 5, "stream_write");
    check_eq!(write(sv[0], b"world"), 5, "stream_write2");
    // Stream: no boundaries, both writes coalesce.
    check_eq!(recv(sv[1], &mut buf[..3], MSG_PEEK), 3, "stream_peek");
    check_eq!(read(sv[1], &mut buf), 10, "stream_read_coalesced");
    check_eq!(&buf[..10], b"helloworld", "stream_content");
    // Bidirectional.
    write(sv[1], b"back");
    check_eq!(read(sv[0], &mut buf), 4, "stream_bidirectional");
    check_err!(recv(sv[0], &mut buf, MSG_DONTWAIT), EAGAIN, "stream_dontwait_eagain");
    // shutdown(SHUT_WR) -> peer sees EOF but can still send back.
    check_ok!(unsafe { syscall!(nr::SHUTDOWN, sv[0], SHUT_WR) }, "shutdown_wr");
    check_eq!(read(sv[1], &mut buf), 0, "eof_after_shutdown");
    check_eq!(write(sv[1], b"still"), 5, "reverse_still_open");
    check_eq!(read(sv[0], &mut buf), 5, "reverse_read");
    // Closed peer: EPIPE (MSG_NOSIGNAL suppresses SIGPIPE).
    close(sv[0]);
    check_err!(send(sv[1], b"x", MSG_NOSIGNAL), EPIPE, "send_closed_peer_epipe");
    let mut pf = [Pollfd { fd: sv[1], events: POLLIN, revents: 0 }];
    poll(&mut pf, 0);
    check!(pf[0].revents & POLLHUP != 0, "pollhup_closed_peer");
    close(sv[1]);

    // --- SO_PEERCRED on a socketpair is the creator's credentials.
    socketpair(AF_UNIX, SOCK_STREAM, &mut sv);
    let mut cred = Ucred::default();
    let mut len = size_of::<Ucred>() as u32;
    check_ok!(getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut u8, &mut len),
              "so_peercred");
    check_eq!(cred.pid, getpid(), "peercred_pid");
    check_eq!(cred.uid as isize, unsafe { syscall!(nr::GETUID) }, "peercred_uid");

    // --- SCM_RIGHTS: pass a pipe's write end, then use it.
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    check_eq!(send_fds(sv[0], b"F", &[p[1]]), 1, "sendmsg_scm_rights");
    close(p[1]);
    let (r, fds, n, _) = recv_fds(sv[1], &mut buf, 64);
    check!(r == 1 && n == 1 && fds[0] >= 0, "recvmsg_scm_rights", "r={} n={}", r, n);
    check_eq!(fcntl(fds[0], F_GETFD, 0), FD_CLOEXEC as isize, "msg_cmsg_cloexec");
    check_eq!(write(fds[0], b"through-passed-fd"), 17, "write_via_passed_fd");
    check_eq!(read(p[0], &mut buf), 17, "read_from_original_pipe");
    // The passed fd keeps the pipe open: closing it gives EOF.
    close(fds[0]);
    check_eq!(read(p[0], &mut buf), 0, "eof_after_passed_fd_closed");
    close(p[0]);

    // Two fds at once, across processes; child sends us a memfd it created.
    let s1 = sv[1];
    let pid = fork();
    if pid == 0 {
        let m = unsafe { syscall!(nr::MEMFD_CREATE, c"from-child".as_ptr(), 0) } as i32;
        write(m, b"memfd-data");
        let e = unsafe { syscall!(nr::EVENTFD2, 5, 0) } as i32;
        send_fds(s1, b"two", &[m, e]);
        exit(0);
    }
    let (r, fds, n, _) = recv_fds(sv[0], &mut buf, 64);
    check!(r == 3 && n == 2, "recv_two_fds_from_child", "r={} n={}", r, n);
    if n == 2 {
        check_eq!(pread(fds[0], &mut buf[..10], 0), 10, "passed_memfd_readable");
        check_eq!(&buf[..10], b"memfd-data", "passed_memfd_content");
        let mut v = [0u8; 8];
        read(fds[1], &mut v);
        check_eq!(u64::from_ne_bytes(v), 5, "passed_eventfd_value");
        close(fds[0]);
        close(fds[1]);
    }
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // Control buffer too small for the fds: MSG_CTRUNC.
    pipe2(&mut p, 0);
    send_fds(sv[0], b"T", &[p[0], p[1]]);
    let (_, _, _, flags) = recv_fds(sv[1], &mut buf, CMSG_HDR + 4);
    check!(flags & MSG_CTRUNC != 0, "msg_ctrunc");
    close(p[0]);
    close(p[1]);
    close(sv[0]);
    close(sv[1]);

    // --- DGRAM and SEQPACKET keep message boundaries.
    for (ty, name_b, name_t) in [
        (SOCK_DGRAM, "dgram_boundaries", "dgram_msg_trunc"),
        (SOCK_SEQPACKET, "seqpacket_boundaries", "seqpacket_msg_trunc"),
    ] {
        check_ok!(socketpair(AF_UNIX, ty, &mut sv), "socketpair_msg_type");
        write(sv[0], b"one");
        write(sv[0], b"three");
        let a = read(sv[1], &mut buf);
        let b = read(sv[1], &mut buf);
        check!(a == 3 && b == 5, name_b, "a={} b={}", a, b);
        // Truncated read discards the rest; MSG_TRUNC returns the real size.
        write(sv[0], b"0123456789");
        let r = recv(sv[1], &mut buf[..4], MSG_TRUNC);
        check_eq!(r, 10, name_t);
        check_err!(recv(sv[1], &mut buf, MSG_DONTWAIT), EAGAIN, "truncated_rest_discarded");
        close(sv[0]);
        close(sv[1]);
    }

    // --- Named sockets: filesystem path.
    let dir = scratch_dir();
    let path = cpath!("{}/sock", Bytes(dir.as_bytes()));
    let (addr, alen) = SockaddrUn::new(path.as_bytes());
    let srv = check_ok!(socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC), "socket_stream") as i32;
    check_ok!(bind(srv, &addr, alen), "bind_path");
    fstatat(AT_FDCWD, path.as_ptr(), &mut st, 0);
    check_eq!(st.mode & S_IFMT, S_IFSOCK, "bound_path_is_socket");
    let other = socket(AF_UNIX, SOCK_STREAM) as i32;
    check_err!(bind(other, &addr, alen), EADDRINUSE, "bind_existing_eaddrinuse");
    let cli = socket(AF_UNIX, SOCK_STREAM) as i32;
    check_err!(connect(cli, &addr, alen), ECONNREFUSED, "connect_not_listening_refused");
    close(cli);
    check_ok!(listen(srv, 8), "listen");
    fcntl(srv, F_SETFL, O_NONBLOCK as usize);
    check_err!(accept4(srv, 0), EAGAIN, "accept_nonblock_eagain");
    let mut pf = [Pollfd { fd: srv, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pf, 0), 0, "listener_not_ready");
    let cli = socket(AF_UNIX, SOCK_STREAM) as i32;
    check_ok!(connect(cli, &addr, alen), "connect_path");
    check_eq!(poll(&mut pf, 0), 1, "listener_ready_after_connect");
    let conn = check_ok!(accept4(srv, SOCK_CLOEXEC), "accept4") as i32;
    check_eq!(fcntl(conn, F_GETFD, 0), FD_CLOEXEC as isize, "accept4_cloexec");
    write(cli, b"req");
    check_eq!(read(conn, &mut buf), 3, "accepted_conn_reads");
    write(conn, b"resp!");
    check_eq!(read(cli, &mut buf), 5, "client_reads_reply");
    let mut cred = Ucred::default();
    let mut len = size_of::<Ucred>() as u32;
    getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut u8, &mut len);
    check_eq!(cred.pid, getpid(), "accepted_peercred_pid");
    close(conn);
    close(cli);

    // A client in another process: peer credentials name that process.
    fcntl(srv, F_SETFL, 0);
    let pid = fork();
    if pid == 0 {
        let c = socket(AF_UNIX, SOCK_STREAM) as i32;
        if connect(c, &addr, alen) != 0 {
            exit(1);
        }
        write(c, b"hi");
        let mut b = [0u8; 4];
        read(c, &mut b); // wait for server to finish before exiting
        exit(0);
    }
    let conn = accept4(srv, 0) as i32;
    let mut cred = Ucred::default();
    let mut len = size_of::<Ucred>() as u32;
    getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut u8, &mut len);
    check_eq!(cred.pid as isize, pid, "peercred_child_pid");
    check_eq!(read(conn, &mut buf), 2, "child_client_data");
    write(conn, b"ok");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);
    check_eq!(status_code(s), 0, "child_client_ok");
    close(conn);
    close(srv);
    close(other);
    unlinkat(AT_FDCWD, path.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);

    // --- Abstract namespace + SEQPACKET listener (as used by Android's
    // property service / lmkd style sockets).
    let mut name = Path::new();
    name.push(b"\0ktest-abstract-");
    let _ = core::fmt::Write::write_fmt(&mut name, format_args!("{}", getpid()));
    let (aaddr, aalen) = SockaddrUn::new(name.as_bytes());
    let asrv = socket(AF_UNIX, SOCK_SEQPACKET) as i32;
    check_ok!(bind(asrv, &aaddr, aalen), "bind_abstract");
    listen(asrv, 4);
    let acli = socket(AF_UNIX, SOCK_SEQPACKET) as i32;
    check_ok!(connect(acli, &aaddr, aalen), "connect_abstract");
    let aconn = accept4(asrv, 0) as i32;
    write(acli, b"m1");
    write(acli, b"msg2");
    let a = read(aconn, &mut buf);
    let b = read(aconn, &mut buf);
    check!(a == 2 && b == 4, "seqpacket_conn_boundaries");
    close(aconn);
    close(acli);
    close(asrv);

    // --- Unconnected DGRAM with sendto/recvfrom by address.
    let mut dname = Path::new();
    dname.push(b"\0ktest-dgram-");
    let _ = core::fmt::Write::write_fmt(&mut dname, format_args!("{}", getpid()));
    let (daddr, dalen) = SockaddrUn::new(dname.as_bytes());
    let drx = socket(AF_UNIX, SOCK_DGRAM) as i32;
    bind(drx, &daddr, dalen);
    let dtx = socket(AF_UNIX, SOCK_DGRAM) as i32;
    let r = unsafe { syscall!(nr::SENDTO, dtx, b"dg".as_ptr(), 2, 0, &daddr as *const SockaddrUn, dalen) };
    check_eq!(r, 2, "dgram_sendto_address");
    check_eq!(read(drx, &mut buf), 2, "dgram_recv_by_address");

    // SO_PASSCRED: receiver gets SCM_CREDENTIALS with the sender's pid.
    let one = 1i32;
    setsockopt(drx, SOL_SOCKET, SO_PASSCRED, &one as *const i32 as *const u8, 4);
    unsafe { syscall!(nr::SENDTO, dtx, b"c".as_ptr(), 1, 0, &daddr as *const SockaddrUn, dalen) };
    let mut cbuf = Cmsg([0; 64]);
    let mut iov = Iovec { base: buf.as_mut_ptr(), len: buf.len() };
    let mut m = Msghdr {
        name: core::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: cbuf.0.as_mut_ptr(),
        controllen: 64,
        flags: 0,
    };
    recvmsg(drx, &mut m, 0);
    let h = cbuf.0.as_ptr() as *const Cmsghdr;
    let ok = m.controllen >= CMSG_HDR + 12
        && unsafe { (*h).ty == SCM_CREDENTIALS && (cbuf.0.as_ptr().add(CMSG_HDR) as *const i32).read() == getpid() };
    check!(ok, "scm_credentials_pid");
    close(drx);
    close(dtx);

    let lone = socket(AF_UNIX, SOCK_STREAM) as i32;
    check_err!(send(lone, b"x", MSG_NOSIGNAL), ENOTCONN, "send_unconnected_enotconn");
    close(lone);
    let mut q = [0i32; 2];
    pipe2(&mut q, 0);
    check_err!(unsafe { syscall!(nr::SHUTDOWN, q[0], SHUT_RD) }, ENOTSOCK, "shutdown_non_socket_enotsock");
    close(q[0]);
    close(q[1]);
}
