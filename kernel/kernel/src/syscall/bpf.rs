//! bpf(2): the commands AOSP's bpfloader/netbpfload (libbpf), netd and
//! the libbpf_android users issue. The objects are in `crate::bpf`.
//!
//! `union bpf_attr` is read as raw bytes (up to `size`, zero-extended),
//! fields at their uapi offsets.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::bpf::map::{self, BPF_F_RDONLY, BPF_F_WRONLY};
use crate::bpf::{self, BpfLink, BpfMap, BpfObj, BpfProg, Btf, MapParams, ProgParams};
use crate::errno::*;
use crate::fs::file::OpenFile;
use crate::fs::tmpfs::TmpInode;
use crate::fs::vfs::{self, PathRef, S_IFDIR, S_IFMT};
use crate::process::current;

const BPF_MAP_CREATE: u32 = 0;
const BPF_MAP_LOOKUP_ELEM: u32 = 1;
const BPF_MAP_UPDATE_ELEM: u32 = 2;
const BPF_MAP_DELETE_ELEM: u32 = 3;
const BPF_MAP_GET_NEXT_KEY: u32 = 4;
const BPF_PROG_LOAD: u32 = 5;
const BPF_OBJ_PIN: u32 = 6;
const BPF_OBJ_GET: u32 = 7;
const BPF_PROG_ATTACH: u32 = 8;
const BPF_PROG_DETACH: u32 = 9;
const BPF_PROG_TEST_RUN: u32 = 10;
const BPF_PROG_GET_NEXT_ID: u32 = 11;
const BPF_MAP_GET_NEXT_ID: u32 = 12;
const BPF_PROG_GET_FD_BY_ID: u32 = 13;
const BPF_MAP_GET_FD_BY_ID: u32 = 14;
const BPF_OBJ_GET_INFO_BY_FD: u32 = 15;
const BPF_PROG_QUERY: u32 = 16;
const BPF_RAW_TRACEPOINT_OPEN: u32 = 17;
const BPF_BTF_LOAD: u32 = 18;
const BPF_BTF_GET_FD_BY_ID: u32 = 19;
const BPF_MAP_LOOKUP_AND_DELETE_ELEM: u32 = 21;
const BPF_MAP_FREEZE: u32 = 22;
const BPF_BTF_GET_NEXT_ID: u32 = 23;
const BPF_LINK_CREATE: u32 = 28;
const BPF_LINK_GET_FD_BY_ID: u32 = 30;
const BPF_LINK_GET_NEXT_ID: u32 = 31;
const BPF_ENABLE_STATS: u32 = 32;
const BPF_LINK_DETACH: u32 = 34;
const BPF_PROG_BIND_MAP: u32 = 35;

/// Largest program type number accepted (BPF_PROG_TYPE_NETFILTER).
const MAX_PROG_TYPE: u32 = 32;
const BPF_MAXINSNS: u32 = 1 << 20;
/// BPF_OBJ_PIN/GET: `path_fd` is the directory a relative path starts at.
const BPF_F_PATH_FD: u32 = 1 << 14;
/// ld_imm64 source registers that carry a map fd in `imm`.
const BPF_PSEUDO_MAP_FD: u8 = 1;
const BPF_PSEUDO_MAP_VALUE: u8 = 2;
/// Highest ld_imm64 pseudo source (BPF_PSEUDO_MAP_IDX_VALUE).
const BPF_PSEUDO_MAP_IDX_VALUE: u8 = 6;

/// The kernel's `union bpf_attr`, zero past what the caller passed.
struct Attr([u8; 160]);

impl Attr {
    fn u32(&self, off: usize) -> u32 {
        u32::from_ne_bytes(self.0[off..off + 4].try_into().unwrap())
    }

    fn u64(&self, off: usize) -> u64 {
        u64::from_ne_bytes(self.0[off..off + 8].try_into().unwrap())
    }

