//! Open files (file descriptions) and file descriptor tables.
//!
//! An [`OpenFile`] is what `open` returns and `dup` shares: the inode, the
//! path it was opened by (for /proc/self/fd and getcwd-like lookups), the
//! status flags, the file position and the [`FileOps`] that implement
//! read/write for this kind of file (regular file, directory, device,
//! pipe end, pseudo file).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::poll::{DEFAULT_POLLMASK, WaitQueue};
use super::vfs::{Inode, PathRef, S_IFDIR, S_IFMT, S_IFREG};
use crate::errno::*;

pub const O_ACCMODE: u32 = 3;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECT: u32 = 0o40000;
pub const O_LARGEFILE: u32 = 0o100000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_NOATIME: u32 = 0o1000000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const O_PATH: u32 = 0o10000000;
pub const O_TMPFILE: u32 = 0o20000000 | O_DIRECTORY;

/// Largest fd table (Linux's fs.nr_open default is far larger; init asks
/// for 32768).
pub const MAX_FDS: usize = 32768;
/// Default RLIMIT_NOFILE.
pub const DEFAULT_NOFILE: u64 = 1024;

/// Per-kind file behaviour. `pos` is the file position (seekable files
/// update it; stream files ignore it).
pub trait FileOps: Send + Sync + Any {
    fn read(&self, _file: &OpenFile, _pos: &mut u64, _buf: &mut [u8]) -> KResult<usize> {
        Err(EINVAL)
    }

    fn write(&self, _file: &OpenFile, _pos: &mut u64, _data: &[u8]) -> KResult<usize> {
        Err(EINVAL)
    }

    /// False for pipes and character devices: lseek/pread give ESPIPE
    /// (devices that accept lseek override [`FileOps::lseek_ok`]).
    fn seekable(&self) -> bool {
        false
    }

    /// lseek on a non-seekable file: pipes say ESPIPE, devices succeed
    /// with position 0 (Linux's noop_llseek / no_seek_end).
    fn lseek_ok(&self) -> bool {
        false
    }

    /// Size for SEEK_END.
    fn size(&self, file: &OpenFile) -> u64 {
        file.inode.size()
    }

    fn ioctl(&self, _file: &OpenFile, _cmd: u32, _arg: u64) -> KResult<u64> {
        Err(ENOTTY)
    }

    /// mmap of a device with its own memory (binder); returns the address.
    /// Regular files and /dev/zero are handled by the generic code.
    #[allow(clippy::too_many_arguments)]
    fn mmap(&self, _file: &Arc<OpenFile>, _addr: u64, _len: u64, _prot: u32, _fixed: bool, _noreplace: bool, _off: u64) -> KResult<u64> {
        Err(ENODEV)
    }

    /// Current readiness (POLLIN, POLLOUT, POLLHUP, ...). Files that never
    /// block are always ready.
    fn poll(&self, _file: &OpenFile) -> u32 {
        DEFAULT_POLLMASK
    }

    /// The queue woken when `poll` may have changed. Files without one
    /// cannot be added to an epoll set (EPERM, as on Linux).
    fn wait_queue(&self, _file: &OpenFile) -> Option<Arc<WaitQueue>> {
        None
    }

    /// Anonymous-inode files: the "[name]" in `anon_inode:[name]`.
    fn anon_name(&self) -> Option<&'static str> {
        None
    }

    /// Type-specific lines of /proc/<pid>/fdinfo/<fd>.
    fn fdinfo(&self, _file: &OpenFile) -> String {
        String::new()
    }

    fn as_any(&self) -> &dyn Any;
}

pub struct OpenFile {
    pub inode: Arc<dyn Inode>,
    /// The path it was opened by (None for pipes and the boot console).
    pub path: Option<PathRef>,
    pub ops: Box<dyn FileOps>,
    /// Access mode and status flags (O_APPEND, O_NONBLOCK, ...).
    pub flags: AtomicU32,
    pos: Mutex<u64>,
}

/// Flags that `open` does not keep in the file description.
const OPEN_ONLY_FLAGS: u32 = O_CREAT | O_EXCL | O_NOCTTY | O_TRUNC | O_CLOEXEC;
/// Flags F_SETFL may change.
pub const SETFL_FLAGS: u32 = O_APPEND | O_NONBLOCK | O_DIRECT | O_NOATIME | 0o20000; // O_ASYNC

impl OpenFile {
    pub fn new(inode: Arc<dyn Inode>, path: Option<PathRef>, ops: Box<dyn FileOps>, flags: u32) -> Arc<OpenFile> {
        Arc::new(OpenFile {
            inode,
            path,
            ops,
            flags: AtomicU32::new((flags & !OPEN_ONLY_FLAGS) | O_LARGEFILE),
            pos: Mutex::new(0),
        })
    }

    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    pub fn nonblock(&self) -> bool {
        self.flags() & O_NONBLOCK != 0
    }

