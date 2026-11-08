//! B3: address space management — anonymous mmap, MAP_FIXED,
//! MAP_FIXED_NOREPLACE, munmap, mprotect enforcement, mremap, madvise, brk,
//! demand paging of large regions.
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use rt::*;

rt::test_main!("b3_mmap", main);

const PG: usize = 4096;

fn faults(f: impl FnOnce()) -> i32 {
    let st = run_child(|| {
        f();
        0
    });
    if wifsignaled(st) { wtermsig(st) } else { 0 }
}

fn main() {
    // Anonymous memory is zeroed and writable.
    let r = mmap(0, 4 * PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    check!(r > 0 && r as usize % PG == 0, "mmap_anon_aligned", "returned {:#x}", r);
    let p = r as *mut u8;
    let zero = (0..4 * PG).all(|i| unsafe { rd(p.add(i)) } == 0);
    check!(zero, "mmap_anon_zeroed");
    unsafe {
        wr(p, 0xaa);
        wr(p.add(4 * PG - 1), 0xbb);
    }
    check!(unsafe { rd(p) == 0xaa && rd(p.add(4 * PG - 1)) == 0xbb }, "mmap_anon_rw");

    // Bad arguments.
    check_err!(mmap(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0), EINVAL, "mmap_zero_len_einval");
    check_err!(mmap(0, PG, PROT_READ, MAP_ANONYMOUS, -1, 0), EINVAL, "mmap_no_type_einval");
    check_err!(mmap(0, PG, PROT_READ, MAP_PRIVATE, 999, 0), EBADF, "mmap_bad_fd_ebadf");
    check_err!(mmap(123, PG, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0), EINVAL,
               "mmap_fixed_unaligned_einval");

    // munmap the middle page: neighbours survive, the hole faults.
    check_ok!(munmap(unsafe { p.add(PG) }, PG), "munmap_middle");
    check!(unsafe { rd(p) == 0xaa && rd(p.add(4 * PG - 1)) == 0xbb }, "munmap_neighbours_intact");
    check_eq!(faults(|| unsafe { wr(p.add(PG), 1) }), SIGSEGV, "access_unmapped_sigsegv");
    check_err!(munmap(unsafe { p.add(1) }, PG), EINVAL, "munmap_unaligned_einval");

    // MAP_FIXED into the hole replaces nothing else; MAP_FIXED_NOREPLACE refuses.
    let hole = unsafe { p.add(PG) } as usize;
    let r = mmap(hole, PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    check_eq!(r as usize, hole, "mmap_fixed_exact");
    check_err!(mmap(hole, PG, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0), EEXIST,
               "map_fixed_noreplace_eexist");
    // MAP_FIXED over an existing mapping replaces it with fresh zeroes.
    let r = mmap(p as usize, PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    check!(r as usize == p as usize && unsafe { rd(p) } == 0, "map_fixed_replaces");

    // mprotect.
    unsafe { wr(p, 7) };
    check_ok!(mprotect(p, PG, PROT_READ), "mprotect_readonly");
    check_eq!(unsafe { rd(p) }, 7, "readonly_still_readable");
    check_eq!(faults(|| unsafe { wr(p, 1) }), SIGSEGV, "write_readonly_sigsegv");
    check_ok!(mprotect(p, PG, PROT_NONE), "mprotect_none");
    check_eq!(faults(|| unsafe {
        rd(p);
    }), SIGSEGV, "read_prot_none_sigsegv");
    check_ok!(mprotect(p, PG, PROT_READ | PROT_WRITE), "mprotect_rw_again");
    check_eq!(unsafe { rd(p) }, 7, "contents_kept_across_mprotect");
    check_err!(mprotect(unsafe { p.add(1) }, PG, PROT_READ), EINVAL, "mprotect_unaligned_einval");
    // Executing from a non-exec page faults; PROT_EXEC makes it runnable.
    let code = alloc_pages(PG);
    unsafe {
        // mov eax, 42; ret
        for (i, b) in [0xb8u8, 42, 0, 0, 0, 0xc3].iter().enumerate() {
            wr(code.add(i), *b);
        }
    }
    let f: extern "C" fn() -> i32 = unsafe { core::mem::transmute(code) };
    check_eq!(faults(|| {
        f();
    }), SIGSEGV, "exec_noexec_sigsegv");
    mprotect(code, PG, PROT_READ | PROT_EXEC);
    check_eq!(f(), 42, "exec_after_prot_exec");

    // mremap: grow in place or move, contents preserved.
    let q = alloc_pages(PG);
    unsafe { wr(q.add(100), 0x5c) };
    let r = unsafe { syscall!(nr::MREMAP, q, PG, 16 * PG, MREMAP_MAYMOVE) };
    check!(r > 0, "mremap_grow", "returned {}", errname(r));
    if r > 0 {
        let q2 = r as *mut u8;
        check_eq!(unsafe { rd(q2.add(100)) }, 0x5c, "mremap_preserves_contents");
        unsafe { wr(q2.add(16 * PG - 1), 1) };
        check!(unsafe { rd(q2.add(16 * PG - 1)) == 1 && rd(q2.add(8 * PG)) == 0 }, "mremap_new_tail_usable");
        let r = unsafe { syscall!(nr::MREMAP, q2, 16 * PG, PG, 0) };
        check_eq!(r as usize, q2 as usize, "mremap_shrink_in_place");
    }

    // madvise(DONTNEED) on private anon memory drops contents to zero.
    let m = alloc_pages(2 * PG);
    unsafe { wr(m, 9) };
    check_ok!(unsafe { syscall!(nr::MADVISE, m, 2 * PG, MADV_DONTNEED) }, "madvise_dontneed");
    check_eq!(unsafe { rd(m) }, 0, "dontneed_zeroes");

    // A large sparse mapping is demand-paged (touch a few pages only).
    let big = 1usize << 30;
    let r = mmap(0, big, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    check!(r > 0, "mmap_1gib_lazy", "returned {}", errname(r));
    if r > 0 {
        let b = r as *mut u8;
        let mut ok = true;
        for i in (0..big).step_by(64 << 20) {
            unsafe { wr(b.add(i), (i >> 20) as u8) };
        }
        for i in (0..big).step_by(64 << 20) {
            ok &= unsafe { rd(b.add(i)) } == (i >> 20) as u8;
        }
        check!(ok, "sparse_pages_hold_data");
        check_ok!(munmap(b, big), "munmap_1gib");
    }

    // brk: query, grow, use, shrink.
    let cur = brk(0);
    check!(cur > 0, "brk_query");
    let grown = brk(cur + 64 * 1024);
    check_eq!(grown, cur + 64 * 1024, "brk_grow");
    let hp = cur as *mut u8;
    unsafe {
        wr(hp, 1);
        wr(hp.add(64 * 1024 - 1), 2);
    }
    check!(unsafe { rd(hp) == 1 && rd(hp.add(64 * 1024 - 1)) == 2 }, "brk_memory_usable");
    check_eq!(brk(cur), cur, "brk_shrink");
    check_eq!(faults(|| unsafe { wr(hp.add(32 * 1024), 1) }), SIGSEGV, "brk_released_sigsegv");
    // brk below the start of the heap fails (returns the current break).
    check_eq!(brk(4096), cur, "brk_too_low_unchanged");

    // Stack grows on demand beyond the initial pages (recursion uses ~1 MiB).
    fn recurse(n: usize) -> usize {
        let pad = core::hint::black_box([n as u8; 1024]);
        if n == 0 { pad[0] as usize } else { recurse(n - 1) + pad[1] as usize % 2 }
    }
    let st = run_child(|| (recurse(1000) < 10_000) as i32);
    check_eq!(status_code(st), 1, "stack_growth_1mib");
}
