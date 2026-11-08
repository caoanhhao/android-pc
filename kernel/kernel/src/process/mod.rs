//! Tasks (threads) and processes (thread groups).
//!
//! - `Task`: a schedulable thread with its kernel stack, saved context,
//!   user FPU/FS/GS state, address space and per-thread signal state.
//! - `Process`: what threads of a group share: pid, signal actions and
//!   process-wide pending signals, the fd table, cwd/umask, executable.
//! - `TABLE`: the global process tree (parent/children, threads, zombies)
//!   and the tid -> task map, under one lock.
//!
//! PIDs and TIDs share one number space; a process's pid is its first
//! thread's tid (the thread group leader).

pub mod elf;
pub mod futex;
pub mod lifecycle;
pub mod signal;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{FsBase, KernelGsBase};

use crate::arch::x86_64::entry;
use crate::arch::{FpuState, TrapFrame};
use crate::fs::file::FdTable;
use crate::fs::poll::WaitQueue;
use crate::fs::vfs::PathRef;
use crate::mm::kstack::KernelStack;
use crate::mm::user::AddressSpace;
use crate::sched;
use signal::{ProcessSignals, ThreadSignals};

pub struct Task {
    pub tid: u32,
    pub process: Arc<Process>,
    kstack: KernelStack,
    pub(crate) ctx_rsp: UnsafeCell<u64>,
    pub(crate) state: AtomicU8,
    pub(crate) on_cpu: AtomicBool,
    pub(crate) queued: AtomicBool,
    pub(crate) wake_reason: AtomicU8,
    pub fs_base: AtomicU64,
    pub gs_base: AtomicU64,
    fpu: UnsafeCell<FpuState>,
    /// Only touched by the task itself or by the scheduler on its CPU.
    mm: UnsafeCell<Arc<AddressSpace>>,
    pub sig: Mutex<ThreadSignals>,
    pub clear_child_tid: AtomicU64,
    pub robust_list: AtomicU64,
}

unsafe impl Send for Task {}
unsafe impl Sync for Task {}

impl Task {
    /// A task that starts in user mode with `frame`.
    pub fn new(tid: u32, process: Arc<Process>, mm: Arc<AddressSpace>, frame: &TrapFrame) -> Option<Arc<Task>> {
        let kstack = KernelStack::new()?;
        let rsp = unsafe { entry::prepare_new_stack(kstack.top(), frame) };
        Some(Arc::new(Task {
            tid,
            process,
            kstack,
            ctx_rsp: UnsafeCell::new(rsp),
            state: AtomicU8::new(sched::RUNNING),
            on_cpu: AtomicBool::new(false),
            queued: AtomicBool::new(false),
            wake_reason: AtomicU8::new(0),
            fs_base: AtomicU64::new(0),
            gs_base: AtomicU64::new(0),
            fpu: UnsafeCell::new(FpuState::initial()),
            mm: UnsafeCell::new(mm),
            sig: Mutex::new(ThreadSignals::default()),
            clear_child_tid: AtomicU64::new(0),
            robust_list: AtomicU64::new(0),
        }))
    }

    pub fn kstack_top(&self) -> u64 {
        self.kstack.top()
    }

    pub fn mm(&self) -> Arc<AddressSpace> {
        unsafe { (*self.mm.get()).clone() }
    }

    pub(crate) fn mm_ref(&self) -> &AddressSpace {
        unsafe { &*self.mm.get() }
    }

    /// Replace the address space (execve). Must be called by the task itself.
    pub fn set_mm(&self, mm: Arc<AddressSpace>) -> Arc<AddressSpace> {
        let me = crate::arch::x86_64::cpu::index_fast();
        x86_64::instructions::interrupts::without_interrupts(|| {
            mm.active_cpus.fetch_or(1 << me, Ordering::SeqCst);
            mm.activate();
            let old = unsafe { core::mem::replace(&mut *self.mm.get(), mm) };
            old.active_cpus.fetch_and(!(1 << me), Ordering::SeqCst);
            old
        })
    }

    /// The user frame at the top of this task's kernel stack.
    pub fn user_frame(&self) -> *mut TrapFrame {
        entry::user_frame(self.kstack_top())
    }

    /// Set the FPU state of a task that is not running (new child).
    pub fn set_fpu(&self, state: &FpuState) {
        unsafe { *self.fpu.get() = *state };
    }

    /// Scheduler: save the live user state of the task switched away from.
    pub(crate) fn save_user_context(&self) {
        unsafe { (*self.fpu.get()).save() };
        self.fs_base.store(FsBase::read().as_u64(), Ordering::Relaxed);
        self.gs_base.store(KernelGsBase::read().as_u64(), Ordering::Relaxed);
    }

    /// Scheduler: load the user state of the task switched to.
    pub(crate) fn load_user_context(&self) {
        unsafe { (*self.fpu.get()).restore() };
        FsBase::write(VirtAddr::new_truncate(self.fs_base.load(Ordering::Relaxed)));
        KernelGsBase::write(VirtAddr::new_truncate(self.gs_base.load(Ordering::Relaxed)));
    }

    pub fn pid(&self) -> u32 {
        self.process.pid
    }
}

/// File system context: working directory and umask.
#[derive(Clone)]
pub struct FsContext {
    pub cwd: PathRef,
    pub umask: u32,
}

