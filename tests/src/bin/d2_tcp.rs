//! D2: TCP beyond the basics (c3_net): 8 MiB each way at once over
//! 127.0.0.1 and 2 MiB over ::1 (checked byte by byte), backpressure on
//! a non-blocking sender (EAGAIN, POLLOUT off, back on once the reader
//! drains), MSG_WAITALL across slow writes, shutdown semantics (SHUT_WR:
//! peer EOF + POLLRDHUP, EPIPE after it, half-close the other way;
//! SHUT_RD: reads end), resets (SO_LINGER 0 and close with unread data:
//! the peer gets ECONNRESET once, then EOF / EPIPE; SO_ERROR and
//! POLLERR|POLLHUP when only polled; data to a closed peer is answered
//! with a reset), and 64 simultaneous non-blocking connects to one
//! listener, all accepted and served.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("d2_tcp", main);

const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;
const SOL_SOCKET: i32 = 1;
const SO_ERROR: i32 = 4;
const SO_LINGER: i32 = 13;
const MSG_WAITALL: i32 = 0x100;
const MSG_DONTWAIT: i32 = 0x40;
const EINPROGRESS: i32 = 115;
const ECONNRESET: i32 = 104;
const POLLRDHUP: i16 = 0x2000;

const LOOP4: [u8; 4] = [127, 0, 0, 1];
const LOOP6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn sock(domain: i32, ty: i32) -> i32 {
    (unsafe { syscall!(41, domain, ty | SOCK_CLOEXEC, 0) }) as i32
}

