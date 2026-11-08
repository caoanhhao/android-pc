//! C3: the sysfs device hierarchy ueventd's coldboot walks, checked the
//! same way on Linux and on the Rust kernel: every /sys/class/{mem,tty,
//! misc,block}/X is a symlink into /sys/devices whose directory has a
//! `uevent` file (MAJOR/MINOR/DEVNAME matching its `dev` file), a
//! `subsystem` link back to the class, and a /sys/dev/{char,block}/M:m
//! link to the same place; /sys/block/X are whole disks (DEVTYPE=disk,
//! size, queue/) and their partitions DEVTYPE=partition; /dev/null and
//! friends have the device numbers sysfs gives. On the Rust kernel also
//! the hw_random misc device prng_seeder reads, and PSI triggers on
//! /proc/pressure/memory that lmkd registers with epoll.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c3_sysfs", main);

/// Names in directory `dir` (without . and ..), passed to `f`.
fn each_entry(dir: &[u8], mut f: impl FnMut(&[u8])) -> bool {
    let p = cpath!("{}", Bytes(dir));
    let fd = open(p.as_ptr(), O_RDONLY | O_DIRECTORY | O_CLOEXEC, 0);
    if fd < 0 {
        return false;
    }
    let mut buf = [0u8; 4096];
    loop {
        let n = getdents64(fd as i32, &mut buf);
        if n <= 0 {
            break;
        }
        let mut off = 0;
        while off < n as usize {
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            let name = cfield(&buf[off + 19..off + reclen]);
            if name != b"." && name != b".." {
                f(name);
            }
            off += reclen;
        }
    }
    close(fd as i32);
    true
}

fn read_small(path: &Path) -> ([u8; 512], usize) {
    let mut b = [0u8; 512];
    let n = read_file(path.as_ptr(), &mut b);
    (b, n.max(0) as usize)
}

fn uevent_var<'a>(ev: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    ev.split(|&c| c == b'\n').find_map(|l| l.strip_prefix(key)?.strip_prefix(b"="))
}

/// "MAJOR:MINOR" from a dev file.
fn dev_numbers(dev: &[u8]) -> Option<(i64, i64)> {
    let d = trim(dev);
    let colon = d.iter().position(|&c| c == b':')?;
    Some((parse_int(&d[..colon])?, parse_int(&d[colon + 1..])?))
}

/// Check one class device; returns a short reason on failure.
fn check_class_device(class: &[u8], name: &[u8]) -> Result<(), &'static str> {
    let link = cpath!("/sys/class/{}/{}", Bytes(class), Bytes(name));
    let mut target = [0u8; 512];
    let n = readlink(link.as_ptr(), &mut target);
    if n <= 0 {
        return Err("class entry is not a symlink");
    }
    let target = &target[..n as usize];
    if !target.windows(8).any(|w| w == b"devices/") {
        return Err("class link does not point into /sys/devices");
    }
    let (ev, evn) = read_small(&cpath!("{}/uevent", Bytes(link.as_bytes())));
    let ev = &ev[..evn];
    let (dev, devn) = read_small(&cpath!("{}/dev", Bytes(link.as_bytes())));
    if devn == 0 {
        // Some class devices (e.g. virtual consoles' parents) have no node.
        return Ok(());
    }
    let (maj, min) = dev_numbers(&dev[..devn]).ok_or("bad dev file")?;
    if uevent_var(ev, b"MAJOR").and_then(parse_int) != Some(maj) || uevent_var(ev, b"MINOR").and_then(parse_int) != Some(min) {
        return Err("uevent MAJOR/MINOR differ from dev");
    }
    if uevent_var(ev, b"DEVNAME").is_none() {
        return Err("uevent without DEVNAME");
    }
    let mut sub = [0u8; 512];
    let sn = readlink(cpath!("{}/subsystem", Bytes(link.as_bytes())).as_ptr(), &mut sub);
    let want_sub = cpath!("/{}", Bytes(class));
    if sn <= 0 || !sub[..sn as usize].ends_with(want_sub.as_bytes()) {
        return Err("subsystem link");
    }
    let kind = if class == b"block" { "block" } else { "char" };
    let byno = cpath!("/sys/dev/{}/{}:{}", kind, maj, min);
    let mut t2 = [0u8; 512];
    let n2 = readlink(byno.as_ptr(), &mut t2);
    // Both links end in the same devices/... path.
    let tail = |t: &[u8]| -> usize { t.windows(8).position(|w| w == b"devices/").unwrap_or(0) };
    if n2 <= 0 || t2[tail(&t2[..n2 as usize])..n2 as usize] != target[tail(target)..] {
        return Err("/sys/dev link differs from the class link");
    }
    Ok(())
}

