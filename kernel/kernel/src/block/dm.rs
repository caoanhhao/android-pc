//! Device mapper: `/dev/device-mapper` (10:236) ioctls and dm-N block
//! devices (253:N), enough for AOSP's libdm / liblp / dmctl.
//!
//! Commands: VERSION, REMOVE_ALL, LIST_DEVICES, DEV_CREATE, DEV_REMOVE,
//! DEV_RENAME, DEV_SUSPEND (suspend / resume = swap the inactive table in),
//! DEV_STATUS, DEV_WAIT (returns at once), TABLE_LOAD, TABLE_CLEAR,
//! TABLE_DEPS, TABLE_STATUS, LIST_VERSIONS, GET_TARGET_VERSION,
//! DEV_SET_GEOMETRY / DEV_ARM_POLL (accepted). Targets: `linear`, `zero`,
//! `error`, and `verity` accepted as a pass-through to its data device
//! (no hash checking). I/O is never queued while suspended.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::Ordering;
use spin::Mutex;

use super::{Backend, BlockDev, DM_MAJOR, SECTOR};
use crate::errno::*;
use crate::fs::sysfs;
use crate::fs::file::{FileOps, OpenFile};

const IOCTL_SIZE: usize = 312;
const NAME_LEN: usize = 128;
const UUID_LEN: usize = 129;
const VERSION: [u32; 3] = [4, 48, 0];

const DM_VERSION: u32 = 0;
const DM_REMOVE_ALL: u32 = 1;
const DM_LIST_DEVICES: u32 = 2;
const DM_DEV_CREATE: u32 = 3;
const DM_DEV_REMOVE: u32 = 4;
const DM_DEV_RENAME: u32 = 5;
const DM_DEV_SUSPEND: u32 = 6;
const DM_DEV_STATUS: u32 = 7;
const DM_DEV_WAIT: u32 = 8;
const DM_TABLE_LOAD: u32 = 9;
const DM_TABLE_CLEAR: u32 = 10;
const DM_TABLE_DEPS: u32 = 11;
const DM_TABLE_STATUS: u32 = 12;
const DM_LIST_VERSIONS: u32 = 13;
const DM_TARGET_MSG: u32 = 14;
const DM_DEV_SET_GEOMETRY: u32 = 15;
const DM_DEV_ARM_POLL: u32 = 16;
const DM_GET_TARGET_VERSION: u32 = 17;

const DM_READONLY_FLAG: u32 = 1;
const DM_SUSPEND_FLAG: u32 = 2;
const DM_PERSISTENT_DEV_FLAG: u32 = 8;
const DM_STATUS_TABLE_FLAG: u32 = 16;
const DM_ACTIVE_PRESENT_FLAG: u32 = 32;
const DM_INACTIVE_PRESENT_FLAG: u32 = 64;
const DM_BUFFER_FULL_FLAG: u32 = 256;
const DM_QUERY_INACTIVE_TABLE_FLAG: u32 = 4096;
const DM_UEVENT_GENERATED_FLAG: u32 = 8192;
const DM_UUID_FLAG: u32 = 16384;
const DM_DATA_OUT_FLAG: u32 = 65536;

const DM_NAME_LIST_FLAG_HAS_UUID: u32 = 1;
const DM_NAME_LIST_FLAG_DOESNT_HAVE_UUID: u32 = 2;

const TARGETS: [(&str, [u32; 3]); 4] =
    [("linear", [1, 4, 0]), ("zero", [1, 2, 0]), ("error", [1, 6, 0]), ("verity", [1, 9, 0])];

enum Kind {
    /// Lower device and start sector on it.
    Linear(Arc<BlockDev>, u64),
    Zero,
    Error,
}

struct Target {
    start: u64,
    len: u64,
    ttype: String,
    params: String,
    kind: Kind,
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Kind::Linear(d, _) = &self.kind {
            d.holders.fetch_sub(1, Ordering::Relaxed);
            super::loop_dev::maybe_release(d);
        }
    }
}

