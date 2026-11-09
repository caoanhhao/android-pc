//! eBPF objects: maps, programs, BTF and links, their ids, the files
//! that refer to them and bpffs pinning. `syscall::bpf` is the bpf(2)
//! front end.
//!
//! There is no verifier, JIT or interpreter: BPF_PROG_LOAD keeps the
//! instructions (and the maps they reference) and reports them as both
//! the translated and the "JITed" image, attaching is recorded but
//! nothing runs. Maps are real (`map`): user space can create, fill,
//! iterate, pin and share them, which is what AOSP's bpfloader, netd and
//! the libbpf users do with them.
//!
//! Every object has an id (one id space per kind, like Linux's idrs) and
//! lives while a file or a bpffs node refers to it (or, for maps, a
//! program that uses it); the id registries hold weak references only.

pub mod map;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::{FileOps, O_RDONLY, O_RDWR, O_WRONLY, OpenFile};
use crate::fs::poll::WaitQueue;
use crate::fs::tmpfs::TmpInode;
use crate::mm::shmem::Shmem;
use map::MapData;

/// Ids of one kind of object: id -> weak reference, next id to hand out.
struct Ids<T> {
    map: BTreeMap<u32, Weak<T>>,
    next: u32,
}

impl<T> Ids<T> {
    const fn new() -> Self {
        Ids { map: BTreeMap::new(), next: 1 }
    }

    fn alloc(&mut self, obj: &Arc<T>) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        self.map.insert(id, Arc::downgrade(obj));
        id
    }

    fn get(&self, id: u32) -> KResult<Arc<T>> {
        self.map.get(&id).and_then(Weak::upgrade).ok_or(ENOENT)
    }

    /// The smallest live id above `start`.
    fn next_after(&self, start: u32) -> KResult<u32> {
        use core::ops::Bound::{Excluded, Unbounded};
        self.map.range((Excluded(start), Unbounded)).find(|(_, w)| w.strong_count() > 0).map(|(id, _)| *id).ok_or(ENOENT)
    }
}

static MAP_IDS: Mutex<Ids<BpfMap>> = Mutex::new(Ids::new());
static PROG_IDS: Mutex<Ids<BpfProg>> = Mutex::new(Ids::new());
static BTF_IDS: Mutex<Ids<Btf>> = Mutex::new(Ids::new());
static LINK_IDS: Mutex<Ids<BpfLink>> = Mutex::new(Ids::new());

/// Object names: up to 15 characters of [A-Za-z0-9_.].
pub fn obj_name(raw: &[u8; 16]) -> KResult<[u8; 16]> {
    let len = raw.iter().position(|&c| c == 0).ok_or(EINVAL)?;
    if !raw[..len].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.') {
        return Err(EINVAL);
    }
    let mut n = [0u8; 16];
    n[..len].copy_from_slice(&raw[..len]);
    Ok(n)
}

// ---------------------------------------------------------------------------
// Maps

pub struct BpfMap {
    pub id: u32,
    pub ty: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
    pub flags: u32,
    pub name: [u8; 16],
    pub btf: Option<Arc<Btf>>,
    pub btf_key_type_id: u32,
    pub btf_value_type_id: u32,
    pub map_extra: u64,
    /// BPF_MAP_FREEZE: no more writes from user space.
    pub frozen: Mutex<bool>,
    pub data: Mutex<MapData>,
    /// Ring buffers: readers wait here (never woken: no producer runs).
    wq: Arc<WaitQueue>,
}

pub struct MapParams {
    pub ty: u32,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
    pub flags: u32,
    pub name: [u8; 16],
    pub btf: Option<Arc<Btf>>,
    pub btf_key_type_id: u32,
    pub btf_value_type_id: u32,
    pub map_extra: u64,
}

impl BpfMap {
    pub fn create(p: MapParams) -> KResult<Arc<BpfMap>> {
        let data = MapData::new(p.ty, p.key_size, p.value_size, p.max_entries, p.flags)?;
        // devmaps are read-only to programs (Linux sets the flag itself).
        let mut flags = p.flags;
        if matches!(p.ty, map::BPF_MAP_TYPE_DEVMAP | map::BPF_MAP_TYPE_DEVMAP_HASH) {
            flags |= map::BPF_F_RDONLY_PROG;
        }
        let mut ids = MAP_IDS.lock();
        let m = Arc::new(BpfMap {
            id: ids.next,
            ty: p.ty,
            key_size: p.key_size,
            value_size: p.value_size,
            max_entries: p.max_entries,
            flags,
            name: p.name,
            btf: p.btf,
            btf_key_type_id: p.btf_key_type_id,
            btf_value_type_id: p.btf_value_type_id,
            map_extra: p.map_extra,
            frozen: Mutex::new(false),
            data: Mutex::new(data),
            wq: WaitQueue::new(),
        });
        ids.alloc(&m);
        Ok(m)
    }

