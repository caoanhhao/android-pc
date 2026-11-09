//! D3: userfaultfd as ART's concurrent mark-compact GC uses it.
//! userfaultfd(2) flags (UFFD_USER_MODE_ONLY, EINVAL for unknown ones),
//! read/poll before UFFDIO_API (EINVAL / POLLERR), UFFDIO_API features
//! (SIGBUS, MISSING_SHMEM, MINOR_SHMEM, MOVE) and its one-shot rule,
//! MREMAP_DONTUNMAP of shared anonymous memory (ART's probe),
//! UFFDIO_REGISTER (returned ioctls, EINVAL for unaligned ranges, EBUSY
//! for a second context); missing faults waking a monitor (blocked
//! thread, uffd_msg, POLLIN) resolved by UFFDIO_COPY; COPY/ZEROPAGE
//! results (EEXIST, EAGAIN with partial counts); SIGBUS mode (BUS_ADRERR
//! at the faulting address, the handler fills the page, kernel accesses
//! get EFAULT); ART's register + mremap(DONTUNMAP) pattern; minor faults
//! on shmem resolved by UFFDIO_CONTINUE; missing shmem pages filled by
//! UFFDIO_COPY and seen through another mapping; UFFDIO_MOVE (with
//! ALLOW_SRC_HOLES); fork children not inheriting registrations;
//! UNREGISTER and close; /dev/userfaultfd's USERFAULTFD_IOC_NEW.
//!
//! On the Linux host the monitor needs UFFD_USER_MODE_ONLY (unprivileged
//! userfaultfd) and /dev/userfaultfd may be root-only (then skipped).
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering::SeqCst};
use rt::*;

rt::test_main!("d3_uffd", main);

const PG: usize = 4096;
const NR_USERFAULTFD: usize = 323;
const NR_MREMAP: usize = 25;
const UFFD_USER_MODE_ONLY: usize = 1;
const UFFD_API: u64 = 0xaa;

const FEATURE_MISSING_SHMEM: u64 = 1 << 5;
const FEATURE_SIGBUS: u64 = 1 << 7;
const FEATURE_MINOR_SHMEM: u64 = 1 << 10;
const FEATURE_MOVE: u64 = 1 << 16;

const MODE_MISSING: u64 = 1;
const MODE_MINOR: u64 = 4;

const fn iowr(nr: u32, size: u32) -> u32 {
    (3 << 30) | (size << 16) | (0xaa << 8) | nr
}
const fn ior(nr: u32, size: u32) -> u32 {
    (2 << 30) | (size << 16) | (0xaa << 8) | nr
}
const UFFDIO_API: u32 = iowr(0x3f, 24);
const UFFDIO_REGISTER: u32 = iowr(0, 32);
const UFFDIO_UNREGISTER: u32 = ior(1, 16);
const UFFDIO_WAKE: u32 = ior(2, 16);
const UFFDIO_COPY: u32 = iowr(3, 40);
const UFFDIO_ZEROPAGE: u32 = iowr(4, 32);
const UFFDIO_MOVE: u32 = iowr(5, 40);
const UFFDIO_CONTINUE: u32 = iowr(7, 32);
const USERFAULTFD_IOC_NEW: u32 = 0xaa00;

const MREMAP_MAYMOVE: usize = 1;
const MREMAP_FIXED: usize = 2;
const MREMAP_DONTUNMAP: usize = 4;
const MOVE_ALLOW_SRC_HOLES: u64 = 2;
const BUS_ADRERR: i32 = 2;

fn uffd_new(flags: usize) -> isize {
    unsafe { syscall!(NR_USERFAULTFD, flags) }
}

/// A context with UFFDIO_API done; returns (fd, features offered).
fn uffd_api(features: u64) -> (i32, u64) {
    let fd = uffd_new(O_CLOEXEC as usize | UFFD_USER_MODE_ONLY);
    assert!(fd >= 0, "userfaultfd: {}", errname(fd));
    let mut api = [UFFD_API, features, 0];
    let r = ioctl(fd as i32, UFFDIO_API, api.as_mut_ptr() as usize);
    assert!(r == 0, "UFFDIO_API({features:#x}): {}", errname(r));
    (fd as i32, api[1])
}

