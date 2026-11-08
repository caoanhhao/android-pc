//! procfs: /proc/self, /proc/thread-self, /proc/<pid>/{stat,status,
//! cmdline,comm,maps,mounts,mountinfo,cgroup,environ,oom_score_adj,...,
//! fd/,fdinfo/,attr/,task/<tid>/,ns/,exe,cwd,root}, /proc/{meminfo,cpuinfo,
//! uptime,mounts,filesystems,cmdline,bootconfig,version,loadavg,stat,
//! partitions,cgroups,...}, /proc/net/{unix,netlink}, /proc/pressure/*
//! and /proc/sys (`sysctl`).
//!
//! Inodes are made on demand and never cached in dentries; file contents
//! are generated when the file is opened. Writable nodes: /proc/sys/*,
//! /proc/<pid>/attr/* (the caller's own SELinux attributes),
//! /proc/<pid>/oom_score_adj (stored), /proc/sysrq-trigger. /proc/mounts
//! (and <pid>/mounts, mountinfo) are regenerated when read from offset 0
//! and poll POLLPRI|POLLERR once after every mount table change.
//! /proc/<pid>/fd/N and ns/* are "magic" links: opening them reaches the
//! open file (or a namespace file) itself.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, Once};

use super::file::{FileOps, OpenFile, SeqFile};
use super::poll::WaitQueue;
use super::vfs::{self, DirEntry, Inode, MagicLink, Meta, PathRef, S_IFDIR, S_IFLNK, S_IFREG, dtype};
use crate::errno::*;
use crate::process::{self, Process, TABLE};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Node {
    Root,
    SelfLink,
    ThreadSelfLink,
    Global(Global),
    PidDir(u32),
    PidFile(u32, PidFile),
    FdDir(u32),
    FdLink(u32, i32),
    FdInfoDir(u32),
    FdInfo(u32, i32),
    /// attr/ of a process (tid 0) or a thread.
    AttrDir(u32, u32),
    Attr(u32, u32, Attr),
    TaskDir(u32),
    TaskTid(u32, u32),
    TaskFile(u32, u32, PidFile),
    NsDir(u32),
    Ns(u32, Ns),
    NetDir,
    NetUnix,
    NetNetlink,
    PressureDir,
    Pressure(u8),
    /// /proc/sys entries (index into `sysctl::nodes`).
    Sys(u16),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Global {
    Meminfo,
    Cpuinfo,
    Uptime,
    Mounts,
    Filesystems,
    Cmdline,
    Bootconfig,
    Version,
    Loadavg,
    Stat,
    Partitions,
    Cgroups,
    Devices,
    Modules,
    Swaps,
    Vmstat,
    Interrupts,
    SysrqTrigger,
    Kmsg,
}

const GLOBALS: [(&[u8], Global); 19] = [
    (b"bootconfig", Global::Bootconfig),
    (b"cgroups", Global::Cgroups),
    (b"cmdline", Global::Cmdline),
    (b"cpuinfo", Global::Cpuinfo),
    (b"devices", Global::Devices),
    (b"filesystems", Global::Filesystems),
    (b"interrupts", Global::Interrupts),
    (b"kmsg", Global::Kmsg),
    (b"loadavg", Global::Loadavg),
    (b"meminfo", Global::Meminfo),
    (b"modules", Global::Modules),
    (b"mounts", Global::Mounts),
    (b"partitions", Global::Partitions),
    (b"stat", Global::Stat),
    (b"swaps", Global::Swaps),
    (b"sysrq-trigger", Global::SysrqTrigger),
    (b"uptime", Global::Uptime),
    (b"version", Global::Version),
    (b"vmstat", Global::Vmstat),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PidFile {
    Stat,
    Status,
    Cmdline,
    Comm,
    Maps,
    Mounts,
    Mountinfo,
    Cgroup,
    Environ,
    OomScoreAdj,
    OomAdj,
    OomScore,
    Statm,
    Schedstat,
    TimerslackNs,
    Wchan,
    Exe,
    Cwd,
    Root,
}

const PID_FILES: [(&[u8], PidFile); 19] = [
    (b"cgroup", PidFile::Cgroup),
    (b"cmdline", PidFile::Cmdline),
    (b"comm", PidFile::Comm),
    (b"cwd", PidFile::Cwd),
    (b"environ", PidFile::Environ),
    (b"exe", PidFile::Exe),
    (b"maps", PidFile::Maps),
    (b"mountinfo", PidFile::Mountinfo),
    (b"mounts", PidFile::Mounts),
    (b"oom_adj", PidFile::OomAdj),
    (b"oom_score", PidFile::OomScore),
    (b"oom_score_adj", PidFile::OomScoreAdj),
    (b"root", PidFile::Root),
    (b"schedstat", PidFile::Schedstat),
    (b"stat", PidFile::Stat),
    (b"statm", PidFile::Statm),
    (b"status", PidFile::Status),
    (b"timerslack_ns", PidFile::TimerslackNs),
    (b"wchan", PidFile::Wchan),
];

/// Files in /proc/<pid>/task/<tid>/.
const TASK_FILES: [(&[u8], PidFile); 6] = [
    (b"cgroup", PidFile::Cgroup),
    (b"cmdline", PidFile::Cmdline),
    (b"comm", PidFile::Comm),
    (b"stat", PidFile::Stat),
    (b"status", PidFile::Status),
    (b"timerslack_ns", PidFile::TimerslackNs),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attr {
    Current,
    Prev,
    Exec,
    Fscreate,
    Keycreate,
    Sockcreate,
}

const ATTRS: [(&[u8], Attr); 6] = [
    (b"current", Attr::Current),
    (b"exec", Attr::Exec),
    (b"fscreate", Attr::Fscreate),
    (b"keycreate", Attr::Keycreate),
    (b"prev", Attr::Prev),
    (b"sockcreate", Attr::Sockcreate),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ns {
    Cgroup,
    Ipc,
    Mnt,
    Net,
    Pid,
    PidForChildren,
    Time,
    TimeForChildren,
    User,
    Uts,
}

const NSES: [(&[u8], Ns, u64); 10] = [
    (b"cgroup", Ns::Cgroup, 4026531835),
    (b"ipc", Ns::Ipc, 4026531839),
    (b"mnt", Ns::Mnt, 4026531841),
    (b"net", Ns::Net, 4026531840),
    (b"pid", Ns::Pid, 4026531836),
    (b"pid_for_children", Ns::PidForChildren, 4026531836),
    (b"time", Ns::Time, 4026531834),
    (b"time_for_children", Ns::TimeForChildren, 4026531834),
    (b"user", Ns::User, 4026531837),
    (b"uts", Ns::Uts, 4026531838),
];

const PRESSURE: [&[u8]; 3] = [b"cpu", b"io", b"memory"];

pub struct ProcInode {
    meta: Meta,
    node: Node,
}

static PROC_DEV: Once<u64> = Once::new();

fn proc_dev() -> u64 {
    *PROC_DEV.call_once(vfs::alloc_dev)
}

/// Stable inode numbers: a hash of what the node is.
fn ino_of(node: Node) -> u64 {
    if node == Node::Root {
        return 1;
    }
    let s = format!("{node:?}");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    (h >> 2) | 2
}

fn mode_of(node: Node) -> u32 {
    match node {
        Node::Root
        | Node::PidDir(_)
        | Node::NetDir
        | Node::AttrDir(..)
        | Node::TaskDir(_)
        | Node::TaskTid(..)
        | Node::PressureDir => S_IFDIR | 0o555,
        Node::FdDir(_) | Node::NsDir(_) => S_IFDIR | 0o500 | 0o011,
        Node::FdInfoDir(_) => S_IFDIR | 0o555,
        Node::FdInfo(..) => S_IFREG | 0o444,
        Node::SelfLink | Node::ThreadSelfLink => S_IFLNK | 0o777,
        Node::FdLink(..) => S_IFLNK | 0o700,
        Node::Ns(..) => S_IFLNK | 0o777,
        Node::PidFile(_, PidFile::Exe | PidFile::Cwd | PidFile::Root) => S_IFLNK | 0o777,
        Node::PidFile(_, PidFile::OomScoreAdj | PidFile::OomAdj | PidFile::TimerslackNs) => S_IFREG | 0o644,
        Node::Attr(_, _, Attr::Current | Attr::Prev) => S_IFREG | 0o444 | if matches!(node, Node::Attr(_, _, Attr::Current)) { 0o222 } else { 0 },
        Node::Attr(..) => S_IFREG | 0o666,
        Node::Global(Global::SysrqTrigger) => S_IFREG | 0o200,
        Node::Global(Global::Kmsg) => S_IFREG | 0o400,
        Node::Global(Global::Bootconfig) => S_IFREG | 0o440,
        Node::Sys(i) => {
            if super::sysctl::nodes()[i as usize].file.is_some() {
                S_IFREG | 0o644
            } else {
                S_IFDIR | 0o555
            }
        }
        Node::Pressure(_) => S_IFREG | 0o666,
        Node::Global(_) | Node::PidFile(..) | Node::TaskFile(..) | Node::NetUnix | Node::NetNetlink => S_IFREG | 0o444,
    }
}

/// The process a /proc/<pid>/... node belongs to.
fn node_pid(node: Node) -> Option<u32> {
    match node {
        Node::PidDir(p)
        | Node::PidFile(p, _)
        | Node::FdDir(p)
        | Node::FdLink(p, _)
        | Node::FdInfoDir(p)
        | Node::FdInfo(p, _)
        | Node::AttrDir(p, _)
        | Node::Attr(p, ..)
        | Node::TaskDir(p)
        | Node::TaskTid(p, _)
        | Node::TaskFile(p, ..)
        | Node::NsDir(p)
        | Node::Ns(p, _) => Some(p),
        _ => None,
    }
}

fn make(node: Node) -> Arc<dyn Inode> {
    let meta = Meta::with_ino(ino_of(node), proc_dev(), mode_of(node), 0);
    // /proc/<pid>/... belongs to the process's effective ids (ps reads
    // the owner of /proc/<pid>).
    if let Some(c) = node_pid(node).and_then(process::process_creds) {
        meta.update(|a| {
            a.uid = c.euid;
            a.gid = c.egid;
        });
    }
    Arc::new(ProcInode { meta, node })
}

/// A new procfs instance (its root directory).
pub fn new_procfs() -> Arc<dyn Inode> {
    make(Node::Root)
}

fn process(pid: u32) -> KResult<Arc<Process>> {
    TABLE.lock().procs.get(&pid).map(|n| n.proc.clone()).ok_or(ENOENT)
}

/// `tid` is a live thread of `pid`.
fn check_thread(pid: u32, tid: u32) -> KResult<()> {
    let t = TABLE.lock();
    let n = t.procs.get(&pid).ok_or(ENOENT)?;
    if n.threads.contains(&tid) { Ok(()) } else { Err(ENOENT) }
}

fn parse_num(name: &[u8]) -> Option<u64> {
    if name.is_empty() || name.len() > 10 || (name.len() > 1 && name[0] == b'0') {
        return None;
    }
    core::str::from_utf8(name).ok()?.parse().ok()
}

fn ns_id(n: Ns) -> u64 {
    NSES.iter().find(|(_, x, _)| *x == n).map_or(0, |e| e.2)
}

/// The inode an open of /proc/<pid>/ns/<x> reaches (nsfs-like).
fn ns_inode(n: Ns) -> Arc<dyn Inode> {
    static NS: Once<Vec<Arc<dyn Inode>>> = Once::new();
    let all = NS.call_once(|| {
        let dev = vfs::alloc_dev();
        NSES.iter().map(|(_, _, id)| Arc::new(NsInode { meta: Meta::with_ino(*id, dev, S_IFREG | 0o444, 0) }) as Arc<dyn Inode>).collect()
    });
    all[NSES.iter().position(|(_, x, _)| *x == n).unwrap_or(0)].clone()
}

struct NsInode {
    meta: Meta,
}

/// The mount namespace of the process owning node `node`.
fn proc_mnt_ns(node: Node) -> KResult<Arc<vfs::MntNs>> {
    let p = process(node_pid(node).ok_or(ENOENT)?)?;
    let ns = p.mnt_ns.lock().clone();
    Ok(ns.unwrap_or_else(vfs::initial_ns))
}

/// What opening /proc/<pid>/ns/mnt reaches: an nsfs inode numbered by the
/// namespace id that setns() accepts.
struct MntNsInode {
    meta: Meta,
    ns: Arc<vfs::MntNs>,
}

impl Inode for MntNsInode {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn mnt_ns_inode(ns: Arc<vfs::MntNs>) -> Arc<dyn Inode> {
    static DEV: Once<u64> = Once::new();
    let dev = *DEV.call_once(vfs::alloc_dev);
    Arc::new(MntNsInode { meta: Meta::with_ino(ns.id, dev, S_IFREG | 0o444, 0), ns })
}

/// The mount namespace an open /proc/<pid>/ns/mnt refers to (setns).
pub fn mnt_ns_of(inode: &dyn Inode) -> Option<Arc<vfs::MntNs>> {
    inode.as_any().downcast_ref::<MntNsInode>().map(|i| i.ns.clone())
}

impl Inode for NsInode {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Inode for ProcInode {
    fn meta(&self) -> &Meta {
        &self.meta
    }

    fn lookup(&self, name: &[u8]) -> KResult<Arc<dyn Inode>> {
        let node = match self.node {
            Node::Root => match name {
                b"self" => Node::SelfLink,
                b"thread-self" => Node::ThreadSelfLink,
                b"net" => Node::NetDir,
                b"sys" => Node::Sys(0),
                b"pressure" => Node::PressureDir,
                _ => {
                    if let Some((_, g)) = GLOBALS.iter().find(|(n, _)| *n == name) {
                        Node::Global(*g)
                    } else {
                        let pid = parse_num(name).ok_or(ENOENT)? as u32;
                        // A thread id names its thread group's directory
                        // too (hidden from readdir, as on Linux).
                        let pid = if process(pid).is_ok() {
                            pid
                        } else {
                            TABLE.lock().tasks.get(&pid).map(|t| t.pid()).ok_or(ENOENT)?;
                            pid
                        };
                        Node::PidDir(pid)
                    }
                }
            },
            Node::PidDir(pid) => {
                let pid = owner(pid)?;
                match name {
                    b"fd" => Node::FdDir(pid),
                    b"fdinfo" => Node::FdInfoDir(pid),
                    b"attr" => Node::AttrDir(pid, 0),
                    b"task" => Node::TaskDir(pid),
                    b"ns" => Node::NsDir(pid),
                    _ => {
                        let (_, f) = PID_FILES.iter().find(|(n, _)| *n == name).ok_or(ENOENT)?;
                        Node::PidFile(pid, *f)
                    }
                }
            }
            Node::FdDir(pid) => {
                let fd = parse_num(name).ok_or(ENOENT)? as i32;
                process(pid)?.files().get(fd).map_err(|_| ENOENT)?;
                Node::FdLink(pid, fd)
            }
            Node::FdInfoDir(pid) => {
                let fd = parse_num(name).ok_or(ENOENT)? as i32;
                process(pid)?.files().get(fd).map_err(|_| ENOENT)?;
                Node::FdInfo(pid, fd)
            }
            Node::AttrDir(pid, tid) => {
                let (_, a) = ATTRS.iter().find(|(n, _)| *n == name).ok_or(ENOENT)?;
                Node::Attr(pid, tid, *a)
            }
            Node::TaskDir(pid) => {
                let tid = parse_num(name).ok_or(ENOENT)? as u32;
                check_thread(pid, tid)?;
                Node::TaskTid(pid, tid)
            }
            Node::TaskTid(pid, tid) => {
                if name == b"attr" {
                    Node::AttrDir(pid, tid)
                } else {
                    let (_, f) = TASK_FILES.iter().find(|(n, _)| *n == name).ok_or(ENOENT)?;
                    Node::TaskFile(pid, tid, *f)
                }
            }
            Node::NsDir(pid) => {
                let (_, n, _) = NSES.iter().find(|(n, _, _)| *n == name).ok_or(ENOENT)?;
                Node::Ns(pid, *n)
            }
            Node::NetDir => match name {
                b"unix" => Node::NetUnix,
                b"netlink" => Node::NetNetlink,
                _ => return Err(ENOENT),
            },
            Node::PressureDir => {
                let i = PRESSURE.iter().position(|p| *p == name).ok_or(ENOENT)?;
                Node::Pressure(i as u8)
            }
            Node::Sys(i) => {
                let n = &super::sysctl::nodes()[i as usize];
                if n.file.is_some() {
                    return Err(ENOTDIR);
                }
                let path = if n.path.is_empty() {
                    String::from_utf8_lossy(name).into_owned()
                } else {
                    format!("{}/{}", n.path, String::from_utf8_lossy(name))
                };
                Node::Sys(super::sysctl::find(&path).ok_or(ENOENT)? as u16)
            }
            _ => return Err(ENOTDIR),
        };
        Ok(make(node))
    }

    fn entries(&self) -> KResult<Vec<DirEntry>> {
        let ent = |name: &[u8], node: Node| DirEntry { name: name.to_vec(), ino: ino_of(node), dtype: dtype(mode_of(node)) };
        let mut v = Vec::new();
        match self.node {
            Node::Root => {
                v.push(ent(b"self", Node::SelfLink));
                v.push(ent(b"thread-self", Node::ThreadSelfLink));
                v.push(ent(b"net", Node::NetDir));
                v.push(ent(b"sys", Node::Sys(0)));
                v.push(ent(b"pressure", Node::PressureDir));
                for (n, g) in GLOBALS {
                    v.push(ent(n, Node::Global(g)));
                }
                let pids: Vec<u32> = TABLE.lock().procs.keys().copied().collect();
                for pid in pids {
                    v.push(ent(format!("{pid}").as_bytes(), Node::PidDir(pid)));
                }
            }
            Node::PidDir(pid) => {
                process(pid)?;
                v.push(ent(b"fd", Node::FdDir(pid)));
                v.push(ent(b"fdinfo", Node::FdInfoDir(pid)));
                v.push(ent(b"attr", Node::AttrDir(pid, 0)));
                v.push(ent(b"task", Node::TaskDir(pid)));
                v.push(ent(b"ns", Node::NsDir(pid)));
                for (n, f) in PID_FILES {
                    v.push(ent(n, Node::PidFile(pid, f)));
                }
            }
            Node::FdDir(pid) => {
                for (fd, _) in process(pid)?.files().list() {
                    v.push(ent(format!("{fd}").as_bytes(), Node::FdLink(pid, fd)));
                }
            }
            Node::FdInfoDir(pid) => {
                for (fd, _) in process(pid)?.files().list() {
                    v.push(ent(format!("{fd}").as_bytes(), Node::FdInfo(pid, fd)));
                }
            }
            Node::AttrDir(pid, tid) => {
                for (n, a) in ATTRS {
                    v.push(ent(n, Node::Attr(pid, tid, a)));
                }
            }
            Node::TaskDir(pid) => {
                let tids = TABLE.lock().procs.get(&pid).map(|n| n.threads.clone()).ok_or(ENOENT)?;
                for tid in tids {
                    v.push(ent(format!("{tid}").as_bytes(), Node::TaskTid(pid, tid)));
                }
            }
            Node::TaskTid(pid, tid) => {
                v.push(ent(b"attr", Node::AttrDir(pid, tid)));
                for (n, f) in TASK_FILES {
                    v.push(ent(n, Node::TaskFile(pid, tid, f)));
                }
            }
            Node::NsDir(pid) => {
                for (n, x, _) in NSES {
                    v.push(ent(n, Node::Ns(pid, x)));
                }
            }
            Node::NetDir => {
                v.push(ent(b"netlink", Node::NetNetlink));
                v.push(ent(b"unix", Node::NetUnix));
            }
            Node::PressureDir => {
                for (i, p) in PRESSURE.iter().enumerate() {
                    v.push(ent(p, Node::Pressure(i as u8)));
                }
            }
            Node::Sys(i) => {
                for (name, j) in super::sysctl::children(i as usize) {
                    v.push(ent(name.as_bytes(), Node::Sys(j as u16)));
                }
            }
            _ => return Err(ENOTDIR),
        }
        Ok(v)
    }

    fn readlink(&self) -> KResult<Vec<u8>> {
        let path = |p: Option<PathRef>| p.map(|p| vfs::path_of(&p)).ok_or(ENOENT);
        match self.node {
            Node::SelfLink => Ok(format!("{}", process::current().pid()).into_bytes()),
            Node::ThreadSelfLink => {
                let t = process::current();
                Ok(format!("{}/task/{}", t.pid(), t.tid).into_bytes())
            }
            Node::PidFile(pid, PidFile::Exe) => path(process(pid)?.exe.lock().clone()),
            Node::PidFile(pid, PidFile::Cwd) => path(Some(process(pid)?.fs.lock().cwd.clone())),
            Node::PidFile(_, PidFile::Root) => Ok(b"/".to_vec()),
            Node::FdLink(pid, fd) => {
                let f = process(pid)?.files().get(fd).map_err(|_| ENOENT)?;
                Ok(super::file_path(&f))
            }
            Node::Ns(_, n) => {
                let name = NSES.iter().find(|(_, x, _)| *x == n).map_or(&b"mnt"[..], |e| e.0);
                let name = match name {
                    b"pid_for_children" => &b"pid"[..],
                    b"time_for_children" => &b"time"[..],
                    x => x,
                };
                let id = if n == Ns::Mnt { proc_mnt_ns(self.node)?.id } else { ns_id(n) };
                Ok(format!("{}:[{}]", String::from_utf8_lossy(name), id).into_bytes())
            }
            _ => Err(EINVAL),
        }
    }

    fn magic_link(&self) -> Option<KResult<MagicLink>> {
        match self.node {
            Node::FdLink(pid, fd) => Some((|| {
                let f = process(pid)?.files().get(fd).map_err(|_| ENOENT)?;
                Ok(match &f.path {
                    Some(p) => MagicLink::Path(p.clone()),
                    None => MagicLink::Inode(f.inode.clone()),
                })
            })()),
            Node::Ns(_, Ns::Mnt) => Some(proc_mnt_ns(self.node).map(|ns| MagicLink::Inode(mnt_ns_inode(ns)))),
            Node::Ns(_, n) => Some(Ok(MagicLink::Inode(ns_inode(n)))),
            Node::PidFile(pid, PidFile::Exe) => {
                Some(process(pid).and_then(|p| p.exe.lock().clone().map(MagicLink::Path).ok_or(ENOENT)))
            }
            Node::PidFile(pid, PidFile::Cwd) => Some(process(pid).map(|p| MagicLink::Path(p.fs.lock().cwd.clone()))),
            _ => None,
        }
    }

    fn open(&self, _flags: u32) -> KResult<Option<Box<dyn FileOps>>> {
        let data = match self.node {
            Node::Global(Global::Mounts) | Node::PidFile(_, PidFile::Mounts) => {
                return Ok(Some(Box::new(MountsFile::new(false))));
            }
            Node::PidFile(_, PidFile::Mountinfo) => return Ok(Some(Box::new(MountsFile::new(true)))),
            Node::Global(Global::Kmsg) => return Ok(Some(super::kmsg::open_proc_kmsg())),
            Node::Global(Global::SysrqTrigger) | Node::Sys(_) | Node::Attr(..) | Node::Pressure(_) => {
                return Ok(Some(Box::new(ProcRw { node: self.node, data: Mutex::new(rw_contents(self.node)?) })));
            }
            Node::PidFile(_, PidFile::OomScoreAdj | PidFile::OomAdj | PidFile::TimerslackNs) => {
                return Ok(Some(Box::new(ProcRw { node: self.node, data: Mutex::new(rw_contents(self.node)?) })));
            }
            Node::Global(g) => global(g),
            Node::PidFile(pid, f) | Node::TaskFile(pid, _, f) => pid_file(pid, f)?,
            Node::FdInfo(pid, fd) => fdinfo(pid, fd)?,
            Node::NetUnix => crate::net::unix::proc_net_unix(),
            Node::NetNetlink => crate::net::netlink::proc_net_netlink(),
            _ => return Ok(None),
        };
        Ok(Some(Box::new(SeqFile { data: data.into_bytes() })))
    }

    fn set_xattr(&self, _name: &[u8], _value: Option<&[u8]>, _flags: u32) -> KResult<()> {
        // restorecon on /proc: labels come from genfscon, nothing to store.
        Ok(())
    }

    fn truncate(&self, _size: u64) -> KResult<()> {
        // O_TRUNC when writing a /proc/sys or attr file.
        Ok(())
    }

    fn cacheable(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The thread group of `pid` (a tid reaches its process's directory).
fn owner(pid: u32) -> KResult<u32> {
    let t = TABLE.lock();
    if t.procs.contains_key(&pid) {
        return Ok(pid);
    }
    t.tasks.get(&pid).map(|x| x.pid()).ok_or(ENOENT)
}

// ---------------------------------------------------------------------------
// Writable nodes

fn attr_of(s: &crate::security::SecAttrs, a: Attr) -> Vec<u8> {
    let v = match a {
        Attr::Current => &s.current,
        Attr::Prev => &s.prev,
        Attr::Exec => &s.exec,
        Attr::Fscreate => &s.fscreate,
        Attr::Keycreate => &s.keycreate,
        Attr::Sockcreate => &s.sockcreate,
    };
    let mut v = v.clone();
    if !v.is_empty() {
        v.push(0);
    }
    v
}

fn rw_contents(node: Node) -> KResult<Vec<u8>> {
    Ok(match node {
        Node::Sys(i) => super::sysctl::read(i as usize),
        Node::Attr(pid, _, a) => attr_of(&process(pid)?.sec.lock(), a),
        Node::PidFile(pid, PidFile::OomScoreAdj) => format!("{}\n", process(pid)?.attrs.lock().oom_score_adj).into_bytes(),
        Node::PidFile(pid, PidFile::OomAdj) => {
            let adj = process(pid)?.attrs.lock().oom_score_adj;
            format!("{}\n", if adj == 1000 { 15 } else { adj * 17 / 1000 }).into_bytes()
        }
        Node::PidFile(pid, PidFile::TimerslackNs) => format!("{}\n", process(pid)?.attrs.lock().timerslack).into_bytes(),
        Node::Pressure(_) => b"some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n".to_vec(),
        _ => Vec::new(),
    })
}

fn parse_int(data: &[u8]) -> KResult<i64> {
    core::str::from_utf8(data).map_err(|_| EINVAL)?.trim_matches(|c: char| c.is_whitespace() || c == '\0').parse().map_err(|_| EINVAL)
}

fn proc_write(node: Node, data: &[u8]) -> KResult<usize> {
    match node {
        Node::Sys(i) => super::sysctl::write(i as usize, data),
        Node::Attr(pid, _, a) => {
            let me = process::current();
            if me.pid() != pid {
                return Err(EACCES);
            }
            let c = crate::security::clean_context(data);
            if !c.is_empty() && !crate::security::valid_context(&c) {
                return Err(EINVAL);
            }
            let mut s = me.process.sec.lock();
            match a {
                Attr::Current => {
                    if c.is_empty() {
                        return Err(EINVAL);
                    }
                    s.prev = core::mem::replace(&mut s.current, c);
                }
                Attr::Prev => return Err(EACCES),
                Attr::Exec => s.exec = c,
                Attr::Fscreate => s.fscreate = c,
                Attr::Keycreate => s.keycreate = c,
                Attr::Sockcreate => s.sockcreate = c,
            }
            Ok(data.len())
        }
        Node::PidFile(pid, PidFile::OomScoreAdj) => {
            let v = parse_int(data)?;
            if !(-1000..=1000).contains(&v) {
                return Err(EINVAL);
            }
            process(pid)?.attrs.lock().oom_score_adj = v as i32;
            Ok(data.len())
        }
        Node::PidFile(pid, PidFile::OomAdj) => {
            let v = parse_int(data)?;
            if !(-17..=15).contains(&v) {
                return Err(EINVAL);
            }
            process(pid)?.attrs.lock().oom_score_adj = if v == 15 { 1000 } else { (v * 1000 / 17) as i32 };
            Ok(data.len())
        }
        Node::PidFile(pid, PidFile::TimerslackNs) => {
            let v = parse_int(data)?;
            if v < 0 {
                return Err(EINVAL);
            }
            process(pid)?.attrs.lock().timerslack = v as u64;
            Ok(data.len())
        }
        Node::Global(Global::SysrqTrigger) => {
            log::info!("sysrq: {}", String::from_utf8_lossy(data).trim());
            Ok(data.len())
        }
        // PSI triggers ("some 150000 1000000"): accepted, never fire.
        Node::Pressure(_) => Ok(data.len()),
        _ => Err(EINVAL),
    }
}

struct ProcRw {
    node: Node,
    data: Mutex<Vec<u8>>,
}

impl FileOps for ProcRw {
    fn read(&self, _: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut d = self.data.lock();
        if *pos == 0
            && let Ok(fresh) = rw_contents(self.node)
        {
            // Re-read from the start sees later writes (init checks
            // mmap_rnd_bits that way, through one ifstream).
            *d = fresh;
        }
        let start = (*pos as usize).min(d.len());
        let n = buf.len().min(d.len() - start);
        buf[..n].copy_from_slice(&d[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let n = proc_write(self.node, data)?;
        if let Ok(d) = rw_contents(self.node) {
            *self.data.lock() = d;
        }
        Ok(n)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.data.lock().len() as u64
    }

    /// PSI files take triggers that never fire: epoll-able, never ready
    /// (lmkd waits on them).
    fn poll(&self, _: &OpenFile) -> u32 {
        if matches!(self.node, Node::Pressure(_)) { 0 } else { super::poll::DEFAULT_POLLMASK }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        static PSI_WQ: Once<Arc<WaitQueue>> = Once::new();
        matches!(self.node, Node::Pressure(_)).then(|| PSI_WQ.call_once(WaitQueue::new).clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// /proc/mounts, /proc/<pid>/mounts and mountinfo: regenerated whenever
/// read from offset 0; POLLERR|POLLPRI once per mount table change.
struct MountsFile {
    info: bool,
    data: Mutex<Vec<u8>>,
    seen: AtomicU64,
}

impl MountsFile {
    fn new(info: bool) -> MountsFile {
        MountsFile { info, data: Mutex::new(mounts_text(info).into_bytes()), seen: AtomicU64::new(vfs::mount_generation()) }
    }
}

impl FileOps for MountsFile {
    fn read(&self, _: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut d = self.data.lock();
        if *pos == 0 {
            *d = mounts_text(self.info).into_bytes();
        }
        let start = (*pos as usize).min(d.len());
        let n = buf.len().min(d.len() - start);
        buf[..n].copy_from_slice(&d[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.data.lock().len() as u64
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let g = vfs::mount_generation();
        let mut m = super::poll::READ_EVENTS;
        if self.seen.swap(g, Ordering::Relaxed) != g {
            m |= super::poll::POLLERR | super::poll::POLLPRI;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(vfs::mount_wait_queue())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Contents

/// The boot id (/proc/sys/kernel/random/boot_id), fixed at boot.
pub fn boot_id() -> String {
    static BOOT_ID: Once<String> = Once::new();
    BOOT_ID.call_once(random_uuid).clone()
}

/// A random (version 4) UUID line, as /proc/sys/kernel/random/uuid.
pub fn random_uuid() -> String {
    let mut b = [0u8; 16];
    crate::random::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut s = String::new();
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{x:02x}");
    }
    s.push('\n');
    s
}

fn fdinfo(pid: u32, fd: i32) -> KResult<String> {
    let files = process(pid)?.files();
    let f = files.get(fd).map_err(|_| ENOENT)?;
    let mut flags = f.flags();
    if files.cloexec(fd).unwrap_or(false) {
        flags |= super::file::O_CLOEXEC;
    }
    let mut s = format!("pos:\t{}\nflags:\t0{:o}\nmnt_id:\t{}\nino:\t{}\n", f.pos(), flags, f.inode.meta().dev, f.inode.meta().ino);
    s.push_str(&f.ops.fdinfo(&f));
    Ok(s)
}

pub const FILESYSTEMS: [&str; 17] = [
    "rootfs", "tmpfs", "ramfs", "devtmpfs", "proc", "sysfs", "binder", "selinuxfs", "cgroup", "cgroup2", "devpts",
    "tracefs", "debugfs", "pstore", "bpf", "configfs", "fusectl",
];

fn global(g: Global) -> String {
    let mut s = String::new();
    match g {
        Global::Meminfo => {
            let (total, free) = crate::mm::frame::stats();
            let (total, free) = (total * 4, free * 4);
            let rows = [
                ("MemTotal", total),
                ("MemFree", free),
                ("MemAvailable", free),
                ("Buffers", 0),
                ("Cached", 0),
                ("SwapCached", 0),
                ("Active", 0),
                ("Inactive", 0),
                ("SwapTotal", 0),
                ("SwapFree", 0),
                ("Dirty", 0),
                ("AnonPages", total - free),
                ("Mapped", 0),
                ("Shmem", 0),
                ("Slab", 0),
                ("CommitLimit", total),
                ("VmallocTotal", 34359738367),
            ];
            for (k, v) in rows {
                let _ = writeln!(s, "{:<16}{:>8} kB", format!("{k}:"), v);
            }
        }
        Global::Cpuinfo => cpuinfo(&mut s),
        Global::Uptime => {
            let ms = crate::time::uptime_ms();
            let _ = writeln!(s, "{}.{:02} {}.{:02}", ms / 1000, ms % 1000 / 10, ms / 1000, ms % 1000 / 10);
        }
        Global::Mounts => s = mounts_text(false),
        Global::Bootconfig => s = bootconfig(),
        Global::Cgroups => s = super::cgroupfs::proc_cgroups(),
        Global::Devices => {
            s.push_str("Character devices:\n  1 mem\n  4 /dev/vc/0\n  4 tty\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n  7 vcs\n 10 misc\n244 binder\n\nBlock devices:\n  7 loop\n253 device-mapper\n254 virtblk\n259 blkext\n");
        }
        Global::Modules | Global::SysrqTrigger | Global::Kmsg => {}
        Global::Swaps => s.push_str("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"),
        Global::Vmstat => {
            let (total, free) = crate::mm::frame::stats();
            let _ = write!(
                s,
                "nr_free_pages {free}\nnr_inactive_anon 0\nnr_active_anon {}\nnr_inactive_file 0\nnr_active_file 0\nnr_unevictable 0\nnr_file_pages 0\nnr_shmem 0\nnr_dirty 0\nnr_writeback 0\npgpgin 0\npgpgout 0\npswpin 0\npswpout 0\npgfault 0\npgmajfault 0\npgsteal_kswapd 0\npgsteal_direct 0\npgscan_kswapd 0\npgscan_direct 0\nworkingset_refault_file 0\nnr_zone_inactive_file 0\nnr_zone_active_file 0\n",
                total - free
            );
        }
        Global::Interrupts => {
            let cpus = crate::arch::x86_64::smp::online_cpus();
            s.push_str("     ");
            for i in 0..cpus {
                let _ = write!(s, "      CPU{i}");
            }
            s.push('\n');
        }
        Global::Partitions => s.push_str(&crate::block::proc_partitions()),
        Global::Filesystems => {
            for f in FILESYSTEMS {
                let _ = writeln!(s, "nodev\t{f}");
            }
            for f in ["ext4", "ext3", "ext2", "erofs"] {
                let _ = writeln!(s, "\t{f}");
            }
        }
        Global::Cmdline => {
            let _ = writeln!(s, "{}", crate::boot::cmdline());
        }
        Global::Version => {
            let _ = writeln!(
                s,
                "Linux version 6.6.0-android-pc (android-pc) #1 SMP android-pc {}",
                env!("CARGO_PKG_VERSION")
            );
        }
        Global::Loadavg => {
            let (n, last) = {
                let t = TABLE.lock();
                (t.tasks.len(), t.tasks.keys().last().copied().unwrap_or(0))
            };
            let _ = writeln!(s, "0.00 0.00 0.00 1/{n} {last}");
        }
        Global::Stat => {
            let cpus = crate::arch::x86_64::smp::online_cpus();
            let _ = writeln!(s, "cpu  0 0 0 0 0 0 0 0 0 0");
            for i in 0..cpus {
                let _ = writeln!(s, "cpu{i} 0 0 0 0 0 0 0 0 0 0");
            }
            let boot = (crate::time::realtime_ns() / 1_000_000_000) as u64 - crate::time::uptime_ms() / 1000;
            let _ = writeln!(s, "intr 0\nctxt 0\nbtime {boot}\nprocesses {}", TABLE.lock().procs.len());
            let _ = writeln!(s, "procs_running 1\nprocs_blocked 0");
        }
    }
    s
}

/// /proc/mounts (or, with `info`, /proc/self/mountinfo) lines for every
/// mount reachable from the root.
fn mounts_text(info: bool) -> String {
    let mut s = String::new();
    let all = vfs::mounts();
    let root = vfs::root();
    for (i, m) in all.iter().enumerate() {
        let me = PathRef { dentry: m.root.clone(), mnt: m.clone() };
        if !vfs::reachable(&me, &root) {
            continue;
        }
        let at = vfs::path_string(&me);
        let src = String::from_utf8_lossy(&m.source);
        let mut opts = String::from(if m.flags & 1 != 0 { "ro" } else { "rw" });
        if let Some(extra) = m.root.inode.mount_options()
            && !extra.is_empty()
        {
            opts.push(',');
            opts.push_str(&extra);
        }
        if info {
            let parent = m
                .parent()
                .and_then(|p| all.iter().position(|x| Arc::ptr_eq(x, &p.mnt)))
                .map_or(i, |x| x);
            let dev = m.root.inode.meta().dev;
            let shared = if m.group() != 0 { alloc::format!(" shared:{}", m.group()) } else { String::new() };
            let _ = writeln!(
                s,
                "{} {} {}:{} / {at} {opts}{shared} - {} {src} {opts}",
                i + 1,
                parent + 1,
                vfs::dev_major(dev),
                vfs::dev_minor(dev),
                m.fstype
            );
        } else {
            let _ = writeln!(s, "{src} {at} {} {opts} 0 0", m.fstype);
        }
    }
    s
}

/// /proc/bootconfig: the androidboot.* parameters of the kernel command
/// line in bootconfig syntax (there is no separate bootconfig blob).
fn bootconfig() -> String {
    let mut s = String::new();
    for w in crate::boot::cmdline().split_ascii_whitespace() {
        if w == "--" {
            break;
        }
        if let Some((k, v)) = w.split_once('=')
            && k.starts_with("androidboot.")
        {
            let _ = writeln!(s, "{k} = \"{v}\"");
        }
    }
    s
}

fn cpuinfo(s: &mut String) {
    use core::arch::x86_64::__cpuid;
    let l0 = __cpuid(0);
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&l0.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&l0.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&l0.ecx.to_le_bytes());
    let mut model = Vec::new();
    if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let r = __cpuid(leaf);
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                model.extend_from_slice(&reg.to_le_bytes());
            }
        }
    }
    let model = String::from_utf8_lossy(&model).trim_matches(|c: char| c == '\0' || c == ' ').into();
    let model: String = model;
    let l1 = __cpuid(1);
    let family = (l1.eax >> 8) & 0xf;
    let mut flags = String::new();
    let edx = [(0, "fpu"), (4, "tsc"), (5, "msr"), (6, "pae"), (8, "cx8"), (9, "apic"), (15, "cmov"), (19, "clflush"), (23, "mmx"), (24, "fxsr"), (25, "sse"), (26, "sse2")];
    let ecx = [(0, "pni"), (9, "ssse3"), (13, "cx16"), (19, "sse4_1"), (20, "sse4_2"), (22, "movbe"), (23, "popcnt"), (25, "aes"), (26, "xsave"), (28, "avx"), (30, "rdrand"), (31, "hypervisor")];
    for (bit, name) in edx {
        if l1.edx & (1 << bit) != 0 {
            flags.push_str(name);
            flags.push(' ');
        }
    }
    flags.push_str("syscall nx lm ");
    for (bit, name) in ecx {
        if l1.ecx & (1 << bit) != 0 {
            flags.push_str(name);
            flags.push(' ');
        }
    }
    let mhz = crate::time::tsc_hz() / 1_000_000;
    let cpus = crate::arch::x86_64::smp::online_cpus();
    for i in 0..cpus {
        let _ = write!(
            s,
            "processor\t: {i}\nvendor_id\t: {}\ncpu family\t: {family}\nmodel name\t: {model}\n\
             cpu MHz\t\t: {mhz}.000\ncache size\t: 0 KB\nphysical id\t: 0\nsiblings\t: {cpus}\n\
             core id\t\t: {i}\ncpu cores\t: {cpus}\napicid\t\t: {i}\nfpu\t\t: yes\nwp\t\t: yes\n\
             flags\t\t: {}\nbogomips\t: {}.00\nclflush size\t: 64\naddress sizes\t: 48 bits physical, 48 bits virtual\n\n",
            String::from_utf8_lossy(&vendor),
            flags.trim_end(),
            mhz * 2
        );
    }
}

struct PidInfo {
    proc: Arc<Process>,
    ppid: u32,
    pgid: u32,
    sid: u32,
    threads: usize,
    zombie: bool,
}

fn pid_info(pid: u32) -> KResult<PidInfo> {
    let t = TABLE.lock();
    let n = t.procs.get(&pid).ok_or(ENOENT)?;
    Ok(PidInfo {
        proc: n.proc.clone(),
        ppid: n.parent,
        pgid: n.pgid,
        sid: n.sid,
        threads: n.threads.len(),
        zombie: n.zombie.is_some(),
    })
}

fn comm(p: &Process) -> String {
    let c = *p.comm.lock();
    let n = c.iter().position(|&b| b == 0).unwrap_or(c.len());
    String::from_utf8_lossy(&c[..n]).into_owned()
}

/// The address space of `pid`, only for the calling process (reading
/// another task's mm pointer would race with its execve).
fn own_mm(pid: u32) -> Option<Arc<crate::mm::user::AddressSpace>> {
    let t = process::current();
    (t.pid() == pid).then(|| t.mm())
}

fn pid_file(pid: u32, f: PidFile) -> KResult<String> {
    let info = pid_info(pid)?;
    let p = &info.proc;
    let mut s = String::new();
    let state = if info.zombie {
        ('Z', "Z (zombie)")
    } else if process::current().pid() == pid {
        ('R', "R (running)")
    } else {
        ('S', "S (sleeping)")
    };
    match f {
        PidFile::Stat => {
            let (vsize, brk) = own_mm(pid).map_or((0, (0, 0)), |mm| {
                (mm.total_size(), mm.brk_range())
            });
            let _ = write!(
                s,
                "{pid} ({}) {} {} {} {} 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 {} 0 0 {vsize} 0 \
                 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 {} {} 0 0 0 0 0\n",
                comm(p),
                state.0,
                info.ppid,
                info.pgid,
                info.sid,
                info.threads,
                p.exit_signal,
                brk.0,
                brk.1,
            );
        }
        PidFile::Status => {
            let c = process::process_creds(pid).unwrap_or_default();
            let _ = write!(
                s,
                "Name:\t{}\nUmask:\t{:04o}\nState:\t{}\nTgid:\t{pid}\nNgid:\t0\nPid:\t{pid}\nPPid:\t{}\n\
                 TracerPid:\t0\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nFDSize:\t64\nGroups:\t{}\n\
                 Threads:\t{}\nSigQ:\t0/0\nCapEff:\t000001ffffffffff\nSeccomp:\t0\n\
                 voluntary_ctxt_switches:\t0\nnonvoluntary_ctxt_switches:\t0\n",
                comm(p),
                p.fs.lock().umask,
                state.1,
                info.ppid,
                c.uid,
                c.euid,
                c.suid,
                c.fsuid,
                c.gid,
                c.egid,
                c.sgid,
                c.fsgid,
                c.groups.iter().map(|g| format!("{g} ")).collect::<String>(),
                info.threads,
            );
            if let Some(mm) = own_mm(pid) {
                let kb: u64 = mm.total_size() / 1024;
                let _ = writeln!(s, "VmSize:\t{kb:>8} kB");
            }
        }
        PidFile::Cmdline => return Ok(String::from_utf8_lossy(&p.cmdline.lock()).into_owned()),
        PidFile::Comm => {
            let _ = writeln!(s, "{}", comm(p));
        }
        PidFile::Maps => maps(pid, p, &mut s),
        PidFile::Mounts => s = mounts_text(false),
        PidFile::Mountinfo => s = mounts_text(true),
        PidFile::Cgroup => s = super::cgroupfs::proc_cgroup(&p.cgroups.lock()),
        PidFile::Environ => {}
        PidFile::OomScoreAdj | PidFile::OomAdj | PidFile::TimerslackNs => {}
        PidFile::OomScore => {
            let adj = p.attrs.lock().oom_score_adj;
            let _ = writeln!(s, "{}", (adj.max(0) as i64 + 666 * (adj >= 0) as i64).min(2000));
        }
        PidFile::Statm => {
            let pages = own_mm(pid).map_or(0, |mm| mm.total_size() / 4096);
            let _ = writeln!(s, "{pages} 0 0 0 0 0 0");
        }
        PidFile::Schedstat => s.push_str("0 0 0\n"),
        PidFile::Wchan => s.push('0'),
        PidFile::Exe | PidFile::Cwd | PidFile::Root => return Err(EINVAL),
    }
    Ok(s)
}

fn maps(pid: u32, p: &Process, s: &mut String) {
    let Some(mm) = own_mm(pid) else { return };
    let (brk_start, brk) = mm.brk_range();
    let exe = p.exe.lock().clone();
    let (exe_name, exe_ino) = exe
        .map(|e| (vfs::path_string(&e), e.inode().meta().ino))
        .unwrap_or_default();
    let interp = p.interp.lock().clone();
    let (interp_name, interp_ino, interp_range) = interp
        .map(|(i, r)| (vfs::path_string(&i), i.inode().meta().ino, r))
        .unwrap_or_default();
    let stack_top = crate::mm::user::STACK_TOP;
    for v in mm.vma_list() {
        let (start, end, prot) = (v.start, v.end, v.prot);
        let perms = [
            if prot & crate::mm::user::PROT_READ != 0 { 'r' } else { '-' },
            if prot & crate::mm::user::PROT_WRITE != 0 { 'w' } else { '-' },
            if prot & crate::mm::user::PROT_EXEC != 0 { 'x' } else { '-' },
            if v.shared { 's' } else { 'p' },
        ];
        let perms: String = perms.iter().collect();
        let mut dev = 0;
        let file_name;
        let (name, ino) = if let Some(f) = &v.file {
            file_name = String::from_utf8_lossy(&super::file_path(f)).into_owned();
            dev = f.inode.meta().dev;
            (file_name.as_str(), f.inode.meta().ino)
        } else if v.backed {
            // Anonymous shared memory, named like Linux's shmem file.
            ("/dev/zero (deleted)", 0)
        } else if start < brk_start && !exe_name.is_empty() {
            (exe_name.as_str(), exe_ino)
        } else if start >= interp_range.0 && end <= interp_range.1 && !interp_name.is_empty() {
            (interp_name.as_str(), interp_ino)
        } else if start >= brk_start && end <= brk.next_multiple_of(4096).max(brk_start) && start < brk {
            ("[heap]", 0)
        } else if start < stack_top && end >= stack_top {
            ("[stack]", 0)
        } else {
            ("", 0)
        };
        let _ = write!(
            s,
            "{start:08x}-{end:08x} {perms} {:08x} {:02x}:{:02x} {ino}",
            v.offset,
            vfs::dev_major(dev),
            vfs::dev_minor(dev)
        );
        if !name.is_empty() {
            let _ = write!(s, "{:>width$}{name}", "", width = 26usize.saturating_sub(format!("{ino}").len()));
        }
        s.push('\n');
    }
}