    pub fn readable(&self) -> bool {
        let f = self.flags();
        f & O_PATH == 0 && f & O_ACCMODE != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        let f = self.flags();
        f & O_PATH == 0 && f & O_ACCMODE != O_RDONLY
    }

    pub fn is_path_only(&self) -> bool {
        self.flags() & O_PATH != 0
    }

    pub fn pos(&self) -> u64 {
        *self.pos.lock()
    }

    pub fn set_pos(&self, pos: u64) {
        *self.pos.lock() = pos;
    }

    /// Read at the file position (or at `at`, for pread).
    pub fn read(&self, buf: &mut [u8], at: Option<u64>) -> KResult<usize> {
        if !self.readable() {
            return Err(EBADF);
        }
        if !self.ops.seekable() {
            if at.is_some() {
                return Err(ESPIPE);
            }
            let mut p = 0;
            return self.ops.read(self, &mut p, buf);
        }
        match at {
            Some(mut p) => self.ops.read(self, &mut p, buf),
            None => {
                let mut pos = self.pos.lock();
                self.ops.read(self, &mut pos, buf)
            }
        }
    }

    pub fn write(&self, data: &[u8], at: Option<u64>) -> KResult<usize> {
        if !self.writable() {
            return Err(EBADF);
        }
        if !self.ops.seekable() {
            if at.is_some() {
                return Err(ESPIPE);
            }
            let mut p = 0;
            return self.ops.write(self, &mut p, data);
        }
        let r = match at {
            Some(mut p) => self.ops.write(self, &mut p, data),
            None => {
                let mut pos = self.pos.lock();
                self.ops.write(self, &mut pos, data)
            }
        };
        if matches!(r, Ok(n) if n > 0) && self.path.is_some() {
            super::inotify::touched(&*self.inode, self.path.as_ref(), super::inotify::IN_MODIFY);
        }
        r
    }

    pub fn lseek(&self, off: i64, whence: u32) -> KResult<u64> {
        if !self.ops.seekable() {
            return if self.ops.lseek_ok() && whence <= 2 { Ok(0) } else { Err(ESPIPE) };
        }
        let mut cur = self.pos.lock();
        let base = match whence {
            0 => 0,
            1 => *cur as i64,
            2 => self.ops.size(self) as i64,
            _ => return Err(EINVAL),
        };
        let new = base.checked_add(off).filter(|&n| n >= 0).ok_or(EINVAL)?;
        *cur = new as u64;
        Ok(new as u64)
    }

    pub fn ioctl(&self, cmd: u32, arg: u64) -> KResult<u64> {
        if self.is_path_only() {
            return Err(EBADF);
        }
        self.ops.ioctl(self, cmd, arg)
    }

    pub fn poll(&self) -> u32 {
        self.ops.poll(self)
    }
}

/// Regular files: read/write through the inode at the file position.
impl Drop for OpenFile {
    fn drop(&mut self) {
        if self.path.is_some() && self.flags() & O_PATH == 0 {
            let mask = if self.writable() { super::inotify::IN_CLOSE_WRITE } else { super::inotify::IN_CLOSE_NOWRITE };
            super::inotify::touched(&*self.inode, self.path.as_ref(), mask);
        }
    }
}

pub struct RegularFile;

