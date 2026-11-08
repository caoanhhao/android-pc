//! B2: regular files and directories through the VFS — openat flags,
//! read/write/lseek/pread/pwrite, stat, truncate, rename, link/symlink,
//! mkdir/rmdir, dup/fcntl, cwd. Works in a scratch directory under /tmp.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("b2_files", main);

fn main() {
    let dir = scratch_dir();
    let d = Bytes(dir.as_bytes());
    let f = cpath!("{}/file", d);

    // Create, write, read back.
    let fd = check_ok!(open(f.as_ptr(), O_CREAT | O_EXCL | O_RDWR, 0o640), "open_creat_excl") as i32;
    check_err!(open(f.as_ptr(), O_CREAT | O_EXCL | O_RDWR, 0o640), EEXIST, "open_excl_eexist");
    check_eq!(write(fd, b"hello, kernel"), 13, "write_file");
    check_eq!(lseek(fd, 0, SEEK_CUR), 13, "lseek_cur_after_write");
    check_eq!(lseek(fd, 0, SEEK_SET), 0, "lseek_set");
    let mut buf = [0u8; 64];
    check_eq!(read(fd, &mut buf), 13, "read_file");
    check_eq!(&buf[..13], b"hello, kernel", "read_content");
    check_eq!(read(fd, &mut buf), 0, "read_eof");
    check_eq!(lseek(fd, -6, SEEK_END), 7, "lseek_end");
    check_eq!(read(fd, &mut buf[..6]), 6, "read_after_seek_end");
    check_eq!(&buf[..6], b"kernel", "read_after_seek_content");
    check_err!(lseek(fd, -100, SEEK_SET), EINVAL, "lseek_negative_einval");

    // pread/pwrite leave the offset alone.
    check_eq!(pwrite(fd, b"HELLO", 0), 5, "pwrite");
    check_eq!(lseek(fd, 0, SEEK_CUR), 13, "pwrite_keeps_offset");
    check_eq!(pread(fd, &mut buf[..5], 0), 5, "pread");
    check_eq!(&buf[..5], b"HELLO", "pread_content");

    // Writing past EOF leaves a zero-filled hole.
    check_eq!(pwrite(fd, b"Z", 100), 1, "pwrite_past_eof");
    check_eq!(pread(fd, &mut buf[..10], 50), 10, "read_hole");
    check!(buf[..10].iter().all(|&b| b == 0), "hole_is_zero");

    // readv/writev.
    let (mut x, mut y) = (*b"abc", *b"defg");
    let iov = [Iovec { base: x.as_mut_ptr(), len: 3 }, Iovec { base: y.as_mut_ptr(), len: 4 }];
    lseek(fd, 0, SEEK_SET);
    check_eq!(unsafe { syscall!(nr::WRITEV, fd, iov.as_ptr(), 2) }, 7, "writev");
    x = [0; 3];
    y = [0; 4];
    lseek(fd, 0, SEEK_SET);
    let iov = [Iovec { base: x.as_mut_ptr(), len: 3 }, Iovec { base: y.as_mut_ptr(), len: 4 }];
    check_eq!(unsafe { syscall!(nr::READV, fd, iov.as_ptr(), 2) }, 7, "readv");
    check!(&x == b"abc" && &y == b"defg", "readv_content");

    // fstat / fstatat.
    let mut st = Stat::default();
    check_ok!(fstat(fd, &mut st), "fstat");
    check_eq!(st.size, 101, "fstat_size");
    check_eq!(st.mode & S_IFMT, S_IFREG, "fstat_is_reg");
    check_eq!(st.mode & 0o777, 0o640 & !umask(), "fstat_mode_umask");
    check_eq!(st.nlink, 1, "fstat_nlink");
    let mut st2 = Stat::default();
    check_ok!(fstatat(AT_FDCWD, f.as_ptr(), &mut st2, 0), "fstatat");
    check!(st2.ino == st.ino && st2.dev == st.dev, "fstatat_same_inode");
    check_ok!(fstatat(fd, c"".as_ptr() as *const u8, &mut st2, AT_EMPTY_PATH), "fstatat_empty_path");
    check_eq!(st2.ino, st.ino, "fstatat_empty_path_ino");

    // ftruncate shrink and grow.
    check_ok!(ftruncate(fd, 4), "ftruncate_shrink");
    fstat(fd, &mut st);
    check_eq!(st.size, 4, "size_after_shrink");
    ftruncate(fd, 8192);
    check_eq!(pread(fd, &mut buf[..8], 4), 8, "read_after_grow");
    check!(buf[..8].iter().all(|&b| b == 0), "grown_region_zero");
    ftruncate(fd, 0);

    // O_APPEND always writes at the end.
    let fa = check_ok!(open(f.as_ptr(), O_WRONLY | O_APPEND, 0), "open_append") as i32;
    lseek(fd, 0, SEEK_SET);
    write(fd, b"12345");
    lseek(fa, 0, SEEK_SET);
    write(fa, b"END");
    check_eq!(pread(fd, &mut buf[..8], 0), 8, "append_size");
    check_eq!(&buf[..8], b"12345END", "append_content");
    close(fa);

    // O_TRUNC.
    let ft = open(f.as_ptr(), O_WRONLY | O_TRUNC, 0) as i32;
    fstat(ft, &mut st);
    check_eq!(st.size, 0, "o_trunc");
    close(ft);

    // Access mode enforcement.
    let ro = open(f.as_ptr(), O_RDONLY, 0) as i32;
    check_err!(write(ro, b"x"), EBADF, "write_rdonly_ebadf");
    close(ro);
    let wo = open(f.as_ptr(), O_WRONLY, 0) as i32;
    check_err!(read(wo, &mut buf), EBADF, "read_wronly_ebadf");
    close(wo);

    // dup / dup3 / fcntl share the file offset.
    lseek(fd, 0, SEEK_SET);
    let fd2 = check_ok!(dup(fd), "dup") as i32;
    write(fd, b"abcd");
    check_eq!(lseek(fd2, 0, SEEK_CUR), 4, "dup_shares_offset");
    check_eq!(dup3(fd, 50, O_CLOEXEC), 50, "dup3_target");
    check_eq!(fcntl(50, F_GETFD, 0), FD_CLOEXEC as isize, "dup3_cloexec");
    check_err!(dup3(fd, fd, 0), EINVAL, "dup3_same_fd_einval");
    check_eq!(unsafe { syscall!(nr::DUP2, fd, fd) }, fd as isize, "dup2_same_fd_ok");
    let low = fcntl(fd, F_DUPFD, 60);
    check!(low >= 60, "fcntl_dupfd_min", "got {}", low);
    check_eq!(fcntl(fd, F_GETFL, 0) & 3, O_RDWR as isize, "fcntl_getfl_accmode");
    fcntl(fd, F_SETFL, O_APPEND as usize);
    check!(fcntl(fd, F_GETFL, 0) & O_APPEND as isize != 0, "fcntl_setfl_append");
    check_err!(close(1000), EBADF, "close_ebadf");
    close(fd2);
    close(50);
    close(low as i32);

    // Directories.
    let sub = cpath!("{}/sub", d);
    check_ok!(mkdirat(AT_FDCWD, sub.as_ptr(), 0o755), "mkdir");
    check_err!(mkdirat(AT_FDCWD, sub.as_ptr(), 0o755), EEXIST, "mkdir_eexist");
    fstatat(AT_FDCWD, sub.as_ptr(), &mut st, 0);
    check_eq!(st.mode & S_IFMT, S_IFDIR, "stat_is_dir");
    check_err!(open(sub.as_ptr(), O_WRONLY, 0), EISDIR, "open_dir_wronly_eisdir");
    check_err!(open(f.as_ptr(), O_RDONLY | O_DIRECTORY, 0), ENOTDIR, "o_directory_on_file");
    let nested = cpath!("{}/file/x", d);
    check_err!(open(nested.as_ptr(), O_RDONLY, 0), ENOTDIR, "path_through_file_enotdir");
    let missing = cpath!("{}/nope/x", d);
    check_err!(open(missing.as_ptr(), O_RDONLY, 0), ENOENT, "open_enoent");

    // openat relative to a directory fd.
    let dfd = check_ok!(open(dir.as_ptr(), O_RDONLY | O_DIRECTORY, 0), "open_dir") as i32;
    let rel = check_ok!(openat(dfd, c"sub/inner".as_ptr() as *const u8, O_CREAT | O_WRONLY, 0o600), "openat_relative");
    close(rel as i32);
    check_err!(unlinkat(AT_FDCWD, sub.as_ptr(), AT_REMOVEDIR), ENOTEMPTY, "rmdir_notempty");
    check_ok!(unlinkat(dfd, c"sub/inner".as_ptr() as *const u8, 0), "unlinkat_relative");
    check_err!(unlinkat(AT_FDCWD, sub.as_ptr(), 0), EISDIR, "unlink_dir_eisdir");
    check_ok!(unlinkat(AT_FDCWD, sub.as_ptr(), AT_REMOVEDIR), "rmdir");

    // rename (including over an existing file).
    let g = cpath!("{}/renamed", d);
    let h = cpath!("{}/other", d);
    close(open(h.as_ptr(), O_CREAT | O_WRONLY, 0o600) as i32);
    check_ok!(unsafe { syscall!(nr::RENAMEAT, AT_FDCWD, f.as_ptr(), AT_FDCWD, g.as_ptr()) }, "rename");
    check_err!(fstatat(AT_FDCWD, f.as_ptr(), &mut st, 0), ENOENT, "rename_old_gone");
    check_ok!(unsafe { syscall!(nr::RENAMEAT, AT_FDCWD, g.as_ptr(), AT_FDCWD, h.as_ptr()) }, "rename_replace");
    fstatat(AT_FDCWD, h.as_ptr(), &mut st, 0);
    check_eq!(st.size, 4, "rename_replaced_content");

    // Hard links share the inode; unlink keeps open files alive.
    let l = cpath!("{}/hardlink", d);
    check_ok!(unsafe { syscall!(nr::LINKAT, AT_FDCWD, h.as_ptr(), AT_FDCWD, l.as_ptr(), 0) }, "link");
    fstatat(AT_FDCWD, h.as_ptr(), &mut st, 0);
    check_eq!(st.nlink, 2, "link_nlink_2");
    let lfd = open(l.as_ptr(), O_RDONLY, 0) as i32;
    unlinkat(AT_FDCWD, h.as_ptr(), 0);
    unlinkat(AT_FDCWD, l.as_ptr(), 0);
    check_eq!(read(lfd, &mut buf), 4, "read_after_unlink");
    fstat(lfd, &mut st);
    check_eq!(st.nlink, 0, "unlinked_nlink_0");
    close(lfd);

    // Symlinks.
    let target = cpath!("{}/target", d);
    let sl = cpath!("{}/symlink", d);
    let tfd = open(target.as_ptr(), O_CREAT | O_WRONLY, 0o644) as i32;
    write(tfd, b"via-link");
    close(tfd);
    check_ok!(unsafe { syscall!(nr::SYMLINKAT, c"target".as_ptr(), AT_FDCWD, sl.as_ptr()) }, "symlink");
    let n = unsafe { syscall!(nr::READLINKAT, AT_FDCWD, sl.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    check_eq!(n, 6, "readlink_len");
    check_eq!(&buf[..6], b"target", "readlink_content");
    let sfd = check_ok!(open(sl.as_ptr(), O_RDONLY, 0), "open_follows_symlink") as i32;
    check_eq!(read(sfd, &mut buf), 8, "read_through_symlink");
    close(sfd);
    fstatat(AT_FDCWD, sl.as_ptr(), &mut st, AT_SYMLINK_NOFOLLOW);
    check_eq!(st.mode & S_IFMT, S_IFLNK, "lstat_is_link");
    check_err!(open(sl.as_ptr(), O_RDONLY | O_NOFOLLOW, 0), 40, "o_nofollow_eloop");
    unlinkat(AT_FDCWD, sl.as_ptr(), 0);
    unlinkat(AT_FDCWD, target.as_ptr(), 0);

    // getcwd / chdir / fchdir.
    let mut cwd = [0u8; 256];
    check!(unsafe { syscall!(nr::GETCWD, cwd.as_mut_ptr(), cwd.len()) } > 0, "getcwd");
    let old = cwd;
    check_ok!(unsafe { syscall!(nr::CHDIR, dir.as_ptr()) }, "chdir");
    unsafe { syscall!(nr::GETCWD, cwd.as_mut_ptr(), cwd.len()) };
    check_eq!(cfield(&cwd), dir.as_bytes(), "getcwd_after_chdir");
    let rfd = check_ok!(open(c"relfile".as_ptr() as *const u8, O_CREAT | O_WRONLY, 0o600), "open_relative_to_cwd");
    close(rfd as i32);
    check_ok!(fstatat(dfd, c"relfile".as_ptr() as *const u8, &mut st, 0), "relative_created_in_cwd");
    unlinkat(dfd, c"relfile".as_ptr() as *const u8, 0);
    check_err!(unsafe { syscall!(nr::GETCWD, cwd.as_mut_ptr(), 2) }, ERANGE, "getcwd_erange");
    check_ok!(unsafe { syscall!(nr::CHDIR, old.as_ptr()) }, "chdir_back");
    check_ok!(unsafe { syscall!(nr::FCHDIR, dfd) }, "fchdir");
    unsafe { syscall!(nr::CHDIR, old.as_ptr()) };

    // faccessat.
    check_eq!(unsafe { syscall!(nr::FACCESSAT, AT_FDCWD, dir.as_ptr(), 0) }, 0, "access_exists");
    check_err!(unsafe { syscall!(nr::FACCESSAT, AT_FDCWD, missing.as_ptr(), 0) }, ENOENT, "access_enoent");

    // close_range closes everything in the range.
    let a = dup(dfd) as i32;
    let b = dup(dfd) as i32;
    check_ok!(unsafe { syscall!(nr::CLOSE_RANGE, a, b, 0) }, "close_range");
    check!(fcntl(a, F_GETFD, 0) < 0 && fcntl(b, F_GETFD, 0) < 0, "close_range_closed");

    close(fd);
    close(dfd);
    check_ok!(unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR), "cleanup_scratch");
}

fn umask() -> u32 {
    let old = unsafe { syscall!(nr::UMASK, 0o022) } as u32;
    unsafe { syscall!(nr::UMASK, old) };
    old
}