    pub fn by_id(id: u32) -> KResult<Arc<BpfMap>> {
        MAP_IDS.lock().get(id)
    }

    pub fn next_id(start: u32) -> KResult<u32> {
        MAP_IDS.lock().next_after(start)
    }
}

impl Drop for BpfMap {
    fn drop(&mut self) {
        MAP_IDS.lock().map.remove(&self.id);
    }
}

// ---------------------------------------------------------------------------
// Programs

pub struct BpfProg {
    pub id: u32,
    pub ty: u32,
    pub name: [u8; 16],
    pub insns: Vec<u8>,
    pub tag: [u8; 8],
    pub gpl: bool,
    pub load_time: u64,
    pub uid: u32,
    /// Maps the instructions reference (kept alive while the program is).
    pub maps: Mutex<Vec<Arc<BpfMap>>>,
    pub btf: Option<Arc<Btf>>,
}

pub struct ProgParams {
    pub ty: u32,
    pub name: [u8; 16],
    pub insns: Vec<u8>,
    pub gpl: bool,
    pub maps: Vec<Arc<BpfMap>>,
    pub btf: Option<Arc<Btf>>,
}

impl BpfProg {
    pub fn load(p: ProgParams) -> Arc<BpfProg> {
        let mut ids = PROG_IDS.lock();
        let prog = Arc::new(BpfProg {
            id: ids.next,
            ty: p.ty,
            name: p.name,
            tag: tag(&p.insns),
            insns: p.insns,
            gpl: p.gpl,
            load_time: crate::time::monotonic_ns(),
            uid: crate::process::current_creds().uid,
            maps: Mutex::new(p.maps),
            btf: p.btf,
        });
        ids.alloc(&prog);
        prog
    }

    pub fn by_id(id: u32) -> KResult<Arc<BpfProg>> {
        PROG_IDS.lock().get(id)
    }

    pub fn next_id(start: u32) -> KResult<u32> {
        PROG_IDS.lock().next_after(start)
    }

    /// BPF_PROG_BIND_MAP: keep `m` alive with the program.
    pub fn bind_map(&self, m: &Arc<BpfMap>) {
        let mut maps = self.maps.lock();
        if !maps.iter().any(|x| Arc::ptr_eq(x, m)) {
            maps.push(m.clone());
        }
    }

    pub fn map_ids(&self) -> Vec<u32> {
        self.maps.lock().iter().map(|m| m.id).collect()
    }
}

impl Drop for BpfProg {
    fn drop(&mut self) {
        PROG_IDS.lock().map.remove(&self.id);
        detach_all(self.id);
    }
}

