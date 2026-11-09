//! File syscalls over the VFS (`crate::fs`). Paths are resolved relative
//! to the process cwd or a directory fd (`*at` calls).

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::errno::*;
use crate::fs::file::*;
use crate::fs::pipe::{self, PIPE_CAPACITY, PipeReader, PipeWriter};
use crate::fs::vfs::{self, Inode, PathRef, S_IFCHR, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG};
use crate::fs::{self, lock};
use crate::mm::shmem;
use crate::process::current;

pub const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
const AT_REMOVEDIR: u32 = 0x200;
const AT_EACCESS: u32 = 0x200;
const AT_SYMLINK_FOLLOW: u32 = 0x400;
const AT_NO_AUTOMOUNT: u32 = 0x800;
const AT_EMPTY_PATH: u32 = 0x1000;
const AT_STATX_SYNC_TYPE: u32 = 0x6000;
const F_DUPFD: u32 = 0;
const F_GETFD: u32 = 1;
const F_SETFD: u32 = 2;
const F_GETFL: u32 = 3;
const F_SETFL: u32 = 4;
const F_GETLK: u32 = 5;
const F_SETLK: u32 = 6;
const F_SETLKW: u32 = 7;
const F_OFD_GETLK: u32 = 36;
const F_OFD_SETLK: u32 = 37;
const F_OFD_SETLKW: u32 = 38;
const F_DUPFD_CLOEXEC: u32 = 1030;
const F_SETPIPE_SZ: u32 = 1031;
const F_GETPIPE_SZ: u32 = 1032;
const F_ADD_SEALS: u32 = 1033;
const F_GET_SEALS: u32 = 1034;
const MFD_CLOEXEC: u32 = 1;
const MFD_ALLOW_SEALING: u32 = 2;
const MFD_NOEXEC_SEAL: u32 = 8;
const MFD_EXEC: u32 = 0x10;
/// Longest memfd name: NAME_MAX minus "memfd:".
const MFD_NAME_MAX: usize = 249;
const FD_CLOEXEC: u64 = 1;
const F_UNLCK: u16 = 2;
const FIONCLEX: u32 = 0x5450;
const FIOCLEX: u32 = 0x5451;
const FIONBIO: u32 = 0x5421;
const CLOSE_RANGE_UNSHARE: u32 = 2;
const CLOSE_RANGE_CLOEXEC: u32 = 4;
const UTIME_NOW: i64 = (1 << 30) - 1;
const UTIME_OMIT: i64 = (1 << 30) - 2;
/// Largest single read/write chunk copied through a kernel buffer.
const IO_CHUNK: usize = 64 * 1024;

fn file(fd: i32) -> KResult<Arc<OpenFile>> {
    current().process.files().get(fd)
}

/// A file usable for I/O (not O_PATH).
fn io_file(fd: i32) -> KResult<Arc<OpenFile>> {
    let f = file(fd)?;
    if f.is_path_only() { Err(EBADF) } else { Ok(f) }
}

fn user_path(addr: u64) -> KResult<Vec<u8>> {
    if addr == 0 {
        return Err(EFAULT);
    }
    current().mm().read_cstr(addr, vfs::PATH_MAX - 1)
}

/// The directory a relative path starts from.
fn base_dir(dirfd: i32) -> KResult<PathRef> {
    if dirfd == AT_FDCWD {
        return Ok(current().process.cwd());
    }
    let f = file(dirfd)?;
    match &f.path {
        Some(p) if p.is_dir() => Ok(p.clone()),
        _ => Err(ENOTDIR),
    }
}

/// Resolve (dirfd, path) for *at calls taking AT_EMPTY_PATH /
/// AT_SYMLINK_NOFOLLOW. Returns the inode and, when known, its path.
fn resolve_at(dirfd: i32, path: &[u8], flags: u32) -> KResult<(Arc<dyn Inode>, Option<PathRef>)> {
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(ENOENT);
        }
        if dirfd == AT_FDCWD {
            let cwd = current().process.cwd();
            return Ok((cwd.inode().clone(), Some(cwd)));
        }
        let f = file(dirfd)?;
        return Ok((f.inode.clone(), f.path.clone()));
    }
    let base = if path[0] == b'/' { vfs::root() } else { base_dir(dirfd)? };
    let p = vfs::lookup(&base, path, flags & AT_SYMLINK_NOFOLLOW == 0)?;
    Ok((p.inode().clone(), Some(p)))
}

/// (directory, last name, trailing slash) for creating/removing calls.
fn parent_at(dirfd: i32, path: &[u8]) -> KResult<(PathRef, Vec<u8>, bool)> {
    let base = if path.first() == Some(&b'/') { vfs::root() } else { base_dir(dirfd)? };
    vfs::lookup_parent(&base, path)
}

fn umask() -> u32 {
    current().process.fs.lock().umask
}

// ---------------------------------------------------------------------------
// read / write

