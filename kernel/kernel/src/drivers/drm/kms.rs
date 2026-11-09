//! KMS objects of the virtio-gpu DRM device: one primary plane, one CRTC,
//! one virtual encoder and one virtual connector (scanout 0), their
//! properties, framebuffers, property blobs and the atomic state.
//!
//! A commit builds a new [`State`] from property writes, checks it and,
//! unless TEST_ONLY, shows the plane's framebuffer on the scanout: the
//! framebuffer becomes a virtio-gpu resource on first use (backed by the
//! buffer's pages), SET_SCANOUT points the scanout at it when it changed,
//! and TRANSFER_TO_HOST_2D + RESOURCE_FLUSH copy it to the host. Commits
//! complete synchronously, so out-fences are already signalled and flip
//! events are queued at once.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::dma_buf::Buffer;
use crate::drivers::virtio::gpu::{self, Rect};
use crate::errno::*;

pub const PLANE_ID: u32 = 31;
pub const CRTC_ID: u32 = 32;
pub const ENCODER_ID: u32 = 33;
pub const CONNECTOR_ID: u32 = 34;
const FIRST_DYNAMIC_ID: u32 = 40;

pub const OBJ_CRTC: u32 = 0xcccc_cccc;
pub const OBJ_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const OBJ_ENCODER: u32 = 0xe0e0_e0e0;
pub const OBJ_FB: u32 = 0xfbfb_fbfb;
pub const OBJ_PLANE: u32 = 0xeeee_eeee;

pub const PROP_RANGE: u32 = 2;
pub const PROP_IMMUTABLE: u32 = 4;
pub const PROP_ENUM: u32 = 8;
pub const PROP_BLOB: u32 = 16;
pub const PROP_OBJECT: u32 = 1 << 6;
pub const PROP_SIGNED_RANGE: u32 = 2 << 6;
pub const PROP_ATOMIC: u32 = 0x8000_0000;

pub const MODE_SIZE: usize = 68;

pub const FMT_XRGB8888: u32 = fourcc(b"XR24");
pub const FMT_ARGB8888: u32 = fourcc(b"AR24");
pub const FMT_XBGR8888: u32 = fourcc(b"XB24");
pub const FMT_ABGR8888: u32 = fourcc(b"AB24");
pub const FMT_BGRX8888: u32 = fourcc(b"BX24");
pub const FMT_BGRA8888: u32 = fourcc(b"BA24");
pub const FMT_RGBX8888: u32 = fourcc(b"RX24");
pub const FMT_RGBA8888: u32 = fourcc(b"RA24");

const fn fourcc(c: &[u8; 4]) -> u32 {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | (c[3] as u32) << 24
}

/// Scanout formats and their virtio-gpu formats (all 32 bpp).
pub const FORMATS: [(u32, u32); 8] = [
    (FMT_XRGB8888, 2),   // B8G8R8X8_UNORM
    (FMT_ARGB8888, 1),   // B8G8R8A8_UNORM
    (FMT_XBGR8888, 134), // R8G8B8X8_UNORM
    (FMT_ABGR8888, 67),  // R8G8B8A8_UNORM
    (FMT_BGRX8888, 4),   // X8R8G8B8_UNORM
    (FMT_BGRA8888, 3),   // A8R8G8B8_UNORM
    (FMT_RGBX8888, 68),  // X8B8G8R8_UNORM
    (FMT_RGBA8888, 121), // A8B8G8R8_UNORM
];

pub fn virtio_format(fmt: u32) -> Option<u32> {
    FORMATS.iter().find(|f| f.0 == fmt).map(|f| f.1)
}

// ---------------------------------------------------------------------------
// Properties

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Range(u64, u64),
    SignedRange(i64, i64),
    Enum(&'static [(u64, &'static str)]),
    Blob,
    Object(u32),
}

pub struct PropDef {
    pub id: u32,
    pub name: &'static str,
    pub kind: Kind,
    pub immutable: bool,
    /// Only visible to clients with DRM_CLIENT_CAP_ATOMIC.
    pub atomic: bool,
}

