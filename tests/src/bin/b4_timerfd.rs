//! B4: timerfd — one-shot and periodic expirations, gettime, disarm,
//! TFD_TIMER_ABSTIME, nonblocking reads, epoll integration.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b4_timerfd", main);

fn rd64(fd: i32) -> (isize, u64) {
    let mut b = [0u8; 8];
    let r = read(fd, &mut b);
    (r, u64::from_ne_bytes(b))
}

fn settime(fd: i32, flags: i32, value_ns: i64, interval_ns: i64) -> isize {
    let it = Itimerspec {
        interval: Timespec { sec: interval_ns / 1_000_000_000, nsec: interval_ns % 1_000_000_000 },
        value: Timespec { sec: value_ns / 1_000_000_000, nsec: value_ns % 1_000_000_000 },
    };
    unsafe { syscall!(nr::TIMERFD_SETTIME, fd, flags, &it as *const Itimerspec, 0) }
}

fn main() {
    let fd = check_ok!(unsafe { syscall!(nr::TIMERFD_CREATE, CLOCK_MONOTONIC, TFD_CLOEXEC) }, "timerfd_create") as i32;
    check_err!(unsafe { syscall!(nr::TIMERFD_CREATE, 1234, 0) }, EINVAL, "timerfd_bad_clock_einval");

    // One-shot 30ms: blocking read returns 1 expiration after >= 30ms.
    let t0 = now_ns();
    check_ok!(settime(fd, 0, 30_000_000, 0), "settime_oneshot");
    let mut cur = Itimerspec::default();
    unsafe { syscall!(nr::TIMERFD_GETTIME, fd, &mut cur as *mut Itimerspec) };
    let left = cur.value.sec * 1_000_000_000 + cur.value.nsec;
    check!(left > 0 && left <= 30_000_000, "gettime_remaining", "{} ns", left);
    check_eq!(rd64(fd), (8, 1), "oneshot_one_expiration");
    check!(now_ns() - t0 >= 30_000_000, "oneshot_not_early");

    // Periodic 10ms: after ~55ms, about 5 expirations accumulated.
    settime(fd, 0, 10_000_000, 10_000_000);
    sleep_ms(55);
    let (r, n) = rd64(fd);
    check!(r == 8 && (3..=7).contains(&n), "periodic_expirations", "r={} n={}", r, n);
    // The next read blocks until the next tick only.
    let t0 = now_ns();
    let (_, n) = rd64(fd);
    check!(n >= 1 && now_ns() - t0 < 100_000_000, "periodic_next_tick");

    // Disarm; nonblocking read then fails.
    check_ok!(settime(fd, 0, 0, 0), "disarm");
    fcntl(fd, F_SETFL, O_NONBLOCK as usize);
    check_err!(rd64(fd).0, EAGAIN, "disarmed_nonblock_eagain");
    unsafe { syscall!(nr::TIMERFD_GETTIME, fd, &mut cur as *mut Itimerspec) };
    check!(cur.value == Timespec::default(), "gettime_disarmed_zero");

    // Absolute expiry.
    let target = now_ns() + 20_000_000;
    check_ok!(settime(fd, TFD_TIMER_ABSTIME, target, 0), "settime_abstime");
    let ep = epoll_create1(0) as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, fd, EPOLLIN, 3);
    let mut ev = [EpollEvent::default(); 1];
    check_eq!(epoll_wait(ep, &mut ev, 2000), 1, "epoll_timerfd_ready");
    check!(now_ns() >= target, "abstime_not_early");
    check_eq!(rd64(fd), (8, 1), "abstime_one_expiration");
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "epoll_cleared_after_read");

    // REALTIME clock timers work too.
    let rfd = unsafe { syscall!(nr::TIMERFD_CREATE, CLOCK_REALTIME, 0) } as i32;
    settime(rfd, 0, 10_000_000, 0);
    check_eq!(rd64(rfd), (8, 1), "realtime_timerfd");
    close(rfd);
    // BOOTTIME is what Android's alarm code uses.
    let bfd = unsafe { syscall!(nr::TIMERFD_CREATE, CLOCK_BOOTTIME, 0) };
    check_ok!(bfd, "boottime_timerfd");
    close(bfd as i32);
    close(fd);
}
