//! selinuxfs (`mount -t selinuxfs selinuxfs /sys/fs/selinux`): the
//! interface libselinux and init expect, over the stub in
//! `crate::security` (policy accepted, every check allowed).
//!
//! | node                         | behaviour                                  |
//! |------------------------------|--------------------------------------------|
//! | enforce                      | "0"/"1"; writes are stored                 |
//! | load                         | a write is a policy: accepted, size noted  |
//! | context                      | write a context, read it back (validity)   |
//! | access                       | "scon tcon class perms" -> all allowed     |
//! | create, relabel, member      | -> the target context                      |
//! | user                         | -> no reachable contexts ("0")             |
//! | status                       | mmap-able selinux_kernel_status page       |
//! | deny_unknown, reject_unknown | "0": unknown classes/perms are allowed     |
//! | checkreqprot, mls, policyvers | "0", "1", "33"                            |
//! | null                         | a null device (1:3)                        |
//! | initial_contexts/*           | kernel = u:r:kernel:s0, others unlabeled   |
//! | booleans/, class/, policy_capabilities/, avc/ | empty / stats     |
//!
//! `class/` is empty, so libselinux's selinux_check_access finds no class
//! and, with deny_unknown = 0, allows (logging "Unknown class") without
//! asking `access`. One instance is shared by every mount, like Linux's.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Once;

use super::tmpfs::{Data, PseudoFile, TmpInode};
use super::vfs::{Dentry, S_IFCHR, S_IFDIR, S_IFREG, encode_dev};
use crate::errno::*;
use crate::security;

pub const SELINUX_MAGIC: u64 = 0xf97c_ff8c;

static ROOT: Once<Arc<TmpInode>> = Once::new();
static DENTRY: Once<Arc<Dentry>> = Once::new();
static STATUS: Once<Arc<TmpInode>> = Once::new();

type ReadFn = fn() -> Vec<u8>;
type WriteFn = fn(u64, &[u8], &mut Vec<u8>) -> KResult<usize>;

/// A node: what reads see, what a write does.
struct Node(ReadFn, Option<WriteFn>);

impl PseudoFile for Node {
    fn read(&self) -> Vec<u8> {
        (self.0)()
    }

    fn write(&self, off: u64, data: &[u8], reply: &mut Vec<u8>) -> KResult<usize> {
        match self.1 {
            Some(w) => w(off, data, reply),
            None => Err(EINVAL),
        }
    }
}

fn empty() -> Vec<u8> {
    Vec::new()
}

/// Space-separated words of a transaction write.
fn words(data: &[u8]) -> Vec<&[u8]> {
    data.split(|&c| c == b' ' || c == b'\n' || c == 0).filter(|w| !w.is_empty()).collect()
}

fn w_enforce(_off: u64, data: &[u8], _: &mut Vec<u8>) -> KResult<usize> {
    let s = core::str::from_utf8(data).map_err(|_| EINVAL)?.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let v: i64 = s.parse().map_err(|_| EINVAL)?;
    security::set_enforcing(v != 0);
    Ok(data.len())
}

/// A policy arrives in chunks; the one at offset 0 starts a new load.
fn w_load(off: u64, data: &[u8], _: &mut Vec<u8>) -> KResult<usize> {
    if off == 0 && data.len() < 8 {
        return Err(EINVAL);
    }
    security::load_policy(off, data.len());
    Ok(data.len())
}

fn w_context(_off: u64, data: &[u8], reply: &mut Vec<u8>) -> KResult<usize> {
    let c = security::clean_context(data);
    if !security::valid_context(&c) {
        return Err(EINVAL);
    }
    *reply = c;
    reply.push(0);
    Ok(data.len())
}

fn w_access(_off: u64, data: &[u8], reply: &mut Vec<u8>) -> KResult<usize> {
    if words(data).len() < 3 {
        return Err(EINVAL);
    }
    *reply = security::access_reply().into_bytes();
    Ok(data.len())
}

/// create / relabel / member: answer with the target context.
fn w_compute(_off: u64, data: &[u8], reply: &mut Vec<u8>) -> KResult<usize> {
    let w = words(data);
    if w.len() < 3 {
        return Err(EINVAL);
    }
    *reply = w[1].to_vec();
    reply.push(0);
    Ok(data.len())
}

