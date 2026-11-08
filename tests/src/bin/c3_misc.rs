//! C3: the other kernel interfaces Android's init needs. waitid
//! (WNOWAIT peeking, WNOHANG, siginfo of exited and killed children),
//! sendfile and copy_file_range, pseudo-terminals (/dev/ptmx, unlockpt,
//! TIOCGPTN, /dev/pts/N, raw mode, hang-up in both directions),
//! /proc/self/fd magic links (reopening a memfd), /proc/self/ns/mnt,
//! /proc/self/mountinfo, oom_score_adj, prctl(PR_SET_CHILD_SUBREAPER,
//! PR_SET_PDEATHSIG) and ioprio. On the Rust kernel also: /proc/sys
//! values that read back what was written (init's mmap_rnd_bits check),
//! the hostname sysctl changing uname, /proc/bootconfig, MS_MOVE and
//! unshare/setns.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_misc", main);

const WAITID: usize = 247;
const SENDFILE: usize = 40;
const COPY_FILE_RANGE: usize = 326;
const MEMFD_CREATE: usize = 319;
const PRCTL: usize = 157;
const IOPRIO_SET: usize = 251;
const IOPRIO_GET: usize = 252;
const UNSHARE: usize = 272;
const SETNS: usize = 308;
const UNAME: usize = 63;
const P_PID: usize = 1;
const P_ALL: usize = 0;
const WEXITED: usize = 4;
const WNOHANG: usize = 1;
const WNOWAIT: usize = 0x0100_0000;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;
const TIOCGPTN: u32 = 0x8004_5430;
const TIOCSPTLCK: u32 = 0x4004_5431;
const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const MS_MOVE: usize = 8192;
const CLONE_NEWNS: usize = 0x20000;

/// waitid(); returns (ret, si_signo, si_code, si_pid, si_status).
fn waitid(idtype: usize, id: usize, options: usize) -> (isize, i32, i32, i32, i32) {
    let mut si = [0u8; 128];
    let r = unsafe { syscall!(WAITID, idtype, id, si.as_mut_ptr(), options, 0) };
    let w = |o: usize| i32::from_le_bytes(si[o..o + 4].try_into().unwrap());
    (r, w(0), w(8), w(16), w(24))
}

fn test_waitid() {
    let pid = fork();
    if pid == 0 {
        exit(7);
    }
    sleep_ms(50);
    let (r, signo, code, spid, status) = waitid(P_PID, pid as usize, WEXITED | WNOWAIT);
    check!(r == 0 && signo == 17 && code == CLD_EXITED && spid == pid as i32 && status == 7, "waitid_nowait_peek");
    let (r, _, code, spid, status) = waitid(P_PID, pid as usize, WEXITED);
    check!(r == 0 && code == CLD_EXITED && spid == pid as i32 && status == 7, "waitid_reap_after_peek");
    let mut st = 0;
    check_err!(wait4(pid as i32, &mut st, 0), ECHILD, "waitid_reaped");

    let pid = fork();
    if pid == 0 {
        loop {
            sleep_ms(1000);
        }
    }
    let (r, _, _, spid, _) = waitid(P_ALL, 0, WEXITED | WNOHANG);
    check!(r == 0 && spid == 0, "waitid_wnohang_none");
    kill(pid as i32, 9);
    let (r, _, code, spid, status) = waitid(P_ALL, 0, WEXITED);
    check!(r == 0 && code == CLD_KILLED && spid == pid as i32 && status == 9, "waitid_killed");
    check_err!(waitid(P_ALL, 0, WEXITED).0, ECHILD, "waitid_echild");
    check_err!(waitid(P_ALL, 0, 0).0, EINVAL, "waitid_no_options");
}

fn test_copies(dir: &Path) {
    let a = cpath!("{}/src", Bytes(dir.as_bytes()));
    let fd = open(a.as_ptr(), O_CREAT | O_RDWR, 0o644) as i32;
    let mut data = [0u8; 100_000];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    write(fd, &data);
    let b = cpath!("{}/dst", Bytes(dir.as_bytes()));
    let out = open(b.as_ptr(), O_CREAT | O_RDWR | O_TRUNC, 0o644) as i32;
    let mut off: i64 = 1000;
    let n = unsafe { syscall!(SENDFILE, out, fd, &mut off as *mut i64, 50_000) };
    check!(n == 50_000 && off == 51_000, "sendfile_offset", "{} {}", errname(n), off);
    let mut back = [0u8; 50_000];
    pread(out, &mut back, 0);
    check!(back[..] == data[1000..51_000], "sendfile_data");
    // Without an offset: the input file position moves.
    lseek(fd, 99_000, SEEK_SET);
    let n = unsafe { syscall!(SENDFILE, out, fd, 0, 5000) };
    check!(n == 1000 && lseek(fd, 0, SEEK_CUR) == 100_000, "sendfile_file_pos");
    let (mut oi, mut oo): (i64, i64) = (10, 0);
    let n = unsafe { syscall!(COPY_FILE_RANGE, fd, &mut oi as *mut i64, out, &mut oo as *mut i64, 20_000, 0) };
    check!(n == 20_000 && oi == 20_010 && oo == 20_000, "copy_file_range");
    pread(out, &mut back[..20_000], 0);
    check!(back[..20_000] == data[10..20_010], "copy_file_range_data");
    check_err!(unsafe { syscall!(COPY_FILE_RANGE, fd, 0, out, 0, 10, 1) }, EINVAL, "copy_file_range_flags");
    close(fd);
    close(out);
    unlinkat(AT_FDCWD, a.as_ptr(), 0);
    unlinkat(AT_FDCWD, b.as_ptr(), 0);
}

