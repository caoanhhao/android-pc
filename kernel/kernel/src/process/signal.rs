//! POSIX signals: actions, masks, pending sets, delivery on return to user
//! mode through a Linux-compatible `rt_sigframe`, and `rt_sigreturn`.
//!
//! Standard and real-time signals both keep one pending bit and one
//! siginfo (real-time signals do not queue yet). Stop/continue signals are
//! accepted but job control is not implemented: SIGSTOP/SIGTSTP/... are
//! ignored, SIGCONT does nothing (phase B4/B5).

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{Task, current, lifecycle, TABLE};
use crate::arch::x86_64::entry::{RFLAGS_DF, RFLAGS_RF, RFLAGS_TF, RFLAGS_USER_MASK};
use crate::arch::x86_64::gdt;
use crate::arch::{FpuState, TrapFrame};
use crate::errno::*;
use crate::sched;

pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGSEGV: u32 = 11;
pub const SIGPIPE: u32 = 13;
pub const SIGCHLD: u32 = 17;
pub const SIGSTOP: u32 = 19;
pub const NSIG: usize = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

pub const SA_NOCLDWAIT: u64 = 2;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

pub const SS_ONSTACK: i32 = 1;
pub const SS_DISABLE: i32 = 2;
pub const MINSIGSTKSZ: u64 = 2048;

pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_TKILL: i32 = -6;
pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;

const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

pub const fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

/// Kernel-side siginfo; serialized to the 128-byte user layout.
#[derive(Clone, Copy, Default, Debug)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub errno: i32,
    /// si_pid / si_uid (kill, SIGCHLD) ...
    pub pid: i32,
    pub uid: u32,
    /// ... or si_addr (faults).
    pub addr: Option<u64>,
    /// si_status (SIGCHLD).
    pub status: Option<i32>,
}

impl SigInfo {
    pub fn user(signo: u32, code: i32, pid: u32) -> Self {
        Self {
            signo: signo as i32,
            code,
            pid: pid as i32,
            ..Default::default()
        }
    }

    pub fn to_bytes(self) -> [u8; 128] {
        let mut b = [0u8; 128];
        b[0..4].copy_from_slice(&self.signo.to_le_bytes());
        b[4..8].copy_from_slice(&self.errno.to_le_bytes());
        b[8..12].copy_from_slice(&self.code.to_le_bytes());
        match self.addr {
            Some(a) => b[16..24].copy_from_slice(&a.to_le_bytes()),
            None => {
                b[16..20].copy_from_slice(&self.pid.to_le_bytes());
                b[20..24].copy_from_slice(&self.uid.to_le_bytes());
                if let Some(s) = self.status {
                    b[24..28].copy_from_slice(&s.to_le_bytes());
                }
            }
        }
        b
    }
}

#[derive(Clone, Copy, Default)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    pub flags: i32,
}

impl AltStack {
    fn enabled(&self) -> bool {
        self.flags & SS_DISABLE == 0 && self.size != 0
    }

    fn contains(&self, sp: u64) -> bool {
        self.enabled() && sp > self.sp && sp <= self.sp + self.size
    }
}

pub struct ProcessSignals {
    pub actions: [SigAction; NSIG],
    /// Process-directed pending signals.
    pub pending: u64,
    pub info: [SigInfo; NSIG],
}

impl Default for ProcessSignals {
    fn default() -> Self {
        Self {
            actions: [SigAction::default(); NSIG],
            pending: 0,
            info: [SigInfo::default(); NSIG],
        }
    }
}

impl ProcessSignals {
    /// Copy for a new process (fork): actions only.
    pub fn fork(&self) -> Self {
        Self {
            actions: self.actions,
            ..Default::default()
        }
    }

    /// execve: handled signals revert to SIG_DFL; ignored stay ignored.
    pub fn reset_for_exec(&mut self) {
        for a in self.actions.iter_mut() {
            let handler = if a.handler == SIG_IGN { SIG_IGN } else { SIG_DFL };
            *a = SigAction { handler, ..Default::default() };
        }
    }

