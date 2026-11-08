//! B4: readiness — poll/ppoll and epoll (level vs edge triggered, ONESHOT,
//! MOD/DEL, EPOLLHUP, timeouts, wakeup from another process, nesting).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b4_epoll", main);

fn main() {
    let mut p = [0i32; 2];
    pipe2(&mut p, O_NONBLOCK);
    let (r, w) = (p[0], p[1]);

    // poll
    let mut fds = [Pollfd { fd: r, events: POLLIN, revents: 0 }, Pollfd { fd: w, events: POLLOUT, revents: 0 }];
    check_eq!(poll(&mut fds, 0), 1, "poll_only_writer_ready");
    check!(fds[0].revents == 0 && fds[1].revents & POLLOUT != 0, "poll_revents_initial");
    write(w, b"x");
    check_eq!(poll(&mut fds, 0), 2, "poll_both_ready");
    check!(fds[0].revents & POLLIN != 0, "poll_pollin");
    let mut bad = [Pollfd { fd: 999, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut bad, 0), 1, "poll_bad_fd_counts");
    check_eq!(bad[0].revents, POLLNVAL, "poll_pollnval");
    // Negative fds are ignored.
    let mut neg = [Pollfd { fd: -1, events: POLLIN, revents: 7 }];
    check!(poll(&mut neg, 0) == 0 && neg[0].revents == 0, "poll_negative_fd_ignored");
    let t0 = now_ns();
    let mut none = [Pollfd { fd: w, events: 0, revents: 0 }];
    check_eq!(poll(&mut none, 30), 0, "poll_timeout");
    check!(now_ns() - t0 >= 30_000_000, "poll_timeout_waited");
    let ts = Timespec { sec: 0, nsec: 0 };
    check_eq!(unsafe { syscall!(nr::PPOLL, fds.as_mut_ptr(), 2, &ts as *const Timespec, 0, 8) }, 2, "ppoll");
    let mut b = [0u8; 8];
    read(r, &mut b);

    // epoll level-triggered.
    let ep = check_ok!(epoll_create1(EPOLL_CLOEXEC), "epoll_create1") as i32;
    check_eq!(fcntl(ep, F_GETFD, 0), FD_CLOEXEC as isize, "epoll_cloexec");
    check_ok!(epoll_ctl(ep, EPOLL_CTL_ADD, r, EPOLLIN, 0x1234), "epoll_ctl_add");
    check_err!(epoll_ctl(ep, EPOLL_CTL_ADD, r, EPOLLIN, 0), EEXIST, "epoll_add_twice_eexist");
    check_err!(epoll_ctl(ep, EPOLL_CTL_MOD, w, EPOLLIN, 0), ENOENT, "epoll_mod_missing_enoent");
    check_err!(epoll_ctl(ep, EPOLL_CTL_ADD, ep, EPOLLIN, 0), EINVAL, "epoll_add_self_einval");
    let mut ev = [EpollEvent::default(); 4];
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "epoll_nothing_ready");
    write(w, b"ab");
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "epoll_ready");
    let (e0, d0) = (ev[0].events, ev[0].data);
    check!(e0 & EPOLLIN != 0 && d0 == 0x1234, "epoll_event_and_data");
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "epoll_level_triggered_repeats");
    read(r, &mut b);
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "epoll_cleared_after_drain");

    // Edge-triggered: reported once per new arrival.
    check_ok!(epoll_ctl(ep, EPOLL_CTL_MOD, r, EPOLLIN | EPOLLET, 0x55), "epoll_ctl_mod_et");
    write(w, b"a");
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "epollet_first");
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "epollet_not_repeated");
    write(w, b"b");
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "epollet_new_data");
    read(r, &mut b);

    // ONESHOT disables after one report until re-armed.
    epoll_ctl(ep, EPOLL_CTL_MOD, r, EPOLLIN | EPOLLONESHOT, 0x66);
    write(w, b"c");
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "oneshot_first");
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "oneshot_disarmed");
    epoll_ctl(ep, EPOLL_CTL_MOD, r, EPOLLIN | EPOLLONESHOT, 0x66);
    check_eq!(epoll_wait(ep, &mut ev, 0), 1, "oneshot_rearmed");
    read(r, &mut b);

    // DEL.
    check_ok!(epoll_ctl(ep, EPOLL_CTL_DEL, r, 0, 0), "epoll_ctl_del");
    write(w, b"d");
    check_eq!(epoll_wait(ep, &mut ev, 0), 0, "deleted_not_reported");
    read(r, &mut b);

    // Several fds at once, writer side EPOLLOUT.
    epoll_ctl(ep, EPOLL_CTL_ADD, r, EPOLLIN, 1);
    epoll_ctl(ep, EPOLL_CTL_ADD, w, EPOLLOUT, 2);
    write(w, b"e");
    let n = epoll_wait(ep, &mut ev, 0);
    check_eq!(n, 2, "epoll_two_ready");
    let mut sum = 0u64;
    for e in &ev[..n.max(0) as usize] {
        sum += e.data;
    }
    check_eq!(sum, 3, "epoll_two_ready_data");
    // maxevents limits the batch.
    check_eq!(epoll_wait(ep, &mut ev[..1], 0), 1, "epoll_maxevents");
    check_err!(epoll_wait(ep, &mut ev[..0], 0), EINVAL, "epoll_maxevents_zero_einval");
    read(r, &mut b);

    // Closing the writer gives EPOLLHUP on the reader.
    epoll_ctl(ep, EPOLL_CTL_DEL, w, 0, 0);
    close(w);
    let n = epoll_wait(ep, &mut ev, 0);
    let e0 = ev[0].events;
    check!(n == 1 && e0 & EPOLLHUP != 0, "epollhup_on_writer_close", "n={} events={:#x}", n, e0);
    close(r);

    // Closing an fd removes it from the interest list.
    let mut q = [0i32; 2];
    pipe2(&mut q, 0);
    epoll_ctl(ep, EPOLL_CTL_ADD, q[0], EPOLLIN, 9);
    write(q[1], b"z");
    close(q[0]);
    let n = epoll_wait(ep, &mut ev, 0);
    let mut stale = false;
    for e in &ev[..n.max(0) as usize] {
        stale |= e.data == 9;
    }
    check!(!stale, "closed_fd_auto_removed");
    close(q[1]);

    // Blocking epoll_wait woken by another process; timeout respected.
    let ep2 = epoll_create1(0) as i32;
    pipe2(&mut q, 0);
    epoll_ctl(ep2, EPOLL_CTL_ADD, q[0], EPOLLIN, 7);
    let t0 = now_ns();
    check_eq!(epoll_wait(ep2, &mut ev, 25), 0, "epoll_wait_timeout");
    check!(now_ns() - t0 >= 25_000_000, "epoll_timeout_waited");
    let w1 = q[1];
    let pid = fork();
    if pid == 0 {
        sleep_ms(30);
        write(w1, b"!");
        exit(0);
    }
    let n = epoll_wait(ep2, &mut ev, 5000);
    let d = ev[0].data;
    check!(n == 1 && d == 7, "epoll_woken_by_other_process");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // epoll fds nest: an epoll fd is readable when its set is ready.
    let outer = epoll_create1(0) as i32;
    check_ok!(epoll_ctl(outer, EPOLL_CTL_ADD, ep2, EPOLLIN, 77), "epoll_nested_add");
    let n = epoll_wait(outer, &mut ev, 0);
    let d = ev[0].data;
    check!(n == 1 && d == 77, "epoll_nested_ready");
    let mut pf = [Pollfd { fd: ep2, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pf, 0), 1, "poll_on_epoll_fd");

    // epoll_pwait with a NULL sigmask behaves like epoll_wait.
    let n = unsafe { syscall!(nr::EPOLL_PWAIT, ep2, ev.as_mut_ptr(), 4, 0, 0, 8) };
    check_eq!(n, 1, "epoll_pwait");
}
