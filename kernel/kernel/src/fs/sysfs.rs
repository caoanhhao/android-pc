//! sysfs: one shared tmpfs tree that drivers fill in.
//!
//! Devices live below `/sys/devices` like on Linux (PCI functions under
//! `devices/pci0000:00/0000:BB:DD.F`, virtual ones under
//! `devices/virtual/<class>/<name>`), each with a `uevent` file and a
//! `subsystem` symlink; `/sys/class/<class>/<name>`, `/sys/block/<disk>`
//! and `/sys/dev/{block,char}/M:m` are symlinks into that tree.
//!
//! A device's `uevent` file ([`Uevent`]) reads as its KEY=value lines
//! (MAJOR, MINOR, DEVNAME, ...). Writing an action ("add", "change",
//! "remove", ...) to it broadcasts that uevent on NETLINK_KOBJECT_UEVENT
//! (`net::netlink`), which is how ueventd and first-stage init's coldboot
//! discover devices that already exist; registering and removing a device
//! sends "add"/"remove" by itself (hotplug).
//!
//! Besides devices, the tree has the usual mount points (fs/selinux,
//! fs/cgroup, kernel/tracing, ...) and a few kernel attributes.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, Once};

use super::tmpfs::{Data, PseudoFile, TmpInode};
use super::vfs::{Inode, S_IFDIR, S_IFLNK, S_IFREG};
use crate::errno::*;

static ROOT: Once<Arc<TmpInode>> = Once::new();
/// Every device's uevent file, by its sysfs path (without "/sys/").
static DEVICES: Mutex<BTreeMap<String, Arc<Uevent>>> = Mutex::new(BTreeMap::new());

/// The sysfs root (created and populated on first use).
pub fn root() -> Arc<TmpInode> {
    ROOT.call_once(|| {
        let root = TmpInode::new_fs(0o555);
        for d in ["block", "bus", "class", "dev", "devices", "firmware", "fs", "kernel", "module", "power"] {
            let _ = root.add(d.as_bytes(), S_IFDIR | 0o755, 0, None);
        }
        root
    })
    .clone()
}

fn mkdir(dir: &Arc<TmpInode>, name: &[u8]) -> Option<Arc<TmpInode>> {
    match dir.get(name) {
        Some(d) => Some(d),
        None => dir.add(name, S_IFDIR | 0o755, 0, None).ok(),
    }
}

/// The directory `path` (relative to /sys), created with its parents.
pub fn dir(path: &str) -> Option<Arc<TmpInode>> {
    let mut d = root();
    for c in path.split('/').filter(|c| !c.is_empty()) {
        d = mkdir(&d, c.as_bytes())?;
    }
    Some(d)
}

/// Write the attribute file `<dir>/<name>`.
pub fn set(dir_path: &str, name: &str, contents: &str) {
    if let Some(d) = dir(dir_path) {
        let _ = d.replace(name.as_bytes(), S_IFREG | 0o444, 0, Some(Data::Owned(contents.as_bytes().to_vec())));
    }
}

/// A writable attribute file whose writes are kept (and read back).
pub fn set_rw(dir_path: &str, name: &str, contents: &str) {
    if let Some(d) = dir(dir_path) {
        let _ = d.replace(name.as_bytes(), S_IFREG | 0o644, 0, Some(Data::Owned(contents.as_bytes().to_vec())));
    }
}

pub fn symlink(dir_path: &str, name: &str, target: &str) {
    if let Some(d) = dir(dir_path) {
        let _ = d.replace(name.as_bytes(), S_IFLNK | 0o777, 0, Some(Data::Owned(target.as_bytes().to_vec())));
    }
}

/// Remove a file or a whole directory tree.
pub fn remove(path: &str) {
    let (d, name) = match path.rsplit_once('/') {
        Some((d, n)) => (dir(d), n),
        None => (Some(root()), path),
    };
    if let Some(d) = d {
        remove_tree(&d, name.as_bytes());
    }
    DEVICES.lock().retain(|p, _| p != path && !p.starts_with(&format!("{path}/")));
}

fn remove_tree(dir: &Arc<TmpInode>, name: &[u8]) {
    if let Some(child) = dir.get(name)
        && child.meta().is_dir()
        && let Ok(entries) = child.entries()
    {
        for e in entries {
            remove_tree(&child, &e.name);
        }
    }
    let is_dir = dir.get(name).is_some_and(|c| c.meta().is_dir());
    let _ = dir.unlink(name, is_dir);
}

/// `target` (relative to /sys) as a relative symlink from directory `from`.
pub fn relative(from: &str, target: &str) -> String {
    let depth = from.split('/').filter(|c| !c.is_empty()).count();
    let mut s = "../".repeat(depth);
    s.push_str(target);
    s
}

/// A device's `uevent` file.
pub struct Uevent {
    /// "/devices/..." (DEVPATH).
    pub devpath: String,
    pub subsystem: String,
    vars: Mutex<String>,
}

impl Uevent {
    pub fn vars(&self) -> String {
        self.vars.lock().clone()
    }

    pub fn emit(&self, action: &str) {
        crate::net::netlink::send_uevent(action, &self.devpath, &self.subsystem, &self.vars());
    }
}

const ACTIONS: [&str; 8] = ["add", "remove", "change", "move", "online", "offline", "bind", "unbind"];

impl PseudoFile for Uevent {
    fn read(&self) -> Vec<u8> {
        self.vars().into_bytes()
    }

    fn write(&self, _off: u64, data: &[u8], _: &mut Vec<u8>) -> KResult<usize> {
        let s = core::str::from_utf8(data).map_err(|_| EINVAL)?;
        let action = s.split_ascii_whitespace().next().ok_or(EINVAL)?;
        if !ACTIONS.contains(&action) {
            return Err(EINVAL);
        }
        self.emit(action);
        Ok(data.len())
    }
}

