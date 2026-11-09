//! Fences and sync_file fds (`anon_inode:sync_file`), plus sw_sync
//! timelines for tests.
//!
//! A [`Fence`] is a point on a timeline (context, seqno) that signals once
//! (status 1, or a negative error). A sync file holds one or more fences
//! and polls POLLIN once all of them have signalled; its ioctls are
//! SYNC_IOC_MERGE, SYNC_IOC_FILE_INFO and SYNC_IOC_SET_DEADLINE. Every
//! signal wakes one global queue (sync files have no per-file queue; poll
//! re-checks).
//!
//! sw_sync (Linux's debugfs `sync/sw_sync`, here the misc device
//! 10:[`SW_SYNC_MINOR`] at /sys/kernel/debug/sync/sw_sync): each open is
//! a timeline, SW_SYNC_IOC_CREATE_FENCE makes a sync file for a future
//! value and SW_SYNC_IOC_INC advances it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, Once};

use crate::errno::*;
use crate::fs::file::{FileOps, O_RDWR, OpenFile};
use crate::fs::poll::{POLLIN, WaitQueue};
use crate::process::current;
use crate::sched::{self, Wake, WaitMode};

pub const SW_SYNC_MINOR: u32 = 250;

const SYNC_IOC_MERGE: u32 = 0xc030_3e03;
const SYNC_IOC_FILE_INFO: u32 = 0xc038_3e04;
const SYNC_IOC_SET_DEADLINE: u32 = 0x4010_3e05;
const SW_SYNC_IOC_CREATE_FENCE: u32 = 0xc028_5700;
const SW_SYNC_IOC_INC: u32 = 0x4004_5701;

static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// A new fence context (one per timeline).
pub fn new_context() -> u64 {
    NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed)
}

fn queue() -> &'static Arc<WaitQueue> {
    static Q: Once<Arc<WaitQueue>> = Once::new();
    Q.call_once(WaitQueue::new)
}

#[derive(Clone, Copy)]
struct State {
    /// 0 pending, 1 signalled, < 0 signalled with an error.
    status: i32,
    timestamp_ns: u64,
}

pub struct Fence {
    pub context: u64,
    pub seqno: u64,
    pub driver: &'static str,
    pub timeline: String,
    state: Mutex<State>,
}

impl Fence {
    pub fn new(context: u64, seqno: u64, driver: &'static str, timeline: String) -> Arc<Fence> {
        Arc::new(Fence { context, seqno, driver, timeline, state: Mutex::new(State { status: 0, timestamp_ns: 0 }) })
    }

    /// An already signalled fence (synchronous drivers).
    pub fn signalled(context: u64, seqno: u64, driver: &'static str, timeline: String) -> Arc<Fence> {
        let f = Self::new(context, seqno, driver, timeline);
        f.signal(1);
        f
    }

    pub fn status(&self) -> i32 {
        self.state.lock().status
    }

    /// Signal once with `status` (1, or a negative errno).
    pub fn signal(&self, status: i32) {
        {
            let mut s = self.state.lock();
            if s.status != 0 {
                return;
            }
            *s = State { status, timestamp_ns: crate::time::monotonic_ns() };
        }
        queue().wake(POLLIN);
    }
}

/// An open sync file.
pub struct SyncFile {
    name: Mutex<[u8; 32]>,
    pub fences: Vec<Arc<Fence>>,
}

fn name32(s: &[u8]) -> [u8; 32] {
    let mut n = [0u8; 32];
    let l = s.iter().position(|&c| c == 0).unwrap_or(s.len()).min(31);
    n[..l].copy_from_slice(&s[..l]);
    n
}

impl SyncFile {
    /// File status: 1 when every fence signalled, the first error if one
    /// failed, else 0.
    pub fn status(&self) -> i32 {
        let mut st = 1;
        for f in &self.fences {
            match f.status() {
                0 => st = 0,
                e if e < 0 => return e,
                _ => {}
            }
        }
        st
    }

