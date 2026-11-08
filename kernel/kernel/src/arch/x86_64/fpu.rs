//! User FPU/SSE state (`fxsave` format, 512 bytes).
//!
//! The kernel is soft-float and never touches these registers, so while a
//! task is in the kernel its user FPU state is still live in the CPU. It is
//! saved and restored eagerly on every context switch.
//! TODO: XSAVE (AVX state) once user code built for newer CPUs runs.

#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct FpuState(pub [u8; 512]);

/// MXCSR bits user code may set (others make `fxrstor` fault).
const MXCSR_MASK: u32 = 0xffff;

impl FpuState {
    /// State after `execve`: x87 control word 0x37f, MXCSR 0x1f80.
    pub const fn initial() -> Self {
        let mut b = [0u8; 512];
        b[0] = 0x7f;
        b[1] = 0x03;
        b[24] = 0x80;
        b[25] = 0x1f;
        Self(b)
    }

    /// Save the CPU's current FPU state.
    pub fn save(&mut self) {
        unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) self.0.as_mut_ptr(), options(nostack)) };
    }

    /// Load this state into the CPU.
    pub fn restore(&self) {
        unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) self.0.as_ptr(), options(nostack)) };
    }

    /// Clear bits that would make `fxrstor` fault (state from user memory).
    pub fn sanitize(&mut self) {
        let mut mxcsr = u32::from_le_bytes(self.0[24..28].try_into().unwrap());
        mxcsr &= MXCSR_MASK;
        self.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    }
}
