//! B1: thread-local storage — arch_prctl(ARCH_SET_FS/GS), %fs-relative
//! loads, preservation across syscalls/sleeps/fork, CLONE_SETTLS.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
use rt::*;

rt::test_main!("b1_tls", main);

const AT_HWCAP2: usize = 26;
const HWCAP2_FSGSBASE: usize = 1 << 1;

/// A TLS block whose first word points to itself, as in the x86_64 TLS ABI
/// (bionic and musl both rely on %fs:0 being the thread pointer).
#[repr(C, align(64))]
struct Tcb {
    self_ptr: usize,
    value: u64,
    _pad: [u64; 6],
}

fn fs_load(off: usize) -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, fs:[{}]", out(reg) v, in(reg) off, options(nostack, readonly)) };
    v
}

fn gs_load(off: usize) -> u64 {
    let v: u64;
    unsafe { asm!("mov {}, gs:[{}]", out(reg) v, in(reg) off, options(nostack, readonly)) };
    v
}

fn arch_get(code: i32) -> usize {
    let mut v = 0usize;
    unsafe { syscall!(nr::ARCH_PRCTL, code, &mut v as *mut usize) };
    v
}

static THREAD_SAW: AtomicU64 = AtomicU64::new(0);

extern "C" fn tls_thread(_: usize) -> i32 {
    THREAD_SAW.store(fs_load(8), SeqCst);
    0
}

fn main() {
    let tcb = alloc_pages(4096) as *mut Tcb;
    unsafe {
        (*tcb).self_ptr = tcb as usize;
        (*tcb).value = 0xfeed_f00d_1234_5678;
    }
    check_ok!(unsafe { syscall!(nr::ARCH_PRCTL, ARCH_SET_FS, tcb) }, "arch_set_fs");
    check_eq!(arch_get(ARCH_GET_FS), tcb as usize, "arch_get_fs");
    check_eq!(fs_load(0), tcb as u64, "fs0_self_pointer");
    check_eq!(fs_load(8), 0xfeed_f00d_1234_5678, "fs_relative_load");

    // Survives syscalls, a sleep (context switch) and a yield.
    unsafe { syscall!(nr::SCHED_YIELD) };
    sleep_ms(10);
    check_eq!(fs_load(8), 0xfeed_f00d_1234_5678, "fs_preserved_across_switch");

    // Inherited by fork.
    let st = run_child(|| (fs_load(8) == 0xfeed_f00d_1234_5678) as i32);
    check_eq!(status_code(st), 1, "fs_inherited_by_fork");

    // GS works too.
    let gsblk = alloc_pages(4096) as *mut u64;
    unsafe { *gsblk.add(1) = 0xabcd };
    check_ok!(unsafe { syscall!(nr::ARCH_PRCTL, ARCH_SET_GS, gsblk) }, "arch_set_gs");
    check_eq!(arch_get(ARCH_GET_GS), gsblk as usize, "arch_get_gs");
    check_eq!(gs_load(8), 0xabcd, "gs_relative_load");

    // A thread created with CLONE_SETTLS gets its own %fs.
    let tcb2 = alloc_pages(4096) as *mut Tcb;
    unsafe {
        (*tcb2).self_ptr = tcb2 as usize;
        (*tcb2).value = 0x2222;
    }
    let t = spawn(tls_thread, 0, tcb2 as usize);
    t.join();
    check_eq!(THREAD_SAW.load(SeqCst), 0x2222, "clone_settls_thread_fs");
    check_eq!(fs_load(8), 0xfeed_f00d_1234_5678, "main_fs_unchanged_by_thread");

    // Non-canonical bases are rejected.
    check_err!(unsafe { syscall!(nr::ARCH_PRCTL, ARCH_SET_FS, 0x8000_0000_0000_0000usize) }, EPERM,
               "arch_set_fs_noncanonical");
    check_err!(unsafe { syscall!(nr::ARCH_PRCTL, 0x9999, 0) }, EINVAL, "arch_prctl_bad_code");

    // If the kernel advertises FSGSBASE, rdfsbase must agree.
    match getauxval(AT_HWCAP2) {
        Some(h) if h & HWCAP2_FSGSBASE != 0 => {
            let v: usize;
            unsafe { asm!("rdfsbase {}", out(reg) v, options(nostack)) };
            check_eq!(v, tcb as usize, "rdfsbase_matches");
        }
        _ => skip("rdfsbase_matches", "AT_HWCAP2 lacks FSGSBASE"),
    }
}
