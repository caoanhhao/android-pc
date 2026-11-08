//! C3: the kernel log buffer as logd and init use it. /dev/kmsg: a write
//! ("<N>text") becomes one record; reads return one record per call as
//! "prio,seq,usec,flags;text\n" (facility and level kept, sequence numbers
//! increasing), each open file has its own position (SEEK_SET 0 = oldest,
//! SEEK_END = only new records), O_NONBLOCK reads EAGAIN when caught up,
//! a too-small buffer gives EINVAL, poll reports POLLIN for new records
//! and a blocked reader is woken by another process's write. syslog(2)
//! (klogctl): SIZE_BUFFER, READ_ALL holding the text in "<N>[time] text"
//! form, SIZE_UNREAD. /proc/kmsg (logd's klogd): readable, poll/epoll-able.
//! Needs root on Linux (dmesg_restrict, /dev/kmsg writes): SKIP otherwise.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_kmsg", main);

const SYSLOG: usize = 103;
const SYSLOG_ACTION_READ_ALL: i32 = 3;
const SYSLOG_ACTION_SIZE_UNREAD: i32 = 9;
const SYSLOG_ACTION_SIZE_BUFFER: i32 = 10;

fn kmsg_open(flags: i32) -> i32 {
    open(c"/dev/kmsg".as_ptr() as *const u8, flags | O_CLOEXEC, 0) as i32
}

/// One /dev/kmsg record: (prio, seq, text).
fn parse(rec: &[u8]) -> Option<(u32, u64, &[u8])> {
    let semi = rec.iter().position(|&c| c == b';')?;
    let mut f = rec[..semi].split(|&c| c == b',');
    let prio = parse_int(f.next()?)? as u32;
    let seq = parse_int(f.next()?)? as u64;
    parse_int(f.next()?)?;
    let text = &rec[semi + 1..];
    Some((prio, seq, text.strip_suffix(b"\n")?))
}

/// Read records until one whose text is `want` (non-blocking fd).
fn find(fd: i32, want: &[u8]) -> Option<(u32, u64)> {
    let mut b = [0u8; 2048];
    for _ in 0..4096 {
        let n = read(fd, &mut b);
        if n == -(EPIPE as isize) {
            continue;
        }
        if n <= 0 {
            return None;
        }
        if let Some((p, s, t)) = parse(&b[..n as usize])
            && t == want
        {
            return Some((p, s));
        }
    }
    None
}

