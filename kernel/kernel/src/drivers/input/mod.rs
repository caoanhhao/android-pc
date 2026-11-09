//! The input core: input devices, their evdev nodes (/dev/input/eventN,
//! 13:64+N) and /dev/uinput (10:223, `uinput`).
//!
//! A device ([`InputDev`]) is registered with its capabilities (name, id,
//! property and event bits, absolute axis ranges) by a driver (virtio-input,
//! uinput). It appears in sysfs as `<parent>/input/inputN` (class "input",
//! with the PRODUCT/NAME/EV/KEY/... uevent variables Linux sends) with an
//! `eventN` child carrying MAJOR/MINOR/DEVNAME=input/eventN, so ueventd
//! creates the node; devtmpfs gets it too.
//!
//! The driver reports events with [`InputDev::event`]. As on Linux, events
//! that change nothing (a key already down, the same absolute value, a
//! zero relative motion) are dropped, the key/LED/switch/axis/MT-slot state
//! is tracked (EVIOCGKEY, EVIOCGABS, EVIOCGMTSLOTS, ...), and events are
//! collected into packets that reach the clients at SYN_REPORT. Every open
//! of an event node is a client with its own queue (`EVDEV_BUF` events; on
//! overflow the queue is replaced by SYN_DROPPED), clock (EVIOCSCLOCKID),
//! event mask (EVIOCSMASK) and grab (EVIOCGRAB: only the grabbing client
//! gets events). Writing input_events to a node injects them (`sendevent`);
//! LED and sound events also go to the driver ([`Sink`]).

pub mod uinput;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLERR, POLLHUP, READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

pub const INPUT_MAJOR: u32 = 13;
pub const EVDEV_MINOR_BASE: u32 = 64;
const MAX_DEVICES: u32 = 32;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;
pub const EV_SW: u16 = 0x05;
pub const EV_LED: u16 = 0x11;
pub const EV_SND: u16 = 0x12;
pub const EV_REP: u16 = 0x14;
pub const EV_FF: u16 = 0x15;
pub const EV_CNT: usize = 0x20;

pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_FIRST: u16 = 0x30;
const ABS_MT_LAST: u16 = 0x3d;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const MAX_SLOTS: usize = 64;

pub const PROP_CNT: usize = 0x20;
pub const ABS_CNT: usize = 0x40;

/// Number of codes per event type (Linux *_CNT).
pub fn code_count(ty: u16) -> usize {
    match ty {
        EV_SYN => 0x10,
        EV_KEY => 0x300,
        EV_REL => 0x10,
        EV_ABS => ABS_CNT,
        EV_MSC => 0x08,
        EV_SW => 0x11,
        EV_LED => 0x10,
        EV_SND => 0x08,
        EV_REP => 0x02,
        EV_FF => 0x80,
        _ => 0,
    }
}

/// struct input_event on x86_64: timeval, type, code, value.
pub const EVENT_SIZE: usize = 24;
/// Events a client queue holds.
const EVDEV_BUF: usize = 1024;

/// A bit set of `n` bits stored as 64-bit words (Linux's unsigned long
/// bitmaps on x86_64).
#[derive(Clone, Default)]
pub struct Bits(pub Vec<u64>);

impl Bits {
    pub fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64).max(1)])
    }

    pub fn set(&mut self, i: usize, on: bool) {
        if let Some(w) = self.0.get_mut(i / 64) {
            if on {
                *w |= 1 << (i % 64);
            } else {
                *w &= !(1 << (i % 64));
            }
        }
    }

    pub fn get(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Linux's sysfs/uevent form: words high to low in hex, leading zero
    /// words left out ("120013", "1000000000007 ff9f207ac14057ff ...").
    pub fn to_sysfs(&self) -> String {
        let mut words = self.0.as_slice();
        while words.len() > 1 && words[words.len() - 1] == 0 {
            words = &words[..words.len() - 1];
        }
        let mut s = String::new();
        for (i, w) in words.iter().rev().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&format!("{w:x}"));
        }
        s
    }
}

