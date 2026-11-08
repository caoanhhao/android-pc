//! Process and thread life cycle: clone/fork/vfork, execve, exit,
//! exit_group, wait4.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::signal::{self, bit};
use super::{ProcNode, Process, TABLE, Task, current, elf, futex};
use crate::arch::{FpuState, TrapFrame};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};

pub const CSIGNAL: u64 = 0xff;
pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_PIDFD: u64 = 0x1000;
pub const CLONE_VFORK: u64 = 0x4000;
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

/// Look up an executable and load it into a new address space.
pub fn load_executable(path: &[u8], argv: &[Vec<u8>], envp: &[Vec<u8>]) -> KResult<elf::Image> {
    let node = crate::fs::lookup(path)?;
    if node.is_dir() || !node.is_reg() || node.mode & 0o111 == 0 {
        return Err(EACCES);
    }
    let data = node.data.lock();
    elf::load(data.bytes(), path, argv, envp)
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
    if flags & CLONE_PIDFD != 0 {
        return Err(EINVAL); // TODO: pidfd (B4)
    }
    let cur = current();
    let mm = if flags & CLONE_VM != 0 {
        cur.mm()
    } else {
        cur.mm().fork().ok_or(ENOMEM)?
    };
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
        let p = Process::new(tid, files, cur.process.sig.lock().fork(), (flags & CSIGNAL) as u32);
        *p.comm.lock() = *cur.process.comm.lock();
        p
    };
    let task = Task::new(tid, process.clone(), mm.clone(), &child_frame).ok_or(ENOMEM)?;
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

/// Terminate the calling process because of signal `sig`.
pub fn exit_group_signal(sig: i32) -> ! {
    let t = current();
    kill_group(t.pid(), sig);
    exit_thread(sig & 0x7f)
}

pub fn sys_exit_group(code: i32) -> ! {
    let t = current();
    let status = (code & 0xff) << 8;
    {
        let mut table = TABLE.lock();
        let node = table.node(t.pid());
        if node.group_exit.is_none() {
            node.group_exit = Some(status);
        }
    }
    kill_group(t.pid(), signal::SIGKILL as i32);
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
    }

    if let Some((parent, final_status, auto_reap)) = notify_parent {
        t.process.files().close_all();
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

pub fn sys_wait4(pid: i32, status_ptr: u64, options: u32, rusage: u64) -> KResult<u64> {
    if options & !(WNOHANG | 2 | 8 | 0x0100_0000 | 0x2000_0000 | 0x4000_0000 | 0x8000_0000) != 0 {
        return Err(EINVAL);
    }
    let t = current();
    let me = t.pid();
    let mut result: KResult<(u32, i32)> = Err(ECHILD);
    let w = sched::wait_until((WAIT_CHILD, me as u64), None, WaitMode::Interruptible, || {
        let mut table = TABLE.lock();
        let my_pgid = table.node(me).pgid;
        let children = table.node(me).children.clone();
        let matching: Vec<u32> = children
            .into_iter()
            .filter(|c| {
                let n = &table.procs[c];
                match pid {
                    p if p > 0 => *c == p as u32,
                    -1 => true,
                    0 => n.pgid == my_pgid,
                    g => n.pgid == (-g) as u32,
                }
            })
            .collect();
        if matching.is_empty() {
            result = Err(ECHILD);
            return true;
        }
        if let Some(&z) = matching.iter().find(|c| table.procs[c].zombie.is_some()) {
            let status = table.procs.remove(&z).unwrap().zombie.unwrap();
            table.node(me).children.retain(|&c| c != z);
            result = Ok((z, status));
            return true;
        }
        if options & WNOHANG != 0 {
            result = Ok((0, 0));
            return true;
        }
        false
    });
    if w != Wake::Ready {
        return Err(ERESTARTSYS);
    }
    let (child, status) = result?;
    let mm = t.mm();
    if child != 0 && status_ptr != 0 {
        mm.copy_out(status_ptr, &status.to_le_bytes())?;
    }
    if rusage != 0 {
        mm.copy_out(rusage, &[0u8; 144])?;
    }
    Ok(child as u64)
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
    let image = load_executable(&path, &argv, &envp)?;

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
            exit_thread(signal::SIGKILL as i32);
        }
    }

    let old = t.set_mm(image.mm);
    drop(old);
    release_vfork(&t.process);
    t.process.sig.lock().reset_for_exec();
    {
        let mut ts = t.sig.lock();
        ts.altstack = Default::default();
    }
    t.process.files().close_on_exec();
    t.process.set_comm(basename(&path));
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
