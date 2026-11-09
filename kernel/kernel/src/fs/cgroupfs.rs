//! cgroup file systems: the unified v2 hierarchy (`mount -t cgroup2`) and
//! v1 hierarchies (`mount -t cgroup -o cpuset,noprefix ...`), as far as
//! init and libprocessgroup use them.
//!
//! Every hierarchy is a tree of groups. mkdir creates a group with its
//! control files, rmdir removes an empty one (EBUSY while it has member
//! processes or child groups). Membership is kept per process
//! ([`Membership`]: hierarchy id -> group path) and inherited by fork:
//! writing a pid to `cgroup.procs` (or a tid to `tasks`, which moves the
//! whole process) moves it, reading lists the live members, `cgroup.kill`
//! (v2) sends SIGKILL to them, `cgroup.events` reports "populated".
//! Every other control file only stores what is written (limits, cpus,
//! uclamp, ...); nothing is enforced. A child group starts with its
//! parent's values. Mounting the same controller set again gives the same
//! hierarchy; /proc/mounts shows the controllers as mount options.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use spin::{Mutex, Once};

use super::file::{FileOps, OpenFile};
use super::poll::{DEFAULT_POLLMASK, POLLERR, POLLPRI, WaitQueue};
use core::sync::atomic::{AtomicU64, Ordering};
use super::vfs::{self, Dentry, DirEntry, Inode, Meta, S_IFDIR, S_IFMT, S_IFREG, dtype};
use crate::errno::*;
use crate::process::{TABLE, signal};

pub const CGROUP_SUPER_MAGIC: u64 = 0x0027_e0eb;
pub const CGROUP2_SUPER_MAGIC: u64 = 0x6367_7270;

const V1_CONTROLLERS: [&str; 10] =
    ["cpuset", "cpu", "cpuacct", "memory", "blkio", "freezer", "devices", "net_cls", "pids", "schedtune"];
const V2_CONTROLLERS: &str = "cpuset cpu io memory pids";

/// Group paths ("" = root, "a/b") per hierarchy id; absent = root.
#[derive(Clone, Default)]
pub struct Membership(pub BTreeMap<u32, String>);

impl Membership {
    pub fn path(&self, hier: u32) -> &str {
        self.0.get(&hier).map_or("", |s| s.as_str())
    }
}

struct Hier {
    /// 0 is the v2 hierarchy, v1 hierarchies count from 1.
    id: u32,
    controllers: Vec<&'static str>,
    noprefix: bool,
    dev: u64,
    root: Once<Arc<CgDir>>,
    dentry: Once<Arc<Dentry>>,
}

impl Hier {
    fn v2(&self) -> bool {
        self.id == 0
    }

    fn has(&self, c: &str) -> bool {
        self.controllers.contains(&c)
    }

    fn options(&self) -> String {
        if self.v2() {
            return String::new();
        }
        let mut o = self.controllers.join(",");
        if self.noprefix {
            o.push_str(",noprefix");
        }
        o
    }

