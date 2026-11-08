//! C2: disk file systems. Mounts each image of the C2 test disk
//! (tools/mkc2-images.py: ext4 4 KiB, ext3 1 KiB with htree dirs and
//! indirect blocks, erofs plain / lz4 compact / lz4 legacy) read-only from
//! its GPT partition and compares the whole tree with the source tree the
//! images were built from (/c2/tree in the initramfs: what the host's own
//! file system held). Per image: names, types, modes, sizes, contents,
//! symlink targets, hard links, user.* xattrs (in-inode and block),
//! statfs, mmap (private, shared refused), EROFS for writes, exec of a
//! static ELF from the image, st_dev, umount. Also mount errors (ENOTBLK,
//! EINVAL, ENODEV).
//!
//! Needs the disk (run-qemu.sh --ktest attaches it); on the host (no
//! /dev/block/by-name, no root to mount) every case is skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("c2_fs", main);

const TREE: &str = "/c2/tree";
const MS_RDONLY: usize = 1;
const ST_RDONLY: i64 = 1;
const EXT4_MAGIC: i64 = 0xef53;
const EROFS_MAGIC: i64 = 0xe0f5_e1e2;

struct Img {
    part: &'static str,
    fstype: &'static str,
    magic: i64,
}

const IMAGES: [Img; 5] = [
    Img { part: "c2_ext4", fstype: "ext4", magic: EXT4_MAGIC },
    Img { part: "c2_ext3_1k", fstype: "ext4", magic: EXT4_MAGIC },
    Img { part: "c2_erofs", fstype: "erofs", magic: EROFS_MAGIC },
    Img { part: "c2_erofs_lz4", fstype: "erofs", magic: EROFS_MAGIC },
    Img { part: "c2_erofs_legacy", fstype: "erofs", magic: EROFS_MAGIC },
];

/// "<part>_<what>" as a case name.
fn case(part: &str, what: &str) -> Buf<64> {
    let mut b = Buf::<64>::new();
    b.push(part.as_bytes());
    b.push(b"_");
    b.push(what.as_bytes());
    b
}

fn name(b: &Buf<64>) -> &str {
    core::str::from_utf8(b.as_bytes()).unwrap()
}

fn lstat(p: &Path, st: &mut Stat) -> isize {
    fstatat(AT_FDCWD, p.as_ptr(), st, AT_SYMLINK_NOFOLLOW)
}

/// Read a whole file into `buf` (up to its size); returns bytes read.
fn read_file(p: &Path, buf: &mut [u8]) -> isize {
    let fd = open(p.as_ptr(), O_RDONLY, 0) as i32;
    if fd < 0 {
        return fd as isize;
    }
    let mut n = 0usize;
    loop {
        let r = read(fd, &mut buf[n..]);
        if r < 0 {
            close(fd);
            return r;
        }
        if r == 0 || n + r as usize == buf.len() {
            n += r as usize;
            break;
        }
        n += r as usize;
    }
    close(fd);
    n as isize
}

const BUFSZ: usize = 4 << 20;

/// Difference report of a tree comparison.
struct Cmp {
    files: usize,
    first_diff: Path,
    ok: bool,
    a: *mut u8,
    b: *mut u8,
}

impl Cmp {
    fn fail(&mut self, p: &Path, what: &str) {
        if self.ok {
            self.ok = false;
            self.first_diff = cpath!("{} ({})", Bytes(p.as_bytes()), what);
        }
    }
}

