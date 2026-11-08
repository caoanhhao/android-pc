//! C3: inotify. inotify_init1 flags, add_watch on a directory (IN_CREATE,
//! IN_MODIFY, IN_CLOSE_WRITE, IN_ATTRIB, IN_MOVED_FROM/TO with one cookie,
//! IN_DELETE, IN_ISDIR), watches on a file (IN_DELETE_SELF + IN_IGNORED,
//! IN_ONESHOT), the same wd for a second add_watch (IN_MASK_ADD), rm_watch
//! (IN_IGNORED), IN_ONLYDIR, EAGAIN/EINVAL reads, FIONREAD, poll and a
//! blocking read woken by another process.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_inotify", main);

const INOTIFY_INIT1: usize = 294;
const INOTIFY_ADD_WATCH: usize = 254;
const INOTIFY_RM_WATCH: usize = 255;
const IN_MODIFY: u32 = 0x2;
const IN_ATTRIB: u32 = 0x4;
const IN_CLOSE_WRITE: u32 = 0x8;
const IN_MOVED_FROM: u32 = 0x40;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const IN_DELETE_SELF: u32 = 0x400;
const IN_IGNORED: u32 = 0x8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
const FIONREAD: u32 = 0x541b;
const FCHMODAT: usize = 268;
const RENAMEAT: usize = 264;

fn add_watch(fd: i32, path: &Path, mask: u32) -> isize {
    unsafe { syscall!(INOTIFY_ADD_WATCH, fd, path.as_ptr(), mask) }
}

#[derive(Clone, Copy)]
struct Ev {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: [u8; 32],
}

impl Ev {
    fn name(&self) -> &[u8] {
        cfield(&self.name)
    }
}

/// Read every queued event (non-blocking fd).
fn events(fd: i32) -> ([Ev; 16], usize) {
    let mut out = [Ev { wd: 0, mask: 0, cookie: 0, name: [0; 32] }; 16];
    let mut n = 0;
    let mut buf = [0u8; 4096];
    loop {
        let r = read(fd, &mut buf);
        if r <= 0 {
            break;
        }
        let mut off = 0;
        while off + 16 <= r as usize && n < out.len() {
            let w = |i: usize| u32::from_le_bytes(buf[off + i..off + i + 4].try_into().unwrap());
            let len = w(12) as usize;
            let mut e = Ev { wd: w(0) as i32, mask: w(4), cookie: w(8), name: [0; 32] };
            let nm = cfield(&buf[off + 16..off + 16 + len]);
            e.name[..nm.len().min(31)].copy_from_slice(&nm[..nm.len().min(31)]);
            out[n] = e;
            n += 1;
            off += 16 + len;
        }
    }
    (out, n)
}

fn has(evs: &[Ev], wd: i32, mask: u32, name: &[u8]) -> bool {
    evs.iter().any(|e| e.wd == wd && e.mask == mask && e.name() == name)
}

