//! Pseudo-terminals: /dev/ptmx (5:2) and devpts (/dev/pts/N, 136:N).
//!
//! Opening /dev/ptmx makes a new pair and its slave node in the (single,
//! shared) devpts instance; TIOCSPTLCK/TIOCGPTN (unlockpt/ptsname) and
//! TIOCGPTPEER work as on Linux. The line discipline is minimal: bytes the
//! master writes reach the slave's reads unchanged (no echo, no line
//! editing, no signals), slave output gets OPOST/ONLCR ("\n" -> "\r\n")
//! when set in its termios, which TCGETS/TCSETS store. When the master
//! closes, the slave sees end of file (and EIO on write); once every slave
//! descriptor is closed after one was opened, master reads give EIO, which
//! is how logwrapper notices that its child is done.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::{Mutex, Once};

use super::file::{FileOps, O_NONBLOCK, O_RDWR, OpenFile};
use super::poll::{POLLHUP, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use super::tmpfs::TmpInode;
use super::vfs::{AnonInode, Inode, S_IFCHR, encode_dev};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};

pub const PTS_MAJOR: u32 = 136;
const MAX_PTYS: u32 = 4096;
/// Bytes buffered in each direction before writers block.
const BUF_MAX: usize = 64 * 1024;

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCSCTTY: u32 = 0x540e;
const TIOCGPGRP: u32 = 0x540f;
const TIOCSPGRP: u32 = 0x5410;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const FIONREAD: u32 = 0x541b;
const TIOCNOTTY: u32 = 0x5422;
const TIOCGPTN: u32 = 0x8004_5430;
const TIOCSPTLCK: u32 = 0x4004_5431;
const TIOCGPTLCK: u32 = 0x8004_5439;
const TIOCGPTPEER: u32 = 0x5441;

const OPOST: u32 = 1;
const ONLCR: u32 = 4;

