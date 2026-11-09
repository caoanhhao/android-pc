//! pidfds: files that name a process (pidfd_open, CLONE_PIDFD). They are
//! readable (POLLIN) once the process has exited, take signals
//! (pidfd_send_signal) and identify the target of process_madvise and
//! process_mrelease (lmkd uses all of them).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use spin::Once;

use super::{Process, TABLE, current};
use crate::errno::*;
use crate::fs::file::{FileOps, O_CLOEXEC, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLIN, POLLRDNORM, WaitQueue};

pub const PIDFD_NONBLOCK: u32 = O_NONBLOCK;

pub struct PidFd {
    pub pid: u32,
    proc: Weak<Process>,
}

/// Woken whenever a process exits (pidfds re-check their target).
fn exit_queue() -> &'static Arc<WaitQueue> {
    static Q: Once<Arc<WaitQueue>> = Once::new();
    Q.call_once(WaitQueue::new)
}

/// Called when a process became a zombie or was reaped.
pub fn notify_exit() {
    exit_queue().wake(POLLIN);
}

impl PidFd {
    /// The process, while it is alive (not a zombie, pid not reused).
    pub fn live(&self) -> Option<Arc<Process>> {
        let p = self.proc.upgrade()?;
        let t = TABLE.lock();
        let n = t.procs.get(&self.pid)?;
        (Arc::ptr_eq(&n.proc, &p) && n.zombie.is_none()).then_some(p)
    }
}

impl FileOps for PidFd {
    fn poll(&self, _: &OpenFile) -> u32 {
        if self.live().is_some() { 0 } else { POLLIN | POLLRDNORM }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(exit_queue().clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[pidfd]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        let pid = if self.live().is_some() { self.pid as i64 } else { -1 };
        format!("Pid:\t{pid}\nNSpid:\t{pid}\n")
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A pidfd for the thread-group leader `pid`.
pub fn open(pid: u32, flags: u32) -> KResult<Arc<OpenFile>> {
    let proc = {
        let t = TABLE.lock();
        t.procs.get(&pid).ok_or(ESRCH)?.proc.clone()
    };
    if proc.pid != pid {
        return Err(EINVAL); // not a thread-group leader
    }
    let ops = PidFd { pid, proc: Arc::downgrade(&proc) };
    Ok(OpenFile::new(crate::fs::anon_inode(), None, Box::new(ops), O_RDWR | (flags & O_NONBLOCK)))
}

/// The pidfd behind file descriptor `fd`.
pub fn get(fd: i32) -> KResult<(Arc<OpenFile>, u32)> {
    let f = current().process.files().get(fd)?;
    let pid = f.ops.as_any().downcast_ref::<PidFd>().ok_or(EBADF)?.pid;
    Ok((f, pid))
}

pub fn sys_pidfd_open(pid: i32, flags: u32) -> KResult<u64> {
    if pid <= 0 || flags & !PIDFD_NONBLOCK != 0 {
        return Err(EINVAL);
    }
    let f = open(pid as u32, flags)?;
    Ok(current().process.files().alloc(f, true, 0)? as u64)
}

pub fn sys_pidfd_send_signal(fd: i32, sig: u32, info: u64, flags: u32) -> KResult<u64> {
    if flags != 0 {
        return Err(EINVAL);
    }
    let (f, pid) = get(fd)?;
    let pfd = f.ops.as_any().downcast_ref::<PidFd>().unwrap();
    if pfd.live().is_none() {
        return Err(ESRCH);
    }
    if sig == 0 {
        return Ok(0);
    }
    if sig as usize > super::signal::NSIG {
        return Err(EINVAL);
    }
    let me = current().pid();
    let si = if info != 0 {
        let mut b = [0u8; 16];
        current().mm().copy_in(info, &mut b)?;
        let code = i32::from_ne_bytes(b[8..12].try_into().unwrap());
        // Only signals that look like kill()/sigqueue() from user space.
        if code >= 0 && pid != me {
            return Err(EPERM);
        }
        super::signal::SigInfo::user(sig, code, me)
    } else {
        super::signal::SigInfo::user(sig, super::signal::SI_USER, me)
    };
    super::signal::send_process(pid, sig, si)?;
    Ok(0)
}

/// pidfd_getfd(2): a duplicate of another process's file descriptor.
pub fn sys_pidfd_getfd(fd: i32, target: i32, flags: u32) -> KResult<u64> {
    if flags != 0 {
        return Err(EINVAL);
    }
    let (f, _) = get(fd)?;
    let p = f.ops.as_any().downcast_ref::<PidFd>().unwrap().live().ok_or(ESRCH)?;
    let file = p.files().get(target)?;
    Ok(current().process.files().alloc(file, true, 0)? as u64)
}

/// process_mrelease(2): the memory of a dying process is freed when it
/// exits anyway; only the arguments are checked.
pub fn sys_process_mrelease(fd: i32, flags: u32) -> KResult<u64> {
    if flags != 0 {
        return Err(EINVAL);
    }
    get(fd)?;
    Ok(0)
}

/// process_madvise(2): MADV_COLD / PAGEOUT / WILLNEED / COLLAPSE are hints
/// (nothing is swapped); returns the bytes "advised".
pub fn sys_process_madvise(fd: i32, iov: u64, vlen: u64, advice: u32, flags: u32) -> KResult<u64> {
    if flags != 0 || vlen > 1024 {
        return Err(EINVAL);
    }
    if !matches!(advice, 3 | 20 | 21 | 25) {
        return Err(EINVAL);
    }
    let (f, _) = get(fd)?;
    f.ops.as_any().downcast_ref::<PidFd>().unwrap().live().ok_or(ESRCH)?;
    let mut total = 0u64;
    let mm = current().mm();
    for i in 0..vlen {
        let mut b = [0u8; 16];
        mm.copy_in(iov + i * 16, &mut b)?;
        total += u64::from_ne_bytes(b[8..16].try_into().unwrap());
    }
    Ok(total)
}

/// CLONE_PIDFD: a pidfd for the new child, close-on-exec.
pub fn install_for_child(pid: u32) -> KResult<i32> {
    let f = open(pid, 0)?;
    current().process.files().alloc(f, O_CLOEXEC != 0, 0).map(|fd| fd as i32)
}