fn read_into(f: &OpenFile, buf: u64, count: u64, at: Option<u64>) -> KResult<u64> {
    let mm = current().mm();
    let count = count as usize;
    let mut done = 0usize;
    let mut kbuf = vec![0u8; count.min(IO_CHUNK)];
    loop {
        let n = (count - done).min(IO_CHUNK);
        let r = match f.read(&mut kbuf[..n], at.map(|a| a + done as u64)) {
            Ok(r) => r,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        mm.copy_out(buf + done as u64, &kbuf[..r])?;
        done += r;
        // Only regular files are read in several chunks: a second read of a
        // pipe or device could block.
        if done >= count || r < n || !f.ops.seekable() {
            break;
        }
    }
    Ok(done as u64)
}

fn write_from(f: &OpenFile, buf: u64, count: u64, at: Option<u64>) -> KResult<u64> {
    let mm = current().mm();
    let count = count as usize;
    let mut done = 0usize;
    loop {
        let n = (count - done).min(IO_CHUNK);
        let mut kbuf = vec![0u8; n];
        mm.copy_in(buf + done as u64, &mut kbuf)?;
        let w = match f.write(&kbuf, at.map(|a| a + done as u64)) {
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

pub fn sys_read(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    read_into(&*io_file(fd)?, buf, count, None)
}

pub fn sys_write(fd: i32, buf: u64, count: u64) -> KResult<u64> {
    write_from(&*io_file(fd)?, buf, count, None)
}

pub fn sys_pread64(fd: i32, buf: u64, count: u64, off: i64) -> KResult<u64> {
    if off < 0 {
        return Err(EINVAL);
    }
    read_into(&*io_file(fd)?, buf, count, Some(off as u64))
}

pub fn sys_pwrite64(fd: i32, buf: u64, count: u64, off: i64) -> KResult<u64> {
    if off < 0 {
        return Err(EINVAL);
    }
    write_from(&*io_file(fd)?, buf, count, Some(off as u64))
}

pub(super) fn iovecs(iov: u64, cnt: u64) -> KResult<Vec<(u64, u64)>> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    let mut b = vec![0u8; cnt as usize * 16];
    current().mm().copy_in(iov, &mut b)?;
    let v: Vec<(u64, u64)> = b
        .chunks(16)
        .map(|c| (u64::from_le_bytes(c[..8].try_into().unwrap()), u64::from_le_bytes(c[8..].try_into().unwrap())))
        .collect();
    if v.iter().any(|&(_, len)| len > isize::MAX as u64) {
        return Err(EINVAL);
    }
    Ok(v)
}

fn readv_at(fd: i32, iov: u64, cnt: u64, at: Option<u64>) -> KResult<u64> {
    let f = io_file(fd)?;
    let iov = iovecs(iov, cnt)?;
    // One receive per call on a socket: a datagram is not split over reads.
    if let Ok(s) = crate::net::unix::socket_of(&f) {
        if at.is_some() {
            return Err(ESPIPE);
        }
        return super::net::readv(&f, &s, &iov);
    }
    let mut total = 0;
    for (base, len) in iov {
        if len == 0 {
            continue;
        }
        let n = match read_into(&f, base, len, at.map(|a| a + total)) {
            Ok(n) => n,
            Err(e) if total == 0 => return Err(e),
            Err(_) => break,
        };
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

fn writev_at(fd: i32, iov: u64, cnt: u64, at: Option<u64>) -> KResult<u64> {
    // Gather into one buffer so a line written with writev stays together.
    let f = io_file(fd)?;
    let mm = current().mm();
    let mut data = Vec::new();
    for (base, len) in iovecs(iov, cnt)? {
        let start = data.len();
        data.try_reserve(len as usize).map_err(|_| ENOMEM)?;
        data.resize(start + len as usize, 0);
        mm.copy_in(base, &mut data[start..])?;
    }
    Ok(f.write(&data, at)? as u64)
}

pub fn sys_readv(fd: i32, iov: u64, cnt: u64) -> KResult<u64> {
    readv_at(fd, iov, cnt, None)
}

pub fn sys_writev(fd: i32, iov: u64, cnt: u64) -> KResult<u64> {
    writev_at(fd, iov, cnt, None)
}

pub fn sys_preadv(fd: i32, iov: u64, cnt: u64, off: i64) -> KResult<u64> {
    if off < 0 {
        return Err(EINVAL);
    }
    readv_at(fd, iov, cnt, Some(off as u64))
}

pub fn sys_pwritev(fd: i32, iov: u64, cnt: u64, off: i64) -> KResult<u64> {
    if off < 0 {
        return Err(EINVAL);
    }
    writev_at(fd, iov, cnt, Some(off as u64))
}

pub fn sys_lseek(fd: i32, off: i64, whence: u32) -> KResult<u64> {
    io_file(fd)?.lseek(off, whence)
}

// ---------------------------------------------------------------------------
// open / close / dup / fcntl / ioctl

pub fn sys_openat(dirfd: i32, path: u64, flags: u32, mode: u32) -> KResult<u64> {
    let p = user_path(path)?;
    let base = if p.first() == Some(&b'/') { vfs::root() } else { base_dir(dirfd)? };
    let f = fs::open(&base, &p, flags, mode & 0o7777 & !umask())?;
    Ok(current().process.files().alloc(f, flags & O_CLOEXEC != 0, 0)? as u64)
}

pub fn sys_close(fd: i32) -> KResult<u64> {
    current().process.files().close(fd)?;
    Ok(0)
}

pub fn sys_close_range(first: u32, last: u32, flags: u32) -> KResult<u64> {
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 || first > last {
        return Err(EINVAL);
    }
    let p = &current().process;
    if flags & CLOSE_RANGE_UNSHARE != 0 {
        let mut files = p.files.lock();
        if Arc::strong_count(&files) > 1 {
            let copy = files.duplicate();
            *files = copy;
        }
    }
    p.files().close_range(first, last, flags & CLOSE_RANGE_CLOEXEC != 0);
    Ok(0)
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

fn pipe_capacity(f: &OpenFile) -> Option<u64> {
    let any = f.ops.as_any();
    (any.is::<PipeReader>() || any.is::<PipeWriter>()).then_some(PIPE_CAPACITY as u64)
}

pub fn sys_fcntl(fd: i32, cmd: u32, arg: u64) -> KResult<u64> {
    let files = current().process.files();
    let f = files.get(fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let min = usize::try_from(arg as i32).map_err(|_| EINVAL)?;
            Ok(files.alloc(f, cmd == F_DUPFD_CLOEXEC, min)? as u64)
        }
        F_GETFD => Ok(if files.cloexec(fd)? { FD_CLOEXEC } else { 0 }),
        F_SETFD => {
            files.set_cloexec(fd, arg & FD_CLOEXEC != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(f.flags() as u64),
        F_SETFL => {
            if f.is_path_only() {
                return Err(EBADF);
            }
            let keep = f.flags() & !SETFL_FLAGS;
            f.flags.store(keep | (arg as u32 & SETFL_FLAGS), Ordering::Relaxed);
            Ok(0)
        }
        // Open file description locks are real (`fs::lock`).
        F_OFD_SETLK | F_OFD_SETLKW | F_OFD_GETLK => {
            let mut b = [0u8; 32];
            current().mm().copy_in(arg, &mut b)?;
            let r = lock::Request::parse(&f, &b)?;
            if cmd != F_OFD_GETLK {
                lock::set(&f, &r, cmd == F_OFD_SETLKW)?;
                return Ok(0);
            }
            match lock::test(&f, &r) {
                Some((ty, start, end)) => {
                    b[0..2].copy_from_slice(&ty.to_ne_bytes());
                    b[2..4].copy_from_slice(&0i16.to_ne_bytes());
                    b[8..16].copy_from_slice(&start.to_ne_bytes());
                    let len = if end == u64::MAX { 0 } else { end - start };
                    b[16..24].copy_from_slice(&len.to_ne_bytes());
                    b[24..28].copy_from_slice(&(-1i32).to_ne_bytes());
                }
                None => b[0..2].copy_from_slice(&lock::F_UNLCK.to_ne_bytes()),
            }
            current().mm().copy_out(arg, &b)?;
            Ok(0)
        }
        // Classic record locks: no contention model yet; every lock is
        // granted and F_GETLK reports "unlocked".
        F_SETLK | F_SETLKW => Ok(0),
        F_GETLK => {
            current().mm().copy_out(arg, &F_UNLCK.to_le_bytes())?;
            Ok(0)
        }
        F_GETPIPE_SZ | F_SETPIPE_SZ => pipe_capacity(&f).ok_or(EBADF),
        F_ADD_SEALS => {
            let obj = f.inode.shmem().ok_or(EINVAL)?;
            if !f.writable() {
                return Err(EPERM);
            }
            obj.add_seals(arg as u32)?;
            Ok(0)
        }
        F_GET_SEALS => Ok(f.inode.shmem().ok_or(EINVAL)?.seals() as u64),
        _ => Err(EINVAL),
    }
}

pub fn sys_ioctl(fd: i32, cmd: u32, arg: u64) -> KResult<u64> {
    let files = current().process.files();
    let f = files.get(fd)?;
    match cmd {
        FIOCLEX | FIONCLEX => {
            files.set_cloexec(fd, cmd == FIOCLEX)?;
            Ok(0)
        }
        FIONBIO => {
            let mut b = [0u8; 4];
            current().mm().copy_in(arg, &mut b)?;
            if u32::from_le_bytes(b) != 0 {
                f.flags.fetch_or(O_NONBLOCK, Ordering::Relaxed);
            } else {
                f.flags.fetch_and(!O_NONBLOCK, Ordering::Relaxed);
            }
            Ok(0)
        }
        _ => f.ioctl(cmd, arg),
    }
}

pub fn sys_pipe2(fds: u64, flags: u32) -> KResult<u64> {
    if flags & !(O_CLOEXEC | O_NONBLOCK | O_DIRECT) != 0 {
        return Err(EINVAL);
    }
    let (r, w) = pipe::new_pipe(flags);
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

/// `fadvise64`: hints only (everything is in RAM).
pub fn sys_fadvise64(fd: i32, advice: u32) -> KResult<u64> {
    let f = io_file(fd)?;
    if f.inode.meta().file_type() == vfs::S_IFIFO || crate::net::unix::socket_of(&f).is_ok() {
        return Err(ESPIPE);
    }
    if advice > 5 {
        return Err(EINVAL);
    }
    Ok(0)
}

pub fn sys_fsync(fd: i32) -> KResult<u64> {
    file(fd)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// stat family

fn stat_bytes(i: &dyn Inode) -> [u8; 144] {
    let mut b = [0u8; 144];
    let m = i.meta();
    let a = m.attr();
    let size = i.size();
    let put = |b: &mut [u8; 144], off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, m.dev);
    put(&mut b, 8, m.ino);
    put(&mut b, 16, a.nlink as u64);
    b[24..28].copy_from_slice(&a.mode.to_le_bytes());
    b[28..32].copy_from_slice(&a.uid.to_le_bytes());
    b[32..36].copy_from_slice(&a.gid.to_le_bytes());
    put(&mut b, 40, m.rdev);
    put(&mut b, 48, size);
    put(&mut b, 56, 4096);
    put(&mut b, 64, size.div_ceil(512));
    for (off, t) in [(72, a.atime), (88, a.mtime), (104, a.ctime)] {
        put(&mut b, off, t.div_euclid(1_000_000_000) as u64);
        put(&mut b, off + 8, t.rem_euclid(1_000_000_000) as u64);
    }
    b
}

pub fn sys_fstat(fd: i32, st: u64) -> KResult<u64> {
    let b = stat_bytes(&*file(fd)?.inode);
    current().mm().copy_out(st, &b)?;
    Ok(0)
}

pub fn sys_newfstatat(dirfd: i32, path: u64, st: u64, flags: u32) -> KResult<u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH | AT_NO_AUTOMOUNT) != 0 {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, _) = resolve_at(dirfd, &p, flags)?;
    current().mm().copy_out(st, &stat_bytes(&*inode))?;
    Ok(0)
}

pub fn sys_stat(path: u64, st: u64, follow: bool) -> KResult<u64> {
    sys_newfstatat(AT_FDCWD, path, st, if follow { 0 } else { AT_SYMLINK_NOFOLLOW })
}

const STATX_BASIC_STATS: u32 = 0x7ff;

pub fn sys_statx(dirfd: i32, path: u64, flags: u32, _mask: u32, buf: u64) -> KResult<u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_STATX_SYNC_TYPE) != 0
        || flags & AT_STATX_SYNC_TYPE == AT_STATX_SYNC_TYPE
    {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, _) = resolve_at(dirfd, &p, flags)?;
    let m = inode.meta();
    let a = m.attr();
    let size = inode.size();
    let mut b = [0u8; 256];
    let put32 = |b: &mut [u8; 256], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8; 256], off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut b, 0, STATX_BASIC_STATS);
    put32(&mut b, 4, 4096);
    put32(&mut b, 16, a.nlink);
    put32(&mut b, 20, a.uid);
    put32(&mut b, 24, a.gid);
    b[28..30].copy_from_slice(&(a.mode as u16).to_le_bytes());
    put64(&mut b, 32, m.ino);
    put64(&mut b, 40, size);
    put64(&mut b, 48, size.div_ceil(512));
    // atime 64, btime 80 (not reported), ctime 96, mtime 112.
    for (off, t) in [(64, a.atime), (96, a.ctime), (112, a.mtime)] {
        put64(&mut b, off, t.div_euclid(1_000_000_000) as u64);
        put32(&mut b, off + 8, t.rem_euclid(1_000_000_000) as u32);
    }
    put32(&mut b, 128, vfs::dev_major(m.rdev));
    put32(&mut b, 132, vfs::dev_minor(m.rdev));
    put32(&mut b, 136, vfs::dev_major(m.dev));
    put32(&mut b, 140, vfs::dev_minor(m.dev));
    current().mm().copy_out(buf, &b)?;
    Ok(0)
}

pub fn sys_faccessat(dirfd: i32, path: u64, mode: u32, flags: u32) -> KResult<u64> {
    if mode & !7 != 0 || flags & !(AT_EACCESS | AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, _) = resolve_at(dirfd, &p, flags)?;
    // Permissions are not checked (credentials are only tracked): only
    // execute permission can be missing.
    let a = inode.meta().attr();
    if mode & 1 != 0 && a.mode & S_IFMT != S_IFDIR && a.mode & 0o111 == 0 {
        return Err(EACCES);
    }
    Ok(0)
}

const STATFS_SIZE: usize = 120;

fn statfs_bytes(fstype: &str) -> [u8; STATFS_SIZE] {
    statfs_of(fstype, None, 0)
}

/// statfs for a mount: disk file systems report their own numbers.
fn statfs_mount(m: &vfs::Mount) -> [u8; STATFS_SIZE] {
    statfs_of(m.fstype, m.root.inode.statfs(), m.flags)
}

fn statfs_of(fstype: &str, disk: Option<vfs::StatFs>, mnt_flags: u64) -> [u8; STATFS_SIZE] {
    if let Some(d) = disk {
        let mut b = [0u8; STATFS_SIZE];
        let vals = [d.magic, d.bsize, d.blocks, d.bfree, d.bavail, d.files, d.ffree];
        for (i, v) in vals.iter().enumerate() {
            b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        b[64..72].copy_from_slice(&d.namelen.to_le_bytes());
        b[72..80].copy_from_slice(&d.bsize.to_le_bytes());
        b[80..88].copy_from_slice(&(d.flags | (mnt_flags & 1)).to_le_bytes());
        return b;
    }
    let mut b = [0u8; STATFS_SIZE];
    let (total, free) = crate::mm::frame::stats();
    let vals: [u64; 7] = [
        fs::fs_magic(fstype),
        4096,
        total as u64,
        free as u64,
        free as u64,
        1 << 20,
        1 << 19,
    ];
    for (i, v) in vals.iter().enumerate() {
        b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    // f_fsid (8 bytes at 56) stays 0; f_namelen, f_frsize, f_flags.
    b[64..72].copy_from_slice(&(vfs::NAME_MAX as u64).to_le_bytes());
    b[72..80].copy_from_slice(&4096u64.to_le_bytes());
    b
}

pub fn sys_statfs(path: u64, buf: u64) -> KResult<u64> {
    let p = user_path(path)?;
    let at = vfs::lookup(&current().process.cwd(), &p, true)?;
    current().mm().copy_out(buf, &statfs_mount(&at.mnt))?;
    Ok(0)
}

pub fn sys_fstatfs(fd: i32, buf: u64) -> KResult<u64> {
    let f = file(fd)?;
    let ty = f.path.as_ref().map_or("pipefs", |p| p.mnt.fstype);
    let mut b = match &f.path {
        Some(p) => statfs_mount(&p.mnt),
        None => statfs_bytes(ty),
    };
    if ty == "pipefs" {
        b[..8].copy_from_slice(&0x5049_5045u64.to_le_bytes());
    }
    current().mm().copy_out(buf, &b)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Directories and names

pub fn sys_getdents64(fd: i32, dirp: u64, count: u64) -> KResult<u64> {
    let f = io_file(fd)?;
    if !f.inode.meta().is_dir() {
        return Err(ENOTDIR);
    }
    let mut entries = Vec::new();
    let self_ino = f.inode.meta().ino;
    let parent_ino = f
        .path
        .as_ref()
        .map_or(self_ino, |p| vfs::parent_of(&vfs::root(), p).inode().meta().ino);
    entries.push(vfs::DirEntry { name: b".".to_vec(), ino: self_ino, dtype: vfs::DT_DIR });
    entries.push(vfs::DirEntry { name: b"..".to_vec(), ino: parent_ino, dtype: vfs::DT_DIR });
    entries.extend(f.inode.entries()?);

    let count = (count as usize).min(IO_CHUNK);
    let mut out = Vec::with_capacity(count);
    let mut pos = f.pos() as usize;
    while let Some(e) = entries.get(pos) {
        let reclen = (19 + e.name.len() + 1).next_multiple_of(8);
        if out.len() + reclen > count {
            if out.is_empty() {
                return Err(EINVAL);
            }
            break;
        }
        out.extend_from_slice(&e.ino.to_le_bytes());
        out.extend_from_slice(&((pos + 1) as i64).to_le_bytes());
        out.extend_from_slice(&(reclen as u16).to_le_bytes());
        out.push(e.dtype);
        out.extend_from_slice(&e.name);
        out.resize(out.len() + reclen - 19 - e.name.len(), 0);
        pos += 1;
    }
    current().mm().copy_out(dirp, &out)?;
    f.set_pos(pos as u64);
    Ok(out.len() as u64)
}

pub fn sys_mknodat(dirfd: i32, path: u64, mode: u32, dev: u64) -> KResult<u64> {
    let p = user_path(path)?;
    let ty = match mode & S_IFMT {
        0 => S_IFREG,
        S_IFREG | S_IFCHR | vfs::S_IFBLK | vfs::S_IFIFO | vfs::S_IFSOCK => mode & S_IFMT,
        S_IFDIR => return Err(EPERM),
        _ => return Err(EINVAL),
    };
    let (dir, name, trailing) = parent_at(dirfd, &p)?;
    if trailing {
        return Err(ENOENT);
    }
    let rdev = vfs::encode_dev(((dev >> 8) & 0xfff) as u32, ((dev & 0xff) | ((dev >> 12) & !0xff)) as u32);
    vfs::create(&dir, &name, ty | (mode & 0o7777 & !umask()), rdev)?;
    Ok(0)
}

pub fn sys_mkdirat(dirfd: i32, path: u64, mode: u32) -> KResult<u64> {
    let p = user_path(path)?;
    let (dir, name, _) = parent_at(dirfd, &p)?;
    vfs::create(&dir, &name, S_IFDIR | (mode & 0o7777 & !umask()), 0)?;
    Ok(0)
}

pub fn sys_unlinkat(dirfd: i32, path: u64, flags: u32) -> KResult<u64> {
    if flags & !AT_REMOVEDIR != 0 {
        return Err(EINVAL);
    }
    let rmdir = flags & AT_REMOVEDIR != 0;
    let p = user_path(path)?;
    let (dir, name, trailing) = parent_at(dirfd, &p)?;
    if trailing && !rmdir {
        let t = vfs::child_of(&dir, &name)?;
        return Err(if t.is_dir() { EISDIR } else { ENOTDIR });
    }
    vfs::unlink(&dir, &name, rmdir)?;
    Ok(0)
}

pub fn sys_renameat2(olddirfd: i32, old: u64, newdirfd: i32, new: u64, flags: u32) -> KResult<u64> {
    let (op, np) = (user_path(old)?, user_path(new)?);
    let (od, oname, otrail) = parent_at(olddirfd, &op)?;
    let (nd, nname, ntrail) = parent_at(newdirfd, &np)?;
    if (otrail || ntrail) && !vfs::child_of(&od, &oname)?.is_dir() {
        return Err(ENOTDIR);
    }
    vfs::rename(&od, &oname, &nd, &nname, flags)?;
    Ok(0)
}

pub fn sys_linkat(olddirfd: i32, old: u64, newdirfd: i32, new: u64, flags: u32) -> KResult<u64> {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    let (op, np) = (user_path(old)?, user_path(new)?);
    let src = if op.is_empty() && flags & AT_EMPTY_PATH != 0 {
        file(olddirfd)?.path.clone().ok_or(ENOENT)?
    } else {
        let base = if op.first() == Some(&b'/') { vfs::root() } else { base_dir(olddirfd)? };
        vfs::lookup(&base, &op, flags & AT_SYMLINK_FOLLOW != 0)?
    };
    let (dir, name, trailing) = parent_at(newdirfd, &np)?;
    if trailing {
        return Err(ENOENT);
    }
    vfs::link(&src, &dir, &name)?;
    Ok(0)
}

pub fn sys_symlinkat(target: u64, newdirfd: i32, path: u64) -> KResult<u64> {
    let t = user_path(target)?;
    let p = user_path(path)?;
    let (dir, name, trailing) = parent_at(newdirfd, &p)?;
    if trailing {
        return Err(ENOENT);
    }
    vfs::symlink(&dir, &name, &t)?;
    Ok(0)
}

pub fn sys_readlinkat(dirfd: i32, path: u64, buf: u64, size: i32) -> KResult<u64> {
    if size <= 0 {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, _) = resolve_at(dirfd, &p, AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH)?;
    if inode.meta().file_type() != S_IFLNK {
        return Err(EINVAL);
    }
    let target = inode.readlink()?;
    let n = target.len().min(size as usize);
    current().mm().copy_out(buf, &target[..n])?;
    Ok(n as u64)
}

pub fn sys_chdir(path: u64) -> KResult<u64> {
    let p = user_path(path)?;
    let t = current();
    let d = vfs::lookup(&t.process.cwd(), &p, true)?;
    if !d.is_dir() {
        return Err(ENOTDIR);
    }
    t.process.fs.lock().cwd = d;
    Ok(0)
}

pub fn sys_fchdir(fd: i32) -> KResult<u64> {
    let f = file(fd)?;
    let d = f.path.clone().filter(|p| p.is_dir()).ok_or(ENOTDIR)?;
    current().process.fs.lock().cwd = d;
    Ok(0)
}

pub fn sys_getcwd(buf: u64, size: u64) -> KResult<u64> {
    let t = current();
    let cwd = t.process.cwd();
    if cwd.dentry.is_deleted() {
        return Err(ENOENT);
    }
    let mut path = vfs::path_of(&cwd);
    path.push(0);
    if (size as usize) < path.len() {
        return Err(ERANGE);
    }
    t.mm().copy_out(buf, &path)?;
    Ok(path.len() as u64)
}

// ---------------------------------------------------------------------------
// Attributes

pub fn sys_truncate(path: u64, len: i64) -> KResult<u64> {
    let p = user_path(path)?;
    let at = vfs::lookup(&current().process.cwd(), &p, true)?;
    truncate_inode(at.inode(), len)
}

pub fn sys_ftruncate(fd: i32, len: i64) -> KResult<u64> {
    let f = io_file(fd)?;
    if f.flags() & O_ACCMODE == O_RDONLY {
        return Err(EINVAL);
    }
    truncate_inode(&f.inode, len)
}

fn truncate_inode(inode: &Arc<dyn Inode>, len: i64) -> KResult<u64> {
    if len < 0 {
        return Err(EINVAL);
    }
    match inode.meta().file_type() {
        S_IFREG => {}
        S_IFDIR => return Err(EISDIR),
        _ => return Err(EINVAL),
    }
    inode.truncate(len as u64)?;
    Ok(0)
}

pub fn sys_fallocate(fd: i32, mode: u32, off: i64, len: i64) -> KResult<u64> {
    let f = io_file(fd)?;
    if off < 0 || len <= 0 {
        return Err(EINVAL);
    }
    if !f.writable() {
        return Err(EBADF);
    }
    match f.inode.meta().file_type() {
        S_IFREG => {}
        S_IFDIR => return Err(EISDIR),
        vfs::S_IFIFO => return Err(ESPIPE),
        _ => return Err(ENODEV),
    }
    f.inode.shmem().ok_or(EOPNOTSUPP)?.fallocate(mode, off as u64, len as u64)?;
    f.inode.meta().touch_mtime();
    Ok(0)
}

/// `memfd_create(2)`: an unnamed tmpfs file, shown as
/// "/memfd:NAME (deleted)".
pub fn sys_memfd_create(name: u64, flags: u32) -> KResult<u64> {
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL | MFD_EXEC) != 0
        || flags & (MFD_NOEXEC_SEAL | MFD_EXEC) == MFD_NOEXEC_SEAL | MFD_EXEC
    {
        return Err(EINVAL);
    }
    let name = current().mm().read_cstr(name, MFD_NAME_MAX).map_err(|e| if e == EFAULT { e } else { EINVAL })?;
    if name.len() > MFD_NAME_MAX {
        return Err(EINVAL);
    }
    let (mode, seals) = if flags & MFD_NOEXEC_SEAL != 0 {
        (0o666, shmem::F_SEAL_EXEC)
    } else if flags & MFD_ALLOW_SEALING != 0 {
        (0o777, 0)
    } else {
        (0o777, shmem::F_SEAL_SEAL)
    };
    let inode = fs::tmpfs::TmpInode::new_memfd(&name, mode, seals);
    let file = OpenFile::new(inode, None, alloc::boxed::Box::new(RegularFile), O_RDWR);
    Ok(current().process.files().alloc(file, flags & MFD_CLOEXEC != 0, 0)? as u64)
}

pub fn sys_umask(mask: u32) -> KResult<u64> {
    let t = current();
    let mut fs = t.process.fs.lock();
    let old = fs.umask;
    fs.umask = mask & 0o777;
    Ok(old as u64)
}

fn chmod_inode(inode: &dyn Inode, path: Option<&PathRef>, mode: u32) -> KResult<u64> {
    inode.meta().update(|a| {
        a.mode = (a.mode & S_IFMT) | (mode & 0o7777);
        a.ctime = vfs::now();
    });
    crate::fs::inotify::touched(inode, path, crate::fs::inotify::IN_ATTRIB);
    Ok(0)
}

pub fn sys_fchmodat(dirfd: i32, path: u64, mode: u32, flags: u32) -> KResult<u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, path) = resolve_at(dirfd, &p, flags)?;
    if flags & AT_SYMLINK_NOFOLLOW != 0 && inode.meta().file_type() == S_IFLNK {
        return Err(EOPNOTSUPP);
    }
    chmod_inode(&*inode, path.as_ref(), mode)
}

pub fn sys_fchmod(fd: i32, mode: u32) -> KResult<u64> {
    let f = file(fd)?;
    chmod_inode(&*f.inode, f.path.as_ref(), mode)
}

/// Ownership is stored only (credentials are never checked).
fn chown_inode(inode: &dyn Inode, path: Option<&PathRef>, uid: u32, gid: u32) -> KResult<u64> {
    inode.meta().update(|a| {
        if uid != u32::MAX {
            a.uid = uid;
        }
        if gid != u32::MAX {
            a.gid = gid;
        }
        a.ctime = vfs::now();
    });
    crate::fs::inotify::touched(inode, path, crate::fs::inotify::IN_ATTRIB);
    Ok(0)
}

pub fn sys_fchownat(dirfd: i32, path: u64, uid: u32, gid: u32, flags: u32) -> KResult<u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    let p = user_path(path)?;
    let (inode, path) = resolve_at(dirfd, &p, flags)?;
    chown_inode(&*inode, path.as_ref(), uid, gid)
}

pub fn sys_fchown(fd: i32, uid: u32, gid: u32) -> KResult<u64> {
    let f = file(fd)?;
    chown_inode(&*f.inode, f.path.as_ref(), uid, gid)
}

pub fn sys_utimensat(dirfd: i32, path: u64, times: u64, flags: u32) -> KResult<u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(EINVAL);
    }
    let inode = if path == 0 {
        if dirfd == AT_FDCWD {
            return Err(EFAULT);
        }
        file(dirfd)?.inode.clone()
    } else {
        resolve_at(dirfd, &user_path(path)?, flags)?.0
    };
    let now = vfs::now();
    let mut ts = [(now, false); 2];
    if times != 0 {
        let mut b = [0u8; 32];
        current().mm().copy_in(times, &mut b)?;
        for (i, t) in ts.iter_mut().enumerate() {
            let sec = i64::from_le_bytes(b[i * 16..i * 16 + 8].try_into().unwrap());
            let nsec = i64::from_le_bytes(b[i * 16 + 8..i * 16 + 16].try_into().unwrap());
            *t = match nsec {
                UTIME_NOW => (now, false),
                UTIME_OMIT => (0, true),
                0..=999_999_999 => (sec * 1_000_000_000 + nsec, false),
                _ => return Err(EINVAL),
            };
        }
    }
    inode.meta().update(|a| {
        if !ts[0].1 {
            a.atime = ts[0].0;
        }
        if !ts[1].1 {
            a.mtime = ts[1].0;
        }
        a.ctime = now;
    });
    crate::fs::inotify::touched(&*inode, None, crate::fs::inotify::IN_ATTRIB);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Extended attributes (read-only: disk file systems report theirs)

