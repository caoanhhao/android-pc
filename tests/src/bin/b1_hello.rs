//! B1: the first thing a new kernel must run. Exercises the ELF loader's
//! initial stack (argc/argv/envp/auxv), write(2), and identity syscalls.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b1_hello", main);

fn main() {
    check!(write(1, b"# hello from userspace\n") == 23, "write_stdout");
    check!(argc() >= 1, "argc_ge_1");
    check!(argv(0).is_some_and(|a| !a.is_empty()), "argv0_nonempty");
    check!(argv_raw(argc()).is_none(), "argv_bounds");
    // envp may legitimately be empty; just walk it.
    let n = envc();
    check!((0..n).all(|i| env(i).is_some()), "envp_walk");

    check_eq!(getauxval(AT_PAGESZ), Some(4096), "auxv_pagesz");
    check!(getauxval(AT_PHDR).is_some_and(|p| p != 0), "auxv_phdr");
    check_eq!(getauxval(AT_PHENT), Some(56), "auxv_phent");
    check!(getauxval(AT_PHNUM).is_some_and(|n| n > 0), "auxv_phnum");
    check!(getauxval(AT_ENTRY).is_some_and(|e| e == entry_addr()), "auxv_entry_is_start");
    match getauxval(AT_RANDOM) {
        Some(p) if p != 0 => {
            let r = unsafe { core::slice::from_raw_parts(p as *const u8, 16) };
            check!(r.iter().any(|&b| b != 0), "auxv_random_nonzero");
        }
        _ => {
            check!(false, "auxv_random_nonzero", "AT_RANDOM missing");
        }
    }
    // The initial stack pointer must be 16-byte aligned at _start
    // (verified indirectly: rt_start ran and we got here).

    let pid = getpid();
    check!(pid > 0, "getpid_positive");
    check_eq!(gettid(), pid, "gettid_eq_pid");
    check!(getppid() >= 0, "getppid");

    let mut u: Utsname = unsafe { core::mem::zeroed() };
    check_ok!(unsafe { syscall!(nr::UNAME, &mut u as *mut Utsname) }, "uname");
    check_eq!(cfield(&u.sysname), b"Linux", "uname_sysname");
    check_eq!(cfield(&u.machine), b"x86_64", "uname_machine");

    let uid = unsafe { syscall!(nr::GETUID) };
    check!(uid >= 0, "getuid");
    check!(unsafe { syscall!(nr::GETEUID) } == uid, "geteuid_eq_uid");

    // Unknown syscall numbers must fail with ENOSYS, not crash.
    check_err!(unsafe { syscall!(100000) }, ENOSYS, "unknown_syscall_enosys");

    // Writes to a closed / invalid fd.
    check_err!(write(987, b"x"), EBADF, "write_ebadf");

    // A zero-length write succeeds.
    check_eq!(write(1, b""), 0, "write_zero_len");
}

fn entry_addr() -> usize {
    unsafe extern "C" {
        fn _start();
    }
    _start as *const () as usize
}
