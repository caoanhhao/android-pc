//! signalfd: read pending signals from a mask as `signalfd_siginfo`
//! records (128 bytes each) instead of running handlers. As on Linux, the
//! file reads and polls the signals of whichever thread uses it; the wait
//! queue is the process's (`Process::signalfd_wq`), woken whenever a
//! signal is queued to it or one of its threads.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use super::poll::{READ_EVENTS, WaitQueue};
use crate::errno::*;
use crate::process::{self, signal};
use crate::sched::{self, Wake, WaitMode};

pub const SIGINFO_SIZE: usize = 128;

pub struct SignalFd {
    pub mask: AtomicU64,
}

/// `struct signalfd_siginfo`.
fn encode(sig: u32, si: &signal::SigInfo) -> [u8; SIGINFO_SIZE] {
    let mut b = [0u8; SIGINFO_SIZE];
    let mut put32 = |off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put32(0, sig);
    put32(4, si.errno as u32);
    put32(8, si.code as u32);
    if si.addr.is_none() {
        put32(12, si.pid as u32);
        put32(16, si.uid);
    }
    if let Some(st) = si.status {
        put32(40, st as u32);
    }
    if let Some(a) = si.addr {
        b[72..80].copy_from_slice(&a.to_le_bytes());
    }
    b
}

impl FileOps for SignalFd {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if buf.len() < SIGINFO_SIZE {
            return Err(EINVAL);
        }
        let t = process::current();
        let mask = self.mask.load(Ordering::Relaxed);
        let nonblock = f.nonblock();
        let mut got = None;
        let w = sched::wait_until(t.process.signalfd_wq.key(), None, WaitMode::Interruptible, || {
            got = signal::dequeue(&t, mask);
            got.is_some() || nonblock
        });
        let Some((sig, si)) = got else {
            return Err(if w == Wake::Ready { EAGAIN } else { ERESTARTSYS });
        };
        buf[..SIGINFO_SIZE].copy_from_slice(&encode(sig, &si));
        let mut n = SIGINFO_SIZE;
        // More records without blocking.
        while buf.len() - n >= SIGINFO_SIZE {
            let Some((sig, si)) = signal::dequeue(&t, mask) else { break };
            buf[n..n + SIGINFO_SIZE].copy_from_slice(&encode(sig, &si));
            n += SIGINFO_SIZE;
        }
        Ok(n)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let t = process::current();
        if signal::pending_set(&t) & self.mask.load(Ordering::Relaxed) != 0 { READ_EVENTS } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(process::current().process.signalfd_wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[signalfd]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        format!("sigmask:\t{:016x}\n", self.mask.load(Ordering::Relaxed))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new signalfd file; `flags` may hold O_NONBLOCK.
pub fn new(mask: u64, flags: u32) -> Arc<OpenFile> {
    let ops = SignalFd { mask: AtomicU64::new(mask) };
    OpenFile::new(super::anon_inode(), None, Box::new(ops), O_RDWR | (flags & O_NONBLOCK))
}
