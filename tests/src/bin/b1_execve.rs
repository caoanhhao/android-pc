//! B1: execve — argv/envp passing, pid preserved, O_CLOEXEC, signal
//! disposition reset, blocked mask kept, error cases. The test re-executes
//! itself as `b1_execve --rt-child <case> ...`.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b1_execve", main);

fn main() {
    if is_child_mode() {
        return child();
    }

    check!(exec_self(&[b"args\0", b"hello world\0", b"\0"], &[b"KTEST_A=1\0", b"KTEST_B=two\0"]) == 0,
           "argv_envp_passed");

    // Within a forked child the pid differs from ours; pass the child's own.
    let st = run_child(|| {
        let me = cpath!("{}", getpid());
        exec_self_raw(&[b"pid\0", nul(&me)], &[])
    });
    check_eq!(status_code(st), 0, "pid_preserved");

    // CLOEXEC fds are closed, others survive.
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    fcntl(p[0], F_SETFD, FD_CLOEXEC as usize);
    let (a, b) = (cpath!("{}", p[0]), cpath!("{}", p[1]));
    let st = run_child(|| exec_self_raw(&[b"cloexec\0", nul(&a), nul(&b)], &[]));
    check_eq!(status_code(st), 0, "cloexec_closed_others_kept");
    close(p[0]);
    close(p[1]);

    // Handled signals reset to SIG_DFL; ignored stay ignored; mask is kept.
    let st = run_child(|| {
        signal(SIGUSR1, handler, 0);
        let ign = Sigaction { handler: SIG_IGN, flags: SA_RESTORER, restorer: restorer(), mask: 0 };
        sigaction(SIGUSR2, Some(&ign), None);
        sigprocmask(SIG_BLOCK, Some(sigbit(SIGHUP)), None);
        exec_self_raw(&[b"signals\0"], &[])
    });
    check_eq!(status_code(st), 0, "signal_dispositions_after_exec");

    // Error cases (in the parent: a failed execve must return).
    let argv = [c"x".as_ptr() as *const u8, core::ptr::null()];
    let envp = [core::ptr::null::<u8>()];
    check_err!(execve(c"/nonexistent/ktest".as_ptr() as *const u8, argv.as_ptr(), envp.as_ptr()), ENOENT,
               "execve_enoent");
    check_err!(execve(c"/".as_ptr() as *const u8, argv.as_ptr(), envp.as_ptr()), EACCES, "execve_dir_eacces");

    // Executing a non-ELF file without #! fails with ENOEXEC (8).
    let dir = scratch_dir();
    let f = cpath!("{}/notelf", Bytes(dir.as_bytes()));
    let fd = open(f.as_ptr(), O_CREAT | O_WRONLY | O_TRUNC, 0o755) as i32;
    write(fd, b"\x01\x02garbage-not-an-elf-file\n");
    close(fd);
    check_err!(execve(f.as_ptr(), argv.as_ptr(), envp.as_ptr()), 8, "execve_enoexec");
    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}

extern "C" fn handler(_: i32) {}

fn nul(b: &Path) -> &[u8] {
    &b.buf[..b.len + 1]
}

/// Fork + exec ourselves with `--rt-child <args>`; returns the exit code.
fn exec_self(args: &[&[u8]], env: &[&[u8]]) -> i32 {
    status_code(run_child(|| exec_self_raw(args, env)))
}

/// execve(argv[0], [argv0, --rt-child, args...], env). Returns only on error.
fn exec_self_raw(args: &[&[u8]], env: &[&[u8]]) -> i32 {
    let mut av: [*const u8; 16] = [core::ptr::null(); 16];
    av[0] = argv_raw(0).unwrap();
    av[1] = b"--rt-child\0".as_ptr();
    for (i, a) in args.iter().enumerate() {
        av[2 + i] = a.as_ptr();
    }
    let mut ev: [*const u8; 16] = [core::ptr::null(); 16];
    for (i, e) in env.iter().enumerate() {
        ev[i] = e.as_ptr();
    }
    let r = execve(av[0], av.as_ptr(), ev.as_ptr());
    println!("# execve failed: {}", errname(r));
    127
}

fn child() {
    let case = argv(2).unwrap_or(b"");
    match case {
        b"args" => {
            check_eq!(argc(), 5, "argc");
            check_eq!(argv(3), Some(&b"hello world"[..]), "argv_space");
            check_eq!(argv(4), Some(&b""[..]), "argv_empty");
            check_eq!(envc(), 2, "envc");
            check_eq!(env(0), Some(&b"KTEST_A=1"[..]), "env0");
            check_eq!(env(1), Some(&b"KTEST_B=two"[..]), "env1");
            check!(getauxval(AT_PAGESZ) == Some(4096), "auxv_after_exec");
            let execfn = getauxval(AT_EXECFN).map(|p| unsafe { cstr_bytes(p as *const u8) });
            check_eq!(execfn, argv(0), "auxv_execfn");
        }
        b"pid" => {
            let want = argv(3).and_then(parse_int).unwrap_or(-1);
            check_eq!(getpid() as i64, want, "pid_same");
        }
        b"cloexec" => {
            let a = argv(3).and_then(parse_int).unwrap_or(-1) as i32;
            let b = argv(4).and_then(parse_int).unwrap_or(-1) as i32;
            check_err!(fcntl(a, F_GETFD, 0), EBADF, "cloexec_fd_closed");
            check_eq!(fcntl(b, F_GETFD, 0), 0, "plain_fd_open");
        }
        b"signals" => {
            let mut old = Sigaction::default();
            sigaction(SIGUSR1, None, Some(&mut old));
            check_eq!(old.handler, SIG_DFL, "handled_reset_to_default");
            sigaction(SIGUSR2, None, Some(&mut old));
            check_eq!(old.handler, SIG_IGN, "ignored_stays_ignored");
            let mut mask = 0u64;
            sigprocmask(SIG_BLOCK, None, Some(&mut mask));
            check!(mask & sigbit(SIGHUP) != 0, "blocked_mask_kept");
        }
        _ => {
            check!(false, "unknown_child_case");
        }
    }
}