/// Compare directory `m` (mounted image) with `t` (source tree).
fn compare_dir(m: &Path, t: &Path, c: &mut Cmp, depth: usize) {
    if depth > 16 || !c.ok {
        return;
    }
    let mut listing = [0u8; 32768];
    let tfd = open(t.as_ptr(), O_RDONLY | O_DIRECTORY, 0) as i32;
    let mfd = open(m.as_ptr(), O_RDONLY | O_DIRECTORY, 0) as i32;
    if tfd < 0 || mfd < 0 {
        c.fail(m, "opendir");
        close(tfd);
        close(mfd);
        return;
    }
    // Count the image's entries (minus lost+found) to catch extra names.
    let mut mcount = 0;
    loop {
        let n = getdents64(mfd, &mut listing);
        if n <= 0 {
            break;
        }
        walk(&listing[..n as usize], |nm, _| {
            if nm != b"." && nm != b".." && nm != b"lost+found" {
                mcount += 1;
            }
        });
    }
    close(mfd);
    let mut tcount = 0;
    loop {
        let n = getdents64(tfd, &mut listing);
        if n <= 0 {
            break;
        }
        let chunk = listing;
        walk(&chunk[..n as usize], |nm, _| {
            if nm == b"." || nm == b".." || !c.ok {
                return;
            }
            tcount += 1;
            let mp = cpath!("{}/{}", Bytes(m.as_bytes()), Bytes(nm));
            let tp = cpath!("{}/{}", Bytes(t.as_bytes()), Bytes(nm));
            compare_entry(&mp, &tp, c, depth);
        });
    }
    close(tfd);
    if c.ok && mcount != tcount {
        c.fail(m, "entry count");
    }
}

fn compare_entry(mp: &Path, tp: &Path, c: &mut Cmp, depth: usize) {
    let (mut ms, mut ts) = (Stat::default(), Stat::default());
    if lstat(mp, &mut ms) < 0 {
        return c.fail(mp, "missing");
    }
    lstat(tp, &mut ts);
    if ms.mode != ts.mode {
        return c.fail(mp, "mode");
    }
    match ts.mode & S_IFMT {
        S_IFDIR => compare_dir(mp, tp, c, depth + 1),
        S_IFLNK => {
            let (mut a, mut b) = ([0u8; 512], [0u8; 512]);
            let (na, nb) = (readlink(mp.as_ptr(), &mut a), readlink(tp.as_ptr(), &mut b));
            if na < 0 || na != nb || a[..na as usize] != b[..nb as usize] {
                c.fail(mp, "symlink target");
            }
        }
        S_IFREG => {
            if ms.size != ts.size {
                return c.fail(mp, "size");
            }
            let a = unsafe { core::slice::from_raw_parts_mut(c.a, BUFSZ) };
            let b = unsafe { core::slice::from_raw_parts_mut(c.b, BUFSZ) };
            let (na, nb) = (read_file(mp, a), read_file(tp, b));
            if na != ts.size as isize || nb != na || a[..na as usize] != b[..nb as usize] {
                return c.fail(mp, "contents");
            }
            c.files += 1;
        }
        _ => {}
    }
}

/// getdents64 records: (name, d_type).
fn walk(buf: &[u8], mut f: impl FnMut(&[u8], u8)) {
    let mut off = 0;
    while off + 19 <= buf.len() {
        let reclen = u16::from_ne_bytes([buf[off + 16], buf[off + 17]]) as usize;
        if reclen == 0 {
            break;
        }
        f(cfield(&buf[off + 19..off + reclen]), buf[off + 18]);
        off += reclen;
    }
}

fn statfs(p: &Path, out: &mut [i64; 15]) -> isize {
    unsafe { syscall!(nr::STATFS, p.as_ptr(), out.as_mut_ptr()) }
}

fn lgetxattr(p: &Path, n: &core::ffi::CStr, v: &mut [u8]) -> isize {
    unsafe { syscall!(nr::LGETXATTR, p.as_ptr(), n.as_ptr(), v.as_mut_ptr(), v.len()) }
}

