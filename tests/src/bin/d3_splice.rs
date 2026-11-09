//! D3b: splice(2) as PackageInstaller's `FileUtils.copy` uses it for
//! `adb install`: socket -> pipe -> file in a loop until EOF, plus file ->
//! pipe with an offset, a short return when the pipe holds less than asked,
//! and the errors (no pipe: EINVAL; offset on the pipe side: ESPIPE;
//! wrong direction: EBADF).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("d3_splice", main);

const SPLICE: usize = 275;
const SPLICE_F_MOVE: usize = 1;
const SPLICE_F_MORE: usize = 4;

fn splice(fd_in: i32, off_in: Option<&mut i64>, fd_out: i32, off_out: Option<&mut i64>, len: usize, flags: usize) -> isize {
    let oi = off_in.map_or(0, |o| o as *mut i64 as usize);
    let oo = off_out.map_or(0, |o| o as *mut i64 as usize);
    unsafe { syscall!(SPLICE, fd_in, oi, fd_out, oo, len, flags) }
}

fn main() {
    let dir = scratch_dir();
    let mut data = [0u8; 200_000];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 13 % 251) as u8;
    }

    // A writer process streams `data` into a socket; the parent splices
    // socket -> pipe -> file until EOF, as FileUtils.copyInternalSplice does.
    let mut sv = [0i32; 2];
    socketpair(AF_UNIX, SOCK_STREAM, &mut sv);
    let pid = fork();
    if pid == 0 {
        close(sv[0]);
        write_all(sv[1], &data);
        exit(0);
    }
    close(sv[1]);
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    let a = cpath!("{}/apk", Bytes(dir.as_bytes()));
    let out = open(a.as_ptr(), O_CREAT | O_RDWR | O_TRUNC, 0o644) as i32;
    let (mut total, mut calls, mut err) = (0usize, 0, 0isize);
    loop {
        let n = splice(sv[0], None, p[1], None, 65536, SPLICE_F_MOVE | SPLICE_F_MORE);
        if n <= 0 {
            err = n;
            break;
        }
        let mut left = n as usize;
        while left > 0 {
            let m = splice(p[0], None, out, None, left, SPLICE_F_MOVE | SPLICE_F_MORE);
            if m <= 0 {
                err = m;
                break;
            }
            left -= m as usize;
            total += m as usize;
        }
        calls += 1;
        if err != 0 {
            break;
        }
    }
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    check!(err == 0 && total == data.len() && calls > 1, "socket_pipe_file_copy",
           "{} bytes in {} calls, last {}", total, calls, errname(err));
    let mut back = [0u8; 200_000];
    let n = pread(out, &mut back, 0);
    check!(n == data.len() as isize && back[..] == data[..], "copy_data");
    check_eq!(lseek(out, 0, SEEK_CUR), data.len() as isize, "file_position_moved");
    close(sv[0]);

    // file -> pipe with an explicit input offset: the file position stays.
    lseek(out, 7, SEEK_SET);
    let mut off: i64 = 1000;
    let n = splice(out, Some(&mut off), p[1], None, 3000, 0);
    let mut buf = [0u8; 4096];
    let r = read(p[0], &mut buf);
    check!(n == 3000 && off == 4000 && r == 3000 && buf[..3000] == data[1000..4000] && lseek(out, 0, SEEK_CUR) == 7,
           "file_to_pipe_offset", "{} off {} read {}", errname(n), off, r);

    // pipe -> file at an output offset; a short pipe gives a short count.
    write(p[1], b"spliced");
    let mut oo: i64 = 50;
    let n = splice(p[0], None, out, Some(&mut oo), 100, 0);
    let r = pread(out, &mut buf[..7], 50);
    check!(n == 7 && oo == 57 && r == 7 && &buf[..7] == b"spliced", "pipe_to_file_offset_short",
           "{} off {}", errname(n), oo);

    check_eq!(splice(p[0], None, out, None, 0, 0), 0, "zero_length");
    let b = cpath!("{}/other", Bytes(dir.as_bytes()));
    let out2 = open(b.as_ptr(), O_CREAT | O_RDWR | O_TRUNC, 0o644) as i32;
    check_err!(splice(out, None, out2, None, 10, 0), EINVAL, "no_pipe_einval");
    let mut z: i64 = 0;
    check_err!(splice(p[0], Some(&mut z), out, None, 10, 0), ESPIPE, "offset_on_pipe_espipe");
    check_err!(splice(p[1], None, out, None, 10, 0), EBADF, "read_from_write_end_ebadf");

    // EOF: all writers gone, the pipe is empty.
    close(p[1]);
    check_eq!(splice(p[0], None, out, None, 10, 0), 0, "eof");
    close(p[0]);
    close(out);
    close(out2);
    unlinkat(AT_FDCWD, a.as_ptr(), 0);
    unlinkat(AT_FDCWD, b.as_ptr(), 0);
    unlinkat(AT_FDCWD, dir.as_ptr(), AT_REMOVEDIR);
}
