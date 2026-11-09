//! DRM for virtio-gpu: /dev/dri/card0 (226:0) and /dev/dri/renderD128
//! (226:128), driver name "virtio_gpu".
//!
//! Buffers are GEM objects backed by [`Buffer`]s (dumb buffers, or
//! dma-bufs imported with PRIME); handles are per open file, mmap goes
//! through a fake offset from MODE_MAP_DUMB / VIRTGPU_MAP. KMS (see
//! `kms`): one plane, CRTC, encoder and connector, legacy SETCRTC /
//! SETPLANE / PAGE_FLIP and atomic commits with IN_FENCE_FD,
//! OUT_FENCE_PTR and flip events (read from the fd). The virtgpu
//! ioctls answer as a 2D-only device (no virgl, no blobs, no capsets),
//! which is what minigbm's 2D path and the ranchu composer expect.
//!
//! Render nodes get EACCES for KMS ioctls, except the dumb-buffer ones
//! (minigbm allocates dumb buffers through renderD128).

pub mod kms;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, Once};

use crate::drivers::dma_buf::{self, Buffer, sync_file};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLIN, POLLOUT, POLLRDNORM, POLLWRNORM, WaitQueue};
use crate::mm::PAGE_SIZE;
use crate::mm::shmem::Shmem;
use crate::process::current;
use crate::sched::{self, Wake, WaitMode};
use kms::*;

pub const DRM_MAJOR: u32 = 226;
const RENDER_MINOR: u32 = 128;

/// First fake mmap offset (Linux's DRM_FILE_PAGE_OFFSET_START, in bytes).
const MMAP_BASE: u64 = 0x1_0000_0000;

const CAP_DUMB_BUFFER: u64 = 1;
const CAP_VBLANK_HIGH_CRTC: u64 = 2;
const CAP_DUMB_PREFERRED_DEPTH: u64 = 3;
const CAP_DUMB_PREFER_SHADOW: u64 = 4;
const CAP_PRIME: u64 = 5;
const CAP_TIMESTAMP_MONOTONIC: u64 = 6;
const CAP_ASYNC_PAGE_FLIP: u64 = 7;
const CAP_CURSOR_WIDTH: u64 = 8;
const CAP_CURSOR_HEIGHT: u64 = 9;
const CAP_ADDFB2_MODIFIERS: u64 = 0x10;
const CAP_PAGE_FLIP_TARGET: u64 = 0x11;
const CAP_CRTC_IN_VBLANK_EVENT: u64 = 0x12;
const CAP_SYNCOBJ: u64 = 0x13;
const CAP_SYNCOBJ_TIMELINE: u64 = 0x14;
const CAP_ATOMIC_ASYNC_PAGE_FLIP: u64 = 0x15;

const CLIENT_CAP_STEREO_3D: u64 = 1;
const CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
const CLIENT_CAP_ATOMIC: u64 = 3;
const CLIENT_CAP_ASPECT_RATIO: u64 = 4;
const CLIENT_CAP_WRITEBACK_CONNECTORS: u64 = 5;
const CLIENT_CAP_CURSOR_PLANE_HOTSPOT: u64 = 6;

const PAGE_FLIP_EVENT: u32 = 0x01;
const PAGE_FLIP_ASYNC: u32 = 0x02;
const ATOMIC_TEST_ONLY: u32 = 0x0100;
const ATOMIC_NONBLOCK: u32 = 0x0200;
const ATOMIC_ALLOW_MODESET: u32 = 0x0400;
const ATOMIC_FLAGS: u32 = PAGE_FLIP_EVENT | PAGE_FLIP_ASYNC | ATOMIC_TEST_ONLY | ATOMIC_NONBLOCK | ATOMIC_ALLOW_MODESET;

const EVENT_VBLANK: u32 = 0x01;
const EVENT_FLIP_COMPLETE: u32 = 0x02;
/// Most queued event bytes per file (Linux: 4 KiB of events in flight).
const EVENT_SPACE: usize = 4096;

const VBLANK_RELATIVE: u32 = 0x1;
const VBLANK_EVENT: u32 = 0x400_0000;
const VBLANK_FLAGS: u32 = 0xfc00_003f;
/// _DRM_VBLANK_SECONDARY and the high-CRTC bits.
const VBLANK_OTHER_CRTC: u32 = 0x2000_003e;

const DRM_CLOEXEC: u32 = 0o2000000;
const DRM_RDWR: u32 = 2;

const ENCODER_VIRTUAL: u32 = 5;
const CONNECTOR_VIRTUAL: u32 = 15;

/// virtgpu GETPARAM values: 3D_FEATURES, CAPSET_QUERY_FIX, RESOURCE_BLOB,
/// HOST_VISIBLE, CROSS_DEVICE, CONTEXT_INIT, SUPPORTED_CAPSET_IDs.
const VIRTGPU_PARAMS: [u32; 7] = [0, 1, 0, 0, 0, 0, 0];

struct Device {
    kms: Mutex<Kms>,
    /// "0000:00:02.0" (GET_UNIQUE).
    unique: String,
    /// Pid of the open file that is DRM master (0: none).
    master: AtomicU64,
}

static DEV: Once<Device> = Once::new();
static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
static NEXT_MMAP: AtomicU64 = AtomicU64::new(MMAP_BASE);
/// Start of the emulated 60 Hz vblank counter.
static VBLANK_EPOCH: Once<u64> = Once::new();

const FRAME_NS: u64 = 1_000_000_000 / 60;
/// Sleep key of WAIT_VBLANK (never woken: the deadline ends the wait).
const VBLANK_KEY: u64 = 0x5642_4c4b_0000_0000;

