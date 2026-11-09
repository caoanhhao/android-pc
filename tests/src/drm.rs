//! DRM, dma-buf and sync_file ioctl helpers for the D1 tests (raw
//! structs as libdrm lays them out).

use crate::sys::*;

/// _IOWR('d', nr, size).
pub const fn iowr(nr: u32, size: usize) -> u32 {
    0xc000_0000 | (size as u32) << 16 | 0x6400 | nr
}

/// _IOW('d', nr, size).
pub const fn iow(nr: u32, size: usize) -> u32 {
    0x4000_0000 | (size as u32) << 16 | 0x6400 | nr
}

pub const VERSION: u32 = iowr(0x00, 64);
pub const GET_UNIQUE: u32 = iowr(0x01, 16);
pub const GEM_CLOSE: u32 = iow(0x09, 8);
pub const GET_CAP: u32 = iowr(0x0c, 16);
pub const SET_CLIENT_CAP: u32 = iow(0x0d, 16);
pub const PRIME_HANDLE_TO_FD: u32 = iowr(0x2d, 12);
pub const PRIME_FD_TO_HANDLE: u32 = iowr(0x2e, 12);
pub const WAIT_VBLANK: u32 = iowr(0x3a, 24);
pub const MODE_GETRESOURCES: u32 = iowr(0xa0, 64);
pub const MODE_GETCRTC: u32 = iowr(0xa1, 104);
pub const MODE_SETCRTC: u32 = iowr(0xa2, 104);
pub const MODE_GETENCODER: u32 = iowr(0xa6, 20);
pub const MODE_GETCONNECTOR: u32 = iowr(0xa7, 80);
pub const MODE_GETPROPERTY: u32 = iowr(0xaa, 64);
pub const MODE_GETPROPBLOB: u32 = iowr(0xac, 16);
pub const MODE_GETFB: u32 = iowr(0xad, 28);
pub const MODE_ADDFB: u32 = iowr(0xae, 28);
pub const MODE_RMFB: u32 = iowr(0xaf, 4);
pub const MODE_PAGE_FLIP: u32 = iowr(0xb0, 24);
pub const MODE_DIRTYFB: u32 = iowr(0xb1, 24);
pub const MODE_CREATE_DUMB: u32 = iowr(0xb2, 32);
pub const MODE_MAP_DUMB: u32 = iowr(0xb3, 16);
pub const MODE_DESTROY_DUMB: u32 = iowr(0xb4, 4);
pub const MODE_GETPLANERESOURCES: u32 = iowr(0xb5, 16);
pub const MODE_GETPLANE: u32 = iowr(0xb6, 32);
pub const MODE_ADDFB2: u32 = iowr(0xb8, 104);
pub const MODE_OBJ_GETPROPERTIES: u32 = iowr(0xb9, 32);
pub const MODE_ATOMIC: u32 = iowr(0xbc, 56);
pub const MODE_CREATEPROPBLOB: u32 = iowr(0xbd, 16);
pub const MODE_DESTROYPROPBLOB: u32 = iowr(0xbe, 4);

pub const CAP_DUMB_BUFFER: u64 = 1;
pub const CAP_PRIME: u64 = 5;
pub const CAP_CURSOR_WIDTH: u64 = 8;
pub const CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
pub const CLIENT_CAP_ATOMIC: u64 = 3;

pub const OBJ_CRTC: u32 = 0xcccc_cccc;
pub const OBJ_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const OBJ_PLANE: u32 = 0xeeee_eeee;

pub const FMT_XRGB8888: u32 = 0x3432_5258;
pub const FMT_ABGR8888: u32 = 0x3432_4241;

pub const PAGE_FLIP_EVENT: u32 = 1;
pub const ATOMIC_TEST_ONLY: u32 = 0x100;
pub const ATOMIC_NONBLOCK: u32 = 0x200;
pub const ATOMIC_ALLOW_MODESET: u32 = 0x400;