pub struct Process {
    pub pid: u32,
    pub sig: Mutex<ProcessSignals>,
    pub files: Mutex<Arc<FdTable>>,
    pub fs: Mutex<FsContext>,
    /// The executable (for /proc/<pid>/exe and maps).
    pub exe: Mutex<Option<PathRef>>,
    /// argv joined by NULs (/proc/<pid>/cmdline).
    pub cmdline: Mutex<Vec<u8>>,
    /// Signal sent to the parent on exit (SIGCHLD for fork; 0 for none).
    pub exit_signal: u32,
    /// Set when a vfork child execs or exits (releases the parent).
    pub vfork_done: AtomicBool,
    pub comm: Mutex<[u8; 16]>,
    /// Woken when a signal is queued (signalfd readers and pollers).
    pub signalfd_wq: Arc<WaitQueue>,
}

impl Process {
    pub fn new(pid: u32, files: Arc<FdTable>, fs: FsContext, sig: ProcessSignals, exit_signal: u32) -> Arc<Process> {
        Arc::new(Process {
            pid,
            sig: Mutex::new(sig),
            files: Mutex::new(files),
            fs: Mutex::new(fs),
            exe: Mutex::new(None),
            cmdline: Mutex::new(Vec::new()),
            exit_signal,
            vfork_done: AtomicBool::new(false),
            comm: Mutex::new([0; 16]),
            signalfd_wq: WaitQueue::new(),
        })
    }

    pub fn files(&self) -> Arc<FdTable> {
        self.files.lock().clone()
    }

    pub fn cwd(&self) -> PathRef {
        self.fs.lock().cwd.clone()
    }

    /// Record what execve (or the boot) started: executable and argv.
    pub fn set_exec(&self, exe: PathRef, argv: &[Vec<u8>]) {
        *self.exe.lock() = Some(exe);
        let mut c = Vec::new();
        for a in argv {
            c.extend_from_slice(a);
            c.push(0);
        }
        *self.cmdline.lock() = c;
    }

    pub fn set_comm(&self, name: &[u8]) {
        let mut c = self.comm.lock();
        *c = [0; 16];
        let n = name.len().min(15);
        c[..n].copy_from_slice(&name[..n]);
    }
}

/// Process tree entry.
pub struct ProcNode {
    pub proc: Arc<Process>,
    pub parent: u32,
    pub children: Vec<u32>,
    /// Live threads (tids).
    pub threads: Vec<u32>,
    pub pgid: u32,
    pub sid: u32,
    /// Wait status once every thread has exited.
    pub zombie: Option<i32>,
    /// Status set by exit_group or a fatal signal.
    pub group_exit: Option<i32>,
    /// Status of the leader thread's own exit().
    pub leader_status: i32,
}

pub struct Table {
    pub procs: BTreeMap<u32, ProcNode>,
    pub tasks: BTreeMap<u32, Arc<Task>>,
    next_pid: u32,
}

impl Table {
    pub fn alloc_pid(&mut self) -> u32 {
        loop {
            let pid = self.next_pid;
            self.next_pid = if pid >= 4_194_303 { 2 } else { pid + 1 };
            if !self.procs.contains_key(&pid) && !self.tasks.contains_key(&pid) {
                return pid;
            }
        }
    }

    pub fn node(&mut self, pid: u32) -> &mut ProcNode {
        self.procs.get_mut(&pid).expect("process not in table")
    }
}

pub static TABLE: Mutex<Table> = Mutex::new(Table {
    procs: BTreeMap::new(),
    tasks: BTreeMap::new(),
    next_pid: 1,
});

/// The running task.
pub fn current() -> Arc<Task> {
    sched::current()
}

/// Start `/init` (or `path`) as pid 1 with fds 0-2 on the console.
pub fn start_init(path: &[u8], args: &[&[u8]]) {
    let mut argv: Vec<Vec<u8>> = Vec::new();
    argv.push(path.to_vec());
    argv.extend(args.iter().map(|a| a.to_vec()));
    let envp: Vec<Vec<u8>> = [&b"HOME=/"[..], b"TERM=linux", b"PATH=/tests:/bin"]
        .iter()
        .map(|e| e.to_vec())
        .collect();
    let root = crate::fs::vfs::root();
    let (image, exe) = match lifecycle::load_executable(&root, path, &argv, &envp) {
        Ok(i) => i,
        Err(e) => {
            log::error!(
                "cannot run init {}: {:?}",
                core::str::from_utf8(path).unwrap_or("?"),
                e
            );
            return;
        }
    };
    let files = FdTable::new();
    for fd in 0..3 {
        files.install(fd, crate::fs::console(), false).unwrap();
    }
    let mut table = TABLE.lock();
    let pid = table.alloc_pid();
    let fs = FsContext { cwd: root, umask: 0o022 };
    let proc = Process::new(pid, files, fs, ProcessSignals::default(), 0);
    proc.set_comm(lifecycle::basename(path));
    proc.set_exec(exe, &argv);
    let frame = TrapFrame::new_user(image.entry, image.sp);
    let task = Task::new(pid, proc.clone(), image.mm, &frame).expect("no memory for init");
    table.tasks.insert(pid, task.clone());
    table.procs.insert(
        pid,
        ProcNode {
            proc,
            parent: 0,
            children: Vec::new(),
            threads: alloc::vec![pid],
            pgid: pid,
            sid: pid,
            zombie: None,
            group_exit: None,
            leader_status: 0,
        },
    );
    drop(table);
    log::info!(
        "starting init (pid {pid}): {}",
        core::str::from_utf8(path).unwrap_or("?")
    );
    sched::spawn(task);
}
