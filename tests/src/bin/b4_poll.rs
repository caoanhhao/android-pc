//! B4: select/pselect6, ppoll's signal mask (EINTR, mask restored), FIFOs
//! (open rules, O_NONBLOCK, POLLHUP), epoll refusals (EPERM, ELOOP), and
//! anon_inode names in /proc/self/fd and fdinfo.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicI32, Ordering};
use rt::*;

rt::test_main!("b4_poll", main);

const ENXIO: i32 = 6;
const ELOOP: i32 = 40;

static HITS: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_sig(_: i32) {
    HITS.fetch_add(1, Ordering::SeqCst);
}

fn select(nfds: i32, r: &mut u64, w: &mut u64, timeout: Option<&mut [i64; 2]>) -> isize {
    let t = timeout.map_or(core::ptr::null_mut(), |t| t.as_mut_ptr());
    unsafe { syscall!(nr::SELECT, nfds, r as *mut u64, w as *mut u64, 0, t) }
}

fn readlink(path: &Path, buf: &mut [u8]) -> usize {
    let n = unsafe { syscall!(nr::READLINK, path.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    n.max(0) as usize
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn main() {
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    let (r, w) = (p[0], p[1]);

    // select: write end ready, read end not; then both.
    let (mut rs, mut ws) = (1u64 << r, 1u64 << w);
    let mut tv = [0i64, 0];
    check_eq!(select(w + 1, &mut rs, &mut ws, Some(&mut tv)), 1, "select_writer_ready");
    check!(rs == 0 && ws == 1 << w, "select_sets_updated");
    write(w, b"x");
    let (mut rs, mut ws) = (1u64 << r, 1u64 << w);
    check_eq!(select(w + 1, &mut rs, &mut ws, Some(&mut tv)), 2, "select_both_ready");
    let mut b = [0u8; 8];
    read(r, &mut b);
    let (mut rs, mut ws) = (1u64 << r, 0u64);
    let mut tv = [0i64, 20_000];
    let t0 = now_ns();
    check_eq!(select(r + 1, &mut rs, &mut ws, Some(&mut tv)), 0, "select_timeout");
    check!(now_ns() - t0 >= 20_000_000, "select_timeout_waited");
    let (mut rs, mut ws) = (1u64 << 60, 0u64);
    check_err!(select(61, &mut rs, &mut ws, None), EBADF, "select_bad_fd_ebadf");
    let (mut rs, mut ws) = (0u64, 1u64 << w);
    let ts = Timespec { sec: 0, nsec: 0 };
    check_eq!(
        unsafe { syscall!(nr::PSELECT6, w + 1, &mut rs as *mut u64, &mut ws as *mut u64, 0, &ts as *const Timespec, 0) },
        1,
        "pselect6"
    );

    // ppoll with a mask that unblocks SIGUSR1: EINTR, handler ran, old
    // mask back afterwards.
    signal(SIGUSR1, on_sig, 0);
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR1)), None);
    let me = getpid();
    let pid = fork();
    if pid == 0 {
        sleep_ms(20);
        kill(me, SIGUSR1);
        exit(0);
    }
    let mut pf = [Pollfd { fd: r, events: POLLIN, revents: 0 }];
    let unblocked: u64 = 0;
    let n = unsafe { syscall!(nr::PPOLL, pf.as_mut_ptr(), 1, 0, &unblocked as *const u64, 8) };
    check_err!(n, EINTR, "ppoll_sigmask_eintr");
    check_eq!(HITS.load(Ordering::SeqCst), 1, "ppoll_handler_ran");
    let mut cur = 0u64;
    sigprocmask(SIG_BLOCK, None, Some(&mut cur));
    check!(cur & sigbit(SIGUSR1) != 0, "ppoll_mask_restored");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // FIFOs.
    let dir = scratch_dir();
    let fifo = cpath!("{}/fifo", Bytes(dir.as_bytes()));
    check_ok!(unsafe { syscall!(nr::MKNODAT, AT_FDCWD, fifo.as_ptr(), S_IFIFO | 0o600, 0) }, "mkfifo");
    let mut st = Stat::default();
    fstatat(AT_FDCWD, fifo.as_ptr(), &mut st, 0);
    check_eq!(st.mode & S_IFMT, S_IFIFO, "fifo_stat_type");
    check_err!(open(fifo.as_ptr(), O_WRONLY | O_NONBLOCK, 0), ENXIO, "fifo_nonblock_writer_no_reader_enxio");
    let fr = check_ok!(open(fifo.as_ptr(), O_RDONLY | O_NONBLOCK, 0), "fifo_nonblock_reader") as i32;
    let mut pf = [Pollfd { fd: fr, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pf, 0), 0, "fifo_no_hup_before_writer");
    let fw = check_ok!(open(fifo.as_ptr(), O_WRONLY | O_NONBLOCK, 0), "fifo_nonblock_writer") as i32;
    check_eq!(write(fw, b"abc"), 3, "fifo_write");
    check_eq!(poll(&mut pf, 0), 1, "fifo_poll_readable");
    check_eq!(read(fr, &mut b), 3, "fifo_read");
    check_err!(lseek(fr, 0, SEEK_SET), ESPIPE, "fifo_lseek_espipe");
    close(fw);
    poll(&mut pf, 0);
    check!(pf[0].revents & POLLHUP != 0, "fifo_hup_after_writer_close");
    close(fr);
    // Blocking opens meet each other; the data survives between them.
    let pid = fork();
    if pid == 0 {
        let fw = open(fifo.as_ptr(), O_WRONLY, 0) as i32;
        write(fw, b"hi");
        exit(0);
    }
    sleep_ms(20);
    let fr = check_ok!(open(fifo.as_ptr(), O_RDONLY, 0), "fifo_blocking_open") as i32;
    let n = read(fr, &mut b);
    check!(n == 2 && &b[..2] == b"hi", "fifo_blocking_data");
    wait4(pid as i32, &mut s, 0);
    close(fr);
    unlinkat(AT_FDCWD, fifo.as_ptr(), 0);

    // epoll refuses regular files and loops.
    let ep = epoll_create1(0) as i32;
    let reg = cpath!("{}/reg", Bytes(dir.as_bytes()));
    let rf = open(reg.as_ptr(), O_RDWR | O_CREAT, 0o600) as i32;
    check_err!(epoll_ctl(ep, EPOLL_CTL_ADD, rf, EPOLLIN, 0), EPERM, "epoll_regular_file_eperm");
    let ep2 = epoll_create1(0) as i32;
    check_ok!(epoll_ctl(ep, EPOLL_CTL_ADD, ep2, EPOLLIN, 0), "epoll_nest");
    check_err!(epoll_ctl(ep2, EPOLL_CTL_ADD, ep, EPOLLIN, 0), ELOOP, "epoll_loop_eloop");
    close(rf);
    unlinkat(AT_FDCWD, reg.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);

    // anon_inode names and fdinfo.
    let efd = unsafe { syscall!(nr::EVENTFD2, 5, 0) } as i32;
    let tfd = unsafe { syscall!(nr::TIMERFD_CREATE, CLOCK_MONOTONIC, 0) } as i32;
    let mask = sigbit(SIGUSR2);
    let sfd = unsafe { syscall!(nr::SIGNALFD4, -1i32, &mask as *const u64, 8, 0) } as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, efd, EPOLLIN, 0x77);
    let mut l = [0u8; 64];
    for (fd, name, case) in [
        (efd, &b"anon_inode:[eventfd]"[..], "proc_fd_eventfd"),
        (ep, b"anon_inode:[eventpoll]", "proc_fd_eventpoll"),
        (tfd, b"anon_inode:[timerfd]", "proc_fd_timerfd"),
        (sfd, b"anon_inode:[signalfd]", "proc_fd_signalfd"),
    ] {
        let n = readlink(&cpath!("/proc/self/fd/{}", fd), &mut l);
        check!(&l[..n] == name, case, "got {}", Bytes(&l[..n]));
    }
    let mut info = [0u8; 512];
    let f = open(cpath!("/proc/self/fdinfo/{}", efd).as_ptr(), O_RDONLY, 0) as i32;
    let n = read(f, &mut info).max(0) as usize;
    close(f);
    check!(contains(&info[..n], b"flags:") && contains(&info[..n], b"eventfd-count:"), "fdinfo_eventfd");
    let f = open(cpath!("/proc/self/fdinfo/{}", ep).as_ptr(), O_RDONLY, 0) as i32;
    let n = read(f, &mut info).max(0) as usize;
    close(f);
    check!(contains(&info[..n], b"tfd:") && contains(&info[..n], b"data:"), "fdinfo_epoll");
}
