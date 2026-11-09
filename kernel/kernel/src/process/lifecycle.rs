//! Process and thread life cycle: clone/fork/vfork, execve, exit,
//! exit_group, wait4.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::signal::{self, bit};
use super::{ProcNode, Process, TABLE, Task, current, elf, futex};
use crate::arch::{FpuState, TrapFrame};
use crate::errno::*;
use crate::fs::vfs::{self, PathRef};
use crate::sched::{self, Wake, WaitMode};

pub const CSIGNAL: u64 = 0xff;
pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_PIDFD: u64 = 0x1000;
pub const CLONE_VFORK: u64 = 0x4000;
pub const CLONE_NEWNS: u64 = 0x2_0000;
pub const CLONE_PARENT: u64 = 0x8000;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_SETTLS: u64 = 0x80000;
pub const CLONE_PARENT_SETTID: u64 = 0x100000;
pub const CLONE_CHILD_CLEARTID: u64 = 0x200000;
pub const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

pub const WNOHANG: u32 = 1;

const WAIT_CHILD: u64 = 0x5741_4954_0000_0000;
const WAIT_VFORK: u64 = 0x5646_4f52_4b00_0000;
const WAIT_EXEC: u64 = 0x4558_4543_0000_0000;

pub fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&c| c == b'/').next().unwrap_or(path)
}

/// Look up an executable (relative to `cwd`) and load it into a new
/// address space. Returns the image, the resolved path, the PT_INTERP
/// program's path and the caller's credentials after the exec (set-user-ID
/// and set-group-ID bits). Handles PT_INTERP (the dynamic linker) and `#!`
/// scripts.
pub fn load_executable(
    cwd: &PathRef,
    path: &[u8],
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
) -> KResult<(elf::Image, PathRef, Option<PathRef>, super::Creds)> {
    let mut path = path.to_vec();
    let mut argv = argv.to_vec();
    // Linux allows a few levels of interpreter scripts (BINPRM_MAX_RECURSION).
    for _ in 0..5 {
        let (p, data) = read_executable(cwd, &path)?;
        if data.starts_with(b"#!") {
            // "#!interp [one-arg]": argv becomes interp [arg] path argv[1..].
            let line = data[2..].split(|&c| c == b'\n').next().unwrap_or(&[]);
            let line = line.trim_ascii();
            let (interp, arg) = match line.iter().position(|c| c.is_ascii_whitespace()) {
                Some(i) => (&line[..i], line[i..].trim_ascii()),
                None => (line, &[][..]),
            };
            if interp.is_empty() {
                return Err(ENOEXEC);
            }
            let mut nargv = alloc::vec![interp.to_vec()];
            if !arg.is_empty() {
                nargv.push(arg.to_vec());
            }
            nargv.push(path.clone());
            nargv.extend(argv.iter().skip(1).cloned());
            argv = nargv;
            path = interp.to_vec();
            continue;
        }
        let (ipath, idata) = match elf::interp_path(&data)? {
            Some(ip) => {
                let (ipath, idata) = read_executable(cwd, &ip).map_err(|e| if e == ENOENT { ENOENT } else { ELIBBAD })?;
                (Some(ipath), Some(idata))
            }
            None => (None, None),
        };
        let a = p.inode().meta().attr();
        let (creds, secure) = super::current_creds().after_exec(a.mode, a.uid, a.gid);
        let image = elf::load(&data, idata.as_deref(), &path, &argv, envp, &creds, secure)?;
        return Ok((image, p, ipath, creds));
    }
    Err(ELOOP)
}

fn read_executable(cwd: &PathRef, path: &[u8]) -> KResult<(PathRef, Vec<u8>)> {
    let p = vfs::lookup(cwd, path, true)?;
    let meta = p.inode().meta();
    if meta.file_type() != vfs::S_IFREG || meta.mode() & 0o111 == 0 {
        return Err(EACCES);
    }
    let data = crate::fs::read_all(p.inode())?;
    Ok((p, data))
}

