//! B2: device nodes and pseudo filesystems that bionic and Android init touch
//! early — /dev/null, /dev/zero, /dev/urandom, /proc/self/*, /proc/meminfo,
//! /proc/cmdline, /sys.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b2_devproc", main);

fn read_file(path: &core::ffi::CStr, buf: &mut [u8]) -> isize {
    let fd = open(path.as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0);
    if fd < 0 {
        return fd;
    }
    let mut total = 0usize;
    loop {
        let r = read(fd as i32, &mut buf[total..]);
        if r <= 0 {
            break;
        }
        total += r as usize;
        if total == buf.len() {
            break;
        }
    }
    close(fd as i32);
    total as isize
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn main() {
    let mut buf = [0u8; 8192];
    let mut st = Stat::default();

    // /dev/null
    let fd = check_ok!(open(c"/dev/null".as_ptr() as *const u8, O_RDWR, 0), "open_dev_null") as i32;
    check_eq!(write(fd, b"discard me"), 10, "dev_null_write");
    check_eq!(read(fd, &mut buf), 0, "dev_null_read_eof");
    fstat(fd, &mut st);
    check_eq!(st.mode & S_IFMT, S_IFCHR, "dev_null_is_chr");
    check_eq!(st.rdev, (1 << 8) | 3, "dev_null_rdev_1_3");
    close(fd);

    // /dev/zero
    let fd = check_ok!(open(c"/dev/zero".as_ptr() as *const u8, O_RDONLY, 0), "open_dev_zero") as i32;
    buf[..100].fill(0xff);
    check_eq!(read(fd, &mut buf[..100]), 100, "dev_zero_read");
    check!(buf[..100].iter().all(|&b| b == 0), "dev_zero_zeros");
    close(fd);

    // /dev/urandom
    let fd = check_ok!(open(c"/dev/urandom".as_ptr() as *const u8, O_RDONLY, 0), "open_dev_urandom") as i32;
    check_eq!(read(fd, &mut buf[..64]), 64, "dev_urandom_read");
    check!(buf[..64].iter().any(|&b| b != 0), "dev_urandom_nonzero");
    close(fd);

    // /proc/self/exe names this binary (compare by inode).
    let n = unsafe { syscall!(nr::READLINKAT, AT_FDCWD, c"/proc/self/exe".as_ptr(), buf.as_mut_ptr(), buf.len() - 1) };
    check!(n > 0 && buf[0] == b'/', "proc_self_exe_readlink", "returned {}", errname(n));
    if n > 0 {
        buf[n as usize] = 0;
        let mut a = Stat::default();
        let mut b = Stat::default();
        fstatat(AT_FDCWD, buf.as_ptr(), &mut a, 0);
        fstatat(AT_FDCWD, argv_raw(0).unwrap(), &mut b, 0);
        check!(a.ino == b.ino && a.dev == b.dev, "proc_self_exe_is_argv0");
    }

    // /proc/self/fd/N links back to the opened file.
    let fd = open(c"/dev/null".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    let link = cpath!("/proc/self/fd/{}", fd);
    let n = unsafe { syscall!(nr::READLINKAT, AT_FDCWD, link.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    check!(n > 0 && &buf[..n as usize] == b"/dev/null", "proc_self_fd_link");
    close(fd);

    // /proc/self/stat starts with "<pid> (<comm>) <state>".
    let n = read_file(c"/proc/self/stat", &mut buf);
    let pid = cpath!("{} (", getpid());
    check!(n > 0 && buf.starts_with(pid.as_bytes()), "proc_self_stat_pid");

    // /proc/self/cmdline is argv joined by NULs.
    let n = read_file(c"/proc/self/cmdline", &mut buf);
    let a0 = argv(0).unwrap();
    check!(n as usize > a0.len() && &buf[..a0.len()] == a0 && buf[a0.len()] == 0, "proc_self_cmdline");

    // /proc/self/maps lists the stack and our executable.
    let n = read_file(c"/proc/self/maps", &mut buf);
    let maps = &buf[..n.max(0) as usize];
    check!(contains(maps, b"[stack]"), "proc_self_maps_stack");
    check!(contains(maps, b"r-xp"), "proc_self_maps_text");

    // /proc/self/status has Pid: and Tgid:.
    let n = read_file(c"/proc/self/status", &mut buf);
    let s = &buf[..n.max(0) as usize];
    check!(contains(s, b"\nPid:\t") && contains(s, b"\nTgid:\t"), "proc_self_status");

    // System-wide files.
    let n = read_file(c"/proc/meminfo", &mut buf);
    check!(n > 0 && buf.starts_with(b"MemTotal:"), "proc_meminfo");
    check!(read_file(c"/proc/cmdline", &mut buf) > 0, "proc_cmdline");
    let n = read_file(c"/proc/filesystems", &mut buf);
    check!(n > 0 && contains(&buf[..n as usize], b"tmpfs"), "proc_filesystems_tmpfs");
    let n = read_file(c"/proc/mounts", &mut buf);
    check!(n > 0 && contains(&buf[..n as usize], b" /proc proc "), "proc_mounts_has_proc");

    // /proc and /sys are directories with the right magic-free basics.
    check!(fstatat(AT_FDCWD, c"/proc/self".as_ptr() as *const u8, &mut st, 0) == 0 && st.mode & S_IFMT == S_IFDIR,
           "proc_self_dir");
    check!(fstatat(AT_FDCWD, c"/sys/kernel".as_ptr() as *const u8, &mut st, 0) == 0 && st.mode & S_IFMT == S_IFDIR,
           "sys_kernel_dir");
    let pidd = cpath!("/proc/{}", getpid());
    check_ok!(fstatat(AT_FDCWD, pidd.as_ptr(), &mut st, 0), "proc_pid_dir");
}