    /// Control files of a new group: (name, initial value). `parent`
    /// values are inherited.
    fn files(&self, root: bool, parent: Option<&CgDir>) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Vec::new();
        let mut add = |n: &str, val: &str| v.push((n.to_string(), val.to_string()));
        let cpus = {
            let n = crate::arch::x86_64::smp::online_cpus();
            if n > 1 { format!("0-{}", n - 1) } else { "0".into() }
        };
        if self.v2() {
            add("cgroup.procs", "");
            add("cgroup.threads", "");
            add("cgroup.controllers", V2_CONTROLLERS);
            add("cgroup.subtree_control", "");
            add("cgroup.events", "");
            add("cgroup.type", "domain");
            add("cgroup.max.depth", "max");
            add("cgroup.max.descendants", "max");
            add("cgroup.stat", "nr_descendants 0\nnr_dying_descendants 0");
            add("cgroup.pressure", "1");
            for p in ["cpu.pressure", "memory.pressure", "io.pressure"] {
                add(p, "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0");
            }
            add("memory.stat", "anon 0\nfile 0\nkernel 0\nshmem 0");
            if !root {
                add("cgroup.freeze", "0");
                add("cgroup.kill", "");
                add("memory.current", "0");
                add("memory.max", "max");
                add("memory.high", "max");
                add("memory.low", "0");
                add("memory.min", "0");
                add("memory.swap.max", "max");
                add("memory.swap.current", "0");
                add("memory.oom.group", "0");
                add("memory.events", "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0");
                add("cpu.weight", "100");
                add("cpu.weight.nice", "0");
                add("cpu.max", "max 100000");
                add("cpu.idle", "0");
                add("cpu.uclamp.min", "0.00");
                add("cpu.uclamp.max", "max");
                add("cpuset.cpus", "");
                add("cpuset.mems", "");
                add("cpuset.cpus.effective", &cpus);
                add("io.weight", "default 100");
                add("pids.max", "max");
                add("pids.current", "0");
            } else {
                add("cpu.stat", "usage_usec 0\nuser_usec 0\nsystem_usec 0");
                add("cpuset.cpus.effective", &cpus);
                add("cpuset.mems.effective", "0");
            }
        } else {
            add("cgroup.procs", "");
            add("tasks", "");
            add("cgroup.clone_children", "0");
            add("notify_on_release", "0");
            if root {
                add("release_agent", "");
                add("cgroup.sane_behavior", "0");
            }
            if self.has("cpuset") {
                let p = if self.noprefix { "" } else { "cpuset." };
                let (c, m) = if root { (cpus.as_str(), "0") } else { ("", "") };
                for (n, val) in [
                    ("cpus", c),
                    ("mems", m),
                    ("effective_cpus", c),
                    ("effective_mems", m),
                    ("cpu_exclusive", if root { "1" } else { "0" }),
                    ("mem_exclusive", if root { "1" } else { "0" }),
                    ("mem_hardwall", "0"),
                    ("memory_migrate", "0"),
                    ("memory_pressure", "0"),
                    ("memory_spread_page", "0"),
                    ("memory_spread_slab", "0"),
                    ("sched_load_balance", "1"),
                    ("sched_relax_domain_level", "-1"),
                ] {
                    add(&format!("{p}{n}"), val);
                }
                if root {
                    add(&format!("{p}memory_pressure_enabled"), "0");
                }
            }
            if self.has("cpu") {
                add("cpu.shares", "1024");
                add("cpu.rt_runtime_us", if root { "950000" } else { "0" });
                add("cpu.rt_period_us", "1000000");
                add("cpu.cfs_quota_us", "-1");
                add("cpu.cfs_period_us", "100000");
                add("cpu.uclamp.min", "0.00");
                add("cpu.uclamp.max", "max");
                add("cpu.uclamp.latency_sensitive", "0");
                add("cpu.idle", "0");
                add("cpu.stat", "nr_periods 0\nnr_throttled 0\nthrottled_time 0");
            }
            if self.has("cpuacct") {
                add("cpuacct.usage", "0");
                add("cpuacct.stat", "user 0\nsystem 0");
            }
            if self.has("memory") {
                let max = "9223372036854771712";
                for (n, val) in [
                    ("memory.limit_in_bytes", max),
                    ("memory.soft_limit_in_bytes", max),
                    ("memory.memsw.limit_in_bytes", max),
                    ("memory.usage_in_bytes", "0"),
                    ("memory.memsw.usage_in_bytes", "0"),
                    ("memory.max_usage_in_bytes", "0"),
                    ("memory.swappiness", "60"),
                    ("memory.move_charge_at_immigrate", "0"),
                    ("memory.use_hierarchy", "1"),
                    ("memory.oom_control", "oom_kill_disable 0\nunder_oom 0"),
                    ("memory.pressure_level", ""),
                    ("memory.force_empty", ""),
                    ("memory.stat", "cache 0\nrss 0\nmapped_file 0\nswap 0"),
                    ("cgroup.event_control", ""),
                ] {
                    add(n, val);
                }
            }
            if self.has("blkio") {
                add("blkio.weight", "500");
                add("blkio.leaf_weight", "500");
                add("blkio.bfq.weight", "100");
                add("blkio.group_idle", "0");
            }
            if self.has("freezer") {
                add("freezer.state", "THAWED");
            }
            if self.has("schedtune") {
                add("schedtune.boost", "0");
                add("schedtune.prefer_idle", "0");
            }
        }
        if let Some(p) = parent {
            for (n, val) in v.iter_mut() {
                if let Some(f) = p.files.get(n.as_bytes())
                    && f.kind == Kind::Value
                    && !n.starts_with("cgroup.")
                {
                    *val = String::from_utf8_lossy(&f.value.lock()).into_owned();
                }
            }
        }
        v
    }
}

