//! virtio-net: one receive and one transmit queue, polled.
//!
//! Every receive descriptor is one 2 KiB buffer (virtio header + a full
//! Ethernet frame, no mergeable buffers, no offloads), kept posted; the
//! network stack collects frames with [`EthDevice::recv`] when the timer
//! tick sees the used ring move ([`rx_hint`]). Transmit copies each frame
//! into one of a fixed set of bounce buffers; finished ones are reclaimed
//! on the next send, and the stack stops sending while none is free
//! ([`EthDevice::can_send`]). Cards become `eth0`, `eth1`, ... in PCI order
//! (`crate::net::inet`).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::PhysAddr;

use super::{Buf, F_VERSION_1, Transport, Virtqueue};
use crate::mm::paging::phys_to_virt;
use crate::mm::{PAGE_SIZE, frame};
use crate::net::inet::EthDevice;

const F_MAC: u64 = 1 << 5;

const BUF_SIZE: usize = 2048;
const RX_BUFS: usize = 256;
const TX_BUFS: usize = 128;

const MAX_CARDS: usize = 8;

/// Per card: address of the receive queue's used index and the last index
/// popped, for the interrupt-context check in [`rx_hint`].
static RX_USED_ADDR: [AtomicU64; MAX_CARDS] = [const { AtomicU64::new(0) }; MAX_CARDS];
static RX_LAST: [AtomicU64; MAX_CARDS] = [const { AtomicU64::new(0) }; MAX_CARDS];
static CARDS: AtomicU64 = AtomicU64::new(0);

/// Whether received frames are waiting on any card (callable from the
/// timer interrupt).
pub fn rx_hint() -> bool {
    (0..CARDS.load(Ordering::Acquire) as usize).any(|i| {
        let a = RX_USED_ADDR[i].load(Ordering::Acquire);
        a != 0 && unsafe { core::ptr::read_volatile(a as *const u16) } as u64 != RX_LAST[i].load(Ordering::Relaxed)
    })
}

pub struct VirtioNet {
    transport: Transport,
    rx: Virtqueue,
    tx: Virtqueue,
    rx_mem: PhysAddr,
    tx_mem: PhysAddr,
    /// Receive buffer index of each posted head descriptor.
    rx_slot: Vec<u16>,
    /// Transmit buffers not in flight, and the buffer of each head.
    tx_free: Vec<u16>,
    tx_slot: Vec<u16>,
    hdr_len: usize,
    mac: [u8; 6],
    /// Index into RX_USED_ADDR / RX_LAST.
    card: usize,
}

fn buf_ptr(base: PhysAddr, i: usize) -> *mut u8 {
    unsafe { phys_to_virt(base).as_mut_ptr::<u8>().add(i * BUF_SIZE) }
}

impl VirtioNet {
    fn post_rx(&mut self, i: u16) {
        let phys = self.rx_mem.as_u64() + i as u64 * BUF_SIZE as u64;
        if let Some(head) = self.rx.add(&[Buf { phys, len: BUF_SIZE as u32, write: true }]) {
            self.rx_slot[head as usize] = i;
        }
    }

    fn reclaim_tx(&mut self) {
        while let Some((head, _)) = self.tx.pop_used() {
            self.tx_free.push(self.tx_slot[head as usize]);
        }
    }
}

impl EthDevice for VirtioNet {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn driver(&self) -> &'static str {
        "virtio_net"
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        let (head, len) = self.rx.pop_used()?;
        RX_LAST[self.card].store(self.rx.last_used() as u64, Ordering::Relaxed);
        let i = self.rx_slot[head as usize];
        let len = (len as usize).min(BUF_SIZE);
        let frame = if len > self.hdr_len {
            let p = buf_ptr(self.rx_mem, i as usize);
            unsafe { core::slice::from_raw_parts(p.add(self.hdr_len), len - self.hdr_len) }.to_vec()
        } else {
            Vec::new()
        };
        self.post_rx(i);
        self.rx.notify();
        Some(frame)
    }

    fn can_send(&mut self) -> bool {
        self.reclaim_tx();
        !self.tx_free.is_empty()
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        self.reclaim_tx();
        if frame.len() + self.hdr_len > BUF_SIZE {
            return false;
        }
        let Some(i) = self.tx_free.pop() else {
            return false;
        };
        let p = buf_ptr(self.tx_mem, i as usize);
        unsafe {
            core::ptr::write_bytes(p, 0, self.hdr_len);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), p.add(self.hdr_len), frame.len());
        }
        let phys = self.tx_mem.as_u64() + i as u64 * BUF_SIZE as u64;
        match self.tx.add(&[Buf { phys, len: (self.hdr_len + frame.len()) as u32, write: false }]) {
            Some(head) => {
                self.tx_slot[head as usize] = i;
                self.tx.notify();
                self.transport.ack_isr();
                true
            }
            None => {
                self.tx_free.push(i);
                false
            }
        }
    }
}

/// Bring up one virtio-net device and hand it to the network stack.
pub fn probe(t: Transport, sysdev: alloc::string::String) {
    t.begin_init();
    let offered = t.device_features();
    let modern = t.is_modern();
    let features = offered & (F_MAC | if modern { F_VERSION_1 } else { 0 });
    if modern && features & F_VERSION_1 == 0 || !t.set_features(features) {
        log::warn!("virtio-net: feature negotiation failed ({offered:#x})");
        t.fail();
        return;
    }
    let (Some(rx), Some(tx)) = (t.setup_queue(0, 256), t.setup_queue(1, 256)) else {
        log::warn!("virtio-net: missing queues");
        t.fail();
        return;
    };
    let pages = |n: usize| (n * BUF_SIZE).div_ceil(PAGE_SIZE as usize);
    let (Some(rx_mem), Some(tx_mem)) = (frame::alloc_contiguous(pages(RX_BUFS), 1), frame::alloc_contiguous(pages(TX_BUFS), 1))
    else {
        t.fail();
        return;
    };
    let mut mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
    if features & F_MAC != 0 {
        for (i, b) in mac.iter_mut().enumerate() {
            *b = t.config_read8(i as u16);
        }
    }
    let card = CARDS.load(Ordering::Acquire) as usize;
    if card >= MAX_CARDS {
        log::warn!("virtio-net: too many cards");
        t.fail();
        return;
    }
    let (rxn, txn) = (rx.size() as usize, tx.size() as usize);
    let mut dev = VirtioNet {
        transport: t,
        rx,
        tx,
        rx_mem,
        tx_mem,
        rx_slot: alloc::vec![0; rxn],
        tx_free: (0..TX_BUFS.min(txn) as u16).collect(),
        tx_slot: alloc::vec![0; txn],
        // virtio 1.x always has num_buffers; legacy without MRG_RXBUF not.
        hdr_len: if modern { 12 } else { 10 },
        mac,
        card,
    };
    for i in 0..RX_BUFS.min(rxn) as u16 {
        dev.post_rx(i);
    }
    dev.transport.driver_ok();
    dev.rx.notify();
    RX_LAST[card].store(dev.rx.last_used() as u64, Ordering::Relaxed);
    RX_USED_ADDR[card].store(dev.rx.used_idx_addr(), Ordering::Release);
    CARDS.store(card as u64 + 1, Ordering::Release);
    log::info!(
        "virtio-net: eth{card} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, {}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        if modern { "modern" } else { "legacy" }
    );
    crate::net::inet::add_ethernet(Box::new(dev), sysdev);
}