// ---------------------------------------------------------------------------
// clone

pub fn sys_clone(frame: &TrapFrame, flags: u64, newsp: u64, ptid: u64, ctid: u64, tls: u64) -> KResult<u64> {
    if flags & CLONE_THREAD != 0 && flags & CLONE_SIGHAND == 0 {
        return Err(EINVAL);
    }
    if flags & CLONE_SIGHAND != 0 && flags & CLONE_VM == 0 {
        return Err(EINVAL);
    }
    // CLONE_PIDFD returns the pidfd through the parent_tid pointer.
    if flags & CLONE_PIDFD != 0 && flags & (CLONE_THREAD | CLONE_PARENT_SETTID) != 0 {
        return Err(EINVAL);
    }
    let cur = current();
    let mm = if flags & CLONE_VM != 0 {
        cur.mm()
    } else {
        cur.mm().fork().ok_or(ENOMEM)?
    };
    // CLONE_NEWNS: the child gets a copy of the mount namespace (and its
    // working directory moves into the copy).
    let new_ns = (flags & CLONE_NEWNS != 0 && flags & CLONE_THREAD == 0).then(|| {
        let (ns, map) = vfs::copy_ns(&vfs::current_ns());
        (ns, map(&cur.process.cwd()))
    });
    let mut child_frame = *frame;
    child_frame.rax = 0;
    child_frame.error_code = u64::MAX;
    if newsp != 0 {
        child_frame.rsp = newsp;
    }

    let mut table = TABLE.lock();
    let tid = table.alloc_pid();
    let process = if flags & CLONE_THREAD != 0 {
        if table.node(cur.pid()).group_exit.is_some() {
            return Err(EAGAIN);
        }
        cur.process.clone()
    } else {
        let files = if flags & CLONE_FILES != 0 {
            cur.process.files()
        } else {
            cur.process.files().duplicate()
        };
        let fs = cur.process.fs.lock().clone();
        let p = Process::new(tid, files, fs, cur.process.sig.lock().fork(), (flags & CSIGNAL) as u32);
        *p.comm.lock() = *cur.comm.lock();
        *p.exe.lock() = cur.process.exe.lock().clone();
        *p.interp.lock() = cur.process.interp.lock().clone();
        *p.cmdline.lock() = cur.process.cmdline.lock().clone();
        *p.attrs.lock() = cur.process.attrs.lock().clone();
        *p.sec.lock() = cur.process.sec.lock().clone();
        *p.cgroups.lock() = cur.process.cgroups.lock().clone();
        *p.mnt_ns.lock() = cur.process.mnt_ns.lock().clone();
        if let Some((ns, cwd)) = new_ns {
            *p.mnt_ns.lock() = Some(ns);
            p.fs.lock().cwd = cwd;
        }
        p
    };
    let task = Task::new(tid, process.clone(), mm.clone(), &child_frame).ok_or(ENOMEM)?;
    *task.creds.lock() = cur.creds.lock().clone();
    *task.comm.lock() = *cur.comm.lock();
    let mut fpu = FpuState::initial();
    fpu.save();
    task.set_fpu(&fpu);
    task.fs_base.store(
        if flags & CLONE_SETTLS != 0 {
            tls
        } else {
            x86_64::registers::model_specific::FsBase::read().as_u64()
        },
        Ordering::Relaxed,
    );
    task.gs_base.store(
        x86_64::registers::model_specific::KernelGsBase::read().as_u64(),
        Ordering::Relaxed,
    );
    {
        let cs = cur.sig.lock();
        let mut ts = task.sig.lock();
        ts.mask = cs.mask;
        if flags & CLONE_VM == 0 || flags & CLONE_VFORK != 0 {
            ts.altstack = cs.altstack;
        }
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        task.clear_child_tid.store(ctid, Ordering::Relaxed);
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        mm.copy_out(ctid, &tid.to_le_bytes())?;
    }
    if flags & CLONE_PARENT_SETTID != 0 {
        cur.mm().copy_out(ptid, &tid.to_le_bytes())?;
    }

    table.tasks.insert(tid, task.clone());
    if flags & CLONE_THREAD != 0 {
        table.node(cur.pid()).threads.push(tid);
    } else {
        let me = table.node(cur.pid());
        let parent = if flags & CLONE_PARENT != 0 { me.parent } else { cur.pid() };
        let (pgid, sid) = (me.pgid, me.sid);
        table.procs.insert(
            tid,
            ProcNode {
                proc: process.clone(),
                parent,
                children: Vec::new(),
                threads: alloc::vec![tid],
                pgid,
                sid,
                zombie: None,
                group_exit: None,
                leader_status: 0,
            },
        );
        if let Some(p) = table.procs.get_mut(&parent) {
            p.children.push(tid);
        }
    }
    drop(table);
    if flags & CLONE_PIDFD != 0 {
        match super::pidfd::install_for_child(tid).and_then(|fd| cur.mm().copy_out(ptid, &fd.to_le_bytes())) {
            Ok(()) => {}
            Err(e) => {
                // Too late to undo the clone cleanly: let the child run,
                // report the failure like Linux would before creating it.
                log::warn!("clone: CLONE_PIDFD for {tid} failed: {e:?}");
            }
        }
    }
    sched::spawn(task);

    if flags & CLONE_VFORK != 0 {
        sched::wait_until((WAIT_VFORK, tid as u64), None, WaitMode::Killable, || {
            process.vfork_done.load(Ordering::SeqCst)
        });
    }
    Ok(tid as u64)
}

