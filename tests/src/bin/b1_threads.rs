//! B1: threads and futex — clone(CLONE_THREAD...) as bionic's pthread_create
//! does it, CLONE_CHILD_CLEARTID join, FUTEX_WAIT/WAKE semantics, a futex
//! mutex under contention, exit_group killing all threads.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering::SeqCst};
use rt::*;

rt::test_main!("b1_threads", main);

static SHARED: AtomicUsize = AtomicUsize::new(0);
static THREAD_TID: AtomicI32 = AtomicI32::new(0);
static THREAD_PID: AtomicI32 = AtomicI32::new(0);
static GATE: AtomicI32 = AtomicI32::new(0);
static SAW_GATE: AtomicI32 = AtomicI32::new(0);

extern "C" fn simple(arg: usize) -> i32 {
    THREAD_TID.store(gettid(), SeqCst);
    THREAD_PID.store(getpid(), SeqCst);
    SHARED.fetch_add(arg, SeqCst);
    0
}

extern "C" fn waiter(_: usize) -> i32 {
    while GATE.load(SeqCst) == 0 {
        futex(GATE.as_ptr(), FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, core::ptr::null());
    }
    SAW_GATE.store(GATE.load(SeqCst), SeqCst);
    0
}

// A three-state futex mutex (Drepper, "Futexes are tricky"): 0 unlocked,
// 1 locked, 2 locked with waiters.
static LOCK: AtomicI32 = AtomicI32::new(0);
static mut COUNTER: u64 = 0;
const ITERS: u64 = 20000;
const NTHREADS: usize = 4;

fn lock() {
    if LOCK.compare_exchange(0, 1, SeqCst, SeqCst).is_ok() {
        return;
    }
    loop {
        if LOCK.swap(2, SeqCst) == 0 {
            return;
        }
        futex(LOCK.as_ptr(), FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 2, core::ptr::null());
    }
}

fn unlock() {
    if LOCK.swap(0, SeqCst) == 2 {
        futex(LOCK.as_ptr(), FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, core::ptr::null());
    }
}

extern "C" fn contender(_: usize) -> i32 {
    for i in 0..ITERS {
        lock();
        unsafe {
            let p = &raw mut COUNTER;
            p.write_volatile(p.read_volatile() + 1);
        }
        if i % 512 == 0 {
            unsafe { syscall!(nr::SCHED_YIELD) };
        }
        unlock();
    }
    0
}

extern "C" fn spinner(_: usize) -> i32 {
    loop {
        sleep_ms(1000);
    }
}

fn main() {
    // FUTEX_WAIT returns EAGAIN immediately if *uaddr != val.
    let word = AtomicU32::new(5);
    check_err!(futex(word.as_ptr() as *const i32, FUTEX_WAIT, 4, core::ptr::null()), EAGAIN,
               "futex_wait_value_mismatch");
    // ... and ETIMEDOUT after the relative timeout.
    let ts = Timespec { sec: 0, nsec: 30_000_000 };
    let t0 = now_ns();
    check_err!(futex(word.as_ptr() as *const i32, FUTEX_WAIT, 5, &ts), ETIMEDOUT, "futex_wait_timeout");
    let dt = now_ns() - t0;
    check!(dt >= 30_000_000 && dt < 2_000_000_000, "futex_timeout_duration", "{} ns", dt);
    check_eq!(futex(word.as_ptr() as *const i32, FUTEX_WAKE, 1, core::ptr::null()), 0, "futex_wake_no_waiters");
    check_err!(futex(3 as *const i32, FUTEX_WAIT, 0, core::ptr::null()), EINVAL, "futex_unaligned_einval");

    // set_tid_address returns the caller's tid.
    let mut dummy = 0i32;
    check_eq!(unsafe { syscall!(nr::SET_TID_ADDRESS, &mut dummy as *mut i32) } as i32, gettid(),
              "set_tid_address_returns_tid");
    unsafe { syscall!(nr::SET_TID_ADDRESS, 0) };

    // A thread shares memory and the pid, has its own tid.
    let t = spawn(simple, 7, 0);
    t.join();
    check_eq!(SHARED.load(SeqCst), 7, "thread_shared_memory");
    check_eq!(THREAD_PID.load(SeqCst), getpid(), "thread_getpid_is_tgid");
    check!(THREAD_TID.load(SeqCst) == t.tid && t.tid != getpid(), "thread_own_tid");

    // Futex handshake: the thread sleeps until we flip GATE and wake it.
    let t = spawn(waiter, 0, 0);
    sleep_ms(30);
    GATE.store(1, SeqCst);
    futex(GATE.as_ptr(), FUTEX_WAKE | FUTEX_PRIVATE_FLAG, i32::MAX, core::ptr::null());
    t.join();
    check_eq!(SAW_GATE.load(SeqCst), 1, "futex_wait_wake_handshake");

    // Mutual exclusion under contention.
    let mut ts: [Option<Thread>; NTHREADS] = [const { None }; NTHREADS];
    for t in ts.iter_mut() {
        *t = Some(spawn(contender, 0, 0));
    }
    for t in ts.iter().flatten() {
        t.join();
    }
    check_eq!(unsafe { (&raw const COUNTER).read_volatile() }, ITERS * NTHREADS as u64, "futex_mutex_counter");

    // tgkill addresses a specific thread (signal 0: existence check only).
    // The spinner lives until our final exit_group.
    let t = spawn(spinner, 0, 0);
    check_eq!(tgkill(getpid(), t.tid, 0), 0, "tgkill_thread_exists");
    check_err!(tgkill(getpid(), 1_000_000, 0), ESRCH, "tgkill_bad_tid");

    // exit_group from the main thread ends a process that has a busy thread.
    let st = run_child(|| {
        spawn(spinner, 0, 0);
        sleep_ms(10);
        5
    });
    check_eq!(status_code(st), 5, "exit_group_kills_threads");

    // A thread calling exit_group ends the whole process.
    extern "C" fn group_exit(_: usize) -> i32 {
        exit(6)
    }
    let st = run_child(|| {
        spawn(group_exit, 0, 0);
        loop {
            sleep_ms(1000);
        }
    });
    check_eq!(status_code(st), 6, "thread_exit_group");

    // After the main thread calls plain exit(), the process lives on (leader
    // is a zombie) until another thread ends it with exit_group.
    extern "C" fn finisher(_: usize) -> i32 {
        sleep_ms(20);
        exit(8)
    }
    let st = run_child(|| {
        spawn(finisher, 0, 0);
        exit_thread(0)
    });
    check_eq!(status_code(st), 8, "main_thread_exit_then_group_exit");
}
