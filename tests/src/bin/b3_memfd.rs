//! B3: memfd and file-backed mappings — memfd_create, ftruncate, two
//! MAP_SHARED views alias, sharing across fork and via fd, seals
//! (F_SEAL_SHRINK/GROW/WRITE), MAP_PRIVATE file COW, MAP_SHARED write-back,
//! SIGBUS beyond EOF.
#![no_std]
#![no_main]

use core::ptr::{read_volatile as rd, write_volatile as wr};
use rt::*;

rt::test_main!("b3_memfd", main);

const PG: usize = 4096;

fn memfd(name: &core::ffi::CStr, flags: u32) -> isize {
    unsafe { syscall!(nr::MEMFD_CREATE, name.as_ptr(), flags) }
}

fn main() {
    let fd = check_ok!(memfd(c"ktest", MFD_CLOEXEC | MFD_ALLOW_SEALING), "memfd_create") as i32;
    check_eq!(fcntl(fd, F_GETFD, 0), FD_CLOEXEC as isize, "memfd_cloexec");
    let mut st = Stat::default();
    fstat(fd, &mut st);
    check!(st.size == 0 && st.mode & S_IFMT == S_IFREG, "memfd_empty_regular");

    check_ok!(ftruncate(fd, 2 * PG as i64), "memfd_ftruncate");
    check_eq!(write(fd, b"memfd!"), 6, "memfd_write");

    // Two shared views of the same pages alias each other and the file.
    let a = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    let b = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    check!(a > 0 && b > 0 && a != b, "memfd_mmap_twice");
    let (a, b) = (a as *mut u8, b as *mut u8);
    check_eq!(unsafe { rd(a) }, b'm', "mmap_sees_written_data");
    unsafe { wr(a.add(PG + 5), 0x77) };
    check_eq!(unsafe { rd(b.add(PG + 5)) }, 0x77, "shared_views_alias");
    let mut buf = [0u8; 1];
    pread(fd, &mut buf, (PG + 5) as i64);
    check_eq!(buf[0], 0x77, "mmap_write_visible_via_read");
    pwrite(fd, b"\x42", 10);
    check_eq!(unsafe { rd(a.add(10)) }, 0x42, "write_visible_via_mmap");

    // Offset mapping.
    let c = mmap(0, PG, PROT_READ, MAP_SHARED, fd, PG as i64);
    check!(c > 0 && unsafe { rd((c as *const u8).add(5)) } == 0x77, "mmap_with_offset");

    // Shared with a forked child, and with an unrelated mapping of a dup'd fd.
    let st2 = run_child(|| {
        unsafe { wr(a.add(100), 0x99) };
        0
    });
    check!(st2 == 0 && unsafe { rd(b.add(100)) } == 0x99, "memfd_shared_across_fork");

    // Seals.
    check_eq!(fcntl(fd, F_GET_SEALS, 0), 0, "no_seals_initially");
    check_ok!(fcntl(fd, F_ADD_SEALS, (F_SEAL_SHRINK | F_SEAL_GROW) as usize), "add_seals_shrink_grow");
    check_eq!(fcntl(fd, F_GET_SEALS, 0), (F_SEAL_SHRINK | F_SEAL_GROW) as isize, "get_seals");
    check_err!(ftruncate(fd, PG as i64), EPERM, "seal_shrink_blocks_truncate");
    check_err!(ftruncate(fd, 4 * PG as i64), EPERM, "seal_grow_blocks_extend");
    check_err!(pwrite(fd, b"x", 3 * PG as i64), EPERM, "seal_grow_blocks_write_past_eof");
    check_eq!(pwrite(fd, b"x", 0), 1, "write_inside_allowed");
    // F_SEAL_WRITE fails while shared mappings that could become writable
    // exist (VM_MAYWRITE: even PROT_READ ones from an O_RDWR fd).
    check_err!(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE as usize), EBUSY, "seal_write_busy_with_mapping");
    munmap(a, 2 * PG);
    munmap(b, 2 * PG);
    munmap(c as *mut u8, PG);
    check_ok!(fcntl(fd, F_ADD_SEALS, (F_SEAL_WRITE | F_SEAL_SEAL) as usize), "seal_write_and_seal");
    check_err!(pwrite(fd, b"x", 0), EPERM, "seal_write_blocks_write");
    check_err!(mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0), EPERM, "seal_write_blocks_shared_rw_mmap");
    check!(mmap(0, PG, PROT_READ, MAP_SHARED, fd, 0) > 0, "seal_write_allows_ro_mmap");
    check_err!(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW as usize), EPERM, "seal_seal_blocks_more_seals");
    close(fd);

    // Without MFD_ALLOW_SEALING, sealing is refused.
    let fd2 = memfd(c"nosealing", 0) as i32;
    check_err!(fcntl(fd2, F_ADD_SEALS, F_SEAL_SHRINK as usize), EPERM, "sealing_not_allowed");
    close(fd2);

    // Access beyond EOF in a shared file mapping raises SIGBUS.
    let fd3 = memfd(c"sigbus", 0) as i32;
    ftruncate(fd3, PG as i64);
    let m = mmap(0, 2 * PG, PROT_READ | PROT_WRITE, MAP_SHARED, fd3, 0) as *mut u8;
    let st3 = run_child(|| {
        unsafe { wr(m.add(PG), 1) };
        0
    });
    check!(wifsignaled(st3) && wtermsig(st3) == SIGBUS, "access_past_eof_sigbus", "status {:#x}", st3);
    close(fd3);

    // Regular files: MAP_PRIVATE is COW, MAP_SHARED writes back.
    let dir = scratch_dir();
    let f = cpath!("{}/data", Bytes(dir.as_bytes()));
    let ffd = open(f.as_ptr(), O_CREAT | O_RDWR, 0o600) as i32;
    let mut page = [0u8; PG];
    for (i, x) in page.iter_mut().enumerate() {
        *x = i as u8;
    }
    write(ffd, &page);
    let pm = mmap(0, PG, PROT_READ | PROT_WRITE, MAP_PRIVATE, ffd, 0);
    check!(pm > 0, "file_mmap_private");
    let pm = pm as *mut u8;
    check!((0..PG).all(|i| unsafe { rd(pm.add(i)) } == i as u8), "file_mmap_contents");
    unsafe { wr(pm, 0xee) };
    pread(ffd, &mut buf, 0);
    check_eq!(buf[0], 0, "private_write_not_in_file");
    let sm = mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED, ffd, 0) as *mut u8;
    unsafe { wr(sm.add(1), 0xcd) };
    pread(ffd, &mut buf, 1);
    check_eq!(buf[0], 0xcd, "shared_write_in_file");
    check_ok!(unsafe { syscall!(26, sm, PG, 4) }, "msync"); // MS_SYNC
    // A read-only fd cannot back a writable shared mapping.
    let rofd = open(f.as_ptr(), O_RDONLY, 0) as i32;
    check_err!(mmap(0, PG, PROT_READ | PROT_WRITE, MAP_SHARED, rofd, 0), EACCES, "shared_rw_on_rdonly_fd_eacces");
    check!(mmap(0, PG, PROT_READ | PROT_WRITE, MAP_PRIVATE, rofd, 0) > 0, "private_rw_on_rdonly_fd_ok");
    // Mapping survives closing the fd and unlinking the file.
    close(rofd);
    close(ffd);
    unlinkat(AT_FDCWD, f.as_ptr(), 0);
    check_eq!(unsafe { rd(sm.add(1)) }, 0xcd, "mapping_outlives_fd_and_name");
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}