fn w_user(_off: u64, data: &[u8], reply: &mut Vec<u8>) -> KResult<usize> {
    if words(data).len() < 2 {
        return Err(EINVAL);
    }
    *reply = b"0\0".to_vec();
    Ok(data.len())
}

fn w_accept(_off: u64, data: &[u8], _: &mut Vec<u8>) -> KResult<usize> {
    Ok(data.len())
}

/// struct selinux_kernel_status: version, sequence, enforcing,
/// policyload, deny_unknown (u32 each).
fn status_bytes() -> Vec<u8> {
    let mut b = alloc::vec![0u8; 4096];
    let fields = [1, security::sequence(), security::enforcing() as u32, security::policy_loads(), 0];
    for (i, f) in fields.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    b
}

/// Rewrite the status page (mapped by libselinux's selinux_status_open).
pub fn update_status() {
    if let Some(s) = STATUS.get()
        && let Some(obj) = super::vfs::Inode::shmem(&**s)
    {
        let _ = obj.write(0, &status_bytes()[..20]);
    }
}

fn root() -> Arc<TmpInode> {
    ROOT.call_once(|| {
        let r = TmpInode::new_fs(0o755);
        let rw = |name: &[u8], read: ReadFn, write: WriteFn, perm: u32| {
            let _ = r.add_pseudo(name, perm, Arc::new(Node(read, Some(write))));
        };
        rw(b"enforce", || alloc::format!("{}", security::enforcing() as u8).into_bytes(), w_enforce, 0o644);
        rw(b"load", empty, w_load, 0o600);
        rw(b"context", empty, w_context, 0o666);
        rw(b"access", empty, w_access, 0o666);
        rw(b"create", empty, w_compute, 0o666);
        rw(b"relabel", empty, w_compute, 0o666);
        rw(b"member", empty, w_compute, 0o666);
        rw(b"user", empty, w_user, 0o666);
        rw(b"checkreqprot", || b"0".to_vec(), w_accept, 0o644);
        rw(b"commit_pending_bools", empty, w_accept, 0o200);
        let ro = |name: &[u8], read: ReadFn| {
            let _ = r.add_pseudo(name, 0o444, Arc::new(Node(read, None)));
        };
        ro(b"deny_unknown", || b"0".to_vec());
        ro(b"reject_unknown", || b"0".to_vec());
        ro(b"mls", || b"1".to_vec());
        ro(b"policyvers", || b"33".to_vec());
        ro(b"policy", empty);
        if let Ok(s) = r.add(b"status", S_IFREG | 0o444, 0, Some(Data::Owned(status_bytes()))) {
            STATUS.call_once(|| s);
        }
        let _ = r.add(b"null", S_IFCHR | 0o666, encode_dev(1, 3), None);
        if let Ok(ic) = r.add(b"initial_contexts", S_IFDIR | 0o555, 0, None) {
            for name in ["kernel", "security", "unlabeled", "file", "init", "devnull", "sysctl", "proc", "any_socket"] {
                let ctx: &[u8] = if name == "kernel" { security::KERNEL_CONTEXT } else { b"u:object_r:unlabeled:s0" };
                let _ = ic.add(name.as_bytes(), S_IFREG | 0o444, 0, Some(Data::Owned(ctx.to_vec())));
            }
        }
        for d in ["booleans", "class", "policy_capabilities"] {
            let _ = r.add(d.as_bytes(), S_IFDIR | 0o555, 0, None);
        }
        if let Ok(avc) = r.add(b"avc", S_IFDIR | 0o555, 0, None) {
            let _ = avc.add(b"cache_threshold", S_IFREG | 0o644, 0, Some(Data::Owned(b"512".to_vec())));
            let _ = avc.add(b"cache_stats", S_IFREG | 0o444, 0, Some(Data::Owned(b"lookups hits misses allocations reclaims frees\n".to_vec())));
        }
        r
    })
    .clone()
}

/// The shared selinuxfs root dentry.
pub fn mount_root() -> Arc<Dentry> {
    DENTRY.call_once(|| Dentry::new_root(root())).clone()
}
