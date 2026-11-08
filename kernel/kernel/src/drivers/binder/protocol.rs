//! BINDER_WRITE_READ: the BC_* commands of the write buffer, transactions
//! (buffer allocation in the receiver and object translation) and the
//! BR_* stream of the read buffer.

use alloc::vec;
use alloc::vec::Vec;

use super::abi::*;
use super::state::*;
use crate::errno::*;
use crate::mm::user::AddressSpace;
use crate::process::current;
use crate::sched::{self, Wake, WaitMode};

/// Largest transaction (data + offsets + scatter-gather buffers): the
/// whole buffer area.
const MAX_TXN: u64 = super::MAX_MAP;
/// Security context reported with BR_TRANSACTION_SEC_CTX (no SELinux yet).
const SECCTX: &[u8] = b"u:r:kernel:s0\0";

/// binder_transaction_data as sent by user space.
struct TrData {
    target: u64,
    code: u32,
    flags: u32,
    data_size: u64,
    offsets_size: u64,
    buffer: u64,
    offsets: u64,
}

impl TrData {
    fn parse(a: &[u8]) -> TrData {
        TrData {
            target: rd64(a, 0),
            code: rd32(a, 16),
            flags: rd32(a, 20),
            data_size: rd64(a, 32),
            offsets_size: rd64(a, 40),
            buffer: rd64(a, 48),
            offsets: rd64(a, 56),
        }
    }
}

// ---------------------------------------------------------------------------
// Write side

/// Run the commands in `[wb + *wc, wb + ws)`, advancing `*wc` past each.
pub fn write(p: Id, tid: u32, mm: &AddressSpace, wb: u64, ws: u64, wc: &mut u64) -> KResult<()> {
    if *wc >= ws {
        return Ok(());
    }
    let len = ws - *wc;
    if len > MAX_TXN {
        return Err(EINVAL);
    }
    let mut buf = Vec::new();
    buf.try_reserve_exact(len as usize).map_err(|_| ENOMEM)?;
    buf.resize(len as usize, 0);
    mm.copy_in(wb + *wc, &mut buf)?;
    let mut pos = 0;
    while pos < buf.len() {
        if lock().thread(p, tid).error_pending {
            break;
        }
        if pos + 4 > buf.len() {
            return Err(EINVAL);
        }
        let cmd = rd32(&buf, pos);
        let end = pos + 4 + ioc_size(cmd);
        if end > buf.len() {
            return Err(EINVAL);
        }
        command(p, tid, mm, cmd, &buf[pos + 4..end])?;
        *wc += (end - pos) as u64;
        pos = end;
    }
    Ok(())
}