fn register(fd: i32, addr: usize, len: usize, mode: u64) -> (isize, u64) {
    let mut reg = [addr as u64, len as u64, mode, 0];
    let r = ioctl(fd, UFFDIO_REGISTER, reg.as_mut_ptr() as usize);
    (r, reg[3])
}

fn unregister(fd: i32, addr: usize, len: usize) -> isize {
    let range = [addr as u64, len as u64];
    ioctl(fd, UFFDIO_UNREGISTER, range.as_ptr() as usize)
}

/// UFFDIO_COPY: (ioctl result, .copy).
fn copy(fd: i32, dst: usize, src: usize, len: usize) -> (isize, i64) {
    let mut c = [dst as u64, src as u64, len as u64, 0, 0];
    let r = ioctl(fd, UFFDIO_COPY, c.as_mut_ptr() as usize);
    (r, c[4] as i64)
}

fn zeropage(fd: i32, dst: usize, len: usize) -> (isize, i64) {
    let mut z = [dst as u64, len as u64, 0, 0];
    let r = ioctl(fd, UFFDIO_ZEROPAGE, z.as_mut_ptr() as usize);
    (r, z[3] as i64)
}

fn cont(fd: i32, dst: usize, len: usize) -> (isize, i64) {
    let mut z = [dst as u64, len as u64, 0, 0];
    let r = ioctl(fd, UFFDIO_CONTINUE, z.as_mut_ptr() as usize);
    (r, z[3] as i64)
}

fn mv(fd: i32, dst: usize, src: usize, len: usize, mode: u64) -> (isize, i64) {
    let mut m = [dst as u64, src as u64, len as u64, mode, 0];
    let r = ioctl(fd, UFFDIO_MOVE, m.as_mut_ptr() as usize);
    (r, m[4] as i64)
}

fn anon(len: usize) -> usize {
    let a = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert!(a > 0, "mmap: {}", errname(a));
    a as usize
}

fn mremap(old: usize, old_len: usize, new_len: usize, flags: usize, new: usize) -> isize {
    unsafe { syscall!(NR_MREMAP, old, old_len, new_len, flags, new) }
}

fn page_of(v: u8) -> [u8; PG] {
    [v; PG]
}

// --- thread mode: a thread faults, the main thread is the monitor ------

static T_ADDR: AtomicUsize = AtomicUsize::new(0);
static T_VAL: AtomicI32 = AtomicI32::new(-1);

extern "C" fn toucher(_: usize) -> i32 {
    let p = T_ADDR.load(SeqCst) as *const u8;
    T_VAL.store(unsafe { rd(p.add(7)) } as i32, SeqCst);
    0
}

// --- SIGBUS mode: the handler fills the page ---------------------------

static S_FD: AtomicI32 = AtomicI32::new(-1);
/// 0: UFFDIO_COPY from S_SRC, 1: UFFDIO_CONTINUE, 2: UFFDIO_ZEROPAGE.
static S_HOW: AtomicI32 = AtomicI32::new(0);
static S_SRC: AtomicUsize = AtomicUsize::new(0);
static S_HITS: AtomicI32 = AtomicI32::new(0);
static S_CODE: AtomicI32 = AtomicI32::new(0);
static S_ADDR: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_sigbus(_: i32, info: *const Siginfo, _: usize) {
    let info = unsafe { &*info };
    let addr = info.u0 as usize;
    S_HITS.fetch_add(1, SeqCst);
    S_CODE.store(info.code, SeqCst);
    S_ADDR.store(addr, SeqCst);
    let page = addr & !(PG - 1);
    let fd = S_FD.load(SeqCst);
    let r = match S_HOW.load(SeqCst) {
        0 => copy(fd, page, S_SRC.load(SeqCst), PG).0,
        1 => cont(fd, page, PG).0,
        _ => zeropage(fd, page, PG).0,
    };
    if r != 0 {
        exit(77);
    }
}

