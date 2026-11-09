//! Process attribute syscalls: resource limits and usage, scheduling
//! policy and affinity, priorities, credentials, capabilities,
//! personality, sigqueueinfo.
//!
//! There is no credentials model yet: every process is root (uid/gid 0,
//! all capabilities), so the set*id calls accept any id and change nothing.
//! The scheduler is a single round-robin class: policies, priorities, nice
//! values and affinity masks are stored or validated and reported back,
//! but do not change scheduling.

use alloc::sync::Arc;

use crate::errno::*;
use crate::process::signal::{self, SigInfo};
use crate::process::{RLIM_NLIMITS, TABLE, current};

/// The process `pid` (0 = the caller) or ESRCH.
fn process(pid: i32) -> KResult<Arc<crate::process::Process>> {
    if pid < 0 {
        return Err(EINVAL);
    }
    let t = current();
    if pid == 0 || pid as u32 == t.pid() {
        return Ok(t.process.clone());
    }
    // A tid names its thread group.
    TABLE.lock().tasks.get(&(pid as u32)).map(|t| t.process.clone()).ok_or(ESRCH)
}

fn put_u64s(addr: u64, words: &[u64]) -> KResult<()> {
    let b: alloc::vec::Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    current().mm().copy_out(addr, &b)
}

// ---------------------------------------------------------------------------
// Resource limits and usage

pub fn sys_prlimit64(pid: i32, resource: u32, new: u64, old: u64) -> KResult<u64> {
    let r = resource as usize;
    if r >= RLIM_NLIMITS {
        return Err(EINVAL);
    }
    let p = process(pid)?;
    let mm = current().mm();
    let newlim = if new != 0 {
        let mut b = [0u8; 16];
        mm.copy_in(new, &mut b)?;
        let soft = u64::from_le_bytes(b[..8].try_into().unwrap());
        let hard = u64::from_le_bytes(b[8..].try_into().unwrap());
        if soft > hard {
            return Err(EINVAL);
        }
        // RLIMIT_NOFILE cannot exceed the fd table size (Linux: nr_open).
        if r == 7 && hard > crate::fs::file::MAX_FDS as u64 {
            return Err(EPERM);
        }
        Some((soft, hard))
    } else {
        None
    };
    let mut a = p.attrs.lock();
    let prev = a.rlimits[r];
    if let Some(l) = newlim {
        a.rlimits[r] = l;
    }
    drop(a);
    if old != 0 {
        put_u64s(old, &[prev.0, prev.1])?;
    }
    Ok(0)
}

pub fn sys_getrlimit(resource: u32, old: u64) -> KResult<u64> {
    sys_prlimit64(0, resource, 0, old)
}

pub fn sys_setrlimit(resource: u32, new: u64) -> KResult<u64> {
    sys_prlimit64(0, resource, new, 0)
}

const RUSAGE_SELF: i32 = 0;
const RUSAGE_CHILDREN: i32 = -1;
const RUSAGE_THREAD: i32 = 1;

/// `getrusage`: no CPU time accounting yet, so every field is zero.
pub fn sys_getrusage(who: i32, usage: u64) -> KResult<u64> {
    if !matches!(who, RUSAGE_SELF | RUSAGE_CHILDREN | RUSAGE_THREAD) {
        return Err(EINVAL);
    }
    current().mm().copy_out(usage, &[0u8; 144])?;
    Ok(0)
}

/// `times`: zero CPU times; returns clock ticks since boot.
pub fn sys_times(buf: u64) -> KResult<u64> {
    if buf != 0 {
        put_u64s(buf, &[0; 4])?;
    }
    Ok(crate::time::uptime_ms() * crate::time::HZ / 1000)
}