fn command(p: Id, tid: u32, mm: &AddressSpace, cmd: u32, a: &[u8]) -> KResult<()> {
    match cmd {
        BC_TRANSACTION | BC_REPLY | BC_TRANSACTION_SG | BC_REPLY_SG => {
            let tr = TrData::parse(a);
            let sg = if cmd == BC_TRANSACTION_SG || cmd == BC_REPLY_SG { rd64(a, 64) } else { 0 };
            let reply = cmd == BC_REPLY || cmd == BC_REPLY_SG;
            let prep = prepare(mm, &tr, sg);
            transaction(&mut lock(), p, tid, &tr, prep, reply);
        }
        BC_INCREFS | BC_ACQUIRE | BC_RELEASE | BC_DECREFS => {
            let desc = rd32(a, 0);
            let strong = cmd == BC_ACQUIRE || cmd == BC_RELEASE;
            let inc = cmd == BC_INCREFS || cmd == BC_ACQUIRE;
            let mut s = lock();
            if inc && desc == 0 && !s.proc(p).refs.contains_key(&0) {
                let ctx = s.proc(p).ctx;
                match s.mgr.get(&ctx).copied() {
                    Some(m) if s.nodes.get(&m).and_then(|n| n.owner) != Some(p) => {
                        s.ref_for_node(p, m);
                    }
                    _ => {
                        log::debug!("binder: {} on handle 0 without a context manager", cmd_name(cmd));
                        return Ok(());
                    }
                }
            }
            let r = if inc { s.inc_ref(p, desc, strong, None) } else { s.dec_ref(p, desc, strong) };
            if r.is_err() {
                log::debug!("binder: pid {} {} on bad handle {}", s.proc(p).pid, cmd_name(cmd), desc);
            }
        }
        BC_INCREFS_DONE | BC_ACQUIRE_DONE => {
            let (ptr, cookie) = (rd64(a, 0), rd64(a, 8));
            let strong = cmd == BC_ACQUIRE_DONE;
            let mut s = lock();
            let Some(&n) = s.proc(p).nodes.get(&ptr) else {
                log::debug!("binder: {} for unknown node {:#x}", cmd_name(cmd), ptr);
                return Ok(());
            };
            let node = s.nodes.get_mut(&n).unwrap();
            let pending = if strong { &mut node.pending_strong } else { &mut node.pending_weak };
            if node.cookie != cookie || !*pending {
                log::debug!("binder: {} for node {:#x} not pending", cmd_name(cmd), ptr);
                return Ok(());
            }
            *pending = false;
            s.dec_node(n, strong, false);
        }
        BC_FREE_BUFFER => {
            let ptr = rd64(a, 0);
            let mut s = lock();
            let Some(m) = s.proc(p).map.as_ref() else { return Ok(()) };
            let off = ptr.wrapping_sub(m.base);
            if !m.used.get(&off).is_some_and(|b| b.user_free) {
                log::debug!("binder: BC_FREE_BUFFER {:#x}: no such buffer", ptr);
                return Ok(());
            }
            s.release_buffer(p, off, true);
        }
        BC_REGISTER_LOOPER => {
            let mut s = lock();
            let pr = s.proc(p);
            let ok = pr.requested > 0;
            if ok {
                pr.requested -= 1;
                pr.started += 1;
            }
            let th = s.thread(p, tid);
            if th.looper & LOOPER_ENTERED != 0 || !ok {
                th.looper |= LOOPER_INVALID;
            }
            th.looper |= LOOPER_REGISTERED;
        }
        BC_ENTER_LOOPER => {
            let mut s = lock();
            let th = s.thread(p, tid);
            if th.looper & LOOPER_REGISTERED != 0 {
                th.looper |= LOOPER_INVALID;
            }
            th.looper |= LOOPER_ENTERED;
        }
        BC_EXIT_LOOPER => lock().thread(p, tid).looper |= LOOPER_EXITED,
        BC_REQUEST_DEATH_NOTIFICATION | BC_CLEAR_DEATH_NOTIFICATION => {
            let (handle, cookie) = (rd32(a, 0), rd64(a, 4));
            death_command(&mut lock(), p, tid, handle, cookie, cmd == BC_REQUEST_DEATH_NOTIFICATION);
        }
        BC_DEAD_BINDER_DONE => {
            let cookie = rd64(a, 0);
            let mut s = lock();
            let st = &mut *s;
            let found = st.procs[&p].delivered.iter().position(|d| st.deaths.get(d).is_some_and(|dt| dt.cookie == cookie));
            let Some(i) = found else { return Ok(()) };
            let d = s.proc(p).delivered.remove(i);
            let dt = s.deaths.get_mut(&d).unwrap();
            dt.delivered = false;
            if dt.kind == DeathKind::DeadAndClear {
                dt.kind = DeathKind::Clear;
                let looper = s.thread(p, tid).looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0;
                s.queue_death(d, looper.then_some(tid));
            }
        }
        // Never implemented by Linux either.
        BC_ATTEMPT_ACQUIRE | BC_ACQUIRE_RESULT => return Err(EINVAL),
        // Freeze notifications are not supported (no freezer).
        _ => return Err(EINVAL),
    }
    Ok(())
}