fn release_vfork(p: &Process) {
    if !p.vfork_done.swap(true, Ordering::SeqCst) {
        sched::wake_all((WAIT_VFORK, p.pid as u64));
    }
}

// ---------------------------------------------------------------------------
// exit

/// Make every thread of process `pid` exit with `status` (SIGKILL, or the
/// group exit status if one is already set).
pub fn kill_group(pid: u32, sig: i32) {
    let threads: Vec<Arc<Task>> = {
        let mut t = TABLE.lock();
        let Some(node) = t.procs.get_mut(&pid) else {
            return;
        };
        if node.group_exit.is_none() {
            node.group_exit = Some(sig & 0x7f);
        }
        let tids = node.threads.clone();
        tids.iter().filter_map(|tid| t.tasks.get(tid).cloned()).collect()
    };
    for th in threads {
        th.sig.lock().pending |= bit(signal::SIGKILL);
        sched::wake_task(&th);
    }
}

// The exit functions never return, so the frames below them are never
// unwound: callers must drop every `Arc` (task, address space, file) they
// hold before calling one, or the task and everything it references leak.

/// Terminate the calling process because of signal `sig`.
pub fn exit_group_signal(sig: i32) -> ! {
    let pid = current().pid();
    kill_group(pid, sig);
    exit_thread(sig & 0x7f)
}

pub fn sys_exit_group(code: i32) -> ! {
    let pid = current().pid();
    let status = (code & 0xff) << 8;
    {
        let mut table = TABLE.lock();
        let node = table.node(pid);
        if node.group_exit.is_none() {
            node.group_exit = Some(status);
        }
    }
    kill_group(pid, signal::SIGKILL as i32);
    exit_thread(status)
}

pub fn sys_exit(code: i32) -> ! {
    exit_thread((code & 0xff) << 8)
}

