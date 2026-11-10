//! 8254 PIT, used only as a reference clock to calibrate the LAPIC timer.

use x86_64::instructions::port::Port;

const PIT_HZ: u64 = 1_193_182;

/// Run `start`, wait `ms` milliseconds using PIT channel 2, then return
/// `stop()`. Busy-waits; `ms` must be at most 54. None if the PIT never
/// reaches the count (chipsets that gate the 8254 off): given up after
/// about 2^34 TSC cycles (seconds on any CPU).
pub fn measure<R>(ms: u64, start: impl FnOnce(), stop: impl FnOnce() -> R) -> Option<R> {
    let count = PIT_HZ * ms / 1000;
    assert!(count > 0 && count <= 0xffff, "PIT interval out of range");
    unsafe {
        let mut gate = Port::<u8>::new(0x61);
        let mut command = Port::<u8>::new(0x43);
        let mut channel2 = Port::<u8>::new(0x42);

        let saved = gate.read();
        // Gate low (paused), speaker off.
        gate.write(saved & !0x03);
        // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count).
        command.write(0b1011_0000);
        channel2.write(count as u8);
        channel2.write((count >> 8) as u8);

        start();
        gate.write((saved & !0x02) | 0x01); // gate high: counting starts
        let t0 = crate::time::rdtsc();
        while gate.read() & 0x20 == 0 {
            if crate::time::rdtsc().wrapping_sub(t0) > 1 << 34 {
                gate.write(saved);
                return None;
            }
            core::hint::spin_loop();
        }
        let result = stop();
        gate.write(saved);
        Some(result)
    }
}
