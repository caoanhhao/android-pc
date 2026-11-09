//! D3: POSIX timers and interval timers (lmkd's watchdog, alarm()).
//! timer_create (default sigevent = SIGALRM with the timer id as value,
//! SIGEV_SIGNAL with si_code SI_TIMER / si_timerid / si_value,
//! SIGEV_THREAD_ID, SIGEV_NONE, EINVAL for bad clocks and signals),
//! timer_settime (relative, TIMER_ABSTIME, periodic, disarm, old value),
//! timer_gettime, timer_getoverrun, timer_delete (EINVAL afterwards);
//! setitimer/getitimer(ITIMER_REAL) raising SIGALRM, alarm() returning
//! the seconds left.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicI32, AtomicU64, Ordering::SeqCst};
use rt::*;

rt::test_main!("d3_timers", main);

const NR_TIMER_CREATE: usize = 222;
const NR_TIMER_SETTIME: usize = 223;
const NR_TIMER_GETTIME: usize = 224;
const NR_TIMER_GETOVERRUN: usize = 225;
const NR_TIMER_DELETE: usize = 226;
const NR_GETITIMER: usize = 36;
const NR_ALARM: usize = 37;
const NR_SETITIMER: usize = 38;
const SIGALRM: i32 = 14;
const SIGRTMIN: i32 = 34;
const SI_TIMER: i32 = -2;
const TIMER_ABSTIME: usize = 1;

#[repr(C)]
#[derive(Default)]
struct Sigevent {
    value: u64,
    signo: i32,
    notify: i32,
    tid: i32,
    pad: [i32; 11],
}

static HITS: AtomicI32 = AtomicI32::new(0);
static CODE: AtomicI32 = AtomicI32::new(0);
static TIMERID: AtomicI32 = AtomicI32::new(-1);
static VALUE: AtomicU64 = AtomicU64::new(0);
static TID: AtomicI32 = AtomicI32::new(0);
static ALRM: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_timer(_: i32, info: *const Siginfo, _: usize) {
    let i = unsafe { &*info };
    HITS.fetch_add(1, SeqCst);
    CODE.store(i.code, SeqCst);
    TIMERID.store(i.u0 as u32 as i32, SeqCst);
    VALUE.store(i.u1, SeqCst);
    TID.store(gettid(), SeqCst);
}

extern "C" fn on_alrm(_: i32, _: *const Siginfo, _: usize) {
    ALRM.fetch_add(1, SeqCst);
}

fn install(sig: i32, f: extern "C" fn(i32, *const Siginfo, usize)) {
    let act = Sigaction { handler: f as *const () as usize, flags: SA_SIGINFO | SA_RESTORER | 0x1000_0000, restorer: restorer(), mask: 0 };
    sigaction(sig, Some(&act), None);
}

fn create(clock: i32, ev: Option<&Sigevent>) -> (isize, i32) {
    let mut id: i32 = -1;
    let r = unsafe { syscall!(NR_TIMER_CREATE, clock, ev.map_or(core::ptr::null(), |e| e as *const Sigevent), &mut id as *mut i32) };
    (r, id)
}

fn settime(id: i32, flags: usize, value_ns: i64, interval_ns: i64, old: Option<&mut [i64; 4]>) -> isize {
    let its = [interval_ns / 1_000_000_000, interval_ns % 1_000_000_000, value_ns / 1_000_000_000, value_ns % 1_000_000_000];
    unsafe { syscall!(NR_TIMER_SETTIME, id, flags, its.as_ptr(), old.map_or(core::ptr::null_mut(), |o| o.as_mut_ptr())) }
}

fn gettime(id: i32) -> [i64; 4] {
    let mut its = [0i64; 4];
    unsafe { syscall!(NR_TIMER_GETTIME, id, its.as_mut_ptr()) };
    its
}

fn wait_for(f: impl Fn() -> bool, ms: i64) -> bool {
    let end = now_ns() + ms * 1_000_000;
    while now_ns() < end {
        if f() {
            return true;
        }
        sleep_ms(5);
    }
    f()
}