pub enum XattrTarget {
    /// Path, follow the last symlink.
    Path(u64, bool),
    Fd(i32),
}

fn xattr_inode(t: XattrTarget) -> KResult<Arc<dyn Inode>> {
    match t {
        XattrTarget::Path(p, follow) => {
            let path = user_path(p)?;
            let flags = if follow { 0 } else { AT_SYMLINK_NOFOLLOW };
            Ok(resolve_at(AT_FDCWD, &path, flags)?.0)
        }
        XattrTarget::Fd(fd) => Ok(file(fd)?.inode.clone()),
    }
}

pub fn sys_getxattr(t: XattrTarget, name: u64, value: u64, size: u64) -> KResult<u64> {
    let inode = xattr_inode(t)?;
    let name = xattr_name(name)?;
    let v = match inode.xattrs()?.into_iter().find(|(n, _)| *n == name) {
        Some((_, v)) => v,
        None if name == crate::security::XATTR_SELINUX => crate::security::UNLABELED.to_vec(),
        None => return Err(ENODATA),
    };
    if size == 0 {
        return Ok(v.len() as u64);
    }
    if (size as usize) < v.len() {
        return Err(ERANGE);
    }
    current().mm().copy_out(value, &v)?;
    Ok(v.len() as u64)
}

pub fn sys_listxattr(t: XattrTarget, list: u64, size: u64) -> KResult<u64> {
    let inode = xattr_inode(t)?;
    let mut out = Vec::new();
    for (n, _) in inode.xattrs()? {
        out.extend_from_slice(&n);
        out.push(0);
    }
    if size == 0 {
        return Ok(out.len() as u64);
    }
    if (size as usize) < out.len() {
        return Err(ERANGE);
    }
    current().mm().copy_out(list, &out)?;
    Ok(out.len() as u64)
}

