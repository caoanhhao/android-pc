//! Architecture support. Only x86_64 is implemented; the rest of the kernel
//! should use the re-exports below rather than reaching into `x86_64::*`.

pub mod x86_64;

pub use self::x86_64::{
    cpu_index, halt_forever, idle_loop, init_bsp_early, init_bsp_late, interrupts_disable, smp,
    wait_for_interrupt,
};
