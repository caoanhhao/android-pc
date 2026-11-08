//! inotify: watches on inodes, events queued on the inotify file.
//!
//! Watches are keyed by inode (device, inode number) in one global table
//! of (key, instance, wd, mask). The VFS reports changes:
//! create/mkdir/mknod/symlink/link (IN_CREATE), unlink/rmdir (IN_DELETE on
//! the directory, IN_DELETE_SELF + IN_IGNORED on a watched victim whose
//! last link went), rename (IN_MOVED_FROM/IN_MOVED_TO with a shared cookie,
//! IN_MOVE_SELF), write (IN_MODIFY), the last close of a file opened for
//! writing (IN_CLOSE_WRITE, else IN_CLOSE_NOWRITE), chmod/chown/utimes/
//! xattr changes (IN_ATTRIB). Events about a directory's child go to the
//! directory's watches with the child's name (and IN_ISDIR for
//! directories). IN_ONESHOT, IN_MASK_ADD, IN_ONLYDIR, IN_DONT_FOLLOW and
//! IN_MASK_CREATE are honoured; identical consecutive events coalesce;
//! the queue holds 16384 events, then IN_Q_OVERFLOW.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::file::{FileOps, O_NONBLOCK, O_RDONLY, OpenFile};
use super::poll::{READ_EVENTS, WaitQueue};
use super::vfs::Inode;
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};

pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
pub const IN_CLOSE_WRITE: u32 = 0x8;
pub const IN_CLOSE_NOWRITE: u32 = 0x10;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
pub const IN_MOVE_SELF: u32 = 0x800;
pub const IN_ALL_EVENTS: u32 = 0xfff;
pub const IN_Q_OVERFLOW: u32 = 0x4000;
pub const IN_IGNORED: u32 = 0x8000;
pub const IN_ONLYDIR: u32 = 0x0100_0000;
pub const IN_DONT_FOLLOW: u32 = 0x0200_0000;
pub const IN_EXCL_UNLINK: u32 = 0x0400_0000;
pub const IN_MASK_CREATE: u32 = 0x1000_0000;
pub const IN_MASK_ADD: u32 = 0x2000_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;
pub const IN_ONESHOT: u32 = 0x8000_0000;

const MAX_EVENTS: usize = 16384;
const EVENT_HDR: usize = 16;

type Key = (u64, u64);

fn key_of(inode: &dyn Inode) -> Key {
    (inode.meta().dev, inode.meta().ino)
}

struct Watch {
    key: Key,
    inst: Weak<Instance>,
    wd: i32,
    mask: u32,
}

static WATCHES: Mutex<Vec<Watch>> = Mutex::new(Vec::new());
static COOKIE: AtomicU32 = AtomicU32::new(1);

struct State {
    next_wd: i32,
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    overflowed: bool,
}

pub struct Instance {
    wq: Arc<WaitQueue>,
    st: Mutex<State>,
}

fn event(wd: i32, mask: u32, cookie: u32, name: &[u8]) -> Vec<u8> {
    let len = if name.is_empty() { 0 } else { (name.len() + 1).next_multiple_of(EVENT_HDR) };
    let mut e = Vec::with_capacity(EVENT_HDR + len);
    e.extend_from_slice(&wd.to_le_bytes());
    e.extend_from_slice(&mask.to_le_bytes());
    e.extend_from_slice(&cookie.to_le_bytes());
    e.extend_from_slice(&(len as u32).to_le_bytes());
    e.extend_from_slice(name);
    e.resize(EVENT_HDR + len, 0);
    e
}

impl Instance {
    fn queue(&self, ev: Vec<u8>) {
        {
            let mut s = self.st.lock();
            if s.queue.back() == Some(&ev) {
                return;
            }
            if s.queue.len() >= MAX_EVENTS {
                if !s.overflowed {
                    s.overflowed = true;
                    let o = event(-1, IN_Q_OVERFLOW, 0, b"");
                    s.bytes += o.len();
                    s.queue.push_back(o);
                }
            } else {
                s.bytes += ev.len();
                s.queue.push_back(ev);
            }
        }
        self.wq.wake(READ_EVENTS);
    }
}

