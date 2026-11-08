//! Pipes and FIFOs: a 64 KiB ring shared by read ends and write ends.
//! Readers block until data arrives or every writer is gone (EOF); writers
//! block while the pipe is full and get EPIPE + SIGPIPE once every reader
//! is gone. Writes of up to PIPE_BUF bytes are atomic.
//!
//! A FIFO (mknod S_IFIFO) gets its pipe on first open; the pipe lives as
//! long as some end is open (data is dropped when the last end closes).
//! Opening for reading blocks until a writer opens and vice versa, unless
//! O_NONBLOCK (a nonblocking writer with no reader gets ENXIO); O_RDWR
//! never blocks.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use super::file::{FileOps, O_ACCMODE, O_NONBLOCK, O_RDONLY, O_RDWR, O_WRONLY, OpenFile};
use super::poll::{POLLERR, POLLHUP, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use super::vfs::{AnonInode, Inode, S_IFIFO};
use crate::errno::*;
use crate::sched::{self, Wake};

pub const PIPE_CAPACITY: usize = 64 * 1024;
pub const PIPE_BUF: usize = 4096;

pub struct Pipe {
    buf: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    /// Opens of each end so far (FIFO open waits for the other side).
    r_opens: AtomicU64,
    w_opens: AtomicU64,
    wq: Arc<WaitQueue>,
}

impl Pipe {
    pub fn new() -> Arc<Pipe> {
        Arc::new(Pipe {
            buf: Mutex::new(VecDeque::new()),
            readers: AtomicUsize::new(0),
            writers: AtomicUsize::new(0),
            r_opens: AtomicU64::new(0),
            w_opens: AtomicU64::new(0),
            wq: WaitQueue::new(),
        })
    }

    fn key(&self) -> sched::WaitKey {
        self.wq.key()
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
                self.wq.wake(WRITE_EVENTS);
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
            self.wq.wake(READ_EVENTS);
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

/// A read end. `w_seen`: writer opens when it was opened; POLLHUP is only
/// reported once a writer has come (and gone) since then (a FIFO reader
/// opened with O_NONBLOCK and no writer is not "hung up").
pub struct PipeReader(pub Arc<Pipe>, u64);
pub struct PipeWriter(pub Arc<Pipe>);

impl PipeReader {
    pub fn new(p: Arc<Pipe>) -> Self {
        let seen = p.w_opens.load(Ordering::SeqCst);
        p.readers.fetch_add(1, Ordering::SeqCst);
        p.r_opens.fetch_add(1, Ordering::SeqCst);
        p.wq.wake(0);
        PipeReader(p, seen)
    }

    fn poll(&self) -> u32 {
        let p = &self.0;
        let mut m = if p.buf.lock().is_empty() { 0 } else { READ_EVENTS };
        if p.writers.load(Ordering::SeqCst) == 0 && p.w_opens.load(Ordering::SeqCst) != self.1 {
            m |= POLLHUP;
        }
        m
    }
}

impl PipeWriter {
    pub fn new(p: Arc<Pipe>) -> Self {
        p.writers.fetch_add(1, Ordering::SeqCst);
        p.w_opens.fetch_add(1, Ordering::SeqCst);
        p.wq.wake(0);
        PipeWriter(p)
    }

    fn poll(&self) -> u32 {
        let p = &self.0;
        let mut m = if p.buf.lock().len() < PIPE_CAPACITY { WRITE_EVENTS } else { 0 };
        if p.readers.load(Ordering::SeqCst) == 0 {
            m |= POLLERR;
        }
        m
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::SeqCst);
        self.0.wq.wake(POLLERR | WRITE_EVENTS);
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::SeqCst);
        self.0.wq.wake(POLLHUP | READ_EVENTS);
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

    fn poll(&self, _: &OpenFile) -> u32 {
        PipeReader::poll(self)
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
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

    fn poll(&self, _: &OpenFile) -> u32 {
        PipeWriter::poll(self)
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
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

/// A FIFO opened O_RDWR: both ends in one description.
pub struct FifoRdwr(PipeReader, PipeWriter);

impl FileOps for FifoRdwr {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        self.0.0.read(buf, f.nonblock())
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        self.1.0.write(data, f.nonblock())
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd == FIONREAD { fionread(&self.0.0, arg) } else { Err(ENOTTY) }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        self.0.poll() | self.1.poll()
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// FIFO inode -> its pipe while some end is open (keyed by the inode's
/// address, which cannot be reused while an open end holds the inode).
static FIFOS: Mutex<BTreeMap<usize, Weak<Pipe>>> = Mutex::new(BTreeMap::new());

fn fifo_pipe(inode: &Arc<dyn Inode>) -> Arc<Pipe> {
    let key = Arc::as_ptr(inode) as *const () as usize;
    let mut m = FIFOS.lock();
    m.retain(|_, w| w.strong_count() > 0);
    if let Some(p) = m.get(&key).and_then(Weak::upgrade) {
        return p;
    }
    let p = Pipe::new();
    m.insert(key, Arc::downgrade(&p));
    p
}

/// Wait until `count` (the other side's opens) moves past `seen` or the
/// other side is open.
fn wait_peer(p: &Pipe, others: &AtomicUsize, opens: &AtomicU64, seen: u64) -> KResult<()> {
    let w = sched::wait_until(p.key(), None, sched::WaitMode::Interruptible, || {
        others.load(Ordering::SeqCst) > 0 || opens.load(Ordering::SeqCst) != seen
    });
    if w == Wake::Ready { Ok(()) } else { Err(ERESTARTSYS) }
}

/// Open a FIFO node.
pub fn open_fifo(inode: &Arc<dyn Inode>, flags: u32) -> KResult<Box<dyn FileOps>> {
    let p = fifo_pipe(inode);
    let nonblock = flags & O_NONBLOCK != 0;
    match flags & O_ACCMODE {
        O_RDONLY => {
            let seen = p.w_opens.load(Ordering::SeqCst);
            let r = PipeReader::new(p.clone());
            if !nonblock {
                wait_peer(&p, &p.writers, &p.w_opens, seen)?;
            }
            Ok(Box::new(r))
        }
        O_WRONLY => {
            if nonblock && p.readers.load(Ordering::SeqCst) == 0 {
                return Err(ENXIO);
            }
            let seen = p.r_opens.load(Ordering::SeqCst);
            let w = PipeWriter::new(p.clone());
            if !nonblock {
                wait_peer(&p, &p.readers, &p.r_opens, seen)?;
            }
            Ok(Box::new(w))
        }
        O_RDWR => Ok(Box::new(FifoRdwr(PipeReader::new(p.clone()), PipeWriter::new(p)))),
        _ => Err(EINVAL),
    }
}