/// End the calling thread; `status` is its wait status. The process ends
/// with its last thread.
pub fn exit_thread(status: i32) -> ! {
    let t = current();
    let pid = t.pid();
    let ctid = t.clear_child_tid.swap(0, Ordering::Relaxed);
    if ctid != 0 {
        let mm = t.mm();
        if mm.copy_out(ctid, &0u32.to_le_bytes()).is_ok() {
            futex::wake(&mm, ctid, 1);
        }
    }

    // The last user of the address space drops its mappings below.
    let mm_last = t.mm().users.fetch_sub(1, Ordering::SeqCst) == 1;
    let mut table = TABLE.lock();
    table.tasks.remove(&t.tid);
    let node = table.node(pid);
    node.threads.retain(|&x| x != t.tid);
    if t.tid == pid {
        node.leader_status = status;
    }
    let last = node.threads.is_empty();
    let mut notify_parent = None;
    let mut wake_init = false;
    if last {
        let final_status = node.group_exit.unwrap_or(node.leader_status);
        let parent = node.parent;
        let children = core::mem::take(&mut node.children);
        // Orphans go to init.
        for c in &children {
            if let Some(cn) = table.procs.get_mut(c) {
                cn.parent = 1;
                wake_init |= cn.zombie.is_some();
            }
        }
        if pid != 1
            && let Some(init) = table.procs.get_mut(&1)
        {
            init.children.extend(children);
        }
        let auto_reap = table
            .procs
            .get(&parent)
            .is_some_and(|p| signal::ignores_children(&p.proc));
        if auto_reap || parent == 0 {
            table.procs.remove(&pid);
            if let Some(p) = table.procs.get_mut(&parent) {
                p.children.retain(|&c| c != pid);
            }
        } else {
            table.node(pid).zombie = Some(final_status);
        }
        notify_parent = Some((parent, final_status, auto_reap));
    }
    drop(table);
    if !last {
        sched::wake_all((WAIT_EXEC, pid as u64));
    } else {
        super::pidfd::notify_exit();
        super::ptimer::exit(pid);
        // The process left its cgroups (cgroup.events "populated").
        crate::fs::cgroupfs::notify_events();
    }

    if let Some((parent, final_status, auto_reap)) = notify_parent {
        t.process.files().close_all();
        // Like Linux's exit_mm, drop the mappings (and the files they map:
        // a binder mapping keeps its binder process alive) before the
        // parent hears of the exit (it may check for them, e.g.
        // F_SEAL_WRITE). Not when another task (a vfork parent) shares the
        // address space.
        if mm_last {
            t.mm().unmap_all();
        }
        release_vfork(&t.process);
        if pid == 1 {
            log::info!("init exited with status {:#x}; powering off", final_status);
            crate::arch::power_off();
        }
        if !auto_reap {
            let sig = t.process.exit_signal;
            if sig != 0 && signal::valid(sig) {
                let _ = signal::send_process(parent, sig, signal::child_info(pid, final_status));
            }
            sched::wake_all((WAIT_CHILD, parent as u64));
        }
        if wake_init {
            sched::wake_all((WAIT_CHILD, 1));
        }
    }
    // Leave the CPU for good; the scheduler drops our last reference.
    t.state.store(sched::DEAD, Ordering::SeqCst);
    drop(t);
    sched::schedule();
    unreachable!("dead task scheduled");
}

// ---------------------------------------------------------------------------
// wait4

/// Which children a wait call is about (wait4's pid argument).
#[derive(Clone, Copy)]
enum WaitFor {
    Pid(u32),
    Any,
    OwnGroup,
    Group(u32),
}

const WNOWAIT: u32 = 0x0100_0000;