fn main() {
    for class in [&b"mem"[..], b"tty", b"misc", b"block"] {
        let mut count = 0;
        let mut bad: Option<(Path, &'static str)> = None;
        let dir = cpath!("/sys/class/{}", Bytes(class));
        let listed = each_entry(dir.as_bytes(), |name| {
            count += 1;
            if bad.is_none()
                && let Err(e) = check_class_device(class, name)
            {
                bad = Some((cpath!("{}", Bytes(name)), e));
            }
        });
        let case = cpath!("class_{}", Bytes(class));
        let case = core::str::from_utf8(case.as_bytes()).unwrap();
        match bad {
            None => {
                check!(listed && count > 0, case, "{} entries", count);
            }
            Some((n, e)) => {
                check!(false, case, "{}: {}", Bytes(n.as_bytes()), e);
            }
        }
    }

    // /sys/block: disks, with partitions below them.
    let mut disks = 0;
    let mut err: Option<&'static str> = None;
    each_entry(b"/sys/block", |name| {
        disks += 1;
        let d = cpath!("/sys/block/{}", Bytes(name));
        let (ev, n) = read_small(&cpath!("{}/uevent", Bytes(d.as_bytes())));
        if uevent_var(&ev[..n], b"DEVTYPE") != Some(b"disk") {
            err.get_or_insert("disk without DEVTYPE=disk");
        }
        let (sz, n) = read_small(&cpath!("{}/size", Bytes(d.as_bytes())));
        if parse_int(trim(&sz[..n])).is_none() {
            err.get_or_insert("disk without size");
        }
        let (lb, n) = read_small(&cpath!("{}/queue/logical_block_size", Bytes(d.as_bytes())));
        if parse_int(trim(&lb[..n])).is_none_or(|v| v < 512) {
            err.get_or_insert("disk without queue/logical_block_size");
        }
        // Partitions are subdirectories with a `partition` file.
        each_entry(d.as_bytes(), |sub| {
            let part = cpath!("{}/{}/partition", Bytes(d.as_bytes()), Bytes(sub));
            let (_, pn) = read_small(&part);
            if pn > 0 {
                let (ev, n) = read_small(&cpath!("{}/{}/uevent", Bytes(d.as_bytes()), Bytes(sub)));
                if uevent_var(&ev[..n], b"DEVTYPE") != Some(b"partition") || uevent_var(&ev[..n], b"PARTN").is_none() {
                    err.get_or_insert("partition uevent");
                }
            }
        });
    });
    check!(disks > 0 && err.is_none(), "sys_block_disks", "{} disks, {:?}", disks, err);

    // Device nodes carry the numbers sysfs gives.
    for (node, sys) in [(c"/dev/null", c"/sys/class/mem/null/dev"), (c"/dev/zero", c"/sys/class/mem/zero/dev"),
        (c"/dev/urandom", c"/sys/class/mem/urandom/dev"), (c"/dev/tty", c"/sys/class/tty/tty/dev")]
    {
        let mut st = Stat::default();
        fstatat(AT_FDCWD, node.as_ptr() as *const u8, &mut st, 0);
        let mut b = [0u8; 32];
        let n = read_file(sys.as_ptr() as *const u8, &mut b);
        let want = dev_numbers(&b[..n.max(0) as usize]);
        let got = ((st.rdev >> 8) & 0xfff, (st.rdev & 0xff) | ((st.rdev >> 12) & 0xfff00));
        let case = cpath!("devnode_{}", core::str::from_utf8(&node.to_bytes()[5..]).unwrap());
        check!(want == Some((got.0 as i64, got.1 as i64)), core::str::from_utf8(case.as_bytes()).unwrap(),
            "{:?} vs {:?}", want, got);
    }

    if !is_rust_kernel() {
        skip("hw_random", "host hardware differs");
        skip("psi_trigger", "needs the Rust kernel's PSI files");
        return;
    }

    // hw_random: misc 10:183, random bytes.
    let (dev, n) = read_small(&cpath!("/sys/class/misc/hw_random/dev"));
    check_eq!(dev_numbers(&dev[..n]), Some((10, 183)), "hw_random_sysfs");
    let fd = open(c"/dev/hw_random".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0);
    if fd < 0 {
        // ktest's /dev is devtmpfs without the node: make it.
        unsafe { syscall!(nr::MKNODAT, AT_FDCWD, c"/tmp/hw_random".as_ptr(), S_IFCHR | 0o400, (10 << 8) | 183) };
    }
    let fd = if fd < 0 { open(c"/tmp/hw_random".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0) } else { fd };
    let mut r = [0u8; 64];
    check!(fd >= 0 && read(fd as i32, &mut r) == 64 && r.iter().any(|&c| c != 0), "hw_random_read");
    close(fd as i32);
    unlinkat(AT_FDCWD, c"/tmp/hw_random".as_ptr() as *const u8, 0);

    // PSI: lmkd's trigger registration, epoll-able, quiet when idle.
    let psi = open(c"/proc/pressure/memory".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    check!(psi >= 0 && write(psi, b"some 70000 1000000\0") > 0, "psi_trigger");
    let ep = epoll_create1(EPOLL_CLOEXEC) as i32;
    check_ok!(epoll_ctl(ep, EPOLL_CTL_ADD, psi, EPOLLPRI, 7), "psi_epoll_add");
    let mut ev = [EpollEvent::default(); 1];
    check_eq!(epoll_wait(ep, &mut ev, 50), 0, "psi_idle");
    close(ep);
    close(psi);
}
