//! D3: process and memory interfaces of zygote, system_server and lmkd.
//! pidfd_open (flags, ESRCH), poll on a pidfd (POLLIN at exit),
//! pidfd_send_signal (EBADF on other fds, ESRCH after exit),
//! clone(CLONE_PIDFD), pidfd_getfd, process_madvise, process_mrelease
//! flags; membarrier (QUERY, EPERM before REGISTER_PRIVATE_EXPEDITED,
//! GET_REGISTRATIONS); /proc/self/stat startstack inside the [stack]
//! VMA (bionic's pthread_getattr_np); VMAs split by mprotect merging
//! back (mremap of the whole range, bionic's atexit array); ashmem
//! (/dev/ashmem: names, sizes, shared mappings across fork, prot mask,
//! pinning; skipped without the device); AF_VSOCK streams to the local
//! CID (skipped without a loopback transport); PI futexes (LOCK_PI,
//! LOCK_PI2 with a timeout, TRYLOCK_PI, UNLOCK_PI, FUTEX_WAITERS, EDEADLK,
//! EPERM, hand-over to a blocked thread); access(W_OK) on a read-only
//! mount (EROFS: ART refuses dex files that look writable); AF_KEY sockets (libbpf's
//! synchronizeKernelRCU); /dev/net/tun (TUNSETIFF makes a link, gone at
//! close; skipped without the device or CAP_NET_ADMIN); /dev/hvc1
//! (Rust kernel: raw termios, writes accepted, nothing to read);
//! AF_PACKET on eth0 (an ARP request for the gateway sent as a raw frame,
//! the reply received through a classic BPF filter using SKF_NET_OFF;
//! skipped without eth0 or CAP_NET_RAW); /proc/config.gz (a gzip of
//! CONFIG_ lines); /proc/self/cmdline following argv[0] rewrites in
//! memory; CPU time in /proc/self/stat; /proc/self/task/<tid>/wchan.
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering::SeqCst};
use rt::*;

rt::test_main!("d3_proc", main);

const PG: usize = 4096;
const NR_PIDFD_OPEN: usize = 434;
const NR_PIDFD_SEND_SIGNAL: usize = 424;
const NR_PIDFD_GETFD: usize = 438;
const NR_PROCESS_MADVISE: usize = 440;
const NR_PROCESS_MRELEASE: usize = 448;
const NR_MEMBARRIER: usize = 324;
const CLONE_PIDFD: usize = 0x1000;
const SIGCHLD: usize = 17;

fn pidfd_open(pid: i32, flags: u32) -> isize {
    unsafe { syscall!(NR_PIDFD_OPEN, pid, flags) }
}
fn pidfd_send_signal(fd: i32, sig: i32) -> isize {
    unsafe { syscall!(NR_PIDFD_SEND_SIGNAL, fd, sig, 0, 0) }
}
fn membarrier(cmd: u32) -> isize {
    unsafe { syscall!(NR_MEMBARRIER, cmd, 0, 0) }
}

fn pollin(fd: i32, ms: i32) -> bool {
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    poll(&mut p, ms) == 1 && p[0].revents & POLLIN != 0
}

/// Parse a hex number at the start of `b`; returns (value, rest).
fn hex(b: &[u8]) -> (u64, &[u8]) {
    let mut v = 0u64;
    let mut i = 0;
    while i < b.len() {
        let d = match b[i] {
            c @ b'0'..=b'9' => c - b'0',
            c @ b'a'..=b'f' => c - b'a' + 10,
            _ => break,
        };
        v = v * 16 + d as u64;
        i += 1;
    }
    (v, &b[i..])
}

