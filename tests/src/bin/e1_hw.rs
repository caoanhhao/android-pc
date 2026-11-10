//! E1: real-hardware-class controllers, against QEMU's emulated ones
//! (run-qemu.sh --hw-test): the C2 test disk on an NVMe controller
//! (nvme0n1) and on an AHCI SATA port (sda), a USB keyboard and mouse on
//! an xHCI controller; the same disk as a USB mass storage device (sdb)
//! and the keyboard and mouse behind a USB hub.
//!
//! Disks: nodes and device numbers (NVMe on blkext 259:n, SATA 8:16n+p),
//! sysfs paths below the controller's PCI function, GPT partitions on
//! both, /proc/partitions, identical contents on the three disks (every
//! byte), writes through the cache and around the 128 KiB (SATA) and
//! 120 KiB (USB) transfer limits on each scratch partition (after
//! BLKFLSBUF the data comes back from the device), and file systems
//! mounted from the SATA and USB disks (c2_fs covers NVMe through
//! /dev/block/by-name). The USB disk (8:16) sits below the xHCI function
//! as on Linux (`usb1/1-1/1-1:1.0/host1/target1:0:0/1:0:0:0/block/sdb`).
//!
//! HID: the evdev nodes of "QEMU QEMU USB Keyboard"/"... Mouse" (bus USB,
//! QEMU's ids, key and relative-axis bits), then the test prints
//! "E1-HID-READY" and waits for what the host sends through the QEMU
//! monitor (tools/hw-test.py: `sendkey a`, `mouse_move 10 -5`,
//! `mouse_button 1`, `mouse_button 0`): KEY_A down and up, the motion,
//! BTN_LEFT down and up, each packet ending in SYN_REPORT. Then
//! "E1-HID-STRESS": the host floods 3000 mouse moves and 150 key presses
//! (rings wrap many times over) and a right click; all must arrive.
//!
//! Without these devices (plain --ktest, the host) the cases are skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("e1_hw", main);

const BLKFLSBUF: u32 = 0x1261;
const BLKGETSIZE64: u32 = 0x8008_1272;
const MS_RDONLY: usize = 1;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const KEY_A: u16 = 30;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
/// The host's flood (tools/hw-test.py): this many `mouse_move 1 0`, one
/// `sendkey a` per STRESS_KEY_EVERY moves, then a right click.
const STRESS_MOVES: i32 = 3000;
const STRESS_KEY_EVERY: i32 = 20;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;

const CHUNK: usize = 1 << 20;