fn main() {
    install(SIGRTMIN, on_timer);
    install(SIGALRM, on_alrm);

    check_err!(create(99, None).0, EINVAL, "timer_create_bad_clock");
    let bad = Sigevent { signo: 200, notify: 0, ..Default::default() };
    check_err!(create(CLOCK_MONOTONIC, Some(&bad)).0, EINVAL, "timer_create_bad_signal");

    // One-shot with SIGEV_SIGNAL.
    let ev = Sigevent { value: 0xfeed, signo: SIGRTMIN, notify: 0, ..Default::default() };
    let (r, id) = create(CLOCK_MONOTONIC, Some(&ev));
    check!(r == 0 && id >= 0, "timer_create", "r {} id {}", r, id);
    check_eq!(gettime(id), [0, 0, 0, 0], "gettime_disarmed");
    check_ok!(settime(id, 0, 30_000_000, 0, None), "settime_oneshot");
    let g = gettime(id);
    check!(g[2] == 0 && g[3] > 0 && g[3] <= 30_000_000, "gettime_armed", "{:?}", g);
    check!(wait_for(|| HITS.load(SeqCst) == 1, 2000), "oneshot_fired");
    check_eq!(CODE.load(SeqCst), SI_TIMER, "si_code_si_timer");
    check_eq!(TIMERID.load(SeqCst), id, "si_timerid");
    check_eq!(VALUE.load(SeqCst), 0xfeed, "si_value");
    sleep_ms(80);
    check_eq!(HITS.load(SeqCst), 1, "oneshot_fires_once");
    check_eq!(gettime(id), [0, 0, 0, 0], "oneshot_disarmed_after");

    // Periodic, then disarmed; old value reported.
    HITS.store(0, SeqCst);
    check_ok!(settime(id, 0, 10_000_000, 10_000_000, None), "settime_periodic");
    check!(wait_for(|| HITS.load(SeqCst) >= 4, 3000), "periodic_fires", "hits {}", HITS.load(SeqCst));
    let mut old = [0i64; 4];
    check_ok!(settime(id, 0, 0, 0, Some(&mut old)), "settime_disarm");
    check!(old[1] == 10_000_000 && old[3] > 0, "settime_old_value", "{:?}", old);
    let n = HITS.load(SeqCst);
    sleep_ms(60);
    check_eq!(HITS.load(SeqCst), n, "disarmed_stops");
    check!(unsafe { syscall!(NR_TIMER_GETOVERRUN, id) } >= 0, "getoverrun");

    // Absolute time on CLOCK_REALTIME.
    HITS.store(0, SeqCst);
    let (_, rid) = create(CLOCK_REALTIME, Some(&ev));
    let mut ts = Timespec::default();
    clock_gettime(CLOCK_REALTIME, &mut ts);
    let at = ts.sec * 1_000_000_000 + ts.nsec + 20_000_000;
    check_ok!(settime(rid, TIMER_ABSTIME, at, 0, None), "settime_abstime_realtime");
    check!(wait_for(|| HITS.load(SeqCst) == 1, 2000), "abstime_fired");
    check_ok!(unsafe { syscall!(NR_TIMER_DELETE, rid) }, "timer_delete");
    check_err!(unsafe { syscall!(NR_TIMER_DELETE, rid) }, EINVAL, "timer_delete_twice_einval");
    check_err!(settime(rid, 0, 1_000_000, 0, None), EINVAL, "settime_deleted_einval");

    // SIGEV_THREAD_ID: delivered to that thread (here: ourselves).
    HITS.store(0, SeqCst);
    let tev = Sigevent { value: 7, signo: SIGRTMIN, notify: 4, tid: gettid(), ..Default::default() };
    let (r, tid_timer) = create(CLOCK_BOOTTIME, Some(&tev));
    check_eq!(r, 0, "timer_create_thread_id");
    settime(tid_timer, 0, 10_000_000, 0, None);
    check!(wait_for(|| HITS.load(SeqCst) == 1, 2000) && TID.load(SeqCst) == gettid(), "thread_id_delivery");
    let none = Sigevent { notify: 1, ..Default::default() };
    let (r, nid) = create(CLOCK_MONOTONIC, Some(&none));
    check_eq!(r, 0, "timer_create_sigev_none");
    HITS.store(0, SeqCst);
    settime(nid, 0, 5_000_000, 0, None);
    sleep_ms(40);
    check_eq!(HITS.load(SeqCst), 0, "sigev_none_no_signal");
    unsafe { syscall!(NR_TIMER_DELETE, nid) };

    // Default sigevent: SIGALRM.
    let (r, did) = create(CLOCK_MONOTONIC, None);
    check_eq!(r, 0, "timer_create_default");
    settime(did, 0, 5_000_000, 0, None);
    check!(wait_for(|| ALRM.load(SeqCst) == 1, 2000), "default_sigalrm");
    unsafe { syscall!(NR_TIMER_DELETE, did) };

    // setitimer / getitimer / alarm.
    ALRM.store(0, SeqCst);
    let itv = [0i64, 0, 0, 20_000]; // interval 0, value 20 ms
    check_ok!(unsafe { syscall!(NR_SETITIMER, 0, itv.as_ptr(), 0) }, "setitimer_real");
    let mut cur = [0i64; 4];
    unsafe { syscall!(NR_GETITIMER, 0, cur.as_mut_ptr()) };
    check!(cur[2] == 0 && cur[3] > 0 && cur[3] <= 20_000, "getitimer_real", "{:?}", cur);
    check!(wait_for(|| ALRM.load(SeqCst) == 1, 2000), "setitimer_sigalrm");
    check_eq!(unsafe { syscall!(NR_ALARM, 100) }, 0, "alarm_none_pending");
    let left = unsafe { syscall!(NR_ALARM, 0) };
    check!(left == 100 || left == 99, "alarm_returns_left", "left {}", left);
    check_err!(unsafe { syscall!(NR_SETITIMER, 7, itv.as_ptr(), 0) }, EINVAL, "setitimer_bad_which");
}
