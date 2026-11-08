//! B2: directory listing with getdents64 — entries, d_type, ".."/".",
//! small buffers (resume across calls), EINVAL on tiny buffer, ENOTDIR.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b2_getdents", main);

const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

/// Walk a getdents64 buffer: (ino, d_type, name).
fn for_each(buf: &[u8], mut f: impl FnMut(u64, u8, &[u8])) -> bool {
    let mut off = 0;
    while off < buf.len() {
        if off + 19 > buf.len() {
            return false;
        }
        let ino = u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap());
        let reclen = u16::from_ne_bytes(buf[off + 16..off + 18].try_into().unwrap()) as usize;
        let ty = buf[off + 18];
        if reclen < 20 || off + reclen > buf.len() || reclen % 8 != 0 {
            return false;
        }
        let name = cfield(&buf[off + 19..off + reclen]);
        f(ino, ty, name);
        off += reclen;
    }
    true
}

fn main() {
    let dir = scratch_dir();
    let d = Bytes(dir.as_bytes());
    let names: [&[u8]; 3] = [b"alpha", b"beta", b"a-much-longer-file-name-to-vary-record-lengths"];
    for n in names {
        let p = cpath!("{}/{}", d, Bytes(n));
        close(open(p.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32);
    }
    let sub = cpath!("{}/subdir", d);
    mkdirat(AT_FDCWD, sub.as_ptr(), 0o755);
    let ln = cpath!("{}/link", d);
    unsafe { syscall!(nr::SYMLINKAT, c"alpha".as_ptr(), AT_FDCWD, ln.as_ptr()) };

    let fd = open(dir.as_ptr(), O_RDONLY | O_DIRECTORY, 0) as i32;
    let mut buf = [0u8; 4096];
    let n = check_ok!(getdents64(fd, &mut buf), "getdents64");
    let mut seen = [false; 7]; // . .. alpha beta long subdir link
    let mut types_ok = true;
    let mut ino_ok = true;
    let well_formed = for_each(&buf[..n.max(0) as usize], |ino, ty, name| {
        ino_ok &= ino != 0;
        let idx = match name {
            b"." => 0,
            b".." => 1,
            b"alpha" => 2,
            b"beta" => 3,
            b"subdir" => 5,
            b"link" => 6,
            n if n == names[2] => 4,
            _ => return,
        };
        seen[idx] = true;
        let want = match idx {
            0 | 1 | 5 => DT_DIR,
            6 => DT_LNK,
            _ => DT_REG,
        };
        // DT_UNKNOWN (0) is allowed by the ABI, but Android's code paths
        // expect real types from tmpfs/ext4, so require them.
        types_ok &= ty == want;
    });
    check!(well_formed, "records_well_formed");
    check!(seen.iter().all(|&s| s), "all_entries_listed", "seen {:?}", seen);
    check!(types_ok, "d_type_correct");
    check!(ino_ok, "d_ino_nonzero");
    check_eq!(getdents64(fd, &mut buf), 0, "end_of_directory");

    // Rewind and read with a buffer that fits only one or two records.
    lseek(fd, 0, SEEK_SET);
    // 80 bytes: holds the longest record (72) but rarely two.
    let mut small = [0u8; 80];
    let mut count = 0;
    let mut ok = true;
    for _ in 0..100 {
        let n = getdents64(fd, &mut small);
        if n <= 0 {
            ok &= n == 0;
            break;
        }
        ok &= for_each(&small[..n as usize], |_, _, _| count += 1);
    }
    check!(ok, "small_buffer_resume");
    check_eq!(count, 7, "small_buffer_total_entries");

    let mut tiny = [0u8; 8];
    lseek(fd, 0, SEEK_SET);
    check_err!(getdents64(fd, &mut tiny), EINVAL, "tiny_buffer_einval");

    let file = cpath!("{}/alpha", d);
    let ffd = open(file.as_ptr(), O_RDONLY, 0) as i32;
    check_err!(getdents64(ffd, &mut buf), ENOTDIR, "getdents_on_file_enotdir");
    close(ffd);

    // Entries removed after opendir disappear from a fresh listing.
    unlinkat(AT_FDCWD, file.as_ptr(), 0);
    lseek(fd, 0, SEEK_SET);
    let n = getdents64(fd, &mut buf);
    let mut found = false;
    for_each(&buf[..n.max(0) as usize], |_, _, name| found |= name == b"alpha");
    check!(!found, "unlinked_entry_gone");

    // Empty directory lists just "." and "..".
    let sfd = open(sub.as_ptr(), O_RDONLY | O_DIRECTORY, 0) as i32;
    let n = getdents64(sfd, &mut buf);
    let mut c = 0;
    for_each(&buf[..n.max(0) as usize], |_, _, _| c += 1);
    check_eq!(c, 2, "empty_dir_dot_dotdot");
    close(sfd);
    close(fd);

    // Cleanup.
    for n in &names[1..] {
        let p = cpath!("{}/{}", d, Bytes(n));
        unlinkat(AT_FDCWD, p.as_ptr(), 0);
    }
    unlinkat(AT_FDCWD, ln.as_ptr(), 0);
    unlinkat(AT_FDCWD, sub.as_ptr(), AT_REMOVEDIR);
    check_ok!(unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR), "cleanup_scratch");
}
