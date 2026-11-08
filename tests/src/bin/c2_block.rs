//! C2: block devices. The C2 test disk (tools/mkc2-images.py) is vda
//! (modern virtio-pci, writes go to a throw-away overlay) and, when
//! attached, vdb is the same image on a legacy-only virtio device,
//! read-only. Checks: /dev/block nodes and device numbers, GPT partitions
//! (sysfs dev/size/start/partition/uevent PARTNAME, /sys/class/block and
//! /sys/dev/block links, by-name symlinks, /proc/partitions), block
//! ioctls (BLKGETSIZE64, BLKGETSIZE, BLKSSZGET, BLKPBSZGET, BLKBSZGET,
//! BLKROGET/BLKROSET, BLKFLSBUF, BLKDISCARD, BLKZEROOUT), partition
//! offsets and bounds, read/write through the cache at odd offsets and
//! after a flush, SEEK_END, and the legacy transport.
//!
//! On the host there is no such disk: every case is skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c2_block", main);

const BLKROSET: u32 = 0x125d;
const BLKROGET: u32 = 0x125e;
const BLKGETSIZE: u32 = 0x1260;
const BLKFLSBUF: u32 = 0x1261;
const BLKSSZGET: u32 = 0x1268;
const BLKBSZGET: u32 = 0x8008_1270;
const BLKGETSIZE64: u32 = 0x8008_1272;
const BLKDISCARD: u32 = 0x1277;
const BLKPBSZGET: u32 = 0x127b;
const BLKZEROOUT: u32 = 0x127f;

