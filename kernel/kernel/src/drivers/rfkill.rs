//! rfkill: radio kill switches (`/dev/rfkill`, misc 10:242, and
//! /sys/class/rfkill/rfkillN), what wpa_supplicant and Android's Wi-Fi
//! and Bluetooth stacks read to learn whether a radio is blocked.
//!
//! Radios register with [`register`] (soft and hard unblocked). An open
//! /dev/rfkill first reads an RFKILL_OP_ADD event per radio, then a
//! CHANGE event whenever a block changes. Writing RFKILL_OP_CHANGE (one
//! radio) or RFKILL_OP_CHANGE_ALL (a type) sets the soft block, which is
//! passed to the radio's owner (`net::wireless`). Events are
//! `struct rfkill_event_ext` (9 bytes; shorter reads get the old 8-byte
//! layout).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

pub const RFKILL_MINOR: u32 = 242;

pub const TYPE_ALL: u8 = 0;
pub const TYPE_WLAN: u8 = 1;
pub const TYPE_BLUETOOTH: u8 = 2;

const OP_ADD: u8 = 0;
const OP_CHANGE: u8 = 2;
const OP_CHANGE_ALL: u8 = 3;
const EVENT_SIZE: usize = 9;

struct Radio {
    idx: u32,
    ty: u8,
    name: String,
    soft: bool,
    hard: bool,
}

struct Reader {
    q: Mutex<VecDeque<[u8; EVENT_SIZE]>>,
    wq: Arc<WaitQueue>,
}

static RADIOS: Mutex<Vec<Radio>> = Mutex::new(Vec::new());
static READERS: Mutex<Vec<Weak<Reader>>> = Mutex::new(Vec::new());

fn type_name(ty: u8) -> &'static str {
    match ty {
        TYPE_WLAN => "wlan",
        TYPE_BLUETOOTH => "bluetooth",
        _ => "unknown",
    }
}

fn event(r: &Radio, op: u8) -> [u8; EVENT_SIZE] {
    let mut e = [0u8; EVENT_SIZE];
    e[..4].copy_from_slice(&r.idx.to_le_bytes());
    e[4] = r.ty;
    e[5] = op;
    e[6] = r.soft as u8;
    e[7] = r.hard as u8;
    e
}

fn broadcast(e: [u8; EVENT_SIZE]) {
    let readers: Vec<Arc<Reader>> = {
        let mut g = READERS.lock();
        g.retain(|w| w.strong_count() > 0);
        g.iter().filter_map(Weak::upgrade).collect()
    };
    for r in readers {
        r.q.lock().push_back(e);
        r.wq.wake(READ_EVENTS);
    }
}

fn sysfs_state(r: &Radio) {
    let dir = format!("devices/virtual/rfkill/rfkill{}", r.idx);
    let state = if r.hard { 2 } else if r.soft { 0 } else { 1 };
    crate::fs::sysfs::set(&dir, "state", &format!("{state}\n"));
    crate::fs::sysfs::set(&dir, "soft", &format!("{}\n", r.soft as u8));
    crate::fs::sysfs::set(&dir, "hard", &format!("{}\n", r.hard as u8));
}

/// A new radio of type `ty` named `name` (e.g. "phy0"); its index.
pub fn register(ty: u8, name: &str) -> u32 {
    let r = {
        let mut g = RADIOS.lock();
        let idx = g.len() as u32;
        g.push(Radio { idx, ty, name: String::from(name), soft: false, hard: false });
        let r = g.last().unwrap();
        let dir = format!("devices/virtual/rfkill/rfkill{idx}");
        crate::fs::sysfs::add_device(
            &dir,
            "rfkill",
            "class/rfkill",
            format!("RFKILL_NAME={name}\nRFKILL_TYPE={}\nRFKILL_STATE=1\n", type_name(ty)),
            true,
        );
        crate::fs::sysfs::set(&dir, "name", &format!("{name}\n"));
        crate::fs::sysfs::set(&dir, "type", &format!("{}\n", type_name(ty)));
        crate::fs::sysfs::set(&dir, "index", &format!("{idx}\n"));
        crate::fs::sysfs::set(&dir, "persistent", "0\n");
        sysfs_state(r);
        event(r, OP_ADD)
    };
    broadcast(r);
    u32::from_le_bytes(r[..4].try_into().unwrap())
}

/// Whether radio `idx` is blocked (soft or hard).
pub fn blocked(idx: u32) -> bool {
    RADIOS.lock().iter().find(|r| r.idx == idx).is_some_and(|r| r.soft || r.hard)
}

fn set_soft(pick: impl Fn(&Radio) -> bool, soft: bool) {
    let mut changed = Vec::new();
    {
        let mut g = RADIOS.lock();
        for r in g.iter_mut().filter(|r| pick(r)) {
            if r.soft != soft {
                r.soft = soft;
                sysfs_state(r);
                changed.push((r.idx, r.ty, event(r, OP_CHANGE)));
            }
        }
    }
    for (idx, ty, e) in changed {
        broadcast(e);
        if ty == TYPE_WLAN {
            crate::net::wireless::rfkill_changed(idx, soft);
        }
    }
}

pub fn open() -> KResult<Box<dyn FileOps>> {
    let r = Arc::new(Reader { q: Mutex::new(VecDeque::new()), wq: WaitQueue::new() });
    {
        let g = RADIOS.lock();
        let mut q = r.q.lock();
        for radio in g.iter() {
            q.push_back(event(radio, OP_ADD));
        }
    }
    READERS.lock().push(Arc::downgrade(&r));
    Ok(Box::new(RfkillFile(r)))
}

struct RfkillFile(Arc<Reader>);

impl FileOps for RfkillFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut ev = None;
        let w = sched::wait_until(self.0.wq.key(), None, WaitMode::Interruptible, || {
            ev = self.0.q.lock().pop_front();
            ev.is_some() || f.nonblock()
        });
        if w == Wake::Interrupted {
            return Err(ERESTARTSYS);
        }
        let e = ev.ok_or(EAGAIN)?;
        let n = buf.len().min(EVENT_SIZE);
        buf[..n].copy_from_slice(&e[..n]);
        Ok(n)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        if data.len() < 8 {
            return Err(EINVAL);
        }
        let idx = u32::from_le_bytes(data[..4].try_into().unwrap());
        let (ty, op, soft) = (data[4], data[5], data[6] != 0);
        match op {
            OP_CHANGE => set_soft(|r| r.idx == idx, soft),
            OP_CHANGE_ALL => set_soft(|r| ty == TYPE_ALL || r.ty == ty, soft),
            _ => return Err(EINVAL),
        }
        Ok(data.len())
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        WRITE_EVENTS | if self.0.q.lock().is_empty() { 0 } else { READ_EVENTS }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
