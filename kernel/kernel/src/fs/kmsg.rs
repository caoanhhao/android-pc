//! The kernel log buffer (printk ring): every kernel log line and every
//! line written to /dev/kmsg becomes a record with a sequence number, a
//! timestamp and a syslog priority. Readers:
//!
//! - `/dev/kmsg` (`KmsgDev`): one record per read() as
//!   "prio,seq,usec,-;text\n", each open file with its own position (the
//!   file position is the next sequence number; SEEK_SET 0 = oldest,
//!   SEEK_END = after the newest); blocks when nothing is new; POLLIN.
//! - `/proc/kmsg` (`ProcKmsg`) and syslog(2) READ: one shared, destructive
//!   cursor, "<prio>[sec.usec] text\n" lines; logd's klogd reads it.
//! - syslog(2) READ_ALL/READ_CLEAR/CLEAR/SIZE_*.
//!
//! Records live in a fixed static array (no allocation: the logger runs in
//! any context, interrupts included); text longer than [`TEXT`] bytes is
//! cut. Readers are woken from the timer tick ([`tick`]) or directly by
//! /dev/kmsg writes.

use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Once;

use super::file::{FileOps, OpenFile};
use super::poll::{POLLIN, POLLOUT, POLLRDNORM, POLLWRNORM, WaitQueue};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};
use crate::sync::IrqLock;

const RECORDS: usize = 1024;
const TEXT: usize = 400;
/// LOG_USER: the facility of /dev/kmsg writes without a "<N>" prefix.
const FAC_USER: u8 = 1;

#[derive(Clone, Copy)]
struct Rec {
    seq: u64,
    ts_us: u64,
    /// facility << 3 | level
    prio: u8,
    len: u16,
    text: [u8; TEXT],
}

const EMPTY: Rec = Rec { seq: 0, ts_us: 0, prio: 0, len: 0, text: [0; TEXT] };

struct Ring {
    recs: [Rec; RECORDS],
    /// Sequence number of the next record (records `next - RECORDS ..
    /// next` are kept).
    next: u64,
    /// /proc/kmsg and syslog READ position.
    syslog: u64,
    /// syslog CLEAR position (READ_ALL starts here).
    clear: u64,
}

impl Ring {
    fn first(&self) -> u64 {
        self.next.saturating_sub(RECORDS as u64)
    }

    fn get(&self, seq: u64) -> Option<&Rec> {
        (seq >= self.first() && seq < self.next).then(|| &self.recs[(seq % RECORDS as u64) as usize])
    }
}

static RING: IrqLock<Ring> = IrqLock::new(Ring { recs: [EMPTY; RECORDS], next: 0, syslog: 0, clear: 0 });
static PENDING: AtomicBool = AtomicBool::new(false);
static WQ: Once<Arc<WaitQueue>> = Once::new();

fn wq() -> Arc<WaitQueue> {
    WQ.call_once(WaitQueue::new).clone()
}

/// Append a record (any context). `level` 0-7, `facility` 0-23.
pub fn record(facility: u8, level: u8, text: &[u8]) {
    let ts_us = crate::time::monotonic_ns() / 1000;
    let mut text = text;
    while let Some((&b'\n', rest)) = text.split_last() {
        text = rest;
    }
    let n = text.len().min(TEXT);
    {
        let mut r = RING.lock();
        let seq = r.next;
        let rec = &mut r.recs[(seq % RECORDS as u64) as usize];
        rec.seq = seq;
        rec.ts_us = ts_us;
        rec.prio = (facility << 3) | (level & 7);
        rec.len = n as u16;
        rec.text[..n].copy_from_slice(&text[..n]);
        r.next = seq + 1;
        let first = r.first();
        r.syslog = r.syslog.max(first);
        r.clear = r.clear.max(first);
    }
    PENDING.store(true, Ordering::Release);
}

/// From the timer interrupt: wake readers if records were added.
pub fn tick() {
    if PENDING.swap(false, Ordering::AcqRel)
        && let Some(q) = WQ.get()
    {
        q.wake(POLLIN);
    }
}

/// A write to /dev/kmsg: "<N>text" (N = facility << 3 | level; default
/// LOG_USER, LOG_INFO... like Linux, level 4 when no prefix is given).
/// Returns the text without the prefix.
pub fn user_write(data: &[u8]) -> &[u8] {
    let (mut fac, mut level, mut s) = (FAC_USER, 4u8, data);
    if s.first() == Some(&b'<')
        && let Some(end) = s.iter().take(6).position(|&c| c == b'>')
        && let Some(p) = core::str::from_utf8(&s[1..end]).ok().and_then(|x| x.parse::<u32>().ok())
    {
        level = (p & 7) as u8;
        // Userspace may not log as the kernel (facility 0).
        if p >> 3 != 0 {
            fac = (p >> 3).min(23) as u8;
        }
        s = &s[end + 1..];
    }
    record(fac, level, s);
    tick();
    s
}