fn test_image(img: &Img, cmp_a: *mut u8, cmp_b: *mut u8) {
    let part = img.part;
    let dev = cpath!("/dev/block/by-name/{}", part);
    let mnt = cpath!("/tmp/c2fs-{}", part);
    mkdirat(AT_FDCWD, mnt.as_ptr(), 0o755);
    let ty = cpath!("{}", img.fstype);
    let r = mount(dev.as_ptr(), mnt.as_ptr(), ty.as_ptr(), MS_RDONLY, core::ptr::null());
    if check_ok!(r, name(&case(part, "mount"))) < 0 {
        return;
    }

    // The whole tree.
    let mut c = Cmp { files: 0, first_diff: Path::new(), ok: true, a: cmp_a, b: cmp_b };
    compare_dir(&mnt, &cpath!("{}", TREE), &mut c, 0);
    check!(c.ok && c.files > 300, name(&case(part, "tree_matches_host")), "{} files, first difference: {}",
           c.files, Bytes(c.first_diff.as_bytes()));

    // statfs: magic and read-only flag.
    let mut sf = [0i64; 15];
    check!(statfs(&mnt, &mut sf) == 0 && sf[0] == img.magic && sf[10] & ST_RDONLY != 0 && sf[2] > 0,
           name(&case(part, "statfs")), "type {:#x} blocks {} flags {:#x}", sf[0], sf[2], sf[10]);

    // st_dev of files = the partition's device number.
    let (mut ds, mut fs) = (Stat::default(), Stat::default());
    fstatat(AT_FDCWD, dev.as_ptr(), &mut ds, 0);
    fstatat(AT_FDCWD, cpath!("{}/small.txt", Bytes(mnt.as_bytes())).as_ptr(), &mut fs, 0);
    check!(ds.rdev != 0 && fs.dev == ds.rdev, name(&case(part, "st_dev")), "st_dev {:#x}, device {:#x}", fs.dev, ds.rdev);

    // Hard link: same inode, two links.
    let mut hs = Stat::default();
    fstatat(AT_FDCWD, cpath!("{}/hard.txt", Bytes(mnt.as_bytes())).as_ptr(), &mut hs, 0);
    check!(hs.ino == fs.ino && hs.nlink == 2, name(&case(part, "hard_link")), "ino {} vs {}, nlink {}", hs.ino, fs.ino, hs.nlink);

    // xattrs: small (in the inode) and 200 bytes (ext4: xattr block).
    let mut v = [0u8; 256];
    let small = cpath!("{}/small.txt", Bytes(mnt.as_bytes()));
    let n = lgetxattr(&small, c"user.c2", &mut v);
    let big = lgetxattr(&cpath!("{}/a", Bytes(mnt.as_bytes())), c"user.big", &mut v[..]);
    let all_x = big == 200 && v[..200].iter().all(|&b| b == b'x');
    let n2 = lgetxattr(&small, c"user.c2", &mut v);
    let missing = lgetxattr(&small, c"user.nope", &mut v);
    let mut list = [0u8; 256];
    let ln = unsafe { syscall!(nr::LLISTXATTR, small.as_ptr(), list.as_mut_ptr(), list.len()) };
    let listed = ln > 0 && list[..ln as usize].split(|&b| b == 0).any(|x| x == b"user.c2");
    check!(n == 11 && n2 == 11 && &v[..11] == b"hello-xattr" && all_x && missing == -(ENODATA as isize) && listed,
           name(&case(part, "xattrs")), "user.c2 {} big {} missing {} list {}", n, big, errname(missing), ln);

    // mmap: private read matches read(); shared+write refused on O_RDONLY.
    let rpath = cpath!("{}/random.bin", Bytes(mnt.as_bytes()));
    let fd = open(rpath.as_ptr(), O_RDONLY, 0) as i32;
    let len = 1048699usize;
    let p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    let a = unsafe { core::slice::from_raw_parts_mut(cmp_a, BUFSZ) };
    let got = pread(fd, &mut a[..len], 0);
    let mut ok = p > 0 && got == len as isize;
    if ok {
        let m = unsafe { core::slice::from_raw_parts_mut(p as *mut u8, len) };
        ok = m[..] == a[..len];
        m[5] ^= 0xff; // private copy only
        let mut one = [0u8; 1];
        pread(fd, &mut one, 5);
        ok &= one[0] == a[5];
        munmap(p as *mut u8, len);
    }
    let shared = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check!(ok && shared == -(EACCES as isize), name(&case(part, "mmap")), "private {:#x}, shared rw {}", p, errname(shared));
    close(fd);

    // Read-only: no writes, creates or unlinks.
    let w = open(rpath.as_ptr(), O_WRONLY, 0);
    let mk = mkdirat(AT_FDCWD, cpath!("{}/newdir", Bytes(mnt.as_bytes())).as_ptr(), 0o755);
    let un = unlinkat(AT_FDCWD, small.as_ptr(), 0);
    check!(w == -(EROFS as isize) && mk == -(EROFS as isize) && un == -(EROFS as isize),
           name(&case(part, "read_only")), "open {} mkdir {} unlink {}", errname(w), errname(mk), errname(un));

    // A static ELF executed from the image (pages from its page cache).
    let hello = cpath!("{}/bin/hello", Bytes(mnt.as_bytes()));
    let status = run_child(|| {
        let null = open(c"/dev/null".as_ptr() as *const u8, O_WRONLY, 0) as i32;
        dup3(null, 1, 0);
        let av = [hello.as_ptr(), core::ptr::null()];
        let ev = [core::ptr::null()];
        execve(hello.as_ptr(), av.as_ptr(), ev.as_ptr());
        99
    });
    check!(wifexited(status) && wexitstatus(status) == 0, name(&case(part, "exec_from_image")), "status {:#x}", status);

    check_eq!(umount2(mnt.as_ptr(), 0), 0, name(&case(part, "umount")));
    let mut st = Stat::default();
    check!(lstat(&cpath!("{}/small.txt", Bytes(mnt.as_bytes())), &mut st) == -(ENOENT as isize),
           name(&case(part, "gone_after_umount")));
}

