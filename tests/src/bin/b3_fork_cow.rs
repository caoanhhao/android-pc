//! B3: fork copy-on-write and shared mappings — private pages diverge after
//! fork in both directions, MAP_SHARED anon is shared, read-only pages stay
//! read-only in the child, many-page COW stress.
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use rt::*;

rt::test_main!("b3_fork_cow", main);

const PG: usize = 4096;

static mut GLOBAL: u64 = 1; // .data page

fn main() {
    let p = alloc_pages(PG) as *mut u64;
    unsafe { wr(p, 100) };

    // Child writes; parent unaffected.
    let st = run_child(|| unsafe {
        wr(p, 200);
        (rd(p) == 200) as i32
    });
    check_eq!(status_code(st), 1, "child_sees_own_write");
    check_eq!(unsafe { rd(p) }, 100, "parent_unaffected_by_child_write");

    // Parent writes after fork; child still sees the old value.
    let mut fds = [0i32; 2];
    pipe2(&mut fds, 0);
    let (r0, w1) = (fds[0], fds[1]);
    let pid = fork();
    if pid == 0 {
        let mut b = [0u8; 1];
        read(r0, &mut b); // wait for the parent's write
        exit(unsafe { rd(p) } as i32);
    }
    unsafe { wr(p, 101) };
    write(w1, b"x");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);
    check_eq!(status_code(s), 100, "child_unaffected_by_parent_write");
    close(r0);
    close(w1);

    // .data segment is COW as well.
    let st = run_child(|| unsafe {
        let g = &raw mut GLOBAL;
        wr(g, 5);
        rd(g) as i32
    });
    check_eq!(status_code(st), 5, "data_segment_child_write");
    check_eq!(unsafe { rd(&raw const GLOBAL) }, 1, "data_segment_parent_unchanged");

    // MAP_SHARED anonymous memory is shared across fork.
    let r = mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check!(r > 0, "mmap_shared_anon");
    let sh = r as *mut u64;
    let st = run_child(|| {
        unsafe { wr(sh, 0xdead) };
        0
    });
    check!(st == 0 && unsafe { rd(sh) } == 0xdead, "shared_anon_visible_to_parent");

    // Read-only private pages stay read-only in the child.
    let ro = alloc_pages(PG);
    mprotect(ro, PG, PROT_READ);
    let st = run_child(|| {
        unsafe { wr(ro, 1) };
        0
    });
    check!(wifsignaled(st) && wtermsig(st) == SIGSEGV, "readonly_stays_readonly_in_child");

    // Stress: 256 pages, child rewrites all, parent verifies its copy; then
    // a second generation (grandchild) does the same.
    const N: usize = 256;
    let big = alloc_pages(N * PG);
    for i in 0..N {
        unsafe { wr(big.add(i * PG) as *mut u64, i as u64) };
    }
    let st = run_child(|| {
        let mut ok = true;
        for i in 0..N {
            unsafe {
                let q = big.add(i * PG) as *mut u64;
                ok &= rd(q) == i as u64;
                wr(q, !(i as u64));
            }
        }
        let gst = run_child(|| {
            let mut ok = true;
            for i in 0..N {
                unsafe {
                    let q = big.add(i * PG) as *mut u64;
                    ok &= rd(q) == !(i as u64);
                    wr(q, 0);
                }
            }
            ok as i32
        });
        for i in 0..N {
            ok &= unsafe { rd(big.add(i * PG) as *const u64) } == !(i as u64);
        }
        (ok && status_code(gst) == 1) as i32
    });
    check_eq!(status_code(st), 1, "cow_stress_child_and_grandchild");
    let ok = (0..N).all(|i| unsafe { rd(big.add(i * PG) as *const u64) } == i as u64);
    check!(ok, "cow_stress_parent_intact");

    // Many sequential forks don't leak/corrupt (each child touches memory).
    let mut all = true;
    for k in 0..50u64 {
        let st = run_child(|| unsafe {
            wr(p, k);
            (rd(p) == k) as i32
        });
        all &= status_code(st) == 1;
    }
    check!(all && unsafe { rd(p) } == 101, "fifty_forks");
}