fn death_command(s: &mut State, p: Id, tid: u32, handle: u32, cookie: u64, request: bool) {
    let Some(r) = s.proc(p).refs.get(&handle) else {
        log::debug!("binder: death notification on bad handle {handle}");
        return;
    };
    let (node, death) = (r.node, r.death);
    if request {
        if death.is_some() {
            return;
        }
        let d = s.next_id();
        s.deaths.insert(d, Death { proc: p, cookie, kind: DeathKind::Dead, queued: false, delivered: false });
        s.proc(p).refs.get_mut(&handle).unwrap().death = Some(d);
        if s.nodes.get(&node).is_none_or(|n| n.owner.is_none()) {
            s.queue_death(d, None);
        }
        return;
    }
    let Some(d) = death.filter(|d| s.deaths.get(d).is_some_and(|dt| dt.cookie == cookie)) else {
        log::debug!("binder: clear death notification: no match for handle {handle}");
        return;
    };
    s.proc(p).refs.get_mut(&handle).unwrap().death = None;
    let dt = s.deaths.get_mut(&d).unwrap();
    if !dt.queued && !dt.delivered {
        dt.kind = DeathKind::Clear;
        let looper = s.thread(p, tid).looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0;
        s.queue_death(d, looper.then_some(tid));
    } else {
        dt.kind = DeathKind::DeadAndClear;
    }
}

fn cmd_name(cmd: u32) -> &'static str {
    match cmd {
        BC_INCREFS => "BC_INCREFS",
        BC_ACQUIRE => "BC_ACQUIRE",
        BC_RELEASE => "BC_RELEASE",
        BC_DECREFS => "BC_DECREFS",
        BC_INCREFS_DONE => "BC_INCREFS_DONE",
        BC_ACQUIRE_DONE => "BC_ACQUIRE_DONE",
        _ => "BC_?",
    }
}

// ---------------------------------------------------------------------------
// Transactions

#[derive(Clone)]
enum Kind {
    Binder { weak: bool },
    Handle { weak: bool },
    Fd,
    /// Offsets of the fd slots (inside the parent buffer's copy).
    Fda(Vec<usize>),
    /// Copy at `sg` in the transaction buffer; `parent` = (object index,
    /// offset of the pointer to patch in the parent's copy).
    Ptr { sg: usize, len: usize, parent: Option<(usize, usize)> },
}

struct Obj {
    off: usize,
    kind: Kind,
}

/// A transaction copied in from the sender, objects validated, before the
/// receiver is known to have room for it.
struct Prepared {
    kbuf: Vec<u8>,
    data_size: u64,
    offsets_size: u64,
    objs: Vec<Obj>,
}

