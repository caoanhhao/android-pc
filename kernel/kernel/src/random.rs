//! Kernel random bytes for `getrandom`, `/dev/urandom` and AT_RANDOM.
//!
//! RDRAND when the CPU has it, mixed with a xorshift generator seeded from
//! the TSC. Not a vetted CSPRNG; good enough until a proper one (ChaCha20
//! seeded from RDSEED/virtio-rng) lands.

use core::sync::atomic::{AtomicU64, Ordering};

static STATE: AtomicU64 = AtomicU64::new(0);

fn has_rdrand() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0
}

fn rdrand() -> Option<u64> {
    for _ in 0..10 {
        let v: u64;
        let ok: u8;
        unsafe { core::arch::asm!("rdrand {}", "setc {}", out(reg) v, out(reg_byte) ok, options(nomem, nostack)) };
        if ok != 0 {
            return Some(v);
        }
    }
    None
}

fn next() -> u64 {
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = crate::time::rdtsc() | 1;
    }
    // xorshift64*
    s ^= s >> 12;
    s ^= s << 25;
    s ^= s >> 27;
    STATE.store(s, Ordering::Relaxed);
    let mut v = s.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ crate::time::rdtsc().rotate_left(32);
    if has_rdrand()
        && let Some(r) = rdrand()
    {
        v ^= r;
    }
    v
}

pub fn fill(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        let v = next().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
}
