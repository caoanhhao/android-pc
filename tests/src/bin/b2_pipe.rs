//! B2: pipes — pipe2 flags, data flow, EOF, EPIPE/SIGPIPE, O_NONBLOCK,
//! blocking reader woken by a writer in another process, capacity.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b2_pipe", main);

fn main() {
    let mut p = [0i32; 2];
    check_ok!(pipe2(&mut p, 0), "pipe2");
    check!(p[0] >= 0 && p[1] >= 0 && p[0] != p[1], "pipe_fds_distinct");
    let mut st = Stat::default();
    fstat(p[0], &mut st);
    check_eq!(st.mode & S_IFMT, S_IFIFO, "pipe_is_fifo");

    check_eq!(write(p[1], b"ping"), 4, "pipe_write");
    let mut buf = [0u8; 64];
    check_eq!(read(p[0], &mut buf), 4, "pipe_read");
    check_eq!(&buf[..4], b"ping", "pipe_content");
    check_err!(lseek(p[0], 0, SEEK_SET), ESPIPE, "pipe_lseek_espipe");
    check_err!(write(p[0], b"x"), EBADF, "write_read_end_ebadf");

    // Short reads return what's available.
    write(p[1], b"abc");
    check_eq!(read(p[0], &mut buf[..2]), 2, "partial_read");
    check_eq!(read(p[0], &mut buf), 1, "rest_of_data");

    // EOF once all writers are closed.
    close(p[1]);
    check_eq!(read(p[0], &mut buf), 0, "eof_after_writer_closed");
    close(p[0]);

    // Writing with no readers: EPIPE plus SIGPIPE (ignored here).
    pipe2(&mut p, 0);
    let ign = Sigaction { handler: SIG_IGN, flags: SA_RESTORER, restorer: restorer(), mask: 0 };
    sigaction(SIGPIPE, Some(&ign), None);
    close(p[0]);
    check_err!(write(p[1], b"x"), EPIPE, "write_no_reader_epipe");
    close(p[1]);
    // Default SIGPIPE kills.
    let st = run_child(|| {
        let mut q = [0i32; 2];
        let dfl = Sigaction { handler: SIG_DFL, flags: SA_RESTORER, restorer: restorer(), mask: 0 };
        sigaction(SIGPIPE, Some(&dfl), None);
        pipe2(&mut q, 0);
        close(q[0]);
        write(q[1], b"x");
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == SIGPIPE, "sigpipe_default_kills", "status {:#x}", st);

    // O_NONBLOCK and O_CLOEXEC.
    check_ok!(pipe2(&mut p, O_NONBLOCK | O_CLOEXEC), "pipe2_flags");
    check_err!(read(p[0], &mut buf), EAGAIN, "nonblock_empty_eagain");
    check_eq!(fcntl(p[0], F_GETFD, 0), FD_CLOEXEC as isize, "pipe_cloexec");
    check!(fcntl(p[1], F_GETFL, 0) & O_NONBLOCK as isize != 0, "pipe_nonblock_flag");
    // Fill to capacity: a nonblocking write eventually returns EAGAIN.
    let chunk = [0x5au8; 4096];
    let mut total = 0usize;
    loop {
        let r = write(p[1], &chunk);
        if r < 0 {
            check_err!(r, EAGAIN, "full_pipe_eagain");
            break;
        }
        total += r as usize;
        if total > 16 << 20 {
            break;
        }
    }
    check!(total >= 4096 && total <= 1 << 20, "pipe_capacity_sane", "capacity {}", total);
    // Drain and verify bytes.
    let mut got = 0usize;
    let mut ok = true;
    loop {
        let r = read(p[0], &mut buf);
        if r <= 0 {
            break;
        }
        ok &= buf[..r as usize].iter().all(|&b| b == 0x5a);
        got += r as usize;
    }
    check_eq!(got, total, "drained_all");
    check!(ok, "drained_bytes_intact");
    close(p[0]);
    close(p[1]);

    // Blocking reader is woken by a writer in a child process.
    pipe2(&mut p, 0);
    let (r0, w1) = (p[0], p[1]);
    let pid = fork();
    if pid == 0 {
        close(r0);
        sleep_ms(30);
        write(w1, b"from-child");
        exit(0);
    }
    close(w1);
    let t0 = now_ns();
    let n = read(r0, &mut buf);
    check_eq!(n, 10, "blocking_read_woken");
    check!(now_ns() - t0 >= 20_000_000, "read_actually_blocked");
    check_eq!(read(r0, &mut buf), 0, "eof_after_child_exit");
    close(r0);
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // Large transfer through a pipe between processes (exceeds capacity).
    pipe2(&mut p, 0);
    let (r0, w1) = (p[0], p[1]);
    const TOTAL: usize = 1 << 20;
    let pid = fork();
    if pid == 0 {
        close(r0);
        let mut b = [0u8; 3000];
        let mut sent = 0;
        while sent < TOTAL {
            let n = b.len().min(TOTAL - sent);
            for (i, x) in b[..n].iter_mut().enumerate() {
                *x = ((sent + i) % 251) as u8;
            }
            write_all(w1, &b[..n]);
            sent += n;
        }
        exit(0);
    }
    close(w1);
    let mut recvd = 0usize;
    let mut ok = true;
    let mut b = [0u8; 4096];
    loop {
        let n = read(r0, &mut b);
        if n <= 0 {
            break;
        }
        for (i, &x) in b[..n as usize].iter().enumerate() {
            ok &= x == ((recvd + i) % 251) as u8;
        }
        recvd += n as usize;
    }
    check_eq!(recvd, TOTAL, "bulk_transfer_size");
    check!(ok, "bulk_transfer_ordered");
    close(r0);
    wait4(pid as i32, &mut s, 0);

    // The legacy pipe syscall.
    check_ok!(unsafe { syscall!(nr::PIPE, p.as_mut_ptr()) }, "pipe_legacy");
    close(p[0]);
    close(p[1]);
}