    fn name(&self, off: usize) -> KResult<[u8; 16]> {
        bpf::obj_name(self.0[off..off + 16].try_into().unwrap())
    }
}

fn copy_in(addr: u64, len: usize) -> KResult<Vec<u8>> {
    let mut v = vec![0u8; len];
    if len > 0 {
        if addr == 0 {
            return Err(EFAULT);
        }
        current().mm().copy_in(addr, &mut v)?;
    }
    Ok(v)
}

fn copy_out(addr: u64, data: &[u8]) -> KResult<()> {
    if data.is_empty() {
        return Ok(());
    }
    if addr == 0 {
        return Err(EFAULT);
    }
    current().mm().copy_out(addr, data)
}

fn get_file(fd: u32) -> KResult<Arc<OpenFile>> {
    current().process.files().get(fd as i32)
}

fn get_obj(fd: u32) -> KResult<(Arc<OpenFile>, BpfObj)> {
    let f = get_file(fd)?;
    let o = bpf::file_obj(&f)?;
    Ok((f, o))
}

fn get_map(fd: u32) -> KResult<(Arc<OpenFile>, Arc<BpfMap>)> {
    match get_obj(fd)? {
        (f, BpfObj::Map(m)) => Ok((f, m)),
        _ => Err(EINVAL),
    }
}

fn get_prog(fd: u32) -> KResult<Arc<BpfProg>> {
    match get_obj(fd)?.1 {
        BpfObj::Prog(p) => Ok(p),
        _ => Err(EINVAL),
    }
}

fn get_btf(fd: u32) -> KResult<Arc<Btf>> {
    match get_obj(fd)?.1 {
        BpfObj::Btf(b) => Ok(b),
        _ => Err(EINVAL),
    }
}

/// New fds for bpf objects are always close-on-exec.
fn install(obj: BpfObj, flags: u32) -> KResult<u64> {
    let f = bpf::new_file(obj, flags);
    Ok(current().process.files().alloc(f, true, 0)? as u64)
}

pub fn sys_bpf(cmd: u32, uattr: u64, size: u32) -> KResult<u64> {
    let mut a = Attr([0; 160]);
    let n = (size as usize).min(a.0.len());
    if n > 0 {
        if uattr == 0 {
            return Err(EFAULT);
        }
        current().mm().copy_in(uattr, &mut a.0[..n])?;
    }
    let r = run(cmd, &a, uattr, size);
    if let Err(e) = r
        && e != ENOENT
    {
        log::debug!("bpf: cmd {cmd} -> {e:?}");
    }
    r
}