fn pidfd_tests() {
    check_err!(pidfd_open(getpid(), 0x1234), EINVAL, "pidfd_open_bad_flags");
    check_err!(pidfd_open(-1, 0), EINVAL, "pidfd_open_negative_pid");
    check_err!(pidfd_open(4_000_000, 0), ESRCH, "pidfd_open_no_such_pid");

    let child = fork();
    if child == 0 {
        loop {
            sleep_ms(1000);
        }
    }
    let pfd = check_ok!(pidfd_open(child as i32, 0), "pidfd_open") as i32;
    check_eq!(fcntl(pfd, F_GETFD, 0), FD_CLOEXEC as isize, "pidfd_cloexec");
    check!(!pollin(pfd, 0), "pidfd_not_readable_while_alive");
    check_eq!(pidfd_send_signal(pfd, 0), 0, "pidfd_send_signal_0");
    check_eq!(pidfd_send_signal(pfd, SIGKILL), 0, "pidfd_send_signal_kill");
    check!(pollin(pfd, 5000), "pidfd_pollin_at_exit");
    let mut st = 0;
    wait4(child as i32, &mut st, 0);
    check!(wifsignaled(st) && wtermsig(st) == SIGKILL, "pidfd_killed_child", "status {:#x}", st);
    check_err!(pidfd_send_signal(pfd, SIGTERM), ESRCH, "pidfd_send_signal_after_exit_esrch");
    close(pfd);

    let mut p = [0i32; 2];
    pipe2(&mut p, O_CLOEXEC);
    check_err!(pidfd_send_signal(p[0], SIGTERM), EBADF, "pidfd_send_signal_not_pidfd_ebadf");

    // clone(CLONE_PIDFD): the pidfd comes back through parent_tid.
    let mut cfd: i32 = -1;
    let r = unsafe { syscall!(56, CLONE_PIDFD | SIGCHLD, 0, &mut cfd as *mut i32, 0, 0) };
    if r == 0 {
        exit(7);
    }
    check!(r > 0 && cfd >= 0, "clone_pidfd", "clone {} pidfd {}", r, cfd);
    check!(pollin(cfd, 5000), "clone_pidfd_pollin_at_exit");
    wait4(r as i32, &mut st, 0);
    check_eq!(status_code(st), 7, "clone_pidfd_child_status");
    check_eq!(fcntl(cfd, F_GETFD, 0), FD_CLOEXEC as isize, "clone_pidfd_cloexec");
    close(cfd);

    // pidfd_getfd on ourselves.
    let me = pidfd_open(getpid(), 0) as i32;
    let dupfd = unsafe { syscall!(NR_PIDFD_GETFD, me, p[1], 0) };
    if dupfd == -(EPERM as isize) {
        skip("pidfd_getfd", "not permitted");
    } else {
        check!(dupfd >= 0 && dupfd as i32 != p[1], "pidfd_getfd", "returned {}", errname(dupfd));
        write(dupfd as i32, b"x");
        let mut b = [0u8; 1];
        check_eq!(read(p[0], &mut b), 1, "pidfd_getfd_same_pipe");
        check_eq!(fcntl(dupfd as i32, F_GETFD, 0), FD_CLOEXEC as isize, "pidfd_getfd_cloexec");
        close(dupfd as i32);
    }
    // process_madvise hints on our own memory.
    let m = alloc_pages(2 * PG);
    let iov = [m as usize, 2 * PG];
    let r = unsafe { syscall!(NR_PROCESS_MADVISE, me, iov.as_ptr(), 1, 20, 0) }; // MADV_COLD
    if r == -(EPERM as isize) {
        skip("process_madvise", "not permitted");
    } else {
        check_eq!(r, (2 * PG) as isize, "process_madvise_cold");
    }
    check_err!(unsafe { syscall!(NR_PROCESS_MADVISE, me, iov.as_ptr(), 1, 99, 0) }, EINVAL, "process_madvise_bad_advice_einval");
    check_err!(unsafe { syscall!(NR_PROCESS_MRELEASE, me, 1) }, EINVAL, "process_mrelease_flags_einval");
    close(me);
    close(p[0]);
    close(p[1]);
}

