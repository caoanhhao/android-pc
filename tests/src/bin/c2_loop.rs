//! C2: loop devices. /dev/loop-control (GET_FREE, ADD, REMOVE), binding
//! a tmpfs file with LOOP_CONFIGURE (offset, size limit, read-only),
//! LOOP_GET/SET_STATUS64, sysfs loop/ attributes, EBUSY on a second bind,
//! mounting erofs from a loop device, LOOP_CLR_FD, writes reaching the
//! backing file, LOOP_SET_CAPACITY, and AUTOCLEAR teardown after umount.
//!
//! The erofs image comes from the C2 test disk; without it (or without
//! permission to use loop devices, e.g. on the host) cases are skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c2_loop", main);

const LOOP_SET_FD: u32 = 0x4c00;
const LOOP_CLR_FD: u32 = 0x4c01;
const LOOP_SET_STATUS64: u32 = 0x4c04;
const LOOP_GET_STATUS64: u32 = 0x4c05;
const LOOP_SET_CAPACITY: u32 = 0x4c07;
const LOOP_CONFIGURE: u32 = 0x4c0a;
const LOOP_CTL_ADD: u32 = 0x4c80;
const LOOP_CTL_REMOVE: u32 = 0x4c81;
const LOOP_CTL_GET_FREE: u32 = 0x4c82;
const LO_FLAGS_READ_ONLY: u32 = 1;
const LO_FLAGS_AUTOCLEAR: u32 = 4;
const BLKGETSIZE64: u32 = 0x8008_1272;
const BLKROGET: u32 = 0x125e;
const MS_RDONLY: usize = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct LoopInfo64 {
    device: u64,
    inode: u64,
    rdevice: u64,
    offset: u64,
    sizelimit: u64,
    number: u32,
    encrypt_type: u32,
    encrypt_key_size: u32,
    flags: u32,
    file_name: [u8; 64],
    crypt_name: [u8; 64],
    encrypt_key: [u8; 32],
    init: [u64; 2],
}