fn vblank_now() -> (u32, u64) {
    let now = crate::time::monotonic_ns();
    let epoch = *VBLANK_EPOCH.call_once(|| now);
    let seq = (now - epoch) / FRAME_NS;
    (seq as u32, epoch + seq * FRAME_NS)
}

/// Create the DRM device for the probed virtio-gpu (`sysdev`: its virtio
/// device directory in sysfs).
pub fn register(sysdev: &str) {
    let Some(g) = crate::drivers::virtio::gpu::get() else { return };
    let unique = sysdev.split('/').find(|c| c.starts_with("0000:")).unwrap_or("").into();
    DEV.call_once(|| Device { kms: Mutex::new(Kms::new(g)), unique, master: AtomicU64::new(0) });
    use crate::fs::sysfs;
    for (name, minor) in [("card0", 0), ("renderD128", RENDER_MINOR)] {
        let path = alloc::format!("{sysdev}/drm/{name}");
        sysfs::add_char_device(&path, "drm", name, &alloc::format!("dri/{name}"), DRM_MAJOR, minor);
    }
    let conn = alloc::format!("{sysdev}/drm/card0/card0-Virtual-1");
    sysfs::set(&conn, "status", "connected\n");
    sysfs::set(&conn, "enabled", "disabled\n");
    let (w, h) = { let k = DEV.get().unwrap().kms.lock(); (k.width, k.height) };
    sysfs::set(&conn, "modes", &alloc::format!("{w}x{h}\n"));
    let root = crate::fs::devtmpfs_root();
    let dri = match root.get(b"dri") {
        Some(d) => Some(d),
        None => root.add(b"dri", crate::fs::vfs::S_IFDIR | 0o755, 0, None).ok(),
    };
    if let Some(d) = dri {
        use crate::fs::vfs::{S_IFCHR, encode_dev};
        let _ = d.add(b"card0", S_IFCHR | 0o666, encode_dev(DRM_MAJOR, 0), None);
        let _ = d.add(b"renderD128", S_IFCHR | 0o666, encode_dev(DRM_MAJOR, RENDER_MINOR), None);
    }
}

/// Open a DRM minor.
pub fn open(minor: u32) -> KResult<Box<dyn FileOps>> {
    let dev = DEV.get().ok_or(ENXIO)?;
    let render = match minor {
        0 => false,
        RENDER_MINOR => true,
        _ => return Err(ENXIO),
    };
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    if !render {
        let _ = dev.master.compare_exchange(0, id, Ordering::AcqRel, Ordering::Relaxed);
    }
    Ok(Box::new(DrmFile {
        id,
        render,
        st: Mutex::new(FileState { handles: BTreeMap::new(), next_handle: 1, atomic: false, universal: false, events: VecDeque::new() }),
        wq: WaitQueue::new(),
    }))
}

struct FileState {
    handles: BTreeMap<u32, Arc<Buffer>>,
    next_handle: u32,
    atomic: bool,
    universal: bool,
    events: VecDeque<Vec<u8>>,
}

impl FileState {
    fn buffer(&self, handle: u32) -> KResult<Arc<Buffer>> {
        self.handles.get(&handle).cloned().ok_or(ENOENT)
    }

    /// The handle of `buf` in this file, created if needed (one handle
    /// per buffer, as GEM's PRIME import).
    fn handle_of(&mut self, buf: &Arc<Buffer>) -> u32 {
        if let Some((&h, _)) = self.handles.iter().find(|(_, b)| Arc::ptr_eq(b, buf)) {
            return h;
        }
        let h = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(h, buf.clone());
        h
    }

    fn queued(&self) -> usize {
        self.events.iter().map(|e| e.len()).sum()
    }
}

pub struct DrmFile {
    id: u64,
    render: bool,
    st: Mutex<FileState>,
    wq: Arc<WaitQueue>,
}

/// Ioctl argument bytes: what the caller passed (its struct size), zero
/// padded to what we use; written back the same way.
struct Args {
    b: Vec<u8>,
    user: usize,
    arg: u64,
}