/// struct input_absinfo: value, minimum, maximum, fuzz, flat, resolution.
#[derive(Clone, Copy, Default)]
pub struct AbsInfo(pub [i32; 6]);

/// What a device can report, fixed at registration.
#[derive(Clone)]
pub struct Caps {
    pub name: String,
    pub phys: String,
    pub uniq: String,
    /// bustype, vendor, product, version.
    pub id: [u16; 4],
    pub props: Bits,
    /// Bit `t` set: the device reports event type `t`.
    pub ev: Bits,
    /// Per event type, the codes it reports.
    pub codes: Vec<Bits>,
    pub abs: Vec<AbsInfo>,
    /// EV_REP delay and period (ms).
    pub rep: [u32; 2],
}

impl Caps {
    pub fn new(name: &str) -> Caps {
        Caps {
            name: name.into(),
            phys: String::new(),
            uniq: String::new(),
            id: [0; 4],
            props: Bits::new(PROP_CNT),
            ev: Bits::new(EV_CNT),
            codes: (0..EV_CNT as u16).map(|t| Bits::new(code_count(t))).collect(),
            abs: vec![AbsInfo::default(); ABS_CNT],
            rep: [250, 33],
        }
    }

    pub fn has(&self, ty: u16, code: u16) -> bool {
        self.ev.get(ty as usize) && self.codes.get(ty as usize).is_some_and(|b| b.get(code as usize))
    }

    fn slots(&self) -> usize {
        if self.has(EV_ABS, ABS_MT_SLOT) { (self.abs[ABS_MT_SLOT as usize].0[2].max(0) as usize + 1).min(MAX_SLOTS) } else { 0 }
    }
}

/// Where events written to a device node go besides the clients (LEDs to
/// the virtio status queue, everything to a uinput reader).
pub trait Sink: Send + Sync {
    fn output(&self, ty: u16, code: u16, value: i32);
}

#[derive(Clone, Copy)]
struct Event {
    ns: u64,
    ty: u16,
    code: u16,
    value: i32,
}

/// Current state (what the EVIOCG* state ioctls report).
struct State {
    key: Bits,
    led: Bits,
    snd: Bits,
    sw: Bits,
    /// Per MT slot, the values of ABS_MT_FIRST..=ABS_MT_LAST.
    mt: Vec<[i32; 14]>,
    slot: usize,
    /// Events since the last SYN_REPORT.
    packet: Vec<Event>,
    clients: Vec<Weak<Client>>,
    grab: Option<u64>,
}

pub struct InputDev {
    pub index: u32,
    pub caps: Caps,
    abs: Mutex<Vec<AbsInfo>>,
    st: Mutex<State>,
    sink: Option<Box<dyn Sink>>,
    dead: AtomicBool,
    /// sysfs directory of inputN (relative to /sys).
    pub sysdir: String,
}

static DEVICES: Mutex<Vec<Arc<InputDev>>> = Mutex::new(Vec::new());
static USED: AtomicU64 = AtomicU64::new(0);

fn alloc_index() -> Option<u32> {
    loop {
        let used = USED.load(Ordering::Acquire);
        let i = (0..MAX_DEVICES).find(|i| used & (1 << i) == 0)?;
        if USED.compare_exchange(used, used | 1 << i, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return Some(i);
        }
    }
}

/// The device behind event node minor `minor`.
fn by_minor(minor: u32) -> Option<Arc<InputDev>> {
    DEVICES.lock().iter().find(|d| EVDEV_MINOR_BASE + d.index == minor).cloned()
}

