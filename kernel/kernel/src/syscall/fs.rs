//! File syscalls over the B1 ramfs (see `fs`). Paths relative to a
//! directory fd other than AT_FDCWD are not supported yet (B2).

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::errno::*;
use crate::fs::{self, file::*, ramfs::Inode};
use crate::process::current;

pub const AT_FDCWD: i32 = -100;
const AT_REMOVEDIR: u32 = 0x200;
const AT_EMPTY_PATH: u32 = 0x1000;
const F_DUPFD: u32 = 0;
const F_GETFD: u32 = 1;
const F_SETFD: u32 = 2;
const F_GETFL: u32 = 3;
const F_SETFL: u32 = 4;
const F_DUPFD_CLOEXEC: u32 = 1030;
const FD_CLOEXEC: u64 = 1;
/// Largest single read/write chunk copied through a kernel buffer.
const IO_CHUNK: usize = 64 * 1024;

fn path_arg(dirfd: i32, addr: u64) -> KResult<Vec<u8>> {
    let p = current().mm().read_cstr(addr, 4096)?;
    if dirfd != AT_FDCWD && !p.starts_with(b"/") {
        return Err(ENOSYS);
    }
    Ok(p)
}

fn file(fd: i32) -> KResult<Arc<OpenFile>> {
    current().process.files().get(fd)
}

pub fn sys_read(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    let f = file(fd)?;
    let mut kbuf = vec![0u8; (count as usize).min(IO_CHUNK)];
    let n = f.read(&mut kbuf)?;
    current().mm().copy_out(buf, &kbuf[..n])?;
    Ok(n as u64)
}

pub fn sys_write(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    let f = file(fd)?;
    let mm = current().mm();
    let mut done = 0usize;
    let count = count as usize;
    loop {
        let n = (count - done).min(IO_CHUNK);
        let mut kbuf = vec![0u8; n];
        mm.copy_in(buf + done as u64, &mut kbuf)?;
        let w = match f.write(&kbuf) {
            Ok(w) => w,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        done += w;
        if done >= count || w < n {
            break;
        }
    }
    Ok(done as u64)
}

fn iovecs(iov: u64, cnt: u64) -> KResult<Vec<(u64, u64)>> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    let mut b = vec![0u8; cnt as usize * 16];
    current().mm().copy_in(iov, &mut b)?;
    Ok(b.chunks(16)
        .map(|c| {
            (
                u64::from_le_bytes(c[..8].try_into().unwrap()),
                u64::from_le_bytes(c[8..].try_into().unwrap()),
            )
        })
        .collect())
}