fn main() {
    let w = kmsg_open(O_WRONLY);
    if w < 0 {
        skip("all", "/dev/kmsg not writable (needs root)");
        return;
    }
    let r = kmsg_open(O_RDONLY | O_NONBLOCK);
    if r < 0 {
        skip("all", "/dev/kmsg not readable (dmesg_restrict)");
        close(w);
        return;
    }
    let tag = cpath!("c3_kmsg-{}", getpid());
    let t = |s: &str| cpath!("{} {}", Bytes(tag.as_bytes()), s);

    // A record with an explicit priority (LOG_DAEMON | LOG_NOTICE = 29).
    let m1 = t("first");
    let line = cpath!("<29>{}\n", Bytes(m1.as_bytes()));
    check_eq!(write(w, line.as_bytes()), line.len as isize, "write_record");
    let a = find(r, m1.as_bytes());
    check!(a.is_some_and(|(p, _)| p == 29), "read_record_prio", "{:?}", a);

    // Without a prefix: LOG_USER (facility 1); sequence numbers grow.
    let m2 = t("second");
    write(w, m2.as_bytes());
    let b = find(r, m2.as_bytes());
    check!(b.is_some_and(|(p, _)| p >> 3 == 1), "default_facility_user", "{:?}", b);
    check!(matches!((a, b), (Some((_, s1)), Some((_, s2))) if s2 > s1), "seq_increases");

    // Caught up: EAGAIN.
    let mut buf = [0u8; 2048];
    check_err!(read(r, &mut buf), EAGAIN, "nonblock_eagain");

    // Each open file has its own position; SEEK_SET 0 rewinds to the oldest.
    let r2 = kmsg_open(O_RDONLY | O_NONBLOCK);
    check_ok!(lseek(r2, 0, SEEK_SET), "seek_set_0");
    check!(find(r2, m1.as_bytes()).is_some(), "second_reader_sees_old");
    check_ok!(lseek(r2, 0, SEEK_END), "seek_end");
    check_err!(read(r2, &mut buf), EAGAIN, "seek_end_skips_old");

    // Too small a buffer for the next record.
    let m3 = t("third, a somewhat longer line of text");
    write(w, m3.as_bytes());
    check_err!(read(r2, &mut buf[..8]), EINVAL, "small_buffer_einval");
    check!(find(r2, m3.as_bytes()).is_some(), "record_kept_after_einval");

    // poll: nothing new, then POLLIN after a write.
    let mut p = [Pollfd { fd: r2, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut p, 0), 0, "poll_idle");
    let m4 = t("fourth");
    write(w, m4.as_bytes());
    let mut p = [Pollfd { fd: r2, events: POLLIN, revents: 0 }];
    check!(poll(&mut p, 1000) == 1 && p[0].revents & POLLIN != 0, "poll_pollin");
    find(r2, m4.as_bytes());

    // A blocking reader is woken by another process's write.
    let rb = kmsg_open(O_RDONLY);
    lseek(rb, 0, SEEK_END);
    let m5 = t("wakeup");
    let pid = fork();
    if pid == 0 {
        sleep_ms(100);
        write(w, m5.as_bytes());
        exit(0);
    }
    let mut got = false;
    for _ in 0..64 {
        let n = read(rb, &mut buf);
        if n <= 0 {
            break;
        }
        if parse(&buf[..n as usize]).is_some_and(|(_, _, t)| t == m5.as_bytes()) {
            got = true;
            break;
        }
    }
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    check!(got, "blocking_read_woken");
    close(rb);

    // syslog(2).
    let size = unsafe { syscall!(SYSLOG, SYSLOG_ACTION_SIZE_BUFFER, 0, 0) };
    check!(size > 0, "syslog_size_buffer", "{}", size);
    let all = alloc_pages(1 << 20);
    let n = unsafe { syscall!(SYSLOG, SYSLOG_ACTION_READ_ALL, all, 1 << 20) };
    let text = if n > 0 { unsafe { core::slice::from_raw_parts(all, n as usize) } } else { &[][..] };
    check!(text.windows(m4.len).any(|x| x == m4.as_bytes()), "syslog_read_all_has_text", "n={}", n);
    check!(text.starts_with(b"<") && text.iter().take(32).any(|&c| c == b'['), "syslog_format");
    check!(unsafe { syscall!(SYSLOG, SYSLOG_ACTION_SIZE_UNREAD, 0, 0) } >= 0, "syslog_size_unread");
    check_err!(unsafe { syscall!(SYSLOG, 99, 0, 0) }, EINVAL, "syslog_bad_action");

    // /proc/kmsg: what logd's klogd reads (destructive, so only check that
    // it is there and pollable: on Linux a running syslog daemon reads it).
    let pk = open(c"/proc/kmsg".as_ptr() as *const u8, O_RDONLY | O_NONBLOCK | O_CLOEXEC, 0) as i32;
    check!(pk >= 0, "proc_kmsg_open", "{}", errname(pk as isize));
    let ep = epoll_create1(EPOLL_CLOEXEC) as i32;
    check_ok!(epoll_ctl(ep, EPOLL_CTL_ADD, pk, EPOLLIN, 1), "proc_kmsg_epoll");
    if is_rust_kernel() {
        let m6 = t("proc");
        write(w, m6.as_bytes());
        let mut ev = [EpollEvent::default(); 1];
        check_eq!(epoll_wait(ep, &mut ev, 1000), 1, "proc_kmsg_ready");
        let mut seen = false;
        for _ in 0..4096 {
            let n = read(pk, &mut buf);
            if n <= 0 {
                break;
            }
            // "<14>[    1.234567] text\n" lines.
            if buf[..n as usize].split(|&c| c == b'\n').any(|l| l.starts_with(b"<") && l.ends_with(m6.as_bytes())) {
                seen = true;
                break;
            }
        }
        check!(seen, "proc_kmsg_line");
    }
    close(ep);
    close(pk);
    close(r2);
    close(r);
    close(w);
}