fn makedev(major: u64, minor: u64) -> u64 {
    (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}

fn size64(fd: i32) -> u64 {
    let mut v = 0u64;
    ioctl(fd, BLKGETSIZE64, &mut v as *mut u64 as usize);
    v
}

fn slurp(path: &Path, buf: &mut [u8]) -> usize {
    let n = read_file(path.as_ptr(), buf);
    n.max(0) as usize
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn rdev_of(path: &Path) -> Option<u64> {
    let mut st = Stat::default();
    (fstatat(AT_FDCWD, path.as_ptr(), &mut st, 0) == 0 && st.mode & S_IFMT == 0o060000).then_some(st.rdev)
}

fn disks() {
    let nvme = cpath!("/dev/block/nvme0n1");
    let Some(nv_rdev) = rdev_of(&nvme) else {
        skip("disks", "no NVMe disk (run-qemu.sh --hw-test only)");
        return;
    };
    check!(nv_rdev >> 8 & 0xfff == 259, "nvme_blkext_major", "rdev {:#x}", nv_rdev);
    let sda = cpath!("/dev/block/sda");
    check_eq!(rdev_of(&sda), Some(makedev(8, 0)), "sata_node");
    check_eq!(rdev_of(&cpath!("/dev/block/sda3")), Some(makedev(8, 3)), "sata_partition_node");
    let usb = cpath!("/dev/block/sdb");
    check_eq!(rdev_of(&usb), Some(makedev(8, 16)), "usb_node");
    check_eq!(rdev_of(&cpath!("/dev/block/sdb6")), Some(makedev(8, 22)), "usb_partition_node");
    let p1 = rdev_of(&cpath!("/dev/block/nvme0n1p1"));
    check!(p1.is_some_and(|r| r >> 8 & 0xfff == 259 && r != nv_rdev), "nvme_partition_node", "{:?}", p1);

    let mut buf = [0u8; 512];
    let n = readlink(cpath!("/sys/block/nvme0n1").as_ptr(), &mut buf);
    let l = &buf[..n.max(0) as usize];
    check!(contains(l, b"devices/pci0000:00/0000:00:") && contains(l, b"/nvme/nvme0/"), "nvme_sysfs_path", "{}", Bytes(l));
    let n = readlink(cpath!("/sys/block/sda").as_ptr(), &mut buf);
    let l = &buf[..n.max(0) as usize];
    check!(contains(l, b"/ata1/host") && contains(l, b"block/sda"), "sata_sysfs_path", "{}", Bytes(l));
    let n = readlink(cpath!("/sys/block/sdb").as_ptr(), &mut buf);
    let l = &buf[..n.max(0) as usize];
    check!(contains(l, b"/0000:00:06.0/usb1/1-1/1-1:1.0/host1/target1:0:0/1:0:0:0/block/sdb"), "usb_sysfs_path", "{}", Bytes(l));
    let n = slurp(&cpath!("/sys/class/block/sdb3/uevent"), &mut buf);
    check!(contains(&buf[..n], b"PARTNAME=c2_erofs") && contains(&buf[..n], b"DEVNAME=sdb3"), "usb_partition_uevent", "{}", Bytes(&buf[..n]));
    let n = slurp(&cpath!("/sys/class/block/nvme0n1p3/uevent"), &mut buf);
    check!(contains(&buf[..n], b"PARTNAME=c2_erofs") && contains(&buf[..n], b"DEVNAME=nvme0n1p3"), "nvme_partition_uevent", "{}", Bytes(&buf[..n]));
    let n = slurp(&cpath!("/sys/class/block/sda6/uevent"), &mut buf);
    check!(contains(&buf[..n], b"PARTNAME=c2_scratch") && contains(&buf[..n], b"PARTN=6"), "sata_partition_uevent", "{}", Bytes(&buf[..n]));
    let mut pp = [0u8; 4096];
    let n = slurp(&cpath!("/proc/partitions"), &mut pp);
    check!(contains(&pp[..n], b" nvme0n1p6\n") && contains(&pp[..n], b" sda6\n"), "proc_partitions");

    // Same image on all three: compare every byte.
    let a = open(nvme.as_ptr(), O_RDONLY, 0) as i32;
    let b = open(sda.as_ptr(), O_RDONLY, 0) as i32;
    let c = open(usb.as_ptr(), O_RDONLY, 0) as i32;
    let size = size64(a);
    check!(size > 0 && size == size64(b) && size == size64(c), "same_size", "{} {} {}", size, size64(b), size64(c));
    let ba = alloc_pages(CHUNK);
    let bb = alloc_pages(CHUNK);
    let (ca, cb) = unsafe { (core::slice::from_raw_parts_mut(ba, CHUNK), core::slice::from_raw_parts_mut(bb, CHUNK)) };
    let mut off = 0u64;
    let mut same = true;
    while off < size && same {
        let n = (size - off).min(CHUNK as u64) as usize;
        let (ra, rb) = (pread(a, &mut ca[..n], off as i64), pread(b, &mut cb[..n], off as i64));
        same = ra == n as isize && rb == n as isize && ca[..n] == cb[..n];
        let rc = pread(c, &mut cb[..n], off as i64);
        same &= rc == n as isize && ca[..n] == cb[..n];
        if !same {
            println!("# differ in the MiB at {off}: reads {ra} {rb} {rc}");
        }
        off += n as u64;
    }
    check!(same, "contents_identical");
    // GPT header ("EFI PART" at LBA 1).
    pread(a, &mut buf, 512);
    check!(&buf[..8] == b"EFI PART", "gpt_header");
    close(a);
    close(b);
    close(c);

    // Writes on each scratch partition: 300 KiB at an odd sector (three
    // transfers), read back after dropping the caches.
    for (dev, what) in [("nvme0n1p6", "nvme_write"), ("sda6", "sata_write"), ("sdb6", "usb_write")] {
        let p = cpath!("/dev/block/{}", dev);
        let fd = open(p.as_ptr(), O_RDWR, 0) as i32;
        let len = 300 * 1024;
        for (i, x) in ca[..len].iter_mut().enumerate() {
            *x = (i * 7 + i / 512 + dev.len()) as u8;
        }
        let w = pwrite(fd, &ca[..len], 3 * 512);
        let f = ioctl(fd, BLKFLSBUF, 0);
        cb[..len].fill(0);
        let r = pread(fd, &mut cb[..len], 3 * 512);
        check!(w == len as isize && f == 0 && r == len as isize && ca[..len] == cb[..len], what, "w {} flush {} r {}", w, f, r);
        // An unaligned read-modify-write in the middle.
        let w2 = pwrite(fd, b"e1 hw test", 4097);
        ioctl(fd, BLKFLSBUF, 0);
        let mut back = [0u8; 10];
        let r2 = pread(fd, &mut back, 4097);
        check!(w2 == 10 && r2 == 10 && &back == b"e1 hw test", name(&case(what, "_unaligned")), "{} {}", w2, r2);
        close(fd);
    }

    // File systems from the SATA disk.
    for (part, fstype) in [("sda1", c"ext4"), ("sda2", c"ext4"), ("sda3", c"erofs"), ("sda4", c"erofs"), ("sdb1", c"ext4"), ("sdb3", c"erofs")] {
        let mnt = cpath!("/tmp/e1-{}", part);
        mkdirat(AT_FDCWD, mnt.as_ptr(), 0o755);
        let src = cpath!("/dev/block/{}", part);
        let r = mount(src.as_ptr(), mnt.as_ptr(), fstype.as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
        let mut b = [0u8; 16];
        let n = read_file(cpath!("{}/small.txt", Bytes(mnt.as_bytes())).as_ptr(), &mut b);
        let what = if part.starts_with("sdb") { "usb_mount_" } else { "sata_mount_" };
        check!(r == 0 && n == 9 && &b[..9] == b"hello c2\n", name(&case(what, part)), "mount {} read {}", errname(r), n);
        umount2(mnt.as_ptr(), 0);
    }
}

/// "<a><b>" as a case name.
fn case(a: &str, b: &str) -> Buf<64> {
    let mut c = Buf::<64>::new();
    c.push(a.as_bytes());
    c.push(b.as_bytes());
    c
}

fn name(b: &Buf<64>) -> &str {
    core::str::from_utf8(b.as_bytes()).unwrap()
}

fn eioc_read(nr: u32, size: usize) -> u32 {
    2 << 30 | (size as u32) << 16 | (b'E' as u32) << 8 | nr
}

fn bit(bits: &[u8], n: u16) -> bool {
    bits.get(n as usize / 8).is_some_and(|b| b & (1 << (n % 8)) != 0)
}

/// The event node of the input device named `name`.
fn find_input(name: &[u8]) -> Option<i32> {
    for i in 0..32 {
        let p = cpath!("/dev/input/event{}", i);
        let fd = open(p.as_ptr(), O_RDONLY | O_NONBLOCK | O_CLOEXEC, 0);
        if fd < 0 {
            continue;
        }
        let fd = fd as i32;
        let mut n = [0u8; 64];
        ioctl(fd, eioc_read(0x06, 64), n.as_mut_ptr() as usize);
        if cfield(&n) == name {
            return Some(fd);
        }
        close(fd);
    }
    None
}

/// Read (type, code, value) events from `fds` until `done` says so or
/// `ms` pass.
fn collect(fds: &[i32], ms: i64, mut done: impl FnMut(u16, u16, i32) -> bool) -> bool {
    let end = now_ns() + ms * 1_000_000;
    loop {
        let left = (end - now_ns()) / 1_000_000;
        if left <= 0 {
            return false;
        }
        let mut p = [Pollfd { fd: fds[0], events: POLLIN, revents: 0 }, Pollfd { fd: fds[1], events: POLLIN, revents: 0 }];
        poll(&mut p, left as i32);
        for fd in fds {
            let mut e = [0u8; 24 * 16];
            let r = read(*fd, &mut e);
            if r <= 0 {
                continue;
            }
            for ev in e[..r as usize].chunks(24) {
                let ty = u16::from_le_bytes([ev[16], ev[17]]);
                let code = u16::from_le_bytes([ev[18], ev[19]]);
                let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
                println!("# event fd {} type {} code {} value {}", fd, ty, code, value);
                if done(ty, code, value) {
                    return true;
                }
            }
        }
    }
}

fn hid() {
    let (Some(kbd), Some(mouse)) = (find_input(b"QEMU QEMU USB Keyboard"), find_input(b"QEMU QEMU USB Mouse")) else {
        skip("hid", "no USB keyboard and mouse (run-qemu.sh --hw-test only)");
        return;
    };
    let mut id = [0u16; 4];
    ioctl(kbd, eioc_read(0x02, 8), id.as_mut_ptr() as usize);
    check!(id[0] == 3 && id[1] == 0x0627, "keyboard_id", "{:x?}", id);
    let mut keys = [0u8; 96];
    ioctl(kbd, eioc_read(0x21, 96), keys.as_mut_ptr() as usize);
    check!(bit(&keys, KEY_A) && bit(&keys, 28) && bit(&keys, 42), "keyboard_keys");
    let mut mkeys = [0u8; 96];
    ioctl(mouse, eioc_read(0x21, 96), mkeys.as_mut_ptr() as usize);
    let mut rel = [0u8; 2];
    ioctl(mouse, eioc_read(0x22, 2), rel.as_mut_ptr() as usize);
    check!(bit(&mkeys, BTN_LEFT) && bit(&rel, REL_X) && bit(&rel, REL_Y), "mouse_bits");
    let mut phys = [0u8; 64];
    ioctl(kbd, eioc_read(0x07, 64), phys.as_mut_ptr() as usize);
    // Behind the hub on root port 1 of usb2... as QEMU numbers them.
    check!(cfield(&phys).starts_with(b"usb-0000:00:06.0-") && contains(cfield(&phys), b".1/input0"), "keyboard_phys_behind_hub", "{}", Bytes(cfield(&phys)));
    let mut mphys = [0u8; 64];
    ioctl(mouse, eioc_read(0x07, 64), mphys.as_mut_ptr() as usize);
    check!(cfield(&mphys).starts_with(b"usb-0000:00:06.0-") && contains(cfield(&mphys), b".3/input0"), "mouse_phys_behind_hub", "{}", Bytes(cfield(&mphys)));

    println!("E1-HID-READY");
    let fds = [kbd, mouse];
    let (mut down, mut up, mut syn) = (false, false, false);
    let ok = collect(&fds, 30_000, |ty, code, v| {
        match (ty, code, v) {
            (EV_KEY, KEY_A, 1) => down = true,
            (EV_KEY, KEY_A, 0) if down => up = true,
            (EV_SYN, 0, _) if up => syn = true,
            _ => {}
        }
        syn
    });
    check!(ok && down && up, "key_a_press_release", "down {} up {}", down, up);
    let (mut dx, mut dy, mut bdown, mut bup) = (0, 0, false, false);
    let ok = collect(&fds, 30_000, |ty, code, v| {
        match (ty, code) {
            (EV_REL, REL_X) => dx += v,
            (EV_REL, REL_Y) => dy += v,
            (EV_KEY, BTN_LEFT) if v == 1 => bdown = true,
            (EV_KEY, BTN_LEFT) if v == 0 && bdown => bup = true,
            _ => {}
        }
        bup && ty == EV_SYN
    });
    check!(ok && dx == 10 && dy == -5, "mouse_motion", "dx {} dy {}", dx, dy);
    check!(ok && bdown && bup, "mouse_button", "down {} up {}", bdown, bup);
    stress(&fds);
    close(kbd);
    close(mouse);
}

/// Thousands of reports, far more than the 256-entry transfer and event
/// rings hold, so they wrap many times: every motion and key press must
/// arrive, and the right click the host sends last too.
fn stress(fds: &[i32]) {
    println!("E1-HID-STRESS");
    let (mut dx, mut keys, mut reports, mut right) = (0i32, 0i32, 0u32, false);
    let ok = collect_quiet(fds, 120_000, |ty, code, v| {
        match (ty, code) {
            (EV_REL, REL_X) => dx += v,
            (EV_KEY, KEY_A) if v == 1 => keys += 1,
            (EV_SYN, _) => reports += 1,
            (EV_KEY, BTN_RIGHT) if v == 1 => right = true,
            _ => {}
        }
        right && ty == EV_SYN
    });
    println!("# stress: dx {} keys {} reports {} end marker {}", dx, keys, reports, right);
    check!(ok && right, "stress_end_marker_arrives", "dx {} keys {} reports {}", dx, keys, reports);
    check!(dx == STRESS_MOVES, "stress_all_motion", "dx {} of {}", dx, STRESS_MOVES);
    check!(keys == STRESS_MOVES / STRESS_KEY_EVERY, "stress_all_keys", "{} of {}", keys, STRESS_MOVES / STRESS_KEY_EVERY);
    check!(reports > 600, "stress_rings_wrap", "{} reports", reports);
}

/// [`collect`] without a line per event.
fn collect_quiet(fds: &[i32], ms: i64, mut done: impl FnMut(u16, u16, i32) -> bool) -> bool {
    let end = now_ns() + ms * 1_000_000;
    loop {
        let left = (end - now_ns()) / 1_000_000;
        if left <= 0 {
            return false;
        }
        let mut p = [Pollfd { fd: fds[0], events: POLLIN, revents: 0 }, Pollfd { fd: fds[1], events: POLLIN, revents: 0 }];
        poll(&mut p, left as i32);
        for fd in fds {
            let mut e = [0u8; 24 * 64];
            let r = read(*fd, &mut e);
            if r <= 0 {
                continue;
            }
            for ev in e[..r as usize].chunks(24) {
                let ty = u16::from_le_bytes([ev[16], ev[17]]);
                let code = u16::from_le_bytes([ev[18], ev[19]]);
                let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
                if done(ty, code, value) {
                    return true;
                }
            }
        }
    }
}

fn main() {
    if !is_rust_kernel() {
        skip("all", "Rust kernel with run-qemu.sh --hw-test only");
        return;
    }
    disks();
    hid();
}