/// Register a device below the sysfs directory `parent` (relative to
/// /sys, e.g. a virtio device's or "devices/virtual").
pub fn register(caps: Caps, parent: &str, sink: Option<Box<dyn Sink>>) -> KResult<Arc<InputDev>> {
    use crate::fs::sysfs;
    let index = alloc_index().ok_or(ENOSPC)?;
    let sysdir = format!("{parent}/input/input{index}");
    let mut caps = caps;
    caps.ev.set(EV_SYN as usize, true);
    let slots = caps.slots();
    let dev = Arc::new(InputDev {
        index,
        abs: Mutex::new(caps.abs.clone()),
        st: Mutex::new(State {
            key: Bits::new(code_count(EV_KEY)),
            led: Bits::new(code_count(EV_LED)),
            snd: Bits::new(code_count(EV_SND)),
            sw: Bits::new(code_count(EV_SW)),
            // Slots start unused: tracking id -1, the rest 0.
            mt: vec![core::array::from_fn(|i| if i == (ABS_MT_TRACKING_ID - ABS_MT_FIRST) as usize { -1 } else { 0 }); slots.max(1)],
            slot: 0,
            packet: Vec::new(),
            clients: Vec::new(),
            grab: None,
        }),
        sink,
        dead: AtomicBool::new(false),
        sysdir: sysdir.clone(),
        caps,
    });
    let c = &dev.caps;
    let [bus, vendor, product, version] = c.id;
    let mut vars = format!(
        "PRODUCT={bus:x}/{vendor:x}/{product:x}/{version:x}\nNAME=\"{}\"\nPHYS=\"{}\"\nUNIQ=\"{}\"\nPROP={}\nEV={}\n",
        c.name,
        c.phys,
        c.uniq,
        c.props.to_sysfs(),
        c.ev.to_sysfs()
    );
    for (ty, key) in [(EV_KEY, "KEY"), (EV_REL, "REL"), (EV_ABS, "ABS"), (EV_MSC, "MSC"), (EV_LED, "LED"), (EV_SND, "SND"), (EV_FF, "FF"), (EV_SW, "SW")] {
        if c.ev.get(ty as usize) {
            vars.push_str(&format!("{key}={}\n", c.codes[ty as usize].to_sysfs()));
        }
    }
    let modalias = dev.modalias();
    vars.push_str(&format!("MODALIAS={modalias}\n"));
    sysfs::add_device(&sysdir, "input", "class/input", vars, true);
    sysfs::symlink("class/input", &format!("input{index}"), &sysfs::relative("class/input", &sysdir));
    sysfs::symlink(&sysdir, "device", "../..");
    sysfs::set(&sysdir, "name", &format!("{}\n", c.name));
    sysfs::set(&sysdir, "phys", &format!("{}\n", c.phys));
    sysfs::set(&sysdir, "uniq", &format!("{}\n", c.uniq));
    sysfs::set(&sysdir, "properties", &format!("{}\n", c.props.to_sysfs()));
    sysfs::set(&sysdir, "modalias", &format!("{modalias}\n"));
    for (f, v) in [("bustype", bus), ("vendor", vendor), ("product", product), ("version", version)] {
        sysfs::set(&format!("{sysdir}/id"), f, &format!("{v:04x}\n"));
    }
    for (ty, f) in [(EV_KEY, "key"), (EV_REL, "rel"), (EV_ABS, "abs"), (EV_MSC, "msc"), (EV_LED, "led"), (EV_SND, "snd"), (EV_FF, "ff"), (EV_SW, "sw")] {
        sysfs::set(&format!("{sysdir}/capabilities"), f, &format!("{}\n", c.codes[ty as usize].to_sysfs()));
    }
    sysfs::set(&format!("{sysdir}/capabilities"), "ev", &format!("{}\n", c.ev.to_sysfs()));
    let minor = EVDEV_MINOR_BASE + index;
    let event = format!("event{index}");
    sysfs::add_char_device(&format!("{sysdir}/{event}"), "input", &event, &format!("input/{event}"), INPUT_MAJOR, minor);
    sysfs::symlink(&format!("{sysdir}/{event}"), "device", "..");
    devtmpfs_node(&event, minor, true);
    DEVICES.lock().push(dev.clone());
    log::info!("input: input{index} (event{index}): \"{}\"", c.name);
    Ok(dev)
}