pub fn sys_sysinfo(info: u64) -> KResult<u64> {
    let (usable, free) = crate::mm::frame::stats();
    let page = crate::mm::PAGE_SIZE;
    let procs = TABLE.lock().procs.len().min(u16::MAX as usize) as u64;
    let mut b = [0u8; 112];
    let mut put = |o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put(0, crate::time::uptime_ms() / 1000);
    put(32, usable as u64 * page);
    put(40, free as u64 * page);
    put(80, procs); // procs is a u16 followed by padding
    put(104, 1); // mem_unit (u32) + padding
    current().mm().copy_out(info, &b)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Scheduling

const SCHED_OTHER: u32 = 0;
const SCHED_FIFO: u32 = 1;
const SCHED_RR: u32 = 2;
const SCHED_BATCH: u32 = 3;
const SCHED_IDLE: u32 = 5;
const SCHED_RESET_ON_FORK: u32 = 0x4000_0000;

fn prio_range(policy: u32) -> KResult<(u32, u32)> {
    match policy {
        SCHED_FIFO | SCHED_RR => Ok((1, 99)),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => Ok((0, 0)),
        _ => Err(EINVAL),
    }
}

pub fn sys_sched_get_priority_max(policy: u32) -> KResult<u64> {
    prio_range(policy).map(|r| r.1 as u64)
}

pub fn sys_sched_get_priority_min(policy: u32) -> KResult<u64> {
    prio_range(policy).map(|r| r.0 as u64)
}

pub fn sys_sched_getscheduler(pid: i32) -> KResult<u64> {
    Ok(process(pid)?.attrs.lock().policy as u64)
}

fn read_param(param: u64) -> KResult<u32> {
    if param == 0 {
        return Err(EINVAL);
    }
    let mut b = [0u8; 4];
    current().mm().copy_in(param, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub fn sys_sched_setscheduler(pid: i32, policy: i32, param: u64) -> KResult<u64> {
    let p = process(pid)?;
    let prio = read_param(param)?;
    let mut a = p.attrs.lock();
    let policy = if policy < 0 { a.policy } else { policy as u32 & !SCHED_RESET_ON_FORK };
    let (lo, hi) = prio_range(policy)?;
    if prio < lo || prio > hi {
        return Err(EINVAL);
    }
    a.policy = policy;
    a.rt_priority = prio;
    Ok(0)
}

pub fn sys_sched_getparam(pid: i32, param: u64) -> KResult<u64> {
    let prio = process(pid)?.attrs.lock().rt_priority;
    current().mm().copy_out(param, &prio.to_le_bytes())?;
    Ok(0)
}

pub fn sys_sched_setparam(pid: i32, param: u64) -> KResult<u64> {
    sys_sched_setscheduler(pid, -1, param)
}

/// Bytes of an affinity mask covering every CPU (a multiple of 8).
fn mask_bytes() -> usize {
    crate::arch::x86_64::smp::online_cpus().div_ceil(64) * 8
}

pub fn sys_sched_getaffinity(pid: i32, len: u64, mask: u64) -> KResult<u64> {
    process(pid)?;
    let n = mask_bytes();
    if (len as usize) < n || len % 8 != 0 {
        return Err(EINVAL);
    }
    let mut b = alloc::vec![0u8; n];
    for cpu in 0..crate::arch::x86_64::smp::online_cpus() {
        b[cpu / 8] |= 1 << (cpu % 8);
    }
    current().mm().copy_out(mask, &b)?;
    Ok(n as u64)
}

/// Accepted when the mask names at least one online CPU; threads still
/// run anywhere.
pub fn sys_sched_setaffinity(pid: i32, len: u64, mask: u64) -> KResult<u64> {
    process(pid)?;
    let n = (len as usize).min(mask_bytes());
    let mut b = alloc::vec![0u8; n];
    current().mm().copy_in(mask, &mut b)?;
    let any = (0..crate::arch::x86_64::smp::online_cpus()).any(|c| c / 8 < n && b[c / 8] & (1 << (c % 8)) != 0);
    if any { Ok(0) } else { Err(EINVAL) }
}

pub fn sys_getcpu(cpu: u64, node: u64) -> KResult<u64> {
    let mm = current().mm();
    if cpu != 0 {
        mm.copy_out(cpu, &crate::arch::cpu_index().to_le_bytes())?;
    }
    if node != 0 {
        mm.copy_out(node, &0u32.to_le_bytes())?;
    }
    Ok(0)
}

const PRIO_PROCESS: u32 = 0;
const PRIO_PGRP: u32 = 1;
const PRIO_USER: u32 = 2;

/// Processes selected by a `getpriority`/`setpriority` (which, who).
fn prio_targets(which: u32, who: i32) -> KResult<alloc::vec::Vec<Arc<crate::process::Process>>> {
    match which {
        PRIO_PROCESS => Ok(alloc::vec![process(who)?]),
        PRIO_PGRP | PRIO_USER => {
            let me = current().pid();
            let t = TABLE.lock();
            let pgid = if who == 0 { t.procs.get(&me).map_or(0, |n| n.pgid) } else { who as u32 };
            // Everyone is uid 0: PRIO_USER matches uid 0 only.
            let v: alloc::vec::Vec<_> = t
                .tasks
                .values()
                .filter(|task| task.tid == task.pid())
                .filter(|task| match which {
                    PRIO_PGRP => t.procs.get(&task.pid()).is_some_and(|n| n.pgid == pgid && n.zombie.is_none()),
                    _ => who == 0,
                })
                .map(|task| task.process.clone())
                .collect();
            if v.is_empty() { Err(ESRCH) } else { Ok(v) }
        }
        _ => Err(EINVAL),
    }
}

/// Returns 20 - nice (1..=40), like the raw Linux syscall.
pub fn sys_getpriority(which: u32, who: i32) -> KResult<u64> {
    let best = prio_targets(which, who)?.iter().map(|p| p.attrs.lock().nice).min().unwrap_or(0);
    Ok((20 - best) as u64)
}

pub fn sys_setpriority(which: u32, who: i32, nice: i32) -> KResult<u64> {
    let nice = nice.clamp(-20, 19);
    for p in prio_targets(which, who)? {
        p.attrs.lock().nice = nice;
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Credentials (`process::Creds`) and capabilities (not modelled)

const NO_ID: u32 = u32::MAX;

fn creds() -> crate::process::Creds {
    current().creds.lock().clone()
}

/// Apply `f` to the caller's credentials; fs ids follow the effective ones.
fn change_creds(f: impl FnOnce(&mut crate::process::Creds) -> KResult<()>) -> KResult<u64> {
    let t = current();
    let mut c = t.creds.lock();
    let mut n = c.clone();
    f(&mut n)?;
    n.fsuid = n.euid;
    n.fsgid = n.egid;
    *c = n;
    Ok(0)
}

pub fn sys_getid(nr: u64) -> KResult<u64> {
    use super::nr;
    let c = creds();
    Ok(match nr {
        nr::GETUID => c.uid,
        nr::GETEUID => c.euid,
        nr::GETGID => c.gid,
        _ => c.egid,
    } as u64)
}

pub fn sys_getresid(group: bool, r: u64, e: u64, s: u64) -> KResult<u64> {
    let c = creds();
    let ids = if group { [c.gid, c.egid, c.sgid] } else { [c.uid, c.euid, c.suid] };
    let mm = current().mm();
    for (a, v) in [r, e, s].into_iter().zip(ids) {
        mm.copy_out(a, &v.to_le_bytes())?;
    }
    Ok(0)
}

/// setuid / setgid: privileged callers set all three ids, others only the
/// effective one, to their real or saved id.
pub fn sys_setid1(group: bool, id: u32) -> KResult<u64> {
    if id == NO_ID {
        return Err(EINVAL);
    }
    change_creds(|c| {
        let priv_ = c.privileged();
        let (r, e, s) = if group { (&mut c.gid, &mut c.egid, &mut c.sgid) } else { (&mut c.uid, &mut c.euid, &mut c.suid) };
        if priv_ {
            (*r, *e, *s) = (id, id, id);
        } else if id == *r || id == *s {
            *e = id;
        } else {
            return Err(EPERM);
        }
        Ok(())
    })
}

/// setresuid / setresgid (-1 keeps an id); setreuid / setregid map to it
/// (the saved id becomes the new effective one when the real id is set or
/// the effective one differs from the old real id).
pub fn sys_setresid(group: bool, nr: i32, ne: i32, ns: i32, re: bool) -> KResult<u64> {
    change_creds(|c| {
        let priv_ = c.privileged();
        let (r, e, s) = if group { (&mut c.gid, &mut c.egid, &mut c.sgid) } else { (&mut c.uid, &mut c.euid, &mut c.suid) };
        let (old_r, old_e, old_s) = (*r, *e, *s);
        let want = |v: i32| (v != -1).then_some(v as u32);
        let (nr, ne) = (want(nr), want(ne));
        let ns = if re {
            // setre*id: saved = new effective if the real id is set or the
            // effective one changes to something other than the old real id.
            match (nr, ne) {
                (Some(_), _) => Some(ne.unwrap_or(old_e)),
                (None, Some(x)) if x != old_r => Some(x),
                _ => None,
            }
        } else {
            want(ns)
        };
        if !priv_ {
            // Unprivileged: only ids the caller already has (setre*id: the
            // real id only from the real or effective one).
            let any = |v: Option<u32>| v.is_none_or(|v| v == old_r || v == old_e || v == old_s);
            let real_ok = if re { nr.is_none_or(|v| v == old_r || v == old_e) } else { any(nr) };
            if !real_ok || !any(ne) || (!re && !any(ns)) {
                return Err(EPERM);
            }
        }
        if let Some(v) = nr {
            *r = v;
        }
        if let Some(v) = ne {
            *e = v;
        }
        if let Some(v) = ns {
            *s = v;
        }
        Ok(())
    })
}

/// setfsuid / setfsgid: returns the previous fs id; changes it when
/// privileged or to one of the caller's ids.
pub fn sys_setfsid(group: bool, id: u32) -> KResult<u64> {
    let t = current();
    let mut c = t.creds.lock();
    let old = if group { c.fsgid } else { c.fsuid };
    let ok = c.privileged()
        || if group { [c.gid, c.egid, c.sgid, c.fsgid].contains(&id) } else { [c.uid, c.euid, c.suid, c.fsuid].contains(&id) };
    if ok && id != NO_ID {
        if group {
            c.fsgid = id;
        } else {
            c.fsuid = id;
        }
    }
    Ok(old as u64)
}

pub fn sys_getgroups(size: i32, list: u64) -> KResult<u64> {
    if size < 0 {
        return Err(EINVAL);
    }
    let g = creds().groups;
    if size == 0 {
        return Ok(g.len() as u64);
    }
    if (size as usize) < g.len() {
        return Err(EINVAL);
    }
    let b: alloc::vec::Vec<u8> = g.iter().flat_map(|x| x.to_le_bytes()).collect();
    current().mm().copy_out(list, &b)?;
    Ok(g.len() as u64)
}

const NGROUPS_MAX: i32 = 65536;

pub fn sys_setgroups(size: i32, list: u64) -> KResult<u64> {
    if !(0..=NGROUPS_MAX).contains(&size) {
        return Err(EINVAL);
    }
    let mut b = alloc::vec![0u8; size as usize * 4];
    current().mm().copy_in(list, &mut b)?;
    let mut groups: alloc::vec::Vec<u32> = b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    groups.sort_unstable();
    change_creds(|c| {
        if !c.privileged() {
            return Err(EPERM);
        }
        c.groups = groups;
        Ok(())
    })
}

const CAP_V1: u32 = 0x1998_0330;
const CAP_V2: u32 = 0x2007_1026;
const CAP_V3: u32 = 0x2008_0522;
/// CAP_CHOWN .. CAP_CHECKPOINT_RESTORE (40).
const CAP_FULL: u64 = (1 << 41) - 1;

fn cap_words(hdr: u64) -> KResult<usize> {
    let mm = current().mm();
    let mut b = [0u8; 8];
    mm.copy_in(hdr, &mut b)?;
    let version = u32::from_le_bytes(b[..4].try_into().unwrap());
    let pid = i32::from_le_bytes(b[4..].try_into().unwrap());
    match version {
        CAP_V1 => Ok(1),
        CAP_V2 | CAP_V3 => {
            if pid != 0 {
                process(pid)?;
            }
            Ok(2)
        }
        _ => {
            mm.copy_out(hdr, &CAP_V3.to_le_bytes())?;
            Err(EINVAL)
        }
    }
}

pub fn sys_capget(hdr: u64, data: u64) -> KResult<u64> {
    let n = match cap_words(hdr) {
        Err(e) if e == EINVAL && data == 0 => return Ok(0),
        r => r?,
    };
    if data == 0 {
        return Ok(0);
    }
    let mut b = alloc::vec::Vec::new();
    for i in 0..n {
        let w = (CAP_FULL >> (32 * i)) as u32;
        // effective, permitted, inheritable
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
    }
    current().mm().copy_out(data, &b)?;
    Ok(0)
}

/// Accepted (root may drop or keep capabilities); nothing is enforced.
pub fn sys_capset(hdr: u64, data: u64) -> KResult<u64> {
    let n = cap_words(hdr)?;
    let mut b = alloc::vec![0u8; n * 12];
    current().mm().copy_in(data, &mut b)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Misc

pub fn sys_personality(persona: u32) -> KResult<u64> {
    let p = current().process.clone();
    let mut a = p.attrs.lock();
    let old = a.personality;
    if persona != 0xffff_ffff {
        a.personality = persona;
    }
    Ok(old as u64)
}

/// `rt_sigqueueinfo` (tid < 0) and `rt_tgsigqueueinfo`.
pub fn sys_rt_sigqueueinfo(tgid: i32, tid: i32, sig: u32, uinfo: u64) -> KResult<u64> {
    if !signal::valid(sig) && sig != 0 {
        return Err(EINVAL);
    }
    let mut b = [0u8; 128];
    current().mm().copy_in(uinfo, &mut b)?;
    let code = i32::from_le_bytes(b[8..12].try_into().unwrap());
    let me = current();
    // Only the kernel may send SI_USER/SI_KERNEL (>= 0) or SI_TKILL to others.
    let to_self = tgid as u32 == me.pid();
    if (code >= 0 || code == signal::SI_TKILL) && !to_self {
        return Err(EPERM);
    }
    let info = SigInfo {
        signo: sig as i32,
        code,
        pid: i32::from_le_bytes(b[16..20].try_into().unwrap()),
        uid: u32::from_le_bytes(b[20..24].try_into().unwrap()),
        value: Some(u64::from_le_bytes(b[24..32].try_into().unwrap())),
        ..Default::default()
    };
    if tid < 0 {
        if tgid <= 0 {
            return Err(EINVAL);
        }
        signal::send_process(tgid as u32, sig, info)?;
    } else {
        if tgid <= 0 || tid <= 0 {
            return Err(EINVAL);
        }
        let task = TABLE.lock().tasks.get(&(tid as u32)).cloned().ok_or(ESRCH)?;
        if task.pid() != tgid as u32 {
            return Err(ESRCH);
        }
        if sig != 0 {
            signal::send_thread(&task, sig, info);
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Namespaces, host name, kernel log, I/O priority

static HOSTNAME: spin::Mutex<alloc::vec::Vec<u8>> = spin::Mutex::new(alloc::vec::Vec::new());
static DOMAINNAME: spin::Mutex<alloc::vec::Vec<u8>> = spin::Mutex::new(alloc::vec::Vec::new());

/// uname's release. Android 16's GKI is 6.12 (an LTS branch, so
/// netbpfload's and libbpf's version checks enable every eBPF feature it
/// expects); the suffix tells the tests they run on this kernel.
pub const UTS_RELEASE: &str = "6.12.0-android-pc";

/// uname's nodename and domainname ("android-pc" / "(none)" until set).
pub fn uts_names() -> (alloc::vec::Vec<u8>, alloc::vec::Vec<u8>) {
    let h = HOSTNAME.lock().clone();
    let d = DOMAINNAME.lock().clone();
    (if h.is_empty() { b"android-pc".to_vec() } else { h }, if d.is_empty() { b"(none)".to_vec() } else { d })
}

/// Set the host (or domain) name; also what /proc/sys/kernel/{hostname,
/// domainname} write.
pub fn set_uts_name(name: &[u8], domain: bool) -> KResult<()> {
    if name.len() > 64 {
        return Err(EINVAL);
    }
    *(if domain { &DOMAINNAME } else { &HOSTNAME }).lock() = name.to_vec();
    Ok(())
}

pub fn sys_sethostname(name: u64, len: u64, domain: bool) -> KResult<u64> {
    if len > 64 {
        return Err(EINVAL);
    }
    let mut b = alloc::vec![0u8; len as usize];
    current().mm().copy_in(name, &mut b)?;
    set_uts_name(&b, domain)?;
    Ok(0)
}

const CLONE_FS: u64 = 0x200;
const CLONE_FILES: u64 = 0x400;
const CLONE_NEWNS: u64 = 0x2_0000;
const CLONE_SYSVSEM: u64 = 0x4_0000;
const CLONE_NEWCGROUP: u64 = 0x200_0000;
const CLONE_NEWUTS: u64 = 0x400_0000;
const CLONE_NEWIPC: u64 = 0x800_0000;
const CLONE_NEWUSER: u64 = 0x1000_0000;
const CLONE_NEWPID: u64 = 0x2000_0000;
const CLONE_NEWNET: u64 = 0x4000_0000;
const CLONE_NEWTIME: u64 = 0x80;

/// unshare: CLONE_FILES gives the caller a private fd table, CLONE_NEWNS a
/// copy of its mount namespace; the other namespace flags are accepted but
/// there is one global namespace of each other kind.
pub fn sys_unshare(flags: u64) -> KResult<u64> {
    let ns = CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWTIME;
    if flags & !(ns | CLONE_FS | CLONE_FILES | CLONE_SYSVSEM) != 0 {
        return Err(EINVAL);
    }
    if flags & CLONE_FILES != 0 {
        let p = &current().process;
        let dup = p.files().duplicate();
        *p.files.lock() = dup;
    }
    if flags & CLONE_NEWNS != 0 {
        let p = current().process.clone();
        let (ns, map) = crate::fs::vfs::copy_ns(&crate::fs::vfs::current_ns());
        let cwd = map(&p.cwd());
        *p.mnt_ns.lock() = Some(ns);
        p.fs.lock().cwd = cwd;
    }
    Ok(0)
}

/// setns: joining a mount namespace (an open /proc/<pid>/ns/mnt) moves the
/// caller there, its working directory to that namespace's root; there is
/// one namespace of every other kind, so joining one of those is a no-op.
pub fn sys_setns(fd: i32, nstype: u32) -> KResult<u64> {
    let f = current().process.files().get(fd)?;
    if let Some(ns) = crate::fs::procfs::mnt_ns_of(&*f.inode) {
        if nstype != 0 && nstype as u64 != CLONE_NEWNS {
            return Err(EINVAL);
        }
        let p = current().process.clone();
        p.fs.lock().cwd = crate::fs::vfs::ns_root(&ns);
        *p.mnt_ns.lock() = Some(ns);
    }
    Ok(0)
}

/// syslog (klogctl) on the kernel log buffer (`fs::kmsg`).
pub fn sys_syslog(ty: i32, buf: u64, len: i32) -> KResult<u64> {
    use crate::fs::kmsg;
    match ty {
        // CLOSE, OPEN, CONSOLE_OFF/ON/LEVEL
        0 | 1 | 6 | 7 => Ok(0),
        8 => if (1..=8).contains(&len) { Ok(0) } else { Err(EINVAL) },
        2..=4 => {
            if len < 0 || buf == 0 {
                return Err(EINVAL);
            }
            if len == 0 {
                return Ok(0);
            }
            let data = if ty == 2 {
                let mut b = alloc::vec![0u8; len as usize];
                let n = kmsg::read_syslog(&mut b, false)?;
                b.truncate(n);
                b
            } else {
                kmsg::read_all(len as usize, ty == 4)
            };
            current().mm().copy_out(buf, &data)?;
            Ok(data.len() as u64)
        }
        5 => {
            kmsg::clear();
            Ok(0)
        }
        9 => Ok(kmsg::size_unread() as u64),
        10 => Ok(kmsg::BUFFER_SIZE as u64),
        _ => Err(EINVAL),
    }
}

/// ioprio: stored per process (IOPRIO_WHO_PROCESS), not applied.
pub fn sys_ioprio_set(which: i32, who: i32, prio: u32) -> KResult<u64> {
    if !(1..=3).contains(&which) || prio >> 13 > 3 {
        return Err(EINVAL);
    }
    if which == 1 {
        let p = target_process(who)?;
        p.attrs.lock().ioprio = prio;
    }
    Ok(0)
}

pub fn sys_ioprio_get(which: i32, who: i32) -> KResult<u64> {
    if !(1..=3).contains(&which) {
        return Err(EINVAL);
    }
    if which == 1 {
        return Ok(target_process(who)?.attrs.lock().ioprio as u64);
    }
    Ok(4)
}

fn target_process(who: i32) -> KResult<alloc::sync::Arc<crate::process::Process>> {
    if who == 0 {
        return Ok(current().process.clone());
    }
    let t = TABLE.lock();
    match t.procs.get(&(who as u32)) {
        Some(n) => Ok(n.proc.clone()),
        None => t.tasks.get(&(who as u32)).map(|x| x.process.clone()).ok_or(ESRCH),
    }
}
