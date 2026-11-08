//! B3: shared memory details — memfd names in /proc/self/fd and maps, a
//! futex in shared memory across processes and mappings, mremap of a file
//! mapping, fallocate, truncate under a live mapping, MAP_SHARED /dev/zero,
//! F_SEAL_FUTURE_WRITE, msync/mprotect errors.
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use rt::*;

rt::test_main!("b3_shmem", main);

const PG: usize = 4096;
const FALLOCATE: usize = 285;
const MSYNC: usize = 26;
const MS_ASYNC: usize = 1;
const MS_SYNC: usize = 4;
const F_SEAL_FUTURE_WRITE: usize = 16;
const FALLOC_FL_KEEP_SIZE: usize = 1;
const FALLOC_FL_PUNCH_HOLE: usize = 2;
const RW: usize = PROT_READ | PROT_WRITE;

fn memfd(name: &core::ffi::CStr, flags: u32) -> i32 {
    unsafe { syscall!(nr::MEMFD_CREATE, name.as_ptr(), flags) as i32 }
}

fn fallocate(fd: i32, mode: usize, off: usize, len: usize) -> isize {
    unsafe { syscall!(FALLOCATE, fd, mode, off, len) }
}

fn size_of(fd: i32) -> i64 {
    let mut st = Stat::default();
    fstat(fd, &mut st);
    st.size
}

fn read_file(path: *const u8, buf: &mut [u8]) -> usize {
    let fd = open(path, O_RDONLY, 0) as i32;
    let mut n = 0;
    while n < buf.len() {
        let r = read(fd, &mut buf[n..]);
        if r <= 0 {
            break;
        }
        n += r as usize;
    }
    close(fd);
    n
}

/// The /proc/self/maps line of the mapping starting at `addr`.
fn maps_line(maps: &[u8], addr: usize) -> &[u8] {
    let prefix = cpath!("{:08x}-", addr);
    maps.split(|&c| c == b'\n').find(|l| l.starts_with(prefix.as_bytes())).unwrap_or(b"")
}