impl LoopInfo64 {
    fn zeroed() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

#[repr(C)]
struct LoopConfig {
    fd: u32,
    block_size: u32,
    info: LoopInfo64,
    reserved: [u64; 8],
}

fn size64(fd: i32) -> u64 {
    let mut v = 0u64;
    ioctl(fd, BLKGETSIZE64, &mut v as *mut u64 as usize);
    v
}

fn status(fd: i32, info: &mut LoopInfo64) -> isize {
    ioctl(fd, LOOP_GET_STATUS64, info as *mut LoopInfo64 as usize)
}

fn slurp(path: &Path, buf: &mut [u8]) -> usize {
    let fd = open(path.as_ptr(), O_RDONLY, 0) as i32;
    if fd < 0 {
        return 0;
    }
    let n = read(fd, buf);
    close(fd);
    n.max(0) as usize
}

const HDR: u64 = 8192;

fn main() {
    let ctl = open(c"/dev/loop-control".as_ptr() as *const u8, O_RDWR, 0) as i32;
    if ctl < 0 {
        skip("all", "no usable /dev/loop-control (needs root on Linux)");
        return;
    }
    let src = open(c"/dev/block/by-name/c2_erofs".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    if src < 0 {
        skip("all", "no C2 test disk; Rust kernel with run-qemu.sh --ktest only");
        close(ctl);
        return;
    }
    // Backing file: 8 KiB of junk, then the erofs image.
    let img_size = size64(src);
    let file = cpath!("/tmp/c2loop.img");
    let ffd = open(file.as_ptr(), O_CREAT | O_RDWR | O_TRUNC, 0o644) as i32;
    let mut chunk = [0x5au8; 65536];
    pwrite(ffd, &chunk[..HDR as usize], 0);
    let mut off = 0u64;
    while off < img_size {
        let n = pread(src, &mut chunk, off as i64);
        if n <= 0 {
            break;
        }
        pwrite(ffd, &chunk[..n as usize], (HDR + off) as i64);
        off += n as u64;
    }
    close(src);
    pwrite(ffd, b"trailing", (HDR + img_size) as i64);
    close(ffd);

    let n = ioctl(ctl, LOOP_CTL_GET_FREE, 0);
    check!(n >= 0, "get_free", "{}", errname(n));
    let dev = cpath!("/dev/block/loop{}", n);
    let lfd = open(dev.as_ptr(), O_RDWR, 0) as i32;
    check!(lfd >= 0 && size64(lfd) == 0, "unbound_size_zero");
    let mut info = LoopInfo64::zeroed();
    check_err!(status(lfd, &mut info), ENXIO, "status_unbound_enxio");

    // Bind read-only, at an offset, limited to the image.
    let rofd = open(file.as_ptr(), O_RDONLY, 0) as i32;
    let mut cfg = LoopConfig { fd: rofd as u32, block_size: 4096, info: LoopInfo64::zeroed(), reserved: [0; 8] };
    cfg.info.offset = HDR;
    cfg.info.sizelimit = img_size;
    cfg.info.file_name[..10].copy_from_slice(b"c2loop.img");
    let r = ioctl(lfd, LOOP_CONFIGURE, &cfg as *const LoopConfig as usize);
    let mut ro = 0i32;
    ioctl(lfd, BLKROGET, &mut ro as *mut i32 as usize);
    check!(r == 0 && size64(lfd) == img_size && ro == 1, "configure_offset_sizelimit_ro",
           "configure {} size {} (want {}) ro {}", errname(r), size64(lfd), img_size, ro);
    let s = status(lfd, &mut info);
    check!(s == 0 && info.offset == HDR && info.sizelimit == img_size && info.number == n as u32
           && info.flags & LO_FLAGS_READ_ONLY != 0 && cfield(&info.file_name) == b"c2loop.img",
           "get_status64", "offset {} limit {} number {} flags {:#x}", info.offset, info.sizelimit, info.number, info.flags);
    let mut sb = [0u8; 256];
    let k = slurp(&cpath!("/sys/block/loop{}/loop/offset", n), &mut sb);
    let k2 = slurp(&cpath!("/sys/block/loop{}/size", n), &mut sb[128..]);
    check!(&sb[..k] == b"8192\n" && parse_int(&sb[128..128 + k2 - 1]) == Some((img_size / 512) as i64), "sysfs_loop",
           "offset {} size {}", Bytes(&sb[..k]), Bytes(&sb[128..128 + k2]));
    check_err!(ioctl(lfd, LOOP_SET_FD, rofd as usize), EBUSY, "second_bind_ebusy");
    let mut head = [0u8; 2048];
    pread(lfd, &mut head, 0);
    check!(head[1024..1028] == [0xe2, 0xe1, 0xf5, 0xe0], "offset_applied");

    // Mount the file system from the loop device.
    let mnt = cpath!("/tmp/c2loop-mnt");
    mkdirat(AT_FDCWD, mnt.as_ptr(), 0o755);
    let m = mount(dev.as_ptr(), mnt.as_ptr(), c"erofs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
    let mut txt = [0u8; 32];
    let t = slurp(&cpath!("{}/a/b/c/d/deep.txt", Bytes(mnt.as_bytes())), &mut txt);
    check!(m == 0 && &txt[..t] == b"deep\n", "mount_from_loop", "mount {} read {}", errname(m), t);
    check_eq!(umount2(mnt.as_ptr(), 0), 0, "umount");
    check_eq!(ioctl(lfd, LOOP_CLR_FD, 0), 0, "clr_fd");
    check!(status(lfd, &mut info) == -(ENXIO as isize) && size64(lfd) == 0, "cleared");
    close(rofd);

    // Read-write binding: writes reach the file; SET_STATUS64 moves it.
    let rwfd = open(file.as_ptr(), O_RDWR, 0) as i32;
    let r = ioctl(lfd, LOOP_SET_FD, rwfd as usize);
    let full = size64(lfd);
    let w = pwrite(lfd, b"via-loop", 512);
    let mut back = [0u8; 8];
    pread(rwfd, &mut back, 512);
    check!(r == 0 && full == (HDR + img_size + 8) / 512 * 512 && w == 8 && &back == b"via-loop", "rw_bind_writes_file",
           "set_fd {} size {} write {}", errname(r), full, w);
    let mut st = LoopInfo64::zeroed();
    status(lfd, &mut st);
    st.offset = 4096;
    let r = ioctl(lfd, LOOP_SET_STATUS64, &st as *const LoopInfo64 as usize);
    let mut four = [0u8; 4];
    pread(lfd, &mut four, 0);
    check!(r == 0 && size64(lfd) == (HDR + img_size + 8 - 4096) / 512 * 512 && four == [0x5a; 4], "set_status64_offset");
    // Grow the file, then LOOP_SET_CAPACITY picks the new size up.
    pwrite(rwfd, b"x", (HDR + img_size + 1024 * 1024) as i64);
    let before = size64(lfd);
    let c = ioctl(lfd, LOOP_SET_CAPACITY, 0);
    check!(c == 0 && size64(lfd) > before, "set_capacity", "{} -> {}", before, size64(lfd));
    ioctl(lfd, LOOP_CLR_FD, 0);
    close(rwfd);

    // AUTOCLEAR: CLR_FD while mounted is deferred until umount + close.
    let rofd = open(file.as_ptr(), O_RDONLY, 0) as i32;
    let mut cfg = LoopConfig { fd: rofd as u32, block_size: 0, info: LoopInfo64::zeroed(), reserved: [0; 8] };
    cfg.info.offset = HDR;
    cfg.info.sizelimit = img_size;
    cfg.info.flags = LO_FLAGS_AUTOCLEAR;
    ioctl(lfd, LOOP_CONFIGURE, &cfg as *const LoopConfig as usize);
    close(rofd);
    let m = mount(dev.as_ptr(), mnt.as_ptr(), c"erofs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
    let clr = ioctl(lfd, LOOP_CLR_FD, 0);
    let still = status(lfd, &mut info);
    close(lfd);
    umount2(mnt.as_ptr(), 0);
    let lfd = open(dev.as_ptr(), O_RDONLY, 0) as i32;
    let after = status(lfd, &mut info);
    check!(m == 0 && clr == 0 && still == 0 && after == -(ENXIO as isize), "autoclear_after_umount",
           "mount {} clr {} while mounted {} after {}", errname(m), errname(clr), errname(still), errname(after));
    close(lfd);

    // loop-control ADD / REMOVE.
    let add = ioctl(ctl, LOOP_CTL_ADD, 42);
    let mut ls = Stat::default();
    let exists = fstatat(AT_FDCWD, c"/dev/block/loop42".as_ptr() as *const u8, &mut ls, 0);
    let again = ioctl(ctl, LOOP_CTL_ADD, 42);
    let rm = ioctl(ctl, LOOP_CTL_REMOVE, 42);
    let gone = fstatat(AT_FDCWD, c"/dev/block/loop42".as_ptr() as *const u8, &mut ls, 0);
    check!(add == 42 && exists == 0 && again == -(EEXIST as isize) && rm == 42 && gone == -(ENOENT as isize),
           "ctl_add_remove", "add {} exists {} again {} remove {} gone {}", add, exists, errname(again), rm, errname(gone));
    close(ctl);
    unlinkat(AT_FDCWD, file.as_ptr(), 0);
}