    fn ignored(&self, sig: u32) -> bool {
        let h = self.actions[sig as usize - 1].handler;
        h == SIG_IGN || (h == SIG_DFL && default_action(sig) == DefaultAction::Ignore)
    }
}

pub struct ThreadSignals {
    pub mask: u64,
    /// Thread-directed pending signals (tgkill, faults).
    pub pending: u64,
    pub info: [SigInfo; NSIG],
    pub altstack: AltStack,
    /// Mask to restore after a handler run from rt_sigsuspend.
    pub saved_mask: Option<u64>,
}

impl Default for ThreadSignals {
    fn default() -> Self {
        Self {
            mask: 0,
            pending: 0,
            info: [SigInfo::default(); NSIG],
            altstack: AltStack::default(),
            saved_mask: None,
        }
    }
}

#[derive(PartialEq, Eq)]
enum DefaultAction {
    Terminate,
    Core,
    Ignore,
    Stop,
    Continue,
}

fn default_action(sig: u32) -> DefaultAction {
    match sig {
        3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31 => DefaultAction::Core,
        17 | 23 | 28 => DefaultAction::Ignore,
        18 => DefaultAction::Continue,
        19..=22 => DefaultAction::Stop,
        _ => DefaultAction::Terminate,
    }
}

pub fn valid(sig: u32) -> bool {
    (1..=NSIG as u32).contains(&sig)
}

/// Queue a process-directed signal and wake its threads.
pub fn send_process(pid: u32, sig: u32, info: SigInfo) -> KResult<()> {
    let (proc, threads) = {
        let t = TABLE.lock();
        let node = t.procs.get(&pid).ok_or(ESRCH)?;
        let threads: Vec<Arc<Task>> = node.threads.iter().filter_map(|tid| t.tasks.get(tid).cloned()).collect();
        (node.proc.clone(), threads)
    };
    if sig == 0 || threads.is_empty() {
        return Ok(()); // existence check, or a zombie
    }
    if sig == SIGKILL {
        lifecycle::kill_group(pid, sig as i32);
        return Ok(());
    }
    {
        let mut ps = proc.sig.lock();
        let blocked_somewhere = threads.iter().any(|t| t.sig.lock().mask & bit(sig) != 0);
        if ps.ignored(sig) && !blocked_somewhere {
            return Ok(());
        }
        if matches!(default_action(sig), DefaultAction::Stop | DefaultAction::Continue)
            && ps.actions[sig as usize - 1].handler == SIG_DFL
        {
            return Ok(()); // no job control yet
        }
        if ps.pending & bit(sig) == 0 {
            ps.pending |= bit(sig);
            ps.info[sig as usize - 1] = info;
        }
    }
    for t in &threads {
        sched::wake_task(t);
    }
    Ok(())
}

/// Queue a thread-directed signal (tgkill, tkill).
pub fn send_thread(task: &Arc<Task>, sig: u32, info: SigInfo) {
    if sig == 0 {
        return;
    }
    if sig == SIGKILL {
        lifecycle::kill_group(task.pid(), sig as i32);
        return;
    }
    {
        let ps = task.process.sig.lock();
        let mut ts = task.sig.lock();
        if ps.ignored(sig) && ts.mask & bit(sig) == 0 {
            return;
        }
        if ts.pending & bit(sig) == 0 {
            ts.pending |= bit(sig);
            ts.info[sig as usize - 1] = info;
        }
    }
    sched::wake_task(task);
}

/// Signal to the calling thread from the kernel (e.g. SIGPIPE).
pub fn send_to_current(sig: u32) {
    let t = current();
    send_thread(&t, sig, SigInfo::user(sig, SI_KERNEL, 0));
}

/// A synchronous fault in user mode.
pub struct Fault {
    pub signo: u32,
    pub code: i32,
    pub addr: u64,
    pub trapno: u64,
    pub error_code: u64,
}