pub fn sys_readv(fd: i32, iov: u64, cnt: u64) -> KResult<u64> {
    let mut total = 0;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let n = sys_read(fd, base, len)?;
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

pub fn sys_writev(fd: i32, iov: u64, cnt: u64) -> KResult<u64> {
    // Gather into one buffer so a line written with writev stays together.
    let f = file(fd)?;
    let mm = current().mm();
    let mut data = Vec::new();
    for (base, len) in iovecs(iov, cnt)? {
        let start = data.len();
        data.resize(start + len as usize, 0);
        mm.copy_in(base, &mut data[start..])?;
    }
    Ok(f.write(&data)? as u64)
}

pub fn sys_openat(dirfd: i32, path: u64, flags: u32, mode: u32) -> KResult<u64> {
    let p = path_arg(dirfd, path)?;
    let f = fs::open(&p, flags, mode)?;
    Ok(current().process.files().alloc(f, flags & O_CLOEXEC != 0, 0)? as u64)
}

pub fn sys_close(fd: i32) -> KResult<u64> {
    current().process.files().close(fd)?;
    Ok(0)
}

pub fn sys_lseek(fd: i32, off: i64, whence: u32) -> KResult<u64> {
    file(fd)?.lseek(off, whence)
}

pub fn sys_dup(fd: i32) -> KResult<u64> {
    let files = current().process.files();
    let f = files.get(fd)?;
    Ok(files.alloc(f, false, 0)? as u64)
}

pub fn sys_dup3(old: i32, new: i32, flags: u32, allow_same: bool) -> KResult<u64> {
    if flags & !O_CLOEXEC != 0 {
        return Err(EINVAL);
    }
    let files = current().process.files();
    let f = files.get(old)?;
    if old == new {
        return if allow_same { Ok(new as u64) } else { Err(EINVAL) };
    }
    files.install(new, f, flags & O_CLOEXEC != 0)?;
    Ok(new as u64)
}

pub fn sys_fcntl(fd: i32, cmd: u32, arg: u64) -> KResult<u64> {
    let files = current().process.files();
    let f = files.get(fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => Ok(files.alloc(f, cmd == F_DUPFD_CLOEXEC, arg as usize)? as u64),
        F_GETFD => Ok(if files.cloexec(fd)? { FD_CLOEXEC } else { 0 }),
        F_SETFD => {
            files.set_cloexec(fd, arg & FD_CLOEXEC != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(f.flags.load(Ordering::Relaxed) as u64),
        F_SETFL => {
            let keep = f.flags.load(Ordering::Relaxed) & !(O_APPEND | O_NONBLOCK);
            f.flags.store(keep | (arg as u32 & (O_APPEND | O_NONBLOCK)), Ordering::Relaxed);
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

pub fn sys_pipe2(fds: u64, flags: u32) -> KResult<u64> {
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Err(EINVAL);
    }
    let p = Pipe::new();
    let r = OpenFile::new(FileKind::PipeRead(p.clone()), O_RDONLY | (flags & O_NONBLOCK));
    let w = OpenFile::new(FileKind::PipeWrite(p), O_WRONLY | (flags & O_NONBLOCK));
    let t = current();
    let files = t.process.files();
    let cloexec = flags & O_CLOEXEC != 0;
    let rfd = files.alloc(r, cloexec, 0)?;
    let wfd = match files.alloc(w, cloexec, 0) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = files.close(rfd);
            return Err(e);
        }
    };
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&rfd.to_le_bytes());
    b[4..].copy_from_slice(&wfd.to_le_bytes());
    if let Err(e) = t.mm().copy_out(fds, &b) {
        let _ = files.close(rfd);
        let _ = files.close(wfd);
        return Err(e);
    }
    Ok(0)
}

pub fn sys_mkdirat(dirfd: i32, path: u64, mode: u32) -> KResult<u64> {
    fs::mkdir(&path_arg(dirfd, path)?, mode)?;
    Ok(0)
}

pub fn sys_unlinkat(dirfd: i32, path: u64, flags: u32) -> KResult<u64> {
    if flags & !AT_REMOVEDIR != 0 {
        return Err(EINVAL);
    }
    fs::unlink(&path_arg(dirfd, path)?, flags & AT_REMOVEDIR != 0)?;
    Ok(0)
}

fn stat_bytes(i: &Inode) -> [u8; 144] {
    let mut b = [0u8; 144];
    let size = i.size() as u64;
    let put = |b: &mut [u8; 144], off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, 1); // st_dev
    put(&mut b, 8, i.ino);
    put(&mut b, 16, if i.is_dir() { 2 } else { 1 });
    b[24..28].copy_from_slice(&i.mode.to_le_bytes());
    put(&mut b, 40, ((i.rdev.0 as u64) << 8) | i.rdev.1 as u64);
    put(&mut b, 48, size);
    put(&mut b, 56, 4096);
    put(&mut b, 64, size.div_ceil(512));
    b
}

fn file_stat(f: &OpenFile) -> [u8; 144] {
    match &f.kind {
        FileKind::Inode(i) => stat_bytes(i),
        FileKind::PipeRead(_) | FileKind::PipeWrite(_) => {
            let mut b = [0u8; 144];
            b[24..28].copy_from_slice(&(0o010600u32).to_le_bytes());
            b
        }
        _ => {
            let mut b = [0u8; 144];
            b[24..28].copy_from_slice(&(0o020666u32).to_le_bytes());
            b
        }
    }
}

pub fn sys_fstat(fd: i32, st: u64) -> KResult<u64> {
    let b = file_stat(&*file(fd)?);
    current().mm().copy_out(st, &b)?;
    Ok(0)
}

pub fn sys_newfstatat(dirfd: i32, path: u64, st: u64, flags: u32) -> KResult<u64> {
    let mm = current().mm();
    let p = mm.read_cstr(path, 4096)?;
    let b = if p.is_empty() && flags & AT_EMPTY_PATH != 0 {
        file_stat(&*file(dirfd)?)
    } else {
        if dirfd != AT_FDCWD && !p.starts_with(b"/") {
            return Err(ENOSYS);
        }
        stat_bytes(&*fs::lookup(&p)?)
    };
    mm.copy_out(st, &b)?;
    Ok(0)
}