/// Copy the data, the offsets and the scatter-gather buffers in and
/// validate the object layout. No lock is held.
fn prepare(mm: &AddressSpace, tr: &TrData, sg_size: u64) -> KResult<Prepared> {
    if !tr.offsets_size.is_multiple_of(8) || !sg_size.is_multiple_of(8) {
        return Err(EINVAL);
    }
    let ds = align8(tr.data_size.min(MAX_TXN + 1)) as usize;
    let os = align8(tr.offsets_size.min(MAX_TXN + 1)) as usize;
    let total = (ds as u64).saturating_add(os as u64).saturating_add(sg_size);
    if total > MAX_TXN {
        return Err(ENOSPC);
    }
    let mut kbuf = Vec::new();
    kbuf.try_reserve_exact(total as usize).map_err(|_| ENOMEM)?;
    kbuf.resize(total as usize, 0);
    let (data_size, offsets_size) = (tr.data_size as usize, tr.offsets_size as usize);
    mm.copy_in(tr.buffer, &mut kbuf[..data_size])?;
    mm.copy_in(tr.offsets, &mut kbuf[ds..ds + offsets_size])?;
    let mut sg = ds + os;
    let mut min_off = 0;
    let mut objs: Vec<Obj> = Vec::new();
    for i in 0..offsets_size / 8 {
        let off = rd64(&kbuf, ds + i * 8) as usize;
        if !off.is_multiple_of(4) || off < min_off || off.saturating_add(8) > data_size {
            return Err(EINVAL);
        }
        let ty = rd32(&kbuf, off);
        let size = match ty {
            BINDER_TYPE_BINDER | BINDER_TYPE_WEAK_BINDER | BINDER_TYPE_HANDLE | BINDER_TYPE_WEAK_HANDLE | BINDER_TYPE_FD => 24,
            BINDER_TYPE_FDA => 32,
            BINDER_TYPE_PTR => 40,
            _ => return Err(EINVAL),
        };
        if off + size > data_size {
            return Err(EINVAL);
        }
        min_off = off + size;
        // The parent of an fd array or a buffer: an earlier buffer object.
        let parent = |idx: u64| -> KResult<(usize, usize)> {
            match objs.get(idx as usize).map(|o| &o.kind) {
                Some(Kind::Ptr { sg, len, .. }) => Ok((*sg, *len)),
                _ => Err(EINVAL),
            }
        };
        let kind = match ty {
            BINDER_TYPE_BINDER => Kind::Binder { weak: false },
            BINDER_TYPE_WEAK_BINDER => Kind::Binder { weak: true },
            BINDER_TYPE_HANDLE => Kind::Handle { weak: false },
            BINDER_TYPE_WEAK_HANDLE => Kind::Handle { weak: true },
            BINDER_TYPE_FD => Kind::Fd,
            BINDER_TYPE_FDA => {
                let (num, pidx, poff) = (rd64(&kbuf, off + 8), rd64(&kbuf, off + 16), rd64(&kbuf, off + 24));
                let (psg, plen) = parent(pidx)?;
                let end = num.checked_mul(4).and_then(|n| n.checked_add(poff)).ok_or(EINVAL)?;
                if !poff.is_multiple_of(4) || end > plen as u64 {
                    return Err(EINVAL);
                }
                Kind::Fda((0..num as usize).map(|j| psg + poff as usize + j * 4).collect())
            }
            _ => {
                let (flags, ubuf, len) = (rd32(&kbuf, off + 4), rd64(&kbuf, off + 8), rd64(&kbuf, off + 16));
                if align8(len.min(MAX_TXN + 1)) > (kbuf.len() - sg) as u64 {
                    return Err(EINVAL);
                }
                let len = len as usize;
                mm.copy_in(ubuf, &mut kbuf[sg..sg + len])?;
                let par = if flags & BINDER_BUFFER_FLAG_HAS_PARENT != 0 {
                    let (pidx, poff) = (rd64(&kbuf, off + 24), rd64(&kbuf, off + 32));
                    let (_, plen) = parent(pidx)?;
                    if poff.checked_add(8).is_none_or(|e| e > plen as u64) {
                        return Err(EINVAL);
                    }
                    Some((pidx as usize, poff as usize))
                } else {
                    None
                };
                let k = Kind::Ptr { sg, len, parent: par };
                sg += align8(len as u64) as usize;
                k
            }
        };
        objs.push(Obj { off, kind });
    }
    Ok(Prepared { kbuf, data_size: tr.data_size, offsets_size: tr.offsets_size, objs })
}

/// BC_TRANSACTION / BC_REPLY. Failures are reported to user space as
/// BR_* codes, never as ioctl errors.
fn transaction(s: &mut State, p: Id, tid: u32, tr: &TrData, prep: KResult<Prepared>, reply: bool) {
    let debug_id = s.next_id();
    s.thread(p, tid);
    let mut in_reply_to = None;
    match transact(s, p, tid, tr, prep, reply, &mut in_reply_to) {
        Ok(()) => s.thread(p, tid).ee = (debug_id as u32, BR_OK, 0),
        Err((cmd, e)) => {
            log::debug!("binder: pid {} tid {tid} {} failed: {cmd:#x} errno {}", s.proc(p).pid, if reply { "reply" } else { "transaction" }, e.0);
            s.thread(p, tid).ee = (debug_id as u32, cmd, -e.0);
            match in_reply_to {
                Some(t) => {
                    // The replier is done; the caller gets the error.
                    s.queue_thread(p, tid, Work::Complete, false);
                    s.send_failed_reply(t, cmd);
                }
                None => s.queue_thread(p, tid, Work::ReturnError(cmd), false),
            }
        }
    }
}

