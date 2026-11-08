//! Scheduler: one global run queue shared by all CPUs, round-robin with
//! preemption from the LAPIC timer (`HZ` ticks per second on every CPU).
//!
//! - A task runs until it blocks, yields, exits, or a timer tick finds other
//!   runnable tasks while it is in user mode. The kernel itself is not
//!   preemptible: `schedule` is only called at well-defined points, never
//!   while holding a lock.
//! - The run-queue lock is held across the context switch and released by
//!   the next task in `finish_switch` (as Linux does with the rq lock).
//!   `on_cpu` keeps a task that was woken while still running elsewhere
//!   from being picked up twice.
//! - Each CPU has an idle context (its boot stack) that runs when the queue
//!   is empty; it loads the kernel page tables and halts.
//! - Sleeping uses one global queue of (key, deadline, task) entries: wait
//!   queues, futexes and timeouts are all keyed sleeps (good enough for B1;
//!   per-object wait queues and a timer wheel can replace it later).

pub mod timer;

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use x86_64::instructions::interrupts;

use crate::arch::x86_64::{cpu, entry, tlb};
use crate::arch::TrapFrame;
use crate::mm::paging::kernel_pml4;
use crate::process::{self, Task};
use crate::sync::IrqLock;

pub const RUNNING: u8 = 0;
pub const SLEEPING: u8 = 1;
pub const DEAD: u8 = 2;

/// Why a sleeping task was woken (`Task::wake_reason`).
const REASON_NONE: u8 = 0;
const REASON_WOKEN: u8 = 1;
const REASON_TIMEOUT: u8 = 2;

/// Sleep queue key: (namespace/object, address).
pub type WaitKey = (u64, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Ready,
    Timeout,
    Interrupted,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WaitMode {
    /// Any deliverable signal interrupts the wait.
    Interruptible,
    /// Only a fatal signal (SIGKILL / group exit) interrupts.
    Killable,
}

struct CpuSched {
    current: UnsafeCell<Option<Arc<Task>>>,
    /// Task switched away from, released in `finish_switch`.
    prev: UnsafeCell<Option<Arc<Task>>>,
    idle_rsp: UnsafeCell<u64>,
    need_resched: AtomicBool,
}

unsafe impl Sync for CpuSched {}

static CPUS: [CpuSched; cpu::MAX_CPUS] = [const {
    CpuSched {
        current: UnsafeCell::new(None),
        prev: UnsafeCell::new(None),
        idle_rsp: UnsafeCell::new(0),
        need_resched: AtomicBool::new(false),
    }
}; cpu::MAX_CPUS];

static RQ: IrqLock<VecDeque<Arc<Task>>> = IrqLock::new(VecDeque::new());
static RQ_LEN: AtomicUsize = AtomicUsize::new(0);
/// CPUs halted in the idle loop (bit = index).
static IDLE_CPUS: AtomicU64 = AtomicU64::new(0);

struct Sleeper {
    key: WaitKey,
    deadline: u64,
    task: Arc<Task>,
}

static SLEEPERS: IrqLock<Vec<Sleeper>> = IrqLock::new(Vec::new());

fn this_cpu() -> &'static CpuSched {
    &CPUS[cpu::index_fast()]
}

/// The task running on this CPU (panics in the idle context).
pub fn current() -> Arc<Task> {
    try_current().expect("no current task")
}

pub fn try_current() -> Option<Arc<Task>> {
    interrupts::without_interrupts(|| unsafe { (*this_cpu().current.get()).clone() })
}

pub fn set_need_resched() {
    this_cpu().need_resched.store(true, Ordering::Relaxed);
}

/// Make a new or woken task runnable. Caller holds the run-queue lock.
fn enqueue_locked(rq: &mut VecDeque<Arc<Task>>, task: Arc<Task>) {
    if !task.queued.swap(true, Ordering::SeqCst) {
        rq.push_back(task);
        RQ_LEN.store(rq.len(), Ordering::SeqCst);
        // Wake one idle CPU (not ourselves) so the task starts right away.
        let idle = IDLE_CPUS.load(Ordering::SeqCst) & !(1u64 << cpu::index_fast());
        if idle != 0 {
            tlb::kick(idle.trailing_zeros() as usize);
        }
    }
}

/// Add a newly created task to the run queue.
pub fn spawn(task: Arc<Task>) {
    let mut rq = RQ.lock();
    task.state.store(RUNNING, Ordering::SeqCst);
    enqueue_locked(&mut rq, task);
}

/// Wake `task` if it is sleeping.
pub fn wake_task(task: &Arc<Task>) {
    let mut rq = RQ.lock();
    if task
        .state
        .compare_exchange(SLEEPING, RUNNING, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        enqueue_locked(&mut rq, task.clone());
    }
}

