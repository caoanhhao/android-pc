//! Kernel entry and exit: `syscall`, interrupt/exception stubs, the common
//! return path (`sysretq` or `iretq`) and the kernel-stack context switch.
//!
//! Every entry from ring 3 builds a [`TrapFrame`] at the top of the task's
//! kernel stack, in the order of Linux's `struct pt_regs`. Entries from
//! ring 0 (interrupts while in the kernel) build the same frame on the
//! current stack and skip `swapgs`.

use core::arch::global_asm;

use super::gdt;

/// `TrapFrame::vector` value for frames built by the `syscall` instruction.
pub const SYSCALL_VECTOR: u64 = 0x100;

/// Saved user (or interrupted kernel) state; layout is fixed by the
/// assembly below.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    /// Interrupt vector, or `SYSCALL_VECTOR`.
    pub vector: u64,
    /// CPU error code, or the syscall number (`orig_rax`; `u64::MAX` once
    /// the frame no longer belongs to a restartable syscall).
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

pub const TRAP_FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();
const _: () = assert!(TRAP_FRAME_SIZE == 176);

impl TrapFrame {
    pub fn user_mode(&self) -> bool {
        self.cs & 3 == 3
    }

    pub fn is_syscall(&self) -> bool {
        self.vector == SYSCALL_VECTOR
    }

    /// A fresh ring-3 frame starting at `rip` with stack `rsp`.
    pub fn new_user(rip: u64, rsp: u64) -> Self {
        Self {
            rip,
            rsp,
            cs: gdt::USER_CS.0 as u64,
            ss: gdt::USER_DS.0 as u64,
            rflags: RFLAGS_IF | RFLAGS_RESERVED1,
            error_code: u64::MAX,
            ..Default::default()
        }
    }

    /// Syscall arguments in the Linux x86_64 order.
    pub fn args(&self) -> [u64; 6] {
        [self.rdi, self.rsi, self.rdx, self.r10, self.r8, self.r9]
    }

    /// True when `sysretq` restores exactly this frame (RCX/R11 already
    /// hold RIP/RFLAGS and RIP is a canonical user address).
    pub fn sysret_ok(&self) -> bool {
        self.rcx == self.rip
            && self.r11 == self.rflags
            && self.rip < 0x0000_8000_0000_0000
            && self.cs == gdt::USER_CS.0 as u64
            && self.ss == gdt::USER_DS.0 as u64
            && self.rflags & (RFLAGS_TF | RFLAGS_RF) == 0
    }
}

pub const RFLAGS_RESERVED1: u64 = 1 << 1;
pub const RFLAGS_TF: u64 = 1 << 8;
pub const RFLAGS_IF: u64 = 1 << 9;
pub const RFLAGS_DF: u64 = 1 << 10;
pub const RFLAGS_RF: u64 = 1 << 16;
/// Flags user code may change through `rt_sigreturn` (Linux FIX_EFLAGS).
pub const RFLAGS_USER_MASK: u64 = 0x0001_0000 | 0x0004_0000 | 0x4000 | 0x0dd5 | 0x0100;