/// Wait for a matching child to become a zombie and reap it (unless
/// WNOWAIT). Ok(None) for WNOHANG without one.
fn wait_child(which: WaitFor, options: u32) -> KResult<Option<(u32, i32)>> {
    let me = current().pid();
    let mut result: KResult<Option<(u32, i32)>> = Err(ECHILD);
    let w = sched::wait_until((WAIT_CHILD, me as u64), None, WaitMode::Interruptible, || {
        let mut table = TABLE.lock();
        let my_pgid = table.node(me).pgid;
        let children = table.node(me).children.clone();
        let matching: Vec<u32> = children
            .into_iter()
            .filter(|c| {
                let n = &table.procs[c];
                match which {
                    WaitFor::Pid(p) => *c == p,
                    WaitFor::Any => true,
                    WaitFor::OwnGroup => n.pgid == my_pgid,
                    WaitFor::Group(g) => n.pgid == g,
                }
            })
            .collect();
        if matching.is_empty() {
            result = Err(ECHILD);
            return true;
        }
        if let Some(&z) = matching.iter().find(|c| table.procs[c].zombie.is_some()) {
            let status = table.procs[&z].zombie.unwrap();
            if options & WNOWAIT == 0 {
                table.procs.remove(&z);
                table.node(me).children.retain(|&c| c != z);
            }
            result = Ok(Some((z, status)));
            return true;
        }
        if options & WNOHANG != 0 {
            result = Ok(None);
            return true;
        }
        false
    });
    if w != Wake::Ready {
        return Err(ERESTARTSYS);
    }
    result
}

pub fn sys_wait4(pid: i32, status_ptr: u64, options: u32, rusage: u64) -> KResult<u64> {
    if options & !(WNOHANG | 2 | 8 | 0x0100_0000 | 0x2000_0000 | 0x4000_0000 | 0x8000_0000) != 0 {
        return Err(EINVAL);
    }
    let which = match pid {
        p if p > 0 => WaitFor::Pid(p as u32),
        -1 => WaitFor::Any,
        0 => WaitFor::OwnGroup,
        g => WaitFor::Group((-g) as u32),
    };
    // WNOWAIT is a waitid flag only.
    let r = wait_child(which, options & !WNOWAIT)?;
    let t = current();
    let mm = t.mm();
    if let Some((_, status)) = r
        && status_ptr != 0
    {
        mm.copy_out(status_ptr, &status.to_le_bytes())?;
    }
    if rusage != 0 {
        mm.copy_out(rusage, &[0u8; 144])?;
    }
    Ok(r.map_or(0, |(c, _)| c as u64))
}

const P_ALL: u32 = 0;
const P_PID: u32 = 1;
const P_PGID: u32 = 2;
const WEXITED: u32 = 4;
const WSTOPPED: u32 = 2;
const WCONTINUED: u32 = 8;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;
const CLD_DUMPED: i32 = 3;