fn makedev(major: u64, minor: u64) -> u64 {
    (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}

fn size64(fd: i32) -> u64 {
    let mut v = 0u64;
    ioctl(fd, BLKGETSIZE64, &mut v as *mut u64 as usize);
    v
}

/// Contents of a small (sysfs/proc) file, newline included.
fn slurp(path: &Path, buf: &mut [u8]) -> usize {
    let fd = open(path.as_ptr(), O_RDONLY, 0) as i32;
    if fd < 0 {
        return 0;
    }
    let mut n = 0;
    loop {
        let r = read(fd, &mut buf[n..]);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    close(fd);
    n
}

fn sys_num(path: &Path) -> i64 {
    let mut b = [0u8; 64];
    let n = slurp(path, &mut b);
    let s = &b[..n];
    parse_int(s.strip_suffix(b"\n").unwrap_or(s)).unwrap_or(-1)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn main() {
    let mut st = Stat::default();
    if fstatat(AT_FDCWD, c"/dev/block/by-name/c2_scratch".as_ptr() as *const u8, &mut st, 0) < 0 {
        skip("all", "no C2 test disk (/dev/block/by-name/c2_scratch); Rust kernel with run-qemu.sh --ktest only");
        return;
    }

    // Whole disk.
    let mut vs = Stat::default();
    let r = fstatat(AT_FDCWD, c"/dev/block/vda".as_ptr() as *const u8, &mut vs, 0);
    check!(r == 0 && vs.mode & S_IFMT == 0o060000 && vs.rdev == makedev(254, 0), "vda_node", "rdev {:#x} mode {:o}", vs.rdev, vs.mode);
    let vda = open(c"/dev/block/vda".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    let size = size64(vda);
    let sys_size = sys_num(&cpath!("/sys/block/vda/size"));
    let end = lseek(vda, 0, SEEK_END);
    let mut sectors = 0u64;
    ioctl(vda, BLKGETSIZE, &mut sectors as *mut u64 as usize);
    check!(size > 0 && size as i64 == sys_size * 512 && end as u64 == size && sectors == size / 512,
           "size_ioctls_sysfs_seek_end", "BLKGETSIZE64 {} sysfs {} SEEK_END {} BLKGETSIZE {}", size, sys_size, end, sectors);
    let (mut ss, mut ps, mut bs) = (0u32, 0u32, 0u32);
    ioctl(vda, BLKSSZGET, &mut ss as *mut u32 as usize);
    ioctl(vda, BLKPBSZGET, &mut ps as *mut u32 as usize);
    ioctl(vda, BLKBSZGET, &mut bs as *mut u32 as usize);
    check!(ss == 512 && ps == 512 && bs == 4096, "sector_sizes", "{} {} {}", ss, ps, bs);
    let mut ro = -1i32;
    ioctl(vda, BLKROGET, &mut ro as *mut i32 as usize);
    check_eq!(ro, 0, "vda_writable");
    let mut mbr = [0u8; 1024];
    pread(vda, &mut mbr, 0);
    check!(mbr[510] == 0x55 && mbr[511] == 0xaa && &mbr[512..520] == b"EFI PART", "gpt_on_disk");

    // Partitions.
    let mut buf = [0u8; 4096];
    let n = slurp(&cpath!("/sys/class/block/vda3/uevent"), &mut buf);
    let ue = &buf[..n];
    check!(contains(ue, b"PARTNAME=c2_erofs\n") && contains(ue, b"PARTN=3\n") && contains(ue, b"DEVTYPE=partition\n")
           && contains(ue, b"MAJOR=254\n"), "partition_uevent", "{}", Bytes(ue));
    check_eq!(sys_num(&cpath!("/sys/block/vda/vda3/partition")), 3, "sysfs_partition_number");
    let mut link = [0u8; 128];
    let ln = readlink(c"/dev/block/by-name/c2_erofs".as_ptr() as *const u8, &mut link);
    check!(ln > 0 && &link[..ln as usize] == b"/dev/block/vda3", "by_name_symlink", "{}", Bytes(&link[..ln.max(0) as usize]));
    let ln = readlink(c"/sys/dev/block/254:3".as_ptr() as *const u8, &mut link);
    check!(ln > 0 && contains(&link[..ln as usize], b"block/vda/vda3"), "sys_dev_block_link", "{}", Bytes(&link[..ln.max(0) as usize]));
    let n = slurp(&cpath!("/proc/partitions"), &mut buf);
    check!(contains(&buf[..n], b" vda3\n") && contains(&buf[..n], b" vda6\n"), "proc_partitions");
    let mut p6 = Stat::default();
    fstatat(AT_FDCWD, c"/dev/block/vda6".as_ptr() as *const u8, &mut p6, 0);
    check_eq!(p6.rdev, makedev(254, 6), "partition_rdev");

    let p3 = open(c"/dev/block/vda3".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    let start = sys_num(&cpath!("/sys/block/vda/vda3/start")) as i64;
    let psize = size64(p3);
    let (mut a, mut b) = ([0u8; 8192], [0u8; 8192]);
    pread(vda, &mut a, start * 512);
    pread(p3, &mut b, 0);
    check!(start > 0 && a == b && b[1024..1028] == [0xe2, 0xe1, 0xf5, 0xe0]
           && psize as i64 == sys_num(&cpath!("/sys/block/vda/vda3/size")) * 512,
           "partition_offset", "start {} size {}", start, psize);
    check_eq!(pread(p3, &mut a[..512], psize as i64), 0, "read_at_end_is_eof");
    check_eq!(pread(p3, &mut a[..1024], psize as i64 - 512), 512, "read_short_at_end");

    // Writes on the scratch partition (vda6), at odd offsets, through the
    // cache, then again after BLKFLSBUF and through the whole disk.
    let s6 = open(c"/dev/block/by-name/c2_scratch".as_ptr() as *const u8, O_RDWR, 0) as i32;
    let start6 = sys_num(&cpath!("/sys/block/vda/vda6/start")) as i64;
    let mut pat = [0u8; 5000];
    for (i, x) in pat.iter_mut().enumerate() {
        *x = (i * 7 + 3) as u8;
    }
    let w = pwrite(s6, &pat, 1000);
    let mut back = [0u8; 5000];
    pread(s6, &mut back, 1000);
    let cached = back == pat;
    let fl = ioctl(s6, BLKFLSBUF, 0);
    back.fill(0);
    pread(s6, &mut back, 1000);
    let flushed = back == pat;
    back.fill(0);
    pread(vda, &mut back, start6 * 512 + 1000);
    check!(w == 5000 && cached && fl == 0 && flushed && back == pat, "write_read_cache_flush",
           "write {} cached {} flush {} flushed {} disk {}", w, cached, fl, flushed, back == pat);
    let s6size = size64(s6);
    check_err!(pwrite(s6, &pat[..512], s6size as i64), ENOSPC, "write_past_end_enospc");

    // BLKROSET.
    let one = 1i32;
    let zero = 0i32;
    let r1 = ioctl(s6, BLKROSET, &one as *const i32 as usize);
    let mut rg = 0i32;
    ioctl(s6, BLKROGET, &mut rg as *mut i32 as usize);
    let wro = pwrite(s6, &pat[..512], 0);
    let sys_ro = sys_num(&cpath!("/sys/block/vda/vda6/ro"));
    ioctl(s6, BLKROSET, &zero as *const i32 as usize);
    let wrw = pwrite(s6, &pat[..512], 0);
    check!(r1 == 0 && rg == 1 && wro == -(EPERM as isize) && sys_ro == 1 && wrw == 512, "blkroset",
           "set {} get {} write {} sysfs {} after {}", r1, rg, errname(wro), sys_ro, wrw);

    // BLKDISCARD (accepted, no-op) and BLKZEROOUT.
    let range = [4096u64, 8192u64];
    let d = ioctl(s6, BLKDISCARD, range.as_ptr() as usize);
    let bad = [1u64, 512u64];
    let dbad = ioctl(s6, BLKDISCARD, bad.as_ptr() as usize);
    pwrite(s6, &pat[..4096], 4096);
    let z = ioctl(s6, BLKZEROOUT, range.as_ptr() as usize);
    let mut zb = [1u8; 4096];
    pread(s6, &mut zb, 4096);
    check!(d == 0 && dbad == -(EINVAL as isize) && z == 0 && zb.iter().all(|&x| x == 0), "discard_zeroout",
           "discard {} unaligned {} zeroout {}", d, errname(dbad), z);
    close(s6);

    // The same image on a legacy-only (virtio 0.9) device, read-only.
    let vdb = open(c"/dev/block/vdb".as_ptr() as *const u8, O_RDONLY, 0) as i32;
    if vdb < 0 {
        skip("legacy_transport", "no vdb attached");
    } else {
        let mut ro = 0i32;
        ioctl(vdb, BLKROGET, &mut ro as *mut i32 as usize);
        let rw = open(c"/dev/block/vdb".as_ptr() as *const u8, O_RDWR, 0);
        // Partition 3 is never written: identical on both disks.
        pread(vdb, &mut a, start * 512);
        pread(p3, &mut b, 0);
        check!(size64(vdb) == size && ro == 1 && rw == -(EROFS as isize) && a == b, "legacy_transport",
               "size {} ro {} rw-open {}", size64(vdb), ro, errname(rw));
        close(vdb);
    }
    close(p3);
    close(vda);
}
