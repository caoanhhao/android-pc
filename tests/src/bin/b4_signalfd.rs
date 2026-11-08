//! B4: signalfd — blocked signals read as signalfd_siginfo, readiness via
//! poll/epoll, mask update, SIGCHLD from a child.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b4_signalfd", main);

fn signalfd(fd: i32, mask: u64, flags: i32) -> isize {
    unsafe { syscall!(nr::SIGNALFD4, fd, &mask as *const u64, 8, flags) }
}

fn read_info(fd: i32) -> (isize, SignalfdSiginfo) {
    let mut si = SignalfdSiginfo::zeroed();
    let r = unsafe { syscall!(nr::READ, fd, &mut si as *mut SignalfdSiginfo, 128) };
    (r, si)
}

fn main() {
    let me = getpid();
    let mask = sigbit(SIGUSR1) | sigbit(SIGUSR2);
    sigprocmask(SIG_BLOCK, Some(mask | sigbit(SIGCHLD)), None);
    let fd = check_ok!(signalfd(-1, mask, SFD_NONBLOCK | SFD_CLOEXEC), "signalfd4") as i32;
    check_err!(read_info(fd).0, EAGAIN, "empty_eagain");

    let mut pf = [Pollfd { fd, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pf, 0), 0, "poll_not_ready");
    kill(me, SIGUSR1);
    check_eq!(poll(&mut pf, 0), 1, "poll_ready_after_kill");
    let (r, si) = read_info(fd);
    check_eq!(r, 128, "read_siginfo_size");
    check_eq!(si.signo, SIGUSR1 as u32, "ssi_signo");
    check_eq!(si.pid, me as u32, "ssi_pid");
    check_eq!(si.code, 0, "ssi_code_si_user");
    check_err!(read_info(fd).0, EAGAIN, "consumed");

    // Signals not in the mask are not reported.
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGHUP)), None);
    kill(me, SIGHUP);
    check_err!(read_info(fd).0, EAGAIN, "unmasked_signal_not_read");

    // Update the mask in place.
    check_eq!(signalfd(fd, sigbit(SIGHUP), 0), fd as isize, "signalfd_update_mask");
    let (_, si) = read_info(fd);
    check_eq!(si.signo, SIGHUP as u32, "updated_mask_reads_sighup");

    // SIGCHLD via epoll from an exiting child.
    let cfd = signalfd(-1, sigbit(SIGCHLD), 0) as i32;
    let ep = epoll_create1(0) as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, cfd, EPOLLIN, 0);
    let pid = fork();
    if pid == 0 {
        exit(3);
    }
    let mut ev = [EpollEvent::default(); 1];
    check_eq!(epoll_wait(ep, &mut ev, 5000), 1, "epoll_sigchld");
    let (r, si) = read_info(cfd);
    check!(r == 128 && si.signo == SIGCHLD as u32 && si.pid == pid as u32, "sigchld_info");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // Several signals in one read.
    signalfd(fd, mask, 0);
    kill(me, SIGUSR1);
    kill(me, SIGUSR2);
    let mut two = [0u8; 256];
    check_eq!(read(fd, &mut two), 256, "read_two_at_once");

    // A too-small buffer is EINVAL.
    kill(me, SIGUSR1);
    let mut small = [0u8; 64];
    check_err!(read(fd, &mut small), EINVAL, "small_buffer_einval");
}