/// Give up the CPU. Returns when the scheduler picks this task again (or
/// never, for a task that set itself DEAD).
pub fn schedule() {
    let were_enabled = interrupts::are_enabled();
    let mut rq = RQ.lock();
    let me = cpu::index_fast();
    let cpu = &CPUS[me];
    cpu.need_resched.store(false, Ordering::Relaxed);
    let prev = unsafe { (*cpu.current.get()).clone() };

    if let Some(p) = &prev
        && p.state.load(Ordering::SeqCst) == RUNNING {
            enqueue_locked(&mut rq, p.clone());
        }
    // Pick the first runnable task not still running on another CPU;
    // drop dead tasks that were woken just before exiting.
    let mut next = None;
    let mut i = 0;
    while i < rq.len() {
        let t = &rq[i];
        let is_prev = prev.as_ref().is_some_and(|p| Arc::ptr_eq(p, t));
        if t.state.load(Ordering::SeqCst) == DEAD {
            let t = rq.remove(i).unwrap();
            t.queued.store(false, Ordering::SeqCst);
            continue;
        }
        if is_prev || !t.on_cpu.load(Ordering::SeqCst) {
            let t = rq.remove(i).unwrap();
            t.queued.store(false, Ordering::SeqCst);
            next = Some(t);
            break;
        }
        i += 1;
    }
    RQ_LEN.store(rq.len(), Ordering::SeqCst);

    let same = match (&prev, &next) {
        (Some(p), Some(n)) => Arc::ptr_eq(p, n),
        (None, None) => true,
        _ => false,
    };
    if same {
        drop(rq);
        return;
    }

    // Switch user context: FPU, FS/GS bases, address space, kernel stack.
    if let Some(p) = &prev {
        p.save_user_context();
        p.mm_ref().active_cpus.fetch_and(!(1u64 << me), Ordering::SeqCst);
    }
    let save_rsp = match &prev {
        Some(p) => p.ctx_rsp.get(),
        None => cpu.idle_rsp.get(),
    };
    let next_rsp = match &next {
        Some(n) => {
            n.on_cpu.store(true, Ordering::SeqCst);
            let mm = n.mm_ref();
            mm.active_cpus.fetch_or(1u64 << me, Ordering::SeqCst);
            mm.activate();
            n.load_user_context();
            cpu::set_kernel_stack(n.kstack_top());
            unsafe { *n.ctx_rsp.get() }
        }
        None => {
            unsafe {
                x86_64::registers::control::Cr3::write(
                    x86_64::structures::paging::PhysFrame::containing_address(kernel_pml4()),
                    x86_64::registers::control::Cr3Flags::empty(),
                )
            };
            unsafe { *cpu.idle_rsp.get() }
        }
    };
    unsafe {
        *cpu.current.get() = next;
        *cpu.prev.get() = prev;
    }
    // The next context releases the lock (finish_switch).
    rq.leak();
    unsafe { entry::switch_stacks(save_rsp, next_rsp) };
    finish_switch();
    if were_enabled {
        interrupts::enable();
    }
}

/// Second half of a context switch, run by the task (or idle context) we
/// switched to: release the run queue and the previous task.
pub fn finish_switch() {
    let cpu = this_cpu();
    let prev = unsafe { (*cpu.prev.get()).take() };
    if let Some(p) = &prev {
        p.on_cpu.store(false, Ordering::SeqCst);
    }
    unsafe { RQ.force_unlock() };
    // Possibly the last reference to a dead task: frees its kernel stack
    // and address space. We are on another stack and page table now.
    drop(prev);
}

pub fn yield_now() {
    schedule();
}

/// Per-CPU scheduler loop for when nothing is runnable.
pub fn idle() -> ! {
    let bit = 1u64 << cpu::index_fast();
    loop {
        interrupts::disable();
        IDLE_CPUS.fetch_or(bit, Ordering::SeqCst);
        if RQ_LEN.load(Ordering::SeqCst) > 0 {
            IDLE_CPUS.fetch_and(!bit, Ordering::SeqCst);
            schedule();
            interrupts::enable();
        } else {
            interrupts::enable_and_hlt();
            IDLE_CPUS.fetch_and(!bit, Ordering::SeqCst);
        }
    }
}

/// Timer interrupt (every CPU): time keeping on the BSP, timeouts, and a
/// preemption request when other tasks are waiting.
pub fn timer_interrupt() {
    if cpu::index_fast() == 0 {
        crate::time::tick();
        expire_sleepers();
        timer::run_expired();
    }
    if RQ_LEN.load(Ordering::Relaxed) > 0 {
        set_need_resched();
    }
}

/// Work to do before returning to user mode: reschedule, deliver signals,
/// finish an interrupted syscall. Returns with interrupts disabled.
pub fn exit_to_user(frame: &mut TrapFrame) {
    loop {
        interrupts::disable();
        if this_cpu().need_resched.load(Ordering::Relaxed) {
            interrupts::enable();
            schedule();
            continue;
        }
        if process::signal::has_pending() {
            interrupts::enable();
            process::signal::do_signal(frame);
            continue;
        }
        process::signal::finish_syscall(frame);
        break;
    }
}