fn membarrier_tests() {
    let q = membarrier(0);
    check!(q > 0 && q & 8 != 0 && q & 16 != 0, "membarrier_query", "mask {:#x}", q);
    check_err!(membarrier(8), EPERM, "membarrier_private_unregistered_eperm");
    check_eq!(membarrier(16), 0, "membarrier_register_private_expedited");
    check_eq!(membarrier(8), 0, "membarrier_private_expedited");
    check_err!(membarrier(0x4000), EINVAL, "membarrier_bad_cmd");
    let regs = membarrier(512);
    check!(regs >= 0 && regs & 16 != 0, "membarrier_get_registrations", "returned {}", regs);
}

fn stack_and_vma_tests() {
    // /proc/self/stat field 28 (startstack) lies in the [stack] mapping.
    let mut b = [0u8; 1024];
    let n = read_file(c"/proc/self/stat".as_ptr() as *const u8, &mut b);
    let stat = &b[..n.max(0) as usize];
    let after = stat.iter().rposition(|&c| c == b')').map_or(&stat[..0], |i| &stat[i + 2..]);
    // Fields from 3 (state) on.
    let start_stack = after.split(|&c| c == b' ').nth(28 - 3).and_then(parse_int).unwrap_or(0) as u64;
    let mut maps = [0u8; 65536];
    let n = read_file(c"/proc/self/maps".as_ptr() as *const u8, &mut maps);
    let found = maps[..n.max(0) as usize].split(|&c| c == b'\n').any(|line| {
        let (lo, rest) = hex(line);
        let (hi, _) = hex(rest.get(1..).unwrap_or(&[]));
        line.ends_with(b"[stack]") && lo <= start_stack && start_stack < hi
    });
    check!(found, "stat_startstack_in_stack_vma", "startstack {:#x}", start_stack);

    // Pages mprotect()ed apart and back merge into one VMA again.
    let a = mmap(0, 3 * PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) as usize;
    unsafe { wr((a + PG) as *mut u8, 0x42) };
    mprotect((a + PG) as *mut u8, PG, PROT_READ);
    mprotect((a + PG) as *mut u8, PG, PROT_READ | PROT_WRITE);
    let r = unsafe { syscall!(25, a, 3 * PG, 64 * PG, 1) }; // MREMAP_MAYMOVE
    check!(r > 0, "mprotect_pieces_merge_for_mremap", "mremap {}", errname(r));
    check_eq!(unsafe { rd((r as usize + PG) as *const u8) }, 0x42, "mremap_kept_data");
}

const fn iow(nr: u32, size: u32) -> u32 {
    (1 << 30) | (size << 16) | (0x77 << 8) | nr
}
const fn ior(nr: u32, size: u32) -> u32 {
    (2 << 30) | (size << 16) | (0x77 << 8) | nr
}
const ASHMEM_SET_NAME: u32 = iow(1, 256);
const ASHMEM_GET_NAME: u32 = ior(2, 256);
const ASHMEM_SET_SIZE: u32 = iow(3, 8);
const ASHMEM_GET_SIZE: u32 = 0x7704;
const ASHMEM_SET_PROT_MASK: u32 = iow(5, 8);
const ASHMEM_GET_PROT_MASK: u32 = 0x7706;
const ASHMEM_PIN: u32 = iow(7, 8);
const ASHMEM_GET_PIN_STATUS: u32 = 0x7709;

