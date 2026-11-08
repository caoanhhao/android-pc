//! C3: mount namespaces and shared subtrees, as Android's init sets them
//! up (bootstrap and default namespaces, /bootstrap-apex bound on /apex):
//! MS_SHARED / MS_PRIVATE, a bind mount of a shared mount is its peer
//! (mounts and unmounts below one show up below the other), private
//! mounts receive nothing, mountinfo "shared:N"; unshare(CLONE_NEWNS)
//! and clone(CLONE_NEWNS) give a new /proc/self/ns/mnt id and a copy of
//! the mount table (private mounts made afterwards stay inside, shared
//! ones propagate across), setns() back through an open ns/mnt file.
//! Runs in a child that first unshares a user namespace (so the host
//! oracle needs no root).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_mntns", main);

const UNSHARE: usize = 272;
const SETNS: usize = 308;
const CLONE: usize = 56;
const CLONE_NEWNS: usize = 0x2_0000;
const CLONE_NEWUSER: usize = 0x1000_0000;
const MS_BIND: usize = 4096;
const MS_PRIVATE: usize = 1 << 18;
const MS_SHARED: usize = 1 << 20;

fn exists(p: &Path) -> bool {
    let mut st = Stat::default();
    fstatat(AT_FDCWD, p.as_ptr(), &mut st, 0) == 0
}

fn touch(p: &Path) -> bool {
    let fd = open(p.as_ptr(), O_CREAT | O_WRONLY | O_CLOEXEC, 0o644);
    fd >= 0 && close(fd as i32) == 0
}

fn tmpfs(at: &Path) -> isize {
    mount(c"tmpfs".as_ptr() as *const u8, at.as_ptr(), c"tmpfs".as_ptr() as *const u8, 0, core::ptr::null())
}

fn propagation(at: &Path, flags: usize) -> isize {
    mount(core::ptr::null(), at.as_ptr(), core::ptr::null(), flags, core::ptr::null())
}

fn ns_id(path: &core::ffi::CStr) -> ([u8; 64], usize) {
    let mut b = [0u8; 64];
    let n = readlink(path.as_ptr() as *const u8, &mut b);
    (b, n.max(0) as usize)
}

fn mkdir(p: &Path) {
    mkdirat(AT_FDCWD, p.as_ptr(), 0o755);
}