/// Register a device directory `path` (relative to /sys, below devices/)
/// in `subsystem`, whose `subsystem` link points at `subsys_dir` (e.g.
/// "class/block", "bus/pci"). Sends an "add" uevent when `announce`.
pub fn add_device(path: &str, subsystem: &str, subsys_dir: &str, vars: String, announce: bool) -> Arc<Uevent> {
    let u = Arc::new(Uevent { devpath: format!("/{path}"), subsystem: subsystem.into(), vars: Mutex::new(vars) });
    if let Some(d) = dir(path) {
        let _ = d.add_pseudo(b"uevent", 0o644, u.clone());
        let _ = d.replace(b"subsystem", S_IFLNK | 0o777, 0, Some(Data::Owned(relative(path, subsys_dir).into_bytes())));
    }
    let _ = dir(subsys_dir);
    DEVICES.lock().insert(path.into(), u.clone());
    if announce {
        u.emit("add");
    }
    u
}

/// The uevent file of the device at `path`.
pub fn device(path: &str) -> Option<Arc<Uevent>> {
    DEVICES.lock().get(path).cloned()
}

/// Send "remove" for the device at `path` and delete its directory.
pub fn remove_device(path: &str) {
    if let Some(u) = device(path) {
        u.emit("remove");
    }
    remove(path);
}

/// A character device of class `class` at `path`, with the /sys/class
/// and /sys/dev/char links.
pub fn add_char_device(path: &str, class: &str, name: &str, devname: &str, major: u32, minor: u32) {
    let vars = format!("MAJOR={major}\nMINOR={minor}\nDEVNAME={devname}\n");
    add_device(path, class, &format!("class/{class}"), vars, true);
    set(path, "dev", &format!("{major}:{minor}\n"));
    symlink(&format!("class/{class}"), name, &relative(&format!("class/{class}"), path));
    symlink("dev/char", &format!("{major}:{minor}"), &relative("dev/char", path));
}

/// A kernel attribute computed when read.
struct Attr(fn() -> String);

impl PseudoFile for Attr {
    fn read(&self) -> Vec<u8> {
        (self.0)().into_bytes()
    }
}

/// Fill in what exists from boot: mount points, kernel attributes, the
/// memory/tty/misc character devices (so ueventd can create their nodes
/// in a tmpfs /dev), the platform serial port.
pub fn init() {
    for d in [
        "fs/selinux",
        "fs/cgroup",
        "fs/bpf",
        "fs/pstore",
        "fs/fuse/connections",
        "fs/ext4/features",
        "fs/erofs/features",
        "kernel/tracing",
        "kernel/debug",
        "kernel/security",
        "kernel/config",
        "kernel/mm",
        "firmware",
        "module",
        "power",
        "dev/block",
        "dev/char",
        "bus/pci/devices",
        "bus/pci/drivers",
        "bus/virtio/devices",
        "bus/virtio/drivers",
        "bus/platform/devices",
        "bus/platform/drivers",
        "class/block",
        "class/mem",
        "class/tty",
        "class/misc",
        "devices/system/cpu",
        "devices/virtual",
    ] {
        let _ = dir(d);
    }
    if let Some(k) = dir("kernel") {
        let _ = k.add_pseudo(b"uevent_seqnum", 0o444, Arc::new(Attr(|| format!("{}\n", crate::net::netlink::uevent_seqnum()))));
    }
    set_rw("kernel", "uevent_helper", "\n");
    set("fs/ext4/features", "encryption", "supported\n");
    set("fs/ext4/features", "casefold", "supported\n");
    set("fs/erofs/features", "lz4_0padding", "supported\n");
    set_rw("power", "state", "freeze mem\n");
    set_rw("power", "wake_lock", "\n");
    set_rw("power", "wake_unlock", "\n");
    let cpus = crate::arch::x86_64::smp::online_cpus();
    let range = if cpus > 1 { format!("0-{}\n", cpus - 1) } else { String::from("0\n") };
    for f in ["online", "possible", "present"] {
        set("devices/system/cpu", f, &range);
    }
    for (name, minor) in [("null", 3), ("zero", 5), ("full", 7), ("random", 8), ("urandom", 9), ("kmsg", 11)] {
        add_char_device(&format!("devices/virtual/mem/{name}"), "mem", name, name, 1, minor);
    }
    for (name, minor) in [("tty", 0), ("console", 1), ("ptmx", 2)] {
        add_char_device(&format!("devices/virtual/tty/{name}"), "tty", name, name, 5, minor);
    }
    let serial = "devices/platform/serial8250";
    add_device(serial, "platform", "bus/platform", String::from("DRIVER=serial8250\nMODALIAS=platform:serial8250\n"), false);
    symlink("bus/platform/devices", "serial8250", &relative("bus/platform/devices", serial));
    add_char_device(&format!("{serial}/tty/ttyS0"), "tty", "ttyS0", "ttyS0", 4, 64);
    for (name, devname, minor) in [
        ("device-mapper", "mapper/control", crate::block::DM_CONTROL_MINOR),
        ("loop-control", "loop-control", crate::block::LOOP_CONTROL_MINOR),
        ("hw_random", "hw_random", super::dev::HWRNG_MINOR),
    ] {
        add_char_device(&format!("devices/virtual/misc/{name}"), "misc", name, devname, crate::block::MISC_MAJOR, minor);
    }
    // The hw_random class attributes prng_seeder and rngd look at.
    set("devices/virtual/misc/hw_random", "rng_available", "rdrand\n");
    set("devices/virtual/misc/hw_random", "rng_current", "rdrand\n");
}
