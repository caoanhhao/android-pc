//! C3: process credentials, as Android's init, logd and servicemanager see
//! them: getres[ug]id, set[res][ug]id with Linux's rules for privileged
//! and unprivileged callers, setfs[ug]id, setgroups/getgroups, fork
//! inheritance, /proc/self/status Uid/Gid/Groups and the owner of
//! /proc/<pid>; SO_PEERCRED and SCM_CREDENTIALS of a process that dropped
//! to another uid (logd's view of its clients); the owner of new files
//! (fs ids, set-group-ID directories); exec of a set-user-ID file (euid,
//! AT_SECURE). Needs root (on the host: SKIP when not root).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_creds", main);

const GETEGID: usize = 108;
const SETUID: usize = 105;
const SETGID: usize = 106;
const SETREUID: usize = 113;
const GETGROUPS: usize = 115;
const SETGROUPS: usize = 116;
const SETRESUID: usize = 117;
const GETRESUID: usize = 118;
const SETRESGID: usize = 119;
const GETRESGID: usize = 120;
const SETFSUID: usize = 122;
const CHMOD: usize = 90;
const FCHMOD: usize = 91;
const FCHOWN: usize = 93;
const AT_SECURE: usize = 23;

fn resuid() -> (u32, u32, u32) {
    let (mut r, mut e, mut s) = (0u32, 0u32, 0u32);
    unsafe { syscall!(GETRESUID, &mut r as *mut u32, &mut e as *mut u32, &mut s as *mut u32) };
    (r, e, s)
}

fn resgid() -> (u32, u32, u32) {
    let (mut r, mut e, mut s) = (0u32, 0u32, 0u32);
    unsafe { syscall!(GETRESGID, &mut r as *mut u32, &mut e as *mut u32, &mut s as *mut u32) };
    (r, e, s)
}

fn setresuid(r: i32, e: i32, s: i32) -> isize {
    unsafe { syscall!(SETRESUID, r, e, s) }
}

fn setresgid(r: i32, e: i32, s: i32) -> isize {
    unsafe { syscall!(SETRESGID, r, e, s) }
}

fn setgroups(g: &[u32]) -> isize {
    unsafe { syscall!(SETGROUPS, g.len(), g.as_ptr()) }
}

/// Run `f` in a child; `f` returns 0 on success or the number of the
/// first failed step, which becomes the reported detail.
fn child_case(name: &str, f: impl FnOnce() -> i32) {
    let code = status_code(run_child(f));
    check!(code == 0, name, "failed at step {}", code);
}

fn status_has(line: &[u8]) -> bool {
    let mut b = [0u8; 4096];
    let n = read_file(c"/proc/self/status".as_ptr() as *const u8, &mut b);
    n > 0 && b[..n as usize].split(|&c| c == b'\n').any(|l| l == line)
}

