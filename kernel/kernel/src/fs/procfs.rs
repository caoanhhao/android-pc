//! procfs (minimal): /proc/self, /proc/<pid>/{stat,status,cmdline,comm,
//! maps,mounts,fd/,fdinfo/,exe,cwd,root}, /proc/{meminfo,cpuinfo,uptime,mounts,
//! filesystems,cmdline,version,loadavg,stat,net/unix,
//! sys/kernel/random/{boot_id,uuid}}.
//!
//! Inodes are made on demand and never cached in dentries; file contents
//! are generated when the file is opened.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;
use spin::Once;

use super::file::{FileOps, SeqFile};
use super::vfs::{self, DirEntry, Inode, Meta, PathRef, S_IFDIR, S_IFLNK, S_IFREG, dtype};
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
    NetDir,
    NetUnix,
    /// /proc/sys, /proc/sys/kernel, /proc/sys/kernel/random.
    SysDir,
    SysKernelDir,
    SysRandomDir,
    /// Random UUID fixed at boot (libbinder needs it), and a fresh one per open.
    BootId,
    Uuid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Global {
    Meminfo,
    Cpuinfo,
    Uptime,
    Mounts,
    Filesystems,
    Cmdline,
    Version,
    Loadavg,
    Stat,
    Partitions,
}