/// Block until `ready` (non-blocking: EAGAIN).
fn wait(nonblock: bool, mut ready: impl FnMut() -> bool) -> KResult<()> {
    let mut again = false;
    let w = sched::wait_until(wq().key(), None, WaitMode::Interruptible, || {
        if ready() {
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

/// "<prio>[sec.usec] text\n" (syslog format).
fn syslog_line(r: &Rec, out: &mut Vec<u8>) {
    let s = format!("<{}>[{:5}.{:06}] ", r.prio, r.ts_us / 1_000_000, r.ts_us % 1_000_000);
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(&r.text[..r.len as usize]);
    out.push(b'\n');
}

/// Lines from `*seq` on that fit in `max` bytes (at least one record,
/// cut if needed); advances `*seq`.
fn syslog_lines(ring: &Ring, seq: &mut u64, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    *seq = (*seq).max(ring.first());
    while let Some(r) = ring.get(*seq) {
        let mut line = Vec::new();
        syslog_line(r, &mut line);
        if out.len() + line.len() > max {
            if out.is_empty() {
                line.truncate(max);
                out = line;
                *seq += 1;
            }
            break;
        }
        out.extend_from_slice(&line);
        *seq += 1;
    }
    out
}

/// syslog(2) READ and /proc/kmsg read: destructive, blocking.
pub fn read_syslog(buf: &mut [u8], nonblock: bool) -> KResult<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    wait(nonblock, || {
        let r = RING.lock();
        r.syslog < r.next
    })?;
    let mut r = RING.lock();
    let mut seq = r.syslog;
    let out = syslog_lines(&r, &mut seq, buf.len());
    r.syslog = seq;
    buf[..out.len()].copy_from_slice(&out);
    Ok(out.len())
}

/// syslog(2) READ_ALL: the newest records since the last CLEAR that fit
/// in `len` bytes; with `clear` (READ_CLEAR) then clears.
pub fn read_all(len: usize, clear: bool) -> Vec<u8> {
    let mut r = RING.lock();
    // Newest first, to keep the last `len` bytes.
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut total = 0;
    let mut seq = r.next;
    while seq > r.clear.max(r.first()) {
        seq -= 1;
        let mut line = Vec::new();
        syslog_line(r.get(seq).unwrap(), &mut line);
        if total + line.len() > len {
            break;
        }
        total += line.len();
        lines.push(line);
    }
    if clear {
        r.clear = r.next;
    }
    lines.into_iter().rev().flatten().collect()
}

/// syslog(2) CLEAR.
pub fn clear() {
    let mut r = RING.lock();
    r.clear = r.next;
}

/// syslog(2) SIZE_UNREAD: bytes /proc/kmsg would return now.
pub fn size_unread() -> usize {
    let r = RING.lock();
    let mut seq = r.syslog;
    syslog_lines(&r, &mut seq, usize::MAX).len()
}

pub const BUFFER_SIZE: usize = RECORDS * 128;

// ---------------------------------------------------------------------------
// /dev/kmsg

/// /dev/kmsg. The file position is the sequence number of the next record
/// to read.
pub struct KmsgDev;

impl FileOps for KmsgDev {
    fn read(&self, f: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let p = *pos;
        wait(f.nonblock(), || p < RING.lock().next)?;
        let r = RING.lock();
        let seq = (*pos).max(r.first());
        let Some(rec) = r.get(seq) else {
            return Err(EAGAIN);
        };
        let mut line = format!("{},{},{},-;", rec.prio, rec.seq, rec.ts_us).into_bytes();
        for &c in &rec.text[..rec.len as usize] {
            // Non-printable bytes are escaped like Linux does.
            if c < b' ' || c >= 0x7f || c == b'\\' {
                line.extend_from_slice(format!("\\x{c:02x}").as_bytes());
            } else {
                line.push(c);
            }
        }
        line.push(b'\n');
        if line.len() > buf.len() {
            return Err(EINVAL);
        }
        buf[..line.len()].copy_from_slice(&line);
        *pos = seq + 1;
        Ok(line.len())
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let text = user_write(data);
        let ms = crate::time::uptime_ms();
        kprintln!("[{:>5}.{:03}] \x1b[32mINFO \x1b[0m cpu{} kmsg: {}", ms / 1000, ms % 1000, crate::arch::cpu_index(),
            core::str::from_utf8(text).unwrap_or("<binary>").trim_end());
        Ok(data.len())
    }

    /// SEEK_SET 0 (oldest record) and SEEK_END (after the newest) through
    /// the generic code: the "size" is the next sequence number.
    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        RING.lock().next
    }

    fn poll(&self, f: &OpenFile) -> u32 {
        let mut m = POLLOUT | POLLWRNORM;
        if f.pos() < RING.lock().next {
            m |= POLLIN | POLLRDNORM;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(wq())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// /proc/kmsg

pub struct ProcKmsg;

impl FileOps for ProcKmsg {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        read_syslog(buf, f.nonblock())
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let r = RING.lock();
        if r.syslog < r.next { POLLIN | POLLRDNORM } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(wq())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open_proc_kmsg() -> Box<dyn FileOps> {
    Box::new(ProcKmsg)
}
