//! eventfd: a 64-bit counter. write adds (blocking while the sum would
//! exceed 2^64 - 2), read returns and clears it (EFD_SEMAPHORE: returns 1
//! and decrements), blocking while it is 0.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use spin::Mutex;

use super::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use super::poll::{POLLERR, POLLIN, POLLOUT, WaitQueue};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};

pub const EFD_SEMAPHORE: u32 = 1;
const MAX: u64 = u64::MAX - 1;

pub struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
    wq: Arc<WaitQueue>,
}

impl EventFd {
    /// Block (unless nonblocking) until `try_op` succeeds.
    fn wait(&self, nonblock: bool, mut try_op: impl FnMut(&mut u64) -> bool) -> KResult<()> {
        let mut again = false;
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            if try_op(&mut self.count.lock()) {
                return true;
            }
            again = nonblock;
            nonblock
        });
        match w {
            _ if again => Err(EAGAIN),
            Wake::Ready => Ok(()),
            _ => Err(ERESTARTSYS),
        }
    }
}

impl FileOps for EventFd {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let mut v = 0;
        self.wait(f.nonblock(), |c| {
            if *c == 0 {
                return false;
            }
            v = if self.semaphore { 1 } else { *c };
            *c -= v;
            true
        })?;
        buf[..8].copy_from_slice(&v.to_ne_bytes());
        self.wq.wake(POLLOUT);
        Ok(8)
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        if data.len() < 8 {
            return Err(EINVAL);
        }
        let v = u64::from_ne_bytes(data[..8].try_into().unwrap());
        if v == u64::MAX {
            return Err(EINVAL);
        }
        self.wait(f.nonblock(), |c| {
            if MAX - *c < v {
                return false;
            }
            *c += v;
            true
        })?;
        if v != 0 {
            self.wq.wake(POLLIN);
        }
        Ok(8)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let c = *self.count.lock();
        let mut m = 0;
        if c > 0 {
            m |= POLLIN;
        }
        if c == u64::MAX {
            m |= POLLERR;
        }
        if c < MAX {
            m |= POLLOUT;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[eventfd]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        format!("eventfd-count: {:16x}\neventfd-semaphore: {}\n", *self.count.lock(), self.semaphore as u8)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new eventfd file; `flags` may hold O_NONBLOCK and EFD_SEMAPHORE.
pub fn new(initval: u32, flags: u32) -> Arc<OpenFile> {
    let ops = EventFd { count: Mutex::new(initval as u64), semaphore: flags & EFD_SEMAPHORE != 0, wq: WaitQueue::new() };
    OpenFile::new(super::anon_inode(), None, Box::new(ops), O_RDWR | (flags & O_NONBLOCK))
}