static HIERS: Mutex<Vec<Arc<Hier>>> = Mutex::new(Vec::new());

/// The root dentry for a mount: `None` = cgroup2, else the v1 options.
pub fn mount(v1_opts: Option<&[u8]>) -> KResult<Arc<Dentry>> {
    let (controllers, noprefix) = match v1_opts {
        None => (Vec::new(), false),
        Some(o) => {
            let mut cs = Vec::new();
            let mut noprefix = false;
            for w in o.split(|&c| c == b',').filter(|w| !w.is_empty()) {
                let w = core::str::from_utf8(w).map_err(|_| EINVAL)?;
                match V1_CONTROLLERS.iter().find(|c| **c == w) {
                    Some(c) => cs.push(*c),
                    None if w == "noprefix" => noprefix = true,
                    None => {} // cpuset_v2_mode, none, name=, ...
                }
            }
            if cs.is_empty() {
                return Err(EINVAL);
            }
            cs.sort_unstable();
            (cs, noprefix)
        }
    };
    let h = {
        let mut hs = HIERS.lock();
        let v2 = v1_opts.is_none();
        match hs.iter().find(|h| h.v2() == v2 && (v2 || h.controllers == controllers)) {
            Some(h) => h.clone(),
            None => {
                if !v2 && hs.iter().any(|h| !h.v2() && h.controllers.iter().any(|c| controllers.contains(c))) {
                    return Err(EBUSY);
                }
                let id = if v2 { 0 } else { hs.iter().filter(|h| !h.v2()).count() as u32 + 1 };
                let h = Arc::new(Hier {
                    id,
                    controllers,
                    noprefix,
                    dev: vfs::alloc_dev(),
                    root: Once::new(),
                    dentry: Once::new(),
                });
                hs.push(h.clone());
                h
            }
        }
    };
    let root = h.root.call_once(|| CgDir::new(&h, String::new(), None, 0o755)).clone();
    Ok(h.dentry.call_once(|| Dentry::new_root(root)).clone())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Procs,
    Tasks,
    Kill,
    Events,
    Value,
}

pub struct CgDir {
    meta: Meta,
    hier: Arc<Hier>,
    /// Path inside the hierarchy ("" for the root).
    path: String,
    parent: Option<Weak<CgDir>>,
    children: Mutex<BTreeMap<Vec<u8>, Arc<CgDir>>>,
    files: BTreeMap<Vec<u8>, Arc<CgFile>>,
}

pub struct CgFile {
    meta: Meta,
    kind: Kind,
    dir: Weak<CgDir>,
    value: Mutex<Vec<u8>>,
}