impl Args {
    fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.b[o..o + 4].try_into().unwrap())
    }
    fn i32(&self, o: usize) -> i32 {
        self.u32(o) as i32
    }
    fn u64(&self, o: usize) -> u64 {
        u64::from_le_bytes(self.b[o..o + 8].try_into().unwrap())
    }
    fn set32(&mut self, o: usize, v: u32) {
        self.b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn set64(&mut self, o: usize, v: u64) {
        self.b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
}

fn copy_out(ptr: u64, data: &[u8]) -> KResult<()> {
    if data.is_empty() {
        return Ok(());
    }
    current().mm().copy_out(ptr, data)
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn u64s(v: &[u64]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Copy a list to user memory when the caller's count is large enough
/// (Linux's rule), then report the real count at `count_off`.
fn put_list(a: &mut Args, count_off: usize, ptr_off: usize, items: &[u8], n: usize) -> KResult<()> {
    if a.u32(count_off) as usize >= n {
        copy_out(a.u64(ptr_off), items)?;
    }
    a.set32(count_off, n as u32);
    Ok(())
}

fn read_u32s(ptr: u64, n: usize) -> KResult<Vec<u32>> {
    let mut b = vec![0u8; n * 4];
    if n > 0 {
        current().mm().copy_in(ptr, &mut b)?;
    }
    Ok(b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
}

fn read_u64s(ptr: u64, n: usize) -> KResult<Vec<u64>> {
    let mut b = vec![0u8; n * 8];
    if n > 0 {
        current().mm().copy_in(ptr, &mut b)?;
    }
    Ok(b.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect())
}

fn put_str(a: &mut Args, len_off: usize, ptr_off: usize, s: &str) -> KResult<()> {
    let n = (a.u64(len_off) as usize).min(s.len());
    copy_out(a.u64(ptr_off), &s.as_bytes()[..n])?;
    a.set64(len_off, s.len() as u64);
    Ok(())
}

fn name32(s: &str) -> [u8; 32] {
    let mut n = [0u8; 32];
    n[..s.len().min(31)].copy_from_slice(&s.as_bytes()[..s.len().min(31)]);
    n
}

/// struct drm_event_vblank.
fn vblank_event(ty: u32, user_data: u64, seq: u32, ts_ns: u64, crtc: u32) -> Vec<u8> {
    let mut e = vec![0u8; 32];
    e[0..4].copy_from_slice(&ty.to_le_bytes());
    e[4..8].copy_from_slice(&32u32.to_le_bytes());
    e[8..16].copy_from_slice(&user_data.to_le_bytes());
    e[16..20].copy_from_slice(&((ts_ns / 1_000_000_000) as u32).to_le_bytes());
    e[20..24].copy_from_slice(&((ts_ns % 1_000_000_000 / 1000) as u32).to_le_bytes());
    e[24..28].copy_from_slice(&seq.to_le_bytes());
    e[28..32].copy_from_slice(&crtc.to_le_bytes());
    e
}

impl DrmFile {
    fn dev(&self) -> &'static Device {
        DEV.get().unwrap()
    }

    fn queue_event(&self, e: Vec<u8>) {
        self.st.lock().events.push_back(e);
        self.wq.wake(POLLIN);
    }

    /// Reserve room for one event (EINVAL-free check before a commit).
    fn event_room(&self) -> KResult<()> {
        if self.st.lock().queued() + 32 > EVENT_SPACE { Err(ENOMEM) } else { Ok(()) }
    }

    fn kms_only(&self) -> KResult<()> {
        if self.render { Err(EACCES) } else { Ok(()) }
    }

    /// Check, apply and complete a commit: out-fence and flip event.
    fn commit(&self, k: &mut Kms, st: State, flags: u32, extra: &Extra, user_data: u64) -> KResult<()> {
        if flags & PAGE_FLIP_EVENT != 0 {
            // The event belongs to the CRTC, which must be in the commit.
            if !extra.crtc_touched {
                return Err(EINVAL);
            }
            self.event_room()?;
        }
        k.check(&st, flags & ATOMIC_ALLOW_MODESET != 0)?;
        if flags & ATOMIC_TEST_ONLY != 0 {
            return Ok(());
        }
        let active = st.active;
        k.apply(st)?;
        let (seq, ts) = vblank_now();
        if extra.out_fence_ptr != 0 {
            let fd = sync_file::signalled_fd(fence_context(), seq as u64, "virtio_gpu", "crtc-0")?;
            current().mm().copy_out(extra.out_fence_ptr, &fd.to_le_bytes())?;
        }
        if flags & PAGE_FLIP_EVENT != 0 && active {
            self.queue_event(vblank_event(EVENT_FLIP_COMPLETE, user_data, seq, ts, CRTC_ID));
        }
        Ok(())
    }

    fn ioctl_inner(&self, nr: u32, a: &mut Args) -> KResult<u64> {
        let dev = self.dev();
        match nr {
            // VERSION
            0x00 => {
                a.set32(0, 0);
                a.set32(4, 1);
                a.set32(8, 0);
                put_str(a, 16, 24, "virtio_gpu")?;
                put_str(a, 32, 40, "0")?;
                put_str(a, 48, 56, "virtio GPU")?;
            }
            // GET_UNIQUE
            0x01 => put_str(a, 0, 8, &dev.unique)?,
            // GET_MAGIC: any token; AUTH_MAGIC: everyone is authenticated.
            0x02 => a.set32(0, self.id as u32),
            0x11 => {}
            // SET_VERSION: report 1.4 for interface version queries.
            0x07 => {
                a.set32(0, 1);
                a.set32(4, 4);
                a.set32(8, 0);
                a.set32(12, 1);
            }
            // GEM_CLOSE
            0x09 => {
                self.st.lock().handles.remove(&a.u32(0)).ok_or(EINVAL)?;
            }
            // GEM_FLINK / GEM_OPEN: no global names.
            0x0a | 0x0b => return Err(ENODEV),
            // GET_CAP
            0x0c => {
                let v = match a.u64(0) {
                    CAP_DUMB_BUFFER => 1,
                    CAP_PRIME => 3,
                    CAP_TIMESTAMP_MONOTONIC => 1,
                    CAP_SYNCOBJ | CAP_SYNCOBJ_TIMELINE => 0,
                    _ if self.render => return Err(EINVAL),
                    CAP_VBLANK_HIGH_CRTC | CAP_CRTC_IN_VBLANK_EVENT => 1,
                    CAP_DUMB_PREFERRED_DEPTH => 24,
                    CAP_DUMB_PREFER_SHADOW | CAP_ASYNC_PAGE_FLIP | CAP_ADDFB2_MODIFIERS | CAP_PAGE_FLIP_TARGET => 0,
                    CAP_ATOMIC_ASYNC_PAGE_FLIP => 0,
                    CAP_CURSOR_WIDTH | CAP_CURSOR_HEIGHT => 64,
                    _ => return Err(EINVAL),
                };
                a.set64(8, v);
            }
            // SET_CLIENT_CAP
            0x0d => {
                let v = a.u64(8);
                let mut st = self.st.lock();
                match a.u64(0) {
                    CLIENT_CAP_STEREO_3D | CLIENT_CAP_ASPECT_RATIO if v <= 1 => {}
                    CLIENT_CAP_UNIVERSAL_PLANES if v <= 1 => st.universal = v == 1,
                    CLIENT_CAP_ATOMIC if v <= 2 => {
                        st.atomic = v >= 1;
                        if v >= 1 {
                            st.universal = true;
                        }
                    }
                    CLIENT_CAP_WRITEBACK_CONNECTORS | CLIENT_CAP_CURSOR_PLANE_HOTSPOT => return Err(EINVAL),
                    _ => return Err(EINVAL),
                }
            }
            // SET_MASTER / DROP_MASTER
            0x1e => {
                self.kms_only()?;
                let m = dev.master.load(Ordering::Acquire);
                if m != 0 && m != self.id {
                    return Err(EBUSY);
                }
                dev.master.store(self.id, Ordering::Release);
            }
            0x1f => {
                let _ = dev.master.compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Relaxed);
            }
            // PRIME_HANDLE_TO_FD
            0x2d => {
                let flags = a.u32(4);
                if flags & !(DRM_CLOEXEC | DRM_RDWR) != 0 {
                    return Err(EINVAL);
                }
                let buf = self.st.lock().buffer(a.u32(0))?;
                let fd = dma_buf::export_fd(&buf, flags)?;
                a.set32(8, fd as u32);
            }
            // PRIME_FD_TO_HANDLE
            0x2e => {
                let buf = dma_buf::get_fd(a.i32(8))?;
                let h = self.st.lock().handle_of(&buf);
                a.set32(0, h);
            }
            // WAIT_VBLANK
            0x3a => return self.wait_vblank(a),
            // CRTC_GET_SEQUENCE
            0x3b => {
                self.kms_only()?;
                if a.u32(0) != CRTC_ID {
                    return Err(ENOENT);
                }
                let (seq, ts) = vblank_now();
                a.set32(4, dev.kms.lock().state.active as u32);
                a.set64(8, seq as u64);
                a.set64(16, ts);
            }
            0xa0..=0xff => return self.mode_ioctl(nr, a),
            // virtgpu
            0x41 => {
                // VIRTGPU_MAP: same as MAP_DUMB.
                let off = self.map_offset(a.u32(8))?;
                a.set64(0, off);
            }
            0x42 | 0x44 | 0x46 | 0x47 | 0x49 | 0x4a | 0x4b => return Err(ENOSYS),
            0x43 => {
                // VIRTGPU_GETPARAM: writes an int to the user pointer.
                let p = a.u64(0);
                let v = *VIRTGPU_PARAMS.get((p as usize).wrapping_sub(1)).ok_or(EINVAL)?;
                copy_out(a.u64(8), &v.to_le_bytes())?;
            }
            0x45 => {
                // VIRTGPU_RESOURCE_INFO: no host resource until shown.
                let buf = self.st.lock().buffer(a.u32(0))?;
                a.set32(4, 0);
                a.set32(8, buf.size as u32);
                a.set32(12, 0);
            }
            0x48 => {
                // VIRTGPU_WAIT: 2D work is synchronous.
                self.st.lock().buffer(a.u32(0))?;
            }
            _ => return Err(EINVAL),
        }
        Ok(0)
    }

    fn map_offset(&self, handle: u32) -> KResult<u64> {
        let buf = self.st.lock().buffer(handle)?;
        let mut off = buf.drm_offset.lock();
        if *off == 0 {
            *off = NEXT_MMAP.fetch_add(buf.size, Ordering::Relaxed);
        }
        Ok(*off)
    }

    fn wait_vblank(&self, a: &mut Args) -> KResult<u64> {
        self.kms_only()?;
        let ty = a.u32(0);
        if ty & !VBLANK_FLAGS != 0 || ty & VBLANK_OTHER_CRTC != 0 {
            // Secondary/high CRTC selectors: there is only CRTC 0.
            return Err(EINVAL);
        }
        if !self.dev().kms.lock().state.active {
            return Err(EINVAL);
        }
        let (cur, _) = vblank_now();
        let target = if ty & VBLANK_RELATIVE != 0 { cur.wrapping_add(a.u32(4)) } else { a.u32(4) };
        if ty & VBLANK_EVENT != 0 {
            self.event_room()?;
            let epoch = *VBLANK_EPOCH.get().unwrap();
            let when = epoch + target.max(cur) as u64 * FRAME_NS;
            // Delivered at once with the target's timestamp.
            self.queue_event(vblank_event(EVENT_VBLANK, a.u64(8), target.max(cur), when, CRTC_ID));
            a.set32(4, target);
            return Ok(0);
        }
        if (target.wrapping_sub(cur) as i32) > 0 {
            let epoch = *VBLANK_EPOCH.get().unwrap();
            let deadline = epoch + target as u64 * FRAME_NS;
            if let Wake::Interrupted = sched::wait_until((VBLANK_KEY, self.id), Some(deadline), WaitMode::Interruptible, || false) {
                return Err(EINTR);
            }
        }
        let (seq, ts) = vblank_now();
        a.set32(4, seq);
        a.set64(8, ts / 1_000_000_000);
        a.set64(16, ts % 1_000_000_000 / 1000);
        Ok(0)
    }

    fn mode_ioctl(&self, nr: u32, a: &mut Args) -> KResult<u64> {
        let dev = self.dev();
        match nr {
            // CREATE_DUMB
            0xb2 => {
                let (h, w, bpp) = (a.u32(0), a.u32(4), a.u32(8));
                if w == 0 || h == 0 || w > 16384 || h > 16384 || bpp == 0 || bpp > 32 || bpp % 8 != 0 {
                    return Err(EINVAL);
                }
                let pitch = w * (bpp / 8);
                let size = (pitch as u64 * h as u64).next_multiple_of(PAGE_SIZE);
                let buf = Buffer::new(size, "virtio_gpu")?;
                let handle = self.st.lock().handle_of(&buf);
                a.set32(16, handle);
                a.set32(20, pitch);
                a.set64(24, size);
                return Ok(0);
            }
            // MAP_DUMB
            0xb3 => {
                let off = self.map_offset(a.u32(0))?;
                a.set64(8, off);
                return Ok(0);
            }
            // DESTROY_DUMB
            0xb4 => {
                self.st.lock().handles.remove(&a.u32(0)).ok_or(EINVAL)?;
                return Ok(0);
            }
            _ => {}
        }
        self.kms_only()?;
        let (atomic, universal) = {
            let s = self.st.lock();
            (s.atomic, s.universal)
        };
        match nr {
            // GETRESOURCES
            0xa0 => {
                let k = dev.kms.lock();
                let fbs: Vec<u32> = k.fbs.values().filter(|f| f.owner == self.id).map(|f| f.id).collect();
                put_list(a, 32, 0, &u32s(&fbs), fbs.len())?;
                put_list(a, 36, 8, &u32s(&[CRTC_ID]), 1)?;
                put_list(a, 40, 16, &u32s(&[CONNECTOR_ID]), 1)?;
                put_list(a, 44, 24, &u32s(&[ENCODER_ID]), 1)?;
                for (o, v) in [(48, 0), (52, 16384), (56, 0), (60, 16384)] {
                    a.set32(o, v);
                }
            }
            // GETCRTC
            0xa1 => {
                if a.u32(12) != CRTC_ID {
                    return Err(ENOENT);
                }
                let k = dev.kms.lock();
                let s = &k.state;
                let p = &s.plane;
                a.set32(16, p.fb.as_ref().map_or(0, |f| f.id));
                a.set32(20, p.src_x >> 16);
                a.set32(24, p.src_y >> 16);
                a.set32(28, 0);
                match (&s.mode, s.active) {
                    (Some(m), true) => {
                        a.set32(32, 1);
                        a.b[36..36 + MODE_SIZE].copy_from_slice(&m.data);
                    }
                    _ => {
                        a.set32(32, 0);
                        a.b[36..36 + MODE_SIZE].fill(0);
                    }
                }
            }
            // SETCRTC
            0xa2 => return self.setcrtc(a),
            // CURSOR, CURSOR2: no cursor plane.
            0xa3 | 0xbb => return Err(ENXIO),
            // GETGAMMA / SETGAMMA: gamma size 0.
            0xa4 | 0xa5 => {
                if a.u32(0) != CRTC_ID {
                    return Err(ENOENT);
                }
                if a.u32(4) != 0 {
                    return Err(EINVAL);
                }
            }
            // GETENCODER
            0xa6 => {
                if a.u32(0) != ENCODER_ID {
                    return Err(ENOENT);
                }
                let k = dev.kms.lock();
                a.set32(4, ENCODER_VIRTUAL);
                a.set32(8, if k.state.conn_crtc != 0 { CRTC_ID } else { 0 });
                a.set32(12, 1);
                a.set32(16, 0);
            }
            // GETCONNECTOR
            0xa7 => {
                if a.u32(48) != CONNECTOR_ID {
                    return Err(ENOENT);
                }
                let k = dev.kms.lock();
                let mode = k.mode;
                put_list(a, 40, 0, &u32s(&[ENCODER_ID]), 1)?;
                put_list(a, 32, 8, &mode, 1)?;
                let props = visible_props(CONNECTOR_ID, atomic);
                let vals: Vec<u64> = props.iter().map(|&p| k.get_prop(CONNECTOR_ID, p)).collect();
                let n = props.len();
                if a.u32(36) as usize >= n {
                    copy_out(a.u64(16), &u32s(&props))?;
                    copy_out(a.u64(24), &u64s(&vals))?;
                }
                a.set32(36, n as u32);
                a.set32(44, if k.state.conn_crtc != 0 { ENCODER_ID } else { 0 });
                a.set32(52, CONNECTOR_VIRTUAL);
                a.set32(56, 1);
                a.set32(60, 1); // connected
                a.set32(64, k.mm.0);
                a.set32(68, k.mm.1);
                a.set32(72, 1); // SubPixelUnknown
            }
            // GETPROPERTY
            0xaa => {
                let def = prop(a.u32(16)).ok_or(ENOENT)?;
                a.set32(20, def.flags());
                a.b[24..56].copy_from_slice(&name32(def.name));
                let (vals, enums): (Vec<u64>, Vec<u8>) = match def.kind {
                    Kind::Range(lo, hi) => (vec![lo, hi], Vec::new()),
                    Kind::SignedRange(lo, hi) => (vec![lo as u64, hi as u64], Vec::new()),
                    Kind::Object(t) => (vec![t as u64], Vec::new()),
                    Kind::Blob => (Vec::new(), Vec::new()),
                    Kind::Enum(e) => {
                        let mut b = Vec::new();
                        for (v, n) in e {
                            b.extend_from_slice(&v.to_le_bytes());
                            b.extend_from_slice(&name32(n));
                        }
                        (e.iter().map(|x| x.0).collect(), b)
                    }
                };
                let ne = enums.len() / 40;
                put_list(a, 56, 0, &u64s(&vals), vals.len())?;
                put_list(a, 60, 8, &enums, ne)?;
            }
            // SETPROPERTY (connector, legacy)
            0xab => {
                let (v, prop_id, obj) = (a.u64(0), a.u32(8), a.u32(12));
                if obj != CONNECTOR_ID {
                    return Err(ENOENT);
                }
                return self.set_one_prop(obj, prop_id, v, atomic);
            }
            // GETPROPBLOB
            0xac => {
                let k = dev.kms.lock();
                let b = k.blob(a.u32(0)).ok_or(ENOENT)?;
                if a.u32(4) as usize >= b.data.len() {
                    copy_out(a.u64(8), &b.data)?;
                }
                a.set32(4, b.data.len() as u32);
            }
            // GETFB
            0xad => {
                let k = dev.kms.lock();
                let fb = k.fbs.get(&a.u32(0)).ok_or(ENOENT)?;
                a.set32(4, fb.width);
                a.set32(8, fb.height);
                a.set32(12, fb.pitch);
                a.set32(16, 32);
                a.set32(20, if fb.format == FMT_ARGB8888 { 32 } else { 24 });
                let h = if fb.owner == self.id { fb.handle } else { self.st.lock().handle_of(&fb.buf) };
                a.set32(24, h);
            }
            // ADDFB
            0xae => {
                let (w, h, pitch, bpp, depth, handle) = (a.u32(4), a.u32(8), a.u32(12), a.u32(16), a.u32(20), a.u32(24));
                let format = match (bpp, depth) {
                    (32, 24) => FMT_XRGB8888,
                    (32, 32) => FMT_ARGB8888,
                    _ => return Err(EINVAL),
                };
                let id = self.add_fb(w, h, format, handle, pitch, 0)?;
                a.set32(0, id);
            }
            // RMFB
            0xaf => {
                let mut k = dev.kms.lock();
                let id = a.u32(0);
                match k.fbs.get(&id) {
                    Some(f) if f.owner == self.id => {}
                    _ => return Err(ENOENT),
                }
                let fb = k.fbs.remove(&id).unwrap();
                disable_fb(&mut k, &fb);
            }
            // PAGE_FLIP
            0xb0 => {
                let (crtc, fb_id, flags, user_data) = (a.u32(0), a.u32(4), a.u32(8), a.u64(16));
                if crtc != CRTC_ID {
                    return Err(ENOENT);
                }
                if flags & !(PAGE_FLIP_EVENT | PAGE_FLIP_ASYNC) != 0 || flags & PAGE_FLIP_ASYNC != 0 {
                    return Err(EINVAL);
                }
                let mut k = dev.kms.lock();
                if !k.state.active || k.state.plane.fb.is_none() {
                    return Err(EINVAL);
                }
                let fb = k.fbs.get(&fb_id).cloned().ok_or(ENOENT)?;
                let mut st = k.state.clone();
                st.plane.fb = Some(fb);
                let extra = Extra { in_fence: -1, crtc_touched: true, ..Default::default() };
                self.commit(&mut k, st, flags & PAGE_FLIP_EVENT, &extra, user_data)?;
            }
            // DIRTYFB
            0xb1 => {
                let mut k = dev.kms.lock();
                if !k.fbs.contains_key(&a.u32(0)) {
                    return Err(ENOENT);
                }
                k.dirty(a.u32(0))?;
            }
            // GETPLANERESOURCES: the primary plane is universal-only.
            0xb5 => {
                let planes: &[u32] = if universal { &[PLANE_ID] } else { &[] };
                put_list(a, 8, 0, &u32s(planes), planes.len())?;
            }
            // GETPLANE
            0xb6 => {
                if a.u32(0) != PLANE_ID {
                    return Err(ENOENT);
                }
                let k = dev.kms.lock();
                let p = &k.state.plane;
                a.set32(4, p.crtc);
                a.set32(8, p.fb.as_ref().map_or(0, |f| f.id));
                a.set32(12, 1);
                a.set32(16, 0);
                let fmts: Vec<u32> = FORMATS.iter().map(|f| f.0).collect();
                put_list(a, 20, 24, &u32s(&fmts), fmts.len())?;
            }
            // SETPLANE
            0xb7 => return self.setplane(a),
            // ADDFB2
            0xb8 => {
                let (w, h, format, flags) = (a.u32(4), a.u32(8), a.u32(12), a.u32(16));
                if flags & !2 != 0 || flags & 2 != 0 && a.u64(72) != 0 {
                    // Only DRM_MODE_FB_MODIFIERS with the linear modifier.
                    return Err(EINVAL);
                }
                let id = self.add_fb(w, h, format, a.u32(20), a.u32(36), a.u32(52))?;
                a.set32(0, id);
            }
            // OBJ_GETPROPERTIES
            0xb9 => {
                let (obj, ty) = (a.u32(20), a.u32(24));
                let (oty, _) = obj_props(obj).ok_or(ENOENT)?;
                if ty != 0 && ty != oty {
                    return Err(ENOENT);
                }
                let props = visible_props(obj, atomic);
                let k = dev.kms.lock();
                let vals: Vec<u64> = props.iter().map(|&p| k.get_prop(obj, p)).collect();
                let n = props.len();
                if a.u32(16) as usize >= n {
                    copy_out(a.u64(0), &u32s(&props))?;
                    copy_out(a.u64(8), &u64s(&vals))?;
                }
                a.set32(16, n as u32);
            }
            // OBJ_SETPROPERTY
            0xba => {
                let (v, prop_id, obj, ty) = (a.u64(0), a.u32(8), a.u32(12), a.u32(16));
                let (oty, _) = obj_props(obj).ok_or(ENOENT)?;
                if ty != 0 && ty != oty {
                    return Err(ENOENT);
                }
                return self.set_one_prop(obj, prop_id, v, atomic);
            }
            // ATOMIC
            0xbc => return self.atomic(a, atomic),
            // CREATEPROPBLOB
            0xbd => {
                let len = a.u32(8) as usize;
                if len == 0 || len > 1 << 20 {
                    return Err(EINVAL);
                }
                let mut data = vec![0u8; len];
                current().mm().copy_in(a.u64(0), &mut data)?;
                let id = dev.kms.lock().add_blob(data, self.id).id;
                a.set32(12, id);
            }
            // DESTROYPROPBLOB
            0xbe => {
                let mut k = dev.kms.lock();
                match k.blobs.get(&a.u32(0)) {
                    Some(b) if b.owner == self.id => {}
                    Some(_) => return Err(EPERM),
                    None => return Err(ENOENT),
                }
                k.blobs.remove(&a.u32(0));
            }
            // GETFB2
            0xce => {
                let k = dev.kms.lock();
                let fb = k.fbs.get(&a.u32(0)).cloned().ok_or(ENOENT)?;
                drop(k);
                a.set32(4, fb.width);
                a.set32(8, fb.height);
                a.set32(12, fb.format);
                a.set32(16, 0);
                let h = if fb.owner == self.id { fb.handle } else { self.st.lock().handle_of(&fb.buf) };
                for i in 0..4 {
                    a.set32(20 + 4 * i, 0);
                    a.set32(36 + 4 * i, 0);
                    a.set32(52 + 4 * i, 0);
                }
                a.set32(20, h);
                a.set32(36, fb.pitch);
                a.set32(52, fb.offset);
            }
            _ => return Err(EINVAL),
        }
        Ok(0)
    }

    fn add_fb(&self, w: u32, h: u32, format: u32, handle: u32, pitch: u32, offset: u32) -> KResult<u32> {
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(EINVAL);
        }
        if virtio_format(format).is_none() {
            return Err(EINVAL);
        }
        let buf = self.st.lock().buffer(handle)?;
        if pitch < w * 4 || pitch % 4 != 0 {
            return Err(EINVAL);
        }
        if offset as u64 + pitch as u64 * (h as u64 - 1) + w as u64 * 4 > buf.size {
            return Err(EINVAL);
        }
        let mut k = self.dev().kms.lock();
        let id = k.alloc_id();
        k.fbs.insert(id, Arc::new(Fb::new(id, w, h, format, pitch, offset, buf, handle, self.id)));
        Ok(id)
    }

    fn set_one_prop(&self, obj: u32, prop_id: u32, v: u64, atomic: bool) -> KResult<u64> {
        let def = prop(prop_id).ok_or(ENOENT)?;
        if def.atomic && !atomic {
            return Err(EINVAL);
        }
        let mut k = self.dev().kms.lock();
        let mut st = k.state.clone();
        let mut extra = Extra { in_fence: -1, ..Default::default() };
        k.set_prop(&mut st, &mut extra, obj, prop_id, v)?;
        if extra.in_fence >= 0 || extra.out_fence_ptr != 0 {
            return Err(EINVAL);
        }
        self.commit(&mut k, st, ATOMIC_ALLOW_MODESET, &extra, 0)?;
        Ok(0)
    }

    fn setcrtc(&self, a: &mut Args) -> KResult<u64> {
        if a.u32(12) != CRTC_ID {
            return Err(ENOENT);
        }
        let (count, fb_id, x, y, mode_valid) = (a.u32(8), a.u32(16), a.u32(20), a.u32(24), a.u32(32));
        let conns = read_u32s(a.u64(0), count.min(16) as usize)?;
        if conns.iter().any(|&c| c != CONNECTOR_ID) {
            return Err(ENOENT);
        }
        let mut k = self.dev().kms.lock();
        let mut st = k.state.clone();
        if mode_valid == 0 {
            if fb_id != 0 || count != 0 {
                return Err(EINVAL);
            }
            st = State { dpms: st.dpms, ..Default::default() };
        } else {
            let mode = a.b[36..36 + MODE_SIZE].to_vec();
            let (w, h) = mode_size(&mode);
            let fb = if fb_id == u32::MAX { st.plane.fb.clone().ok_or(EINVAL)? } else { k.fbs.get(&fb_id).cloned().ok_or(ENOENT)? };
            if x.checked_add(w).is_none_or(|e| e > fb.width) || y.checked_add(h).is_none_or(|e| e > fb.height) {
                return Err(ENOSPC);
            }
            let same = st.mode.as_ref().is_some_and(|m| m.data == mode);
            if !same {
                st.mode = Some(Arc::new(Blob { id: k.alloc_id(), data: mode, owner: 0 }));
            }
            st.active = true;
            st.conn_crtc = if count > 0 { CRTC_ID } else { st.conn_crtc };
            st.plane = PlaneState { fb: Some(fb), crtc: CRTC_ID, crtc_x: 0, crtc_y: 0, crtc_w: w, crtc_h: h, src_x: x << 16, src_y: y << 16, src_w: w << 16, src_h: h << 16 };
        }
        let extra = Extra { in_fence: -1, crtc_touched: true, ..Default::default() };
        self.commit(&mut k, st, ATOMIC_ALLOW_MODESET, &extra, 0)?;
        Ok(0)
    }

    fn setplane(&self, a: &mut Args) -> KResult<u64> {
        if a.u32(0) != PLANE_ID {
            return Err(ENOENT);
        }
        let (crtc, fb_id) = (a.u32(4), a.u32(8));
        let mut k = self.dev().kms.lock();
        let mut st = k.state.clone();
        if fb_id == 0 {
            st.plane = PlaneState::default();
        } else {
            if crtc != CRTC_ID {
                return Err(ENOENT);
            }
            let fb = k.fbs.get(&fb_id).cloned().ok_or(ENOENT)?;
            st.plane = PlaneState {
                fb: Some(fb),
                crtc,
                crtc_x: a.i32(16),
                crtc_y: a.i32(20),
                crtc_w: a.u32(24),
                crtc_h: a.u32(28),
                src_x: a.u32(32),
                src_y: a.u32(36),
                src_h: a.u32(40),
                src_w: a.u32(44),
            };
        }
        let extra = Extra { in_fence: -1, ..Default::default() };
        self.commit(&mut k, st, 0, &extra, 0)?;
        Ok(0)
    }

    fn atomic(&self, a: &mut Args, atomic: bool) -> KResult<u64> {
        if !atomic {
            return Err(EINVAL);
        }
        let flags = a.u32(0);
        if flags & !ATOMIC_FLAGS != 0 || flags & PAGE_FLIP_ASYNC != 0 {
            return Err(EINVAL);
        }
        if flags & ATOMIC_TEST_ONLY != 0 && flags & PAGE_FLIP_EVENT != 0 {
            return Err(EINVAL);
        }
        let nobj = a.u32(4) as usize;
        if nobj > 64 {
            return Err(EINVAL);
        }
        let objs = read_u32s(a.u64(8), nobj)?;
        let counts = read_u32s(a.u64(16), nobj)?;
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total > 1024 {
            return Err(EINVAL);
        }
        let props = read_u32s(a.u64(24), total)?;
        let vals = read_u64s(a.u64(32), total)?;
        // Wait for in-fences before taking the device lock.
        for (i, &p) in props.iter().enumerate() {
            if p == P_IN_FENCE_FD && (vals[i] as i64) >= 0 {
                let f = sync_file::get_fd(vals[i] as i32).map_err(|_| EINVAL)?;
                let sf = f.ops.as_any().downcast_ref::<sync_file::SyncFile>().unwrap();
                sf.wait(None)?;
            }
        }
        let mut k = self.dev().kms.lock();
        let mut st = k.state.clone();
        let mut extra = Extra { in_fence: -1, ..Default::default() };
        let mut i = 0;
        for (o, &obj) in objs.iter().enumerate() {
            if obj_props(obj).is_none() {
                return Err(ENOENT);
            }
            for _ in 0..counts[o] {
                k.set_prop(&mut st, &mut extra, obj, props[i], vals[i])?;
                i += 1;
            }
        }
        if extra.out_fence_ptr != 0 {
            // Linux writes -1 first, so a failed commit leaves -1.
            current().mm().copy_out(extra.out_fence_ptr, &(-1i32).to_le_bytes())?;
        }
        self.commit(&mut k, st, flags, &extra, a.u64(48))?;
        Ok(0)
    }
}

/// Visible properties of `obj` for a client with or without the atomic
/// cap.
fn visible_props(obj: u32, atomic: bool) -> Vec<u32> {
    obj_props(obj).map_or(Vec::new(), |(_, l)| l.iter().copied().filter(|&p| atomic || !prop(p).unwrap().atomic).collect())
}

/// Removing a framebuffer that is shown turns the plane and the CRTC off
/// (Linux's drm_framebuffer_remove).
fn disable_fb(k: &mut Kms, fb: &Arc<Fb>) {
    if k.state.plane.fb.as_ref().is_some_and(|f| Arc::ptr_eq(f, fb)) {
        let st = State { dpms: k.state.dpms, ..Default::default() };
        let _ = k.apply(st);
    }
}

fn fence_context() -> u64 {
    static CTX: Once<u64> = Once::new();
    *CTX.call_once(sync_file::new_context)
}

impl FileOps for DrmFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut out = 0;
        let w = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || !self.st.lock().events.is_empty() || f.nonblock());
        if let Wake::Interrupted = w {
            return Err(ERESTARTSYS);
        }
        let mut st = self.st.lock();
        if st.events.is_empty() {
            return Err(EAGAIN);
        }
        while let Some(e) = st.events.front() {
            if out + e.len() > buf.len() {
                break;
            }
            buf[out..out + e.len()].copy_from_slice(e);
            out += e.len();
            st.events.pop_front();
        }
        if out == 0 {
            return Err(EINVAL);
        }
        Ok(out)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if (cmd >> 8) & 0xff != 0x64 {
            return Err(ENOTTY);
        }
        let nr = cmd & 0xff;
        let dir = cmd >> 30;
        let size = ((cmd >> 16) & 0x3fff) as usize;
        let mut a = Args { b: vec![0u8; size.max(128)], user: size, arg };
        if dir & 1 != 0 && size > 0 {
            current().mm().copy_in(arg, &mut a.b[..size])?;
        }
        let r = self.ioctl_inner(nr, &mut a)?;
        if dir & 2 != 0 && a.user > 0 {
            current().mm().copy_out(a.arg, &a.b[..a.user])?;
        }
        Ok(r)
    }

    fn mmap_shmem(&self, _: &OpenFile, _shared_write: bool, pgoff: u64) -> KResult<Option<(Arc<Shmem>, u64)>> {
        let off = pgoff * PAGE_SIZE;
        let st = self.st.lock();
        for b in st.handles.values() {
            let start = *b.drm_offset.lock();
            if start != 0 && off >= start && off < start + b.size {
                return Ok(Some((b.shmem.clone(), (off - start) / PAGE_SIZE)));
            }
        }
        Err(EINVAL)
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let st = self.st.lock();
        let mut m = 0;
        if !st.events.is_empty() {
            m |= POLLIN | POLLRDNORM;
        }
        if st.queued() < EVENT_SPACE {
            m |= POLLOUT | POLLWRNORM;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for DrmFile {
    /// Closing a file removes its framebuffers and blobs (turning the
    /// display off if one was shown) and drops master.
    fn drop(&mut self) {
        let Some(dev) = DEV.get() else { return };
        let _ = dev.master.compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Relaxed);
        let mut k = dev.kms.lock();
        let mine: Vec<u32> = k.fbs.values().filter(|f| f.owner == self.id).map(|f| f.id).collect();
        for id in mine {
            let fb = k.fbs.remove(&id).unwrap();
            disable_fb(&mut k, &fb);
        }
        k.blobs.retain(|_, b| b.owner != self.id);
    }
}
