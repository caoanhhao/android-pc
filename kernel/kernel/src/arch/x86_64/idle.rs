//! The idle instruction: MWAIT with the deepest safe C-state hint where
//! the CPU has it, else HLT.
//!
//! MONITOR/MWAIT (CPUID.01H:ECX[3]) with leaf 5 extensions lets an idle
//! CPU ask for deeper C-states than HLT's C1. Deeper than C1 only when
//! the LAPIC timer keeps running in them (ARAT, CPUID.06H:EAX[2]): the
//! scheduler tick and timeouts depend on it. The hint is the deepest MWAIT
//! C-state up to C3 (the 0x20 hint, Intel's C6 on recent cores) that leaf
//! 5's EDX lists sub-states for. `idle=hlt` on the command line keeps
//! HLT, `idle=c1` caps MWAIT at C1. KVM guests normally do not see
//! MONITOR, so QEMU runs keep HLT.
//!
//! The CPU waits with `sti; mwait`: the STI shadow covers MWAIT, so an
//! interrupt that arrives in between still wakes it, as with `sti; hlt`.

use core::sync::atomic::{AtomicU32, Ordering};

use super::cpu::MAX_CPUS;

const USE_HLT: u32 = u32::MAX;
static HINT: AtomicU32 = AtomicU32::new(USE_HLT);

/// One cache line per CPU to monitor (nothing writes it: interrupts wake).
#[repr(C, align(64))]
struct Line([u64; 8]);
static LINES: [Line; MAX_CPUS] = [const { Line([0; 8]) }; MAX_CPUS];

/// Choose the idle instruction (BSP, once).
pub fn init() {
    use core::arch::x86_64::__cpuid;
    let words: alloc::vec::Vec<&str> = crate::boot::cmdline().split_ascii_whitespace().collect();
    let max = __cpuid(0).eax;
    let monitor = __cpuid(1).ecx & (1 << 3) != 0;
    if words.contains(&"idle=hlt") || !monitor || max < 5 {
        log::info!("idle: hlt{}", if monitor { " (idle=hlt)" } else { "" });
        return;
    }
    let l5 = __cpuid(5);
    // Extensions enumerated, interrupts break MWAIT.
    if l5.ecx & 3 != 3 {
        log::info!("idle: hlt (no MWAIT extensions)");
        return;
    }
    let arat = max >= 6 && __cpuid(6).eax & (1 << 2) != 0;
    let limit = if arat && !words.contains(&"idle=c1") { 3 } else { 1 };
    let Some(n) = (1..=limit).rev().find(|&n| (l5.edx >> (4 * n)) & 0xf != 0) else {
        log::info!("idle: hlt (no MWAIT C-states listed)");
        return;
    };
    let hint = (n - 1) << 4;
    HINT.store(hint, Ordering::Relaxed);
    log::info!("idle: mwait, hint {hint:#04x} (MWAIT C{n}){}", if arat { "" } else { ", no ARAT" });
}

/// Enable interrupts and wait for one (called with interrupts off).
pub fn wait() {
    let hint = HINT.load(Ordering::Relaxed);
    if hint == USE_HLT {
        x86_64::instructions::interrupts::enable_and_hlt();
        return;
    }
    let line = &LINES[super::cpu::index_fast() % MAX_CPUS] as *const Line;
    unsafe {
        core::arch::asm!("monitor", in("rax") line, in("ecx") 0, in("edx") 0, options(nostack, preserves_flags));
        core::arch::asm!("sti; mwait", in("eax") hint, in("ecx") 0, options(nostack));
    }
}