fn ashmem_tests() {
    let fd = open(c"/dev/ashmem".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if fd < 0 {
        skip("ashmem", "no /dev/ashmem");
        return;
    }
    let fd = fd as i32;
    check_err!(mmap(0, PG, PROT_READ, MAP_SHARED, fd, 0), EINVAL, "ashmem_mmap_without_size_einval");
    check_ok!(ioctl(fd, ASHMEM_SET_NAME, c"ktest-region".as_ptr() as usize), "ashmem_set_name");
    let mut name = [0u8; 256];
    check_ok!(ioctl(fd, ASHMEM_GET_NAME, name.as_mut_ptr() as usize), "ashmem_get_name");
    check!(name.starts_with(b"ktest-region\0"), "ashmem_name_round_trip");
    check_ok!(ioctl(fd, ASHMEM_SET_SIZE, 2 * PG), "ashmem_set_size");
    check_eq!(ioctl(fd, ASHMEM_GET_SIZE, 0), (2 * PG) as isize, "ashmem_get_size");
    let a = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check!(a > 0, "ashmem_mmap_shared");
    let a = a as *mut u8;
    let st = run_child(|| {
        unsafe { wr(a.add(PG + 3), 0x5c) };
        0
    });
    check!(st == 0 && unsafe { rd(a.add(PG + 3)) } == 0x5c, "ashmem_shared_across_fork");
    let b = mmap(0, 2 * PG, PROT_READ, MAP_SHARED, fd, 0) as *const u8;
    check_eq!(unsafe { rd(b.add(PG + 3)) }, 0x5c, "ashmem_second_mapping_aliases");
    check_err!(ioctl(fd, ASHMEM_SET_SIZE, PG), EINVAL, "ashmem_set_size_after_mmap_einval");
    check_eq!(ioctl(fd, ASHMEM_GET_PROT_MASK, 0), 7, "ashmem_default_prot_mask");
    check_ok!(ioctl(fd, ASHMEM_SET_PROT_MASK, PROT_READ), "ashmem_set_prot_mask_read");
    check_err!(ioctl(fd, ASHMEM_SET_PROT_MASK, PROT_READ | PROT_WRITE), EINVAL, "ashmem_prot_mask_cannot_grow");
    check_err!(mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0), EPERM, "ashmem_write_mapping_after_mask_eperm");
    let pin = [0u32, 0u32];
    check_eq!(ioctl(fd, ASHMEM_PIN, pin.as_ptr() as usize), 0, "ashmem_pin_not_purged");
    check_eq!(ioctl(fd, ASHMEM_GET_PIN_STATUS, 0), 1, "ashmem_is_pinned");
    let mut s = Stat::default();
    fstat(fd, &mut s);
    check!(s.mode & S_IFMT == S_IFCHR, "ashmem_fd_is_char_device");
    close(fd);
}

#[repr(C)]
#[derive(Default)]
struct SockaddrVm {
    family: u16,
    reserved: u16,
    port: u32,
    cid: u32,
    flags: u8,
    zero: [u8; 3],
}
const AF_VSOCK: i32 = 40;

fn vm(port: u32, cid: u32) -> SockaddrVm {
    SockaddrVm { family: AF_VSOCK as u16, port, cid, ..Default::default() }
}

fn vsock_tests() {
    let srv = socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC);
    if srv < 0 {
        skip("vsock", "no AF_VSOCK");
        return;
    }
    let srv = srv as i32;
    let a = vm(41234, u32::MAX);
    check_ok!(unsafe { syscall!(49, srv, &a as *const SockaddrVm, 16) }, "vsock_bind_cid_any");
    check_ok!(listen(srv, 4), "vsock_listen");
    let cli = socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC) as i32;
    let to = vm(41234, 1);
    let r = unsafe { syscall!(42, cli, &to as *const SockaddrVm, 16) };
    if r < 0 {
        skip("vsock_local", "no local vsock transport");
        close(cli);
        close(srv);
        return;
    }
    let c = check_ok!(accept4(srv, SOCK_CLOEXEC), "vsock_accept") as i32;
    check_eq!(write(cli, b"vsock!"), 6, "vsock_send");
    let mut b = [0u8; 16];
    check_eq!(read(c, &mut b), 6, "vsock_recv");
    check_eq!(&b[..6], b"vsock!", "vsock_data");
    let mut name = SockaddrVm::default();
    let mut len: u32 = 16;
    check_ok!(unsafe { syscall!(51, srv, &mut name as *mut SockaddrVm, &mut len as *mut u32) }, "vsock_getsockname");
    check!(name.family == AF_VSOCK as u16 && name.port == 41234, "vsock_sockname", "family {} port {}", name.family, name.port);
    let mut dom = 0i32;
    let mut dl: u32 = 4;
    getsockopt(c, SOL_SOCKET, 39, &mut dom as *mut i32 as *mut u8, &mut dl); // SO_DOMAIN
    check_eq!(dom, AF_VSOCK, "vsock_so_domain");
    close(c);
    close(cli);
    close(srv);
}

