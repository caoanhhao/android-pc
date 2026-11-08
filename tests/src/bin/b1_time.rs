//! B1: clocks and sleeping — clock_gettime/getres, nanosleep,
//! clock_nanosleep(TIMER_ABSTIME), gettimeofday, getrandom, sched_yield.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b1_time", main);

fn ts_ns(t: &Timespec) -> i64 {
    t.sec * 1_000_000_000 + t.nsec
}

fn main() {
    let mut a = Timespec::default();
    check_ok!(clock_gettime(CLOCK_MONOTONIC, &mut a), "clock_monotonic");
    let mut ok = true;
    let mut prev = ts_ns(&a);
    for _ in 0..10000 {
        let n = now_ns();
        ok &= n >= prev;
        prev = n;
    }
    check!(ok, "monotonic_never_goes_back");
    check!(a.nsec >= 0 && a.nsec < 1_000_000_000, "timespec_nsec_normalized");

    let mut rt_ = Timespec::default();
    check_ok!(clock_gettime(CLOCK_REALTIME, &mut rt_), "clock_realtime");
    // 2020-01-01: a kernel that never read the RTC would report ~1970.
    check!(rt_.sec > 1_577_836_800, "realtime_after_2020", "sec {}", rt_.sec);

    let mut bt = Timespec::default();
    check_ok!(clock_gettime(CLOCK_BOOTTIME, &mut bt), "clock_boottime");
    check!(ts_ns(&bt) >= ts_ns(&a), "boottime_ge_monotonic");

    let mut res = Timespec::default();
    check_ok!(unsafe { syscall!(nr::CLOCK_GETRES, CLOCK_MONOTONIC, &mut res as *mut Timespec) }, "clock_getres");
    check!(res.sec == 0 && res.nsec > 0 && res.nsec <= 10_000_000, "clock_res_sane", "{:?}", res);

    check_err!(clock_gettime(1234, &mut a), EINVAL, "clock_bad_id_einval");

    let mut tv = Timeval::default();
    check_ok!(unsafe { syscall!(nr::GETTIMEOFDAY, &mut tv as *mut Timeval, 0) }, "gettimeofday");
    let mut r2 = Timespec::default();
    clock_gettime(CLOCK_REALTIME, &mut r2);
    check!((tv.sec - r2.sec).abs() <= 1, "gettimeofday_matches_realtime");

    let t0 = now_ns();
    check_eq!(nanosleep(&Timespec { sec: 0, nsec: 50_000_000 }, core::ptr::null_mut()), 0, "nanosleep_ok");
    let dt = now_ns() - t0;
    check!(dt >= 50_000_000, "nanosleep_not_early", "{} ns", dt);
    check!(dt < 1_000_000_000, "nanosleep_not_too_late", "{} ns", dt);
    check_err!(nanosleep(&Timespec { sec: 0, nsec: 1_000_000_000 }, core::ptr::null_mut()), EINVAL,
               "nanosleep_bad_nsec_einval");
    check_err!(nanosleep(&Timespec { sec: -1, nsec: 0 }, core::ptr::null_mut()), EINVAL, "nanosleep_negative_einval");

    // Absolute sleep until now + 30ms.
    let target = now_ns() + 30_000_000;
    let abs = Timespec { sec: target / 1_000_000_000, nsec: target % 1_000_000_000 };
    let r = unsafe { syscall!(nr::CLOCK_NANOSLEEP, CLOCK_MONOTONIC, TIMER_ABSTIME, &abs as *const Timespec, 0) };
    check_eq!(r, 0, "clock_nanosleep_abstime");
    check!(now_ns() >= target, "clock_nanosleep_reached_target");
    // An absolute time in the past returns immediately.
    let past = Timespec { sec: 0, nsec: 1 };
    let t0 = now_ns();
    unsafe { syscall!(nr::CLOCK_NANOSLEEP, CLOCK_MONOTONIC, TIMER_ABSTIME, &past as *const Timespec, 0) };
    check!(now_ns() - t0 < 100_000_000, "clock_nanosleep_past_immediate");

    let mut buf = [0u8; 32];
    let r = unsafe { syscall!(nr::GETRANDOM, buf.as_mut_ptr(), buf.len(), 0) };
    check_eq!(r, 32, "getrandom_full");
    check!(buf.iter().any(|&b| b != 0), "getrandom_nonzero");
    let mut buf2 = [0u8; 32];
    unsafe { syscall!(nr::GETRANDOM, buf2.as_mut_ptr(), buf2.len(), 0) };
    check!(buf != buf2, "getrandom_differs");

    check_eq!(unsafe { syscall!(nr::SCHED_YIELD) }, 0, "sched_yield");
}

const TIMER_ABSTIME: i32 = 1;