fn main() {
    // Names: /memfd:NAME (deleted) in /proc/self/fd and in maps, with 's'
    // and the file offset.
    let fd = memfd(c"shm-test", MFD_CLOEXEC);
    check!(fd >= 0, "memfd_create");
    ftruncate(fd, 2 * PG as i64);
    let link = cpath!("/proc/self/fd/{}", fd);
    let mut buf = [0u8; 256];
    let n = unsafe { syscall!(nr::READLINK, link.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    let target = &buf[..n.max(0) as usize];
    check!(target == b"/memfd:shm-test (deleted)", "proc_fd_memfd_name", "got {}", Bytes(target));
    let m = mmap(0, PG, RW, MAP_SHARED, fd, PG as i64);
    check!(m > 0, "mmap_memfd_offset");
    let mut maps = [0u8; 16384];
    let n = read_file(c"/proc/self/maps".as_ptr() as *const u8, &mut maps);
    let line = maps_line(&maps[..n], m as usize);
    check!(
        line.windows(14).any(|w| w == b" rw-s 00001000") && line.ends_with(b"/memfd:shm-test (deleted)"),
        "maps_memfd_line",
        "got {}",
        Bytes(line)
    );
    check_err!(memfd(c"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 0) as isize, EINVAL, "memfd_name_too_long");

    // A futex in shared memory: the child waits through one mapping, the
    // parent wakes it through another one at a different address.
    let v1 = mmap(0, PG, RW, MAP_SHARED, fd, 0) as *mut i32;
    let v2 = mmap(0, PG, RW, MAP_SHARED, fd, 0) as *mut i32;
    check!(v1 != v2, "two_views");
    unsafe { wr(v1.add(1), 0) };
    let pid = fork();
    if pid == 0 {
        unsafe { wr(v1.add(1), 1) }; // ready
        let ts = Timespec { sec: 5, nsec: 0 };
        while unsafe { rd(v1) } == 0 {
            if futex(v1, FUTEX_WAIT, 0, &ts) == -(ETIMEDOUT as isize) {
                exit(1);
            }
        }
        exit(0);
    }
    while unsafe { rd(v2.add(1)) } == 0 {
        sleep_ms(1);
    }
    sleep_ms(50);
    unsafe { wr(v2, 1) };
    futex(v2, FUTEX_WAKE, 1, core::ptr::null());
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    check_eq!(status_code(st), 0, "shared_futex_across_processes_and_views");

    // mremap of a file mapping keeps its file offset as it grows.
    unsafe { wr(v1 as *mut u8, b'A') };
    pwrite(fd, b"B", PG as i64);
    let r = unsafe { syscall!(nr::MREMAP, v1, PG, 2 * PG, MREMAP_MAYMOVE) };
    check!(r > 0, "mremap_file_grow", "returned {}", errname(r));
    if r > 0 {
        let p = r as *const u8;
        check!(unsafe { rd(p) == b'A' && rd(p.add(PG)) == b'B' }, "mremap_file_contents");
    }

    // Shrinking the file unmaps the pages past the end (SIGBUS) and zeroes
    // the tail of the new last page; growing it again gives zero pages.
    let t = mmap(0, 2 * PG, RW, MAP_SHARED, fd, 0) as *mut u8;
    for i in PG..2 * PG {
        unsafe { wr(t.add(i), 0xff) };
    }
    check_ok!(ftruncate(fd, PG as i64 + 10), "truncate_mapped");
    let st = run_child(|| {
        unsafe { wr(t.add(PG + 100), 1) };
        0
    });
    check!(st == 0, "inside_last_page_ok", "status {:#x}", st);
    check_eq!(unsafe { rd(t.add(PG + 9)) }, 0xff, "kept_before_new_eof");
    ftruncate(fd, PG as i64);
    let st = run_child(|| unsafe { rd(t.add(PG)) } as i32);
    check!(wifsignaled(st) && wtermsig(st) == SIGBUS, "truncated_page_sigbus", "status {:#x}", st);
    ftruncate(fd, 2 * PG as i64);
    check_eq!(unsafe { rd(t.add(PG + 9)) }, 0, "regrown_page_zero");
    close(fd);

    // fallocate: allocate (grows), KEEP_SIZE, punch hole, bad modes.
    let fd = memfd(c"falloc", 0);
    check_ok!(fallocate(fd, 0, 0, 3 * PG), "fallocate_grow");
    check_eq!(size_of(fd), 3 * PG as i64, "fallocate_size");
    check_ok!(fallocate(fd, FALLOC_FL_KEEP_SIZE, 0, 8 * PG), "fallocate_keep_size");
    check_eq!(size_of(fd), 3 * PG as i64, "keep_size_unchanged");
    pwrite(fd, b"xyz", 10);
    check_ok!(fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 11, PG), "punch_hole");
    let mut b3 = [0u8; 3];
    pread(fd, &mut b3, 10);
    check!(b3 == *b"x\0\0" && size_of(fd) == 3 * PG as i64, "punched_reads_zero");
    check_err!(fallocate(fd, FALLOC_FL_PUNCH_HOLE, 0, PG), EOPNOTSUPP, "punch_without_keep_size");
    check_err!(fallocate(fd, 0, 0, 0), EINVAL, "fallocate_zero_len");
    close(fd);

    // F_SEAL_FUTURE_WRITE: existing writable mappings stay writable, but
    // write(2), new writable shared mappings and mprotect(PROT_WRITE) on a
    // new read-only one are refused.
    let fd = memfd(c"future", MFD_ALLOW_SEALING);
    ftruncate(fd, PG as i64);
    let w = mmap(0, PG, RW, MAP_SHARED, fd, 0) as *mut u8;
    check_ok!(fcntl(fd, F_ADD_SEALS, F_SEAL_FUTURE_WRITE), "add_future_write");
    unsafe { wr(w, 7) };
    pread(fd, &mut b3[..1], 0);
    check_eq!(b3[0], 7, "old_mapping_still_writable");
    check_err!(pwrite(fd, b"x", 0), EPERM, "future_write_blocks_write");
    check_err!(mmap(0, PG, RW, MAP_SHARED, fd, 0), EPERM, "future_write_blocks_rw_mmap");
    let ro = mmap(0, PG, PROT_READ, MAP_SHARED, fd, 0);
    check!(ro > 0, "future_write_allows_ro_mmap");
    check_err!(mprotect(ro as *mut u8, PG, RW), EACCES, "future_write_blocks_mprotect");
    close(fd);

    // MAP_SHARED /dev/zero is shared across fork; MAP_PRIVATE is plain
    // anonymous memory.
    let z = open(c"/dev/zero".as_ptr() as *const u8, O_RDWR, 0) as i32;
    let zs = mmap(0, PG, RW, MAP_SHARED, z, 0) as *mut u8;
    let zp = mmap(0, PG, RW, MAP_PRIVATE, z, 0) as *mut u8;
    check!(unsafe { rd(zs) == 0 && rd(zp) == 0 }, "dev_zero_maps_zero");
    let st = run_child(|| {
        unsafe {
            wr(zs, 3);
            wr(zp, 3);
        }
        0
    });
    check!(st == 0 && unsafe { rd(zs) == 3 && rd(zp) == 0 }, "dev_zero_shared_vs_private");
    close(z);

    // A read-only fd's shared mapping can't become writable; msync checks.
    let dir = scratch_dir();
    let f = cpath!("{}/ro", Bytes(dir.as_bytes()));
    let wfd = open(f.as_ptr(), O_CREAT | O_RDWR, 0o600) as i32;
    pwrite(wfd, b"data", 0);
    close(wfd);
    let rfd = open(f.as_ptr(), O_RDONLY, 0) as i32;
    let rm = mmap(0, PG, PROT_READ, MAP_SHARED, rfd, 0);
    check!(rm > 0, "shared_ro_map_of_rdonly_fd");
    check_err!(mprotect(rm as *mut u8, PG, RW), EACCES, "mprotect_write_on_rdonly_shared");
    check_err!(unsafe { syscall!(MSYNC, rm + 1, PG, MS_SYNC) }, EINVAL, "msync_unaligned");
    check_err!(unsafe { syscall!(MSYNC, rm, PG, MS_SYNC | MS_ASYNC) }, EINVAL, "msync_sync_and_async");
    munmap(rm as *mut u8, PG);
    check_err!(unsafe { syscall!(MSYNC, rm, PG, MS_ASYNC) }, ENOMEM, "msync_unmapped");
    close(rfd);
    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}