fn install_sigbus() {
    let act = Sigaction { handler: on_sigbus as *const () as usize, flags: SA_SIGINFO | SA_RESTORER, restorer: restorer(), mask: 0 };
    sigaction(SIGBUS, Some(&act), None);
}

fn main() {
    // --- creation and API -------------------------------------------------
    check_err!(uffd_new(0x80 | UFFD_USER_MODE_ONLY), EINVAL, "userfaultfd_bad_flags_einval");
    let fd0 = uffd_new(O_CLOEXEC as usize | O_NONBLOCK as usize | UFFD_USER_MODE_ONLY);
    if fd0 == -(EPERM as isize) || fd0 == -(ENOSYS as isize) {
        skip("all", "userfaultfd not available");
        return;
    }
    let fd0 = check_ok!(fd0, "userfaultfd_create") as i32;
    check_eq!(fcntl(fd0, F_GETFD, 0), FD_CLOEXEC as isize, "userfaultfd_cloexec");
    let mut buf = [0u8; 64];
    check_err!(read(fd0, &mut buf), EINVAL, "read_before_api_einval");
    let mut pfd = [Pollfd { fd: fd0, events: POLLIN, revents: 0 }];
    poll(&mut pfd, 0);
    check!(pfd[0].revents & POLLERR != 0, "poll_before_api_pollerr", "revents {:#x}", pfd[0].revents);
    let mut bad = [0x55u64, 0, 0];
    check_err!(ioctl(fd0, UFFDIO_API, bad.as_mut_ptr() as usize), EINVAL, "api_wrong_version_einval");
    let mut api = [UFFD_API, 0, 0];
    check_ok!(ioctl(fd0, UFFDIO_API, api.as_mut_ptr() as usize), "api");
    let feats = api[1];
    check!(feats & FEATURE_SIGBUS != 0, "api_feature_sigbus", "features {feats:#x}");
    check!(feats & FEATURE_MISSING_SHMEM != 0 && feats & FEATURE_MINOR_SHMEM != 0, "api_feature_shmem", "features {feats:#x}");
    check!(api[2] & 1 != 0 && api[2] & 2 != 0 && api[2] & (1 << 0x3f) != 0, "api_ioctls", "ioctls {:#x}", api[2]);
    let mut again = [UFFD_API, 0, 0];
    check_err!(ioctl(fd0, UFFDIO_API, again.as_mut_ptr() as usize), EINVAL, "api_twice_einval");
    check_err!(read(fd0, &mut buf), EAGAIN, "read_nothing_eagain");
    close(fd0);

    // ART's MREMAP_DONTUNMAP probe on shared anonymous memory.
    let sh = mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0) as usize;
    let moved = mremap(sh, PG, PG, MREMAP_MAYMOVE | MREMAP_DONTUNMAP, 0);
    check!(moved > 0 && moved as usize != sh, "mremap_dontunmap_shared_anon", "returned {}", moved);
    munmap(sh as *mut u8, PG);
    munmap(moved as *mut u8, PG);

    // --- register -----------------------------------------------------------
    let (fd, _) = uffd_api(0);
    let a = anon(4 * PG);
    let (r, ioctls) = register(fd, a, 4 * PG, MODE_MISSING);
    check_eq!(r, 0, "register_missing");
    let want = (1u64 << 2) | (1 << 3) | (1 << 4); // WAKE, COPY, ZEROPAGE
    check!(ioctls & want == want, "register_ioctls", "ioctls {ioctls:#x}");
    check_eq!(register(fd, a + 1, PG, MODE_MISSING).0, -(EINVAL as isize), "register_unaligned_einval");
    check_eq!(register(fd, a, 4 * PG, 0).0, -(EINVAL as isize), "register_no_mode_einval");
    let (other, _) = uffd_api(0);
    check_eq!(register(other, a, PG, MODE_MISSING).0, -(EBUSY as isize), "register_other_ctx_ebusy");
    close(other);

    // --- missing fault, thread mode --------------------------------------------
    T_ADDR.store(a, SeqCst);
    let t = spawn(toucher, 0, 0);
    let mut pfd = [Pollfd { fd, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 5000), 1, "poll_pollin_on_fault");
    let mut msg = [0u64; 8];
    let n = read(fd, unsafe { core::slice::from_raw_parts_mut(msg.as_mut_ptr() as *mut u8, 64) });
    check_eq!(n, 32, "read_one_msg");
    check_eq!(msg[0] & 0xff, 0x12, "msg_event_pagefault");
    check_eq!(msg[2], a as u64, "msg_address_page_aligned");
    check_eq!(msg[1] & 1, 0, "msg_flags_read_fault");
    check_eq!(T_VAL.load(SeqCst), -1, "thread_blocked");
    let src = page_of(0x5a);
    check_eq!(copy(fd, a, src.as_ptr() as usize, PG), (0, PG as i64), "copy_resolves_fault");
    t.join();
    check_eq!(T_VAL.load(SeqCst), 0x5a, "thread_sees_copied_page");

    // COPY / ZEROPAGE results.
    check_eq!(copy(fd, a, src.as_ptr() as usize, PG), (-(EEXIST as isize), -(EEXIST as i64)), "copy_present_eexist");
    let (r, c) = copy(fd, a + 3 * PG, src.as_ptr() as usize, PG);
    check_eq!((r, c), (0, PG as i64), "copy_last_page");
    let two = [0x11u8; 2 * PG];
    check_eq!(copy(fd, a + 2 * PG, two.as_ptr() as usize, 2 * PG), (-(EAGAIN as isize), PG as i64), "copy_partial_eagain");
    check_eq!(unsafe { rd((a + 2 * PG) as *const u8) }, 0x11, "copy_partial_data");
    check_eq!(zeropage(fd, a + PG, PG), (0, PG as i64), "zeropage");
    check_eq!(unsafe { rd((a + PG + 100) as *const u8) }, 0, "zeropage_reads_zero");
    check_eq!(zeropage(fd, a + PG, PG).1, -(EEXIST as i64), "zeropage_present_eexist");
    let unreg = anon(PG);
    check_eq!(copy(fd, unreg, src.as_ptr() as usize, PG).0, -(ENOENT as isize), "copy_unregistered_enoent");
    let range = [a as u64, PG as u64];
    check_ok!(ioctl(fd, UFFDIO_WAKE, range.as_ptr() as usize), "wake");

    // Kernel access to a missing page of a USER_MODE_ONLY context: EFAULT.
    check_ok!(unsafe { syscall!(28, a, 4 * PG, 4) }, "madvise_dontneed_registered"); // MADV_DONTNEED
    let mut p = [0i32; 2];
    pipe2(&mut p, O_NONBLOCK as i32);
    check_err!(write(p[1], unsafe { core::slice::from_raw_parts(a as *const u8, 16) }), EFAULT, "kernel_read_user_only_efault");
    close(p[0]);
    close(p[1]);

    // Unregister: plain demand-zero again.
    check_ok!(unregister(fd, a, 4 * PG), "unregister");
    check_eq!(unsafe { rd(a as *const u8) }, 0, "unregistered_zero_fill");
    close(fd);

    // --- SIGBUS mode -------------------------------------------------------------
    install_sigbus();
    let (fd, _) = uffd_api(FEATURE_SIGBUS);
    S_FD.store(fd, SeqCst);
    let b = anon(4 * PG);
    check_eq!(register(fd, b, 4 * PG, MODE_MISSING).0, 0, "sigbus_register");
    let src = page_of(0xa7);
    S_SRC.store(src.as_ptr() as usize, SeqCst);
    S_HOW.store(0, SeqCst);
    unsafe { wr((b + PG + 9) as *mut u8, 3) };
    check_eq!(S_HITS.load(SeqCst), 1, "sigbus_raised");
    check_eq!(S_CODE.load(SeqCst), BUS_ADRERR, "sigbus_bus_adrerr");
    check_eq!(S_ADDR.load(SeqCst), b + PG + 9, "sigbus_si_addr");
    check!(unsafe { rd((b + PG) as *const u8) } == 0xa7 && unsafe { rd((b + PG + 9) as *const u8) } == 3, "sigbus_handler_filled_page");
    // The kernel never raises SIGBUS for its own accesses: EFAULT.
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    write(p[1], b"0123456789");
    check_err!(read(p[0], unsafe { core::slice::from_raw_parts_mut((b + 2 * PG) as *mut u8, 10) }), EFAULT, "sigbus_kernel_write_efault");
    close(p[0]);
    close(p[1]);
    check_eq!(S_HITS.load(SeqCst), 1, "sigbus_not_for_kernel_access");

    // A fork child does not inherit the registration.
    let st = run_child(|| {
        let v = unsafe { rd((b + 3 * PG) as *const u8) };
        if S_HITS.load(SeqCst) == 1 && v == 0 { 0 } else { 1 }
    });
    check_eq!(st, 0, "fork_child_not_registered");

    // ART's pattern: register, then mremap(DONTUNMAP) the pages to a shadow;
    // the registered range is empty and faults, the shadow keeps the data.
    let c = anon(2 * PG);
    unsafe {
        wr(c as *mut u8, 0x61);
        wr((c + PG) as *mut u8, 0x62);
    }
    let shadow = anon(2 * PG);
    check_eq!(register(fd, c, 2 * PG, MODE_MISSING).0, 0, "art_register");
    let r = mremap(c, 2 * PG, 2 * PG, MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP, shadow);
    check_eq!(r as usize, shadow, "art_mremap_dontunmap_fixed");
    check!(unsafe { rd(shadow as *const u8) } == 0x61 && unsafe { rd((shadow + PG) as *const u8) } == 0x62, "art_shadow_has_data");
    S_SRC.store(shadow + PG, SeqCst);
    let hits = S_HITS.load(SeqCst);
    check_eq!(unsafe { rd((c + PG) as *const u8) }, 0x62, "art_fault_copies_from_shadow");
    check_eq!(S_HITS.load(SeqCst), hits + 1, "art_old_range_still_registered");
    S_HOW.store(2, SeqCst);
    check_eq!(unsafe { rd(c as *const u8) }, 0, "art_zeropage_from_handler");
    // The shadow (new range) is not registered: demand-zero.
    check_ok!(unsafe { syscall!(28, shadow, PG, 4) }, "art_shadow_dontneed");
    check_eq!(unsafe { rd(shadow as *const u8) }, 0, "art_shadow_not_registered");
    check_eq!(S_HITS.load(SeqCst), hits + 2, "art_shadow_no_sigbus");
    check_ok!(unregister(fd, c, 2 * PG), "art_unregister");
    // Unregistered pieces merge back: the whole range mremaps again.
    check_ok!(unregister(fd, b, 2 * PG), "unregister_first_half");
    check_ok!(unregister(fd, b + 2 * PG, 2 * PG), "unregister_second_half");
    let r = mremap(b, 4 * PG, 4 * PG, MREMAP_MAYMOVE | MREMAP_DONTUNMAP, 0);
    check!(r > 0, "mremap_registered_range_whole", "returned {}", r);
    let d = anon(PG);
    check_eq!(register(fd, d, PG, MODE_MISSING).0, 0, "register_before_close");
    close(fd);
    // After close, registered ranges are plain memory.
    check_eq!(unsafe { rd(d as *const u8) }, 0, "closed_ctx_zero_fill");
    check_eq!(S_HITS.load(SeqCst), hits + 2, "closed_ctx_no_sigbus");

    // --- shmem: minor and missing faults ---------------------------------------------
    let mfd = unsafe { syscall!(nr::MEMFD_CREATE, c"uffd".as_ptr(), 0) } as i32;
    ftruncate(mfd, 2 * PG as i64);
    pwrite(mfd, b"minor", 0);
    let m1 = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0) as usize;
    let m2 = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0) as usize;
    let (fd, f) = uffd_api(FEATURE_SIGBUS | FEATURE_MINOR_SHMEM | FEATURE_MISSING_SHMEM);
    check!(f & FEATURE_MINOR_SHMEM != 0, "shmem_features");
    S_FD.store(fd, SeqCst);
    check_eq!(register(fd, m2, 2 * PG, MODE_MINOR | MODE_MISSING).0, 0, "shmem_register_minor_missing");
    check_eq!(register(fd, anon(PG), PG, MODE_MINOR).0, -(EINVAL as isize), "minor_on_anon_einval");
    let hits = S_HITS.load(SeqCst);
    S_HOW.store(1, SeqCst);
    check_eq!(unsafe { rd((m2 + 1) as *const u8) }, b'i', "minor_fault_continue");
    check_eq!(S_HITS.load(SeqCst), hits + 1, "minor_fault_sigbus");
    check_eq!(cont(fd, m2, PG).1, -(EEXIST as i64), "continue_present_eexist");
    // Page 1 is not in the file yet: a missing fault, filled by COPY.
    let src = page_of(0x3c);
    S_SRC.store(src.as_ptr() as usize, SeqCst);
    S_HOW.store(0, SeqCst);
    check_eq!(unsafe { rd((m2 + PG) as *const u8) }, 0x3c, "shmem_missing_copy");
    check_eq!(S_HITS.load(SeqCst), hits + 2, "shmem_missing_sigbus");
    check_eq!(unsafe { rd((m1 + PG + 5) as *const u8) }, 0x3c, "shmem_copy_in_file");
    check_eq!(cont(fd, m2, PG).0, -(EEXIST as isize), "continue_mapped_eexist");
    close(fd);
    close(mfd);

    // --- UFFDIO_MOVE ------------------------------------------------------------------------
    let (fd, f) = uffd_api(0);
    close(fd);
    if f & FEATURE_MOVE == 0 {
        skip("move", "UFFD_FEATURE_MOVE not offered");
    } else {
        let (fd, _) = uffd_api(FEATURE_MOVE);
        let dst = anon(3 * PG);
        let srcm = anon(3 * PG);
        unsafe {
            wr(srcm as *mut u8, 0x71);
            wr((srcm + 2 * PG) as *mut u8, 0x73);
        }
        let (r, ioctls) = register(fd, dst, 3 * PG, MODE_MISSING);
        check!(r == 0 && ioctls & (1 << 5) != 0, "move_register_ioctls", "r {} ioctls {:#x}", r, ioctls);
        check_eq!(mv(fd, dst, srcm, 3 * PG, 0), (-(EAGAIN as isize), PG as i64), "move_stops_at_hole");
        check_eq!(mv(fd, dst + PG, srcm + PG, 2 * PG, MOVE_ALLOW_SRC_HOLES), (0, 2 * PG as i64), "move_allow_src_holes");
        check!(unsafe { rd(dst as *const u8) } == 0x71 && unsafe { rd((dst + 2 * PG) as *const u8) } == 0x73, "move_data");
        check_eq!(mv(fd, dst, srcm, PG, MOVE_ALLOW_SRC_HOLES).1, -(EEXIST as i64), "move_dst_present_eexist");
        check_ok!(unregister(fd, dst, 3 * PG), "move_unregister");
        check_eq!(unsafe { rd(srcm as *const u8) }, 0, "move_src_empty");
        close(fd);
    }

    // --- /dev/userfaultfd ----------------------------------------------------------------------
    let dev = open(c"/dev/userfaultfd".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if dev < 0 {
        skip("dev_userfaultfd", "cannot open /dev/userfaultfd");
    } else {
        let nfd = ioctl(dev as i32, USERFAULTFD_IOC_NEW, O_CLOEXEC as usize | UFFD_USER_MODE_ONLY);
        check!(nfd >= 0, "dev_ioc_new", "returned {}", errname(nfd));
        let mut api = [UFFD_API, FEATURE_SIGBUS, 0];
        check_ok!(ioctl(nfd as i32, UFFDIO_API, api.as_mut_ptr() as usize), "dev_ctx_api");
        check_err!(ioctl(dev as i32, 0xaa01, 0), EINVAL, "dev_bad_ioctl_einval");
        close(nfd as i32);
        close(dev as i32);
    }
}