/// The program tag: 8 bytes identifying the instructions (Linux uses a
/// SHA-1 over them with map fds cleared; this is FNV-1a over the same).
fn tag(insns: &[u8]) -> [u8; 8] {
    let mut v = insns.to_vec();
    for i in (0..v.len() / 8).rev() {
        // ld_imm64 of a map: the fd (imm) depends on the loader.
        if v[i * 8] == 0x18 && matches!(v[i * 8 + 1] >> 4, 1 | 2 | 5 | 6) {
            v[i * 8 + 4..i * 8 + 8].fill(0);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in v {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.to_be_bytes()
}

// ---------------------------------------------------------------------------
// BTF

pub struct Btf {
    pub id: u32,
    pub data: Vec<u8>,
}

pub const BTF_MAGIC: u16 = 0xeb9f;

impl Btf {
    /// BPF_BTF_LOAD: check the header and that the type and string
    /// sections lie inside the blob; the types themselves are not checked.
    pub fn load(data: Vec<u8>) -> KResult<Arc<Btf>> {
        if data.len() < 24 {
            return Err(EINVAL);
        }
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as u64;
        let magic = u16::from_le_bytes([data[0], data[1]]);
        let (version, hdr_len) = (data[2], u32_at(4));
        if magic != BTF_MAGIC || version != 1 || hdr_len < 24 || hdr_len > data.len() as u64 {
            return Err(EINVAL);
        }
        let body = data.len() as u64 - hdr_len;
        let (type_off, type_len, str_off, str_len) = (u32_at(8), u32_at(12), u32_at(16), u32_at(20));
        if type_off + type_len > body || str_off + str_len > body || str_len == 0 {
            return Err(EINVAL);
        }
        let mut ids = BTF_IDS.lock();
        let btf = Arc::new(Btf { id: ids.next, data });
        ids.alloc(&btf);
        Ok(btf)
    }

    pub fn by_id(id: u32) -> KResult<Arc<Btf>> {
        BTF_IDS.lock().get(id)
    }

    pub fn next_id(start: u32) -> KResult<u32> {
        BTF_IDS.lock().next_after(start)
    }
}

impl Drop for Btf {
    fn drop(&mut self) {
        BTF_IDS.lock().map.remove(&self.id);
    }
}

// ---------------------------------------------------------------------------
// Links and attachments

pub struct BpfLink {
    pub id: u32,
    pub ty: u32,
    pub attach_type: u32,
    pub prog: Mutex<Option<Arc<BpfProg>>>,
}

pub const BPF_LINK_TYPE_RAW_TRACEPOINT: u32 = 1;
pub const BPF_LINK_TYPE_CGROUP: u32 = 3;

impl BpfLink {
    pub fn new(ty: u32, attach_type: u32, prog: Arc<BpfProg>) -> Arc<BpfLink> {
        let mut ids = LINK_IDS.lock();
        let l = Arc::new(BpfLink { id: ids.next, ty, attach_type, prog: Mutex::new(Some(prog)) });
        ids.alloc(&l);
        l
    }

    pub fn by_id(id: u32) -> KResult<Arc<BpfLink>> {
        LINK_IDS.lock().get(id)
    }

    pub fn next_id(start: u32) -> KResult<u32> {
        LINK_IDS.lock().next_after(start)
    }
}

impl Drop for BpfLink {
    fn drop(&mut self) {
        LINK_IDS.lock().map.remove(&self.id);
    }
}

/// An attachment point: (device, inode) of the cgroup directory or
/// socket a program is attached to, and the attach type.
type AttachPoint = (u64, u64, u32);

/// BPF_PROG_ATTACH records: which programs are attached where (in
/// attach order). Nothing runs them; BPF_PROG_QUERY reports them.
static ATTACHED: Mutex<BTreeMap<AttachPoint, Vec<Arc<BpfProg>>>> = Mutex::new(BTreeMap::new());

pub const BPF_F_ALLOW_OVERRIDE: u32 = 1;
pub const BPF_F_ALLOW_MULTI: u32 = 2;
pub const BPF_F_REPLACE: u32 = 4;

pub fn attach(point: AttachPoint, prog: Arc<BpfProg>, flags: u32) -> KResult<()> {
    let mut a = ATTACHED.lock();
    let list = a.entry(point).or_default();
    if flags & BPF_F_ALLOW_MULTI != 0 {
        if list.iter().any(|p| Arc::ptr_eq(p, &prog)) {
            return Err(EEXIST);
        }
        list.push(prog);
    } else {
        // Without ALLOW_MULTI there is one program, which is replaced.
        list.clear();
        list.push(prog);
    }
    Ok(())
}

/// Detach `prog` (or, when None, the only program) from `point`.
pub fn detach(point: AttachPoint, prog: Option<&Arc<BpfProg>>) -> KResult<()> {
    let mut a = ATTACHED.lock();
    let list = a.get_mut(&point).ok_or(ENOENT)?;
    match prog {
        Some(p) => {
            let i = list.iter().position(|x| Arc::ptr_eq(x, p)).ok_or(ENOENT)?;
            list.remove(i);
        }
        None if list.len() == 1 => {
            list.clear();
        }
        None => return Err(if list.is_empty() { ENOENT } else { EINVAL }),
    }
    if list.is_empty() {
        a.remove(&point);
    }
    Ok(())
}

pub fn query(point: AttachPoint) -> Vec<u32> {
    ATTACHED.lock().get(&point).map(|l| l.iter().map(|p| p.id).collect()).unwrap_or_default()
}

/// A program being freed is detached from everything (only reachable
/// when nothing attached it, since attachments hold a reference; kept
/// for safety).
fn detach_all(id: u32) {
    if let Some(mut a) = ATTACHED.try_lock() {
        a.retain(|_, l| {
            l.retain(|p| p.id != id);
            !l.is_empty()
        });
    }
}

// ---------------------------------------------------------------------------
// Files

/// Any bpf object, as a file or a bpffs node refers to it.
#[derive(Clone)]
pub enum BpfObj {
    Map(Arc<BpfMap>),
    Prog(Arc<BpfProg>),
    Btf(Arc<Btf>),
    Link(Arc<BpfLink>),
}

impl BpfObj {
    fn anon_name(&self) -> &'static str {
        match self {
            BpfObj::Map(_) => "bpf-map",
            BpfObj::Prog(_) => "bpf-prog",
            BpfObj::Btf(_) => "btf",
            BpfObj::Link(_) => "bpf_link",
        }
    }
}

/// An open bpf object (`anon_inode:bpf-map`, ...).
pub struct BpfFile {
    pub obj: BpfObj,
}

impl FileOps for BpfFile {
    fn mmap_shmem(&self, _file: &OpenFile, shared_write: bool, pgoff: u64) -> KResult<Option<(Arc<Shmem>, u64)>> {
        let BpfObj::Map(m) = &self.obj else { return Err(ENODEV) };
        let mmapable = match m.ty {
            map::BPF_MAP_TYPE_ARRAY => m.flags & map::BPF_F_MMAPABLE != 0,
            map::BPF_MAP_TYPE_RINGBUF => true,
            _ => false,
        };
        if !mmapable {
            return Err(ENODEV);
        }
        if shared_write && (*m.frozen.lock() || m.flags & map::BPF_F_RDONLY_PROG != 0 && m.ty != map::BPF_MAP_TYPE_RINGBUF) {
            return Err(EPERM);
        }
        Ok(m.data.lock().shmem().cloned().map(|s| (s, pgoff)))
    }

    fn poll(&self, _file: &OpenFile) -> u32 {
        match &self.obj {
            // An empty ring buffer: nothing to read.
            BpfObj::Map(m) if m.ty == map::BPF_MAP_TYPE_RINGBUF => 0,
            _ => crate::fs::poll::DEFAULT_POLLMASK,
        }
    }

    fn wait_queue(&self, _file: &OpenFile) -> Option<Arc<WaitQueue>> {
        match &self.obj {
            BpfObj::Map(m) => Some(m.wq.clone()),
            _ => None,
        }
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some(self.obj.anon_name())
    }

    fn fdinfo(&self, _file: &OpenFile) -> String {
        match &self.obj {
            BpfObj::Map(m) => format!(
                "map_type:\t{}\nkey_size:\t{}\nvalue_size:\t{}\nmax_entries:\t{}\nmap_flags:\t{:#x}\nmap_extra:\t{:#x}\nmemlock:\t{}\nmap_id:\t{}\nfrozen:\t{}\n",
                m.ty,
                m.key_size,
                m.value_size,
                m.max_entries,
                m.flags,
                m.map_extra,
                m.data.lock().memlock(),
                m.id,
                *m.frozen.lock() as u8
            ),
            BpfObj::Prog(p) => {
                let tag: String = p.tag.iter().map(|b| format!("{b:02x}")).collect();
                format!(
                    "prog_type:\t{}\nprog_jited:\t1\nprog_tag:\t{}\nmemlock:\t{}\nprog_id:\t{}\nrun_time_ns:\t0\nrun_cnt:\t0\nrecursion_misses:\t0\nverified_insns:\t{}\n",
                    p.ty,
                    tag,
                    p.insns.len().div_ceil(4096) * 4096,
                    p.id,
                    p.insns.len() / 8
                )
            }
            BpfObj::Btf(b) => format!("btf_id:\t{}\n", b.id),
            BpfObj::Link(l) => format!(
                "link_type:\t{}\nlink_id:\t{}\nprog_tag:\t\nprog_id:\t{}\n",
                l.ty,
                l.id,
                l.prog.lock().as_ref().map_or(0, |p| p.id)
            ),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new file for `obj`; `flags` are BPF_F_RDONLY / BPF_F_WRONLY.
pub fn new_file(obj: BpfObj, flags: u32) -> Arc<OpenFile> {
    let mode = if flags & map::BPF_F_RDONLY != 0 {
        O_RDONLY
    } else if flags & map::BPF_F_WRONLY != 0 {
        O_WRONLY
    } else {
        O_RDWR
    };
    OpenFile::new(crate::fs::anon_inode(), None, Box::new(BpfFile { obj }), mode)
}

/// The object behind an open file (EINVAL for other files).
pub fn file_obj(f: &OpenFile) -> KResult<BpfObj> {
    f.ops.as_any().downcast_ref::<BpfFile>().map(|b| b.obj.clone()).ok_or(EINVAL)
}

// ---------------------------------------------------------------------------
// bpffs

/// Devices of the bpffs instances (pinning works only inside one).
static BPFFS_DEVS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A new bpffs instance: an empty tmpfs whose directories can hold
/// pinned objects (BPF_OBJ_PIN); mkdir, rename, unlink, chmod and chown
/// work as on tmpfs. Returns its root.
pub fn new_fs(mode: u32) -> Arc<TmpInode> {
    let root = TmpInode::new_fs(mode);
    BPFFS_DEVS.lock().push(crate::fs::vfs::Inode::meta(&*root).dev);
    root
}

pub fn is_bpffs(dev: u64) -> bool {
    BPFFS_DEVS.lock().contains(&dev)
}

/// Pinned node -> object.
pub fn pinned_obj(node: &TmpInode) -> Option<BpfObj> {
    node.object()?.downcast_ref::<BpfObj>().cloned()
}

pub fn pin_payload(obj: BpfObj) -> Arc<dyn Any + Send + Sync> {
    Arc::new(obj)
}
