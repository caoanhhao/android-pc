//! B1: process creation and reaping — clone(SIGCHLD)/fork/vfork, wait4,
//! exit status encoding, getppid, kill.
#![no_std]
#![no_main]

use core::arch::asm;
use rt::*;

rt::test_main!("b1_fork_wait", main);

fn main() {
    let me = getpid();

    let st = run_child(|| 42);
    check!(wifexited(st), "child_exited", "status {:#x}", st);
    check_eq!(wexitstatus(st), 42, "child_exit_code");

    // Exit codes are truncated to 8 bits.
    check_eq!(status_code(run_child(|| 256 + 3)), 3, "exit_code_truncated");

    check_eq!(status_code(run_child(move || (getppid() == me) as i32)), 1, "child_getppid");
    check_eq!(status_code(run_child(move || (getpid() != me) as i32)), 1, "child_new_pid");

    // The legacy fork syscall.
    let pid = unsafe { syscall!(nr::FORK) };
    if pid == 0 {
        exit(9);
    }
    check!(pid > 0, "fork_syscall", "fork returned {}", errname(pid));
    let mut s = 0;
    check_eq!(wait4(pid as i32, &mut s, 0), pid, "fork_syscall_wait");
    check_eq!(status_code(s), 9, "fork_syscall_status");

    // vfork: the child shares our memory and we are suspended until it
    // exits. The child stores 1 into `flag` and calls exit_group(7) without
    // touching the stack.
    let mut flag: u32 = 0;
    let r: isize;
    unsafe {
        asm!("syscall", "test rax, rax", "jnz 2f",
             "mov dword ptr [rsi], 1", "mov edi, 7", "mov eax, 231", "syscall", "ud2",
             "2:",
             in("rsi") &mut flag as *mut u32,
             inlateout("rax") nr::VFORK as isize => r,
             lateout("rdi") _, lateout("rcx") _, lateout("r11") _);
    }
    check!(r > 0, "vfork", "vfork returned {}", errname(r));
    check_eq!(unsafe { core::ptr::read_volatile(&flag) }, 1, "vfork_shares_memory_and_suspends_parent");
    let mut s = 0;
    check_eq!(wait4(r as i32, &mut s, 0), r, "vfork_wait");
    check_eq!(status_code(s), 7, "vfork_status");

    // WNOHANG on a live child returns 0.
    let pid = fork();
    if pid == 0 {
        sleep_ms(100);
        exit(0);
    }
    let mut s = 0;
    check_eq!(wait4(pid as i32, &mut s, WNOHANG), 0, "wnohang_running");
    check_eq!(wait4(pid as i32, &mut s, 0), pid, "wait_blocking");

    // wait4(-1) reaps any child; collect several.
    let mut sum = 0;
    for i in 1..=5 {
        if fork() == 0 {
            exit(i);
        }
    }
    for _ in 0..5 {
        let mut s = 0;
        if wait4(-1, &mut s, 0) > 0 {
            sum += status_code(s);
        }
    }
    check_eq!(sum, 15, "wait_any_five_children");

    let mut s = 0;
    check_err!(wait4(-1, &mut s, 0), ECHILD, "wait_no_children_echild");
    check_err!(wait4(1_000_000, &mut s, 0), ECHILD, "wait_bad_pid_echild");

    // SIGKILL a blocked child.
    let pid = fork();
    if pid == 0 {
        loop {
            sleep_ms(1000);
        }
    }
    check_ok!(kill(pid as i32, SIGKILL), "kill_child");
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);
    check!(wifsignaled(s) && wtermsig(s) == SIGKILL, "child_killed_sigkill", "status {:#x}", s);

    check_eq!(kill(me, 0), 0, "kill_sig0_self");
    check_err!(kill(1_000_000, 0), ESRCH, "kill_esrch");

    // CLONE_PARENT_SETTID stores the child's pid in the parent.
    let mut ptid: i32 = 0;
    let r = unsafe { syscall!(nr::CLONE, CLONE_PARENT_SETTID | SIGCHLD as usize, 0, &mut ptid as *mut i32, 0, 0) };
    if r == 0 {
        exit(0);
    }
    check_eq!(ptid as isize, r, "clone_parent_settid");
    let mut s = 0;
    wait4(r as i32, &mut s, 0);

    // Memory is copied, not shared, after fork.
    let mut v = 1u32;
    let p = &mut v as *mut u32;
    let st = run_child(move || {
        unsafe { p.write_volatile(2) };
        0
    });
    check!(st == 0 && unsafe { p.read_volatile() } == 1, "fork_private_memory");

    // A grandchild is reaped by its own parent, not by us.
    let st = run_child(|| {
        let g = fork();
        if g == 0 {
            exit(3);
        }
        let mut s = 0;
        if wait4(g as i32, &mut s, 0) == g { status_code(s) } else { 99 }
    });
    check_eq!(status_code(st), 3, "grandchild_reaped_by_child");
}