struct Table {
    targets: Vec<Target>,
    ro: bool,
}

impl Table {
    fn sectors(&self) -> u64 {
        self.targets.last().map_or(0, |t| t.start + t.len)
    }

    /// The distinct lower devices, in table order.
    fn lowers(&self) -> Vec<Arc<BlockDev>> {
        let mut v: Vec<Arc<BlockDev>> = Vec::new();
        for t in &self.targets {
            if let Kind::Linear(d, _) = &t.kind
                && !v.iter().any(|x| Arc::ptr_eq(x, d))
            {
                v.push(d.clone());
            }
        }
        v
    }

    fn deps(&self) -> Vec<u64> {
        let mut v: Vec<u64> = Vec::new();
        for t in &self.targets {
            if let Kind::Linear(d, _) = &t.kind
                && !v.contains(&d.rdev())
            {
                v.push(d.rdev());
            }
        }
        v
    }
}

struct State {
    name: String,
    uuid: String,
    live: Option<Table>,
    inactive: Option<Table>,
    suspended: bool,
    event_nr: u32,
}

pub struct DmDev {
    state: Mutex<State>,
}

impl DmDev {
    /// Walk the live table over [off, off + len), calling `f` per piece.
    fn map(&self, off: u64, len: usize, mut f: impl FnMut(&Kind, u64, usize, usize) -> KResult<()>) -> KResult<()> {
        let st = self.state.lock();
        let table = st.live.as_ref().ok_or(EIO)?;
        let mut done = 0;
        while done < len {
            let sector = (off + done as u64) / SECTOR;
            let t = table.targets.iter().find(|t| sector >= t.start && sector < t.start + t.len).ok_or(EIO)?;
            let in_t = off + done as u64 - t.start * SECTOR;
            let n = ((t.len * SECTOR - in_t) as usize).min(len - done);
            f(&t.kind, in_t, done, n)?;
            done += n;
        }
        Ok(())
    }
}

impl Backend for DmDev {
    fn size(&self) -> u64 {
        self.state.lock().live.as_ref().map_or(0, |t| t.sectors() * SECTOR)
    }

    fn read(&self, off: u64, buf: &mut [u8]) -> KResult<()> {
        self.map(off, buf.len(), |k, in_t, at, n| match k {
            Kind::Linear(d, start) => d.backend.read(start * SECTOR + in_t, &mut buf[at..at + n]),
            Kind::Zero => {
                buf[at..at + n].fill(0);
                Ok(())
            }
            Kind::Error => Err(EIO),
        })
    }

    fn write(&self, off: u64, data: &[u8]) -> KResult<()> {
        if self.read_only() {
            return Err(EROFS);
        }
        self.map(off, data.len(), |k, in_t, at, n| match k {
            Kind::Linear(d, start) => d.backend.write(start * SECTOR + in_t, &data[at..at + n]),
            Kind::Zero => Ok(()),
            Kind::Error => Err(EIO),
        })
    }

    fn flush(&self) -> KResult<()> {
        let devs: Vec<Arc<BlockDev>> = {
            let st = self.state.lock();
            let Some(t) = st.live.as_ref() else { return Ok(()) };
            t.targets
                .iter()
                .filter_map(|t| if let Kind::Linear(d, _) = &t.kind { Some(d.clone()) } else { None })
                .collect()
        };
        for d in devs {
            d.backend.flush()?;
        }
        Ok(())
    }