/// Add or remove /dev/input/<name> in devtmpfs.
fn devtmpfs_node(name: &str, minor: u32, add: bool) {
    use crate::fs::vfs::{Inode, S_IFCHR, S_IFDIR, encode_dev};
    let root = crate::fs::devtmpfs_root();
    let dir = match root.get(b"input") {
        Some(d) => Some(d),
        None if add => root.add(b"input", S_IFDIR | 0o755, 0, None).ok(),
        None => None,
    };
    if let Some(d) = dir {
        if add {
            let _ = d.add(name.as_bytes(), S_IFCHR | 0o660, encode_dev(INPUT_MAJOR, minor), None);
        } else {
            let _ = d.unlink(name.as_bytes(), false);
        }
    }
}

impl InputDev {
    /// "input:b0006v0627p0001e0001-e0,1,4,11,14,k...,ramlsfw".
    fn modalias(&self) -> String {
        let c = &self.caps;
        let [bus, vendor, product, version] = c.id;
        let mut s = format!("input:b{bus:04X}v{vendor:04X}p{product:04X}e{version:04X}-");
        let list = |s: &mut String, letter: char, b: &Bits, n: usize| {
            s.push(letter);
            for i in 0..n {
                if b.get(i) {
                    s.push_str(&format!("{i:X},"));
                }
            }
        };
        list(&mut s, 'e', &c.ev, EV_CNT);
        for (ty, l) in [(EV_KEY, 'k'), (EV_REL, 'r'), (EV_ABS, 'a'), (EV_MSC, 'm'), (EV_LED, 'l'), (EV_SND, 's'), (EV_FF, 'f'), (EV_SW, 'w')] {
            list(&mut s, l, &c.codes[ty as usize], code_count(ty));
        }
        s
    }

    /// Remove the device: clients get ENODEV, the nodes go away.
    pub fn unregister(self: &Arc<Self>) {
        if self.dead.swap(true, Ordering::AcqRel) {
            return;
        }
        DEVICES.lock().retain(|d| !Arc::ptr_eq(d, self));
        crate::fs::sysfs::remove_device(&format!("{}/event{}", self.sysdir, self.index));
        crate::fs::sysfs::remove_device(&self.sysdir);
        crate::fs::sysfs::remove(&format!("class/input/input{}", self.index));
        crate::fs::sysfs::remove(&format!("class/input/event{}", self.index));
        crate::fs::sysfs::remove(&format!("dev/char/{INPUT_MAJOR}:{}", EVDEV_MINOR_BASE + self.index));
        devtmpfs_node(&format!("event{}", self.index), 0, false);
        let clients: Vec<Arc<Client>> = self.st.lock().clients.iter().filter_map(Weak::upgrade).collect();
        for c in clients {
            c.wq.wake(POLLHUP | POLLERR);
        }
        USED.fetch_and(!(1 << self.index), Ordering::AcqRel);
    }