fn transact(s: &mut State, p: Id, tid: u32, tr: &TrData, prep: KResult<Prepared>, reply: bool, in_reply_to: &mut Option<Id>) -> Result<(), (u32, Errno)> {
    let failed = |e| (BR_FAILED_REPLY, e);
    let dead = |e| (BR_DEAD_REPLY, e);
    let oneway = !reply && tr.flags & TF_ONE_WAY != 0;
    let (target_proc, mut target_thread, node);
    if reply {
        let t = s.thread(p, tid).stack.ok_or(failed(EPROTO))?;
        let tx = s.txns.get(&t).ok_or(failed(EPROTO))?;
        if tx.to_proc != p || tx.to_thread != Some(tid) {
            return Err(failed(EPROTO));
        }
        let (to_parent, from) = (tx.to_parent, tx.from);
        s.thread(p, tid).stack = to_parent;
        *in_reply_to = Some(t);
        let (fp, ftid) = from.ok_or(dead(ESRCH))?;
        let fstack = s.thread_opt(fp, ftid).ok_or(dead(ESRCH))?.stack;
        if fstack != Some(t) {
            return Err(failed(EPROTO));
        }
        (target_proc, target_thread, node) = (fp, Some(ftid), None);
    } else {
        let ctx = s.proc(p).ctx;
        let n = if tr.target as u32 == 0 {
            *s.mgr.get(&ctx).ok_or(dead(EINVAL))?
        } else {
            s.proc(p).refs.get(&(tr.target as u32)).ok_or(failed(EINVAL))?.node
        };
        let owner = s.nodes.get(&n).and_then(|x| x.owner).ok_or(dead(EINVAL))?;
        if owner == p {
            return Err(failed(EINVAL));
        }
        (target_proc, target_thread, node) = (owner, None, Some(n));
        // A nested call goes to the thread of the target process that is
        // waiting for this thread (directly or further down the chain).
        if !oneway && let Some(top) = s.thread(p, tid).stack {
            if s.txns.get(&top).is_none_or(|x| x.to_thread != Some(tid)) {
                return Err(failed(EPROTO));
            }
            let mut cur = Some(top);
            while let Some(c) = cur {
                let Some(x) = s.txns.get(&c) else { break };
                if let Some((fp, ftid)) = x.from
                    && fp == target_proc
                {
                    target_thread = Some(ftid);
                    break;
                }
                cur = x.from_parent;
            }
        }
    }
    let prep = prep.map_err(failed)?;
    let allow_fds = match in_reply_to {
        Some(t) => s.txns.get(t).is_some_and(|x| x.flags & TF_ACCEPT_FDS != 0),
        None => node.and_then(|n| s.nodes.get(&n)).is_some_and(|n| n.accept_fds),
    };
    let sec = node.and_then(|n| s.nodes.get(&n)).is_some_and(|n| n.sec_ctx);
    let data_len = prep.kbuf.len() as u64;
    let size = align8(data_len + if sec { SECCTX.len() as u64 } else { 0 }).max(8);

    let m = s.procs.get_mut(&target_proc).ok_or(dead(ESRCH))?.map.as_mut().ok_or(failed(ESRCH))?;
    if oneway && m.async_free < size {
        return Err(failed(ENOSPC));
    }
    let off = m.alloc(size).ok_or(failed(ENOSPC))?;
    if oneway {
        m.async_free -= size;
    }
    let (base, shmem) = (m.base, m.shmem.clone());
    m.used.insert(
        off,
        Buffer {
            size,
            data_size: prep.data_size,
            offsets_size: prep.offsets_size,
            txn: None,
            user_free: false,
            oneway,
            async_node: if oneway { node } else { None },
            held: Vec::new(),
            fds: Vec::new(),
            fda_fds: Vec::new(),
            secctx: None,
        },
    );
    let mut kbuf = prep.kbuf;
    let fail = |s: &mut State, e: Errno| {
        s.release_buffer(target_proc, off, false);
        failed(e)
    };
    if let Err(e) = translate(s, p, tid, target_proc, off, base, &mut kbuf, &prep.objs, allow_fds) {
        return Err(fail(s, e));
    }
    if sec {
        s.buffer(target_proc, off).unwrap().secctx = Some(data_len);
        kbuf.extend_from_slice(SECCTX);
    }
    if shmem.write(off, &kbuf).is_err() {
        return Err(fail(s, ENOMEM));
    }

    let t = s.next_id();
    let sender_pid = s.proc(p).pid;
    let sender_euid = crate::process::current_creds().euid;
    s.txns.insert(
        t,
        Txn {
            from: (!reply && !oneway).then_some((p, tid)),
            sender_pid,
            sender_euid,
            to_proc: target_proc,
            to_thread: None,
            from_parent: None,
            to_parent: None,
            code: tr.code,
            flags: tr.flags,
            reply,
            node,
            buf: Some(off),
        },
    );
    s.buffer(target_proc, off).unwrap().txn = Some(t);
    s.queue_thread(p, tid, Work::Complete, false);
    if reply {
        let irt = in_reply_to.take().unwrap();
        let ftid = target_thread.unwrap();
        let from_parent = s.txns.get(&irt).and_then(|x| x.from_parent);
        s.thread(target_proc, ftid).stack = from_parent;
        s.free_txn(irt);
        s.queue_thread(target_proc, ftid, Work::Txn(t), true);
    } else if !oneway {
        let th = s.thread(p, tid);
        let parent = th.stack.replace(t);
        s.txns.get_mut(&t).unwrap().from_parent = parent;
        match target_thread {
            Some(tt) => s.queue_thread(target_proc, tt, Work::Txn(t), true),
            None => s.queue_proc(target_proc, Work::Txn(t)),
        }
    } else {
        let n = s.nodes.get_mut(&node.unwrap()).unwrap();
        if n.has_async {
            n.async_todo.push_back(t);
        } else {
            n.has_async = true;
            s.queue_proc(target_proc, Work::Txn(t));
        }
    }
    Ok(())
}

