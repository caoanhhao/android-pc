//! Architecture support. Only x86_64 is implemented; the rest of the kernel
//! should use the re-exports below rather than reaching into `x86_64::*`.

pub mod x86_64;

pub use self::x86_64::entry::TrapFrame;
pub use self::x86_64::fpu::FpuState;
pub use self::x86_64::{
    cpu_index, halt_forever, init_bsp_early, init_bsp_late, interrupts_disable, power_off,
    reboot, smp, wait_for_interrupt,
};
