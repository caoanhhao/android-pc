//! B1: signal delivery — handlers and sa_restorer/rt_sigreturn, SA_SIGINFO,
//! masks and pending sets, default actions, faults, sigaltstack, EINTR,
//! sigsuspend, sigtimedwait, SIGCHLD.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use rt::*;

rt::test_main!("b1_signals", main);

static HITS: AtomicUsize = AtomicUsize::new(0);
static LAST_SIG: AtomicI32 = AtomicI32::new(0);
static MASK_IN_HANDLER: AtomicU64 = AtomicU64::new(0);
static SI_CODE: AtomicI32 = AtomicI32::new(-1);
static SI_PID: AtomicI32 = AtomicI32::new(0);
static STACK_ADDR: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_sig(sig: i32) {
    HITS.fetch_add(1, SeqCst);
    LAST_SIG.store(sig, SeqCst);
    let mut m = 0u64;
    sigprocmask(SIG_BLOCK, None, Some(&mut m));
    MASK_IN_HANDLER.store(m, SeqCst);
    let local = 0u8;
    STACK_ADDR.store(&local as *const u8 as usize, SeqCst);
}

extern "C" fn on_siginfo(sig: i32, info: *const Siginfo, _uctx: usize) {
    let info = unsafe { &*info };
    LAST_SIG.store(sig, SeqCst);
    SI_CODE.store(info.code, SeqCst);
    SI_PID.store(info.u0 as u32 as i32, SeqCst);
    HITS.fetch_add(1, SeqCst);
}

extern "C" fn on_segv(_sig: i32, info: *const Siginfo, _uctx: usize) {
    // Fault address is si_addr; exit straight from the handler.
    let addr = unsafe { (*info).u0 };
    exit(if addr == 0x10 { 42 } else { 43 });
}

fn install_siginfo(sig: i32, f: extern "C" fn(i32, *const Siginfo, usize)) {
    let act = Sigaction {
        handler: f as *const () as usize,
        flags: SA_SIGINFO | SA_RESTORER,
        restorer: restorer(),
        mask: 0,
    };
    sigaction(sig, Some(&act), None);
}

fn reset() {
    HITS.store(0, SeqCst);
    LAST_SIG.store(0, SeqCst);
}