impl CgDir {
    fn new(h: &Arc<Hier>, path: String, parent: Option<&Arc<CgDir>>, perm: u32) -> Arc<CgDir> {
        Arc::new_cyclic(|me: &Weak<CgDir>| {
            let mut files = BTreeMap::new();
            for (name, val) in h.files(path.is_empty(), parent.map(|p| &**p)) {
                let kind = match name.as_str() {
                    "cgroup.procs" | "cgroup.threads" => Kind::Procs,
                    "tasks" => Kind::Tasks,
                    "cgroup.kill" => Kind::Kill,
                    "cgroup.events" => Kind::Events,
                    _ => Kind::Value,
                };
                let mode = match name.as_str() {
                    "cgroup.kill" | "cgroup.event_control" | "memory.force_empty" => 0o200,
                    n if n.ends_with(".stat") || n == "cgroup.controllers" || n == "cgroup.events" => 0o444,
                    _ => 0o644,
                };
                files.insert(
                    name.into_bytes(),
                    Arc::new(CgFile {
                        meta: Meta::new(h.dev, S_IFREG | mode, 0),
                        kind,
                        dir: me.clone(),
                        value: Mutex::new(val.into_bytes()),
                    }),
                );
            }
            CgDir {
                meta: Meta::new(h.dev, S_IFDIR | perm, 0),
                hier: h.clone(),
                path,
                parent: parent.map(Arc::downgrade),
                children: Mutex::new(BTreeMap::new()),
                files,
            }
        })
    }

    /// Live member processes (pids) of exactly this group.
    fn members(&self) -> Vec<u32> {
        let t = TABLE.lock();
        t.procs
            .iter()
            .filter(|(_, n)| n.zombie.is_none() && n.proc.cgroups.lock().path(self.hier.id) == self.path)
            .map(|(&p, _)| p)
            .collect()
    }

    /// Thread ids of the members (v1 `tasks`).
    fn member_tasks(&self) -> Vec<u32> {
        let t = TABLE.lock();
        let mut v: Vec<u32> = t
            .procs
            .values()
            .filter(|n| n.zombie.is_none() && n.proc.cgroups.lock().path(self.hier.id) == self.path)
            .flat_map(|n| n.threads.iter().copied())
            .collect();
        v.sort_unstable();
        v
    }

    /// This group or one below it has members.
    fn populated(&self) -> bool {
        let prefix = format!("{}/", self.path);
        let t = TABLE.lock();
        t.procs.values().any(|n| {
            if n.zombie.is_some() {
                return false;
            }
            let m = n.proc.cgroups.lock();
            let p = m.path(self.hier.id);
            p == self.path || self.path.is_empty() || p.starts_with(&prefix)
        })
    }
}

/// Move the process owning `id` (a pid or a tid; 0 = the caller) into
/// group `dir`.
fn attach(dir: &CgDir, id: u32) -> KResult<()> {
    let proc = if id == 0 {
        crate::process::current().process.clone()
    } else {
        let t = TABLE.lock();
        match t.procs.get(&id) {
            Some(n) => n.proc.clone(),
            None => t.tasks.get(&id).map(|task| task.process.clone()).ok_or(ESRCH)?,
        }
    };
    let mut m = proc.cgroups.lock();
    if dir.path.is_empty() {
        m.0.remove(&dir.hier.id);
    } else {
        m.0.insert(dir.hier.id, dir.path.clone());
    }
    drop(m);
    notify_events();
    Ok(())
}

impl Inode for CgDir {
    fn meta(&self) -> &Meta {
        &self.meta
    }

    fn size(&self) -> u64 {
        0
    }

    fn lookup(&self, name: &[u8]) -> KResult<Arc<dyn Inode>> {
        if let Some(f) = self.files.get(name) {
            return Ok(f.clone());
        }
        self.children.lock().get(name).cloned().map(|d| d as Arc<dyn Inode>).ok_or(ENOENT)
    }

    fn create(&self, name: &[u8], mode: u32, _rdev: u64) -> KResult<Arc<dyn Inode>> {
        if mode & S_IFMT != S_IFDIR {
            return Err(EPERM);
        }
        if self.files.contains_key(name) || name.contains(&b'/') || name.len() > vfs::NAME_MAX {
            return Err(EEXIST);
        }
        let me = self.self_arc().ok_or(ENOENT)?;
        let mut c = self.children.lock();
        if c.contains_key(name) {
            return Err(EEXIST);
        }
        let n = core::str::from_utf8(name).map_err(|_| EINVAL)?;
        let path = if self.path.is_empty() { n.to_string() } else { format!("{}/{n}", self.path) };
        let d = CgDir::new(&self.hier, path, Some(&me), mode & 0o7777);
        c.insert(name.to_vec(), d.clone());
        self.meta.inc_nlink(1);
        Ok(d)
    }