const GLOBALS: [(&[u8], Global); 10] = [
    (b"cmdline", Global::Cmdline),
    (b"cpuinfo", Global::Cpuinfo),
    (b"filesystems", Global::Filesystems),
    (b"loadavg", Global::Loadavg),
    (b"meminfo", Global::Meminfo),
    (b"mounts", Global::Mounts),
    (b"partitions", Global::Partitions),
    (b"stat", Global::Stat),
    (b"uptime", Global::Uptime),
    (b"version", Global::Version),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PidFile {
    Stat,
    Status,
    Cmdline,
    Comm,
    Maps,
    Mounts,
    Exe,
    Cwd,
    Root,
}

const PID_FILES: [(&[u8], PidFile); 9] = [
    (b"cmdline", PidFile::Cmdline),
    (b"comm", PidFile::Comm),
    (b"cwd", PidFile::Cwd),
    (b"exe", PidFile::Exe),
    (b"maps", PidFile::Maps),
    (b"mounts", PidFile::Mounts),
    (b"root", PidFile::Root),
    (b"stat", PidFile::Stat),
    (b"status", PidFile::Status),
];

pub struct ProcInode {
    meta: Meta,
    node: Node,
}

static PROC_DEV: Once<u64> = Once::new();

fn proc_dev() -> u64 {
    *PROC_DEV.call_once(vfs::alloc_dev)
}

/// Stable inode numbers: what the node is, packed.
fn ino_of(node: Node) -> u64 {
    match node {
        Node::Root => 1,
        Node::SelfLink => 2,
        Node::ThreadSelfLink => 3,
        Node::Global(g) => 16 + g as u64,
        Node::PidDir(p) => (p as u64) << 16 | 1,
        Node::FdDir(p) => (p as u64) << 16 | 2,
        Node::PidFile(p, f) => (p as u64) << 16 | (16 + f as u64),
        Node::FdLink(p, fd) => (1 << 48) | (p as u64) << 16 | fd as u64,
        Node::FdInfoDir(p) => (p as u64) << 16 | 3,
        Node::FdInfo(p, fd) => (2 << 48) | (p as u64) << 16 | fd as u64,
        Node::NetDir => 4,
        Node::NetUnix => 5,
        Node::SysDir => 6,
        Node::SysKernelDir => 7,
        Node::SysRandomDir => 8,
        Node::BootId => 9,
        Node::Uuid => 10,
    }
}

fn mode_of(node: Node) -> u32 {
    match node {
        Node::Root | Node::PidDir(_) | Node::NetDir | Node::SysDir | Node::SysKernelDir | Node::SysRandomDir => {
            S_IFDIR | 0o555
        }
        Node::FdDir(_) => S_IFDIR | 0o500,
        Node::FdInfoDir(_) => S_IFDIR | 0o555,
        Node::FdInfo(..) => S_IFREG | 0o444,
        Node::SelfLink | Node::ThreadSelfLink => S_IFLNK | 0o777,
        Node::FdLink(..) => S_IFLNK | 0o700,
        Node::PidFile(_, PidFile::Exe | PidFile::Cwd | PidFile::Root) => S_IFLNK | 0o777,
        Node::Global(_) | Node::PidFile(..) | Node::NetUnix | Node::BootId | Node::Uuid => S_IFREG | 0o444,
    }
}

fn make(node: Node) -> Arc<dyn Inode> {
    Arc::new(ProcInode { meta: Meta::with_ino(ino_of(node), proc_dev(), mode_of(node), 0), node })
}

/// A new procfs instance (its root directory).
pub fn new_procfs() -> Arc<dyn Inode> {
    make(Node::Root)
}

fn process(pid: u32) -> KResult<Arc<Process>> {
    TABLE.lock().procs.get(&pid).map(|n| n.proc.clone()).ok_or(ENOENT)
}

fn parse_num(name: &[u8]) -> Option<u64> {
    if name.is_empty() || name.len() > 10 || (name.len() > 1 && name[0] == b'0') {
        return None;
    }
    core::str::from_utf8(name).ok()?.parse().ok()
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
                b"sys" => Node::SysDir,
                _ => {
                    if let Some((_, g)) = GLOBALS.iter().find(|(n, _)| *n == name) {
                        Node::Global(*g)
                    } else {
                        let pid = parse_num(name).ok_or(ENOENT)? as u32;
                        process(pid)?;
                        Node::PidDir(pid)
                    }
                }
            },
            Node::PidDir(pid) => {
                process(pid)?;
                if name == b"fd" {
                    Node::FdDir(pid)
                } else if name == b"fdinfo" {
                    Node::FdInfoDir(pid)
                } else {
                    let (_, f) = PID_FILES.iter().find(|(n, _)| *n == name).ok_or(ENOENT)?;
                    Node::PidFile(pid, *f)
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
            Node::NetDir if name == b"unix" => Node::NetUnix,
            Node::SysDir if name == b"kernel" => Node::SysKernelDir,
            Node::SysKernelDir if name == b"random" => Node::SysRandomDir,
            Node::SysRandomDir if name == b"boot_id" => Node::BootId,
            Node::SysRandomDir if name == b"uuid" => Node::Uuid,
            Node::NetDir | Node::SysDir | Node::SysKernelDir | Node::SysRandomDir => return Err(ENOENT),
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
                v.push(ent(b"sys", Node::SysDir));
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
            Node::NetDir => v.push(ent(b"unix", Node::NetUnix)),
            Node::SysDir => v.push(ent(b"kernel", Node::SysKernelDir)),
            Node::SysKernelDir => v.push(ent(b"random", Node::SysRandomDir)),
            Node::SysRandomDir => {
                v.push(ent(b"boot_id", Node::BootId));
                v.push(ent(b"uuid", Node::Uuid));
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
            _ => Err(EINVAL),
        }
    }

    fn open(&self, _flags: u32) -> KResult<Option<Box<dyn FileOps>>> {
        let data = match self.node {
            Node::Global(g) => global(g),
            Node::PidFile(pid, f) => pid_file(pid, f)?,
            Node::FdInfo(pid, fd) => fdinfo(pid, fd)?,
            Node::NetUnix => crate::net::unix::proc_net_unix(),
            Node::BootId => {
                static BOOT_ID: Once<String> = Once::new();
                BOOT_ID.call_once(random_uuid).clone()
            }
            Node::Uuid => random_uuid(),
            _ => return Ok(None),
        };
        Ok(Some(Box::new(SeqFile { data: data.into_bytes() })))
    }

    fn cacheable(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Contents

/// /proc/<pid>/fdinfo/<fd>: position, flags (with O_CLOEXEC), inode, then
/// what the file type adds (epoll items, eventfd count, timerfd state...).
/// A random (version 4) UUID line, as /proc/sys/kernel/random/uuid.
fn random_uuid() -> String {
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

pub const FILESYSTEMS: [&str; 7] = ["rootfs", "tmpfs", "ramfs", "devtmpfs", "proc", "sysfs", "binder"];

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
        Global::Mounts => mounts(&mut s),
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

fn mounts(s: &mut String) {
    for m in vfs::mounts() {
        let at = vfs::path_string(&PathRef { dentry: m.root.clone(), mnt: m.clone() });
        let src = String::from_utf8_lossy(&m.source);
        let ro = if m.flags & 1 != 0 { "ro" } else { "rw" };
        let _ = writeln!(s, "{src} {at} {} {ro} 0 0", m.fstype);
    }
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
            let _ = write!(
                s,
                "Name:\t{}\nUmask:\t{:04o}\nState:\t{}\nTgid:\t{pid}\nNgid:\t0\nPid:\t{pid}\nPPid:\t{}\n\
                 TracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t64\nGroups:\t\n\
                 Threads:\t{}\nSigQ:\t0/0\nCapEff:\t000001ffffffffff\nSeccomp:\t0\n\
                 voluntary_ctxt_switches:\t0\nnonvoluntary_ctxt_switches:\t0\n",
                comm(p),
                p.fs.lock().umask,
                state.1,
                info.ppid,
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
        PidFile::Mounts => mounts(&mut s),
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