global_asm!(
    ".macro PUSH_REGS",
    "push rdi", "push rsi", "push rdx", "push rcx", "push rax",
    "push r8", "push r9", "push r10", "push r11",
    "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
    ".endm",
    ".macro POP_REGS",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop rbp", "pop rbx",
    "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rax", "pop rcx", "pop rdx", "pop rsi", "pop rdi",
    ".endm",
    "",
    // ---- syscall: RCX = user RIP, R11 = user RFLAGS, IF masked by SFMASK.
    ".globl __syscall_entry",
    "__syscall_entry:",
    "    swapgs",
    "    mov gs:[{user_rsp}], rsp",
    "    mov rsp, gs:[{kernel_rsp}]",
    "    push {user_ds}",
    "    push qword ptr gs:[{user_rsp}]",
    "    push r11",
    "    push {user_cs}",
    "    push rcx",
    "    push rax",
    "    push {syscall_vector}",
    "    PUSH_REGS",
    "    mov rdi, rsp",
    "    call {syscall_handler}",
    "    test al, al",
    "    jz __trap_return",
    // Fast path: the frame's RCX/R11 equal RIP/RFLAGS (checked in Rust).
    "    POP_REGS",
    "    add rsp, 16",
    "    mov rsp, [rsp + 24]",
    "    swapgs",
    "    sysretq",
    "",
    // ---- common exit: RSP points at a TrapFrame, interrupts are off.
    ".globl __trap_return",
    "__trap_return:",
    "    POP_REGS",
    "    add rsp, 16",
    "    test qword ptr [rsp + 8], 3",
    "    jz 2f",
    "    swapgs",
    "2:  iretq",
    "",
    // ---- interrupt / exception stubs.
    ".macro TRAP_STUB vec, has_error",
    ".globl __trap_stub_\\vec",
    "__trap_stub_\\vec:",
    ".if \\has_error == 0",
    "    push 0",
    ".endif",
    "    push \\vec",
    "    test qword ptr [rsp + 24], 3",
    "    jz 3f",
    "    swapgs",
    "3:  PUSH_REGS",
    "    cld",
    "    mov rdi, rsp",
    "    call {trap_handler}",
    "    jmp __trap_return",
    ".endm",
    "TRAP_STUB 0, 0",
    "TRAP_STUB 1, 0",
    "TRAP_STUB 3, 0",
    "TRAP_STUB 4, 0",
    "TRAP_STUB 5, 0",
    "TRAP_STUB 6, 0",
    "TRAP_STUB 7, 0",
    "TRAP_STUB 10, 1",
    "TRAP_STUB 11, 1",
    "TRAP_STUB 12, 1",
    "TRAP_STUB 13, 1",
    "TRAP_STUB 14, 1",
    "TRAP_STUB 16, 0",
    "TRAP_STUB 17, 1",
    "TRAP_STUB 19, 0",
    "TRAP_STUB 32, 0",
    "TRAP_STUB 240, 0",
    "TRAP_STUB 241, 0",
    "",
    // ---- switch_stacks(save_rsp: *mut u64, new_rsp: u64)
    ".globl __switch_stacks",
    "__switch_stacks:",
    "    push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
    "    mov [rdi], rsp",
    "    mov rsp, rsi",
    "    pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp",
    "    ret",
    "",
    // ---- first code a new task runs: RSP points at its TrapFrame.
    ".globl __ret_from_fork",
    "__ret_from_fork:",
    "    mov rdi, rsp",
    "    call {fork_tail}",
    "    jmp __trap_return",
    user_rsp = const super::cpu::PERCPU_USER_RSP,
    kernel_rsp = const super::cpu::PERCPU_KERNEL_RSP,
    user_ds = const gdt::USER_DS.0 as u64,
    user_cs = const gdt::USER_CS.0 as u64,
    syscall_vector = const SYSCALL_VECTOR,
    syscall_handler = sym syscall_handler,
    trap_handler = sym super::trap::trap_handler,
    fork_tail = sym fork_tail,
);

unsafe extern "C" {
    fn __syscall_entry();
    fn __switch_stacks(save_rsp: *mut u64, new_rsp: u64);
    fn __ret_from_fork();
}

macro_rules! stub_addrs {
    ($($v:literal $f:ident),*) => {{
        unsafe extern "C" {
            $( #[link_name = concat!("__trap_stub_", $v)] fn $f(); )*
        }
        [$( ($v as u8, $f as *const () as u64) ),*]
    }};
}

/// (vector, handler address) of every assembly stub.
pub fn trap_stubs() -> [(u8, u64); 18] {
    stub_addrs!(0 s0, 1 s1, 3 s3, 4 s4, 5 s5, 6 s6, 7 s7, 10 s10, 11 s11, 12 s12, 13 s13,
                14 s14, 16 s16, 17 s17, 19 s19, 32 s32, 240 s240, 241 s241)
}

pub fn syscall_entry_addr() -> u64 {
    __syscall_entry as *const () as u64
}

/// Save callee-saved registers and RSP into `*save_rsp`, continue on
/// `new_rsp`. Returns when something switches back to the saved RSP.
///
/// # Safety
/// `new_rsp` must have been produced by this function or `prepare_new_stack`.
pub unsafe fn switch_stacks(save_rsp: *mut u64, new_rsp: u64) {
    unsafe { __switch_stacks(save_rsp, new_rsp) }
}

/// Lay out a new task's kernel stack so that switching to it enters user
/// mode with `frame`. Returns the initial saved RSP.
///
/// # Safety
/// `stack_top` must be the 16-byte aligned top of an unused kernel stack.
pub unsafe fn prepare_new_stack(stack_top: u64, frame: &TrapFrame) -> u64 {
    unsafe {
        let frame_addr = stack_top - TRAP_FRAME_SIZE as u64;
        (frame_addr as *mut TrapFrame).write(*frame);
        let ret = frame_addr - 8;
        (ret as *mut u64).write(__ret_from_fork as *const () as u64);
        // rbp, rbx, r12..r15 popped by __switch_stacks.
        let rsp = ret - 6 * 8;
        core::ptr::write_bytes(rsp as *mut u64, 0, 6);
        rsp
    }
}

/// The trap frame at the top of a task's kernel stack.
pub fn user_frame(stack_top: u64) -> *mut TrapFrame {
    (stack_top - TRAP_FRAME_SIZE as u64) as *mut TrapFrame
}

/// Called from `__syscall_entry` with interrupts off. Returns true when the
/// fast `sysretq` path may be used.
extern "C" fn syscall_handler(frame: &mut TrapFrame) -> bool {
    x86_64::instructions::interrupts::enable();
    crate::syscall::dispatch(frame);
    crate::sched::exit_to_user(frame);
    frame.sysret_ok()
}

extern "C" fn fork_tail(frame: &mut TrapFrame) {
    crate::sched::finish_switch();
    crate::sched::exit_to_user(frame);
}