// ---------------------------------------------------------------------------
// Sleeping and waking.

fn register(task: &Arc<Task>, key: WaitKey, deadline: u64) {
    let mut s = SLEEPERS.lock();
    task.wake_reason.store(REASON_NONE, Ordering::SeqCst);
    task.state.store(SLEEPING, Ordering::SeqCst);
    s.push(Sleeper {
        key,
        deadline,
        task: task.clone(),
    });
}

/// Remove `task`'s sleep entry if still there; returns the wake reason.
fn unregister(task: &Arc<Task>) -> u8 {
    let mut s = SLEEPERS.lock();
    if let Some(i) = s.iter().position(|e| Arc::ptr_eq(&e.task, task)) {
        s.remove(i);
    }
    task.state.store(RUNNING, Ordering::SeqCst);
    task.wake_reason.load(Ordering::SeqCst)
}

/// Re-arm a sleep entry after a spurious wake-up; false if a waker or the
/// timer already removed it.
fn rearm(task: &Arc<Task>) -> bool {
    let s = SLEEPERS.lock();
    if s.iter().any(|e| Arc::ptr_eq(&e.task, task)) {
        task.state.store(SLEEPING, Ordering::SeqCst);
        true
    } else {
        false
    }
}

fn interrupted(mode: WaitMode) -> bool {
    match mode {
        WaitMode::Interruptible => process::signal::has_pending(),
        WaitMode::Killable => process::signal::fatal_pending(),
    }
}

/// Sleep on `key` until `ready()` returns true (re-checked after every
/// wake-up), the monotonic `deadline` (ns) passes, or a signal arrives.
pub fn wait_until(key: WaitKey, deadline: Option<u64>, mode: WaitMode, mut ready: impl FnMut() -> bool) -> Wake {
    let task = current();
    let deadline = deadline.unwrap_or(u64::MAX);
    loop {
        if ready() {
            return Wake::Ready;
        }
        if crate::time::monotonic_ns() >= deadline {
            return Wake::Timeout;
        }
        register(&task, key, deadline);
        if ready() {
            unregister(&task);
            return Wake::Ready;
        }
        if interrupted(mode) {
            unregister(&task);
            return Wake::Interrupted;
        }
        schedule();
        unregister(&task);
    }
}

/// Futex-style sleep: register on `key`, run `check` (its error aborts the
/// wait), then sleep until `wake_n`/`wake_all` on `key`, the deadline, or a
/// signal.
pub fn wait_on(key: WaitKey, deadline: Option<u64>, check: impl FnOnce() -> crate::errno::KResult<()>) -> crate::errno::KResult<Wake> {
    let task = current();
    let deadline = deadline.unwrap_or(u64::MAX);
    register(&task, key, deadline);
    if let Err(e) = check() {
        unregister(&task);
        return Err(e);
    }
    loop {
        if interrupted(WaitMode::Interruptible) {
            return Ok(match unregister(&task) {
                REASON_WOKEN => Wake::Ready,
                _ => Wake::Interrupted,
            });
        }
        if crate::time::monotonic_ns() >= deadline {
            return Ok(match unregister(&task) {
                REASON_WOKEN => Wake::Ready,
                _ => Wake::Timeout,
            });
        }
        schedule();
        if !rearm(&task) {
            return Ok(match unregister(&task) {
                REASON_TIMEOUT => Wake::Timeout,
                _ => Wake::Ready,
            });
        }
    }
}

/// Wake up to `n` tasks sleeping on `key` (oldest first); returns how many.
pub fn wake_n(key: WaitKey, n: usize) -> usize {
    let mut s = SLEEPERS.lock();
    let mut woken = 0;
    let mut i = 0;
    while i < s.len() && woken < n {
        if s[i].key == key {
            let e = s.remove(i);
            e.task.wake_reason.store(REASON_WOKEN, Ordering::SeqCst);
            wake_task(&e.task);
            woken += 1;
        } else {
            i += 1;
        }
    }
    woken
}

pub fn wake_all(key: WaitKey) -> usize {
    wake_n(key, usize::MAX)
}

/// Move up to `n` sleepers from `from` to `to` without waking them.
pub fn requeue(from: WaitKey, to: WaitKey, n: usize) -> usize {
    let mut s = SLEEPERS.lock();
    let mut moved = 0;
    for e in s.iter_mut().filter(|e| e.key == from).take(n) {
        e.key = to;
        moved += 1;
    }
    moved
}

fn expire_sleepers() {
    let now = crate::time::monotonic_ns();
    let mut s = SLEEPERS.lock();
    let mut i = 0;
    while i < s.len() {
        if s[i].deadline <= now {
            let e = s.remove(i);
            e.task.wake_reason.store(REASON_TIMEOUT, Ordering::SeqCst);
            wake_task(&e.task);
        } else {
            i += 1;
        }
    }
}