/// inotify_init1: a new instance as an open file.
pub fn new_file(nonblock: bool) -> Arc<OpenFile> {
    let inst = Arc::new(Instance {
        wq: WaitQueue::new(),
        st: Mutex::new(State { next_wd: 1, queue: VecDeque::new(), bytes: 0, overflowed: false }),
    });
    OpenFile::new(super::anon_inode(), None, Box::new(InotifyFile(inst)), O_RDONLY | if nonblock { O_NONBLOCK } else { 0 })
}

fn instance_of(f: &OpenFile) -> KResult<Arc<Instance>> {
    f.ops.as_any().downcast_ref::<InotifyFile>().map(|i| i.0.clone()).ok_or(EINVAL)
}

/// inotify_add_watch on an already resolved inode.
pub fn add_watch(f: &OpenFile, inode: &Arc<dyn Inode>, mask: u32) -> KResult<i32> {
    let inst = instance_of(f)?;
    if mask & IN_ALL_EVENTS == 0 {
        return Err(EINVAL);
    }
    if mask & IN_MASK_ADD != 0 && mask & IN_MASK_CREATE != 0 {
        return Err(EINVAL);
    }
    if mask & IN_ONLYDIR != 0 && !inode.meta().is_dir() {
        return Err(ENOTDIR);
    }
    let key = key_of(&**inode);
    let keep = mask & (IN_ALL_EVENTS | IN_ONESHOT | IN_EXCL_UNLINK);
    let mut ws = WATCHES.lock();
    ws.retain(|w| w.inst.strong_count() > 0);
    if let Some(w) = ws.iter_mut().find(|w| w.key == key && w.inst.as_ptr() == Arc::as_ptr(&inst)) {
        if mask & IN_MASK_CREATE != 0 {
            return Err(EEXIST);
        }
        w.mask = if mask & IN_MASK_ADD != 0 { w.mask | keep } else { keep };
        return Ok(w.wd);
    }
    let wd = {
        let mut s = inst.st.lock();
        let wd = s.next_wd;
        s.next_wd += 1;
        wd
    };
    ws.push(Watch { key, inst: Arc::downgrade(&inst), wd, mask: keep });
    Ok(wd)
}

pub fn rm_watch(f: &OpenFile, wd: i32) -> KResult<()> {
    let inst = instance_of(f)?;
    let mut ws = WATCHES.lock();
    let i = ws.iter().position(|w| w.wd == wd && w.inst.as_ptr() == Arc::as_ptr(&inst)).ok_or(EINVAL)?;
    ws.remove(i);
    drop(ws);
    inst.queue(event(wd, IN_IGNORED, 0, b""));
    Ok(())
}