/// waitid: like wait4 but reports a siginfo_t (and can peek with
/// WNOWAIT). Stopped/continued children never happen (no job control).
pub fn sys_waitid(idtype: u32, id: u32, infop: u64, options: u32, rusage: u64) -> KResult<u64> {
    if options & !(WNOHANG | WEXITED | WSTOPPED | WCONTINUED | WNOWAIT | 0x2000_0000 | 0x4000_0000 | 0x8000_0000) != 0
        || options & (WEXITED | WSTOPPED | WCONTINUED) == 0
    {
        return Err(EINVAL);
    }
    let which = match idtype {
        P_ALL => WaitFor::Any,
        P_PID if id > 0 => WaitFor::Pid(id),
        P_PGID if id == 0 => WaitFor::OwnGroup,
        P_PGID => WaitFor::Group(id),
        _ => return Err(EINVAL),
    };
    let r = if options & WEXITED != 0 {
        wait_child(which, options)?
    } else if options & WNOHANG != 0 {
        None
    } else {
        // Only stops/continues wanted: they never come.
        wait_child(which, options | WNOHANG)?;
        return Err(ECHILD);
    };
    let mm = current().mm();
    if infop != 0 {
        let mut b = [0u8; 128];
        if let Some((pid, status)) = r {
            let (code, value) = if status & 0x7f == 0 {
                (CLD_EXITED, (status >> 8) & 0xff)
            } else if status & 0x80 != 0 {
                (CLD_DUMPED, status & 0x7f)
            } else {
                (CLD_KILLED, status & 0x7f)
            };
            b[0..4].copy_from_slice(&(signal::SIGCHLD as i32).to_le_bytes());
            b[8..12].copy_from_slice(&code.to_le_bytes());
            b[16..20].copy_from_slice(&(pid as i32).to_le_bytes());
            b[24..28].copy_from_slice(&value.to_le_bytes());
        }
        mm.copy_out(infop, &b)?;
    }
    if rusage != 0 {
        mm.copy_out(rusage, &[0u8; 144])?;
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// execve

fn read_string_array(mm: &crate::mm::user::AddressSpace, mut addr: u64, total: &mut usize) -> KResult<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    if addr == 0 {
        return Ok(out);
    }
    loop {
        let mut b = [0u8; 8];
        mm.copy_in(addr, &mut b)?;
        let p = u64::from_le_bytes(b);
        if p == 0 {
            return Ok(out);
        }
        let s = mm.read_cstr(p, elf::MAX_ARG_BYTES)?;
        *total += s.len() + 1 + 8;
        if *total > elf::MAX_ARG_BYTES {
            return Err(E2BIG);
        }
        out.push(s);
        addr += 8;
    }
}

pub fn sys_execve(frame: &mut TrapFrame, path: u64, argv: u64, envp: u64) -> KResult<u64> {
    let t = current();
    let mm = t.mm();
    let path = mm.read_cstr(path, 4096)?;
    let mut total = 0;
    let argv = read_string_array(&mm, argv, &mut total)?;
    let envp = read_string_array(&mm, envp, &mut total)?;
    drop(mm);
    let (image, exe, interp, creds) = load_executable(&t.process.cwd(), &path, &argv, &envp)?;

    // Point of no return. Other threads of the group go away first.
    let others: Vec<Arc<Task>> = {
        let table = TABLE.lock();
        let node = &table.procs[&t.pid()];
        node.threads
            .iter()
            .filter(|&&x| x != t.tid)
            .filter_map(|x| table.tasks.get(x).cloned())
            .collect()
    };
    if !others.is_empty() {
        for o in &others {
            o.sig.lock().pending |= bit(signal::SIGKILL);
            sched::wake_task(o);
        }
        let me = t.pid();
        let w = sched::wait_until((WAIT_EXEC, me as u64), None, WaitMode::Killable, || {
            TABLE.lock().procs[&me].threads.len() == 1
        });
        if w != Wake::Ready {
            drop((others, image, exe, interp, t));
            exit_thread(signal::SIGKILL as i32);
        }
    }

    let old = t.set_mm(image.mm);
    if old.users.load(Ordering::SeqCst) == 0 {
        // Exited threads may still hold the old space for a moment; its
        // mapped files (binder!) must go now, as with Linux's mmput.
        old.unmap_all();
    }
    drop(old);
    release_vfork(&t.process);
    t.process.sig.lock().reset_for_exec();
    {
        let mut ts = t.sig.lock();
        ts.altstack = Default::default();
    }
    t.process.files().close_on_exec();
    t.process.set_comm(basename(&path));
    *t.comm.lock() = *t.process.comm.lock();
    let label = exe.inode().xattrs().ok().and_then(|x| {
        x.into_iter().find(|(n, _)| n == crate::security::XATTR_SELINUX).map(|(_, v)| v)
    });
    t.process.sec.lock().exec(label.as_deref());
    *t.creds.lock() = creds;
    t.process.set_exec(exe, &argv, interp.zip(image.interp_range));
    t.clear_child_tid.store(0, Ordering::Relaxed);
    t.robust_list.store(0, Ordering::Relaxed);
    t.fs_base.store(0, Ordering::Relaxed);
    t.gs_base.store(0, Ordering::Relaxed);
    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::zero());
    x86_64::registers::model_specific::KernelGsBase::write(x86_64::VirtAddr::zero());
    FpuState::initial().restore();
    *frame = TrapFrame::new_user(image.entry, image.sp);
    Ok(0)
}