/// Kernel struct termios (36 bytes): iflag, oflag, cflag, lflag, line, cc[19].
fn default_termios() -> [u8; 36] {
    let mut t = [0u8; 36];
    let words: [u32; 4] = [0x500 /* ICRNL|IXON */, OPOST | ONLCR, 0xbf /* B38400|CS8|CREAD */, 0x8a3b /* ISIG|ICANON|ECHO... */];
    for (i, w) in words.iter().enumerate() {
        t[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    let cc: [u8; 17] = [3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0];
    t[17..17 + cc.len()].copy_from_slice(&cc);
    t
}

struct Pty {
    index: u32,
    locked: AtomicBool,
    /// Master -> slave.
    input: Mutex<VecDeque<u8>>,
    /// Slave -> master.
    output: Mutex<VecDeque<u8>>,
    master_open: AtomicBool,
    slave_opens: AtomicU32,
    slave_seen: AtomicBool,
    wq: Arc<WaitQueue>,
    termios: Mutex<[u8; 36]>,
    winsize: Mutex<[u8; 8]>,
    pgrp: AtomicU32,
}

static PTYS: Mutex<BTreeMap<u32, Weak<Pty>>> = Mutex::new(BTreeMap::new());
static DEVPTS: Once<Arc<TmpInode>> = Once::new();

/// The shared devpts root (`ptmx` plus one node per open pair).
pub fn devpts_root() -> Arc<TmpInode> {
    DEVPTS
        .call_once(|| {
            let r = TmpInode::new_fs(0o755);
            let _ = r.add(b"ptmx", S_IFCHR | 0o666, encode_dev(5, 2), None);
            r
        })
        .clone()
}

fn user_int(arg: u64) -> KResult<u32> {
    let mut b = [0u8; 4];
    crate::process::current().mm().copy_in(arg, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn put_int(arg: u64, v: u32) -> KResult<u64> {
    crate::process::current().mm().copy_out(arg, &v.to_le_bytes())?;
    Ok(0)
}

impl Pty {
    fn oflag(&self) -> u32 {
        u32::from_le_bytes(self.termios.lock()[4..8].try_into().unwrap())
    }

    fn hung_up_for_master(&self) -> bool {
        self.slave_seen.load(Ordering::SeqCst) && self.slave_opens.load(Ordering::SeqCst) == 0
    }

    /// Wait until `q` has data (or `done` says to stop); take up to
    /// buf.len() bytes. `eof` is the result once the other side is gone.
    fn read_from(&self, q: &Mutex<VecDeque<u8>>, buf: &mut [u8], nonblock: bool, gone: impl Fn() -> bool, eof: KResult<usize>) -> KResult<usize> {
        let mut res = Err(EAGAIN);
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            let mut d = q.lock();
            if !d.is_empty() {
                let n = buf.len().min(d.len());
                for (i, b) in d.drain(..n).enumerate() {
                    buf[i] = b;
                }
                res = Ok(n);
                return true;
            }
            if gone() {
                res = eof;
                return true;
            }
            nonblock
        });
        match w {
            Wake::Ready => {
                if res.is_ok() {
                    self.wq.wake(WRITE_EVENTS);
                }
                res
            }
            Wake::Timeout => Err(EAGAIN),
            Wake::Interrupted => Err(ERESTARTSYS),
        }
    }

    fn write_to(&self, q: &Mutex<VecDeque<u8>>, data: &[u8], nonblock: bool, gone: impl Fn() -> bool) -> KResult<usize> {
        let mut done = 0;
        while done < data.len() {
            let mut res = Ok(0);
            let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
                if gone() {
                    res = Err(EIO);
                    return true;
                }
                let mut d = q.lock();
                let room = BUF_MAX.saturating_sub(d.len());
                if room > 0 {
                    let n = room.min(data.len() - done);
                    d.extend(&data[done..done + n]);
                    res = Ok(n);
                    return true;
                }
                nonblock
            });
            match (w, res) {
                (Wake::Ready, Ok(0)) => return if done > 0 { Ok(done) } else { Err(EAGAIN) },
                (Wake::Ready, Ok(n)) => {
                    done += n;
                    self.wq.wake(READ_EVENTS);
                }
                (Wake::Ready, Err(e)) => return if done > 0 { Ok(done) } else { Err(e) },
                _ => return if done > 0 { Ok(done) } else { Err(ERESTARTSYS) },
            }
        }
        Ok(done)
    }

    /// Ioctls both ends answer.
    fn common_ioctl(&self, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = crate::process::current().mm();
        match cmd {
            TCGETS => {
                mm.copy_out(arg, &*self.termios.lock())?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF => {
                let mut t = [0u8; 36];
                mm.copy_in(arg, &mut t)?;
                *self.termios.lock() = t;
                if cmd == TCSETSF {
                    self.input.lock().clear();
                }
                Ok(0)
            }
            TIOCGWINSZ => {
                mm.copy_out(arg, &*self.winsize.lock())?;
                Ok(0)
            }
            TIOCSWINSZ => {
                let mut w = [0u8; 8];
                mm.copy_in(arg, &mut w)?;
                *self.winsize.lock() = w;
                Ok(0)
            }
            TIOCGPGRP => put_int(arg, self.pgrp.load(Ordering::Relaxed)),
            TIOCSPGRP => {
                self.pgrp.store(user_int(arg)?, Ordering::Relaxed);
                Ok(0)
            }
            TIOCSCTTY | TIOCNOTTY => Ok(0),
            _ => Err(ENOTTY),
        }
    }
}

struct Master(Arc<Pty>);
struct Slave(Arc<Pty>);

impl FileOps for Master {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let p = &self.0;
        p.read_from(&p.output, buf, f.nonblock(), || p.hung_up_for_master(), Err(EIO))
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let p = &self.0;
        p.write_to(&p.input, data, f.nonblock(), || false)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let p = &self.0;
        match cmd {
            TIOCGPTN => put_int(arg, p.index),
            TIOCSPTLCK => {
                p.locked.store(user_int(arg)? != 0, Ordering::SeqCst);
                Ok(0)
            }
            TIOCGPTLCK => put_int(arg, p.locked.load(Ordering::SeqCst) as u32),
            TIOCGPTPEER => {
                let flags = arg as u32;
                let f = slave_file(p, flags)?;
                Ok(crate::process::current().process.files().alloc(f, flags & super::file::O_CLOEXEC != 0, 0)? as u64)
            }
            FIONREAD => put_int(arg, p.output.lock().len() as u32),
            _ => p.common_ioctl(cmd, arg),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let p = &self.0;
        let mut m = 0;
        if !p.output.lock().is_empty() {
            m |= READ_EVENTS;
        }
        if p.input.lock().len() < BUF_MAX {
            m |= WRITE_EVENTS;
        }
        if p.hung_up_for_master() {
            m |= POLLHUP;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        let p = &self.0;
        p.master_open.store(false, Ordering::SeqCst);
        PTYS.lock().remove(&p.index);
        let _ = devpts_root().unlink(format!("{}", p.index).as_bytes(), false);
        p.wq.wake(READ_EVENTS | POLLHUP);
    }
}

impl FileOps for Slave {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let p = &self.0;
        p.read_from(&p.input, buf, f.nonblock(), || !p.master_open.load(Ordering::SeqCst), Ok(0))
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let p = &self.0;
        let gone = || !p.master_open.load(Ordering::SeqCst);
        if p.oflag() & (OPOST | ONLCR) == OPOST | ONLCR && data.contains(&b'\n') {
            let mut out = alloc::vec::Vec::with_capacity(data.len() + 16);
            for &b in data {
                if b == b'\n' {
                    out.push(b'\r');
                }
                out.push(b);
            }
            p.write_to(&p.output, &out, f.nonblock(), gone)?;
            return Ok(data.len());
        }
        p.write_to(&p.output, data, f.nonblock(), gone)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let p = &self.0;
        match cmd {
            FIONREAD => put_int(arg, p.input.lock().len() as u32),
            TIOCGPTN => Err(ENOTTY),
            _ => p.common_ioctl(cmd, arg),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let p = &self.0;
        let mut m = 0;
        if !p.input.lock().is_empty() {
            m |= READ_EVENTS;
        }
        if !p.master_open.load(Ordering::SeqCst) {
            m |= POLLHUP | READ_EVENTS;
        } else if p.output.lock().len() < BUF_MAX {
            m |= WRITE_EVENTS;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for Slave {
    fn drop(&mut self) {
        self.0.slave_opens.fetch_sub(1, Ordering::SeqCst);
        self.0.wq.wake(READ_EVENTS | POLLHUP);
    }
}

fn new_slave(p: &Arc<Pty>) -> KResult<Box<dyn FileOps>> {
    if p.locked.load(Ordering::SeqCst) {
        return Err(EIO);
    }
    if !p.master_open.load(Ordering::SeqCst) {
        return Err(EIO);
    }
    p.slave_opens.fetch_add(1, Ordering::SeqCst);
    p.slave_seen.store(true, Ordering::SeqCst);
    Ok(Box::new(Slave(p.clone())))
}

fn slave_file(p: &Arc<Pty>, flags: u32) -> KResult<Arc<OpenFile>> {
    let ops = new_slave(p)?;
    let inode: Arc<dyn Inode> = AnonInode::new(S_IFCHR | 0o620, encode_dev(PTS_MAJOR, p.index));
    Ok(OpenFile::new(inode, None, ops, O_RDWR | (flags & O_NONBLOCK)))
}

/// Open of /dev/ptmx: a new pair, its master end.
pub fn open_master() -> KResult<Box<dyn FileOps>> {
    let mut all = PTYS.lock();
    all.retain(|_, w| w.strong_count() > 0);
    let index = (0..MAX_PTYS).find(|i| !all.contains_key(i)).ok_or(ENOSPC)?;
    let p = Arc::new(Pty {
        index,
        locked: AtomicBool::new(true),
        input: Mutex::new(VecDeque::new()),
        output: Mutex::new(VecDeque::new()),
        master_open: AtomicBool::new(true),
        slave_opens: AtomicU32::new(0),
        slave_seen: AtomicBool::new(false),
        wq: WaitQueue::new(),
        termios: Mutex::new(default_termios()),
        winsize: Mutex::new([0; 8]),
        pgrp: AtomicU32::new(0),
    });
    all.insert(index, Arc::downgrade(&p));
    drop(all);
    let _ = devpts_root().replace(format!("{index}").as_bytes(), S_IFCHR | 0o620, encode_dev(PTS_MAJOR, index), None);
    Ok(Box::new(Master(p)))
}

/// Open of /dev/pts/N.
pub fn open_slave(index: u32) -> KResult<Box<dyn FileOps>> {
    let p = PTYS.lock().get(&index).and_then(Weak::upgrade).ok_or(EIO)?;
    new_slave(&p)
}
