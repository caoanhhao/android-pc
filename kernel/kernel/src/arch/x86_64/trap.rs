//! Rust side of the interrupt/exception stubs in `entry.rs`.
//!
//! Exceptions raised by user code become signals (SIGSEGV, SIGILL, ...);
//! exceptions in the kernel are fatal, except `int3`.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr2;

use super::entry::TrapFrame;
use super::{apic, tlb};
use crate::process::signal::{self, Fault};

pub const TIMER_VECTOR: u64 = apic::TIMER_VECTOR as u64;
pub const TLB_VECTOR: u64 = 0xf0;
pub const RESCHED_VECTOR: u64 = 0xf1;

/// Number of `int3` traps taken in kernel mode (used by tests).
pub static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

// siginfo si_code values.
const SI_KERNEL: i32 = 0x80;
const ILL_ILLOPN: i32 = 2;
const FPE_INTDIV: i32 = 1;
const FPE_FLTINV: i32 = 7;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;
const BUS_ADRALN: i32 = 1;
const BUS_ADRERR: i32 = 2;
const TRAP_TRACE: i32 = 2;

const PF_PRESENT: u64 = 1 << 0;
const PF_WRITE: u64 = 1 << 1;
const PF_INSN: u64 = 1 << 4;

pub extern "C" fn trap_handler(frame: &mut TrapFrame) {
    match frame.vector {
        TIMER_VECTOR => {
            apic::eoi();
            if !frame.user_mode() {
                profile_sample(frame.rip);
            }
            crate::sched::timer_interrupt(frame.user_mode());
        }
        TLB_VECTOR => {
            tlb::handle_ipi();
            apic::eoi();
        }
        RESCHED_VECTOR => {
            apic::eoi();
            crate::sched::set_need_resched();
        }
        _ if frame.user_mode() => user_exception(frame),
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
            log::trace!("breakpoint at {:#x}", frame.rip);
        }
        v => panic!(
            "exception {} ({}) in kernel mode, error code {:#x}, cr2 {:#x}\n{:#x?}",
            v,
            name(v),
            frame.error_code,
            Cr2::read_raw(),
            frame
        ),
    }
    if frame.user_mode() {
        crate::sched::exit_to_user(frame);
    }
}

/// Kernel RIPs seen by the timer tick (a sampling profile of where the
/// kernel spends its time; `echo p > /proc/sysrq-trigger` logs the top).
static PROFILE: crate::sync::IrqLock<alloc::collections::BTreeMap<u64, u32>> =
    crate::sync::IrqLock::new(alloc::collections::BTreeMap::new());

fn profile_sample(rip: u64) {
    let mut p = PROFILE.lock();
    if p.len() < 4096 || p.contains_key(&rip) {
        *p.entry(rip).or_insert(0) += 1;
    }
}

/// Log the 30 most sampled kernel addresses and clear the profile.
pub fn profile_dump() {
    let p = core::mem::take(&mut *PROFILE.lock());
    let mut v: alloc::vec::Vec<(u64, u32)> = p.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u32 = v.iter().map(|x| x.1).sum();
    log::info!("profile: {total} kernel samples");
    for (rip, n) in v.iter().take(30) {
        log::info!("profile: {rip:#x} {n}");
    }
}

fn user_exception(frame: &mut TrapFrame) {
    let cr2 = Cr2::read_raw();
    interrupts::enable();
    let rip = frame.rip;
    let fault = |signo, code, addr| Fault {
        signo,
        code,
        addr,
        trapno: frame.vector,
        error_code: frame.error_code,
    };
    let f = match frame.vector {
        0 => fault(signal::SIGFPE, FPE_INTDIV, rip),
        1 => fault(signal::SIGTRAP, TRAP_TRACE, rip),
        3 => fault(signal::SIGTRAP, SI_KERNEL, 0),
        4 | 5 | 13 => fault(signal::SIGSEGV, SI_KERNEL, 0),
        6 | 7 => fault(signal::SIGILL, ILL_ILLOPN, rip),
        10..=12 => fault(signal::SIGBUS, SI_KERNEL, 0),
        14 => {
            let write = frame.error_code & PF_WRITE != 0;
            let exec = frame.error_code & PF_INSN != 0;
            // Not present: demand paging; present: copy-on-write.
            let mm = crate::process::current().mm();
            match mm.handle_fault(cr2, write, exec) {
                Ok(()) => return,
                Err(crate::mm::user::FaultError::Userfault(ctx, flags)) => {
                    // Wait for the userfaultfd monitor, then retry the
                    // instruction (or take a signal first).
                    ctx.wait_fault(cr2, flags, || mm.page_present(cr2));
                    return;
                }
                Err(crate::mm::user::FaultError::Bus) => {
                    signal::force_fault(fault(signal::SIGBUS, BUS_ADRERR, cr2));
                    return;
                }
                Err(crate::mm::user::FaultError::Segv) => {}
            }
            let code = if frame.error_code & PF_PRESENT != 0 {
                SEGV_ACCERR
            } else {
                SEGV_MAPERR
            };
            log::debug!(
                "pid {}: SIGSEGV at {:#x} (rip {:#x}, error {:#x})",
                crate::process::current().tid,
                cr2,
                rip,
                frame.error_code
            );
            fault(signal::SIGSEGV, code, cr2)
        }
        16 | 19 => fault(signal::SIGFPE, FPE_FLTINV, rip),
        17 => fault(signal::SIGBUS, BUS_ADRALN, cr2),
        v => panic!("unexpected user exception {v}"),
    };
    signal::force_fault(f);
}

fn name(v: u64) -> &'static str {
    match v {
        0 => "#DE divide error",
        1 => "#DB debug",
        4 => "#OF overflow",
        5 => "#BR bound range",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        10 => "#TS invalid TSS",
        11 => "#NP segment not present",
        12 => "#SS stack segment",
        13 => "#GP general protection",
        14 => "#PF page fault",
        16 => "#MF x87 floating point",
        17 => "#AC alignment check",
        19 => "#XM SIMD floating point",
        _ => "?",
    }
}

#[test_case]
fn breakpoint_returns() {
    let before = BREAKPOINTS.load(Ordering::Relaxed);
    x86_64::instructions::interrupts::int3();
    assert_eq!(BREAKPOINTS.load(Ordering::Relaxed), before + 1);
}