fn run(cmd: u32, a: &Attr, uattr: u64, size: u32) -> KResult<u64> {
    match cmd {
        BPF_MAP_CREATE => map_create(a),
        BPF_MAP_LOOKUP_ELEM => map_lookup(a, false),
        BPF_MAP_LOOKUP_AND_DELETE_ELEM => map_lookup(a, true),
        BPF_MAP_UPDATE_ELEM => map_update(a),
        BPF_MAP_DELETE_ELEM => map_delete(a),
        BPF_MAP_GET_NEXT_KEY => map_next_key(a),
        BPF_MAP_FREEZE => {
            let (f, m) = get_map(a.u32(0))?;
            if !f.writable() {
                return Err(EPERM);
            }
            *m.frozen.lock() = true;
            Ok(0)
        }
        BPF_PROG_LOAD => prog_load(a, uattr, size),
        BPF_OBJ_PIN => obj_pin(a),
        BPF_OBJ_GET => obj_get(a),
        BPF_PROG_ATTACH => prog_attach(a, true),
        BPF_PROG_DETACH => prog_attach(a, false),
        BPF_PROG_QUERY => prog_query(a, uattr),
        BPF_PROG_TEST_RUN => Err(EOPNOTSUPP),
        BPF_PROG_GET_NEXT_ID => next_id(a, uattr, BpfProg::next_id),
        BPF_MAP_GET_NEXT_ID => next_id(a, uattr, BpfMap::next_id),
        BPF_BTF_GET_NEXT_ID => next_id(a, uattr, Btf::next_id),
        BPF_LINK_GET_NEXT_ID => next_id(a, uattr, BpfLink::next_id),
        BPF_PROG_GET_FD_BY_ID => install(BpfObj::Prog(BpfProg::by_id(a.u32(0))?), 0),
        BPF_MAP_GET_FD_BY_ID => {
            let flags = a.u32(8);
            if flags & !(BPF_F_RDONLY | BPF_F_WRONLY) != 0 {
                return Err(EINVAL);
            }
            install(BpfObj::Map(BpfMap::by_id(a.u32(0))?), flags)
        }
        BPF_BTF_GET_FD_BY_ID => install(BpfObj::Btf(Btf::by_id(a.u32(0))?), 0),
        BPF_LINK_GET_FD_BY_ID => install(BpfObj::Link(BpfLink::by_id(a.u32(0))?), 0),
        BPF_OBJ_GET_INFO_BY_FD => get_info(a, uattr),
        BPF_BTF_LOAD => btf_load(a),
        BPF_RAW_TRACEPOINT_OPEN => {
            let prog = get_prog(a.u32(8))?;
            install(BpfObj::Link(BpfLink::new(bpf::BPF_LINK_TYPE_RAW_TRACEPOINT, 0, prog)), 0)
        }
        BPF_LINK_CREATE => link_create(a),
        BPF_LINK_DETACH => match get_obj(a.u32(0))?.1 {
            BpfObj::Link(l) => {
                l.prog.lock().take();
                Ok(0)
            }
            _ => Err(EINVAL),
        },
        BPF_PROG_BIND_MAP => {
            let prog = get_prog(a.u32(0))?;
            let (_, m) = get_map(a.u32(4))?;
            if a.u32(8) != 0 {
                return Err(EINVAL);
            }
            prog.bind_map(&m);
            Ok(0)
        }
        // Statistics are never collected; the fd just has to exist.
        BPF_ENABLE_STATS => {
            if a.u32(0) != 0 {
                return Err(EINVAL);
            }
            Ok(current().process.files().alloc(OpenFile::new(crate::fs::anon_inode(), None, alloc::boxed::Box::new(StatsFile), 0), true, 0)?
                as u64)
        }
        _ => Err(EINVAL),
    }
}

/// BPF_ENABLE_STATS's fd (`anon_inode:bpf-stats`).
struct StatsFile;