static PI_WORD: AtomicU32 = AtomicU32::new(0);
static PI_GOT: AtomicI32 = AtomicI32::new(0);

fn futex_pi(op: i32, timeout: *const Timespec) -> isize {
    unsafe { syscall!(202, PI_WORD.as_ptr(), op | 128, 0, timeout, 0, 0) }
}

extern "C" fn pi_locker(_: usize) -> i32 {
    let r = futex_pi(6, core::ptr::null()); // FUTEX_LOCK_PI
    PI_GOT.store(if r == 0 { gettid() } else { -1 }, SeqCst);
    let ok = PI_WORD.load(SeqCst) & 0x3fff_ffff == gettid() as u32;
    futex_pi(7, core::ptr::null()); // FUTEX_UNLOCK_PI
    if ok { 0 } else { 1 }
}

fn pi_futex_tests() {
    let me = gettid() as u32;
    check_eq!(futex_pi(6, core::ptr::null()), 0, "lock_pi_free");
    check_eq!(PI_WORD.load(SeqCst), me, "lock_pi_word_is_tid");
    check_err!(futex_pi(6, core::ptr::null()), EDEADLK, "lock_pi_twice_edeadlk");
    let t = spawn(pi_locker, 0, 0);
    check!(wait_until_ms(|| PI_WORD.load(SeqCst) & 0x8000_0000 != 0, 2000), "lock_pi_waiter_sets_futex_waiters");
    check_eq!(PI_GOT.load(SeqCst), 0, "lock_pi_blocks_while_held");
    check_eq!(futex_pi(7, core::ptr::null()), 0, "unlock_pi");
    t.join();
    check!(PI_GOT.load(SeqCst) > 0 && PI_GOT.load(SeqCst) != me as i32, "lock_pi_handed_to_waiter", "got {}", PI_GOT.load(SeqCst));
    check_eq!(PI_WORD.load(SeqCst) & 0x3fff_ffff, 0, "lock_pi_released_by_waiter");
    check_err!(futex_pi(7, core::ptr::null()), EPERM, "unlock_pi_not_owner_eperm");
    check_eq!(futex_pi(8, core::ptr::null()), 0, "trylock_pi_free");
    check_eq!(futex_pi(7, core::ptr::null()), 0, "unlock_pi_after_trylock");
    // Held by another thread: TRYLOCK_PI fails, LOCK_PI2 times out on
    // CLOCK_MONOTONIC.
    PI_RELEASE.store(0, SeqCst);
    let holder = spawn(pi_holder, 0, 0);
    check!(wait_until_ms(|| PI_WORD.load(SeqCst) != 0, 2000), "pi_holder_locked");
    check_err!(futex_pi(8, core::ptr::null()), EAGAIN, "trylock_pi_held_eagain");
    let mut ts = Timespec::default();
    clock_gettime(CLOCK_MONOTONIC, &mut ts);
    ts.nsec += 30_000_000;
    if ts.nsec >= 1_000_000_000 {
        ts.sec += 1;
        ts.nsec -= 1_000_000_000;
    }
    let r = futex_pi(13, &ts);
    if r == -(ENOSYS as isize) || r == -(EINVAL as isize) {
        skip("lock_pi2_timeout", "no FUTEX_LOCK_PI2");
    } else {
        check_err!(r, ETIMEDOUT, "lock_pi2_timeout");
    }
    PI_RELEASE.store(1, SeqCst);
    holder.join();
    check_eq!(PI_WORD.load(SeqCst), 0, "pi_holder_unlocked");
}