fn test_pty() {
    let m = open(c"/dev/ptmx".as_ptr() as *const u8, O_RDWR | O_NOCTTY_, 0) as i32;
    if m < 0 {
        skip("pty", "no /dev/ptmx");
        return;
    }
    let mut n = 0u32;
    check_eq!(ioctl(m, TIOCGPTN, &mut n as *mut u32 as usize), 0, "ptmx_ptn");
    let path = cpath!("/dev/pts/{}", n);
    let mut st = Stat::default();
    check!(fstatat(AT_FDCWD, path.as_ptr(), &mut st, 0) == 0 && st.mode & S_IFMT == S_IFCHR, "pts_node");
    let unlock = 0i32;
    check_eq!(ioctl(m, TIOCSPTLCK, &unlock as *const i32 as usize), 0, "unlockpt");
    let s = open(path.as_ptr(), O_RDWR | O_NOCTTY_, 0) as i32;
    if !check!(s >= 0, "open_slave", "{}", errname(s as isize)) {
        close(m);
        return;
    }
    // Slave output: OPOST/ONLCR by default.
    write(s, b"out\n");
    let mut b = [0u8; 64];
    let r = read(m, &mut b);
    check!(r == 5 && &b[..5] == b"out\r\n", "slave_to_master_onlcr", "{}", Bytes(&b[..r.max(0) as usize]));
    // Raw mode on the slave, then master -> slave unchanged.
    let mut t = [0u8; 60];
    check_eq!(ioctl(s, TCGETS, t.as_mut_ptr() as usize), 0, "tcgets");
    let lflag = u32::from_le_bytes(t[12..16].try_into().unwrap()) & !(0o2 | 0o10); // ~ICANON, ~ECHO
    t[12..16].copy_from_slice(&lflag.to_le_bytes());
    check_eq!(ioctl(s, TCSETS, t.as_ptr() as usize), 0, "tcsets_raw");
    write(m, b"in");
    let r = read(s, &mut b);
    check!(r == 2 && &b[..2] == b"in", "master_to_slave_raw");
    // Slave closed: master reads EIO.
    close(s);
    check_err!(read(m, &mut b), EIO, "master_eio_after_slave_close");
    // Master closed: a slave opened by TIOCGPTPEER-less path sees EOF.
    let m2 = open(c"/dev/ptmx".as_ptr() as *const u8, O_RDWR, 0) as i32;
    let mut n2 = 0u32;
    ioctl(m2, TIOCGPTN, &mut n2 as *mut u32 as usize);
    ioctl(m2, TIOCSPTLCK, &unlock as *const i32 as usize);
    let s2 = open(cpath!("/dev/pts/{}", n2).as_ptr(), O_RDWR, 0) as i32;
    close(m2);
    let r = read(s2, &mut b);
    check!(r == 0 || r == -(EIO as isize), "slave_eof_after_master_close", "{}", errname(r));
    check!(write(s2, b"x") < 0, "slave_write_after_hangup");
    close(s2);
    close(m);
}

const O_NOCTTY_: i32 = 0o400;

