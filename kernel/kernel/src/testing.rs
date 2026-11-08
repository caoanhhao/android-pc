//! In-kernel test framework (`cargo test` boots a test kernel in QEMU).
//!
//! Tests are `#[test_case]` functions run on the BSP after full boot; the
//! result is reported to the host through QEMU's `isa-debug-exit` device.

use x86_64::instructions::port::Port;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    /// QEMU exits with (0x10 << 1) | 1 = 33.
    Success = 0x10,
    /// QEMU exits with (0x11 << 1) | 1 = 35.
    Failed = 0x11,
}

/// I/O port of the `isa-debug-exit` device (see tools/run-qemu.sh).
const ISA_DEBUG_EXIT_PORT: u16 = 0xf4;

#[allow(dead_code)]
pub fn exit_qemu(code: QemuExitCode) -> ! {
    unsafe { Port::new(ISA_DEBUG_EXIT_PORT).write(code as u32) };
    // Not running under QEMU with the exit device: just stop.
    crate::arch::halt_forever()
}

pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        kprint!("test {} ... ", core::any::type_name::<T>());
        self();
        kprintln!("\x1b[32mok\x1b[0m");
    }
}

#[allow(dead_code)]
pub fn runner(tests: &[&dyn Testable]) {
    kprintln!("\nrunning {} tests", tests.len());
    for test in tests {
        test.run();
    }
    kprintln!("\ntest result: ok. {} passed; 0 failed\n", tests.len());
    exit_qemu(QemuExitCode::Success);
}