    /// An event from the device (or injected): filtered against the
    /// state, buffered, delivered to the clients at SYN_REPORT.
    pub fn event(&self, ty: u16, code: u16, value: i32) {
        if self.dead.load(Ordering::Relaxed) || ty != EV_SYN && !self.caps.has(ty, code) {
            return;
        }
        let mut wake = Vec::new();
        {
            let mut st = self.st.lock();
            let pass = match ty {
                EV_SYN if code == SYN_REPORT => {
                    let mut pkt = core::mem::take(&mut st.packet);
                    if !pkt.is_empty() {
                        pkt.push(Event { ns: crate::time::monotonic_ns(), ty, code, value: 0 });
                        st.clients.retain(|w| w.strong_count() > 0);
                        let grab = st.grab;
                        for c in st.clients.iter().filter_map(Weak::upgrade) {
                            if grab.is_none_or(|g| g == c.id) && c.push(&pkt) {
                                wake.push(c);
                            }
                        }
                    }
                    false
                }
                EV_SYN => true,
                EV_KEY => {
                    let down = st.key.get(code as usize);
                    if value == 2 {
                        down
                    } else if down != (value != 0) {
                        st.key.set(code as usize, value != 0);
                        true
                    } else {
                        false
                    }
                }
                EV_LED | EV_SND | EV_SW => {
                    let bits = match ty {
                        EV_LED => &mut st.led,
                        EV_SND => &mut st.snd,
                        _ => &mut st.sw,
                    };
                    let changed = bits.get(code as usize) != (value != 0);
                    bits.set(code as usize, value != 0);
                    changed
                }
                EV_ABS if code == ABS_MT_SLOT => {
                    let ok = (value as usize) < st.mt.len() && value >= 0;
                    if ok && st.slot != value as usize {
                        st.slot = value as usize;
                        true
                    } else {
                        false
                    }
                }
                EV_ABS if (ABS_MT_FIRST..=ABS_MT_LAST).contains(&code) && self.caps.slots() > 0 => {
                    let (s, i) = (st.slot, (code - ABS_MT_FIRST) as usize);
                    let changed = st.mt[s][i] != value;
                    st.mt[s][i] = value;
                    changed
                }
                EV_ABS => {
                    let mut abs = self.abs.lock();
                    let changed = abs[code as usize].0[0] != value;
                    abs[code as usize].0[0] = value;
                    changed
                }
                EV_REL => value != 0,
                _ => true,
            };
            if pass {
                st.packet.push(Event { ns: crate::time::monotonic_ns(), ty, code, value });
            }
        }
        for c in wake {
            c.wq.wake(READ_EVENTS);
        }
    }

    /// Events written to a node: delivered like the device's own; LEDs,
    /// sounds and force feedback also go to the driver.
    fn inject(&self, ty: u16, code: u16, value: i32) {
        if matches!(ty, EV_LED | EV_SND | EV_FF | EV_REP)
            && let Some(s) = &self.sink
        {
            s.output(ty, code, value);
        }
        if ty != EV_FF {
            self.event(ty, code, value);
        }
    }
}

/// One open of an event node.
pub struct Client {
    id: u64,
    dev: Arc<InputDev>,
    queue: Mutex<VecDeque<Event>>,
    wq: Arc<WaitQueue>,
    clock: AtomicU32,
    /// Per event type, codes masked out by EVIOCSMASK (None: all pass).
    mask: Mutex<Vec<Option<Bits>>>,
}

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;
const CLOCK_BOOTTIME: u32 = 7;

impl Client {
    /// Queue a packet; false if nothing was queued.
    fn push(&self, pkt: &[Event]) -> bool {
        let mask = self.mask.lock();
        // mask[0] masks whole event types, mask[t] codes of type t.
        let masked = |t: usize, c: usize| mask.get(t).is_some_and(|m| m.as_ref().is_some_and(|m| m.get(c)));
        let pass = |e: &Event| e.ty == EV_SYN || !masked(0, e.ty as usize) && !masked(e.ty as usize, e.code as usize);
        let n = pkt.iter().filter(|e| pass(e)).count();
        if n <= 1 {
            return false;
        }
        let mut q = self.queue.lock();
        if q.len() + n > EVDEV_BUF {
            let ns = pkt[0].ns;
            q.clear();
            q.push_back(Event { ns, ty: EV_SYN, code: SYN_DROPPED, value: 0 });
            return true;
        }
        q.extend(pkt.iter().filter(|e| pass(e)).copied());
        true
    }

