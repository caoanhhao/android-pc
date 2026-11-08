//! Application processor (AP) bring-up via the Limine MP response.
//!
//! APs get their own stack, GDT/TSS, per-CPU area and the shared IDT and
//! kernel page tables, enable their LAPIC, then park in `hlt` until the
//! scheduler (phase B1) gives them work.

use alloc::boxed::Box;
use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use limine::mp::MpInfo;

use super::cpu::{self, PerCpu};
use super::{apic, idt};
use crate::{boot, mm, time};

static TOTAL: AtomicUsize = AtomicUsize::new(1);
static ONLINE: AtomicUsize = AtomicUsize::new(1);

/// CPUs reported by the firmware.
#[allow(dead_code)]
pub fn total_cpus() -> usize {
    TOTAL.load(Ordering::Acquire)
}

/// CPUs that finished initialisation (BSP included).
pub fn online_cpus() -> usize {
    ONLINE.load(Ordering::Acquire)
}

/// Start every AP and wait (up to one second) for them to come online.
pub fn start_aps() {
    let Some(mp) = boot::mp() else {
        log::warn!("SMP: no MP response from the bootloader, running on the BSP only");
        return;
    };
    let cpus = mp.cpus();
    TOTAL.store(cpus.len(), Ordering::Release);
    log::info!(
        "SMP: {} CPU(s), BSP LAPIC id {}, x2APIC {}",
        cpus.len(),
        mp.bsp_lapic_id,
        mp.flags & limine::mp::MP_FLAG_X2APIC as u32 != 0
    );

    let aps = cpus.iter().filter(|c| c.lapic_id != mp.bsp_lapic_id);
    for (index, info) in (1..).zip(aps) {
        let mut pc = Box::new(PerCpu::new(index, info.lapic_id));
        pc.boot_stack_top = cpu::alloc_stack(cpu::AP_STACK_SIZE);
        pc.double_fault_stack_top = cpu::alloc_stack(cpu::IST_STACK_SIZE);
        info.bootstrap(ap_entry, Box::into_raw(pc) as u64);
    }

    let deadline = time::ticks() + time::HZ;
    while online_cpus() < cpus.len() && time::ticks() < deadline {
        crate::arch::wait_for_interrupt();
    }
    if online_cpus() < cpus.len() {
        log::error!(
            "SMP: only {}/{} CPUs came online",
            online_cpus(),
            cpus.len()
        );
    } else {
        log::info!("SMP: all {} CPUs online", cpus.len());
    }
}

/// First code an AP runs: still on Limine's page tables and stack.
unsafe extern "C" fn ap_entry(info: &MpInfo) -> ! {
    let pc = info.extra_argument() as *mut PerCpu;
    unsafe {
        mm::paging::activate_kernel_tables();
        asm!(
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "call {main}",
            "ud2",
            stack = in(reg) (*pc).boot_stack_top,
            main = sym ap_main,
            in("rdi") pc,
            options(noreturn),
        );
    }
}

extern "C" fn ap_main(pc: *mut PerCpu) -> ! {
    unsafe { PerCpu::install(pc) };
    idt::load();
    apic::init_cpu();
    log::debug!("AP online (LAPIC id {})", apic::id());
    ONLINE.fetch_add(1, Ordering::AcqRel);
    crate::arch::idle_loop()
}

#[test_case]
fn all_cpus_online() {
    assert_eq!(online_cpus(), total_cpus());
}

#[test_case]
fn per_cpu_area_matches_lapic() {
    let pc = cpu::current().expect("per-CPU area not installed");
    assert_eq!(pc.index, 0);
    assert_eq!(
        apic::id(),
        boot::mp().map_or(apic::id(), |mp| mp.bsp_lapic_id)
    );
}