static PI_RELEASE: AtomicI32 = AtomicI32::new(0);

extern "C" fn pi_holder(_: usize) -> i32 {
    futex_pi(6, core::ptr::null());
    while PI_RELEASE.load(SeqCst) == 0 {
        sleep_ms(2);
    }
    futex_pi(7, core::ptr::null());
    0
}

fn wait_until_ms(f: impl Fn() -> bool, ms: i64) -> bool {
    let end = now_ns() + ms * 1_000_000;
    while now_ns() < end {
        if f() {
            return true;
        }
        sleep_ms(2);
    }
    f()
}

fn access_tests() {
    let dir = scratch_dir();
    let f = cpath!("{}/acc", Bytes(dir.as_bytes()));
    let fd = open(f.as_ptr(), O_CREAT | O_RDWR, 0o600) as i32;
    close(fd);
    check_ok!(unsafe { syscall!(21, f.as_ptr(), 6) }, "access_owner_rw");
    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    // A read-only mount: W_OK is EROFS, R_OK fine.
    let m = cpath!("{}/romnt", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, m.as_ptr(), 0o755);
    if mount(c"none".as_ptr() as *const u8, m.as_ptr(), c"tmpfs".as_ptr() as *const u8, 1, core::ptr::null()) < 0 {
        skip("access_rofs", "cannot mount tmpfs");
    } else {
        check_err!(unsafe { syscall!(21, m.as_ptr(), 2) }, EROFS, "access_w_ok_readonly_mount_erofs");
        check_ok!(unsafe { syscall!(21, m.as_ptr(), 4) }, "access_r_ok_readonly_mount");
        umount2(m.as_ptr(), 0);
    }
}

fn pfkey_tun_hvc_tests() {
    let k = unsafe { syscall!(41, 15, 3 | O_CLOEXEC as usize, 2) }; // AF_KEY, SOCK_RAW, PF_KEY_V2
    if k == -(EPERM as isize) || k == -(EACCES as isize) {
        skip("af_key", "not permitted");
    } else {
        check!(k >= 0, "af_key_socket", "returned {}", errname(k));
        check_eq!(close(k as i32), 0, "af_key_close");
    }
    let t = open(c"/dev/net/tun".as_ptr() as *const u8, O_RDWR | O_NONBLOCK | O_CLOEXEC, 0);
    if t < 0 {
        skip("tun", "no /dev/net/tun");
    } else {
        let mut ifr = [0u8; 40];
        ifr[..8].copy_from_slice(b"ktun%d\0\0");
        ifr[16..18].copy_from_slice(&(0x0001u16 | 0x1000).to_le_bytes()); // IFF_TUN | IFF_NO_PI
        let r = ioctl(t as i32, 0x4004_54ca, ifr.as_mut_ptr() as usize);
        if r == -(EPERM as isize) {
            skip("tun", "TUNSETIFF needs CAP_NET_ADMIN");
        } else {
            check_eq!(r, 0, "tunsetiff");
            check!(ifr.starts_with(b"ktun"), "tunsetiff_name_filled");
            let n = ifr.iter().position(|&b| b == 0).unwrap_or(16);
            let sysp = cpath!("/sys/class/net/{}", Bytes(&ifr[..n]));
            let mut stb = Stat::default();
            check_eq!(fstatat(AT_FDCWD, sysp.as_ptr(), &mut stb, 0), 0, "tun_link_in_sysfs");
            let mut b = [0u8; 64];
            check_err!(read(t as i32, &mut b), EAGAIN, "tun_read_empty_eagain");
            close(t as i32);
            check!(fstatat(AT_FDCWD, sysp.as_ptr(), &mut stb, 0) < 0, "tun_link_gone_after_close");
        }
    }
    let h = open(c"/dev/hvc1".as_ptr() as *const u8, O_RDWR | O_NONBLOCK | O_CLOEXEC, 0);
    if h < 0 {
        skip("hvc", "no /dev/hvc1");
    } else {
        let mut tio = [0u8; 60];
        check_ok!(ioctl(h as i32, 0x5401, tio.as_mut_ptr() as usize), "hvc_tcgets");
        tio[12..16].fill(0); // c_lflag = 0: raw
        check_ok!(ioctl(h as i32, 0x5402, tio.as_ptr() as usize), "hvc_tcsets_raw");
        check_eq!(write(h as i32, b"hello"), 5, "hvc_write");
        let mut b = [0u8; 8];
        check_err!(read(h as i32, &mut b), EAGAIN, "hvc_read_nothing");
        close(h as i32);
    }
}