    /// Wait until signalled or `timeout_ns` (None: forever); Ok(true) if
    /// signalled.
    pub fn wait(&self, timeout_ns: Option<u64>) -> KResult<bool> {
        let deadline = timeout_ns.map(|t| crate::time::monotonic_ns().saturating_add(t));
        match sched::wait_until(queue().key(), deadline, WaitMode::Interruptible, || self.status() != 0) {
            Wake::Ready => Ok(true),
            Wake::Timeout => Ok(false),
            Wake::Interrupted => Err(ERESTARTSYS),
        }
    }
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// The fences of a merge: both sets, one fence per context (the later
/// seqno), like Linux's sync_file_merge.
fn merge(a: &[Arc<Fence>], b: &[Arc<Fence>]) -> Vec<Arc<Fence>> {
    let mut out: Vec<Arc<Fence>> = Vec::new();
    for f in a.iter().chain(b) {
        match out.iter_mut().find(|g| g.context == f.context) {
            Some(g) if f.seqno > g.seqno => *g = f.clone(),
            Some(_) => {}
            None => out.push(f.clone()),
        }
    }
    out
}

impl FileOps for SyncFile {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        match cmd {
            SYNC_IOC_MERGE => {
                let mut b = [0u8; 48];
                mm.copy_in(arg, &mut b)?;
                if rd32(&b, 40) != 0 || rd32(&b, 44) != 0 {
                    return Err(EINVAL);
                }
                let other = current().process.files().get(rd32(&b, 32) as i32)?;
                let other = other.ops.as_any().downcast_ref::<SyncFile>().ok_or(EINVAL)?;
                let file = new_file(&b[..32], merge(&self.fences, &other.fences));
                let fd = current().process.files().alloc(file, true, 0)?;
                mm.copy_out(arg + 36, &fd.to_le_bytes())?;
                Ok(0)
            }
            SYNC_IOC_FILE_INFO => {
                let mut b = [0u8; 56];
                mm.copy_in(arg, &mut b)?;
                if rd32(&b, 36) != 0 {
                    return Err(EINVAL);
                }
                let (num, ptr) = (rd32(&b, 40) as usize, rd64(&b, 48));
                if num != 0 && num < self.fences.len() {
                    return Err(EINVAL);
                }
                if num != 0 {
                    let mut info = Vec::with_capacity(self.fences.len() * 80);
                    for f in &self.fences {
                        let s = *f.state.lock();
                        let mut e = [0u8; 80];
                        e[..32].copy_from_slice(&name32(f.timeline.as_bytes()));
                        e[32..64].copy_from_slice(&name32(f.driver.as_bytes()));
                        e[64..68].copy_from_slice(&s.status.to_le_bytes());
                        if s.status != 0 {
                            e[72..80].copy_from_slice(&s.timestamp_ns.to_le_bytes());
                        }
                        info.extend_from_slice(&e);
                    }
                    mm.copy_out(ptr, &info)?;
                }
                b[..32].copy_from_slice(&*self.name.lock());
                b[32..36].copy_from_slice(&self.status().to_le_bytes());
                b[40..44].copy_from_slice(&(self.fences.len() as u32).to_le_bytes());
                mm.copy_out(arg, &b)?;
                Ok(0)
            }
            SYNC_IOC_SET_DEADLINE => {
                let mut b = [0u8; 16];
                mm.copy_in(arg, &mut b)?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        if self.status() != 0 { POLLIN } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(queue().clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("sync_file")
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new sync file holding `fences`.
pub fn new_file(name: &[u8], fences: Vec<Arc<Fence>>) -> Arc<OpenFile> {
    let ops = SyncFile { name: Mutex::new(name32(name)), fences };
    OpenFile::new(crate::fs::anon_inode(), None, Box::new(ops), O_RDWR)
}

/// A sync file with one already signalled fence, installed as a new fd
/// (CLOEXEC) of the current process.
pub fn signalled_fd(context: u64, seqno: u64, driver: &'static str, timeline: &str) -> KResult<i32> {
    let f = Fence::signalled(context, seqno, driver, timeline.into());
    current().process.files().alloc(new_file(timeline.as_bytes(), alloc::vec![f]), true, 0)
}

/// The sync file behind fd `fd` of the current process.
pub fn get_fd(fd: i32) -> KResult<Arc<OpenFile>> {
    let f = current().process.files().get(fd)?;
    if f.ops.as_any().downcast_ref::<SyncFile>().is_none() {
        return Err(EINVAL);
    }
    Ok(f)
}

// ---------------------------------------------------------------------------
// sw_sync

struct Timeline {
    value: u32,
    fences: Vec<Weak<Fence>>,
}

pub struct SwSync {
    context: u64,
    tl: Mutex<Timeline>,
}

impl SwSync {
    fn advance(&self, by: u32) {
        let ready: Vec<Arc<Fence>> = {
            let mut tl = self.tl.lock();
            tl.value = tl.value.wrapping_add(by);
            let v = tl.value as u64;
            tl.fences.retain(|w| w.strong_count() > 0);
            tl.fences.iter().filter_map(Weak::upgrade).filter(|f| f.seqno <= v).collect()
        };
        for f in ready {
            f.signal(1);
        }
    }
}

impl FileOps for SwSync {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        match cmd {
            SW_SYNC_IOC_CREATE_FENCE => {
                let mut b = [0u8; 40];
                mm.copy_in(arg, &mut b)?;
                let value = rd32(&b, 0);
                let name = name32(&b[4..36]);
                let len = name.iter().position(|&c| c == 0).unwrap_or(32);
                let timeline = String::from_utf8_lossy(&name[..len]).into_owned();
                let f = Fence::new(self.context, value as u64, "sw_sync", timeline);
                {
                    let mut tl = self.tl.lock();
                    if value <= tl.value {
                        f.signal(1);
                    } else {
                        tl.fences.push(Arc::downgrade(&f));
                    }
                }
                let file = new_file(&name, alloc::vec![f]);
                let fd = current().process.files().alloc(file, true, 0)?;
                mm.copy_out(arg + 36, &fd.to_le_bytes())?;
                Ok(0)
            }
            SW_SYNC_IOC_INC => {
                let mut b = [0u8; 4];
                mm.copy_in(arg, &mut b)?;
                self.advance(u32::from_le_bytes(b));
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for SwSync {
    /// Closing a timeline signals what is left with an error (Linux
    /// signals them; ENOENT-style status keeps waiters from hanging).
    fn drop(&mut self) {
        for f in self.tl.get_mut().fences.iter().filter_map(Weak::upgrade) {
            f.signal(-ENOENT.0);
        }
    }
}

/// A new sw_sync timeline (open of the sw_sync node).
pub fn open_sw_sync() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(SwSync { context: new_context(), tl: Mutex::new(Timeline { value: 0, fences: Vec::new() }) }))
}