fn test_proc() {
    // /proc/self/fd/N of a memfd reopens the same file.
    let fd = unsafe { syscall!(MEMFD_CREATE, c"c3misc".as_ptr(), 0) } as i32;
    write(fd, b"memfd-data");
    let p = cpath!("/proc/self/fd/{}", fd);
    let mut b = [0u8; 64];
    let n = read_file(p.as_ptr(), &mut b);
    check!(n == 10 && &b[..10] == b"memfd-data", "proc_fd_reopen_memfd", "{}", errname(n));
    close(fd);

    let n = readlink(c"/proc/self/ns/mnt".as_ptr() as *const u8, &mut b);
    check!(n > 5 && b.starts_with(b"mnt:["), "ns_mnt_readlink");
    let ns = open(c"/proc/self/ns/mnt".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0) as i32;
    check!(ns >= 0, "ns_mnt_open", "{}", errname(ns as isize));

    let mut mi = [0u8; 8192];
    let n = read_file(c"/proc/self/mountinfo".as_ptr() as *const u8, &mut mi).max(0) as usize;
    check!(mi[..n].split(|&c| c == b'\n').any(|l| l.windows(5).any(|w| w == b" / / ") && l.windows(3).any(|w| w == b" - ")),
        "mountinfo_root");

    check_eq!(write_file(c"/proc/self/oom_score_adj".as_ptr() as *const u8, b"300"), 3, "oom_score_adj_write");
    let n = read_file(c"/proc/self/oom_score_adj".as_ptr() as *const u8, &mut b);
    check!(n > 0 && trim(&b[..n as usize]) == b"300", "oom_score_adj_read");

    // prctl and ioprio.
    let mut v = 0i32;
    check_eq!(unsafe { syscall!(PRCTL, 36, 1) }, 0, "set_child_subreaper");
    unsafe { syscall!(PRCTL, 37, &mut v as *mut i32) };
    check_eq!(v, 1, "get_child_subreaper");
    unsafe { syscall!(PRCTL, 36, 0) };
    check_eq!(unsafe { syscall!(PRCTL, 1, 15) }, 0, "set_pdeathsig");
    unsafe { syscall!(PRCTL, 2, &mut v as *mut i32) };
    check_eq!(v, 15, "get_pdeathsig");
    unsafe { syscall!(PRCTL, 1, 0) };
    let prio = (2usize << 13) | 3; // IOPRIO_CLASS_BE, level 3
    check_eq!(unsafe { syscall!(IOPRIO_SET, 1, 0, prio) }, 0, "ioprio_set");
    check_eq!(unsafe { syscall!(IOPRIO_GET, 1, 0) } as usize, prio, "ioprio_get");

    if !is_rust_kernel() {
        skip("kernel_only", "sysctl writes, MS_MOVE and namespaces need root on Linux");
        close(ns);
        return;
    }
    // A /proc/sys value read back through the same fd after a rewrite.
    let fd = open(c"/proc/sys/vm/mmap_rnd_bits".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    check_eq!(write_file(c"/proc/sys/vm/mmap_rnd_bits".as_ptr() as *const u8, b"32\n"), 3, "sysctl_write");
    let n = pread(fd, &mut b, 0);
    check!(n > 0 && trim(&b[..n as usize]) == b"32", "sysctl_reread");
    close(fd);
    check_err!(open(c"/proc/sys/kernel/no_such_knob".as_ptr() as *const u8, O_WRONLY | O_CREAT, 0o644), EACCES, "sysctl_no_create");
    check_eq!(write_file(c"/proc/sys/kernel/hostname".as_ptr() as *const u8, b"c3host"), 6, "hostname_write");
    let mut u = [0u8; 65 * 6];
    unsafe { syscall!(UNAME, u.as_mut_ptr()) };
    check!(cfield(&u[65..130]) == b"c3host", "uname_nodename");
    write_file(c"/proc/sys/kernel/hostname".as_ptr() as *const u8, b"android-pc");
    let n = read_file(c"/proc/bootconfig".as_ptr() as *const u8, &mut mi);
    check!(n >= 0, "proc_bootconfig", "{}", errname(n));

    // MS_MOVE: a tmpfs moved elsewhere keeps its contents.
    let (a, bb) = (c"/tmp/c3-mv-a".as_ptr() as *const u8, c"/tmp/c3-mv-b".as_ptr() as *const u8);
    mkdirat(AT_FDCWD, a, 0o755);
    mkdirat(AT_FDCWD, bb, 0o755);
    mount(c"tmpfs".as_ptr() as *const u8, a, c"tmpfs".as_ptr() as *const u8, 0, core::ptr::null());
    close(open(c"/tmp/c3-mv-a/marker".as_ptr() as *const u8, O_CREAT | O_WRONLY, 0o644) as i32);
    check_eq!(mount(a, bb, core::ptr::null(), MS_MOVE, core::ptr::null()), 0, "ms_move");
    let mut st = Stat::default();
    check!(fstatat(AT_FDCWD, c"/tmp/c3-mv-b/marker".as_ptr() as *const u8, &mut st, 0) == 0, "ms_move_target_has_files");
    check!(fstatat(AT_FDCWD, c"/tmp/c3-mv-a/marker".as_ptr() as *const u8, &mut st, 0) < 0, "ms_move_source_empty");
    let n = read_file(c"/proc/self/mounts".as_ptr() as *const u8, &mut mi).max(0) as usize;
    check!(mi[..n].windows(13).any(|w| w == b" /tmp/c3-mv-b"), "ms_move_in_mounts");
    check_err!(mount(bb, c"/tmp/c3-mv-b/x".as_ptr() as *const u8, core::ptr::null(), MS_MOVE, core::ptr::null()), ENOENT, "ms_move_into_self");
    umount2(bb, 0);

    check_eq!(unsafe { syscall!(UNSHARE, CLONE_NEWNS) }, 0, "unshare_newns");
    check_eq!(unsafe { syscall!(SETNS, ns, CLONE_NEWNS) }, 0, "setns_mnt");
    close(ns);
}

fn main() {
    let dir = scratch_dir();
    test_waitid();
    test_copies(&dir);
    test_pty();
    test_proc();
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}
