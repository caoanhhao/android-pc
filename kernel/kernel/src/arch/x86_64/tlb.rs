//! TLB shootdown: after changing user page tables that other CPUs may
//! have loaded, those CPUs flush their (non-global) TLB entries.
//!
//! One shootdown runs at a time; the initiator sends IPIs to every CPU in
//! the mask and spins until all of them acknowledged by reloading CR3.

use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::registers::control::Cr3;

use super::{apic, cpu, trap};

static LOCK: Mutex<()> = Mutex::new(());
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Flush the local TLB (non-global entries).
pub fn flush_local() {
    let (frame, flags) = Cr3::read();
    unsafe { Cr3::write(frame, flags) };
}

/// Make every CPU in `cpus` (bit = CPU index) except the caller drop its
/// user TLB entries. Must be called with interrupts enabled.
pub fn shootdown(cpus: u64) {
    let me = cpu::index_fast();
    let targets = cpus & !(1u64 << me);
    if targets == 0 {
        return;
    }
    let _guard = LOCK.lock();
    PENDING.store(targets.count_ones() as usize, Ordering::SeqCst);
    for i in 0..cpu::MAX_CPUS {
        if targets & (1 << i) != 0 {
            let id = cpu::LAPIC_IDS[i].load(Ordering::Relaxed);
            apic::send_ipi(id, trap::TLB_VECTOR as u8);
        }
    }
    while PENDING.load(Ordering::SeqCst) != 0 {
        core::hint::spin_loop();
    }
}

pub fn handle_ipi() {
    flush_local();
    PENDING.fetch_sub(1, Ordering::SeqCst);
}

/// Wake an idle CPU so it looks at the run queue now instead of at its next
/// timer tick.
pub fn kick(cpu_index: usize) {
    let id = cpu::LAPIC_IDS[cpu_index].load(Ordering::Relaxed);
    if id != u32::MAX {
        apic::send_ipi(id, trap::RESCHED_VECTOR as u8);
    }
}