/// Deliver a fault signal to the current thread. If the signal is blocked
/// or ignored, it is unblocked and reset to the default action (as Linux's
/// force_sig does): a fault cannot be skipped.
pub fn force_fault(f: Fault) {
    let t = current();
    {
        let mut ps = t.process.sig.lock();
        let mut ts = t.sig.lock();
        let i = f.signo as usize - 1;
        if ps.actions[i].handler == SIG_IGN || ts.mask & bit(f.signo) != 0 {
            ps.actions[i].handler = SIG_DFL;
            ts.mask &= !bit(f.signo);
        }
        ts.pending |= bit(f.signo);
        ts.info[i] = SigInfo {
            signo: f.signo as i32,
            code: f.code,
            addr: Some(f.addr),
            ..Default::default()
        };
    }
    LAST_FAULT.lock().replace((t.tid, f.trapno, f.error_code, f.addr));
}

/// (tid, trapno, error code, cr2) of the last fault, for the sigcontext.
static LAST_FAULT: spin::Mutex<Option<(u32, u64, u64, u64)>> = spin::Mutex::new(None);

/// Signals the current thread would act on now.
fn deliverable(t: &Task) -> u64 {
    let pp = t.process.sig.lock().pending;
    let ts = t.sig.lock();
    (ts.pending | pp) & !(ts.mask & !UNBLOCKABLE)
}

/// A signal is ready for the current thread (interrupts sleeps).
pub fn has_pending() -> bool {
    match sched::try_current() {
        Some(t) => deliverable(&t) != 0,
        None => false,
    }
}

/// SIGKILL is pending for the current thread (group exit in progress).
pub fn fatal_pending() -> bool {
    sched::try_current().is_some_and(|t| t.sig.lock().pending & bit(SIGKILL) != 0)
}

/// Remove `sig` from the pending sets (sigaction to SIG_IGN).
pub fn flush_pending(pid: u32, sig: u32) {
    let threads: Vec<Arc<Task>> = {
        let t = TABLE.lock();
        t.procs
            .get(&pid)
            .map(|n| n.threads.iter().filter_map(|tid| t.tasks.get(tid).cloned()).collect())
            .unwrap_or_default()
    };
    for t in threads {
        t.sig.lock().pending &= !bit(sig);
    }
}

/// Dequeue one signal from `allowed` (thread-directed first).
pub fn dequeue(t: &Task, allowed: u64) -> Option<(u32, SigInfo)> {
    let mut ps = t.process.sig.lock();
    let mut ts = t.sig.lock();
    let pick = |set: u64| -> Option<u32> {
        if set == 0 {
            None
        } else if set & bit(SIGKILL) != 0 {
            Some(SIGKILL)
        } else {
            Some(set.trailing_zeros() + 1)
        }
    };
    if let Some(sig) = pick(ts.pending & allowed) {
        ts.pending &= !bit(sig);
        return Some((sig, ts.info[sig as usize - 1]));
    }
    if let Some(sig) = pick(ps.pending & allowed) {
        ps.pending &= !bit(sig);
        return Some((sig, ps.info[sig as usize - 1]));
    }
    None
}

/// Handle a syscall interrupted with ERESTARTSYS: restart it, or make it
/// fail with EINTR when a handler without SA_RESTART runs.
fn restart_syscall(frame: &mut TrapFrame, handler_flags: Option<u64>) {
    if !frame.is_syscall() || frame.error_code == u64::MAX {
        return;
    }
    if frame.rax as i64 == -(ERESTARTSYS.0 as i64) {
        match handler_flags {
            Some(f) if f & SA_RESTART == 0 => frame.rax = (-(EINTR.0 as i64)) as u64,
            _ => {
                frame.rax = frame.error_code;
                frame.rip -= 2;
            }
        }
    }
    frame.error_code = u64::MAX;
}

/// Called last on the way to user mode, with no signal left to deliver.
pub fn finish_syscall(frame: &mut TrapFrame) {
    restart_syscall(frame, None);
    if let Some(t) = sched::try_current() {
        let mut ts = t.sig.lock();
        if let Some(m) = ts.saved_mask.take() {
            ts.mask = m;
        }
    }
}