fn main() {
    let me = getpid();

    // Basic handler; the signal is delivered before kill() returns to us.
    check_ok!(signal(SIGUSR1, on_sig, 0), "sigaction_install");
    reset();
    // Keep values live in callee-saved registers across the signal frame.
    let (a, b, c) = (core::hint::black_box(0x1111u64), core::hint::black_box(0x2222u64), core::hint::black_box(0x3333u64));
    kill(me, SIGUSR1);
    check_eq!(HITS.load(SeqCst), 1, "handler_ran");
    check_eq!(LAST_SIG.load(SeqCst), SIGUSR1, "handler_signo");
    check!(core::hint::black_box(a + b + c) == 0x6666, "registers_restored_after_sigreturn");
    check!(MASK_IN_HANDLER.load(SeqCst) & sigbit(SIGUSR1) != 0, "signal_blocked_in_own_handler");
    let mut m = 0u64;
    sigprocmask(SIG_BLOCK, None, Some(&mut m));
    check_eq!(m & sigbit(SIGUSR1), 0, "mask_restored_after_handler");

    // Query the installed action.
    let mut old = Sigaction::default();
    sigaction(SIGUSR1, None, Some(&mut old));
    check_eq!(old.handler, on_sig as *const () as usize, "sigaction_query");

    // SA_SIGINFO from kill(): si_code SI_USER (0), si_pid == sender.
    install_siginfo(SIGUSR2, on_siginfo);
    reset();
    kill(me, SIGUSR2);
    check_eq!(HITS.load(SeqCst), 1, "siginfo_handler_ran");
    check_eq!(SI_CODE.load(SeqCst), 0, "siginfo_si_code_user");
    check_eq!(SI_PID.load(SeqCst), me, "siginfo_si_pid");

    // tgkill: si_code SI_TKILL (-6).
    reset();
    tgkill(me, gettid(), SIGUSR2);
    check_eq!(SI_CODE.load(SeqCst), -6, "tgkill_si_code_tkill");

    // Blocked signals stay pending until unblocked.
    reset();
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR1)), None);
    kill(me, SIGUSR1);
    check_eq!(HITS.load(SeqCst), 0, "blocked_not_delivered");
    let mut pending = 0u64;
    unsafe { syscall!(nr::RT_SIGPENDING, &mut pending as *mut u64, 8) };
    check!(pending & sigbit(SIGUSR1) != 0, "sigpending_shows_blocked");
    // Standard signals do not queue: two sends, one delivery.
    kill(me, SIGUSR1);
    sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR1)), None);
    check_eq!(HITS.load(SeqCst), 1, "unblock_delivers_once");

    // SIGKILL/SIGSTOP cannot be caught or blocked.
    check_err!(signal(SIGKILL, on_sig, 0), EINVAL, "sigaction_sigkill_einval");
    check_err!(signal(SIGSTOP, on_sig, 0), EINVAL, "sigaction_sigstop_einval");

    // SIG_IGN discards.
    let ign = Sigaction { handler: SIG_IGN, flags: SA_RESTORER, restorer: restorer(), mask: 0 };
    sigaction(SIGHUP, Some(&ign), None);
    reset();
    kill(me, SIGHUP);
    let mut pending = 0u64;
    unsafe { syscall!(nr::RT_SIGPENDING, &mut pending as *mut u64, 8) };
    check!(HITS.load(SeqCst) == 0 && pending & sigbit(SIGHUP) == 0, "ignored_signal_discarded");

    // SA_RESETHAND: one-shot handler.
    signal(SIGALRM, on_sig, SA_RESETHAND);
    kill(me, SIGALRM);
    sigaction(SIGALRM, None, Some(&mut old));
    check_eq!(old.handler, SIG_DFL, "sa_resethand");

    // Default actions in children.
    let st = run_child(|| {
        kill(getpid(), SIGTERM);
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == SIGTERM, "default_action_terminate", "status {:#x}", st);
    let st = run_child(|| {
        unsafe { core::ptr::write_volatile(0x10 as *mut u32, 1) };
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == SIGSEGV, "null_write_sigsegv", "status {:#x}", st);
    let st = run_child(|| {
        install_siginfo(SIGSEGV, on_segv);
        unsafe { core::ptr::write_volatile(0x10 as *mut u32, 1) };
        0
    });
    check_eq!(status_code(st), 42, "sigsegv_handler_si_addr");
    let st = run_child(|| {
        unsafe { core::arch::asm!("ud2") };
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == 4, "ud2_sigill", "status {:#x}", st);
    let st = run_child(|| {
        // Integer divide by zero -> SIGFPE.
        let z = core::hint::black_box(0u64);
        unsafe { core::arch::asm!("div {0}", in(reg) z, inout("rax") 1u64 => _, inout("rdx") 0u64 => _) };
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == 8, "div_zero_sigfpe", "status {:#x}", st);
    let st = run_child(|| {
        unsafe { core::arch::asm!("int3") };
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == 5, "int3_sigtrap", "status {:#x}", st);

    // sigaltstack + SA_ONSTACK.
    let alt = alloc_pages(64 * 1024);
    let ss = Stack { sp: alt as usize, flags: 0, size: 64 * 1024 };
    check_ok!(unsafe { syscall!(nr::SIGALTSTACK, &ss as *const Stack, 0) }, "sigaltstack_set");
    signal(SIGUSR1, on_sig, SA_ONSTACK);
    kill(me, SIGUSR1);
    let sp = STACK_ADDR.load(SeqCst);
    check!(sp >= alt as usize && sp < alt as usize + 64 * 1024, "handler_on_altstack", "sp {:#x}", sp);
    let off = Stack { sp: 0, flags: 2, size: 0 }; // SS_DISABLE
    unsafe { syscall!(nr::SIGALTSTACK, &off as *const Stack, 0) };
    signal(SIGUSR1, on_sig, 0);

    // A handler without SA_RESTART interrupts nanosleep with EINTR.
    let pid = fork();
    if pid == 0 {
        sleep_ms(50);
        kill(getppid(), SIGUSR1);
        exit(0);
    }
    reset();
    let req = Timespec { sec: 5, nsec: 0 };
    let mut rem = Timespec::default();
    let t0 = now_ns();
    let r = nanosleep(&req, &mut rem);
    check_err!(r, EINTR, "nanosleep_eintr");
    check!(rem.sec > 0 && rem.sec <= 5, "nanosleep_remaining", "rem {:?}", rem);
    check!(now_ns() - t0 < 2_000_000_000, "nanosleep_woken_early");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);

    // rt_sigsuspend atomically unblocks and waits.
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR1)), None);
    let pid = fork();
    if pid == 0 {
        sleep_ms(20);
        kill(getppid(), SIGUSR1);
        exit(0);
    }
    reset();
    let empty = 0u64;
    let r = unsafe { syscall!(nr::RT_SIGSUSPEND, &empty as *const u64, 8) };
    check_err!(r, EINTR, "sigsuspend_eintr");
    check_eq!(HITS.load(SeqCst), 1, "sigsuspend_handler_ran");
    let mut m = 0u64;
    sigprocmask(SIG_BLOCK, None, Some(&mut m));
    check!(m & sigbit(SIGUSR1) != 0, "sigsuspend_mask_restored");
    wait4(pid as i32, &mut s, 0);
    sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR1)), None);

    // rt_sigtimedwait dequeues a blocked pending signal synchronously.
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR2)), None);
    kill(me, SIGUSR2);
    let set = sigbit(SIGUSR2);
    let ts = Timespec { sec: 1, nsec: 0 };
    let r = unsafe { syscall!(nr::RT_SIGTIMEDWAIT, &set as *const u64, 0, &ts as *const Timespec, 8) };
    check_eq!(r, SIGUSR2 as isize, "sigtimedwait_returns_signo");
    let ts = Timespec { sec: 0, nsec: 10_000_000 };
    let r = unsafe { syscall!(nr::RT_SIGTIMEDWAIT, &set as *const u64, 0, &ts as *const Timespec, 8) };
    check_err!(r, EAGAIN, "sigtimedwait_timeout_eagain");
    sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR2)), None);

    // SIGCHLD reaches the parent when a child exits.
    signal(SIGCHLD, on_sig, 0);
    reset();
    run_child(|| 0);
    check_eq!(LAST_SIG.load(SeqCst), SIGCHLD, "sigchld_on_child_exit");

    // Signal mask is inherited across fork; pending set is not.
    sigprocmask(SIG_BLOCK, Some(sigbit(SIGUSR1)), None);
    kill(me, SIGUSR1);
    let st = run_child(|| {
        let mut m = 0u64;
        let mut p = 0u64;
        sigprocmask(SIG_BLOCK, None, Some(&mut m));
        unsafe { syscall!(nr::RT_SIGPENDING, &mut p as *mut u64, 8) };
        ((m & sigbit(SIGUSR1) != 0) as i32) | (((p & sigbit(SIGUSR1) == 0) as i32) << 1)
    });
    check_eq!(status_code(st), 3, "fork_inherits_mask_not_pending");
    sigprocmask(SIG_UNBLOCK, Some(sigbit(SIGUSR1)), None);
}