    fn unlink(&self, name: &[u8], dir: bool) -> KResult<()> {
        if self.files.contains_key(name) {
            return Err(EPERM);
        }
        let mut c = self.children.lock();
        let d = c.get(name).ok_or(ENOENT)?;
        if !dir {
            return Err(EISDIR);
        }
        if !d.children.lock().is_empty() || !d.members().is_empty() {
            return Err(EBUSY);
        }
        c.remove(name);
        self.meta.inc_nlink(-1);
        Ok(())
    }

    fn rename(&self, _old: &[u8], _new_dir: &Arc<dyn Inode>, _new: &[u8], _flags: u32) -> KResult<()> {
        Err(EPERM)
    }

    fn entries(&self) -> KResult<Vec<DirEntry>> {
        let mut v: Vec<DirEntry> = self
            .files
            .iter()
            .map(|(n, f)| DirEntry { name: n.clone(), ino: f.meta.ino, dtype: dtype(f.meta.mode()) })
            .collect();
        for (n, d) in self.children.lock().iter() {
            v.push(DirEntry { name: n.clone(), ino: d.meta.ino, dtype: dtype(d.meta.mode()) });
        }
        Ok(v)
    }

    fn mount_options(&self) -> Option<String> {
        Some(self.hier.options())
    }

    fn set_xattr(&self, _name: &[u8], _value: Option<&[u8]>, _flags: u32) -> KResult<()> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl CgDir {
    /// An `Arc` of this group (through its parent, or the hierarchy root).
    fn self_arc(&self) -> Option<Arc<CgDir>> {
        match &self.parent {
            None => self.hier.root.get().cloned(),
            Some(p) => {
                let p = p.upgrade()?;
                let name = self.path.rsplit('/').next()?;
                p.children.lock().get(name.as_bytes()).cloned()
            }
        }
    }
}

impl Inode for CgFile {
    fn meta(&self) -> &Meta {
        &self.meta
    }

    fn truncate(&self, _size: u64) -> KResult<()> {
        Ok(())
    }

    fn open(&self, _flags: u32) -> KResult<Option<alloc::boxed::Box<dyn FileOps>>> {
        let dir = self.dir.upgrade().ok_or(ENOENT)?;
        let text = match self.kind {
            Kind::Procs => dir.members().iter().map(|p| format!("{p}\n")).collect::<String>(),
            Kind::Tasks => dir.member_tasks().iter().map(|p| format!("{p}\n")).collect::<String>(),
            Kind::Kill => String::new(),
            Kind::Events => format!("populated {}\nfrozen 0\n", dir.populated() as u8),
            Kind::Value => {
                let v = self.value.lock();
                let mut s = String::from_utf8_lossy(&v).into_owned();
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s
            }
        };
        let events = self.kind == Kind::Events;
        Ok(Some(alloc::boxed::Box::new(CgOps {
            data: Mutex::new(text.into_bytes()),
            events,
            seen: AtomicU64::new(EVENTS_GEN.load(Ordering::SeqCst)),
        })))
    }