fn main() {
    if is_child_mode() {
        // Exec'd set-user-ID root by uid 1000: euid 0, real 1000, AT_SECURE.
        let (r, e, s) = resuid();
        let secure = getauxval(AT_SECURE).unwrap_or(0);
        exit(if r == 1000 && e == 0 && s == 0 && secure == 1 { 0 } else { 1 });
    }
    if resuid() != (0, 0, 0) {
        skip("all", "needs root");
        return;
    }
    check_eq!(unsafe { syscall!(nr::GETUID) }, 0, "getuid_root");
    check_eq!(resgid(), (0, 0, 0), "getresgid_root");

    // Privileged setres*id sets each id; -1 keeps one.
    child_case("setresuid_privileged", || {
        if setresgid(1007, 1007, 1007) != 0 || resgid() != (1007, 1007, 1007) {
            return 1;
        }
        if unsafe { syscall!(GETEGID) } != 1007 {
            return 2;
        }
        if setresuid(1000, 1001, -1) != 0 || resuid() != (1000, 1001, 0) {
            return 3;
        }
        // Unprivileged now (euid 1001): only ids it has.
        if setresuid(-1, 0, -1) != 0 || resuid() != (1000, 0, 0) {
            return 4;
        }
        if setresuid(1000, 1000, 1000) != 0 || resuid() != (1000, 1000, 1000) {
            return 5;
        }
        if setresuid(-1, 0, -1) != -(EPERM as isize) {
            return 6;
        }
        if setresuid(2000, -1, -1) != -(EPERM as isize) {
            return 7;
        }
        if setresgid(0, 0, 0) != -(EPERM as isize) {
            return 8;
        }
        0
    });

    // setuid as root sets all three ids; unprivileged only the effective one.
    child_case("setuid_rules", || {
        if unsafe { syscall!(SETGID, 1000) } != 0 || resgid() != (1000, 1000, 1000) {
            return 1;
        }
        if unsafe { syscall!(SETREUID, 1000, 1001) } != 0 || resuid() != (1000, 1001, 1001) {
            return 2;
        }
        if unsafe { syscall!(SETUID, 1000) } != 0 || resuid() != (1000, 1000, 1001) {
            return 3;
        }
        if unsafe { syscall!(SETUID, 1001) } != 0 || resuid() != (1000, 1001, 1001) {
            return 4;
        }
        if unsafe { syscall!(SETUID, 0) } != -(EPERM as isize) {
            return 5;
        }
        // setfsuid returns the previous fs uid (the effective one).
        if unsafe { syscall!(SETFSUID, -1i32) } != 1001 {
            return 6;
        }
        0
    });

    // Supplementary groups: set (root only), read back, inherited by fork.
    child_case("groups", || {
        if setgroups(&[3009, 1007, 2000]) != 0 {
            return 1;
        }
        let mut g = [0u32; 8];
        let n = unsafe { syscall!(GETGROUPS, 0, core::ptr::null_mut::<u32>()) };
        if n != 3 {
            return 2;
        }
        if unsafe { syscall!(GETGROUPS, 8, g.as_mut_ptr()) } != 3 {
            return 3;
        }
        let mut s = [g[0], g[1], g[2]];
        s.sort_unstable();
        if s != [1007, 2000, 3009] {
            return 4;
        }
        if unsafe { syscall!(GETGROUPS, 1, g.as_mut_ptr()) } != -(EINVAL as isize) {
            return 5;
        }
        let inherited = status_code(run_child(|| (unsafe { syscall!(GETGROUPS, 0, 0) } != 3) as i32));
        if inherited != 0 {
            return 6;
        }
        setresuid(2000, 2000, 2000);
        if setgroups(&[]) != -(EPERM as isize) {
            return 7;
        }
        0
    });

    // /proc: status lines and the owner of /proc/<pid> follow the ids.
    child_case("proc_status", || {
        setgroups(&[1007, 3009]);
        setresgid(2000, 2000, 2000);
        setresuid(2000, 2000, 2000);
        if !status_has(b"Uid:\t2000\t2000\t2000\t2000") || !status_has(b"Gid:\t2000\t2000\t2000\t2000") {
            return 1;
        }
        if !status_has(b"Groups:\t1007 3009 ") {
            return 2;
        }
        let mut st = Stat::default();
        let p = cpath!("/proc/{}", getpid());
        if fstatat(AT_FDCWD, p.as_ptr(), &mut st, 0) != 0 || st.uid != 2000 || st.gid != 2000 {
            return 3;
        }
        0
    });

    // SO_PEERCRED and SCM_CREDENTIALS: the peer's effective ids.
    let mut sv = [0i32; 2];
    socketpair(AF_UNIX, SOCK_DGRAM, &mut sv);
    let on = 1i32;
    setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &on as *const i32 as *const u8, 4);
    let st = run_child(|| {
        setresgid(1036, 1036, 1036);
        setresuid(1036, 1036, 1036);
        // Like a logd client: a plain datagram, the kernel adds the creds.
        if send(sv[1], b"log", 0) != 3 {
            return 1;
        }
        // Explicit SCM_CREDENTIALS with our own ids is accepted.
        let mut c = Cmsg([0; 64]);
        let h = c.0.as_mut_ptr() as *mut Cmsghdr;
        unsafe {
            (*h).len = 16 + 12;
            (*h).level = SOL_SOCKET;
            (*h).ty = SCM_CREDENTIALS;
            let cred = Ucred { pid: getpid(), uid: 1036, gid: 1036 };
            (c.0.as_mut_ptr().add(16) as *mut Ucred).write(cred);
        }
        let mut b = *b"two";
        let mut iov = Iovec { base: b.as_mut_ptr(), len: 3 };
        let m = Msghdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            iov: &mut iov,
            iovlen: 1,
            control: c.0.as_mut_ptr(),
            controllen: 32,
            flags: 0,
        };
        if sendmsg(sv[1], &m, 0) != 3 {
            return 2;
        }
        0
    });
    check_eq!(status_code(st), 0, "child_sends");
    let mut want_pid = 0;
    for case in ["scm_credentials_implicit", "scm_credentials_explicit"] {
        let mut b = [0u8; 16];
        let mut c = Cmsg([0; 64]);
        let mut iov = Iovec { base: b.as_mut_ptr(), len: 16 };
        let mut m = Msghdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            iov: &mut iov,
            iovlen: 1,
            control: c.0.as_mut_ptr(),
            controllen: 64,
            flags: 0,
        };
        let n = recvmsg(sv[0], &mut m, MSG_DONTWAIT);
        let cred = unsafe { (c.0.as_ptr().add(16) as *const Ucred).read() };
        let h = c.0.as_ptr() as *const Cmsghdr;
        let ok = n == 3 && m.controllen >= 28 && unsafe { (*h).ty == SCM_CREDENTIALS } && cred.uid == 1036 && cred.gid == 1036;
        if want_pid == 0 {
            want_pid = cred.pid;
        }
        check!(ok && cred.pid == want_pid && cred.pid > 0, case, "n={} uid={} gid={} pid={}", n, cred.uid, cred.gid, cred.pid);
    }
    close(sv[0]);
    close(sv[1]);
    let mut sp = [0i32; 2];
    let st = run_child(|| {
        setresgid(1000, 1000, 1000);
        setresuid(1000, 1000, 1000);
        socketpair(AF_UNIX, SOCK_STREAM, &mut sp);
        let mut cred = Ucred::default();
        let mut len = 12u32;
        getsockopt(sp[0], SOL_SOCKET, SO_PEERCRED, &mut cred as *mut Ucred as *mut u8, &mut len);
        (cred.uid != 1000 || cred.gid != 1000 || cred.pid != getpid()) as i32
    });
    check_eq!(status_code(st), 0, "so_peercred_dropped");

    // New files belong to the fs ids; set-group-ID directories pass on
    // their group (and the bit to subdirectories).
    let dir = scratch_dir();
    let d = cpath!("{}", Bytes(dir.as_bytes()));
    unsafe { syscall!(CHMOD, d.as_ptr(), 0o777) };
    child_case("new_file_owner", || {
        setresgid(1000, 1000, 1000);
        setresuid(1000, 1000, 1000);
        let f = cpath!("{}/f", Bytes(d.as_bytes()));
        let fd = open(f.as_ptr(), O_CREAT | O_WRONLY, 0o644);
        if fd < 0 {
            return 1;
        }
        close(fd as i32);
        let mut st = Stat::default();
        fstatat(AT_FDCWD, f.as_ptr(), &mut st, 0);
        if st.uid != 1000 || st.gid != 1000 {
            return 2;
        }
        let sub = cpath!("{}/sub", Bytes(d.as_bytes()));
        mkdirat(AT_FDCWD, sub.as_ptr(), 0o755);
        fstatat(AT_FDCWD, sub.as_ptr(), &mut st, 0);
        if st.uid != 1000 || st.gid != 1000 {
            return 3;
        }
        0
    });
    let sg = cpath!("{}/sgid", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, sg.as_ptr(), 0o777);
    let fd = open(sg.as_ptr(), O_RDONLY | O_DIRECTORY, 0) as i32;
    unsafe { syscall!(FCHOWN, fd, 0, 1007) };
    unsafe { syscall!(FCHMOD, fd, 0o2777) };
    close(fd);
    child_case("setgid_dir", || {
        setresgid(1000, 1000, 1000);
        setresuid(1000, 1000, 1000);
        let f = cpath!("{}/f", Bytes(sg.as_bytes()));
        close(open(f.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32);
        let mut st = Stat::default();
        fstatat(AT_FDCWD, f.as_ptr(), &mut st, 0);
        if st.uid != 1000 || st.gid != 1007 {
            return 1;
        }
        let sub = cpath!("{}/sub", Bytes(sg.as_bytes()));
        mkdirat(AT_FDCWD, sub.as_ptr(), 0o755);
        fstatat(AT_FDCWD, sub.as_ptr(), &mut st, 0);
        if st.gid != 1007 || st.mode & 0o2000 == 0 {
            return 2;
        }
        0
    });

    // A set-user-ID root copy of ourselves, run by uid 1000.
    let exe = cpath!("{}/suid", Bytes(dir.as_bytes()));
    let copied = copy_file(c"/proc/self/exe".as_ptr() as *const u8, exe.as_ptr());
    check!(copied, "copy_self");
    let fd = open(exe.as_ptr(), O_RDONLY, 0) as i32;
    unsafe { syscall!(FCHOWN, fd, 0, 0) };
    check_ok!(unsafe { syscall!(FCHMOD, fd, 0o4755) }, "chmod_suid");
    close(fd);
    let st = run_child(|| {
        setresgid(1000, 1000, 1000);
        setresuid(1000, 1000, 1000);
        let argv = [exe.as_ptr(), b"--rt-child\0".as_ptr(), b"suid\0".as_ptr(), core::ptr::null()];
        let envp = [core::ptr::null::<u8>()];
        execve(exe.as_ptr(), argv.as_ptr(), envp.as_ptr());
        99
    });
    check_eq!(status_code(st), 0, "exec_setuid_root");
}

#[repr(C, align(8))]
struct Cmsg([u8; 64]);

fn copy_file(from: *const u8, to: *const u8) -> bool {
    let src = open(from, O_RDONLY, 0);
    let dst = open(to, O_CREAT | O_WRONLY | O_TRUNC, 0o755);
    if src < 0 || dst < 0 {
        return false;
    }
    let mut buf = [0u8; 4096];
    loop {
        let n = read(src as i32, &mut buf);
        if n <= 0 {
            break;
        }
        if write(dst as i32, &buf[..n as usize]) != n {
            return false;
        }
    }
    close(src as i32);
    close(dst as i32);
    true
}