fn main() {
    let probe = cpath!("/dev/block/by-name/c2_ext4");
    let mut st = Stat::default();
    if fstatat(AT_FDCWD, probe.as_ptr(), &mut st, 0) < 0 {
        skip("all", "no C2 test disk (/dev/block/by-name/c2_ext4); Rust kernel with run-qemu.sh --ktest only");
        return;
    }
    let a = alloc_pages(BUFSZ);
    let b = alloc_pages(BUFSZ);
    for img in &IMAGES {
        test_image(img, a, b);
    }

    // Mount errors.
    let mnt = cpath!("/tmp/c2fs-err");
    mkdirat(AT_FDCWD, mnt.as_ptr(), 0o755);
    let file = cpath!("{}/small.txt", TREE);
    check_err!(mount(file.as_ptr(), mnt.as_ptr(), c"ext4".as_ptr() as *const u8, MS_RDONLY, core::ptr::null()),
               ENOTBLK, "mount_regular_file_enotblk");
    let erofs = cpath!("/dev/block/by-name/c2_erofs");
    check_err!(mount(erofs.as_ptr(), mnt.as_ptr(), c"ext4".as_ptr() as *const u8, MS_RDONLY, core::ptr::null()),
               EINVAL, "mount_wrong_type_einval");
    check_err!(mount(erofs.as_ptr(), mnt.as_ptr(), c"nosuchfs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null()),
               ENODEV, "mount_unknown_type_enodev");
    // Mounted twice at once (two mount points, one device).
    let m2 = cpath!("/tmp/c2fs-err2");
    mkdirat(AT_FDCWD, m2.as_ptr(), 0o755);
    let r1 = mount(erofs.as_ptr(), mnt.as_ptr(), c"erofs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
    let r2 = mount(erofs.as_ptr(), m2.as_ptr(), c"erofs".as_ptr() as *const u8, MS_RDONLY, core::ptr::null());
    let mut buf = [0u8; 16];
    let n = read_file(&cpath!("{}/small.txt", Bytes(m2.as_bytes())), &mut buf);
    check!(r1 == 0 && r2 == 0 && n == 9 && &buf[..9] == b"hello c2\n", "mount_twice", "{} {} read {}", r1, r2, n);
    umount2(mnt.as_ptr(), 0);
    umount2(m2.as_ptr(), 0);
}