impl FileOps for RegularFile {
    fn read(&self, f: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let n = f.inode.read_at(*pos, buf)?;
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, f: &OpenFile, pos: &mut u64, data: &[u8]) -> KResult<usize> {
        if f.flags() & O_APPEND != 0 {
            let (n, size) = f.inode.append(data)?;
            *pos = size;
            return Ok(n);
        }
        let n = f.inode.write_at(*pos, data)?;
        *pos += n as u64;
        Ok(n)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Directories: read gives EISDIR; the position is the getdents index.
pub struct DirFile;

impl FileOps for DirFile {
    fn read(&self, _: &OpenFile, _: &mut u64, _: &mut [u8]) -> KResult<usize> {
        Err(EISDIR)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, _: &[u8]) -> KResult<usize> {
        Err(EISDIR)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        0
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// O_PATH descriptors and other files with no I/O.
pub struct NoIo;

impl FileOps for NoIo {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Pseudo files whose contents are generated when opened (/proc).
pub struct SeqFile {
    pub data: Vec<u8>,
}

impl FileOps for SeqFile {
    fn read(&self, _: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let start = (*pos as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, _: &[u8]) -> KResult<usize> {
        Err(EACCES)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.data.len() as u64
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The default file operations for an inode of the given type.
pub fn default_ops(mode: u32) -> Box<dyn FileOps> {
    match mode & S_IFMT {
        S_IFREG => Box::new(RegularFile),
        S_IFDIR => Box::new(DirFile),
        _ => Box::new(NoIo),
    }
}

// ---------------------------------------------------------------------------
// File descriptor tables

#[derive(Clone)]
pub struct FdEntry {
    pub file: Arc<OpenFile>,
    pub cloexec: bool,
}

pub struct FdTable {
    fds: Mutex<Vec<Option<FdEntry>>>,
}

impl FdTable {
    pub fn new() -> Arc<FdTable> {
        Arc::new(FdTable { fds: Mutex::new(Vec::new()) })
    }

    pub fn duplicate(&self) -> Arc<FdTable> {
        Arc::new(FdTable { fds: Mutex::new(self.fds.lock().clone()) })
    }

    pub fn get(&self, fd: i32) -> KResult<Arc<OpenFile>> {
        let fds = self.fds.lock();
        usize::try_from(fd)
            .ok()
            .and_then(|i| fds.get(i))
            .and_then(|e| e.as_ref())
            .map(|e| e.file.clone())
            .ok_or(EBADF)
    }

    /// Install `file` at the lowest free descriptor >= `min`.
    pub fn alloc(&self, file: Arc<OpenFile>, cloexec: bool, min: usize) -> KResult<i32> {
        if min >= MAX_FDS {
            return Err(EINVAL);
        }
        let mut fds = self.fds.lock();
        let i = (min..MAX_FDS)
            .find(|&i| fds.get(i).is_none_or(|e| e.is_none()))
            .ok_or(EMFILE)?;
        if fds.len() <= i {
            fds.resize(i + 1, None);
        }
        fds[i] = Some(FdEntry { file, cloexec });
        Ok(i as i32)
    }

    /// Install at exactly `fd`, closing what was there.
    pub fn install(&self, fd: i32, file: Arc<OpenFile>, cloexec: bool) -> KResult<()> {
        let i = usize::try_from(fd).ok().filter(|&i| i < MAX_FDS).ok_or(EBADF)?;
        let old = {
            let mut fds = self.fds.lock();
            if fds.len() <= i {
                fds.resize(i + 1, None);
            }
            fds[i].replace(FdEntry { file, cloexec })
        };
        drop(old);
        Ok(())
    }

    pub fn close(&self, fd: i32) -> KResult<()> {
        let old = {
            let mut fds = self.fds.lock();
            usize::try_from(fd)
                .ok()
                .and_then(|i| fds.get_mut(i))
                .and_then(|e| e.take())
                .ok_or(EBADF)?
        };
        // Drop (possibly waking pipe peers) outside the table lock.
        drop(old);
        Ok(())
    }

    /// close_range: close (or, with `cloexec_only`, mark CLOEXEC) every
    /// descriptor in [first, last].
    pub fn close_range(&self, first: u32, last: u32, cloexec_only: bool) {
        let closed: Vec<FdEntry> = {
            let mut fds = self.fds.lock();
            let end = (last as usize).saturating_add(1).min(fds.len());
            let mut v = Vec::new();
            for e in fds.iter_mut().take(end).skip(first as usize) {
                if cloexec_only {
                    if let Some(e) = e.as_mut() {
                        e.cloexec = true;
                    }
                } else if let Some(x) = e.take() {
                    v.push(x);
                }
            }
            v
        };
        drop(closed);
    }

    pub fn cloexec(&self, fd: i32) -> KResult<bool> {
        let fds = self.fds.lock();
        usize::try_from(fd)
            .ok()
            .and_then(|i| fds.get(i))
            .and_then(|e| e.as_ref())
            .map(|e| e.cloexec)
            .ok_or(EBADF)
    }

    pub fn set_cloexec(&self, fd: i32, on: bool) -> KResult<()> {
        let mut fds = self.fds.lock();
        let e = usize::try_from(fd)
            .ok()
            .and_then(|i| fds.get_mut(i))
            .and_then(|e| e.as_mut())
            .ok_or(EBADF)?;
        e.cloexec = on;
        Ok(())
    }

    /// Every open descriptor (for /proc/<pid>/fd).
    pub fn list(&self) -> Vec<(i32, Arc<OpenFile>)> {
        self.fds
            .lock()
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i as i32, e.file.clone())))
            .collect()
    }

    /// Close every close-on-exec descriptor (execve).
    pub fn close_on_exec(&self) {
        let closed: Vec<FdEntry> = {
            let mut fds = self.fds.lock();
            fds.iter_mut()
                .filter(|e| e.as_ref().is_some_and(|e| e.cloexec))
                .filter_map(|e| e.take())
                .collect()
        };
        drop(closed);
    }

    /// Close everything (process exit).
    pub fn close_all(&self) {
        let all = core::mem::take(&mut *self.fds.lock());
        drop(all);
    }
}
