//! C3: the SELinux stub, as libselinux and init use it. selinuxfs
//! (enforce, policy load in chunks, the mmap-able status page with its
//! sequence and policyload counters, context/access/create transactions,
//! deny_unknown, policyvers, null), per-process attributes in
//! /proc/self/attr and /proc/thread-self/attr (current, exec applied at
//! execve, fscreate labelling new files, sockcreate), security.selinux
//! xattrs on tmpfs (set/get/list/remove, XATTR_CREATE/REPLACE, the
//! "unlabeled" default) and SO_PEERSEC.
//!
//! Rust kernel only: the host may not run SELinux, and loading a policy
//! there would be destructive.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_selinux", main);

const SETXATTR: usize = 188;
const GETXATTR: usize = 191;
const LISTXATTR: usize = 194;
const REMOVEXATTR: usize = 197;
const XATTR_CREATE: usize = 1;
const XATTR_REPLACE: usize = 2;
const SO_PEERSEC: i32 = 31;
const SELINUX_MAGIC: i64 = 0xf97c_ff8c;
const STATFS: usize = 137;
const MNT: &core::ffi::CStr = c"/tmp/c3-selinuxfs";

fn node(name: &str) -> Path {
    cpath!("/tmp/c3-selinuxfs/{}", name)
}

fn read_node(name: &str, buf: &mut [u8]) -> usize {
    read_file(node(name).as_ptr(), buf).max(0) as usize
}

/// write then read on one fd: a selinuxfs transaction.
fn transact(name: &str, req: &[u8], out: &mut [u8]) -> isize {
    let fd = open(node(name).as_ptr(), O_RDWR, 0) as i32;
    if fd < 0 {
        return fd as isize;
    }
    let w = write(fd, req);
    if w < 0 {
        close(fd);
        return w;
    }
    let r = read(fd, out);
    close(fd);
    r
}

fn status_words(page: *const u32) -> [u32; 5] {
    let mut w = [0u32; 5];
    for (i, x) in w.iter_mut().enumerate() {
        *x = unsafe { core::ptr::read_volatile(page.add(i)) };
    }
    w
}

fn child() {
    // After execve with attr/exec set: running in that context.
    let mut b = [0u8; 64];
    let n = read_file(c"/proc/self/attr/current".as_ptr() as *const u8, &mut b);
    check!(n > 0 && trim(&b[..n as usize]) == b"u:r:c3_exec_test:s0", "exec_context", "{}", Bytes(&b[..n.max(0) as usize]));
    let n = read_file(c"/proc/self/attr/exec".as_ptr() as *const u8, &mut b);
    check_eq!(n, 0, "exec_attr_reset");
}

