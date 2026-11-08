//! B4: eventfd — counter semantics, EFD_SEMAPHORE, EFD_NONBLOCK, overflow,
//! readiness with epoll, cross-process wakeup.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b4_eventfd", main);

fn rd64(fd: i32) -> (isize, u64) {
    let mut b = [0u8; 8];
    let r = read(fd, &mut b);
    (r, u64::from_ne_bytes(b))
}

fn wr64(fd: i32, v: u64) -> isize {
    write(fd, &v.to_ne_bytes())
}

fn main() {
    let fd = check_ok!(unsafe { syscall!(nr::EVENTFD2, 3, EFD_NONBLOCK | EFD_CLOEXEC) }, "eventfd2") as i32;
    check_eq!(rd64(fd), (8, 3), "initial_value");
    check_err!(rd64(fd).0, EAGAIN, "read_zero_nonblock_eagain");
    check_eq!(wr64(fd, 5), 8, "write_add");
    check_eq!(wr64(fd, 7), 8, "write_add_again");
    check_eq!(rd64(fd), (8, 12), "read_sum_resets");
    check_err!(write(fd, b"1234"), EINVAL, "short_write_einval");
    let mut small = [0u8; 4];
    check_err!(read(fd, &mut small), EINVAL, "short_read_einval");
    check_err!(wr64(fd, u64::MAX), EINVAL, "write_max_einval");
    // Counter saturates at MAX-1: a write that would overflow blocks/EAGAIN.
    check_eq!(wr64(fd, u64::MAX - 1), 8, "write_to_max");
    check_err!(wr64(fd, 1), EAGAIN, "overflow_nonblock_eagain");
    rd64(fd);

    // Readiness.
    let mut pf = [Pollfd { fd, events: POLLIN | POLLOUT, revents: 0 }];
    poll(&mut pf, 0);
    check_eq!(pf[0].revents, POLLOUT, "poll_empty_writable_only");
    wr64(fd, 1);
    poll(&mut pf, 0);
    check_eq!(pf[0].revents, POLLIN | POLLOUT, "poll_nonzero_readable");
    rd64(fd);
    close(fd);

    // Semaphore mode: each read returns 1 and decrements.
    let sfd = unsafe { syscall!(nr::EVENTFD2, 2, EFD_SEMAPHORE | EFD_NONBLOCK) } as i32;
    check_eq!(rd64(sfd), (8, 1), "semaphore_read_1");
    check_eq!(rd64(sfd), (8, 1), "semaphore_read_2");
    check_err!(rd64(sfd).0, EAGAIN, "semaphore_exhausted");
    close(sfd);

    // Blocking read woken by another process via epoll.
    let efd = unsafe { syscall!(nr::EVENTFD2, 0, 0) } as i32;
    let ep = epoll_create1(0) as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, efd, EPOLLIN, 1);
    let pid = fork();
    if pid == 0 {
        sleep_ms(20);
        wr64(efd, 42);
        exit(0);
    }
    let mut ev = [EpollEvent::default(); 1];
    check_eq!(epoll_wait(ep, &mut ev, 5000), 1, "epoll_eventfd_wakeup");
    check_eq!(rd64(efd), (8, 42), "value_from_child");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    let pid = fork();
    if pid == 0 {
        sleep_ms(20);
        wr64(efd, 9);
        exit(0);
    }
    check_eq!(rd64(efd), (8, 9), "blocking_read_woken");
    wait4(pid as i32, &mut s, 0);
}
