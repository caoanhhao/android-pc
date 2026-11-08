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
    /// User and group ids. Per thread, like Linux: bionic's set*id change
    /// only the calling thread (ueventd's threads each switch their egid
    /// around a mknod).
    pub creds: Mutex<Creds>,
}

unsafe impl Send for Task {}
unsafe impl Sync for Task {}

impl Task {
    /// A task that starts in user mode with `frame`.
    pub fn new(tid: u32, process: Arc<Process>, mm: Arc<AddressSpace>, frame: &TrapFrame) -> Option<Arc<Task>> {
        let kstack = KernelStack::new()?;
        let rsp = unsafe { entry::prepare_new_stack(kstack.top(), frame) };
        mm.users.fetch_add(1, Ordering::SeqCst);
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
            creds: Mutex::new(Creds::default()),
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
        mm.users.fetch_add(1, Ordering::SeqCst);
        x86_64::instructions::interrupts::without_interrupts(|| {
            mm.active_cpus.fetch_or(1 << me, Ordering::SeqCst);
            mm.activate();
            let old = unsafe { core::mem::replace(&mut *self.mm.get(), mm) };
            old.active_cpus.fetch_and(!(1 << me), Ordering::SeqCst);
            old.users.fetch_sub(1, Ordering::SeqCst);
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
    /// The dynamic linker and where it is mapped (for maps).
    pub interp: Mutex<Option<(PathRef, (u64, u64))>>,
    /// argv joined by NULs (/proc/<pid>/cmdline).
    pub cmdline: Mutex<Vec<u8>>,
    /// Signal sent to the parent on exit (SIGCHLD for fork; 0 for none).
    pub exit_signal: u32,
    /// Set when a vfork child execs or exits (releases the parent).
    pub vfork_done: AtomicBool,
    pub comm: Mutex<[u8; 16]>,
    /// Woken when a signal is queued (signalfd readers and pollers).
    pub signalfd_wq: Arc<WaitQueue>,
    /// Resource limits, nice value, personality, ... (inherited by fork).
    pub attrs: Mutex<Attrs>,
    /// SELinux contexts (/proc/<pid>/attr/*).
    pub sec: Mutex<crate::security::SecAttrs>,
    /// cgroup membership per hierarchy (`fs::cgroupfs`).
    pub cgroups: Mutex<crate::fs::cgroupfs::Membership>,
    /// Mount namespace (None: the initial one).
    pub mnt_ns: Mutex<Option<Arc<crate::fs::vfs::MntNs>>>,
}

/// Process credentials: the real, effective, saved and file-system user
/// and group ids and the supplementary groups. They are reported
/// (get*id, /proc, SO_PEERCRED, SCM_CREDENTIALS, binder's sender_euid,
/// owners of new files) and changed with Linux's rules, but not used for
/// permission checks; capabilities are not modelled (capget reports all).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Creds {
    pub uid: u32,
    pub euid: u32,
    pub suid: u32,
    pub fsuid: u32,
    pub gid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub fsgid: u32,
    pub groups: Vec<u32>,
}

pub const S_ISUID: u32 = 0o4000;
pub const S_ISGID: u32 = 0o2000;

impl Creds {
    /// Privileged for id changes (CAP_SETUID/CAP_SETGID): euid 0.
    pub fn privileged(&self) -> bool {
        self.euid == 0
    }

    /// The credentials after executing a file with `mode`, owned by
    /// `uid`:`gid` (set-user-ID / set-group-ID bits), and whether the exec
    /// changed them (AT_SECURE).
    pub fn after_exec(&self, mode: u32, uid: u32, gid: u32) -> (Creds, bool) {
        let mut c = self.clone();
        if mode & S_ISUID != 0 {
            c.euid = uid;
        }
        if mode & S_ISGID != 0 && mode & 0o010 != 0 {
            c.egid = gid;
        }
        c.suid = c.euid;
        c.fsuid = c.euid;
        c.sgid = c.egid;
        c.fsgid = c.egid;
        let secure = c.euid != c.uid || c.egid != c.gid;
        (c, secure)
    }
}

/// The calling task's credentials (root in kernel context).
pub fn current_creds() -> Creds {
    sched::try_current().map(|t| t.creds.lock().clone()).unwrap_or_default()
}

/// The credentials of process `pid` (its main thread, or any thread left).
pub fn process_creds(pid: u32) -> Option<Creds> {
    let t = TABLE.lock();
    let task = t.tasks.get(&pid).or_else(|| t.procs.get(&pid)?.threads.iter().find_map(|tid| t.tasks.get(tid)))?;
    Some(task.creds.lock().clone())
}

pub const RLIM_NLIMITS: usize = 16;
pub const RLIM_INFINITY: u64 = u64::MAX;

/// Per-process settings that only matter to the syscalls reporting them.
#[derive(Clone)]
pub struct Attrs {
    /// (soft, hard) per RLIMIT_*.
    pub rlimits: [(u64, u64); RLIM_NLIMITS],
    pub nice: i32,
    pub personality: u32,
    /// sched_setscheduler policy and priority (not acted on).
    pub policy: u32,
    pub rt_priority: u32,
    pub dumpable: bool,
    pub no_new_privs: bool,
    pub keepcaps: bool,
    pub securebits: u32,
    pub pdeathsig: i32,
    pub timerslack: u64,
    pub subreaper: bool,
    pub ioprio: u32,
    pub oom_score_adj: i32,
}

impl Default for Attrs {
    fn default() -> Self {
        const INF: u64 = RLIM_INFINITY;
        let mut rlimits = [(INF, INF); RLIM_NLIMITS];
        rlimits[3] = (8 << 20, INF); // STACK
        rlimits[4] = (0, INF); // CORE
        rlimits[6] = (4096, 4096); // NPROC
        rlimits[7] = (crate::fs::file::DEFAULT_NOFILE, crate::fs::file::DEFAULT_NOFILE); // NOFILE
        rlimits[8] = (8 << 20, 8 << 20); // MEMLOCK
        rlimits[11] = (4096, 4096); // SIGPENDING
        rlimits[12] = (819200, 819200); // MSGQUEUE
        rlimits[13] = (0, 0); // NICE
        rlimits[14] = (0, 0); // RTPRIO
        Attrs { rlimits, nice: 0, personality: 0, policy: 0, rt_priority: 0, dumpable: true,
            no_new_privs: false,
            keepcaps: false,
            securebits: 0,
            pdeathsig: 0,
            timerslack: 50_000,
            subreaper: false,
            ioprio: 4,
            oom_score_adj: 0,
        }
    }
}

impl Process {
    pub fn new(pid: u32, files: Arc<FdTable>, fs: FsContext, sig: ProcessSignals, exit_signal: u32) -> Arc<Process> {
        Arc::new(Process {
            pid,
            sig: Mutex::new(sig),
            files: Mutex::new(files),
            fs: Mutex::new(fs),
            exe: Mutex::new(None),
            interp: Mutex::new(None),
            cmdline: Mutex::new(Vec::new()),
            exit_signal,
            vfork_done: AtomicBool::new(false),
            comm: Mutex::new([0; 16]),
            signalfd_wq: WaitQueue::new(),
            attrs: Mutex::new(Attrs::default()),
            sec: Mutex::new(crate::security::SecAttrs::default()),
            cgroups: Mutex::new(Default::default()),
            mnt_ns: Mutex::new(None),
        })
    }

    pub fn files(&self) -> Arc<FdTable> {
        self.files.lock().clone()
    }

    pub fn cwd(&self) -> PathRef {
        self.fs.lock().cwd.clone()
    }

    /// Record what execve (or the boot) started: executable and argv.
    pub fn set_exec(&self, exe: PathRef, argv: &[Vec<u8>], interp: Option<(PathRef, (u64, u64))>) {
        *self.exe.lock() = Some(exe);
        *self.interp.lock() = interp;
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
    let (image, exe, interp, _) = match lifecycle::load_executable(&root, path, &argv, &envp) {
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
    proc.set_exec(exe, &argv, interp.zip(image.interp_range));
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