impl crate::fs::file::FileOps for StatsFile {
    fn anon_name(&self) -> Option<&'static str> {
        Some("bpf-stats")
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Maps

fn map_create(a: &Attr) -> KResult<u64> {
    let btf_fd = a.u32(48);
    let (key_id, value_id) = (a.u32(52), a.u32(56));
    let btf = if key_id != 0 || value_id != 0 || a.u32(60) != 0 { Some(get_btf(btf_fd)?) } else { None };
    if a.u32(20) != 0 && !matches!(a.u32(0), map::BPF_MAP_TYPE_ARRAY_OF_MAPS | map::BPF_MAP_TYPE_HASH_OF_MAPS) {
        return Err(EINVAL);
    }
    if a.u32(0) == map::BPF_MAP_TYPE_ARRAY_OF_MAPS || a.u32(0) == map::BPF_MAP_TYPE_HASH_OF_MAPS {
        get_map(a.u32(20))?;
    }
    let flags = a.u32(16);
    let m = BpfMap::create(MapParams {
        ty: a.u32(0),
        key_size: a.u32(4),
        value_size: a.u32(8),
        max_entries: a.u32(12),
        flags,
        name: a.name(28)?,
        btf,
        btf_key_type_id: key_id,
        btf_value_type_id: value_id,
        map_extra: a.u64(64),
    })?;
    install(BpfObj::Map(m), flags & (BPF_F_RDONLY | BPF_F_WRONLY))
}

fn map_lookup(a: &Attr, delete: bool) -> KResult<u64> {
    let (f, m) = get_map(a.u32(0))?;
    if !f.readable() || delete && !f.writable() {
        return Err(EPERM);
    }
    if a.u64(24) & !map::BPF_F_LOCK != 0 {
        return Err(EINVAL);
    }
    let mut d = m.data.lock();
    let key = copy_in(a.u64(8), d.key_size())?;
    let v = d.lookup(&key)?;
    if delete {
        if *m.frozen.lock() {
            return Err(EPERM);
        }
        d.delete(&key)?;
    }
    drop(d);
    copy_out(a.u64(16), &v)?;
    Ok(0)
}

fn map_update(a: &Attr) -> KResult<u64> {
    let (f, m) = get_map(a.u32(0))?;
    if !f.writable() || *m.frozen.lock() {
        return Err(EPERM);
    }
    let mut d = m.data.lock();
    let key = copy_in(a.u64(8), d.key_size())?;
    let value = copy_in(a.u64(16), d.value_len())?;
    d.update(&key, &value, a.u64(24))?;
    Ok(0)
}

fn map_delete(a: &Attr) -> KResult<u64> {
    let (f, m) = get_map(a.u32(0))?;
    if !f.writable() || *m.frozen.lock() {
        return Err(EPERM);
    }
    let mut d = m.data.lock();
    let key = copy_in(a.u64(8), d.key_size())?;
    d.delete(&key)?;
    Ok(0)
}

fn map_next_key(a: &Attr) -> KResult<u64> {
    let (f, m) = get_map(a.u32(0))?;
    if !f.readable() {
        return Err(EPERM);
    }
    let d = m.data.lock();
    let key = if a.u64(8) != 0 { Some(copy_in(a.u64(8), d.key_size())?) } else { None };
    let next = d.next_key(key.as_deref())?;
    drop(d);
    copy_out(a.u64(16), &next)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Programs, BTF, links

fn prog_load(a: &Attr, uattr: u64, size: u32) -> KResult<u64> {
    let (ty, cnt) = (a.u32(0), a.u32(4));
    if ty == 0 || ty > MAX_PROG_TYPE || cnt == 0 || cnt > BPF_MAXINSNS {
        return Err(EINVAL);
    }
    let insns = copy_in(a.u64(8), cnt as usize * 8)?;
    let license = if a.u64(16) != 0 { current().mm().read_cstr(a.u64(16), 128)? } else { Vec::new() };
    // Linux's license_is_gpl_compatible.
    let gpl = matches!(
        &license[..],
        b"GPL" | b"GPL v2" | b"GPL and additional rights" | b"Dual BSD/GPL" | b"Dual MIT/GPL" | b"Dual MPL/GPL"
    );
    // Maps referenced by ld_imm64 instructions.
    let mut maps: Vec<Arc<BpfMap>> = Vec::new();
    let mut i = 0;
    while i < insns.len() / 8 {
        let ins = &insns[i * 8..i * 8 + 8];
        if ins[0] == 0x18 {
            let src = ins[1] >> 4;
            if src == BPF_PSEUDO_MAP_FD || src == BPF_PSEUDO_MAP_VALUE {
                let fd = u32::from_ne_bytes(ins[4..8].try_into().unwrap());
                let (_, m) = get_map(fd).map_err(|_| EBADF)?;
                if !maps.iter().any(|x| Arc::ptr_eq(x, &m)) {
                    maps.push(m);
                }
            } else if src > BPF_PSEUDO_MAP_IDX_VALUE {
                return Err(EINVAL);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let btf = match a.u32(72) {
        0 => None,
        fd => Some(get_btf(fd)?),
    };
    let prog = BpfProg::load(ProgParams {
        ty,
        name: a.name(48)?,
        insns,
        gpl,
        maps,
        btf,
    });
    // An empty verifier log.
    let (log_size, log_buf) = (a.u32(28), a.u64(32));
    if a.u32(24) != 0 && log_size > 0 && log_buf != 0 {
        copy_out(log_buf, &[0])?;
    }
    if size as usize >= 144 {
        copy_out(uattr + 140, &1u32.to_ne_bytes())?; // log_true_size
    }
    install(BpfObj::Prog(prog), 0)
}

fn btf_load(a: &Attr) -> KResult<u64> {
    let len = a.u32(16);
    if len == 0 || len > 16 << 20 {
        return Err(EINVAL);
    }
    let data = copy_in(a.u64(0), len as usize)?;
    let btf = Btf::load(data)?;
    let (log_buf, log_size) = (a.u64(8), a.u32(20));
    if a.u32(24) != 0 && log_size > 0 && log_buf != 0 {
        copy_out(log_buf, &[0])?;
    }
    install(BpfObj::Btf(btf), 0)
}

fn link_create(a: &Attr) -> KResult<u64> {
    let prog = get_prog(a.u32(0))?;
    let (target, attach_type) = (a.u32(4), a.u32(8));
    let f = get_file(target)?;
    let point = attach_point(&f, attach_type);
    bpf::attach(point, prog.clone(), bpf::BPF_F_ALLOW_MULTI)?;
    install(BpfObj::Link(BpfLink::new(bpf::BPF_LINK_TYPE_CGROUP, attach_type, prog)), 0)
}

fn attach_point(f: &OpenFile, attach_type: u32) -> (u64, u64, u32) {
    let m = f.inode.meta();
    (m.dev, m.ino, attach_type)
}

/// BPF_PROG_ATTACH / DETACH to a cgroup directory or socket: recorded,
/// never run.
fn prog_attach(a: &Attr, attach: bool) -> KResult<u64> {
    let (target, prog_fd, attach_type, flags) = (a.u32(0), a.u32(4), a.u32(8), a.u32(12));
    let f = get_file(target)?;
    let point = attach_point(&f, attach_type);
    if attach {
        if flags & !(bpf::BPF_F_ALLOW_OVERRIDE | bpf::BPF_F_ALLOW_MULTI | bpf::BPF_F_REPLACE) != 0 {
            return Err(EINVAL);
        }
        bpf::attach(point, get_prog(prog_fd)?, flags)?;
    } else {
        let prog = if prog_fd != 0 { Some(get_prog(prog_fd)?) } else { None };
        bpf::detach(point, prog.as_ref())?;
    }
    Ok(0)
}

fn prog_query(a: &Attr, uattr: u64) -> KResult<u64> {
    let f = get_file(a.u32(0))?;
    let ids = bpf::query(attach_point(&f, a.u32(4)));
    let (ptr, cap) = (a.u64(16), a.u32(24));
    copy_out(uattr + 12, &0u32.to_ne_bytes())?; // attach_flags
    copy_out(uattr + 24, &(ids.len() as u32).to_ne_bytes())?; // prog_cnt
    if ptr != 0 && cap > 0 {
        let n = ids.len().min(cap as usize);
        let bytes: Vec<u8> = ids[..n].iter().flat_map(|i| i.to_ne_bytes()).collect();
        copy_out(ptr, &bytes)?;
        if n < ids.len() {
            return Err(ENOSPC);
        }
    }
    Ok(0)
}

fn next_id(a: &Attr, uattr: u64, next: fn(u32) -> KResult<u32>) -> KResult<u64> {
    let id = next(a.u32(0))?;
    copy_out(uattr + 4, &id.to_ne_bytes())?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Pinning

fn path_base(a: &Attr, path: &[u8]) -> KResult<PathRef> {
    if path.first() == Some(&b'/') {
        return Ok(vfs::root());
    }
    if a.u32(12) & BPF_F_PATH_FD != 0 {
        let f = get_file(a.u32(16))?;
        return f.path.clone().filter(|p| p.is_dir()).ok_or(ENOTDIR);
    }
    Ok(current().process.cwd())
}

fn user_path(addr: u64) -> KResult<Vec<u8>> {
    if addr == 0 {
        return Err(EFAULT);
    }
    let p = current().mm().read_cstr(addr, vfs::PATH_MAX - 1)?;
    if p.is_empty() {
        return Err(ENOENT);
    }
    Ok(p)
}

fn obj_pin(a: &Attr) -> KResult<u64> {
    if a.u32(12) & !BPF_F_PATH_FD != 0 {
        return Err(EINVAL);
    }
    let (_, obj) = get_obj(a.u32(8))?;
    let path = user_path(a.u64(0))?;
    let (dir, name, _) = vfs::lookup_parent(&path_base(a, &path)?, &path)?;
    let dnode = dir.inode().as_any().downcast_ref::<TmpInode>().ok_or(EPERM)?;
    if !bpf::is_bpffs(dir.inode().meta().dev) {
        return Err(EPERM);
    }
    if dir.inode().meta().mode() & S_IFMT != S_IFDIR {
        return Err(ENOTDIR);
    }
    let umask = current().process.fs.lock().umask;
    let node = dnode.add_object(&name, 0o600 & !umask, bpf::pin_payload(obj))?;
    vfs::adopt(&dir, &name, node);
    Ok(0)
}

fn obj_get(a: &Attr) -> KResult<u64> {
    let flags = a.u32(12);
    if flags & !(BPF_F_RDONLY | BPF_F_WRONLY | BPF_F_PATH_FD) != 0 || flags & BPF_F_RDONLY != 0 && flags & BPF_F_WRONLY != 0 {
        return Err(EINVAL);
    }
    let path = user_path(a.u64(0))?;
    let p = vfs::lookup(&path_base(a, &path)?, &path, true)?;
    let node = p.inode().as_any().downcast_ref::<TmpInode>().ok_or(EPERM)?;
    let obj = bpf::pinned_obj(node).ok_or(EPERM)?;
    let f = bpf::new_file(obj, flags & (BPF_F_RDONLY | BPF_F_WRONLY));
    let fd = current().process.files().alloc(f, true, 0)?;
    Ok(fd as u64)
}

// ---------------------------------------------------------------------------
// BPF_OBJ_GET_INFO_BY_FD

fn put32(b: &mut [u8], off: usize, v: u32) {
    if off + 4 <= b.len() {
        b[off..off + 4].copy_from_slice(&v.to_ne_bytes());
    }
}

fn put64(b: &mut [u8], off: usize, v: u64) {
    if off + 8 <= b.len() {
        b[off..off + 8].copy_from_slice(&v.to_ne_bytes());
    }
}

fn get32(b: &[u8], off: usize) -> u32 {
    if off + 4 <= b.len() { u32::from_ne_bytes(b[off..off + 4].try_into().unwrap()) } else { 0 }
}

fn get64(b: &[u8], off: usize) -> u64 {
    if off + 8 <= b.len() { u64::from_ne_bytes(b[off..off + 8].try_into().unwrap()) } else { 0 }
}

/// Copy the first `n` of `items` (4 bytes each) to `ptr` when the caller
/// gave a buffer.
fn put_array(ptr: u64, n: u32, items: &[u8]) -> KResult<()> {
    if ptr != 0 && n > 0 {
        copy_out(ptr, &items[..(n as usize).min(items.len())])?;
    }
    Ok(())
}

fn get_info(a: &Attr, uattr: u64) -> KResult<u64> {
    let (_, obj) = get_obj(a.u32(0))?;
    let (ulen, ptr) = (a.u32(4), a.u64(8));
    let ksize = match obj {
        BpfObj::Map(_) => 88,
        BpfObj::Prog(_) => 232,
        BpfObj::Btf(_) => 32,
        BpfObj::Link(_) => 48,
    };
    // A larger struct than the kernel knows must be zero past its end.
    if ulen as usize > ksize {
        let tail = copy_in(ptr + ksize as u64, (ulen as usize - ksize).min(4096))?;
        if tail.iter().any(|&b| b != 0) {
            return Err(E2BIG);
        }
    }
    let len = (ulen as usize).min(ksize);
    let mut info = copy_in(ptr, len)?;
    info.resize(ksize, 0);
    match obj {
        BpfObj::Map(m) => {
            info.fill(0);
            put32(&mut info, 0, m.ty);
            put32(&mut info, 4, m.id);
            put32(&mut info, 8, m.key_size);
            put32(&mut info, 12, m.value_size);
            put32(&mut info, 16, m.max_entries);
            put32(&mut info, 20, m.flags);
            info[24..40].copy_from_slice(&m.name);
            put32(&mut info, 64, m.btf.as_ref().map_or(0, |b| b.id));
            put32(&mut info, 68, m.btf_key_type_id);
            put32(&mut info, 72, m.btf_value_type_id);
            put64(&mut info, 80, m.map_extra);
        }
        BpfObj::Prog(p) => {
            let user = info.clone();
            info.fill(0);
            let ilen = p.insns.len() as u32;
            put32(&mut info, 0, p.ty);
            put32(&mut info, 4, p.id);
            info[8..16].copy_from_slice(&p.tag);
            // No JIT: the "JITed" image is the instructions themselves.
            put32(&mut info, 16, ilen);
            put32(&mut info, 20, ilen);
            let (jptr, jlen, xptr, xlen) = (get64(&user, 24), get32(&user, 16), get64(&user, 32), get32(&user, 20));
            put_array(jptr, jlen, &p.insns)?;
            put_array(xptr, xlen, &p.insns)?;
            put64(&mut info, 24, jptr);
            put64(&mut info, 32, xptr);
            put64(&mut info, 40, p.load_time);
            put32(&mut info, 48, p.uid);
            let ids: Vec<u8> = p.map_ids().iter().flat_map(|i| i.to_ne_bytes()).collect();
            put32(&mut info, 52, (ids.len() / 4) as u32);
            let (mptr, mn) = (get64(&user, 56), get32(&user, 52));
            put_array(mptr, mn.saturating_mul(4), &ids)?;
            put64(&mut info, 56, mptr);
            info[64..80].copy_from_slice(&p.name);
            put32(&mut info, 84, p.gpl as u32);
            put32(&mut info, 128, p.btf.as_ref().map_or(0, |b| b.id));
            // Pointer fields the caller set stay as they were.
            for off in [112, 120, 136, 152, 160, 184] {
                put64(&mut info, off, get64(&user, off));
            }
            put32(&mut info, 216, ilen / 8);
        }
        BpfObj::Btf(b) => {
            let user = info.clone();
            info.fill(0);
            let (bptr, blen) = (get64(&user, 0), get32(&user, 8));
            if bptr != 0 && blen > 0 {
                if (blen as usize) < b.data.len() {
                    return Err(ENOSPC);
                }
                copy_out(bptr, &b.data)?;
            }
            put64(&mut info, 0, bptr);
            put32(&mut info, 8, b.data.len() as u32);
            put32(&mut info, 12, b.id);
            let (nptr, nlen) = (get64(&user, 16), get32(&user, 24));
            if nptr != 0 && nlen > 0 {
                copy_out(nptr, &[0])?;
            }
            // Loaded BTF has no name (only the kernel's and modules' do).
            put64(&mut info, 16, nptr);
        }
        BpfObj::Link(l) => {
            info.fill(0);
            put32(&mut info, 0, l.ty);
            put32(&mut info, 4, l.id);
            put32(&mut info, 8, l.prog.lock().as_ref().map_or(0, |p| p.id));
            put32(&mut info, 20, l.attach_type);
        }
    }
    copy_out(ptr, &info[..len])?;
    copy_out(uattr + 4, &(len as u32).to_ne_bytes())?;
    Ok(0)
}