    fn read_only(&self) -> bool {
        self.state.lock().live.as_ref().is_some_and(|t| t.ro)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Device table

static DEVS: Mutex<Vec<Arc<BlockDev>>> = Mutex::new(Vec::new());

fn dm_of(d: &BlockDev) -> &DmDev {
    d.backend.as_any().downcast_ref::<DmDev>().unwrap()
}

fn find(name: &str, uuid: &str, dev: u64) -> KResult<Arc<BlockDev>> {
    let devs = DEVS.lock();
    let found = if !uuid.is_empty() {
        devs.iter().find(|d| dm_of(d).state.lock().uuid == uuid)
    } else if !name.is_empty() {
        devs.iter().find(|d| dm_of(d).state.lock().name == name)
    } else if dev != 0 {
        devs.iter().find(|d| d.rdev() == dev)
    } else {
        None
    };
    found.cloned().ok_or(ENXIO)
}

fn update_sysfs(d: &BlockDev) {
    let st = dm_of(d).state.lock();
    let dir = format!("{}/dm", d.sysfs_path());
    super::sysfs_set(&dir, "name", &format!("{}\n", st.name));
    super::sysfs_set(&dir, "uuid", &format!("{}\n", st.uuid));
    super::sysfs_set(&dir, "suspended", &format!("{}\n", st.suspended as u8));
    super::sysfs_set(&dir, "use_blk_mq", "1\n");
    drop(st);
    d.update_sysfs_size();
    super::sysfs_set(&d.sysfs_path(), "ro", &format!("{}\n", d.read_only() as u8));
}

/// sysfs `slaves/` of `d` and `holders/` of its lower devices, as Linux
/// keeps them: apexd maps a dm mount back to its loop device through
/// /sys/block/dm-N/slaves when it restarts.
fn link_lowers(d: &BlockDev, old: Vec<Arc<BlockDev>>, new: Vec<Arc<BlockDev>>) {
    let path = d.sysfs_path();
    for l in &old {
        sysfs::remove(&format!("{path}/slaves/{}", l.name));
        sysfs::remove(&format!("{}/holders/{}", l.sysfs_path(), d.name));
    }
    for l in &new {
        let (slaves, holders) = (format!("{path}/slaves"), format!("{}/holders", l.sysfs_path()));
        sysfs::symlink(&slaves, &l.name, &sysfs::relative(&slaves, &l.sysfs_path()));
        sysfs::symlink(&holders, &d.name, &sysfs::relative(&holders, &path));
    }
}

// ---------------------------------------------------------------------------
// ioctl buffer handling

struct Req {
    /// Header + input data as read from user space.
    buf: Vec<u8>,
    flags: u32,
    name: String,
    uuid: String,
    dev: u64,
    data_size: usize,
    data_start: usize,
    target_count: u32,
}

fn cstring(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Output of one command: header fields to set and the data area.
struct Out {
    flags: u32,
    dev: u64,
    open_count: i32,
    target_count: u32,
    event_nr: u32,
    name: Option<String>,
    uuid: Option<String>,
    data: Vec<u8>,
}

impl Out {
    fn new(req: &Req) -> Out {
        Out {
            flags: req.flags & !(DM_BUFFER_FULL_FLAG | DM_DATA_OUT_FLAG),
            dev: req.dev,
            open_count: 0,
            target_count: 0,
            event_nr: 0,
            name: None,
            uuid: None,
            data: Vec::new(),
        }
    }

    /// Linux's __dev_status.
    fn status(&mut self, d: &BlockDev) {
        let dm = dm_of(d);
        let st = dm.state.lock();
        self.flags &= !(DM_SUSPEND_FLAG | DM_READONLY_FLAG | DM_ACTIVE_PRESENT_FLAG | DM_INACTIVE_PRESENT_FLAG);
        if st.suspended {
            self.flags |= DM_SUSPEND_FLAG;
        }
        self.dev = d.rdev();
        self.open_count = d.opens.load(Ordering::Relaxed) as i32 + d.holders.load(Ordering::Relaxed) as i32;
        self.event_nr = st.event_nr;
        self.target_count = 0;
        let query_inactive = self.flags & DM_QUERY_INACTIVE_TABLE_FLAG != 0;
        if let Some(t) = &st.live {
            if !query_inactive {
                // Not d.read_only(): that would take this lock again.
                if t.ro || d.ro.load(Ordering::Relaxed) {
                    self.flags |= DM_READONLY_FLAG;
                }
                self.target_count = t.targets.len() as u32;
            }
            self.flags |= DM_ACTIVE_PRESENT_FLAG;
        }
        if let Some(t) = &st.inactive {
            if query_inactive {
                if t.ro {
                    self.flags |= DM_READONLY_FLAG;
                }
                self.target_count = t.targets.len() as u32;
            }
            self.flags |= DM_INACTIVE_PRESENT_FLAG;
        }
        self.name = Some(st.name.clone());
        self.uuid = Some(st.uuid.clone());
    }
}

fn align8(v: &mut Vec<u8>) {
    while v.len() % 8 != 0 {
        v.push(0);
    }
}

/// `/dev/device-mapper`.
pub struct Control;

impl FileOps for Control {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if (cmd >> 8) & 0xff != 0xfd {
            return Err(ENOTTY);
        }
        let mm = crate::process::current().mm();
        let mut hdr = [0u8; IOCTL_SIZE];
        mm.copy_in(arg, &mut hdr)?;
        if u32_at(&hdr, 0) != VERSION[0] {
            return Err(EINVAL);
        }
        let data_size = u32_at(&hdr, 12) as usize;
        if !(IOCTL_SIZE..=16 << 20).contains(&data_size) {
            return Err(EINVAL);
        }
        let mut buf = vec![0u8; data_size];
        mm.copy_in(arg, &mut buf)?;
        let req = Req {
            flags: u32_at(&buf, 28),
            name: cstring(&buf[48..48 + NAME_LEN]),
            uuid: cstring(&buf[176..176 + UUID_LEN]),
            dev: u64_at(&buf, 40),
            data_start: (u32_at(&buf, 16) as usize).clamp(IOCTL_SIZE, data_size),
            target_count: u32_at(&buf, 20),
            data_size,
            buf,
        };
        let nr = cmd & 0xff;
        let mut out = Out::new(&req);
        command(nr, &req, &mut out)?;
        // Write back: header, then the data area at offset 312.
        let mut res = req.buf.clone();
        res[0..12].copy_from_slice(&[VERSION[0].to_le_bytes(), VERSION[1].to_le_bytes(), VERSION[2].to_le_bytes()].concat());
        let mut flags = out.flags;
        let mut size = IOCTL_SIZE;
        if !out.data.is_empty() {
            if IOCTL_SIZE + out.data.len() > data_size {
                flags |= DM_BUFFER_FULL_FLAG;
            } else {
                res[IOCTL_SIZE..IOCTL_SIZE + out.data.len()].copy_from_slice(&out.data);
                size = IOCTL_SIZE + out.data.len();
                flags |= DM_DATA_OUT_FLAG;
            }
        }
        res[12..16].copy_from_slice(&(size as u32).to_le_bytes());
        res[16..20].copy_from_slice(&(IOCTL_SIZE as u32).to_le_bytes());
        res[20..24].copy_from_slice(&out.target_count.to_le_bytes());
        res[24..28].copy_from_slice(&out.open_count.to_le_bytes());
        res[28..32].copy_from_slice(&flags.to_le_bytes());
        res[32..36].copy_from_slice(&out.event_nr.to_le_bytes());
        res[40..48].copy_from_slice(&out.dev.to_le_bytes());
        if let Some(n) = &out.name {
            res[48..48 + NAME_LEN].fill(0);
            let b = n.as_bytes();
            res[48..48 + b.len().min(NAME_LEN - 1)].copy_from_slice(&b[..b.len().min(NAME_LEN - 1)]);
        }
        if let Some(u) = &out.uuid {
            res[176..176 + UUID_LEN].fill(0);
            let b = u.as_bytes();
            res[176..176 + b.len().min(UUID_LEN - 1)].copy_from_slice(&b[..b.len().min(UUID_LEN - 1)]);
        }
        mm.copy_out(arg, &res[..size.max(IOCTL_SIZE)])?;
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn command(nr: u32, req: &Req, out: &mut Out) -> KResult<()> {
    match nr {
        DM_VERSION => Ok(()),
        DM_REMOVE_ALL => {
            let devs = DEVS.lock().clone();
            for d in devs {
                let _ = remove(&d);
            }
            Ok(())
        }
        DM_LIST_DEVICES => {
            let devs = DEVS.lock().clone();
            let mut v = Vec::new();
            let mut prev: Option<usize> = None;
            for d in &devs {
                let st = dm_of(d).state.lock();
                let at = v.len();
                if let Some(p) = prev {
                    let next = (at - p) as u32;
                    v[p + 8..p + 12].copy_from_slice(&next.to_le_bytes());
                }
                v.extend_from_slice(&d.rdev().to_le_bytes());
                v.extend_from_slice(&0u32.to_le_bytes());
                v.extend_from_slice(st.name.as_bytes());
                v.push(0);
                align8(&mut v);
                v.extend_from_slice(&st.event_nr.to_le_bytes());
                if st.uuid.is_empty() {
                    v.extend_from_slice(&DM_NAME_LIST_FLAG_DOESNT_HAVE_UUID.to_le_bytes());
                } else {
                    v.extend_from_slice(&DM_NAME_LIST_FLAG_HAS_UUID.to_le_bytes());
                    v.extend_from_slice(st.uuid.as_bytes());
                    v.push(0);
                }
                align8(&mut v);
                prev = Some(at);
            }
            if v.is_empty() {
                v.extend_from_slice(&[0u8; 16]);
            }
            out.data = v;
            Ok(())
        }
        DM_DEV_CREATE => {
            if req.name.is_empty() || req.name.contains('/') || req.name.len() >= NAME_LEN {
                return Err(EINVAL);
            }
            let minor = {
                let devs = DEVS.lock();
                for d in devs.iter() {
                    let st = dm_of(d).state.lock();
                    if st.name == req.name || (!req.uuid.is_empty() && st.uuid == req.uuid) {
                        return Err(EBUSY);
                    }
                }
                if req.flags & DM_PERSISTENT_DEV_FLAG != 0 {
                    let m = crate::fs::vfs::dev_minor(req.dev);
                    if devs.iter().any(|d| d.minor == m) {
                        return Err(EBUSY);
                    }
                    m
                } else {
                    (0..).find(|m| !devs.iter().any(|d| d.minor == *m)).unwrap()
                }
            };
            let dm = DmDev {
                state: Mutex::new(State {
                    name: req.name.clone(),
                    uuid: req.uuid.clone(),
                    live: None,
                    inactive: None,
                    suspended: false,
                    event_nr: 0,
                }),
            };
            let d = Arc::new(BlockDev::new(format!("dm-{minor}"), DM_MAJOR, minor, Arc::new(dm)));
            DEVS.lock().push(d.clone());
            super::register(d.clone());
            update_sysfs(&d);
            out.status(&d);
            Ok(())
        }
        DM_DEV_REMOVE => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            out.status(&d);
            remove(&d)?;
            out.flags |= DM_UEVENT_GENERATED_FLAG;
            Ok(())
        }
        DM_DEV_RENAME => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            let new = cstring(&req.buf[req.data_start..req.data_size]);
            if new.is_empty() {
                return Err(EINVAL);
            }
            let uuid = req.flags & DM_UUID_FLAG != 0;
            if !uuid && find(&new, "", 0).is_ok() {
                return Err(EBUSY);
            }
            {
                let mut st = dm_of(&d).state.lock();
                if uuid {
                    if !st.uuid.is_empty() {
                        return Err(EINVAL);
                    }
                    st.uuid = new;
                } else {
                    st.name = new;
                }
            }
            update_sysfs(&d);
            out.status(&d);
            out.flags |= DM_UEVENT_GENERATED_FLAG;
            Ok(())
        }
        DM_DEV_SUSPEND => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            let old = {
                let mut st = dm_of(&d).state.lock();
                if req.flags & DM_SUSPEND_FLAG != 0 {
                    st.suspended = true;
                    None
                } else {
                    let new = st.inactive.take();
                    let old = match new {
                        Some(t) => st.live.replace(t),
                        None => None,
                    };
                    st.suspended = false;
                    st.event_nr += 1;
                    old
                }
            };
            let new = dm_of(&d).state.lock().live.as_ref().map(Table::lowers).unwrap_or_default();
            link_lowers(&d, old.as_ref().map(Table::lowers).unwrap_or_default(), new);
            drop(old);
            d.cache.invalidate();
            update_sysfs(&d);
            out.status(&d);
            Ok(())
        }
        DM_DEV_STATUS | DM_DEV_WAIT | DM_DEV_ARM_POLL | DM_DEV_SET_GEOMETRY => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            out.status(&d);
            Ok(())
        }
        DM_TABLE_LOAD => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            let table = parse_table(req)?;
            let old = dm_of(&d).state.lock().inactive.replace(table);
            drop(old);
            out.status(&d);
            Ok(())
        }
        DM_TABLE_CLEAR => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            let old = dm_of(&d).state.lock().inactive.take();
            drop(old);
            out.status(&d);
            Ok(())
        }
        DM_TABLE_DEPS => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            out.status(&d);
            let deps = {
                let st = dm_of(&d).state.lock();
                let t = if req.flags & DM_QUERY_INACTIVE_TABLE_FLAG != 0 { &st.inactive } else { &st.live };
                t.as_ref().map(Table::deps).unwrap_or_default()
            };
            let mut v = Vec::new();
            v.extend_from_slice(&(deps.len() as u32).to_le_bytes());
            v.extend_from_slice(&0u32.to_le_bytes());
            for dev in deps {
                v.extend_from_slice(&dev.to_le_bytes());
            }
            out.data = v;
            Ok(())
        }
        DM_TABLE_STATUS => {
            let d = find(&req.name, &req.uuid, req.dev)?;
            out.status(&d);
            let st = dm_of(&d).state.lock();
            let t = if req.flags & DM_QUERY_INACTIVE_TABLE_FLAG != 0 { &st.inactive } else { &st.live };
            let mut v = Vec::new();
            if let Some(t) = t {
                for tg in &t.targets {
                    let at = v.len();
                    v.extend_from_slice(&tg.start.to_le_bytes());
                    v.extend_from_slice(&tg.len.to_le_bytes());
                    v.extend_from_slice(&0i32.to_le_bytes());
                    v.extend_from_slice(&0u32.to_le_bytes());
                    let mut ty = [0u8; 16];
                    ty[..tg.ttype.len().min(15)].copy_from_slice(&tg.ttype.as_bytes()[..tg.ttype.len().min(15)]);
                    v.extend_from_slice(&ty);
                    if req.flags & DM_STATUS_TABLE_FLAG != 0 {
                        v.extend_from_slice(tg.params.as_bytes());
                    } else if tg.ttype == "verity" {
                        v.push(b'V');
                    }
                    v.push(0);
                    align8(&mut v);
                    let next = v.len() as u32;
                    v[at + 20..at + 24].copy_from_slice(&next.to_le_bytes());
                }
            }
            out.data = v;
            Ok(())
        }
        DM_LIST_VERSIONS => {
            let mut v = Vec::new();
            let mut prev: Option<usize> = None;
            for (name, ver) in TARGETS {
                let at = v.len();
                if let Some(p) = prev {
                    v[p..p + 4].copy_from_slice(&((at - p) as u32).to_le_bytes());
                }
                push_version(&mut v, name, ver);
                prev = Some(at);
            }
            out.data = v;
            Ok(())
        }
        DM_GET_TARGET_VERSION => {
            let name = cstring(&req.buf[req.data_start..req.data_size]);
            let (n, ver) = TARGETS.iter().find(|(n, _)| *n == name).ok_or(EINVAL)?;
            let mut v = Vec::new();
            push_version(&mut v, n, *ver);
            out.data = v;
            Ok(())
        }
        DM_TARGET_MSG => Err(EINVAL),
        _ => Err(ENOTTY),
    }
}

