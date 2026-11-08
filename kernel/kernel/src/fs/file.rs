//! Open files, file descriptor tables and pipes.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;

use super::ramfs::Inode;
use crate::errno::*;
use crate::sched::{self, Wake};

pub const O_ACCMODE: u32 = 3;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_CLOEXEC: u32 = 0o2000000;

pub const PIPE_CAPACITY: usize = 64 * 1024;
const WAIT_PIPE: u64 = 0x5049_5045_0000_0000;
const MAX_FDS: usize = 1024;

pub enum FileKind {
    Inode(Arc<Inode>),
    Console,
    Null,
    Zero,
    Random,
    PipeRead(Arc<Pipe>),
    PipeWrite(Arc<Pipe>),
}

pub struct OpenFile {
    pub kind: FileKind,
    /// Access mode and status flags (O_APPEND, O_NONBLOCK).
    pub flags: AtomicU32,
    offset: Mutex<u64>,
}

impl OpenFile {
    pub fn new(kind: FileKind, flags: u32) -> Arc<OpenFile> {
        match &kind {
            FileKind::PipeRead(p) => p.readers.fetch_add(1, Ordering::SeqCst),
            FileKind::PipeWrite(p) => p.writers.fetch_add(1, Ordering::SeqCst),
            _ => 0,
        };
        Arc::new(OpenFile {
            kind,
            flags: AtomicU32::new(flags & !(O_CREAT | O_EXCL | O_TRUNC | O_CLOEXEC)),
            offset: Mutex::new(0),
        })
    }

    fn nonblock(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0
    }

    fn readable(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_WRONLY
    }

    fn writable(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_RDONLY
    }

    pub fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if !self.readable() {
            return Err(EBADF);
        }
        match &self.kind {
            FileKind::Inode(i) => {
                if i.is_dir() {
                    return Err(EISDIR);
                }
                let mut off = self.offset.lock();
                let data = i.data.lock();
                let bytes = data.bytes();
                let start = (*off as usize).min(bytes.len());
                let n = buf.len().min(bytes.len() - start);
                buf[..n].copy_from_slice(&bytes[start..start + n]);
                *off += n as u64;
                Ok(n)
            }
            FileKind::Console | FileKind::Null => Ok(0),
            FileKind::Zero => {
                buf.fill(0);
                Ok(buf.len())
            }
            FileKind::Random => {
                crate::random::fill(buf);
                Ok(buf.len())
            }
            FileKind::PipeRead(p) => p.read(buf, self.nonblock()),
            FileKind::PipeWrite(_) => Err(EBADF),
        }
    }

    pub fn write(&self, data: &[u8]) -> KResult<usize> {
        if !self.writable() {
            return Err(EBADF);
        }
        match &self.kind {
            FileKind::Inode(i) => {
                if i.is_dir() {
                    return Err(EISDIR);
                }
                let mut off = self.offset.lock();
                let mut d = i.data.lock();
                let v = d.owned();
                if self.flags.load(Ordering::Relaxed) & O_APPEND != 0 {
                    *off = v.len() as u64;
                }
                let start = *off as usize;
                if v.len() < start + data.len() {
                    v.resize(start + data.len(), 0);
                }
                v[start..start + data.len()].copy_from_slice(data);
                *off += data.len() as u64;
                Ok(data.len())
            }
            FileKind::Console => {
                crate::drivers::serial::write_bytes(data);
                Ok(data.len())
            }
            FileKind::Null | FileKind::Zero | FileKind::Random => Ok(data.len()),
            FileKind::PipeWrite(p) => p.write(data, self.nonblock()),
            FileKind::PipeRead(_) => Err(EBADF),
        }
    }

    pub fn lseek(&self, off: i64, whence: u32) -> KResult<u64> {
        let FileKind::Inode(i) = &self.kind else {
            return match self.kind {
                FileKind::PipeRead(_) | FileKind::PipeWrite(_) => Err(ESPIPE),
                _ => Ok(0),
            };
        };
        let mut cur = self.offset.lock();
        let base = match whence {
            0 => 0,
            1 => *cur as i64,
            2 => i.size() as i64,
            _ => return Err(EINVAL),
        };
        let new = base.checked_add(off).filter(|&n| n >= 0).ok_or(EINVAL)?;
        *cur = new as u64;
        Ok(new as u64)
    }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        match &self.kind {
            FileKind::PipeRead(p) => {
                p.readers.fetch_sub(1, Ordering::SeqCst);
                sched::wake_all(p.key());
            }
            FileKind::PipeWrite(p) => {
                p.writers.fetch_sub(1, Ordering::SeqCst);
                sched::wake_all(p.key());
            }
            _ => {}
        }
    }
}

pub struct Pipe {
    buf: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
}

impl Pipe {
    pub fn new() -> Arc<Pipe> {
        Arc::new(Pipe {
            buf: Mutex::new(VecDeque::new()),
            readers: AtomicUsize::new(0),
            writers: AtomicUsize::new(0),
        })
    }

    fn key(&self) -> sched::WaitKey {
        (WAIT_PIPE, self as *const Pipe as u64)
    }

    fn read(&self, out: &mut [u8], nonblock: bool) -> KResult<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut result = Ok(0);
        let wake = sched::wait_until(self.key(), None, sched::WaitMode::Interruptible, || {
            let mut b = self.buf.lock();
            if !b.is_empty() {
                let n = out.len().min(b.len());
                for (i, byte) in b.drain(..n).enumerate() {
                    out[i] = byte;
                }
                result = Ok(n);
                return true;
            }
            if self.writers.load(Ordering::SeqCst) == 0 {
                result = Ok(0);
                return true;
            }
            if nonblock {
                result = Err(EAGAIN);
                return true;
            }
            false
        });
        match wake {
            Wake::Ready => {
                sched::wake_all(self.key());
                result
            }
            _ => Err(ERESTARTSYS),
        }
    }

    fn write(&self, data: &[u8], nonblock: bool) -> KResult<usize> {
        let mut written = 0;
        while written < data.len() || data.is_empty() {
            let mut err = None;
            let wake = sched::wait_until(self.key(), None, sched::WaitMode::Interruptible, || {
                if self.readers.load(Ordering::SeqCst) == 0 {
                    err = Some(EPIPE);
                    return true;
                }
                let mut b = self.buf.lock();
                let space = PIPE_CAPACITY - b.len();
                if space > 0 || data.is_empty() {
                    let n = space.min(data.len() - written);
                    b.extend(&data[written..written + n]);
                    written += n;
                    return true;
                }
                if nonblock {
                    err = Some(EAGAIN);
                    return true;
                }
                false
            });
            sched::wake_all(self.key());
            if let Some(e) = err {
                if e == EPIPE {
                    crate::process::signal::send_to_current(crate::process::signal::SIGPIPE);
                }
                return if written > 0 { Ok(written) } else { Err(e) };
            }
            if wake != Wake::Ready {
                return if written > 0 { Ok(written) } else { Err(ERESTARTSYS) };
            }
            if data.is_empty() {
                break;
            }
        }
        Ok(written)
    }
}

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
        Arc::new(FdTable {
            fds: Mutex::new(Vec::new()),
        })
    }

    pub fn duplicate(&self) -> Arc<FdTable> {
        Arc::new(FdTable {
            fds: Mutex::new(self.fds.lock().clone()),
        })
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