/// Act on one pending signal of the current thread.
pub fn do_signal(frame: &mut TrapFrame) {
    let t = current();
    let mask = t.sig.lock().mask & !UNBLOCKABLE;
    let Some((sig, info)) = dequeue(&t, !mask) else {
        return;
    };
    if sig == SIGKILL {
        lifecycle::exit_group_signal(SIGKILL as i32);
    }
    let action = t.process.sig.lock().actions[sig as usize - 1];
    match action.handler {
        SIG_IGN => {}
        SIG_DFL => match default_action(sig) {
            DefaultAction::Ignore | DefaultAction::Stop | DefaultAction::Continue => {}
            DefaultAction::Terminate | DefaultAction::Core => lifecycle::exit_group_signal(sig as i32),
        },
        _ => {
            restart_syscall(frame, Some(action.flags));
            if setup_frame(&t, frame, sig, &info, &action).is_err() {
                log::debug!("tid {}: cannot build signal frame, SIGSEGV", t.tid);
                lifecycle::exit_group_signal(SIGSEGV as i32);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Signal frames (x86_64 rt_sigframe, as in Linux arch/x86/kernel/signal.c).

const UC_SIGCONTEXT_SS: u64 = 0x2;
const UC_STRICT_RESTORE_SS: u64 = 0x4;
const SIGCONTEXT_SIZE: usize = 256;
/// pretcode + ucontext (flags, link, stack, sigcontext, sigmask).
const UCONTEXT_SIZE: usize = 8 + 8 + 24 + SIGCONTEXT_SIZE + 8;
const FRAME_SIZE: usize = 8 + UCONTEXT_SIZE + 128;

fn put(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn get(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn setup_frame(t: &Arc<Task>, frame: &mut TrapFrame, sig: u32, info: &SigInfo, act: &SigAction) -> KResult<()> {
    if act.flags & SA_RESTORER == 0 {
        return Err(EFAULT);
    }
    let mm = t.mm();
    let (altstack, old_mask) = {
        let mut ts = t.sig.lock();
        let old = ts.saved_mask.take().unwrap_or(ts.mask);
        (ts.altstack, old)
    };
    let mut sp = frame.rsp - 128; // red zone
    let on_alt = altstack.contains(frame.rsp);
    if act.flags & SA_ONSTACK != 0 && altstack.enabled() && !on_alt {
        sp = altstack.sp + altstack.size;
    }
    let fp_addr = (sp - 512) & !63;
    let frame_addr = ((fp_addr - FRAME_SIZE as u64) & !15) - 8;

    let mut fpu = FpuState::initial();
    fpu.save();
    mm.copy_out(fp_addr, &fpu.0)?;

    let (trapno, err, cr2) = match LAST_FAULT.lock().take() {
        Some((tid, trapno, err, cr2)) if tid == t.tid && info.addr.is_some() => (trapno, err, cr2),
        _ => (0, 0, 0),
    };
    let mut b = [0u8; FRAME_SIZE];
    put(&mut b, 0, act.restorer);
    let uc = 8;
    put(&mut b, uc, UC_SIGCONTEXT_SS | UC_STRICT_RESTORE_SS);
    put(&mut b, uc + 8, 0);
    // uc_stack
    put(&mut b, uc + 16, altstack.sp);
    let ss_flags = if !altstack.enabled() {
        SS_DISABLE
    } else if on_alt {
        SS_ONSTACK
    } else {
        0
    };
    b[uc + 24..uc + 28].copy_from_slice(&ss_flags.to_le_bytes());
    put(&mut b, uc + 32, altstack.size);
    // uc_mcontext (struct sigcontext)
    let sc = uc + 40;
    let regs = [
        frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15,
        frame.rdi, frame.rsi, frame.rbp, frame.rbx, frame.rdx, frame.rax, frame.rcx, frame.rsp,
        frame.rip, frame.rflags,
    ];
    for (i, r) in regs.iter().enumerate() {
        put(&mut b, sc + i * 8, *r);
    }
    // cs, gs, fs, ss (u16 each)
    let segs = (frame.cs & 0xffff) | ((frame.ss & 0xffff) << 48);
    put(&mut b, sc + 144, segs);
    put(&mut b, sc + 152, err);
    put(&mut b, sc + 160, trapno);
    put(&mut b, sc + 168, old_mask);
    put(&mut b, sc + 176, cr2);
    put(&mut b, sc + 184, fp_addr);
    put(&mut b, uc + 40 + SIGCONTEXT_SIZE, old_mask);
    let info_off = 8 + UCONTEXT_SIZE;
    b[info_off..info_off + 128].copy_from_slice(&info.to_bytes());
    mm.copy_out(frame_addr, &b)?;

    // The handler starts with a clean FPU state.
    FpuState::initial().restore();
    frame.rsp = frame_addr;
    frame.rip = act.handler;
    frame.rdi = sig as u64;
    frame.rsi = frame_addr + info_off as u64;
    frame.rdx = frame_addr + uc as u64;
    frame.rax = 0;
    frame.rflags &= !(RFLAGS_TF | RFLAGS_DF | RFLAGS_RF);
    frame.cs = gdt::USER_CS.0 as u64;
    frame.ss = gdt::USER_DS.0 as u64;

    let mut ps = t.process.sig.lock();
    let mut ts = t.sig.lock();
    ts.mask = old_mask | act.mask;
    if act.flags & SA_NODEFER == 0 {
        ts.mask |= bit(sig);
    }
    ts.mask &= !UNBLOCKABLE;
    if act.flags & SA_RESETHAND != 0 {
        ps.actions[sig as usize - 1] = SigAction::default();
    }
    Ok(())
}

/// `rt_sigreturn`: restore the context saved by `setup_frame`.
pub fn sigreturn(frame: &mut TrapFrame) -> KResult<u64> {
    let t = current();
    let mm = t.mm();
    let frame_addr = frame.rsp - 8;
    let mut b = [0u8; FRAME_SIZE];
    if mm.copy_in(frame_addr, &mut b).is_err() {
        lifecycle::exit_group_signal(SIGSEGV as i32);
    }
    let uc = 8;
    let sc = uc + 40;
    let r = |i: usize| get(&b, sc + i * 8);
    frame.r8 = r(0);
    frame.r9 = r(1);
    frame.r10 = r(2);
    frame.r11 = r(3);
    frame.r12 = r(4);
    frame.r13 = r(5);
    frame.r14 = r(6);
    frame.r15 = r(7);
    frame.rdi = r(8);
    frame.rsi = r(9);
    frame.rbp = r(10);
    frame.rbx = r(11);
    frame.rdx = r(12);
    frame.rax = r(13);
    frame.rcx = r(14);
    frame.rsp = r(15);
    frame.rip = r(16);
    frame.rflags = (frame.rflags & !RFLAGS_USER_MASK) | (r(17) & RFLAGS_USER_MASK);
    frame.cs = gdt::USER_CS.0 as u64;
    frame.ss = gdt::USER_DS.0 as u64;
    frame.error_code = u64::MAX; // not a restartable syscall any more
    let fp_addr = get(&b, sc + 184);
    if fp_addr != 0 {
        let mut fpu = FpuState::initial();
        if mm.copy_in(fp_addr, &mut fpu.0).is_err() {
            lifecycle::exit_group_signal(SIGSEGV as i32);
        }
        fpu.sanitize();
        fpu.restore();
    }
    let mask = get(&b, uc + 40 + SIGCONTEXT_SIZE);
    t.sig.lock().mask = mask & !UNBLOCKABLE;
    Ok(frame.rax)
}

// ---------------------------------------------------------------------------
// Syscalls.

pub fn sys_rt_sigaction(sig: u32, act: u64, oact: u64, size: u64) -> KResult<u64> {
    if size != 8 || !valid(sig) {
        return Err(EINVAL);
    }
    let t = current();
    let mm = t.mm();
    let new: Option<SigAction> = if act != 0 {
        if sig == SIGKILL || sig == SIGSTOP {
            return Err(EINVAL);
        }
        let mut b = [0u8; 32];
        mm.copy_in(act, &mut b)?;
        Some(SigAction {
            handler: get(&b, 0),
            flags: get(&b, 8),
            restorer: get(&b, 16),
            mask: get(&b, 24) & !UNBLOCKABLE,
        })
    } else {
        None
    };
    let (old, now_ignored) = {
        let mut ps = t.process.sig.lock();
        let old = ps.actions[sig as usize - 1];
        if let Some(a) = new {
            ps.actions[sig as usize - 1] = a;
        }
        let ign = new.is_some() && ps.ignored(sig);
        if ign {
            ps.pending &= !bit(sig);
        }
        (old, ign)
    };
    if now_ignored {
        flush_pending(t.pid(), sig);
    }
    if oact != 0 {
        let mut b = [0u8; 32];
        put(&mut b, 0, old.handler);
        put(&mut b, 8, old.flags);
        put(&mut b, 16, old.restorer);
        put(&mut b, 24, old.mask);
        mm.copy_out(oact, &b)?;
    }
    Ok(0)
}

pub fn sys_rt_sigprocmask(how: u32, set: u64, oset: u64, size: u64) -> KResult<u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    let t = current();
    let mm = t.mm();
    let new = if set != 0 {
        let mut b = [0u8; 8];
        mm.copy_in(set, &mut b)?;
        Some(u64::from_le_bytes(b))
    } else {
        None
    };
    let old = {
        let mut ts = t.sig.lock();
        let old = ts.mask;
        if let Some(s) = new {
            ts.mask = match how {
                0 => old | s,
                1 => old & !s,
                2 => s,
                _ => return Err(EINVAL),
            } & !UNBLOCKABLE;
        }
        old
    };
    if oset != 0 {
        mm.copy_out(oset, &old.to_le_bytes())?;
    }
    Ok(0)
}

pub fn sys_rt_sigpending(set: u64, size: u64) -> KResult<u64> {
    if size > 8 {
        return Err(EINVAL);
    }
    let t = current();
    let pp = t.process.sig.lock().pending;
    let v = {
        let ts = t.sig.lock();
        (ts.pending | pp) & ts.mask
    };
    t.mm().copy_out(set, &v.to_le_bytes()[..size as usize])?;
    Ok(0)
}

pub fn sys_rt_sigsuspend(set: u64, size: u64) -> KResult<u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    let t = current();
    let mut b = [0u8; 8];
    t.mm().copy_in(set, &mut b)?;
    {
        let mut ts = t.sig.lock();
        let old = ts.mask;
        ts.saved_mask = Some(old);
        ts.mask = u64::from_le_bytes(b) & !UNBLOCKABLE;
    }
    sched::wait_until((WAIT_SIGSUSPEND, t.tid as u64), None, sched::WaitMode::Interruptible, || false);
    Err(EINTR)
}

const WAIT_SIGSUSPEND: u64 = 0x5349_4753_5553_5000;
const WAIT_SIGTIMED: u64 = 0x5349_4754_494d_4500;

pub fn sys_rt_sigtimedwait(set: u64, info: u64, timeout: u64, size: u64) -> KResult<u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    let t = current();
    let mm = t.mm();
    let mut b = [0u8; 8];
    mm.copy_in(set, &mut b)?;
    let wanted = u64::from_le_bytes(b) & !UNBLOCKABLE;
    let deadline = if timeout != 0 {
        let (s, ns) = crate::syscall::time::read_timespec(&mm, timeout)?;
        Some(crate::time::monotonic_ns().saturating_add(s * 1_000_000_000 + ns))
    } else {
        None
    };
    let mut got = None;
    let w = sched::wait_until((WAIT_SIGTIMED, t.tid as u64), deadline, sched::WaitMode::Interruptible, || {
        got = dequeue(&t, wanted);
        got.is_some()
    });
    match (w, got) {
        (_, Some((sig, si))) => {
            if info != 0 {
                mm.copy_out(info, &si.to_bytes())?;
            }
            Ok(sig as u64)
        }
        (sched::Wake::Timeout, _) => Err(EAGAIN),
        _ => Err(EINTR),
    }
}

pub fn sys_sigaltstack(ss: u64, old: u64) -> KResult<u64> {
    let t = current();
    let mm = t.mm();
    let frame = unsafe { &*t.user_frame() };
    let cur = t.sig.lock().altstack;
    let on = cur.contains(frame.rsp);
    if ss != 0 {
        let mut b = [0u8; 24];
        mm.copy_in(ss, &mut b)?;
        if on {
            return Err(EPERM);
        }
        let flags = i32::from_le_bytes(b[8..12].try_into().unwrap());
        let new = if flags & SS_DISABLE != 0 {
            AltStack { sp: 0, size: 0, flags: SS_DISABLE }
        } else {
            if flags & !(1 << 31) != 0 {
                return Err(EINVAL);
            }
            let size = get(&b, 16);
            if size < MINSIGSTKSZ {
                return Err(ENOMEM);
            }
            AltStack { sp: get(&b, 0), size, flags: 0 }
        };
        t.sig.lock().altstack = new;
    }
    if old != 0 {
        let mut b = [0u8; 24];
        put(&mut b, 0, cur.sp);
        let flags = if !cur.enabled() {
            SS_DISABLE
        } else if on {
            SS_ONSTACK
        } else {
            0
        };
        b[8..12].copy_from_slice(&flags.to_le_bytes());
        put(&mut b, 16, cur.size);
        mm.copy_out(old, &b)?;
    }
    Ok(0)
}

pub fn sys_kill(pid: i32, sig: u32) -> KResult<u64> {
    if sig as usize > NSIG {
        return Err(EINVAL);
    }
    let me = current();
    let info = SigInfo::user(sig, SI_USER, me.pid());
    if pid > 0 {
        send_process(pid as u32, sig, info)?;
        return Ok(0);
    }
    // Process groups (0 = ours, -1 = everyone but init, < -1 = group).
    let targets: Vec<u32> = {
        let t = TABLE.lock();
        let my_pgid = t.procs.get(&me.pid()).map_or(0, |n| n.pgid);
        t.procs
            .iter()
            .filter(|&(&p, n)| match pid {
                0 => n.pgid == my_pgid,
                -1 => p != 1,
                g => n.pgid == (-g) as u32,
            })
            .filter(|&(_, n)| n.zombie.is_none())
            .map(|(&p, _)| p)
            .collect()
    };
    if targets.is_empty() {
        return Err(ESRCH);
    }
    for p in targets {
        let _ = send_process(p, sig, info);
    }
    Ok(0)
}

pub fn sys_tgkill(tgid: i32, tid: i32, sig: u32) -> KResult<u64> {
    if sig as usize > NSIG || tid <= 0 {
        return Err(EINVAL);
    }
    let task = TABLE.lock().tasks.get(&(tid as u32)).cloned().ok_or(ESRCH)?;
    if tgid > 0 && task.pid() != tgid as u32 {
        return Err(ESRCH);
    }
    let me = current();
    send_thread(&task, sig, SigInfo::user(sig, SI_TKILL, me.pid()));
    Ok(0)
}

pub fn child_info(pid: u32, status: i32) -> SigInfo {
    let (code, st) = if status & 0x7f == 0 {
        (CLD_EXITED, (status >> 8) & 0xff)
    } else {
        (CLD_KILLED, status & 0x7f)
    };
    SigInfo {
        signo: SIGCHLD as i32,
        code,
        pid: pid as i32,
        status: Some(st),
        ..Default::default()
    }
}

/// The parent ignores SIGCHLD explicitly: children are reaped at exit.
pub fn ignores_children(proc: &super::Process) -> bool {
    let a = proc.sig.lock().actions[SIGCHLD as usize - 1];
    a.handler == SIG_IGN || a.flags & SA_NOCLDWAIT != 0
}
