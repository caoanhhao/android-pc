//! virtio-input (virtio-keyboard/mouse/tablet/multitouch-pci, polled).
//!
//! The device describes itself in its configuration space (name, serial,
//! ids, property bits, the codes of each event type, absolute axis
//! ranges), which becomes an input device (`drivers::input`). The event
//! queue is kept full of 8-byte buffers ({le16 type, le16 code, le32
//! value}); the timer tick notices returned ones ([`tick`]) and the soft
//! interrupt ([`softirq`]) hands the events to the input core. LED events
//! written to the event node go back to the device on the status queue.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use super::{Buf, F_VERSION_1, Transport, Virtqueue};
use crate::drivers::input::{self, AbsInfo, Caps, EV_ABS, EV_CNT, InputDev, PROP_CNT, Sink};
use crate::mm::frame;
use crate::mm::paging::phys_to_virt;

const CFG_ID_NAME: u8 = 0x01;
const CFG_ID_SERIAL: u8 = 0x02;
const CFG_ID_DEVIDS: u8 = 0x03;
const CFG_PROP_BITS: u8 = 0x10;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

/// Event buffers kept posted (one page of 8-byte events).
const EVENT_BUFS: usize = 256;
const MAX_DEVS: usize = 8;

static USED_ADDR: [AtomicU64; MAX_DEVS] = [const { AtomicU64::new(0) }; MAX_DEVS];
static LAST: [AtomicU64; MAX_DEVS] = [const { AtomicU64::new(0) }; MAX_DEVS];
static COUNT: AtomicU64 = AtomicU64::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);

struct EventQ {
    q: Virtqueue,
    mem: PhysAddr,
    dev: Arc<InputDev>,
    slot: usize,
}

static DEVS: Mutex<Vec<EventQ>> = Mutex::new(Vec::new());

/// Status queue (driver -> device): LED states.
struct Status {
    transport: Transport,
    q: Virtqueue,
    buf: PhysAddr,
}

struct StatusSink(Mutex<Status>);

impl Sink for StatusSink {
    fn output(&self, ty: u16, code: u16, value: i32) {
        let s = self.0.lock();
        let p = phys_to_virt(s.buf).as_mut_ptr::<u8>();
        unsafe {
            core::ptr::copy_nonoverlapping(ty.to_le_bytes().as_ptr(), p, 2);
            core::ptr::copy_nonoverlapping(code.to_le_bytes().as_ptr(), p.add(2), 2);
            core::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), p.add(4), 4);
        }
        let buf = s.buf.as_u64();
        let mut s = s;
        let _ = s.q.submit_and_wait(&[Buf { phys: buf, len: 8, write: false }]);
        s.transport.ack_isr();
    }
}

