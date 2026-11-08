//! C3: cgroups as init and libprocessgroup use them. cgroup2: mount,
//! cgroup.controllers / subtree_control, mkdir of uid_N/pid_M groups,
//! moving a process with cgroup.procs (and /proc/<pid>/cgroup showing
//! it), cgroup.events "populated", rmdir EBUSY while populated, children
//! inheriting their parent's group across fork, cgroup.kill. cgroup v1:
//! a cpuset,noprefix hierarchy (prefix-less files, controllers in
//! /proc/mounts, values copied from the parent), a memory hierarchy with
//! stored limits, the `tasks` file, remounting the same controllers
//! giving the same hierarchy, EBUSY for a controller already in use.
//!
//! Needs root on Linux (mounting cgroups): Rust kernel only.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_cgroup", main);

fn p(s: &str) -> Path {
    cpath!("{}", s)
}

fn read_str(path: &Path, buf: &mut [u8]) -> usize {
    read_file(path.as_ptr(), buf).max(0) as usize
}

fn contains_line(b: &[u8], line: &[u8]) -> bool {
    b.split(|&c| c == b'\n').any(|l| l == line)
}

fn main() {
    if !is_rust_kernel() {
        skip("all", "mounting cgroups needs root on Linux");
        return;
    }
    let mut b = [0u8; 4096];
    let none = c"none".as_ptr() as *const u8;

    // --- cgroup v2 -------------------------------------------------------
    mkdirat(AT_FDCWD, p("/tmp/cg2").as_ptr(), 0o755);
    check_eq!(mount(none, p("/tmp/cg2").as_ptr(), c"cgroup2".as_ptr() as *const u8, 0, c"memory_recursiveprot".as_ptr() as *const u8), 0, "mount_cgroup2");
    let n = read_str(&p("/tmp/cg2/cgroup.controllers"), &mut b);
    check!(b[..n].windows(6).any(|w| w == b"memory"), "v2_controllers");
    check_eq!(write_file(p("/tmp/cg2/cgroup.subtree_control").as_ptr(), b"+memory"), 7, "v2_subtree_control");
    check_eq!(mkdirat(AT_FDCWD, p("/tmp/cg2/uid_0").as_ptr(), 0o755), 0, "v2_mkdir");
    check_err!(mkdirat(AT_FDCWD, p("/tmp/cg2/uid_0").as_ptr(), 0o755), EEXIST, "v2_mkdir_exists");
    check_eq!(mkdirat(AT_FDCWD, p("/tmp/cg2/uid_0/pid_1").as_ptr(), 0o755), 0, "v2_mkdir_nested");
    let mut st = Stat::default();
    check!(fstatat(AT_FDCWD, p("/tmp/cg2/uid_0/pid_1/memory.max").as_ptr(), &mut st, 0) == 0, "v2_control_files");
    let n = read_str(&p("/tmp/cg2/uid_0/pid_1/cgroup.events"), &mut b);
    check!(contains_line(&b[..n], b"populated 0"), "v2_events_empty");
    check_err!(open(p("/tmp/cg2/uid_0/newfile").as_ptr(), O_CREAT | O_WRONLY, 0o644), EPERM, "v2_no_plain_files");

    // A child moves itself in; the group then lists it and is populated.
    let mut rd = [0i32; 2];
    pipe2(&mut rd, 0);
    let mut go = [0i32; 2];
    pipe2(&mut go, 0);
    let pid = fork();
    if pid == 0 {
        close(rd[0]);
        close(go[1]);
        let me = cpath!("{}", getpid());
        let r = write_file(p("/tmp/cg2/uid_0/pid_1/cgroup.procs").as_ptr(), me.as_bytes());
        write(rd[1], if r > 0 { b"y" } else { b"n" });
        let mut x = [0u8; 1];
        read(go[0], &mut x);
        exit(0);
    }
    close(rd[1]);
    close(go[0]);
    let mut x = [0u8; 1];
    read(rd[0], &mut x);
    check!(x[0] == b'y', "v2_move_into_group");
    let n = read_str(&p("/tmp/cg2/uid_0/pid_1/cgroup.procs"), &mut b);
    let me = cpath!("{}", pid);
    check!(contains_line(&b[..n], me.as_bytes()), "v2_procs_lists_member");
    let n = read_str(&cpath!("/proc/{}/cgroup", pid), &mut b);
    check!(contains_line(&b[..n], b"0::/uid_0/pid_1"), "proc_pid_cgroup", "{}", Bytes(&b[..n]));
    let n = read_str(&p("/tmp/cg2/uid_0/cgroup.events"), &mut b);
    check!(contains_line(&b[..n], b"populated 1"), "v2_parent_populated");
    check_err!(unlinkat(AT_FDCWD, p("/tmp/cg2/uid_0/pid_1").as_ptr(), AT_REMOVEDIR), EBUSY, "v2_rmdir_busy");
    check_err!(unlinkat(AT_FDCWD, p("/tmp/cg2/uid_0").as_ptr(), AT_REMOVEDIR), EBUSY, "v2_rmdir_parent_busy");
    let wr = write(go[1], b"x") as i32;
    let mut status = 0;
    wait4(pid as i32, &mut status, 0);
    check_eq!(wr, 1, "v2_child_released");
    let n = read_str(&p("/tmp/cg2/uid_0/pid_1/cgroup.procs"), &mut b);
    check_eq!(n, 0, "v2_exited_member_gone");
    check_eq!(unlinkat(AT_FDCWD, p("/tmp/cg2/uid_0/pid_1").as_ptr(), AT_REMOVEDIR), 0, "v2_rmdir_empty");

    // Fork inherits the group; cgroup.kill kills every member.
    mkdirat(AT_FDCWD, p("/tmp/cg2/killme").as_ptr(), 0o755);
    let pid = fork();
    if pid == 0 {
        let me = cpath!("{}", getpid());
        write_file(p("/tmp/cg2/killme/cgroup.procs").as_ptr(), me.as_bytes());
        if fork() == 0 {
            loop {
                sleep_ms(1000);
            }
        }
        loop {
            sleep_ms(1000);
        }
    }
    sleep_ms(200);
    let n = read_str(&p("/tmp/cg2/killme/cgroup.procs"), &mut b);
    check_eq!(b[..n].split(|&c| c == b'\n').filter(|l| !l.is_empty()).count(), 2, "v2_fork_inherits");
    check_eq!(write_file(p("/tmp/cg2/killme/cgroup.kill").as_ptr(), b"1"), 1, "v2_cgroup_kill");
    let mut status = 0;
    wait4(pid as i32, &mut status, 0);
    check!(wifsignaled(status) && wtermsig(status) == 9, "v2_killed", "{:#x}", status);
    sleep_ms(100);
    let n = read_str(&p("/tmp/cg2/killme/cgroup.procs"), &mut b);
    check_eq!(n, 0, "v2_kill_empties");
    // Back to the root for ourselves; groups go away.
    let me = cpath!("{}", getpid());
    check!(write_file(p("/tmp/cg2/cgroup.procs").as_ptr(), me.as_bytes()) > 0, "v2_move_to_root");
    unlinkat(AT_FDCWD, p("/tmp/cg2/killme").as_ptr(), AT_REMOVEDIR);
    unlinkat(AT_FDCWD, p("/tmp/cg2/uid_0").as_ptr(), AT_REMOVEDIR);

    // --- cgroup v1 -------------------------------------------------------
    mkdirat(AT_FDCWD, p("/tmp/cpuset").as_ptr(), 0o755);
    check_eq!(
        mount(none, p("/tmp/cpuset").as_ptr(), c"cgroup".as_ptr() as *const u8, 0, c"cpuset,noprefix,cpuset_v2_mode".as_ptr() as *const u8),
        0,
        "mount_cpuset"
    );
    let n = read_str(&p("/proc/mounts"), &mut b);
    check!(b[..n].split(|&c| c == b'\n').any(|l| l.starts_with(b"none /tmp/cpuset cgroup ") && l.windows(6).any(|w| w == b"cpuset")),
        "proc_mounts_controllers");
    let n = read_str(&p("/tmp/cpuset/cpus"), &mut b);
    let root_cpus = trim(&b[..n]);
    check!(!root_cpus.is_empty() && root_cpus[0] == b'0', "cpuset_noprefix_cpus");
    mkdirat(AT_FDCWD, p("/tmp/cpuset/foreground").as_ptr(), 0o755);
    check_eq!(write_file(p("/tmp/cpuset/foreground/cpus").as_ptr(), b"0"), 1, "cpuset_write_cpus");
    let n = read_str(&p("/tmp/cpuset/foreground/cpus"), &mut b);
    check!(trim(&b[..n]) == b"0", "cpuset_read_back");
    mkdirat(AT_FDCWD, p("/tmp/cpuset/foreground/boost").as_ptr(), 0o755);
    let n = read_str(&p("/tmp/cpuset/foreground/boost/cpus"), &mut b);
    check!(trim(&b[..n]) == b"0", "cpuset_child_inherits");
    let me = cpath!("{}", getpid());
    check!(write_file(p("/tmp/cpuset/foreground/tasks").as_ptr(), me.as_bytes()) > 0, "v1_tasks_write");
    let n = read_str(&p("/tmp/cpuset/foreground/tasks"), &mut b);
    check!(contains_line(&b[..n], me.as_bytes()), "v1_tasks_lists_tid");
    let n = read_str(&p("/proc/self/cgroup"), &mut b);
    check!(b[..n].split(|&c| c == b'\n').any(|l| l.ends_with(b":cpuset:/foreground")), "proc_self_cgroup_v1", "{}", Bytes(&b[..n]));
    write_file(p("/tmp/cpuset/tasks").as_ptr(), me.as_bytes());

    // Same controllers again: same hierarchy. A busy controller: EBUSY.
    mkdirat(AT_FDCWD, p("/tmp/cpuset2").as_ptr(), 0o755);
    check_eq!(mount(none, p("/tmp/cpuset2").as_ptr(), c"cgroup".as_ptr() as *const u8, 0, c"cpuset,noprefix".as_ptr() as *const u8), 0, "remount_same");
    check!(fstatat(AT_FDCWD, p("/tmp/cpuset2/foreground/boost").as_ptr(), &mut st, 0) == 0, "remount_same_hierarchy");
    mkdirat(AT_FDCWD, p("/tmp/mixed").as_ptr(), 0o755);
    check_err!(mount(none, p("/tmp/mixed").as_ptr(), c"cgroup".as_ptr() as *const u8, 0, c"cpuset,memory".as_ptr() as *const u8), EBUSY, "controller_busy");

    mkdirat(AT_FDCWD, p("/tmp/memcg").as_ptr(), 0o755);
    check_eq!(mount(none, p("/tmp/memcg").as_ptr(), c"cgroup".as_ptr() as *const u8, 0, c"memory".as_ptr() as *const u8), 0, "mount_memcg");
    mkdirat(AT_FDCWD, p("/tmp/memcg/apps").as_ptr(), 0o755);
    check_eq!(write_file(p("/tmp/memcg/apps/memory.limit_in_bytes").as_ptr(), b"104857600\n"), 10, "memcg_limit_write");
    let n = read_str(&p("/tmp/memcg/apps/memory.limit_in_bytes"), &mut b);
    check!(trim(&b[..n]) == b"104857600", "memcg_limit_read");
    check!(fstatat(AT_FDCWD, p("/tmp/memcg/apps/memory.swappiness").as_ptr(), &mut st, 0) == 0, "memcg_files");

    for d in ["/tmp/memcg/apps", "/tmp/cpuset/foreground/boost", "/tmp/cpuset/foreground"] {
        check_eq!(unlinkat(AT_FDCWD, p(d).as_ptr(), AT_REMOVEDIR), 0, "v1_rmdir");
    }
    for m in ["/tmp/memcg", "/tmp/cpuset2", "/tmp/cpuset", "/tmp/cg2"] {
        umount2(p(m).as_ptr(), 0);
    }
}