fn main() {
    let dir = scratch_dir();
    let ifd = unsafe { syscall!(INOTIFY_INIT1, O_NONBLOCK | O_CLOEXEC) } as i32;
    if ifd == -(ENOSYS as i32) {
        skip("all", "no inotify");
        return;
    }
    check!(ifd >= 0, "init1", "{}", errname(ifd as isize));
    check!(fcntl(ifd, F_GETFD, 0) == 1, "init1_cloexec");
    check_err!(unsafe { syscall!(INOTIFY_INIT1, 0x1234) }, EINVAL, "init1_bad_flags");
    let mut b16 = [0u8; 16];
    check_err!(read(ifd, &mut b16), EAGAIN, "empty_eagain");

    let mask = IN_CREATE | IN_DELETE | IN_MODIFY | IN_CLOSE_WRITE | IN_ATTRIB | IN_MOVED_FROM | IN_MOVED_TO;
    let wd = add_watch(ifd, &dir, mask) as i32;
    check!(wd > 0, "add_watch_dir", "{}", errname(wd as isize));
    check_eq!(add_watch(ifd, &dir, IN_DELETE | IN_MASK_ADD) as i32, wd, "same_wd_mask_add");

    // Create, write, close: CREATE, MODIFY, CLOSE_WRITE with the name.
    let f = cpath!("{}/a", Bytes(dir.as_bytes()));
    let fd = open(f.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32;
    write(fd, b"hello");
    let mut nread = 0i32;
    ioctl(ifd, FIONREAD, &mut nread as *mut i32 as usize);
    check!(nread >= 32, "fionread", "{}", nread);
    let mut pfd = [Pollfd { fd: ifd, events: POLLIN, revents: 0 }];
    check!(poll(&mut pfd, 0) == 1 && pfd[0].revents & POLLIN != 0, "poll_readable");
    check_err!(read(ifd, &mut b16[..8]), EINVAL, "small_buffer_einval");
    close(fd);
    let (ev, n) = events(ifd);
    let ev = &ev[..n];
    check!(has(ev, wd, IN_CREATE, b"a"), "ev_create");
    check!(has(ev, wd, IN_MODIFY, b"a"), "ev_modify");
    check!(has(ev, wd, IN_CLOSE_WRITE, b"a"), "ev_close_write");

    // chmod: ATTRIB.
    unsafe { syscall!(FCHMODAT, AT_FDCWD, f.as_ptr(), 0o600, 0) };
    let (ev, n) = events(ifd);
    check!(has(&ev[..n], wd, IN_ATTRIB, b"a"), "ev_attrib");

    // rename inside the directory: MOVED_FROM + MOVED_TO, same cookie.
    let g = cpath!("{}/b", Bytes(dir.as_bytes()));
    unsafe { syscall!(RENAMEAT, AT_FDCWD, f.as_ptr(), AT_FDCWD, g.as_ptr()) };
    let (ev, n) = events(ifd);
    let ev = &ev[..n];
    let from = ev.iter().find(|e| e.mask == IN_MOVED_FROM && e.name() == b"a");
    let to = ev.iter().find(|e| e.mask == IN_MOVED_TO && e.name() == b"b");
    check!(from.is_some() && to.is_some(), "ev_moved_from_to");
    check!(from.zip(to).is_some_and(|(a, b)| a.cookie == b.cookie && a.cookie != 0), "ev_move_cookie");

    // mkdir: CREATE|ISDIR.
    let sub = cpath!("{}/sub", Bytes(dir.as_bytes()));
    mkdirat(AT_FDCWD, sub.as_ptr(), 0o755);
    let (ev, n) = events(ifd);
    check!(has(&ev[..n], wd, IN_CREATE | IN_ISDIR, b"sub"), "ev_mkdir_isdir");

    // A watch on the file itself: unlink gives DELETE (dir) and
    // DELETE_SELF then IGNORED (file watch).
    let fw = add_watch(ifd, &g, IN_DELETE_SELF) as i32;
    check!(fw > 0 && fw != wd, "add_watch_file");
    unlinkat(AT_FDCWD, g.as_ptr(), 0);
    let (ev, n) = events(ifd);
    let ev = &ev[..n];
    check!(has(ev, wd, IN_DELETE, b"b"), "ev_delete");
    check!(has(ev, fw, IN_DELETE_SELF, b""), "ev_delete_self");
    check!(has(ev, fw, IN_IGNORED, b""), "ev_ignored_after_delete");

    // IN_ONESHOT: one event, then the watch is gone.
    let h = cpath!("{}/c", Bytes(dir.as_bytes()));
    let fd = open(h.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32;
    close(fd);
    events(ifd);
    let ow = add_watch(ifd, &h, IN_ATTRIB | IN_ONESHOT) as i32;
    unsafe { syscall!(FCHMODAT, AT_FDCWD, h.as_ptr(), 0o600, 0) };
    unsafe { syscall!(FCHMODAT, AT_FDCWD, h.as_ptr(), 0o644, 0) };
    let (ev, n) = events(ifd);
    let ev = &ev[..n];
    check_eq!(ev.iter().filter(|e| e.wd == ow && e.mask == IN_ATTRIB).count(), 1, "oneshot_once");
    check!(has(ev, ow, IN_IGNORED, b""), "oneshot_ignored");

    // rm_watch: IGNORED, then EINVAL for the stale wd.
    check_eq!(unsafe { syscall!(INOTIFY_RM_WATCH, ifd, wd) }, 0, "rm_watch");
    let (ev, n) = events(ifd);
    check!(has(&ev[..n], wd, IN_IGNORED, b""), "rm_watch_ignored");
    check_err!(unsafe { syscall!(INOTIFY_RM_WATCH, ifd, wd) }, EINVAL, "rm_watch_stale");
    check_err!(add_watch(ifd, &h, IN_ATTRIB | IN_ONLYDIR), ENOTDIR, "onlydir_enotdir");
    check_err!(add_watch(ifd, &h, 0), EINVAL, "zero_mask_einval");

    // Blocking read woken by a child creating a file.
    let bfd = unsafe { syscall!(INOTIFY_INIT1, O_CLOEXEC) } as i32;
    add_watch(bfd, &dir, IN_CREATE);
    let pid = fork();
    if pid == 0 {
        sleep_ms(100);
        let p = cpath!("{}/late", Bytes(dir.as_bytes()));
        let fd = open(p.as_ptr(), O_CREAT | O_WRONLY, 0o644);
        close(fd as i32);
        exit(0);
    }
    let mut buf = [0u8; 256];
    let r = read(bfd, &mut buf);
    check!(r >= 16 && cfield(&buf[16..r as usize]) == b"late", "blocking_read_woken", "{}", errname(r));
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);

    close(bfd);
    close(ifd);
    for p in ["c", "late", "sub"] {
        let q = cpath!("{}/{}", Bytes(dir.as_bytes()), p);
        unlinkat(AT_FDCWD, q.as_ptr(), if p == "sub" { AT_REMOVEDIR } else { 0 });
    }
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}