fn main() {
    if is_child_mode() {
        return child();
    }
    if !is_rust_kernel() {
        skip("all", "SELinux stub tests need the Rust kernel");
        return;
    }
    mkdirat(AT_FDCWD, MNT.as_ptr() as *const u8, 0o755);
    let r = mount(c"selinuxfs".as_ptr() as *const u8, MNT.as_ptr() as *const u8, c"selinuxfs".as_ptr() as *const u8, 0, core::ptr::null());
    check_eq!(r, 0, "mount_selinuxfs");
    let mut sfs = [0i64; 15];
    unsafe { syscall!(STATFS, MNT.as_ptr(), sfs.as_mut_ptr()) };
    check_eq!(sfs[0], SELINUX_MAGIC, "statfs_magic");

    let mut b = [0u8; 256];
    let n = read_node("deny_unknown", &mut b);
    check!(trim(&b[..n]) == b"0", "deny_unknown_0");
    let n = read_node("policyvers", &mut b);
    check!(parse_int(trim(&b[..n])).is_some_and(|v| v >= 30), "policyvers");
    let n = read_node("mls", &mut b);
    check!(trim(&b[..n]) == b"1", "mls");

    // The status page, mapped like selinux_status_open does.
    let sfd = open(node("status").as_ptr(), O_RDONLY, 0) as i32;
    let page = mmap(0, 4096, PROT_READ, MAP_SHARED, sfd, 0);
    check!(page > 0, "status_mmap", "{}", errname(page));
    let page = page as *const u32;
    let s0 = status_words(page);
    check!(s0[0] == 1 && s0[1] % 2 == 0, "status_version_seq");

    // enforce: stored and reported; the status page follows.
    let n = read_node("enforce", &mut b);
    let was = trim(&b[..n]);
    check!(was == b"0" || was == b"1", "enforce_read");
    check_eq!(write_file(node("enforce").as_ptr(), b"1"), 1, "enforce_write_1");
    let n = read_node("enforce", &mut b);
    check!(trim(&b[..n]) == b"1", "enforce_reads_1");
    let s1 = status_words(page);
    check!(s1[2] == 1 && s1[1] > s0[1], "status_enforcing_seq");
    check_eq!(write_file(node("enforce").as_ptr(), b"0"), 1, "enforce_write_0");
    check_err!(write_file(node("enforce").as_ptr(), b"x"), EINVAL, "enforce_bad");

    // A policy arrives in several chunks: one load.
    let fd = open(node("load").as_ptr(), O_WRONLY, 0) as i32;
    let mut pol = [0u8; 200_000];
    pol[..8].copy_from_slice(&[0x8c, 0xff, 0x7c, 0xf9, 8, 0, 0, 0]);
    check_eq!(write(fd, &pol), 200_000, "policy_load_write");
    close(fd);
    let s2 = status_words(page);
    check_eq!(s2[3], s1[3] + 1, "status_policyload_once");

    // Transactions.
    let n = transact("context", b"u:object_r:system_file:s0\0", &mut b);
    check!(n > 0 && trim(&b[..n as usize]) == b"u:object_r:system_file:s0", "context_valid");
    check_err!(transact("context", b"garbage\0", &mut b), EINVAL, "context_invalid");
    let n = transact("access", b"u:r:init:s0 u:object_r:system_file:s0 7 1", &mut b);
    check!(n > 0 && b[..n as usize].starts_with(b"ffffffff ffffffff"), "access_allows", "{}", Bytes(&b[..n.max(0) as usize]));
    let n = transact("create", b"u:r:init:s0 u:object_r:device:s0 9", &mut b);
    check!(n > 0 && trim(&b[..n as usize]) == b"u:object_r:device:s0", "create_target");
    let mut st = Stat::default();
    check!(fstatat(AT_FDCWD, node("null").as_ptr(), &mut st, 0) == 0 && st.mode & S_IFMT == S_IFCHR, "null_node");

    // Process attributes.
    let n = read_file(c"/proc/self/attr/current".as_ptr() as *const u8, &mut b);
    check!(n > 0 && trim(&b[..n as usize]).starts_with(b"u:r:"), "attr_current");
    let mut c2 = [0u8; 64];
    let n2 = read_file(c"/proc/thread-self/attr/current".as_ptr() as *const u8, &mut c2);
    check!(n2 == n && c2[..n2 as usize] == b[..n as usize], "thread_self_attr");
    check_err!(write_file(c"/proc/self/attr/fscreate".as_ptr() as *const u8, b"bad"), EINVAL, "attr_invalid");

    // fscreate labels new files; xattrs on tmpfs.
    let dir = scratch_dir();
    check_eq!(write_file(c"/proc/thread-self/attr/fscreate".as_ptr() as *const u8, b"u:object_r:c3_file:s0\0"), 22, "set_fscreate");
    let f = cpath!("{}/labelled", Bytes(dir.as_bytes()));
    close(open(f.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32);
    write_file(c"/proc/thread-self/attr/fscreate".as_ptr() as *const u8, b"");
    let n = unsafe { syscall!(GETXATTR, f.as_ptr(), c"security.selinux".as_ptr(), b.as_mut_ptr(), b.len()) };
    check!(n > 0 && trim(&b[..n as usize]) == b"u:object_r:c3_file:s0", "fscreate_label", "{}", errname(n));
    let g = cpath!("{}/plain", Bytes(dir.as_bytes()));
    close(open(g.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32);
    let n = unsafe { syscall!(GETXATTR, g.as_ptr(), c"security.selinux".as_ptr(), b.as_mut_ptr(), b.len()) };
    check!(n > 0 && trim(&b[..n as usize]) == b"u:object_r:unlabeled:s0", "default_unlabeled");
    let ctx = b"u:object_r:c3_set:s0\0";
    check_eq!(unsafe { syscall!(SETXATTR, g.as_ptr(), c"security.selinux".as_ptr(), ctx.as_ptr(), ctx.len(), 0) }, 0, "setxattr");
    let n = unsafe { syscall!(GETXATTR, g.as_ptr(), c"security.selinux".as_ptr(), b.as_mut_ptr(), b.len()) };
    check!(n == ctx.len() as isize && &b[..ctx.len()] == ctx, "getxattr_after_set");
    check_err!(unsafe { syscall!(SETXATTR, g.as_ptr(), c"user.x".as_ptr(), b"1".as_ptr(), 1, XATTR_REPLACE) }, ENODATA, "xattr_replace_missing");
    check_eq!(unsafe { syscall!(SETXATTR, g.as_ptr(), c"user.x".as_ptr(), b"1".as_ptr(), 1, XATTR_CREATE) }, 0, "xattr_create");
    check_err!(unsafe { syscall!(SETXATTR, g.as_ptr(), c"user.x".as_ptr(), b"2".as_ptr(), 1, XATTR_CREATE) }, EEXIST, "xattr_create_exists");
    let n = unsafe { syscall!(LISTXATTR, g.as_ptr(), b.as_mut_ptr(), b.len()) };
    let names = &b[..n.max(0) as usize];
    check!(names.split(|&c| c == 0).any(|x| x == b"user.x") && names.split(|&c| c == 0).any(|x| x == b"security.selinux"), "listxattr");
    check_eq!(unsafe { syscall!(REMOVEXATTR, g.as_ptr(), c"user.x".as_ptr()) }, 0, "removexattr");
    check_err!(unsafe { syscall!(GETXATTR, g.as_ptr(), c"user.x".as_ptr(), b.as_mut_ptr(), b.len()) }, ENODATA, "getxattr_removed");

    // SO_PEERSEC: the peer's context.
    let mut sv = [0i32; 2];
    socketpair(AF_UNIX, SOCK_STREAM, &mut sv);
    let mut len = b.len() as u32;
    let r = getsockopt(sv[0], SOL_SOCKET, SO_PEERSEC, b.as_mut_ptr(), &mut len);
    check!(r == 0 && trim(&b[..len as usize]).starts_with(b"u:r:"), "so_peersec", "{}", errname(r));
    close(sv[0]);
    close(sv[1]);

    // attr/exec: the context after execve.
    let st = run_child(|| {
        write_file(c"/proc/self/attr/exec".as_ptr() as *const u8, b"u:r:c3_exec_test:s0");
        let me = argv_raw(0).unwrap();
        let av = [me, c"--rt-child".as_ptr() as *const u8, core::ptr::null()];
        execve(me, av.as_ptr(), envp_ptr());
        99
    });
    check_eq!(status_code(st), 0, "exec_child");

    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    unlinkat(AT_FDCWD, g.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
    munmap(page as *mut u8, 4096);
    close(sfd);
    umount2(MNT.as_ptr() as *const u8, 0);
}