fn addr(v6: bool, port: u16) -> ([u8; 28], usize) {
    let mut b = [0u8; 28];
    if v6 {
        b[..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
        b[8..24].copy_from_slice(&LOOP6);
    } else {
        b[..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
        b[4..8].copy_from_slice(&LOOP4);
    }
    b[2..4].copy_from_slice(&port.to_be_bytes());
    (b, if v6 { 28 } else { 16 })
}

/// A listening socket on loopback; returns (fd, port).
fn listener(v6: bool, backlog: i32) -> (i32, u16) {
    let s = sock(if v6 { AF_INET6 } else { AF_INET }, SOCK_STREAM);
    let (a, n) = addr(v6, 0);
    unsafe { syscall!(49, s, a.as_ptr(), n) };
    listen(s, backlog);
    let mut b = [0u8; 28];
    let mut len = 28u32;
    unsafe { syscall!(51, s, b.as_mut_ptr(), &mut len as *mut u32) };
    (s, u16::from_be_bytes([b[2], b[3]]))
}

fn connect_to(v6: bool, port: u16, ty: i32) -> (i32, isize) {
    let c = sock(if v6 { AF_INET6 } else { AF_INET }, ty);
    let (a, n) = addr(v6, port);
    let r = unsafe { syscall!(42, c, a.as_ptr(), n) };
    (c, r)
}

fn accept(s: i32) -> i32 {
    (unsafe { syscall!(288, s, 0, 0, SOCK_CLOEXEC) }) as i32
}

/// A connected pair (client, server side).
fn pair(v6: bool) -> (i32, i32) {
    let (l, port) = listener(v6, 4);
    let (c, r) = connect_to(v6, port, SOCK_STREAM);
    assert!(r == 0, "connect: {}", errname(r));
    let s = accept(l);
    close(l);
    (c, s)
}

fn shutdown(fd: i32, how: i32) -> isize {
    unsafe { syscall!(48, fd, how) }
}

fn so_error(fd: i32) -> i32 {
    let mut v = 0i32;
    let mut len = 4u32;
    getsockopt(fd, SOL_SOCKET, SO_ERROR, &mut v as *mut i32 as *mut u8, &mut len);
    v
}

fn revents(fd: i32, events: i16, ms: i32) -> i16 {
    let mut p = [Pollfd { fd, events, revents: 0 }];
    if poll(&mut p, ms) <= 0 { 0 } else { p[0].revents }
}

fn pattern(off: usize, b: &mut [u8], seed: usize) {
    for (i, x) in b.iter_mut().enumerate() {
        *x = ((off + i) * 7 + seed) as u8 ^ ((off + i) >> 11) as u8;
    }
}

/// Send `total` patterned bytes on `fd`.
fn send_pattern(fd: i32, total: usize, seed: usize) -> bool {
    let mut chunk = [0u8; 16384];
    let mut sent = 0;
    while sent < total {
        let n = chunk.len().min(total - sent);
        pattern(sent, &mut chunk[..n], seed);
        let r = send(fd, &chunk[..n], MSG_NOSIGNAL);
        if r <= 0 {
            return false;
        }
        sent += r as usize;
        // A partial send: resend the rest of this chunk next time round.
    }
    true
}

/// Read until EOF; returns (bytes, all matched the pattern).
fn recv_pattern(fd: i32, seed: usize) -> (usize, bool) {
    let mut buf = [0u8; 9000];
    let mut want = [0u8; 9000];
    let (mut got, mut good) = (0usize, true);
    loop {
        let r = read(fd, &mut buf);
        if r <= 0 {
            return (got, good && r == 0);
        }
        let r = r as usize;
        pattern(got, &mut want[..r], seed);
        good &= buf[..r] == want[..r];
        got += r;
    }
}

/// Both directions at once: a child sends `total` one way while the
/// parent sends `total` the other way.
fn bulk(v6: bool, total: usize, case: &str) {
    let (c, s) = pair(v6);
    let pid = fork();
    if pid == 0 {
        close(s);
        let ok = send_pattern(c, total, 1);
        shutdown(c, 1);
        let (got, good) = recv_pattern(c, 2);
        exit(if ok && got == total && good { 0 } else { 1 });
    }
    close(c);
    let child = fork();
    if child == 0 {
        // The parent's sender, in its own process so neither side stalls.
        exit(if send_pattern(s, total, 2) { 0 } else { 1 });
    }
    let (got, good) = recv_pattern(s, 1);
    let (mut st1, mut st2) = (0, 0);
    wait4(child as i32, &mut st2, 0);
    shutdown(s, 1);
    wait4(pid as i32, &mut st1, 0);
    check!(got == total && good, case, "got {got} of {total}, pattern ok {good}");
    let mut name = [0u8; 48];
    name[..case.len()].copy_from_slice(case.as_bytes());
    name[case.len()..case.len() + 8].copy_from_slice(b"_reverse");
    check!(st1 == 0 && st2 == 0, core::str::from_utf8(&name[..case.len() + 8]).unwrap(), "status {st1:#x} {st2:#x}");
    close(s);
}

fn backpressure() {
    let (l, port) = listener(false, 4);
    let (c, r) = connect_to(false, port, SOCK_STREAM | SOCK_NONBLOCK);
    check!(r == 0 || r == -(EINPROGRESS as isize), "nb_connect");
    check!(revents(c, POLLOUT, 2000) & POLLOUT != 0, "nb_connected");
    let s = accept(l);
    close(l);
    // Fill the pipe: the reader does not read.
    let chunk = [0x5au8; 65536];
    let mut sent = 0usize;
    let mut last = 0;
    for _ in 0..10000 {
        last = send(c, &chunk, MSG_NOSIGNAL);
        if last < 0 {
            break;
        }
        sent += last as usize;
    }
    check!(last == -(EAGAIN as isize) && sent >= 65536, "send_until_eagain", "sent {sent} last {}", errname(last));
    check_eq!(revents(c, POLLOUT, 0) & POLLOUT, 0, "full_not_writable");
    // The reader drains; the writer becomes writable again.
    let pid = fork();
    if pid == 0 {
        let mut b = [0u8; 32768];
        let mut got = 0;
        while got < sent {
            let r = read(s, &mut b);
            if r <= 0 {
                exit(1);
            }
            got += r as usize;
        }
        exit(0);
    }
    check!(revents(c, POLLOUT, 5000) & POLLOUT != 0, "writable_after_drain");
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    check_eq!(st, 0, "reader_got_everything");
    close(c);
    close(s);
}

fn waitall() {
    let (c, s) = pair(false);
    let pid = fork();
    if pid == 0 {
        let b = [7u8; 1000];
        for _ in 0..100 {
            send(c, &b, MSG_NOSIGNAL);
            sleep_ms(2);
        }
        exit(0);
    }
    let mut buf = [0u8; 100_000];
    let r = recv(s, &mut buf, MSG_WAITALL);
    check_eq!(r, 100_000, "recv_waitall");
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    close(c);
    close(s);
}

fn shutdowns() {
    let (c, s) = pair(false);
    let mut b = [0u8; 64];
    // SHUT_WR: the peer reads the data, then EOF; the other way works.
    check_eq!(send(c, b"last words", MSG_NOSIGNAL), 10, "send_before_shut");
    check_ok!(shutdown(c, 1), "shut_wr");
    check_err!(send(c, b"x", MSG_NOSIGNAL), EPIPE, "send_after_shut_wr");
    check!(revents(s, POLLIN | POLLRDHUP, 2000) & POLLRDHUP != 0, "peer_pollrdhup");
    check_eq!(read(s, &mut b), 10, "peer_reads_data");
    check_eq!(read(s, &mut b), 0, "peer_eof");
    check_eq!(read(s, &mut b), 0, "peer_eof_again");
    check_eq!(send(s, b"reply", MSG_NOSIGNAL), 5, "half_close_send");
    check_eq!(read(c, &mut b), 5, "half_close_recv");
    // SHUT_RD: reads end at once.
    check_ok!(shutdown(c, 0), "shut_rd");
    check_eq!(recv(c, &mut b, MSG_DONTWAIT), 0, "recv_after_shut_rd");
    check!(revents(c, POLLIN, 0) & POLLIN != 0, "shut_rd_readable");
    check_err!(shutdown(c, 3), EINVAL, "shutdown_bad_how");
    close(c);
    close(s);
    // Unconnected socket.
    let u = sock(AF_INET, SOCK_STREAM);
    check_err!(shutdown(u, 2), ENOTCONN, "shutdown_unconnected");
    close(u);
}

fn resets() {
    let mut b = [0u8; 64];
    // SO_LINGER {1, 0}: close resets; the peer's next read says so once.
    let (c, s) = pair(false);
    let linger = [1i32, 0];
    check_ok!(setsockopt(c, SOL_SOCKET, SO_LINGER, linger.as_ptr() as *const u8, 8), "so_linger_0");
    close(c);
    sleep_ms(50);
    check_err!(read(s, &mut b), ECONNRESET, "linger0_peer_econnreset");
    check_eq!(read(s, &mut b), 0, "after_reset_eof");
    check_err!(send(s, b"x", MSG_NOSIGNAL), EPIPE, "after_reset_epipe");
    close(s);

    // Unread data at close resets too; a peer that only polls sees
    // POLLERR|POLLHUP and SO_ERROR.
    let (c, s) = pair(false);
    send(s, b"unread", MSG_NOSIGNAL);
    sleep_ms(20);
    close(c);
    let ev = revents(s, POLLIN, 2000);
    check!(ev & POLLERR != 0 && ev & POLLHUP != 0, "unread_close_pollerr_hup", "{ev:#x}");
    check_eq!(so_error(s), ECONNRESET, "so_error_econnreset");
    check_eq!(so_error(s), 0, "so_error_cleared");
    check_eq!(read(s, &mut b), 0, "after_so_error_eof");
    close(s);

    // Writing to a peer that closed: its kernel answers with a reset.
    let (c, s) = pair(false);
    close(c);
    sleep_ms(20);
    check_eq!(read(s, &mut b), 0, "closed_peer_eof");
    check_eq!(send(s, b"into the void", MSG_NOSIGNAL), 13, "first_send_to_closed_peer");
    sleep_ms(100);
    let r = send(s, b"again", MSG_NOSIGNAL);
    check!(r == -(EPIPE as isize) || r == -(ECONNRESET as isize), "second_send_fails", "{}", errname(r));
    close(s);
}

fn many() {
    const N: usize = 64;
    let (l, port) = listener(false, 128);
    let mut cs = [0i32; N];
    for c in cs.iter_mut() {
        let (fd, r) = connect_to(false, port, SOCK_STREAM | SOCK_NONBLOCK);
        if r != 0 && r != -(EINPROGRESS as isize) {
            check!(false, "many_connect", "{}", errname(r));
        }
        *c = fd;
    }
    let mut connected = 0;
    for &c in &cs {
        if revents(c, POLLOUT, 3000) & POLLOUT != 0 && so_error(c) == 0 {
            connected += 1;
        }
    }
    check_eq!(connected, N, "many_connected");
    let mut ss = [0i32; N];
    let mut accepted = 0;
    for s in ss.iter_mut() {
        if revents(l, POLLIN, 3000) & POLLIN == 0 {
            break;
        }
        *s = accept(l);
        if *s >= 0 {
            accepted += 1;
        }
    }
    check_eq!(accepted, N, "many_accepted");
    // Each accepted socket echoes one byte to its client.
    let mut ok = 0;
    for (i, &c) in cs.iter().enumerate() {
        send(c, &[i as u8], MSG_NOSIGNAL);
    }
    for &s in &ss[..accepted] {
        let mut b = [0u8; 1];
        if revents(s, POLLIN, 2000) & POLLIN != 0 && read(s, &mut b) == 1 && send(s, &b, MSG_NOSIGNAL) == 1 {
            ok += 1;
        }
    }
    let mut echoed = 0;
    for (i, &c) in cs.iter().enumerate() {
        let mut b = [0u8; 1];
        if revents(c, POLLIN, 2000) & POLLIN != 0 && read(c, &mut b) == 1 && b[0] == i as u8 {
            echoed += 1;
        }
    }
    check!(ok == N && echoed == N, "many_echo", "{ok} {echoed}");
    for &s in &ss[..accepted] {
        close(s);
    }
    for &c in &cs {
        close(c);
    }
    close(l);
}

fn main() {
    bulk(false, 8 << 20, "bulk_8mib_v4");
    bulk(true, 2 << 20, "bulk_2mib_v6");
    backpressure();
    waitall();
    shutdowns();
    resets();
    many();
}