fn push_version(v: &mut Vec<u8>, name: &str, ver: [u32; 3]) {
    v.extend_from_slice(&0u32.to_le_bytes());
    for x in ver {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(name.as_bytes());
    v.push(0);
    align8(v);
}

fn remove(d: &Arc<BlockDev>) -> KResult<()> {
    if d.opens.load(Ordering::Relaxed) > 0 || d.holders.load(Ordering::Relaxed) > 0 {
        return Err(EBUSY);
    }
    DEVS.lock().retain(|x| !Arc::ptr_eq(x, d));
    let lowers = dm_of(d).state.lock().live.as_ref().map(Table::lowers).unwrap_or_default();
    link_lowers(d, lowers, Vec::new());
    super::unregister(d);
    // Drop the tables (releasing their lower devices).
    let (live, inactive) = {
        let mut st = dm_of(d).state.lock();
        (st.live.take(), st.inactive.take())
    };
    drop((live, inactive));
    Ok(())
}

/// Parse TABLE_LOAD's dm_target_spec list.
fn parse_table(req: &Req) -> KResult<Table> {
    let mut targets: Vec<Target> = Vec::new();
    let mut at = req.data_start;
    let end = req.data_size;
    for i in 0..req.target_count {
        if at + 40 > end {
            return Err(EINVAL);
        }
        let b = &req.buf;
        let start = u64_at(b, at);
        let len = u64_at(b, at + 8);
        let next = u32_at(b, at + 20) as usize;
        let ttype = cstring(&b[at + 24..at + 40]);
        let pend = if i + 1 < req.target_count { (at + next).min(end) } else { end };
        let params = cstring(&b[at + 40..pend.max(at + 40)]);
        let expected = targets.last().map_or(0, |t: &Target| t.start + t.len);
        if start != expected || len == 0 {
            return Err(EINVAL);
        }
        let kind = make_target(&ttype, &params, len)?;
        // Linux reports linear devices as major:minor.
        let params = match &kind {
            Kind::Linear(d, off) if ttype == "linear" => format!("{}:{} {}", d.major, d.minor, off),
            _ => params,
        };
        targets.push(Target { start, len, ttype, params, kind });
        at += next;
    }
    if targets.is_empty() {
        return Err(EINVAL);
    }
    Ok(Table { targets, ro: req.flags & DM_READONLY_FLAG != 0 })
}

fn lower(spec: &str, start: u64, len: u64) -> KResult<Kind> {
    let d = super::resolve(spec.as_bytes())?;
    if (start + len) * SECTOR > d.size() {
        log::warn!("dm: {} too small for {}+{} sectors", d.name, start, len);
        return Err(EINVAL);
    }
    d.holders.fetch_add(1, Ordering::Relaxed);
    Ok(Kind::Linear(d, start))
}

fn make_target(ttype: &str, params: &str, len: u64) -> KResult<Kind> {
    let words: Vec<&str> = params.split_ascii_whitespace().collect();
    match ttype {
        "linear" => {
            let [dev, off] = words[..] else { return Err(EINVAL) };
            lower(dev, off.parse().map_err(|_| EINVAL)?, len)
        }
        "zero" => Ok(Kind::Zero),
        "error" => Ok(Kind::Error),
        "verity" => {
            // <version> <data_dev> <hash_dev> <data_bs> <hash_bs> <#blocks> ...
            if words.len() < 10 {
                return Err(EINVAL);
            }
            log::warn!("dm: verity table accepted without verification ({})", words[1]);
            lower(words[1], 0, len)
        }
        _ => {
            log::warn!("dm: unsupported target type {ttype}");
            Err(EINVAL)
        }
    }
}