fn packet_tests() {
    let s = socket(2, 2 | O_CLOEXEC); // AF_INET, SOCK_DGRAM: for the ioctls
    let mut ifr = [0u8; 40];
    ifr[..5].copy_from_slice(b"eth0\0");
    if s < 0 || ioctl(s as i32, 0x8933, ifr.as_mut_ptr() as usize) < 0 {
        skip("af_packet", "no eth0");
        return;
    }
    let ifindex = i32::from_le_bytes(ifr[16..20].try_into().unwrap());
    let mut hw = [0u8; 40];
    hw[..5].copy_from_slice(b"eth0\0");
    ioctl(s as i32, 0x8927, hw.as_mut_ptr() as usize); // SIOCGIFHWADDR
    let mac: [u8; 6] = hw[18..24].try_into().unwrap();
    let mut a4 = [0u8; 40];
    a4[..5].copy_from_slice(b"eth0\0");
    let have_ip = ioctl(s as i32, 0x8915, a4.as_mut_ptr() as usize) == 0; // SIOCGIFADDR
    let ip: [u8; 4] = a4[20..24].try_into().unwrap();
    close(s as i32);
    let p = socket(17, 3 | O_CLOEXEC | O_NONBLOCK); // AF_PACKET, SOCK_RAW
    if p == -(EPERM as isize) || !have_ip {
        skip("af_packet", "needs CAP_NET_RAW / an IPv4 address");
        return;
    }
    let p = check_ok!(p, "af_packet_socket") as i32;
    // sockaddr_ll: family, protocol (ETH_P_ARP, network order), ifindex.
    let mut ll = [0u8; 20];
    ll[0..2].copy_from_slice(&17u16.to_le_bytes());
    ll[2..4].copy_from_slice(&0x0806u16.to_be_bytes());
    ll[4..8].copy_from_slice(&ifindex.to_le_bytes());
    check_ok!(unsafe { syscall!(49, p, ll.as_ptr(), 20) }, "af_packet_bind_arp");
    // Accept ARP replies only: opcode (offset 6 of the ARP header) == 2.
    let prog: [(u16, u8, u8, u32); 4] = [
        (0x28, 0, 0, (-0x10_0000i32 + 6) as u32), // ld h [SKF_NET_OFF + 6]
        (0x15, 0, 1, 2),                         // jeq #2
        (0x06, 0, 0, 0xffff_ffff),               // ret accept
        (0x06, 0, 0, 0),                         // ret 0
    ];
    let mut code = [0u8; 32];
    for (i, (c, jt, jf, k)) in prog.iter().enumerate() {
        code[i * 8..i * 8 + 2].copy_from_slice(&c.to_le_bytes());
        code[i * 8 + 2] = *jt;
        code[i * 8 + 3] = *jf;
        code[i * 8 + 4..i * 8 + 8].copy_from_slice(&k.to_le_bytes());
    }
    let mut fprog = [0u8; 16];
    fprog[0..2].copy_from_slice(&4u16.to_le_bytes());
    fprog[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    check_ok!(setsockopt(p, 1, 26, fprog.as_ptr(), 16), "so_attach_filter");
    // Who has 10.0.2.2 (the gateway on QEMU's network and our oracle's
    // usual default)? A raw frame with our address.
    let gw = [ip[0], ip[1], ip[2], 2];
    let mut f = [0u8; 42];
    f[0..6].copy_from_slice(&[0xff; 6]);
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    f[14..22].copy_from_slice(&[0, 1, 8, 0, 6, 4, 0, 1]);
    f[22..28].copy_from_slice(&mac);
    f[28..32].copy_from_slice(&ip);
    f[38..42].copy_from_slice(&gw);
    check_eq!(unsafe { syscall!(44, p, f.as_ptr(), 42, 0, ll.as_ptr(), 20) }, 42, "af_packet_sendto_frame");
    let mut pfd = [Pollfd { fd: p, events: POLLIN, revents: 0 }];
    check_eq!(poll(&mut pfd, 3000), 1, "af_packet_reply_pollin");
    let mut b = [0u8; 128];
    let mut from = [0u8; 20];
    let mut flen: u32 = 20;
    let n = unsafe { syscall!(45, p, b.as_mut_ptr(), 128, 0, from.as_mut_ptr(), &mut flen as *mut u32) };
    check!(n >= 42 && b[12..14] == [8, 6] && b[20..22] == [0, 2] && b[28..32] == gw, "af_packet_arp_reply", "n {} op {:?} spa {:?}", n, &b[20..22], &b[28..32]);
    check!(u16::from_le_bytes([from[0], from[1]]) == 17 && i32::from_le_bytes(from[4..8].try_into().unwrap()) == ifindex, "af_packet_recvfrom_sockaddr_ll");
    check_err!(read(p, &mut b), EAGAIN, "af_packet_queue_empty");
    close(p);
}

fn proc_tests() {
    // /proc/config.gz: gzip magic.
    let mut b = [0u8; 4];
    let fd = open(c"/proc/config.gz".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0);
    if fd < 0 {
        skip("config_gz", "no /proc/config.gz");
    } else {
        check!(read(fd as i32, &mut b) == 4 && b[0] == 0x1f && b[1] == 0x8b, "proc_config_gz_magic");
        close(fd as i32);
    }
    // argv[0] rewritten in place shows in /proc/self/cmdline.
    if let Some(a0) = argv_raw(0) {
        let first = unsafe { *a0 };
        unsafe { *(a0 as *mut u8) = b'Z' };
        let mut c = [0u8; 256];
        let n = read_file(c"/proc/self/cmdline".as_ptr() as *const u8, &mut c);
        check!(n > 0 && c[0] == b'Z', "cmdline_follows_argv_memory", "got {:?}", &c[..(n.max(0) as usize).min(16)]);
        unsafe { *(a0 as *mut u8) = first };
    }
    // CPU time: a busy second shows up in utime + stime (ticks).
    let end = now_ns() + 300_000_000;
    let mut x = 0u64;
    while now_ns() < end {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    let mut st = [0u8; 1024];
    let n = read_file(c"/proc/self/stat".as_ptr() as *const u8, &mut st);
    let after = st[..n.max(0) as usize].iter().rposition(|&c| c == b')').map_or(&st[..0], |i| &st[i + 2..n as usize]);
    let mut f = after.split(|&c| c == b' ');
    let ut = f.clone().nth(14 - 3).and_then(parse_int).unwrap_or(-1);
    let stime = f.nth(15 - 3).and_then(parse_int).unwrap_or(-1);
    check!(ut + stime >= 10, "stat_utime_stime", "utime {} stime {} ({})", ut, stime, x & 1);
    // wchan exists for a thread.
    let w = cpath!("/proc/self/task/{}/wchan", gettid());
    let mut wb = [0u8; 64];
    check!(read_file(w.as_ptr(), &mut wb) >= 1, "task_wchan_readable");
}

fn main() {
    packet_tests();
    proc_tests();
    access_tests();
    pfkey_tun_hvc_tests();
    pi_futex_tests();
    pidfd_tests();
    membarrier_tests();
    stack_and_vma_tests();
    ashmem_tests();
    vsock_tests();
}
