//! D1: sync_file fences, driven through sw_sync timelines as libsync's
//! tests do. SW_SYNC_IOC_CREATE_FENCE / INC, a fence's poll state before
//! and after its value, SYNC_IOC_FILE_INFO (file and per-fence status,
//! driver and timeline names, timestamps, the num_fences size check),
//! fences for values already reached, SYNC_IOC_MERGE (same timeline keeps
//! the later point, two timelines give two fences signalled only
//! together, a non-sync fd is EINVAL), a blocked poll and an epoll wait
//! woken by another process advancing the timeline, the
//! `anon_inode:sync_file` link, and pending fences failing when their
//! timeline is closed.
//!
//! Needs sw_sync (debugfs `sync/sw_sync` on Linux, root only): skipped
//! when it cannot be opened.
#![no_std]
#![no_main]

use rt::drm::*;
use rt::*;

rt::test_main!("d1_sync", main);

fn timeline() -> i32 {
    open(c"/sys/kernel/debug/sync/sw_sync".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32
}

fn fence(tl: i32, value: u32, name: &[u8]) -> i32 {
    let mut c = S::<40>::new();
    c.set32(0, value);
    c.0[4..4 + name.len()].copy_from_slice(name);
    if c.io(tl, SW_SYNC_IOC_CREATE_FENCE) < 0 { -1 } else { c.u32(36) as i32 }
}

fn inc(tl: i32, n: u32) -> isize {
    ioctl(tl, SW_SYNC_IOC_INC, &n as *const u32 as usize)
}

fn merge(a: i32, b: i32, name: &[u8]) -> isize {
    let mut m = S::<48>::new();
    m.0[..name.len()].copy_from_slice(name);
    m.set32(32, b as u32);
    let r = m.io(a, SYNC_IOC_MERGE);
    if r < 0 { r } else { m.u32(36) as i32 as isize }
}

fn main() {
    let tl = timeline();
    if tl < 0 {
        skip("all", "no sw_sync");
        return;
    }
    let f1 = fence(tl, 1, b"one");
    check!(f1 >= 0, "create_fence");
    check_eq!(poll_in(f1, 0), 0, "pending_not_ready");
    let i = sync_info(f1);
    check!(i.is_ok_and(|i| i.0 == 0 && i.1 == 1 && i.2 == 0 && i.3 == 0), "pending_info");
    if let Ok(i) = i {
        check!(&i.4[..7] == b"sw_sync", "fence_driver_name", "{}", Bytes(&i.4));
    }
    let mut l = [0u8; 64];
    let p = cpath!("/proc/self/fd/{}", f1);
    let n = readlink(p.as_ptr(), &mut l).max(0) as usize;
    check!(&l[..n] == b"anon_inode:sync_file", "sync_file_link", "{}", Bytes(&l[..n]));
    // num_fences smaller than the real count (but not 0) is EINVAL.
    let f2 = fence(tl, 2, b"two");
    let m = merge(f1, f2, b"m12") as i32;
    check!(m >= 0, "merge_same_timeline");
    let mut s = S::<56>::new();
    check!(s.io(m, SYNC_IOC_FILE_INFO) == 0 && s.u32(40) == 1, "merge_same_timeline_one_fence");
    check!(&s.0[..3] == b"m12", "merged_name");

    check_eq!(inc(tl, 1), 0, "timeline_inc");
    check!(poll_in(f1, 0) & 1 != 0, "signalled_ready");
    let i = sync_info(f1);
    check!(i.is_ok_and(|i| i.0 == 1 && i.2 == 1 && i.3 > 0), "signalled_info_timestamp");
    check_eq!(poll_in(f2, 0), 0, "later_point_still_pending");
    check_eq!(poll_in(m, 0), 0, "merge_keeps_later_point");
    let past = fence(tl, 1, b"past");
    check!(past >= 0 && poll_in(past, 0) & 1 != 0, "fence_for_past_value_signalled");

    // Two timelines: two fences, signalled only together.
    let tl2 = timeline();
    let g = fence(tl2, 5, b"g5");
    let both = merge(f2, g, b"both") as i32;
    check!(both >= 0, "merge_two_timelines");
    let mut big = S::<56>::new();
    big.set32(40, 1);
    let mut info = [0u8; 80];
    big.set64(48, info.as_mut_ptr() as u64);
    check_err!(big.io(both, SYNC_IOC_FILE_INFO), EINVAL, "file_info_num_fences_too_small");
    check!(sync_info(both).is_ok_and(|i| i.1 == 2), "two_fences");
    inc(tl, 1);
    check_eq!(poll_in(both, 0), 0, "half_signalled_pending");
    check!(poll_in(m, 0) & 1 != 0, "merged_same_timeline_done");
    inc(tl2, 5);
    check!(poll_in(both, 0) & 1 != 0, "both_signalled");
    let mut pfd = [0i32; 2];
    pipe2(&mut pfd, O_CLOEXEC);
    check_err!(merge(f1, pfd[0], b"x"), EINVAL, "merge_not_sync_file");
    check_err!(ioctl(pfd[0], SYNC_IOC_FILE_INFO, info.as_mut_ptr() as usize), ENOTTY_, "file_info_on_pipe");

    // A blocked poll, and an epoll wait, woken by another process.
    let f3 = fence(tl, 3, b"three");
    let pid = fork();
    if pid == 0 {
        sleep_ms(100);
        inc(tl, 1);
        exit(0);
    }
    let t0 = now_ns();
    let r = poll_in(f3, 5000);
    let dt = (now_ns() - t0) / 1_000_000;
    check!(r & 1 != 0 && (50..4000).contains(&dt), "poll_woken_by_signal", "r={r} {dt}ms");
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    let f4 = fence(tl, 4, b"four");
    let ep = epoll_create1(O_CLOEXEC) as i32;
    check_eq!(epoll_ctl(ep, 1, f4, EPOLLIN, 44), 0, "epoll_add_sync_file");
    let pid = fork();
    if pid == 0 {
        sleep_ms(50);
        inc(tl, 1);
        exit(0);
    }
    let mut evs = [EpollEvent::default(); 2];
    let n = epoll_wait(ep, &mut evs, 5000);
    let (ev_events, ev_data) = (evs[0].events, evs[0].data);
    check!(n == 1 && ev_events & EPOLLIN != 0 && ev_data == 44, "epoll_woken_by_signal");
    wait4(pid as i32, &mut st, 0);

    // Closing a timeline fails its pending fences.
    let tl3 = timeline();
    let orphan = fence(tl3, 10, b"orphan");
    close(tl3);
    let i = sync_info(orphan);
    check!(poll_in(orphan, 0) & 1 != 0 && i.is_ok_and(|i| i.0 < 0 && i.2 < 0), "closed_timeline_fails_fence");

    for fd in [f1, f2, m, past, g, both, f3, f4, orphan, ep, tl, tl2, pfd[0], pfd[1]] {
        close(fd);
    }
}

const ENOTTY_: i32 = 25;