pub const P_TYPE: u32 = 1;
pub const P_FB_ID: u32 = 2;
pub const P_IN_FENCE_FD: u32 = 3;
pub const P_CRTC_ID: u32 = 4;
pub const P_CRTC_X: u32 = 5;
pub const P_CRTC_Y: u32 = 6;
pub const P_CRTC_W: u32 = 7;
pub const P_CRTC_H: u32 = 8;
pub const P_SRC_X: u32 = 9;
pub const P_SRC_Y: u32 = 10;
pub const P_SRC_W: u32 = 11;
pub const P_SRC_H: u32 = 12;
pub const P_ACTIVE: u32 = 13;
pub const P_MODE_ID: u32 = 14;
pub const P_OUT_FENCE_PTR: u32 = 15;
pub const P_VRR_ENABLED: u32 = 16;
pub const P_EDID: u32 = 17;
pub const P_DPMS: u32 = 18;
pub const P_LINK_STATUS: u32 = 19;
pub const P_NON_DESKTOP: u32 = 20;

const PLANE_TYPES: &[(u64, &str)] = &[(0, "Overlay"), (1, "Primary"), (2, "Cursor")];
const DPMS: &[(u64, &str)] = &[(0, "On"), (1, "Standby"), (2, "Suspend"), (3, "Off")];
const LINK_STATUS: &[(u64, &str)] = &[(0, "Good"), (1, "Bad")];
const U32_MAX: u64 = u32::MAX as u64;

const fn p(id: u32, name: &'static str, kind: Kind, immutable: bool, atomic: bool) -> PropDef {
    PropDef { id, name, kind, immutable, atomic }
}

pub static PROPS: [PropDef; 20] = [
    p(P_TYPE, "type", Kind::Enum(PLANE_TYPES), true, false),
    p(P_FB_ID, "FB_ID", Kind::Object(OBJ_FB), false, true),
    p(P_IN_FENCE_FD, "IN_FENCE_FD", Kind::SignedRange(-1, i32::MAX as i64), false, true),
    p(P_CRTC_ID, "CRTC_ID", Kind::Object(OBJ_CRTC), false, true),
    p(P_CRTC_X, "CRTC_X", Kind::SignedRange(i32::MIN as i64, i32::MAX as i64), false, true),
    p(P_CRTC_Y, "CRTC_Y", Kind::SignedRange(i32::MIN as i64, i32::MAX as i64), false, true),
    p(P_CRTC_W, "CRTC_W", Kind::Range(0, i32::MAX as u64), false, true),
    p(P_CRTC_H, "CRTC_H", Kind::Range(0, i32::MAX as u64), false, true),
    p(P_SRC_X, "SRC_X", Kind::Range(0, U32_MAX), false, true),
    p(P_SRC_Y, "SRC_Y", Kind::Range(0, U32_MAX), false, true),
    p(P_SRC_W, "SRC_W", Kind::Range(0, U32_MAX), false, true),
    p(P_SRC_H, "SRC_H", Kind::Range(0, U32_MAX), false, true),
    p(P_ACTIVE, "ACTIVE", Kind::Range(0, 1), false, true),
    p(P_MODE_ID, "MODE_ID", Kind::Blob, false, true),
    p(P_OUT_FENCE_PTR, "OUT_FENCE_PTR", Kind::Range(0, u64::MAX), false, true),
    p(P_VRR_ENABLED, "VRR_ENABLED", Kind::Range(0, 1), false, false),
    p(P_EDID, "EDID", Kind::Blob, true, false),
    p(P_DPMS, "DPMS", Kind::Enum(DPMS), false, false),
    p(P_LINK_STATUS, "link-status", Kind::Enum(LINK_STATUS), false, false),
    p(P_NON_DESKTOP, "non-desktop", Kind::Range(0, 1), true, false),
];

pub fn prop(id: u32) -> Option<&'static PropDef> {
    PROPS.iter().find(|p| p.id == id)
}