/// Rewrite the objects for the receiver `tp` (buffer `off` at user
/// address `base + off`), taking the references the buffer will hold.
#[allow(clippy::too_many_arguments)]
fn translate(s: &mut State, p: Id, tid: u32, tp: Id, off: u64, base: u64, kbuf: &mut [u8], objs: &[Obj], allow_fds: bool) -> KResult<()> {
    let files = current().process.files();
    for o in objs {
        let at = o.off;
        match &o.kind {
            &Kind::Binder { weak } => {
                let (flags, ptr, cookie) = (rd32(kbuf, at + 4), rd64(kbuf, at + 8), rd64(kbuf, at + 16));
                let n = match s.proc(p).nodes.get(&ptr) {
                    Some(&n) => {
                        if s.nodes[&n].cookie != cookie {
                            return Err(EINVAL);
                        }
                        n
                    }
                    None => s.new_node(p, ptr, cookie, flags),
                };
                let d = s.ref_for_node(tp, n);
                s.inc_ref(tp, d, !weak, Some((p, tid)))?;
                s.buffer(tp, off).unwrap().held.push(Held::Ref { desc: d, strong: !weak });
                wr32(kbuf, at, if weak { BINDER_TYPE_WEAK_HANDLE } else { BINDER_TYPE_HANDLE });
                wr64(kbuf, at + 8, d as u64);
                wr64(kbuf, at + 16, 0);
            }
            &Kind::Handle { weak } => {
                let h = rd32(kbuf, at + 8);
                let n = s.proc(p).refs.get(&h).ok_or(EINVAL)?.node;
                let node = s.nodes.get(&n).ok_or(EINVAL)?;
                if node.owner == Some(tp) {
                    let (ptr, cookie) = (node.ptr, node.cookie);
                    s.inc_node(n, !weak, false, None);
                    s.buffer(tp, off).unwrap().held.push(Held::Node { node: n, strong: !weak });
                    wr32(kbuf, at, if weak { BINDER_TYPE_WEAK_BINDER } else { BINDER_TYPE_BINDER });
                    wr64(kbuf, at + 8, ptr);
                    wr64(kbuf, at + 16, cookie);
                } else {
                    let d = s.ref_for_node(tp, n);
                    s.inc_ref(tp, d, !weak, None)?;
                    s.buffer(tp, off).unwrap().held.push(Held::Ref { desc: d, strong: !weak });
                    wr64(kbuf, at + 8, d as u64);
                    wr64(kbuf, at + 16, 0);
                }
            }
            Kind::Fd => {
                if !allow_fds {
                    return Err(EPERM);
                }
                let file = files.get(rd32(kbuf, at + 8) as i32)?;
                s.buffer(tp, off).unwrap().fds.push(FdFixup { off: (at + 8) as u64, file, fda: false });
            }
            Kind::Fda(slots) => {
                if !allow_fds {
                    return Err(EPERM);
                }
                for &sl in slots {
                    let file = files.get(rd32(kbuf, sl) as i32)?;
                    s.buffer(tp, off).unwrap().fds.push(FdFixup { off: sl as u64, file, fda: true });
                }
            }
            &Kind::Ptr { sg, parent, .. } => {
                let addr = base + off + sg as u64;
                wr64(kbuf, at + 8, addr);
                if let Some((pi, poff)) = parent
                    && let Kind::Ptr { sg: psg, .. } = objs[pi].kind
                {
                    wr64(kbuf, psg + poff, addr);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Read side

/// Fill (at most `avail` bytes of) the read buffer, waiting for work
/// unless `nonblock`. `fresh`: the read starts at offset 0 (BR_NOOP
/// first, BR_SPAWN_LOOPER may replace it).
pub fn read(p: Id, tid: u32, nonblock: bool, fresh: bool, avail: usize) -> KResult<Vec<u8>> {
    let mut out = Vec::new();
    if fresh {
        out.extend_from_slice(&BR_NOOP.to_le_bytes());
    }
    let empty = out.len();
    loop {
        let (proc_work, sleep_key) = {
            let mut s = lock();
            let th = s.thread(p, tid);
            let proc_work = th.proc_work_ok();
            let key = th.key;
            if s.has_work(p, tid, proc_work) {
                (proc_work, None)
            } else if nonblock {
                return Err(EAGAIN);
            } else {
                s.thread(p, tid).waiting_proc = proc_work;
                (proc_work, Some(key))
            }
        };
        if let Some(key) = sleep_key {
            let w = sched::wait_until((WAIT_KEY, key), None, WaitMode::Interruptible, || {
                let mut s = lock();
                let ready = s.has_work(p, tid, proc_work);
                if !ready && let Some(th) = s.thread_opt(p, tid) {
                    // Idle again (picked, but another thread took the work).
                    th.waiting_proc = proc_work;
                }
                ready
            });
            if let Some(th) = lock().thread_opt(p, tid) {
                th.waiting_proc = false;
            }
            if w == Wake::Interrupted {
                return Err(EINTR);
            }
        }

        let mut s = lock();
        let mut no_room = false;
        loop {
            if avail.saturating_sub(out.len()) < 4 + TR_SECCTX_SIZE {
                no_room = true;
                break;
            }
            let th = s.thread(p, tid);
            let w = match th.todo.pop_front() {
                Some(w) => w,
                None if proc_work => match s.proc(p).todo.pop_front() {
                    Some(w) => w,
                    None => break,
                },
                None => break,
            };
            if deliver(&mut s, p, tid, w, &mut out) {
                break;
            }
        }
        if out.len() == empty && !no_room && !s.thread(p, tid).need_return {
            continue;
        }
        let looper = s.thread(p, tid).looper;
        let pr = s.proc(p);
        let waiting = pr.threads.values().any(|t| t.waiting_proc);
        if fresh
            && pr.requested == 0
            && !waiting
            && pr.started < pr.max_threads
            && looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0
        {
            pr.requested += 1;
            wr32(&mut out, 0, BR_SPAWN_LOOPER);
        }
        return Ok(out);
    }
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Write one work item to `out`. True when the read must stop after it
/// (a transaction or BR_DEAD_BINDER was delivered).
fn deliver(s: &mut State, p: Id, tid: u32, w: Work, out: &mut Vec<u8>) -> bool {
    match w {
        Work::Complete => put32(out, BR_TRANSACTION_COMPLETE),
        Work::ReturnError(cmd) => {
            s.thread(p, tid).error_pending = false;
            put32(out, cmd);
        }
        Work::ReplyError(cmd) => put32(out, cmd),
        Work::Node(n) => {
            let Some(node) = s.nodes.get_mut(&n) else { return false };
            node.queued = false;
            let strong = node.internal_strong > 0 || node.local_strong > 0;
            let weak = strong || !node.refs.is_empty() || node.local_weak > 0;
            let mut cmds = Vec::new();
            if weak && !node.has_weak {
                node.has_weak = true;
                node.pending_weak = true;
                node.local_weak += 1;
                cmds.push(BR_INCREFS);
            }
            if strong && !node.has_strong {
                node.has_strong = true;
                node.pending_strong = true;
                node.local_strong += 1;
                cmds.push(BR_ACQUIRE);
            }
            if !strong && node.has_strong {
                node.has_strong = false;
                cmds.push(BR_RELEASE);
            }
            if !weak && node.has_weak {
                node.has_weak = false;
                cmds.push(BR_DECREFS);
            }
            for c in cmds {
                put32(out, c);
                put64(out, node.ptr);
                put64(out, node.cookie);
            }
            if !weak && !strong {
                s.remove_node(n);
            }
        }
        Work::Death(d) => {
            let Some(dt) = s.deaths.get_mut(&d) else { return false };
            dt.queued = false;
            let clear = dt.kind == DeathKind::Clear;
            put32(out, if clear { BR_CLEAR_DEATH_NOTIFICATION_DONE } else { BR_DEAD_BINDER });
            put64(out, dt.cookie);
            if clear {
                s.deaths.remove(&d);
            } else {
                dt.delivered = true;
                s.proc(p).delivered.push(d);
                // The client may make calls to clean up first.
                return true;
            }
        }
        Work::Txn(t) => return deliver_txn(s, p, tid, t, out),
    }
    false
}

fn deliver_txn(s: &mut State, p: Id, tid: u32, t: Id, out: &mut Vec<u8>) -> bool {
    let Some(tx) = s.txns.get(&t) else { return false };
    let Some(off) = tx.buf else {
        s.free_txn(t);
        return false;
    };
    let (reply, sync) = (tx.reply, tx.sync_call());
    // Install the passed files in this (the receiving) process.
    let files = current().process.files();
    let b = s.buffer(p, off).unwrap();
    let fixups = core::mem::take(&mut b.fds);
    let mut installed = Vec::new();
    let mut ok = true;
    for fx in &fixups {
        match files.alloc(fx.file.clone(), true, 0) {
            Ok(fd) => installed.push((fx.off, fd, fx.fda)),
            Err(_) => {
                ok = false;
                break;
            }
        }
    }
    s.grave.extend(fixups.into_iter().map(|f| Grave::File(f.file)));
    if !ok {
        s.grave.extend(installed.into_iter().map(|(_, fd, _)| Grave::Fd(fd)));
        s.release_buffer(p, off, false);
        if reply {
            s.free_txn(t);
            put32(out, BR_FAILED_REPLY);
            return true;
        }
        if sync {
            s.send_failed_reply(t, BR_FAILED_REPLY);
        } else {
            s.free_txn(t);
        }
        return false;
    }
    let m = s.proc(p).map.as_ref().unwrap();
    let (base, shmem) = (m.base, m.shmem.clone());
    for &(o, fd, _) in &installed {
        let _ = shmem.write(off + o, &(fd as u32).to_le_bytes());
    }
    let b = s.buffer(p, off).unwrap();
    b.fda_fds.extend(installed.iter().filter(|x| x.2).map(|x| x.1));
    b.user_free = true;
    let (data_size, offsets_size, secctx) = (b.data_size, b.offsets_size, b.secctx);

    let tx = &s.txns[&t];
    let (ptr, cookie) = match tx.node.and_then(|n| s.nodes.get(&n)) {
        Some(n) if !reply => (n.ptr, n.cookie),
        _ => (0, 0),
    };
    let cmd = if reply {
        BR_REPLY
    } else if secctx.is_some() {
        BR_TRANSACTION_SEC_CTX
    } else {
        BR_TRANSACTION
    };
    let mut tr = vec![0u8; TR_SIZE];
    wr64(&mut tr, 0, ptr);
    wr64(&mut tr, 8, cookie);
    wr32(&mut tr, 16, tx.code);
    wr32(&mut tr, 20, tx.flags);
    wr32(&mut tr, 24, if sync && tx.from.is_some() { tx.sender_pid } else { 0 });
    wr32(&mut tr, 28, tx.sender_euid);
    wr64(&mut tr, 32, data_size);
    wr64(&mut tr, 40, offsets_size);
    wr64(&mut tr, 48, base + off);
    wr64(&mut tr, 56, base + off + align8(data_size));
    put32(out, cmd);
    out.extend_from_slice(&tr);
    if let Some(so) = secctx {
        put64(out, base + off + so);
    }
    if sync {
        let th = s.thread(p, tid);
        let parent = th.stack.replace(t);
        let tx = s.txns.get_mut(&t).unwrap();
        tx.to_thread = Some(tid);
        tx.to_parent = parent;
    } else {
        s.free_txn(t);
    }
    true
}