/// Runs inside the new user + mount namespace. Returns 0 or the number of
/// the failed step (the parent maps it to a case).
fn in_namespace(base: &Path) -> i32 {
    let p = |s: &str| cpath!("{}/{}", Bytes(base.as_bytes()), s);
    // A tmpfs of our own as the playground, shared.
    if tmpfs(base) != 0 {
        return 1;
    }
    for d in ["a", "b", "c", "p"] {
        mkdir(&p(d));
    }
    if tmpfs(&p("a")) != 0 || propagation(&p("a"), MS_SHARED) != 0 {
        return 2;
    }
    mkdir(&p("a/x"));
    mkdir(&p("a/y"));
    mkdir(&p("a/z"));
    // b: a bind of a (peer); c: a bind made private.
    if mount(p("a").as_ptr(), p("b").as_ptr(), core::ptr::null(), MS_BIND, core::ptr::null()) != 0 {
        return 3;
    }
    if mount(p("a").as_ptr(), p("c").as_ptr(), core::ptr::null(), MS_BIND, core::ptr::null()) != 0
        || propagation(&p("c"), MS_PRIVATE) != 0
    {
        return 4;
    }
    // Mounted below a: shows up in its peer b, not in private c.
    if tmpfs(&p("a/x")) != 0 || !touch(&p("a/x/f")) {
        return 5;
    }
    if !exists(&p("b/x/f")) {
        return 10;
    }
    if exists(&p("c/x/f")) {
        return 11;
    }
    // ... and the other way round: below b shows up in a.
    if tmpfs(&p("b/y")) != 0 || !touch(&p("b/y/g")) || !exists(&p("a/y/g")) {
        return 12;
    }
    // mountinfo marks the shared mount.
    let mut mi = [0u8; 16384];
    let n = read_file(c"/proc/self/mountinfo".as_ptr() as *const u8, &mut mi);
    let a_line = mi[..n.max(0) as usize].split(|&c| c == b'\n').find(|l| {
        let want = cpath!(" {} ", Bytes(p("a").as_bytes()));
        l.windows(want.len).any(|w| w == want.as_bytes())
    });
    if !a_line.is_some_and(|l| l.windows(7).any(|w| w == b"shared:")) {
        return 13;
    }
    // Unmounting below b removes the copy below a too.
    if umount2(p("b/x").as_ptr(), 0) != 0 || exists(&p("a/x/f")) {
        return 14;
    }

    // A private mount for the namespace tests.
    if tmpfs(&p("p")) != 0 || propagation(&p("p"), MS_PRIVATE) != 0 {
        return 20;
    }
    mkdir(&p("p/m"));
    let (outer, on) = ns_id(c"/proc/self/ns/mnt");
    if on == 0 || !outer.starts_with(b"mnt:[") {
        return 21;
    }
    let self_ns = open(c"/proc/self/ns/mnt".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0) as i32;
    let st = run_child(|| {
        if unsafe { syscall!(UNSHARE, CLONE_NEWNS) } != 0 {
            return 30;
        }
        let (inner, n) = ns_id(c"/proc/self/ns/mnt");
        if n == 0 || inner[..n] == outer[..on] {
            return 31;
        }
        // Private: stays in here.
        if tmpfs(&p("p/m")) != 0 || !touch(&p("p/m/h")) {
            return 36;
        }
        // Shared (copied as a peer): crosses into the parent's namespace.
        if tmpfs(&p("a/z")) != 0 || !touch(&p("a/z/k")) {
            return 37;
        }
        // setns back: the original id, without the private mount.
        if unsafe { syscall!(SETNS, self_ns, CLONE_NEWNS) } != 0 {
            return 34;
        }
        let (back, bn) = ns_id(c"/proc/self/ns/mnt");
        if back[..bn] != outer[..on] || exists(&p("p/m/h")) {
            return 35;
        }
        0
    });
    if status_code(st) != 0 {
        return status_code(st);
    }
    if exists(&p("p/m/h")) {
        return 38;
    }
    if !exists(&p("a/z/k")) || !exists(&p("b/z/k")) {
        return 39;
    }
    close(self_ns);

    // clone(CLONE_NEWNS): the child starts in a copy.
    let pid = unsafe { syscall!(CLONE, CLONE_NEWNS | SIGCHLD as usize, 0, 0, 0, 0) };
    if pid == 0 {
        let (inner, n) = ns_id(c"/proc/self/ns/mnt");
        let ok = n > 0 && inner[..n] != outer[..on] && exists(&p("a/z/k"));
        let ok = ok && tmpfs(&p("p/m")) == 0 && touch(&p("p/m/q"));
        exit(if ok { 0 } else { 1 });
    }
    let mut s = 0;
    wait4(pid as i32, &mut s, 0);
    if status_code(s) != 0 {
        return 50;
    }
    if exists(&p("p/m/q")) {
        return 51;
    }
    0
}

fn main() {
    let dir = scratch_dir();
    let base = cpath!("{}", Bytes(dir.as_bytes()));
    let (uid, gid) = (unsafe { syscall!(nr::GETUID) }, unsafe { syscall!(nr::GETGID) });
    let st = run_child(|| {
        let r = unsafe { syscall!(UNSHARE, CLONE_NEWUSER | CLONE_NEWNS) };
        if r != 0 {
            return 99;
        }
        // Be root inside (Linux: new files need a mapped fs uid). The Rust
        // kernel has no user namespaces: the files are absent there.
        let m = cpath!("0 {} 1", uid);
        write_file(c"/proc/self/uid_map".as_ptr() as *const u8, m.as_bytes());
        write_file(c"/proc/self/setgroups".as_ptr() as *const u8, b"deny");
        let m = cpath!("0 {} 1", gid);
        write_file(c"/proc/self/gid_map".as_ptr() as *const u8, m.as_bytes());
        // Linux makes everything copied into a less privileged namespace a
        // slave; our playground is a fresh tmpfs, so start private below it.
        in_namespace(&base)
    });
    let code = status_code(st);
    if code == 99 {
        skip("all", "cannot unshare a user + mount namespace");
        return;
    }
    // Map the failed step to the case it belongs to.
    let cases: [(&str, i32, i32); 8] = [
        ("setup", 1, 9),
        ("bind_peer_propagation", 10, 12),
        ("mountinfo_shared", 13, 13),
        ("umount_propagation", 14, 19),
        ("unshare_new_ns_id", 20, 31),
        ("setns_back", 32, 35),
        ("private_stays_shared_crosses", 36, 41),
        ("clone_newns", 50, 59),
    ];
    for (case, lo, hi) in cases {
        let failed_here = code >= lo && code <= hi;
        let failed_before = code != 0 && code < lo;
        if failed_before {
            check!(false, case, "not reached (failed at step {})", code);
        } else {
            check!(!failed_here, case, "failed at step {}", code);
        }
    }
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}
