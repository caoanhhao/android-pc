//! ktest-init: runs the test programs one after another. Used as /init in
//! the initramfs (tools/mkinitramfs.sh) and by run-host.sh.
//!
//!   ktest-init [DIR] [FILTER...]
//!
//! DIR (an argument starting with '/' or '.') holds the test binaries
//! (default /tests). FILTERs are name prefixes ("b1", "b2_pipe"); without
//! one every test in tests.list runs. Each test is one case of this
//! program: `PASS ktest-init b1_hello` when the test exited 0 within the
//! timeout. As PID 1 it first mounts proc/sys/devtmpfs/tmpfs and powers the
//! machine off at the end.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("ktest-init", main);

const LIST: &str = include_str!("../../tests.list");
const TIMEOUT_NS: i64 = 120 * 1_000_000_000;

fn mount(src: &core::ffi::CStr, target: &core::ffi::CStr, fstype: &core::ffi::CStr) {
    let r = unsafe { syscall!(165, src.as_ptr(), target.as_ptr(), fstype.as_ptr(), 0, 0) };
    if r < 0 {
        println!("# mount {:?} on {:?}: {}", fstype, target, errname(r));
    }
}

fn selected(name: &[u8]) -> bool {
    let mut any_filter = false;
    for i in 1..argc() {
        let a = argv(i).unwrap();
        if a.first().is_some_and(|&c| c == b'/' || c == b'.') {
            continue;
        }
        any_filter = true;
        if name.starts_with(a) {
            return true;
        }
    }
    !any_filter
}

fn main() {
    let pid1 = getpid() == 1;
    if pid1 {
        mount(c"proc", c"/proc", c"proc");
        mount(c"sysfs", c"/sys", c"sysfs");
        mount(c"devtmpfs", c"/dev", c"devtmpfs");
        mount(c"devpts", c"/dev/pts", c"devpts");
        mount(c"tmpfs", c"/tmp", c"tmpfs");
    }
    let mut dir: &[u8] = b"/tests";
    for i in 1..argc() {
        let a = argv(i).unwrap();
        if a.first().is_some_and(|&c| c == b'/' || c == b'.') {
            dir = a;
        }
    }

    for line in LIST.lines() {
        let name = line.trim();
        if name.is_empty() || name.starts_with('#') || !selected(name.as_bytes()) {
            continue;
        }
        let path = cpath!("{}/{}", Bytes(dir), name);
        let t0 = now_ns();
        let pid = fork();
        if pid == 0 {
            let av = [path.as_ptr(), core::ptr::null()];
            let ev = [b"PATH=/tests\0".as_ptr(), core::ptr::null()];
            let r = execve(path.as_ptr(), av.as_ptr(), ev.as_ptr());
            println!("# execve {}: {}", Bytes(path.as_bytes()), errname(r));
            exit(127);
        }
        if pid < 0 {
            check!(false, name, "fork failed: {}", errname(pid));
            continue;
        }
        let mut status = 0;
        let mut timed_out = false;
        loop {
            let r = wait4(pid as i32, &mut status, WNOHANG);
            if r == pid {
                break;
            }
            if r < 0 {
                status = -1;
                break;
            }
            if now_ns() - t0 > TIMEOUT_NS {
                timed_out = true;
                kill(pid as i32, SIGKILL);
            }
            sleep_ms(5);
        }
        let ms = (now_ns() - t0) / 1_000_000;
        if timed_out {
            check!(false, name, "timeout after {} ms", ms);
        } else {
            check!(status == 0, name, "exit status {} ({} ms)", status_code(status), ms);
        }
        if pid1 {
            // Reap any orphans the test left behind.
            while wait4(-1, &mut status, WNOHANG) > 0 {}
        }
    }

    if pid1 {
        // PID 1 must never exit: print END, then power off.
        print_end();
        // LINUX_REBOOT_MAGIC1/2, LINUX_REBOOT_CMD_POWER_OFF.
        unsafe { syscall!(169, 0xfee1deadusize, 672274793usize, 0x4321fedcusize, 0) };
        loop {
            sleep_ms(1_000_000);
        }
    }
}