    fn encode(&self, e: &Event) -> [u8; EVENT_SIZE] {
        let ns = match self.clock.load(Ordering::Relaxed) {
            CLOCK_REALTIME => (crate::time::boot_epoch_ns() + e.ns as i128) as i64,
            _ => e.ns as i64,
        };
        let mut b = [0u8; EVENT_SIZE];
        b[..8].copy_from_slice(&ns.div_euclid(1_000_000_000).to_le_bytes());
        b[8..16].copy_from_slice(&(ns.rem_euclid(1_000_000_000) / 1000).to_le_bytes());
        b[16..18].copy_from_slice(&e.ty.to_le_bytes());
        b[18..20].copy_from_slice(&e.code.to_le_bytes());
        b[20..24].copy_from_slice(&e.value.to_le_bytes());
        b
    }
}

pub struct EvdevFile(Arc<Client>);

/// Open of /dev/input/eventN.
pub fn open_evdev(minor: u32) -> KResult<Box<dyn FileOps>> {
    let dev = by_minor(minor).ok_or(ENODEV)?;
    let c = Arc::new(Client {
        id: NEXT_CLIENT.fetch_add(1, Ordering::Relaxed),
        dev: dev.clone(),
        queue: Mutex::new(VecDeque::new()),
        wq: WaitQueue::new(),
        clock: AtomicU32::new(CLOCK_REALTIME),
        mask: Mutex::new(vec![None; EV_CNT]),
    });
    dev.st.lock().clients.push(Arc::downgrade(&c));
    Ok(Box::new(EvdevFile(c)))
}

/// Parse input_events written by user space.
pub fn parse_events(data: &[u8]) -> KResult<Vec<(u16, u16, i32)>> {
    if !data.is_empty() && data.len() < EVENT_SIZE {
        return Err(EINVAL);
    }
    Ok(data
        .chunks_exact(EVENT_SIZE)
        .map(|e| {
            (
                u16::from_le_bytes([e[16], e[17]]),
                u16::from_le_bytes([e[18], e[19]]),
                i32::from_le_bytes(e[20..24].try_into().unwrap()),
            )
        })
        .collect())
}

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

fn copy_out(arg: u64, b: &[u8]) -> KResult<()> {
    crate::process::current().mm().copy_out(arg, b)
}

fn copy_in(arg: u64, b: &mut [u8]) -> KResult<()> {
    crate::process::current().mm().copy_in(arg, b)
}

fn user_int(arg: u64) -> KResult<i32> {
    let mut b = [0u8; 4];
    copy_in(arg, &mut b)?;
    Ok(i32::from_le_bytes(b))
}

/// Copy at most `len` bytes of `data` out; returns the count (what the
/// variable-length EVIOCG* ioctls return).
fn out_var(arg: u64, len: usize, data: &[u8]) -> KResult<u64> {
    let n = len.min(data.len());
    copy_out(arg, &data[..n])?;
    Ok(n as u64)
}

/// A string ioctl result: NUL-terminated, truncated to `len`.
fn out_str(arg: u64, len: usize, s: &str) -> KResult<u64> {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    out_var(arg, len, &b)
}

