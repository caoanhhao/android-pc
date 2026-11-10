//! virtio-console (polled, port 0 only, no multiport): an H4 Bluetooth
//! link. QEMU's `virtconsole` on a socket chardev connected to the host's
//! rootcanal (a software Bluetooth controller) becomes the controller
//! behind `/dev/hvc5` ([`crate::drivers::bluetooth`]), which is how
//! `tools/run-qemu.sh --aosp-bt-test` runs Android's Bluetooth stack.
//!
//! The receive queue is kept full of [`RX_BUF`]-byte buffers; the timer
//! tick notices returned ones ([`tick`]), the soft interrupt
//! ([`softirq`]) cuts the bytes into HCI packets and delivers them.
//! Sending copies the H4 packet into a page and waits for the device.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use super::{Buf, F_VERSION_1, Transport, Virtqueue};
use crate::drivers::bluetooth::{self, Controller, H4Parser};
use crate::errno::*;
use crate::mm::frame;
use crate::mm::paging::phys_to_virt;

const RX_BUFS: usize = 64;
const RX_BUF: usize = 512;

struct Rx {
    q: Virtqueue,
    mem: PhysAddr,
    parser: H4Parser,
}

struct Tx {
    q: Virtqueue,
    buf: PhysAddr,
}

static RX: Mutex<Option<Rx>> = Mutex::new(None);
static TX: Mutex<Option<Tx>> = Mutex::new(None);
static USED_ADDR: AtomicU64 = AtomicU64::new(0);
static LAST: AtomicU64 = AtomicU64::new(0);
static PENDING: AtomicBool = AtomicBool::new(false);

/// From the timer interrupt (BSP): note returned receive buffers.
pub fn tick() {
    let a = USED_ADDR.load(Ordering::Acquire);
    if a != 0 && unsafe { core::ptr::read_volatile(a as *const u16) } as u64 != LAST.load(Ordering::Relaxed) {
        PENDING.store(true, Ordering::Relaxed);
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// Deliver what the host sent (task context).
pub fn softirq() {
    let Some(mut g) = RX.try_lock() else { return };
    PENDING.store(false, Ordering::Relaxed);
    let Some(rx) = g.as_mut() else { return };
    let mut any = false;
    while let Some((head, len)) = rx.q.pop_used() {
        any = true;
        let p = unsafe { phys_to_virt(rx.mem).as_ptr::<u8>().add(head as usize * RX_BUF) };
        let data = unsafe { core::slice::from_raw_parts(p, (len as usize).min(RX_BUF)) };
        let bad = rx.parser.feed(data, bluetooth::deliver);
        if bad > 0 {
            log::warn!("virtio-console: {bad} bytes from the host are not H4, skipped");
        }
        let phys = rx.mem.as_u64() + head as u64 * RX_BUF as u64;
        rx.q.add(&[Buf { phys, len: RX_BUF as u32, write: true }]);
    }
    LAST.store(rx.q.last_used() as u64, Ordering::Relaxed);
    if any {
        rx.q.notify();
    }
}

struct VirtioBt(String);

impl Controller for VirtioBt {
    fn send(&self, kind: u8, pkt: &[u8]) -> KResult<()> {
        if pkt.len() + 1 > 4096 {
            return Err(EINVAL);
        }
        let mut g = TX.lock();
        let tx = g.as_mut().ok_or(EIO)?;
        let p = phys_to_virt(tx.buf).as_mut_ptr::<u8>();
        unsafe {
            *p = kind;
            core::ptr::copy_nonoverlapping(pkt.as_ptr(), p.add(1), pkt.len());
        }
        let b = Buf { phys: tx.buf.as_u64(), len: pkt.len() as u32 + 1, write: false };
        tx.q.submit_and_wait(&[b]).ok_or(EIO)?;
        Ok(())
    }

    fn describe(&self) -> String {
        self.0.clone()
    }
}

pub fn probe(t: Transport, sysdev: String) {
    t.begin_init();
    let offered = t.device_features();
    let features = offered & if t.is_modern() { F_VERSION_1 } else { 0 };
    if !t.set_features(features) {
        log::warn!("virtio-console: feature negotiation failed ({offered:#x})");
        t.fail();
        return;
    }
    let (Some(mut rq), Some(tq)) = (t.setup_queue(0, RX_BUFS as u16), t.setup_queue(1, 8)) else {
        log::warn!("virtio-console: missing queues");
        t.fail();
        return;
    };
    let pages = RX_BUFS * RX_BUF / 4096;
    let (Some(mem), Some(buf)) = (frame::alloc_contiguous(pages, 1), frame::alloc_contiguous(1, 1)) else {
        t.fail();
        return;
    };
    let n = (rq.size() as usize).min(RX_BUFS);
    // Posted in order on a fresh queue, so buffer i gets descriptor i.
    for i in 0..n {
        rq.add(&[Buf { phys: mem.as_u64() + (i * RX_BUF) as u64, len: RX_BUF as u32, write: true }]);
    }
    t.driver_ok();
    rq.notify();
    LAST.store(rq.last_used() as u64, Ordering::Relaxed);
    USED_ADDR.store(rq.used_idx_addr(), Ordering::Release);
    *RX.lock() = Some(Rx { q: rq, mem, parser: H4Parser::new() });
    *TX.lock() = Some(Tx { q: tq, buf });
    let name = sysdev.rsplit('/').next().unwrap_or("virtio");
    bluetooth::register(Arc::new(VirtioBt(format!("virtio-console {name} (H4)"))));
}