pub const DMA_BUF_IOCTL_SYNC: u32 = 0x4008_6200;
pub const DMA_BUF_SET_NAME: u32 = 0x4008_6201;
pub const DMA_BUF_EXPORT_SYNC_FILE: u32 = 0xc008_6202;
pub const DMA_HEAP_IOCTL_ALLOC: u32 = 0xc018_4800;
pub const SYNC_IOC_MERGE: u32 = 0xc030_3e03;
pub const SYNC_IOC_FILE_INFO: u32 = 0xc038_3e04;
pub const SW_SYNC_IOC_CREATE_FENCE: u32 = 0xc028_5700;
pub const SW_SYNC_IOC_INC: u32 = 0x4004_5701;

/// Little-endian struct buffer.
pub struct S<const N: usize>(pub [u8; N]);

impl<const N: usize> S<N> {
    pub fn new() -> Self {
        S([0; N])
    }
    pub fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.0[o..o + 4].try_into().unwrap())
    }
    pub fn u64(&self, o: usize) -> u64 {
        u64::from_le_bytes(self.0[o..o + 8].try_into().unwrap())
    }
    pub fn set32(&mut self, o: usize, v: u32) -> &mut Self {
        self.0[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }
    pub fn set64(&mut self, o: usize, v: u64) -> &mut Self {
        self.0[o..o + 8].copy_from_slice(&v.to_le_bytes());
        self
    }
    pub fn ptr(&mut self) -> usize {
        self.0.as_mut_ptr() as usize
    }
    /// ioctl(fd, cmd, self).
    pub fn io(&mut self, fd: i32, cmd: u32) -> isize {
        ioctl(fd, cmd, self.ptr())
    }
}

impl<const N: usize> Default for S<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The driver name (DRM_IOCTL_VERSION) into `name`; returns its length.
pub fn version(fd: i32, name: &mut [u8; 32]) -> isize {
    let mut v = S::<64>::new();
    v.set64(16, 32).set64(24, name.as_mut_ptr() as u64);
    let r = v.io(fd, VERSION);
    if r < 0 { r } else { v.u64(16) as isize }
}