/// Deliver `mask` (with `name` for a child, cookie for renames) to the
/// watches on `key`. `remove_after` drops them afterwards with IN_IGNORED.
fn deliver(key: Key, mask: u32, cookie: u32, name: &[u8], remove_after: bool) {
    let mut out: Vec<(Arc<Instance>, Vec<u8>)> = Vec::new();
    {
        let mut ws = WATCHES.lock();
        if ws.is_empty() {
            return;
        }
        let mut i = 0;
        while i < ws.len() {
            let w = &ws[i];
            let hit = w.key == key && w.mask & mask & IN_ALL_EVENTS != 0;
            let Some(inst) = w.inst.upgrade() else {
                ws.remove(i);
                continue;
            };
            if w.key == key && (hit || remove_after) {
                if hit {
                    out.push((inst.clone(), event(w.wd, mask, cookie, name)));
                }
                if remove_after || (hit && w.mask & IN_ONESHOT != 0) {
                    out.push((inst, event(w.wd, IN_IGNORED, 0, b"")));
                    ws.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }
    for (inst, ev) in out {
        inst.queue(ev);
    }
}

fn dir_bit(child: &dyn Inode) -> u32 {
    if child.meta().is_dir() { IN_ISDIR } else { 0 }
}

/// `name` was created in `dir`.
pub fn created(dir: &dyn Inode, name: &[u8], child: &dyn Inode) {
    deliver(key_of(dir), IN_CREATE | dir_bit(child), 0, name, false);
}

/// `name` (the inode `child`) was removed from `dir`.
pub fn deleted(dir: &dyn Inode, name: &[u8], child: &dyn Inode) {
    let isdir = dir_bit(child);
    deliver(key_of(dir), IN_DELETE | isdir, 0, name, false);
    if isdir != 0 || child.meta().attr().nlink == 0 {
        deliver(key_of(child), IN_DELETE_SELF, 0, b"", true);
    }
}

/// `child` moved from `old_dir/old` to `new_dir/new`.
pub fn moved(old_dir: &dyn Inode, old: &[u8], new_dir: &dyn Inode, new: &[u8], child: &dyn Inode) {
    let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
    let isdir = dir_bit(child);
    deliver(key_of(old_dir), IN_MOVED_FROM | isdir, cookie, old, false);
    deliver(key_of(new_dir), IN_MOVED_TO | isdir, cookie, new, false);
    deliver(key_of(child), IN_MOVE_SELF, 0, b"", false);
}

/// An event on the file itself, and on its directory with its name when
/// the path it was reached by is known.
pub fn touched(inode: &dyn Inode, path: Option<&super::vfs::PathRef>, mask: u32) {
    if WATCHES.lock().is_empty() {
        return;
    }
    let mask = mask | dir_bit(inode);
    deliver(key_of(inode), mask, 0, b"", false);
    if let Some(p) = path
        && let Some(parent) = p.dentry.parent()
    {
        deliver(key_of(&*parent.inode), mask, 0, &p.dentry.name(), false);
    }
}

pub struct InotifyFile(Arc<Instance>);

const FIONREAD: u32 = 0x541b;

impl FileOps for InotifyFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let inst = &self.0;
        let mut res = Err(EAGAIN);
        let w = sched::wait_until(inst.wq.key(), None, WaitMode::Interruptible, || {
            let mut s = inst.st.lock();
            let Some(first) = s.queue.front() else { return f.nonblock() };
            if first.len() > buf.len() {
                res = Err(EINVAL);
                return true;
            }
            let mut n = 0;
            while let Some(e) = s.queue.front() {
                if n + e.len() > buf.len() {
                    break;
                }
                let e = s.queue.pop_front().unwrap();
                buf[n..n + e.len()].copy_from_slice(&e);
                n += e.len();
                s.bytes -= e.len();
            }
            if s.queue.is_empty() {
                s.overflowed = false;
            }
            res = Ok(n);
            true
        });
        match w {
            Wake::Ready => res,
            Wake::Timeout => Err(EAGAIN),
            Wake::Interrupted => Err(ERESTARTSYS),
        }
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd != FIONREAD {
            return Err(ENOTTY);
        }
        let n = self.0.st.lock().bytes as i32;
        crate::process::current().mm().copy_out(arg, &n.to_le_bytes())?;
        Ok(0)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        if self.0.st.lock().queue.is_empty() { 0 } else { READ_EVENTS }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("inotify")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        let me = Arc::as_ptr(&self.0);
        let mut s = String::new();
        for w in WATCHES.lock().iter().filter(|w| w.inst.as_ptr() == me) {
            s.push_str(&alloc::format!(
                "inotify wd:{} ino:{:x} sdev:{:x} mask:{:x} ignored_mask:0\n",
                w.wd,
                w.key.1,
                w.key.0,
                w.mask
            ));
        }
        s
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for InotifyFile {
    fn drop(&mut self) {
        let me = Arc::as_ptr(&self.0);
        WATCHES.lock().retain(|w| w.inst.as_ptr() != me);
    }
}