/// From the timer interrupt (BSP): note returned event buffers.
pub fn tick() {
    for i in 0..COUNT.load(Ordering::Acquire) as usize {
        let a = USED_ADDR[i].load(Ordering::Acquire);
        if a != 0 && unsafe { core::ptr::read_volatile(a as *const u16) } as u64 != LAST[i].load(Ordering::Relaxed) {
            PENDING.store(true, Ordering::Relaxed);
            return;
        }
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// Deliver the events the devices returned (task context, no locks held).
pub fn softirq() {
    let Some(mut devs) = DEVS.try_lock() else { return };
    PENDING.store(false, Ordering::Relaxed);
    for d in devs.iter_mut() {
        let mut any = false;
        while let Some((head, len)) = d.q.pop_used() {
            any = true;
            if len >= 8 {
                let p = unsafe { phys_to_virt(d.mem).as_ptr::<u8>().add(head as usize * 8) };
                let e = unsafe { core::slice::from_raw_parts(p, 8) };
                let ty = u16::from_le_bytes([e[0], e[1]]);
                let code = u16::from_le_bytes([e[2], e[3]]);
                let value = i32::from_le_bytes(e[4..8].try_into().unwrap());
                d.dev.event(ty, code, value);
            }
            // Head ids equal buffer slots: each chain is one descriptor,
            // re-added right after it comes back.
            let phys = d.mem.as_u64() + head as u64 * 8;
            d.q.add(&[Buf { phys, len: 8, write: true }]);
        }
        LAST[d.slot].store(d.q.last_used() as u64, Ordering::Relaxed);
        if any {
            d.q.notify();
        }
    }
}

/// Read one configuration item: (select, subsel) -> bytes.
fn config(t: &Transport, select: u8, subsel: u8) -> Vec<u8> {
    t.config_write8(0, select);
    t.config_write8(1, subsel);
    let size = t.config_read8(2).min(128) as usize;
    (0..size).map(|i| t.config_read8(8 + i as u16)).collect()
}

fn bits_into(b: &[u8], out: &mut input::Bits, max: usize) {
    for (i, byte) in b.iter().enumerate() {
        for bit in 0..8 {
            let n = i * 8 + bit;
            if byte & (1 << bit) != 0 && n < max {
                out.set(n, true);
            }
        }
    }
}

/// Bring up one virtio-input device and register it with the input core.
pub fn probe(t: Transport, sysdev: String) {
    t.begin_init();
    let offered = t.device_features();
    let modern = t.is_modern();
    let features = offered & if modern { F_VERSION_1 } else { 0 };
    if modern && features & F_VERSION_1 == 0 || !t.set_features(features) {
        log::warn!("virtio-input: feature negotiation failed ({offered:#x})");
        t.fail();
        return;
    }
    let name: String = String::from_utf8_lossy(&config(&t, CFG_ID_NAME, 0)).trim_end_matches('\0').into();
    let mut caps = Caps::new(&name);
    caps.uniq = String::from_utf8_lossy(&config(&t, CFG_ID_SERIAL, 0)).trim_end_matches('\0').into();
    caps.phys = alloc::format!("virtio{}/input0", sysdev.rsplit("virtio").next().unwrap_or("0"));
    let ids = config(&t, CFG_ID_DEVIDS, 0);
    if ids.len() >= 8 {
        for i in 0..4 {
            caps.id[i] = u16::from_le_bytes([ids[2 * i], ids[2 * i + 1]]);
        }
    } else {
        caps.id = [0x06 /* BUS_VIRTUAL */, 0x0627, 0, 1];
    }
    bits_into(&config(&t, CFG_PROP_BITS, 0), &mut caps.props, PROP_CNT);
    for ty in 1..EV_CNT as u16 {
        let b = config(&t, CFG_EV_BITS, ty as u8);
        if b.iter().any(|&x| x != 0) {
            caps.ev.set(ty as usize, true);
            let n = input::code_count(ty);
            bits_into(&b, &mut caps.codes[ty as usize], n);
        }
    }
    for a in 0..input::ABS_CNT {
        if caps.has(EV_ABS, a as u16) {
            let b = config(&t, CFG_ABS_INFO, a as u8);
            if b.len() >= 20 {
                let v = |i: usize| i32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
                caps.abs[a] = AbsInfo([0, v(0), v(1), v(2), v(3), v(4)]);
            }
        }
    }
    let slot = COUNT.load(Ordering::Acquire) as usize;
    let (Some(mut eq), Some(sq)) = (t.setup_queue(0, EVENT_BUFS as u16), t.setup_queue(1, 8)) else {
        log::warn!("virtio-input: missing queues");
        t.fail();
        return;
    };
    let (Some(mem), Some(sbuf)) = (frame::alloc_contiguous(1, 1), frame::alloc_contiguous(1, 1)) else {
        t.fail();
        return;
    };
    if slot >= MAX_DEVS {
        log::warn!("virtio-input: too many devices");
        t.fail();
        return;
    }
    let n = (eq.size() as usize).min(EVENT_BUFS);
    // Posted in order on a fresh queue, so buffer i gets descriptor i.
    for i in 0..n {
        eq.add(&[Buf { phys: mem.as_u64() + i as u64 * 8, len: 8, write: true }]);
    }
    t.driver_ok();
    eq.notify();
    let sink = StatusSink(Mutex::new(Status { transport: t, q: sq, buf: sbuf }));
    let dev = match input::register(caps, &sysdev, Some(Box::new(sink))) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("virtio-input: {name}: {e:?}");
            return;
        }
    };
    LAST[slot].store(eq.last_used() as u64, Ordering::Relaxed);
    USED_ADDR[slot].store(eq.used_idx_addr(), Ordering::Release);
    DEVS.lock().push(EventQ { q: eq, mem, dev, slot });
    COUNT.store(slot as u64 + 1, Ordering::Release);
}