    fn set_xattr(&self, _name: &[u8], _value: Option<&[u8]>, _flags: u32) -> KResult<()> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// cgroup.events notification: a generation bumped (and a wait queue
/// woken) whenever a process exits or changes group. An open events file
/// polls POLLPRI | POLLERR until it is read again after a change, and a
/// read at offset 0 sees the current state (libprocessgroup pread()s the
/// same fd while it waits for a killed group to empty).
static EVENTS_GEN: AtomicU64 = AtomicU64::new(0);
static EVENTS_WQ: Once<Arc<WaitQueue>> = Once::new();

fn events_wq() -> &'static Arc<WaitQueue> {
    EVENTS_WQ.call_once(WaitQueue::new)
}

pub fn notify_events() {
    EVENTS_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(wq) = EVENTS_WQ.get() {
        wq.wake(POLLPRI | POLLERR);
    }
}

struct CgOps {
    data: Mutex<Vec<u8>>,
    events: bool,
    seen: AtomicU64,
}

impl FileOps for CgOps {
    fn read(&self, f: &OpenFile, pos: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut data = self.data.lock();
        if self.events && *pos == 0 {
            self.seen.store(EVENTS_GEN.load(Ordering::SeqCst), Ordering::SeqCst);
            let file = f.inode.as_any().downcast_ref::<CgFile>().ok_or(EINVAL)?;
            let dir = file.dir.upgrade().ok_or(ENODEV)?;
            *data = format!("populated {}\nfrozen 0\n", dir.populated() as u8).into_bytes();
        }
        let start = (*pos as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        *pos += n as u64;
        Ok(n)
    }

    fn write(&self, f: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let file = f.inode.as_any().downcast_ref::<CgFile>().ok_or(EINVAL)?;
        let dir = file.dir.upgrade().ok_or(ENODEV)?;
        let text = core::str::from_utf8(data).map_err(|_| EINVAL)?;
        match file.kind {
            Kind::Procs | Kind::Tasks => {
                for w in text.split_ascii_whitespace() {
                    let id: u32 = w.parse().map_err(|_| EINVAL)?;
                    attach(&dir, id)?;
                }
            }
            Kind::Kill => {
                if text.trim() != "1" {
                    return Err(EINVAL);
                }
                let prefix = format!("{}/", dir.path);
                let victims: Vec<u32> = {
                    let t = TABLE.lock();
                    t.procs
                        .iter()
                        .filter(|(_, n)| {
                            let m = n.proc.cgroups.lock();
                            let p = m.path(dir.hier.id);
                            n.zombie.is_none() && (p == dir.path || p.starts_with(&prefix))
                        })
                        .map(|(&p, _)| p)
                        .collect()
                };
                for p in victims {
                    let _ = signal::send_process(p, signal::SIGKILL, signal::SigInfo::user(signal::SIGKILL, 0x80, 0));
                }
            }
            Kind::Events => return Err(EINVAL),
            Kind::Value => {
                let v = text.trim_end_matches(['\n', '\0']);
                *file.value.lock() = v.as_bytes().to_vec();
            }
        }
        Ok(data.len())
    }

    fn seekable(&self) -> bool {
        true
    }

    fn size(&self, _: &OpenFile) -> u64 {
        self.data.lock().len() as u64
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        if self.events && self.seen.load(Ordering::SeqCst) != EVENTS_GEN.load(Ordering::SeqCst) {
            DEFAULT_POLLMASK | POLLPRI | POLLERR
        } else {
            DEFAULT_POLLMASK
        }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        self.events.then(|| events_wq().clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// /proc/<pid>/cgroup for process `p`.
pub fn proc_cgroup(m: &Membership) -> String {
    let mut s = String::new();
    let hs = HIERS.lock().clone();
    for h in hs.iter().filter(|h| !h.v2()) {
        s.push_str(&format!("{}:{}:/{}\n", h.id, h.controllers.join(","), m.path(h.id)));
    }
    s.push_str(&format!("0::/{}\n", m.path(0)));
    s
}

/// /proc/cgroups.
pub fn proc_cgroups() -> String {
    let hs = HIERS.lock().clone();
    let mut s = String::from("#subsys_name\thierarchy\tnum_cgroups\tenabled\n");
    for c in V1_CONTROLLERS.iter().filter(|c| **c != "schedtune") {
        let h = hs.iter().find(|h| !h.v2() && h.has(c)).map_or(0, |h| h.id);
        s.push_str(&format!("{c}\t{h}\t1\t1\n"));
    }
    s
}