impl FileOps for EvdevFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let c = &self.0;
        if buf.len() < EVENT_SIZE {
            return Err(EINVAL);
        }
        let mut n = 0;
        let mut res = Ok(());
        let w = sched::wait_until(c.wq.key(), None, WaitMode::Interruptible, || {
            if c.dev.dead.load(Ordering::Relaxed) {
                res = Err(ENODEV);
                return true;
            }
            let mut q = c.queue.lock();
            while n + EVENT_SIZE <= buf.len() {
                let Some(e) = q.pop_front() else { break };
                buf[n..n + EVENT_SIZE].copy_from_slice(&c.encode(&e));
                n += EVENT_SIZE;
            }
            n > 0 || f.nonblock()
        });
        match w {
            _ if n > 0 => Ok(n),
            Wake::Ready => res.and(Err(EAGAIN)),
            Wake::Timeout => Err(EAGAIN),
            Wake::Interrupted => Err(ERESTARTSYS),
        }
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let dev = &self.0.dev;
        if dev.dead.load(Ordering::Relaxed) {
            return Err(ENODEV);
        }
        let evs = parse_events(data)?;
        for (ty, code, value) in &evs {
            dev.inject(*ty, *code, *value);
        }
        Ok(evs.len() * EVENT_SIZE)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let c = &self.0;
        let dev = &c.dev;
        if dev.dead.load(Ordering::Relaxed) {
            return Err(ENODEV);
        }
        let caps = &dev.caps;
        let (dir, size, ty, nr) = (cmd >> 30, ((cmd >> 16) & 0x3fff) as usize, (cmd >> 8) & 0xff, cmd & 0xff);
        if ty != b'E' as u32 {
            return Err(ENOTTY);
        }
        match (dir, nr) {
            (IOC_READ, 0x01) => copy_out(arg, &0x010001i32.to_le_bytes()).map(|_| 0),
            (IOC_READ, 0x02) => {
                let b: Vec<u8> = caps.id.iter().flat_map(|v| v.to_le_bytes()).collect();
                copy_out(arg, &b).map(|_| 0)
            }
            (IOC_READ, 0x03) => {
                let b: Vec<u8> = caps.rep.iter().flat_map(|v| v.to_le_bytes()).collect();
                copy_out(arg, &b).map(|_| 0)
            }
            (IOC_WRITE, 0x03) => Ok(0),
            (IOC_READ, 0x06) => out_str(arg, size, &caps.name),
            (IOC_READ, 0x07) if caps.phys.is_empty() => Err(ENOENT),
            (IOC_READ, 0x07) => out_str(arg, size, &caps.phys),
            (IOC_READ, 0x08) if caps.uniq.is_empty() => Err(ENOENT),
            (IOC_READ, 0x08) => out_str(arg, size, &caps.uniq),
            (IOC_READ, 0x09) => out_var(arg, size, &caps.props.bytes()),
            (IOC_READ, 0x0a) => {
                // EVIOCGMTSLOTS: u32 code, then one value per slot.
                let code = user_int(arg)? as u16;
                if !(ABS_MT_FIRST..=ABS_MT_LAST).contains(&code) || size < 4 {
                    return Err(EINVAL);
                }
                let st = dev.st.lock();
                let vals: Vec<u8> = st.mt.iter().take((size - 4) / 4).flat_map(|s| s[(code - ABS_MT_FIRST) as usize].to_le_bytes()).collect();
                copy_out(arg + 4, &vals)?;
                Ok(0)
            }
            (IOC_READ, 0x18..=0x1b) => {
                let st = dev.st.lock();
                let b = match nr {
                    0x18 => &st.key,
                    0x19 => &st.led,
                    0x1a => &st.snd,
                    _ => &st.sw,
                };
                out_var(arg, size, &b.bytes())
            }
            (IOC_READ, 0x20..=0x3f) => {
                let t = (nr - 0x20) as usize;
                if t != 0 && (code_count(t as u16) == 0 || t == EV_REP as usize) {
                    return Err(EINVAL);
                }
                let b = if t == 0 { caps.ev.bytes() } else { caps.codes[t].bytes() };
                out_var(arg, size, &b)
            }
            (IOC_READ, 0x40..=0x7f) => {
                // Like Linux: any axis of a device with EV_ABS (zeros
                // for axes it does not report).
                let a = (nr - 0x40) as usize;
                if !caps.ev.get(EV_ABS as usize) {
                    return Err(EINVAL);
                }
                let mut info = dev.abs.lock()[a];
                if (ABS_MT_FIRST..=ABS_MT_LAST).contains(&(a as u16)) && caps.slots() > 0 {
                    let st = dev.st.lock();
                    info.0[0] = st.mt[st.slot][a - ABS_MT_FIRST as usize];
                } else if a == ABS_MT_SLOT as usize {
                    info.0[0] = dev.st.lock().slot as i32;
                }
                let b: Vec<u8> = info.0.iter().flat_map(|v| v.to_le_bytes()).collect();
                copy_out(arg, &b[..size.min(24)]).map(|_| 0)
            }
            (IOC_WRITE, 0xc0..=0xff) => {
                let a = (nr - 0xc0) as usize;
                if a >= ABS_CNT || !caps.ev.get(EV_ABS as usize) {
                    return Err(EINVAL);
                }
                let mut b = [0u8; 24];
                copy_in(arg, &mut b[..size.min(24)])?;
                let mut abs = dev.abs.lock();
                for (i, v) in b.chunks_exact(4).enumerate().take(size.min(24) / 4) {
                    abs[a].0[i] = i32::from_le_bytes(v.try_into().unwrap());
                }
                Ok(0)
            }
            (IOC_WRITE, 0x80) => Err(ENOSYS),
            (IOC_WRITE, 0x81) => Ok(0),
            (IOC_READ, 0x84) => copy_out(arg, &0i32.to_le_bytes()).map(|_| 0),
            (IOC_WRITE, 0x90) => {
                let mut st = dev.st.lock();
                if arg != 0 {
                    if st.grab.is_some() {
                        return Err(EBUSY);
                    }
                    st.grab = Some(c.id);
                } else {
                    if st.grab != Some(c.id) {
                        return Err(EINVAL);
                    }
                    st.grab = None;
                }
                Ok(0)
            }
            (IOC_WRITE, 0x91) => {
                if arg != 0 {
                    return Err(EINVAL);
                }
                Ok(0)
            }
            (IOC_READ | IOC_WRITE, 0x92 | 0x93) => {
                // struct input_mask { u32 type, codes_size; u64 codes_ptr; }
                let mut m = [0u8; 16];
                copy_in(arg, &mut m)?;
                let t = u32::from_le_bytes(m[..4].try_into().unwrap()) as usize;
                let len = u32::from_le_bytes(m[4..8].try_into().unwrap()) as usize;
                let ptr = u64::from_le_bytes(m[8..].try_into().unwrap());
                if t >= EV_CNT || code_count(t as u16) == 0 && t != 0 || len % 8 != 0 {
                    return Err(EINVAL);
                }
                let ncodes = if t == 0 { EV_CNT } else { code_count(t as u16) };
                let mut mask = c.mask.lock();
                if nr == 0x92 {
                    // Bits set = codes the client receives.
                    let mut b = Bits::new(ncodes);
                    for i in 0..ncodes {
                        b.set(i, !mask[t].as_ref().is_some_and(|m| m.get(i)));
                    }
                    let bytes = b.bytes();
                    copy_out(ptr, &bytes[..len.min(bytes.len())])?;
                } else {
                    let mut bytes = vec![0u8; len.min(ncodes.div_ceil(64) * 8)];
                    copy_in(ptr, &mut bytes)?;
                    let mut b = Bits::new(ncodes);
                    for i in 0..ncodes {
                        let on = bytes.get(i / 8).is_some_and(|x| x & (1 << (i % 8)) != 0);
                        b.set(i, !on);
                    }
                    mask[t] = Some(b);
                }
                Ok(0)
            }
            (IOC_WRITE, 0xa0) => {
                let id = user_int(arg)? as u32;
                if !matches!(id, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
                    return Err(EINVAL);
                }
                c.clock.store(id, Ordering::Relaxed);
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let c = &self.0;
        if c.dev.dead.load(Ordering::Relaxed) {
            return POLLHUP | POLLERR;
        }
        let mut m = WRITE_EVENTS;
        if !c.queue.lock().is_empty() {
            m |= READ_EVENTS;
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

impl Drop for EvdevFile {
    fn drop(&mut self) {
        let c = &self.0;
        let mut st = c.dev.st.lock();
        if st.grab == Some(c.id) {
            st.grab = None;
        }
        st.clients.retain(|w| w.strong_count() > 0 && !core::ptr::eq(w.as_ptr(), Arc::as_ptr(c)));
    }
}