fn xattr_name(name: u64) -> KResult<Vec<u8>> {
    let name = current().mm().read_cstr(name, 256)?;
    if name.is_empty() || name.len() > 255 {
        return Err(ERANGE);
    }
    Ok(name)
}

/// setxattr (`value` = Some((ptr, size))) or removexattr (None). tmpfs
/// (and every tmpfs-based file system) stores them; disk file systems are
/// read-only.
pub fn sys_setxattr(t: XattrTarget, name: u64, value: Option<(u64, u64)>, flags: u32) -> KResult<u64> {
    let inode = xattr_inode(t)?;
    let name = xattr_name(name)?;
    if flags & !3 != 0 {
        return Err(EINVAL);
    }
    let data = match value {
        Some((ptr, size)) => {
            if size > 65536 {
                return Err(E2BIG);
            }
            let mut v = alloc::vec![0u8; size as usize];
            current().mm().copy_in(ptr, &mut v)?;
            Some(v)
        }
        None => None,
    };
    inode.set_xattr(&name, data.as_deref(), flags)?;
    crate::fs::inotify::touched(&*inode, None, crate::fs::inotify::IN_ATTRIB);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Mounts

const MS_RDONLY: u64 = 1;
const MS_REMOUNT: u64 = 32;
const MS_BIND: u64 = 4096;
const MS_MOVE: u64 = 8192;
const MS_REC: u64 = 16384;
const MS_SHARED: u64 = 1 << 20;
const MS_PROPAGATION: u64 = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
const MNT_FLAGS: u32 = 1 | 2 | 4 | 8; // FORCE, DETACH, EXPIRE, UMOUNT_NOFOLLOW

/// "mode=0755" from a mount data string.
fn mode_option(data: &[u8]) -> Option<u32> {
    data.split(|&c| c == b',').find_map(|o| {
        let v = o.strip_prefix(b"mode=")?;
        u32::from_str_radix(core::str::from_utf8(v).ok()?, 8).ok()
    })
}

pub fn sys_mount(source: u64, target: u64, fstype: u64, flags: u64, data: u64) -> KResult<u64> {
    let t = current();
    let tp = user_path(target)?;
    let at = vfs::lookup(&t.process.cwd(), &tp, true)?;
    if flags & MS_REMOUNT != 0 {
        return Ok(0);
    }
    if flags & MS_MOVE != 0 {
        let src = vfs::lookup(&t.process.cwd(), &user_path(source)?, true)?;
        vfs::move_mount(&src, &at)?;
        return Ok(0);
    }
    if flags & MS_BIND != 0 {
        let src = vfs::lookup(&t.process.cwd(), &user_path(source)?, true)?;
        if src.is_dir() != at.is_dir() {
            return Err(ENOTDIR);
        }
        vfs::bind_mount(&src, &at, flags & MS_REC != 0)?;
        return Ok(0);
    }
    if flags & MS_PROPAGATION != 0 {
        // MS_SHARED, or MS_PRIVATE/MS_SLAVE/MS_UNBINDABLE (all private here).
        vfs::set_propagation(&at, flags & MS_SHARED != 0, flags & MS_REC != 0)?;
        return Ok(0);
    }
    let ty = user_path(fstype)?;
    let src = if source == 0 { ty.clone() } else { user_path(source)? };
    let data = if data == 0 { Vec::new() } else { t.mm().read_cstr(data, 4096)? };
    if let Some((root, name, flags)) = fs::new_block_fs(&ty, &src, flags)? {
        vfs::mount(&at, root, name, &src, flags & MS_RDONLY)?;
        return Ok(0);
    }
    let (root, name) = fs::new_fs_opts(&ty, mode_option(&data), &data)?;
    vfs::mount(&at, root, name, &src, flags & MS_RDONLY)?;
    Ok(0)
}

/// chroot: one mount namespace and one root for everybody, so only the
/// current root (after a mount was moved onto "/", Android's switch_root)
/// is accepted.
pub fn sys_chroot(path: u64) -> KResult<u64> {
    let p = vfs::lookup(&current().process.cwd(), &user_path(path)?, true)?;
    if !p.is_dir() {
        return Err(ENOTDIR);
    }
    if !p.same(&vfs::root()) {
        log::warn!("chroot({}): only the current root is supported", vfs::path_string(&p));
        return Err(EPERM);
    }
    Ok(0)
}

pub fn sys_umount2(target: u64, flags: u32) -> KResult<u64> {
    if flags & !MNT_FLAGS != 0 {
        return Err(EINVAL);
    }
    let p = user_path(target)?;
    let at = vfs::lookup(&current().process.cwd(), &p, flags & 8 == 0)?;
    vfs::umount(&at)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// inotify

pub fn sys_inotify_init1(flags: u32) -> KResult<u64> {
    if flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let f = crate::fs::inotify::new_file(flags & O_NONBLOCK != 0);
    Ok(current().process.files().alloc(f, flags & O_CLOEXEC != 0, 0)? as u64)
}

pub fn sys_inotify_add_watch(fd: i32, path: u64, mask: u32) -> KResult<u64> {
    let f = file(fd)?;
    let p = user_path(path)?;
    let flags = if mask & crate::fs::inotify::IN_DONT_FOLLOW != 0 { AT_SYMLINK_NOFOLLOW } else { 0 };
    let (inode, _) = resolve_at(AT_FDCWD, &p, flags)?;
    Ok(crate::fs::inotify::add_watch(&f, &inode, mask)? as u64)
}

pub fn sys_inotify_rm_watch(fd: i32, wd: i32) -> KResult<u64> {
    crate::fs::inotify::rm_watch(&*file(fd)?, wd)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// sendfile / copy_file_range: a copy loop through a kernel buffer.

/// Copy up to `count` bytes from `src` to `dst`. `src_off`/`dst_off` are
/// explicit offsets (pread/pwrite, file positions untouched) or None (the
/// file positions). Returns (bytes copied, final source offset).
fn copy_between(src: &OpenFile, src_off: Option<u64>, dst: &OpenFile, dst_off: Option<u64>, count: u64) -> KResult<(u64, Option<u64>)> {
    if !src.readable() || !dst.writable() {
        return Err(EBADF);
    }
    if dst.flags() & O_APPEND != 0 && dst_off.is_some() {
        return Err(EINVAL);
    }
    let mut done = 0u64;
    let mut buf = vec![0u8; (count as usize).min(IO_CHUNK)];
    while done < count {
        let n = ((count - done) as usize).min(buf.len());
        let r = match src.read(&mut buf[..n], src_off.map(|o| o + done)) {
            Ok(0) => break,
            Ok(r) => r,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        };
        let mut w = 0;
        while w < r {
            match dst.write(&buf[w..r], dst_off.map(|o| o + done + w as u64)) {
                Ok(0) => break,
                Ok(x) => w += x,
                Err(e) if done == 0 && w == 0 => return Err(e),
                Err(_) => break,
            }
        }
        done += w as u64;
        if w < r {
            break;
        }
    }
    Ok((done, src_off.map(|o| o + done)))
}

fn read_off(ptr: u64) -> KResult<Option<u64>> {
    if ptr == 0 {
        return Ok(None);
    }
    let mut b = [0u8; 8];
    current().mm().copy_in(ptr, &mut b)?;
    let v = i64::from_le_bytes(b);
    if v < 0 {
        return Err(EINVAL);
    }
    Ok(Some(v as u64))
}

pub fn sys_sendfile(out_fd: i32, in_fd: i32, offset: u64, count: u64) -> KResult<u64> {
    let (src, dst) = (io_file(in_fd)?, io_file(out_fd)?);
    let off = read_off(offset)?;
    let (n, end) = copy_between(&src, off, &dst, None, count.min(0x7fff_f000))?;
    if let Some(e) = end {
        current().mm().copy_out(offset, &e.to_le_bytes())?;
    }
    Ok(n)
}

pub fn sys_copy_file_range(fd_in: i32, off_in: u64, fd_out: i32, off_out: u64, len: u64, flags: u32) -> KResult<u64> {
    if flags != 0 {
        return Err(EINVAL);
    }
    let (src, dst) = (io_file(fd_in)?, io_file(fd_out)?);
    let (si, so) = (src.inode.meta().file_type(), dst.inode.meta().file_type());
    if si == S_IFDIR || so == S_IFDIR {
        return Err(EISDIR);
    }
    if si != S_IFREG || so != S_IFREG {
        return Err(EINVAL);
    }
    let (oi, oo) = (read_off(off_in)?, read_off(off_out)?);
    let (n, _) = copy_between(&src, oi, &dst, oo, len.min(0x7fff_f000))?;
    if let Some(o) = oi {
        current().mm().copy_out(off_in, &(o + n).to_le_bytes())?;
    }
    if let Some(o) = oo {
        current().mm().copy_out(off_out, &(o + n).to_le_bytes())?;
    }
    Ok(n)
}