impl PropDef {
    pub fn flags(&self) -> u32 {
        let mut f = match self.kind {
            Kind::Range(..) => PROP_RANGE,
            Kind::SignedRange(..) => PROP_SIGNED_RANGE,
            Kind::Enum(_) => PROP_ENUM,
            Kind::Blob => PROP_BLOB,
            Kind::Object(_) => PROP_OBJECT,
        };
        if self.immutable {
            f |= PROP_IMMUTABLE;
        }
        if self.atomic {
            f |= PROP_ATOMIC;
        }
        f
    }

    /// Whether `v` is a legal value (object/blob ids are checked later).
    fn valid(&self, v: u64) -> bool {
        match self.kind {
            Kind::Range(lo, hi) => v >= lo && v <= hi,
            Kind::SignedRange(lo, hi) => (v as i64) >= lo && (v as i64) <= hi,
            Kind::Enum(e) => e.iter().any(|x| x.0 == v),
            Kind::Blob | Kind::Object(_) => v <= U32_MAX,
        }
    }
}

/// The properties of object `obj`, in Linux's order.
pub fn obj_props(obj: u32) -> Option<(u32, &'static [u32])> {
    match obj {
        PLANE_ID => Some((
            OBJ_PLANE,
            &[P_TYPE, P_FB_ID, P_IN_FENCE_FD, P_CRTC_ID, P_CRTC_X, P_CRTC_Y, P_CRTC_W, P_CRTC_H, P_SRC_X, P_SRC_Y, P_SRC_W, P_SRC_H],
        )),
        CRTC_ID => Some((OBJ_CRTC, &[P_ACTIVE, P_MODE_ID, P_OUT_FENCE_PTR, P_VRR_ENABLED])),
        CONNECTOR_ID => Some((OBJ_CONNECTOR, &[P_EDID, P_DPMS, P_LINK_STATUS, P_NON_DESKTOP, P_CRTC_ID])),
        ENCODER_ID => Some((OBJ_ENCODER, &[])),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Framebuffers and blobs

pub struct Fb {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub pitch: u32,
    pub offset: u32,
    pub buf: Arc<Buffer>,
    /// The GEM handle it was created from (GETFB).
    pub handle: u32,
    /// The DRM file that created it.
    pub owner: u64,
    /// Its virtio-gpu resource, once shown.
    res: Mutex<Option<u32>>,
}

impl Fb {
    pub fn new(id: u32, width: u32, height: u32, format: u32, pitch: u32, offset: u32, buf: Arc<Buffer>, handle: u32, owner: u64) -> Fb {
        Fb { id, width, height, format, pitch, offset, buf, handle, owner, res: Mutex::new(None) }
    }

    /// The resource showing this framebuffer: `pitch / 4` pixels wide (the
    /// host reads the backing with that stride), as tall as the buffer.
    fn resource(&self, g: &gpu::VirtioGpu) -> KResult<u32> {
        let mut r = self.res.lock();
        if let Some(id) = *r {
            return Ok(id);
        }
        let fmt = virtio_format(self.format).ok_or(EINVAL)?;
        let w = self.pitch / 4;
        let h = (self.buf.size / self.pitch as u64).min(u32::MAX as u64) as u32;
        let id = g.create_resource(fmt, w, h, &self.buf.pages)?;
        *r = Some(id);
        Ok(id)
    }
}

impl Drop for Fb {
    fn drop(&mut self) {
        if let (Some(id), Some(g)) = (*self.res.get_mut(), gpu::get()) {
            g.destroy_resource(id);
        }
    }
}

pub struct Blob {
    pub id: u32,
    pub data: Vec<u8>,
    /// Creating DRM file (0: the kernel).
    pub owner: u64,
}

// ---------------------------------------------------------------------------
// State

#[derive(Clone, Default)]
pub struct PlaneState {
    pub fb: Option<Arc<Fb>>,
    pub crtc: u32,
    pub crtc_x: i32,
    pub crtc_y: i32,
    pub crtc_w: u32,
    pub crtc_h: u32,
    /// 16.16 fixed point.
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
}

#[derive(Clone, Default)]
pub struct State {
    pub plane: PlaneState,
    pub active: bool,
    pub mode: Option<Arc<Blob>>,
    pub conn_crtc: u32,
    pub dpms: u64,
    pub vrr: bool,
    pub link_bad: bool,
}

impl State {
    /// What the scanout should show: framebuffer and source rectangle.
    fn scanout(&self) -> Option<(Arc<Fb>, Rect)> {
        let p = &self.plane;
        let fb = p.fb.clone()?;
        if !self.active || self.mode.is_none() || p.crtc != CRTC_ID || self.dpms != 0 {
            return None;
        }
        let r = (p.src_x >> 16, p.src_y >> 16, p.src_w >> 16, p.src_h >> 16);
        Some((fb, r))
    }

    fn needs_modeset(&self, old: &State) -> bool {
        self.active != old.active
            || self.conn_crtc != old.conn_crtc
            || self.mode.as_ref().map(|m| &m.data) != old.mode.as_ref().map(|m| &m.data)
    }
}

pub struct Kms {
    pub state: State,
    pub fbs: BTreeMap<u32, Arc<Fb>>,
    pub blobs: BTreeMap<u32, Arc<Blob>>,
    next_id: u32,
    /// The scanout's current (resource, rectangle).
    shown: Option<(u32, Rect)>,
    /// The connector's only mode.
    pub mode: [u8; MODE_SIZE],
    pub width: u32,
    pub height: u32,
    pub mm: (u32, u32),
    pub edid_blob: u32,
}

/// The connector's mode for a `w` x `h` panel at 60 Hz (CVT-like
/// blanking), as struct drm_mode_modeinfo.
fn make_mode(w: u32, h: u32) -> [u8; MODE_SIZE] {
    let (hss, hse, ht) = (w + 48, w + 80, w + 160);
    let (vss, vse, vt) = (h + 3, h + 8, h + 30);
    let clock = (ht as u64 * vt as u64 * 60).div_ceil(1000) as u32;
    let mut m = [0u8; MODE_SIZE];
    m[0..4].copy_from_slice(&clock.to_le_bytes());
    for (i, v) in [w, hss, hse, ht, 0, h, vss, vse, vt, 0].iter().enumerate() {
        m[4 + 2 * i..6 + 2 * i].copy_from_slice(&(*v as u16).to_le_bytes());
    }
    m[24..28].copy_from_slice(&60u32.to_le_bytes()); // vrefresh
    m[28..32].copy_from_slice(&5u32.to_le_bytes()); // PHSYNC | PVSYNC
    m[32..36].copy_from_slice(&(8u32 | 64).to_le_bytes()); // PREFERRED | DRIVER
    let name = alloc::format!("{w}x{h}");
    m[36..36 + name.len().min(31)].copy_from_slice(&name.as_bytes()[..name.len().min(31)]);
    m
}

pub fn mode_size(m: &[u8]) -> (u32, u32) {
    (u16::from_le_bytes([m[4], m[5]]) as u32, u16::from_le_bytes([m[14], m[15]]) as u32)
}

impl Kms {
    pub fn new(g: &gpu::VirtioGpu) -> Kms {
        let s = g.scanouts[0];
        let mut k = Kms {
            state: State { conn_crtc: 0, ..Default::default() },
            fbs: BTreeMap::new(),
            blobs: BTreeMap::new(),
            next_id: FIRST_DYNAMIC_ID,
            shown: None,
            mode: make_mode(s.width, s.height),
            width: s.width,
            height: s.height,
            // ~160 dpi when the EDID gives no size.
            mm: (s.width * 254 / 1600, s.height * 254 / 1600),
            edid_blob: 0,
        };
        if let Some(e) = &g.edid {
            if e[21] != 0 && e[22] != 0 {
                k.mm = (e[21] as u32 * 10, e[22] as u32 * 10);
            }
            k.edid_blob = k.add_blob(e.clone(), 0).id;
        }
        k
    }

    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn add_blob(&mut self, data: Vec<u8>, owner: u64) -> Arc<Blob> {
        let b = Arc::new(Blob { id: self.alloc_id(), data, owner });
        self.blobs.insert(b.id, b.clone());
        b
    }

    /// A blob by id: user blobs from the table, or the current mode's.
    pub fn blob(&self, id: u32) -> Option<Arc<Blob>> {
        self.blobs.get(&id).cloned().or_else(|| self.state.mode.clone().filter(|m| m.id == id))
    }

    /// Current value of property `prop` on object `obj`.
    pub fn get_prop(&self, obj: u32, prop: u32) -> u64 {
        let s = &self.state;
        let p = &s.plane;
        match (obj, prop) {
            (PLANE_ID, P_TYPE) => 1,
            (PLANE_ID, P_FB_ID) => p.fb.as_ref().map_or(0, |f| f.id as u64),
            (PLANE_ID, P_IN_FENCE_FD) => -1i64 as u64,
            (PLANE_ID, P_CRTC_ID) => p.crtc as u64,
            (PLANE_ID, P_CRTC_X) => p.crtc_x as i64 as u64,
            (PLANE_ID, P_CRTC_Y) => p.crtc_y as i64 as u64,
            (PLANE_ID, P_CRTC_W) => p.crtc_w as u64,
            (PLANE_ID, P_CRTC_H) => p.crtc_h as u64,
            (PLANE_ID, P_SRC_X) => p.src_x as u64,
            (PLANE_ID, P_SRC_Y) => p.src_y as u64,
            (PLANE_ID, P_SRC_W) => p.src_w as u64,
            (PLANE_ID, P_SRC_H) => p.src_h as u64,
            (CRTC_ID, P_ACTIVE) => s.active as u64,
            (CRTC_ID, P_MODE_ID) => s.mode.as_ref().map_or(0, |m| m.id as u64),
            (CRTC_ID, P_VRR_ENABLED) => s.vrr as u64,
            (CONNECTOR_ID, P_EDID) => self.edid_blob as u64,
            (CONNECTOR_ID, P_DPMS) => s.dpms,
            (CONNECTOR_ID, P_LINK_STATUS) => s.link_bad as u64,
            (CONNECTOR_ID, P_CRTC_ID) => s.conn_crtc as u64,
            _ => 0,
        }
    }

    /// Apply one property write to `st` (a commit in progress).
    /// OUT_FENCE_PTR and IN_FENCE_FD go to `extra`.
    pub fn set_prop(&self, st: &mut State, extra: &mut Extra, obj: u32, prop: u32, v: u64) -> KResult<()> {
        let (_, list) = obj_props(obj).ok_or(ENOENT)?;
        if !list.contains(&prop) {
            return Err(EINVAL);
        }
        let def = self::prop(prop).ok_or(ENOENT)?;
        if def.immutable || !def.valid(v) {
            return Err(EINVAL);
        }
        let p = &mut st.plane;
        match (obj, prop) {
            (PLANE_ID, P_FB_ID) => p.fb = if v == 0 { None } else { Some(self.fbs.get(&(v as u32)).cloned().ok_or(ENOENT)?) },
            (PLANE_ID, P_IN_FENCE_FD) => extra.in_fence = v as i64 as i32,
            (_, P_CRTC_ID) => {
                if v != 0 && v != CRTC_ID as u64 {
                    return Err(ENOENT);
                }
                if obj == PLANE_ID {
                    p.crtc = v as u32;
                } else {
                    st.conn_crtc = v as u32;
                }
            }
            (PLANE_ID, P_CRTC_X) => p.crtc_x = v as i32,
            (PLANE_ID, P_CRTC_Y) => p.crtc_y = v as i32,
            (PLANE_ID, P_CRTC_W) => p.crtc_w = v as u32,
            (PLANE_ID, P_CRTC_H) => p.crtc_h = v as u32,
            (PLANE_ID, P_SRC_X) => p.src_x = v as u32,
            (PLANE_ID, P_SRC_Y) => p.src_y = v as u32,
            (PLANE_ID, P_SRC_W) => p.src_w = v as u32,
            (PLANE_ID, P_SRC_H) => p.src_h = v as u32,
            (CRTC_ID, P_ACTIVE) => st.active = v != 0,
            (CRTC_ID, P_MODE_ID) => {
                st.mode = if v == 0 {
                    None
                } else {
                    let b = self.blob(v as u32).ok_or(EINVAL)?;
                    if b.data.len() != MODE_SIZE {
                        return Err(EINVAL);
                    }
                    Some(b)
                }
            }
            (CRTC_ID, P_OUT_FENCE_PTR) => extra.out_fence_ptr = v,
            (CRTC_ID, P_VRR_ENABLED) => st.vrr = v != 0,
            (CONNECTOR_ID, P_DPMS) => st.dpms = v,
            (CONNECTOR_ID, P_LINK_STATUS) => st.link_bad = v != 0,
            _ => return Err(EINVAL),
        }
        // Plane changes pull the CRTC into the commit, as on Linux.
        if obj == CRTC_ID || obj == PLANE_ID && st.plane.crtc != 0 {
            extra.crtc_touched = true;
        }
        Ok(())
    }

    /// Check a new state (Linux's atomic_check for this hardware).
    pub fn check(&self, st: &State, allow_modeset: bool) -> KResult<()> {
        let p = &st.plane;
        if p.fb.is_some() != (p.crtc != 0) {
            return Err(EINVAL);
        }
        if st.active && st.mode.is_none() {
            return Err(EINVAL);
        }
        if let Some(fb) = &p.fb {
            if virtio_format(fb.format).is_none() || p.crtc_w == 0 || p.crtc_h == 0 {
                return Err(EINVAL);
            }
            let (sx, sy, sw, sh) = (p.src_x as u64, p.src_y as u64, p.src_w as u64, p.src_h as u64);
            if sw == 0 || sh == 0 || sx + sw > (fb.width as u64) << 16 || sy + sh > (fb.height as u64) << 16 {
                return Err(ENOSPC);
            }
            if st.active && st.mode.is_some() && p.crtc == CRTC_ID {
                // The primary plane covers the whole CRTC, unscaled.
                let (w, h) = mode_size(&st.mode.as_ref().unwrap().data);
                if p.crtc_x != 0 || p.crtc_y != 0 || p.crtc_w != w || p.crtc_h != h || (sw >> 16) != w as u64 || (sh >> 16) != h as u64 {
                    return Err(EINVAL);
                }
            }
        }
        if st.needs_modeset(&self.state) && !allow_modeset {
            return Err(EINVAL);
        }
        Ok(())
    }

    /// Make `st` the current state and update the scanout.
    pub fn apply(&mut self, st: State) -> KResult<()> {
        let g = gpu::get().ok_or(ENODEV)?;
        match st.scanout() {
            Some((fb, r)) => {
                let res = fb.resource(g)?;
                if self.shown != Some((res, r)) {
                    g.set_scanout(0, res, r)?;
                    self.shown = Some((res, r));
                }
                let off = fb.offset as u64 + r.1 as u64 * fb.pitch as u64 + r.0 as u64 * 4;
                g.transfer_and_flush(res, r, off)?;
            }
            None => {
                if self.shown.take().is_some() {
                    g.set_scanout(0, 0, (0, 0, 0, 0))?;
                }
            }
        }
        self.state = st;
        Ok(())
    }

    /// DIRTYFB: copy a framebuffer to the host again if it is shown.
    pub fn dirty(&mut self, fb_id: u32) -> KResult<()> {
        let shown = self.state.scanout();
        if shown.is_some_and(|(fb, _)| fb.id == fb_id) {
            let st = self.state.clone();
            return self.apply(st);
        }
        Ok(())
    }
}

/// Per-commit values that are not state.
#[derive(Default)]
pub struct Extra {
    pub out_fence_ptr: u64,
    pub in_fence: i32,
    pub crtc_touched: bool,
}
