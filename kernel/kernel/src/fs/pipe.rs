//! Pipes: a 64 KiB ring shared by a read end and a write end. Readers
//! block until data arrives or every writer is gone (EOF); writers block
//! while the pipe is full and get EPIPE + SIGPIPE once every reader is
//! gone. Writes of up to PIPE_BUF bytes are atomic.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use super::file::{FileOps, O_NONBLOCK, O_RDONLY, O_WRONLY, OpenFile};
use super::vfs::{AnonInode, Inode, S_IFIFO};
use crate::errno::*;
use crate::sched::{self, Wake};

pub const PIPE_CAPACITY: usize = 64 * 1024;
pub const PIPE_BUF: usize = 4096;
const WAIT_PIPE: u64 = 0x5049_5045_0000_0000;

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

    /// Bytes waiting to be read (FIONREAD).
    pub fn available(&self) -> usize {
        self.buf.lock().len()
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
        // Up to PIPE_BUF bytes go in one piece (never interleaved).
        let atomic = data.len() <= PIPE_BUF;
        while written < data.len() || data.is_empty() {
            let mut err = None;
            let wake = sched::wait_until(self.key(), None, sched::WaitMode::Interruptible, || {
                if self.readers.load(Ordering::SeqCst) == 0 {
                    err = Some(EPIPE);
                    return true;
                }
                let mut b = self.buf.lock();
                let space = PIPE_CAPACITY - b.len();
                let want = data.len() - written;
                if (atomic && space >= want) || (!atomic && space > 0) || data.is_empty() {
                    let n = space.min(want);
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

const FIONREAD: u32 = 0x541b;

fn fionread(pipe: &Pipe, arg: u64) -> KResult<u64> {
    let n = pipe.available() as i32;
    crate::process::current().mm().copy_out(arg, &n.to_le_bytes())?;
    Ok(0)
}

pub struct PipeReader(pub Arc<Pipe>);
pub struct PipeWriter(pub Arc<Pipe>);

impl PipeReader {
    pub fn new(p: Arc<Pipe>) -> Self {
        p.readers.fetch_add(1, Ordering::SeqCst);
        PipeReader(p)
    }
}

impl PipeWriter {
    pub fn new(p: Arc<Pipe>) -> Self {
        p.writers.fetch_add(1, Ordering::SeqCst);
        PipeWriter(p)
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::SeqCst);
        sched::wake_all(self.0.key());
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::SeqCst);
        sched::wake_all(self.0.key());
    }
}

impl FileOps for PipeReader {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        self.0.read(buf, f.nonblock())
    }

    fn write(&self, _: &OpenFile, _: &mut u64, _: &[u8]) -> KResult<usize> {
        Err(EBADF)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd == FIONREAD { fionread(&self.0, arg) } else { Err(ENOTTY) }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FileOps for PipeWriter {
    fn read(&self, _: &OpenFile, _: &mut u64, _: &mut [u8]) -> KResult<usize> {
        Err(EBADF)
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        self.0.write(data, f.nonblock())
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd == FIONREAD { fionread(&self.0, arg) } else { Err(ENOTTY) }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new pipe: (read end, write end). `flags` may hold O_NONBLOCK.
pub fn new_pipe(flags: u32) -> (Arc<OpenFile>, Arc<OpenFile>) {
    let p = Pipe::new();
    let inode: Arc<dyn Inode> = AnonInode::new(S_IFIFO | 0o600, 0);
    let r = OpenFile::new(inode.clone(), None, Box::new(PipeReader::new(p.clone())), O_RDONLY | (flags & O_NONBLOCK));
    let w = OpenFile::new(inode, None, Box::new(PipeWriter::new(p)), O_WRONLY | (flags & O_NONBLOCK));
    (r, w)
}