/// Open a DRM node of the virtio_gpu driver (None: not there, e.g. on a
/// host without virtio-gpu).
pub fn open_virtio(path: &core::ffi::CStr) -> Option<i32> {
    let fd = open(path.as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    if fd < 0 {
        return None;
    }
    let mut name = [0u8; 32];
    let n = version(fd, &mut name);
    if n == 10 && &name[..10] == b"virtio_gpu" {
        Some(fd)
    } else {
        close(fd);
        None
    }
}

pub fn get_cap(fd: i32, cap: u64) -> Result<u64, isize> {
    let mut c = S::<16>::new();
    c.set64(0, cap);
    let r = c.io(fd, GET_CAP);
    if r < 0 { Err(r) } else { Ok(c.u64(8)) }
}

pub fn set_client_cap(fd: i32, cap: u64, v: u64) -> isize {
    let mut c = S::<16>::new();
    c.set64(0, cap).set64(8, v);
    c.io(fd, SET_CLIENT_CAP)
}

/// CREATE_DUMB: Ok((handle, pitch, size)).
pub fn create_dumb(fd: i32, w: u32, h: u32, bpp: u32) -> Result<(u32, u32, u64), isize> {
    let mut c = S::<32>::new();
    c.set32(0, h).set32(4, w).set32(8, bpp);
    let r = c.io(fd, MODE_CREATE_DUMB);
    if r < 0 { Err(r) } else { Ok((c.u32(16), c.u32(20), c.u64(24))) }
}

pub fn map_dumb(fd: i32, handle: u32) -> Result<u64, isize> {
    let mut m = S::<16>::new();
    m.set32(0, handle);
    let r = m.io(fd, MODE_MAP_DUMB);
    if r < 0 { Err(r) } else { Ok(m.u64(8)) }
}

pub fn gem_close(fd: i32, handle: u32) -> isize {
    let mut c = S::<8>::new();
    c.set32(0, handle);
    c.io(fd, GEM_CLOSE)
}

/// mmap a dumb buffer through its fake offset (MAP_SHARED, RW).
pub fn mmap_dumb(fd: i32, handle: u32, size: u64) -> Result<*mut u8, isize> {
    let off = map_dumb(fd, handle)?;
    let p = mmap(0, size as usize, PROT_READ | PROT_WRITE, MAP_SHARED, fd, off as i64);
    if p < 0 { Err(p) } else { Ok(p as *mut u8) }
}

pub fn prime_to_fd(fd: i32, handle: u32, flags: u32) -> isize {
    let mut p = S::<12>::new();
    p.set32(0, handle).set32(4, flags);
    let r = p.io(fd, PRIME_HANDLE_TO_FD);
    if r < 0 { r } else { p.u32(8) as i32 as isize }
}

pub fn prime_to_handle(fd: i32, dmabuf: i32) -> isize {
    let mut p = S::<12>::new();
    p.set32(8, dmabuf as u32);
    let r = p.io(fd, PRIME_FD_TO_HANDLE);
    if r < 0 { r } else { p.u32(0) as isize }
}

/// ADDFB2 with one plane; Ok(fb id).
pub fn add_fb2(fd: i32, w: u32, h: u32, fmt: u32, handle: u32, pitch: u32, offset: u32) -> Result<u32, isize> {
    let mut f = S::<104>::new();
    f.set32(4, w).set32(8, h).set32(12, fmt).set32(20, handle).set32(36, pitch).set32(52, offset);
    let r = f.io(fd, MODE_ADDFB2);
    if r < 0 { Err(r) } else { Ok(f.u32(0)) }
}

pub fn rm_fb(fd: i32, id: u32) -> isize {
    let mut f = S::<4>::new();
    f.set32(0, id);
    f.io(fd, MODE_RMFB)
}

/// GETRESOURCES: (crtcs, connectors, encoders, fbs) ids, up to 8 each.
pub struct Res {
    pub fbs: [u32; 8],
    pub crtcs: [u32; 8],
    pub conns: [u32; 8],
    pub encs: [u32; 8],
    pub n: [u32; 4],
}

pub fn get_resources(fd: i32) -> Result<Res, isize> {
    let mut res = Res { fbs: [0; 8], crtcs: [0; 8], conns: [0; 8], encs: [0; 8], n: [0; 4] };
    let mut r = S::<64>::new();
    r.set64(0, res.fbs.as_mut_ptr() as u64).set64(8, res.crtcs.as_mut_ptr() as u64);
    r.set64(16, res.conns.as_mut_ptr() as u64).set64(24, res.encs.as_mut_ptr() as u64);
    for i in 0..4 {
        r.set32(32 + 4 * i, 8);
    }
    let x = r.io(fd, MODE_GETRESOURCES);
    if x < 0 {
        return Err(x);
    }
    for i in 0..4 {
        res.n[i] = r.u32(32 + 4 * i);
    }
    Ok(res)
}

/// The connector's first mode (68 bytes) and (connection, mm_w, mm_h).
pub fn connector_mode(fd: i32, conn: u32) -> Result<([u8; 68], u32, u32, u32), isize> {
    let mut mode = [0u8; 68 * 4];
    let mut c = S::<80>::new();
    c.set64(8, mode.as_mut_ptr() as u64).set32(32, 4).set32(48, conn);
    let r = c.io(fd, MODE_GETCONNECTOR);
    if r < 0 {
        return Err(r);
    }
    let mut m = [0u8; 68];
    m.copy_from_slice(&mode[..68]);
    Ok((m, c.u32(60), c.u32(64), c.u32(68)))
}

pub fn mode_size(m: &[u8; 68]) -> (u32, u32) {
    (u16::from_le_bytes([m[4], m[5]]) as u32, u16::from_le_bytes([m[14], m[15]]) as u32)
}

/// The id of property `name` on object `obj` and its current value.
pub fn find_prop(fd: i32, obj: u32, ty: u32, name: &[u8]) -> Option<(u32, u64)> {
    let (ids, vals, n) = props_of(fd, obj, ty).ok()?;
    for i in 0..n {
        let p = prop_name(fd, ids[i])?;
        let len = p.iter().position(|&c| c == 0).unwrap_or(32);
        if &p[..len] == name {
            return Some((ids[i], vals[i]));
        }
    }
    None
}

/// OBJ_GETPROPERTIES (struct drm_mode_obj_get_properties, 32 bytes with
/// its tail padding).
pub fn props_of(fd: i32, obj: u32, ty: u32) -> Result<([u32; 32], [u64; 32], usize), isize> {
    let (mut ids, mut vals) = ([0u32; 32], [0u64; 32]);
    let mut o = S::<32>::new();
    o.set64(0, ids.as_mut_ptr() as u64).set64(8, vals.as_mut_ptr() as u64).set32(16, 32).set32(20, obj).set32(24, ty);
    let r = o.io(fd, MODE_OBJ_GETPROPERTIES);
    if r < 0 {
        return Err(r);
    }
    Ok((ids, vals, o.u32(16) as usize))
}

/// GETPROPERTY name of property `id`.
pub fn prop_name(fd: i32, id: u32) -> Option<[u8; 32]> {
    let mut p = S::<64>::new();
    p.set32(16, id);
    if p.io(fd, MODE_GETPROPERTY) < 0 {
        return None;
    }
    let mut n = [0u8; 32];
    n.copy_from_slice(&p.0[24..56]);
    Some(n)
}

/// One atomic commit of (object, property, value) triples, sorted by
/// object as libdrm does.
pub fn atomic(fd: i32, flags: u32, props: &[(u32, u32, u64)], user_data: u64) -> isize {
    let mut objs = [0u32; 8];
    let mut counts = [0u32; 8];
    let mut ids = [0u32; 32];
    let mut vals = [0u64; 32];
    let mut nobj = 0;
    let mut k = 0;
    for &(o, p, v) in props {
        if nobj == 0 || objs[nobj - 1] != o {
            objs[nobj] = o;
            nobj += 1;
        }
        counts[nobj - 1] += 1;
        ids[k] = p;
        vals[k] = v;
        k += 1;
    }
    let mut a = S::<56>::new();
    a.set32(0, flags).set32(4, nobj as u32);
    a.set64(8, objs.as_ptr() as u64).set64(16, counts.as_ptr() as u64);
    a.set64(24, ids.as_ptr() as u64).set64(32, vals.as_ptr() as u64).set64(48, user_data);
    a.io(fd, MODE_ATOMIC)
}

pub fn create_blob(fd: i32, data: &[u8]) -> Result<u32, isize> {
    let mut b = S::<16>::new();
    b.set64(0, data.as_ptr() as u64).set32(8, data.len() as u32);
    let r = b.io(fd, MODE_CREATEPROPBLOB);
    if r < 0 { Err(r) } else { Ok(b.u32(12)) }
}

/// SYNC_IOC_FILE_INFO: (status, num_fences, first fence's status,
/// timestamp, driver name).
pub fn sync_info(fd: i32) -> Result<(i32, u32, i32, u64, [u8; 32]), isize> {
    let mut info = [0u8; 80 * 4];
    let mut s = S::<56>::new();
    s.set32(40, 4).set64(48, info.as_mut_ptr() as u64);
    let r = s.io(fd, SYNC_IOC_FILE_INFO);
    if r < 0 {
        return Err(r);
    }
    let mut drv = [0u8; 32];
    drv.copy_from_slice(&info[32..64]);
    let fst = i32::from_le_bytes(info[64..68].try_into().unwrap());
    let ts = u64::from_le_bytes(info[72..80].try_into().unwrap());
    Ok((s.u32(32) as i32, s.u32(40), fst, ts, drv))
}

/// poll one fd for POLLIN with `ms` timeout; returns revents (0 on
/// timeout, < 0 error).
pub fn poll_in(fd: i32, ms: i32) -> i32 {
    let mut p = [Pollfd { fd, events: 1, revents: 0 }];
    let r = poll(&mut p, ms);
    if r < 0 { r as i32 } else { p[0].revents as i32 }
}
