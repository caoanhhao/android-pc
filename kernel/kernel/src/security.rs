//! SELinux stub: the state behind selinuxfs (`fs::selinuxfs`) and the
//! per-process security attributes (`/proc/<pid>/attr/*`).
//!
//! There is no policy engine. A policy written to `/sys/fs/selinux/load`
//! is accepted (and its size recorded); every access check answers
//! "allowed", so the system always behaves as permissive even when user
//! space sets enforce=1 (the value is stored and reported back).
//!
//! Contexts are strings kept per process: `current` (what getcon reads),
//! `prev`, and the one-shot `exec`, `fscreate`, `keycreate`, `sockcreate`
//! that libselinux sets before execve / creating files / keys / sockets.
//! execve applies `exec`, or else, for the kernel and init domains, the
//! usual Android domain transition guessed from the program's label
//! ("u:object_r:foo_exec:s0" runs as "u:r:foo:s0"); fork copies all of
//! them. New files and sockets are labelled from `fscreate`/`sockcreate`
//! when set.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::process::TABLE;

pub const KERNEL_CONTEXT: &[u8] = b"u:r:kernel:s0";
pub const XATTR_SELINUX: &[u8] = b"security.selinux";

static ENFORCING: AtomicBool = AtomicBool::new(false);
static POLICY_LOADS: AtomicU32 = AtomicU32::new(0);
/// Bumped on every enforce change or policy load (selinuxfs status page).
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

pub fn enforcing() -> bool {
    ENFORCING.load(Ordering::Relaxed)
}

pub fn set_enforcing(on: bool) {
    if ENFORCING.swap(on, Ordering::Relaxed) != on {
        log::info!("SELinux: enforce={} requested (checks always allow)", on as u8);
        bump();
    }
}

/// A chunk of a policy written to selinuxfs `load` (offset 0 = a new one).
pub fn load_policy(off: u64, len: usize) {
    if off == 0 {
        POLICY_LOADS.fetch_add(1, Ordering::Relaxed);
        log::info!("SELinux: policy loaded (not enforced)");
        bump();
    } else {
        let _ = len;
    }
}

pub fn policy_loads() -> u32 {
    POLICY_LOADS.load(Ordering::Relaxed)
}


pub fn sequence() -> u32 {
    SEQUENCE.load(Ordering::Relaxed)
}

fn bump() {
    // Even while updating, odd while being written: the status page
    // protocol. Updates are whole here, so step by two.
    SEQUENCE.fetch_add(2, Ordering::Relaxed);
    crate::fs::selinuxfs::update_status();
}

/// Per-process security attributes (/proc/<pid>/attr/*).
#[derive(Clone)]
pub struct SecAttrs {
    pub current: Vec<u8>,
    pub prev: Vec<u8>,
    pub exec: Vec<u8>,
    pub fscreate: Vec<u8>,
    pub keycreate: Vec<u8>,
    pub sockcreate: Vec<u8>,
}

impl Default for SecAttrs {
    fn default() -> Self {
        SecAttrs {
            current: KERNEL_CONTEXT.to_vec(),
            prev: KERNEL_CONTEXT.to_vec(),
            exec: Vec::new(),
            fscreate: Vec::new(),
            keycreate: Vec::new(),
            sockcreate: Vec::new(),
        }
    }
}

impl SecAttrs {
    /// execve: switch domain, reset the one-shot attributes. `label` is
    /// the program file's security.selinux xattr.
    pub fn exec(&mut self, label: Option<&[u8]>) {
        let next = if !self.exec.is_empty() {
            Some(core::mem::take(&mut self.exec))
        } else if self.current == KERNEL_CONTEXT || self.current == b"u:r:init:s0" {
            label.and_then(auto_transition)
        } else {
            None
        };
        if let Some(n) = next {
            self.prev = core::mem::replace(&mut self.current, n);
        }
        self.exec.clear();
        self.fscreate.clear();
        self.keycreate.clear();
        self.sockcreate.clear();
    }
}

/// "u:object_r:foo_exec:s0" -> "u:r:foo:s0".
fn auto_transition(label: &[u8]) -> Option<Vec<u8>> {
    let label = label.strip_suffix(b"\0").unwrap_or(label);
    let s = core::str::from_utf8(label).ok()?;
    let mut parts = s.splitn(4, ':');
    let (user, _role, ty, level) = (parts.next()?, parts.next()?, parts.next()?, parts.next().unwrap_or("s0"));
    let domain = ty.strip_suffix("_exec")?;
    Some(alloc::format!("{user}:r:{domain}:{level}").into_bytes())
}

/// Normalise a context written by user space (drop a trailing NUL and
/// newline). An empty write clears the attribute.
pub fn clean_context(data: &[u8]) -> Vec<u8> {
    let mut d = data;
    while let Some((&last, rest)) = d.split_last() {
        if last == 0 || last == b'\n' {
            d = rest;
        } else {
            break;
        }
    }
    d.to_vec()
}

/// The current context of process `pid` (the kernel's for unknown ones).
pub fn context_of_pid(pid: u32) -> Vec<u8> {
    let p = TABLE.lock().procs.get(&pid).map(|n| n.proc.clone());
    p.map_or_else(|| KERNEL_CONTEXT.to_vec(), |p| p.sec.lock().current.clone())
}

/// A valid-looking context: "user:role:type[:level]".
pub fn valid_context(c: &[u8]) -> bool {
    let c = c.strip_suffix(b"\0").unwrap_or(c);
    !c.is_empty() && c.iter().filter(|&&b| b == b':').count() >= 2 && c.iter().all(|&b| b.is_ascii_graphic())
}

/// "allowed decided auditallow auditdeny seqno flags" for an access check.
pub fn access_reply() -> String {
    alloc::format!("ffffffff ffffffff 0 ffffffff {} 0", policy_loads())
}

/// The label of an inode that has none stored (Linux would give it the
/// file system's initial SID): reads of security.selinux never fail.
pub const UNLABELED: &[u8] = b"u:object_r:unlabeled:s0\0";

/// Label a new node `name` of `parent` that got no label from fscreate
/// the way the policy's genfscon rules would (bpffs).
pub fn label_genfs(parent: &dyn crate::fs::vfs::Inode, name: &[u8], inode: &dyn crate::fs::vfs::Inode) {
    if inode.xattrs().is_ok_and(|x| x.iter().any(|(n, _)| n == XATTR_SELINUX)) {
        return;
    }
    if let Some(l) = crate::bpf::genfs_label(parent, name) {
        let _ = inode.set_xattr(XATTR_SELINUX, Some(&l), 0);
    }
}

/// Label a newly created inode with the creator's fscreate context.
pub fn label_new_inode(inode: &dyn crate::fs::vfs::Inode, sock: bool) {
    // Kernel-internal creations (in-kernel tests) have no process.
    let Some(p) = crate::sched::try_current() else { return };
    let ctx = {
        let s = p.process.sec.lock();
        if sock && !s.sockcreate.is_empty() { s.sockcreate.clone() } else { s.fscreate.clone() }
    };
    if !ctx.is_empty() {
        let mut v = ctx;
        v.push(0);
        let _ = inode.set_xattr(XATTR_SELINUX, Some(&v), 0);
    }
}
