//! D1: KMS on the virtio-gpu DRM device, as drm_hwcomposer and the ranchu
//! composer drive it. Resources (one CRTC, encoder, connected virtual
//! connector with a mode), plane resources with and without
//! UNIVERSAL_PLANES, properties with and without ATOMIC (names, the
//! "type" enum, blob/object kinds), ADDFB/ADDFB2/GETFB/RMFB, legacy
//! SETCRTC + GETCRTC, PAGE_FLIP with a flip event read from the fd,
//! DIRTYFB, WAIT_VBLANK, atomic commits (TEST_ONLY, ALLOW_MODESET rule,
//! plane/fb checks, OUT_FENCE_PTR giving a signalled sync file, flip
//! events, IN_FENCE_FD waiting for a sw_sync fence), property blobs, and
//! a closed file's framebuffers turning the display off.
//!
//! Needs a virtio-gpu: skipped elsewhere. The IN_FENCE_FD case needs
//! sw_sync (/sys/kernel/debug/sync/sw_sync).
#![no_std]
#![no_main]

use rt::drm::*;
use rt::*;

rt::test_main!("d1_kms", main);

const EINVAL_: isize = -(EINVAL as isize);

struct Ids {
    crtc: u32,
    conn: u32,
    enc: u32,
    mode: [u8; 68],
    w: u32,
    h: u32,
}

fn test_resources(card: i32) -> Option<Ids> {
    let r = get_resources(card);
    check!(r.is_ok(), "getresources");
    let r = r.ok()?;
    check!(r.n[1] == 1 && r.n[2] == 1 && r.n[3] == 1 && r.n[0] == 0, "resource_counts", "{:?}", r.n);
    let (crtc, conn, enc) = (r.crtcs[0], r.conns[0], r.encs[0]);
    let m = connector_mode(card, conn);
    check!(m.is_ok(), "getconnector");
    let (mode, connection, mmw, mmh) = m.ok()?;
    let (w, h) = mode_size(&mode);
    check!(connection == 1 && w > 0 && h > 0 && mmw > 0 && mmh > 0, "connector_connected_mode", "{connection} {w}x{h} {mmw}x{mmh}mm");
    let vrefresh = u32::from_le_bytes(mode[24..28].try_into().unwrap());
    check!((50..=70).contains(&vrefresh) && mode[32] & 8 != 0, "mode_60hz_preferred", "{vrefresh}");
    let mut e = S::<20>::new();
    e.set32(0, enc);
    check!(e.io(card, MODE_GETENCODER) == 0 && e.u32(12) == 1, "getencoder_possible_crtcs");
    e.set32(0, 9999);
    check_err!(e.io(card, MODE_GETENCODER), ENOENT, "getencoder_bad_id");
    check!(find_prop(card, conn, OBJ_CONNECTOR, b"EDID").is_some(), "connector_edid_prop");
    check!(find_prop(card, conn, OBJ_CONNECTOR, b"DPMS").is_some(), "connector_dpms_prop");
    check!(find_prop(card, conn, OBJ_CONNECTOR, b"CRTC_ID").is_none(), "atomic_props_hidden");
    Some(Ids { crtc, conn, enc, mode, w, h })
}

fn plane_ids(card: i32) -> (u32, u32) {
    let mut ids = [0u32; 8];
    let mut p = S::<16>::new();
    p.set64(0, ids.as_mut_ptr() as u64).set32(8, 8);
    let r = p.io(card, MODE_GETPLANERESOURCES);
    if r < 0 { (0, 0) } else { (p.u32(8), ids[0]) }
}

fn test_planes(card: i32, ids: &Ids) -> u32 {
    check_eq!(plane_ids(card).0, 0, "no_planes_without_universal");
    set_client_cap(card, CLIENT_CAP_UNIVERSAL_PLANES, 1);
    let (n, plane) = plane_ids(card);
    check_eq!(n, 1, "one_universal_plane");
    let mut fmts = [0u32; 16];
    let mut g = S::<32>::new();
    g.set32(0, plane).set32(20, 16).set64(24, fmts.as_mut_ptr() as u64);
    check!(g.io(card, MODE_GETPLANE) == 0 && g.u32(12) == 1, "getplane");
    let nf = g.u32(20) as usize;
    check!(fmts[..nf.min(16)].contains(&FMT_XRGB8888) && fmts[..nf.min(16)].contains(&FMT_ABGR8888), "plane_formats", "{nf}");
    let ty = find_prop(card, plane, OBJ_PLANE, b"type");
    check!(ty.is_some_and(|t| t.1 == 1), "plane_type_primary");
    if let Some((id, _)) = ty {
        let mut vals = [0u64; 4];
        let mut enums = [0u8; 40 * 4];
        let mut p = S::<64>::new();
        p.set64(0, vals.as_mut_ptr() as u64).set64(8, enums.as_mut_ptr() as u64).set32(16, id).set32(56, 4).set32(60, 4);
        check!(p.io(card, MODE_GETPROPERTY) == 0, "getproperty_type");
        check!(p.u32(20) & 8 != 0 && p.u32(20) & 4 != 0 && p.u32(60) == 3, "type_is_immutable_enum", "{:#x} {}", p.u32(20), p.u32(60));
        check!(&enums[48..55] == b"Primary" && vals[1] == 1, "type_enum_names");
    }
    check!(find_prop(card, plane, OBJ_PLANE, b"FB_ID").is_none(), "plane_atomic_props_hidden");
    check_eq!(set_client_cap(card, CLIENT_CAP_ATOMIC, 1), 0, "client_cap_atomic");
    for name in [&b"FB_ID"[..], b"CRTC_ID", b"CRTC_X", b"SRC_W", b"IN_FENCE_FD"] {
        check!(find_prop(card, plane, OBJ_PLANE, name).is_some(), "plane_atomic_props", "{}", Bytes(name));
    }
    check!(find_prop(card, ids.crtc, OBJ_CRTC, b"MODE_ID").is_some_and(|p| p.1 == 0), "crtc_mode_id_prop");
    check!(find_prop(card, ids.crtc, OBJ_CRTC, b"OUT_FENCE_PTR").is_some(), "crtc_out_fence_prop");
    check!(find_prop(card, ids.conn, OBJ_CONNECTOR, b"CRTC_ID").is_some(), "connector_crtc_prop");
    check!(props_of(card, plane, OBJ_CRTC).is_err_and(|e| e == -(ENOENT as isize)), "obj_type_mismatch");
    plane
}

fn get_crtc(card: i32, crtc: u32) -> (u32, u32) {
    let mut c = S::<104>::new();
    c.set32(12, crtc);
    c.io(card, MODE_GETCRTC);
    (c.u32(16), c.u32(32))
}

/// A dumb buffer the size of the mode, filled with `px`, as a framebuffer.
fn make_fb(card: i32, ids: &Ids, fmt: u32, px: u32) -> Option<(u32, u32)> {
    let (h, pitch, size) = create_dumb(card, ids.w, ids.h, 32).ok()?;
    let p = mmap_dumb(card, h, size).ok()? as *mut u32;
    for i in 0..(size / 4) as usize {
        unsafe { *p.add(i) = px };
    }
    munmap(p as *mut u8, size as usize);
    let fb = add_fb2(card, ids.w, ids.h, fmt, h, pitch, 0).ok()?;
    Some((fb, h))
}

fn test_legacy(card: i32, ids: &Ids) {
    let Some((fb, h)) = make_fb(card, ids, FMT_XRGB8888, 0x0000_80ff) else {
        check!(false, "make_fb");
        return;
    };
    let mut g = S::<28>::new();
    g.set32(0, fb);
    check!(g.io(card, MODE_GETFB) == 0 && g.u32(4) == ids.w && g.u32(16) == 32 && g.u32(24) == h, "getfb");
    check_err!(add_fb2(card, ids.w, ids.h, 0x3231_5659, h, ids.w * 4, 0).err().unwrap_or(0), EINVAL, "addfb2_bad_format");
    check_err!(add_fb2(card, ids.w, ids.h + 1, FMT_XRGB8888, h, ids.w * 4, 0).err().unwrap_or(0), EINVAL, "addfb2_too_big");
    check_err!(add_fb2(card, ids.w, ids.h, FMT_XRGB8888, 999, ids.w * 4, 0).err().unwrap_or(0), ENOENT, "addfb2_bad_handle");
    // Legacy ADDFB (bpp 32, depth 24 = XRGB8888).
    let mut a = S::<28>::new();
    a.set32(4, ids.w).set32(8, ids.h).set32(12, ids.w * 4).set32(16, 32).set32(20, 24).set32(24, h);
    check!(a.io(card, MODE_ADDFB) == 0 && a.u32(0) != 0, "addfb_legacy");
    let fb_b = a.u32(0);
    if let Ok(r) = get_resources(card) {
        check!(r.n[0] == 2 && r.fbs[..2].contains(&fb), "resources_list_fbs");
    }

    let conns = [ids.conn];
    let mut c = S::<104>::new();
    c.set64(0, conns.as_ptr() as u64).set32(8, 1).set32(12, ids.crtc).set32(16, fb).set32(32, 1);
    c.0[36..104].copy_from_slice(&ids.mode);
    check_eq!(c.io(card, MODE_SETCRTC), 0, "setcrtc");
    check_eq!(get_crtc(card, ids.crtc), (fb, 1), "getcrtc_after_setcrtc");
    check!(find_prop(card, ids.crtc, OBJ_CRTC, b"ACTIVE").is_some_and(|p| p.1 == 1), "crtc_active_prop");
    let mut d = S::<24>::new();
    d.set32(0, fb);
    check_eq!(d.io(card, MODE_DIRTYFB), 0, "dirtyfb");

    // Page flip to the second framebuffer with an event.
    let mut f = S::<24>::new();
    f.set32(0, ids.crtc).set32(4, fb_b).set32(8, PAGE_FLIP_EVENT).set64(16, 0x1234_5678);
    check_eq!(f.io(card, MODE_PAGE_FLIP), 0, "page_flip");
    check!(poll_in(card, 1000) & 1 != 0, "flip_event_pollin");
    let mut ev = [0u8; 64];
    let n = read(card, &mut ev);
    let ty = u32::from_le_bytes(ev[0..4].try_into().unwrap());
    let ud = u64::from_le_bytes(ev[8..16].try_into().unwrap());
    let crtc = u32::from_le_bytes(ev[28..32].try_into().unwrap());
    check!(n == 32 && ty == 2 && ud == 0x1234_5678 && crtc == ids.crtc, "flip_event", "n={n} ty={ty} ud={ud:#x}");
    check_eq!(poll_in(card, 0), 0, "no_more_events");
    check_eq!(get_crtc(card, ids.crtc).0, fb_b, "flip_shows_new_fb");
    f.set32(8, 2);
    check_err!(f.io(card, MODE_PAGE_FLIP), EINVAL, "page_flip_async_unsupported");

    // WAIT_VBLANK: one frame later.
    let mut v = S::<24>::new();
    v.set32(0, 1).set32(4, 1);
    let t0 = now_ns();
    check_eq!(v.io(card, WAIT_VBLANK), 0, "wait_vblank");
    let seq1 = v.u32(4);
    v.set32(0, 1).set32(4, 2);
    v.io(card, WAIT_VBLANK);
    check!(v.u32(4).wrapping_sub(seq1) >= 1 && now_ns() - t0 < 1_000_000_000, "wait_vblank_advances");

    // Removing the shown framebuffer turns the CRTC off.
    check_eq!(rm_fb(card, fb_b), 0, "rmfb_shown");
    check_eq!(get_crtc(card, ids.crtc), (0, 0), "rmfb_disables_crtc");
    check_err!(rm_fb(card, fb_b), ENOENT, "rmfb_twice");
    rm_fb(card, fb);
    gem_close(card, h);
}

fn test_atomic(card: i32, ids: &Ids, plane: u32) {
    let pid = |obj, ty, n: &[u8]| find_prop(card, obj, ty, n).map_or(0, |p| p.0);
    let (p_fb, p_crtc) = (pid(plane, OBJ_PLANE, b"FB_ID"), pid(plane, OBJ_PLANE, b"CRTC_ID"));
    let (p_cx, p_cy, p_cw, p_ch) = (
        pid(plane, OBJ_PLANE, b"CRTC_X"),
        pid(plane, OBJ_PLANE, b"CRTC_Y"),
        pid(plane, OBJ_PLANE, b"CRTC_W"),
        pid(plane, OBJ_PLANE, b"CRTC_H"),
    );
    let (p_sx, p_sy, p_sw, p_sh) = (
        pid(plane, OBJ_PLANE, b"SRC_X"),
        pid(plane, OBJ_PLANE, b"SRC_Y"),
        pid(plane, OBJ_PLANE, b"SRC_W"),
        pid(plane, OBJ_PLANE, b"SRC_H"),
    );
    let p_in = pid(plane, OBJ_PLANE, b"IN_FENCE_FD");
    let (p_active, p_mode, p_out) =
        (pid(ids.crtc, OBJ_CRTC, b"ACTIVE"), pid(ids.crtc, OBJ_CRTC, b"MODE_ID"), pid(ids.crtc, OBJ_CRTC, b"OUT_FENCE_PTR"));
    let p_conn_crtc = pid(ids.conn, OBJ_CONNECTOR, b"CRTC_ID");
    let Some((fb, h)) = make_fb(card, ids, FMT_ABGR8888, 0xff20_c020) else {
        check!(false, "atomic_make_fb");
        return;
    };
    let blob = create_blob(card, &ids.mode);
    check!(blob.is_ok(), "create_mode_blob");
    let blob = blob.unwrap_or(0);
    let mut back = [0u8; 68];
    let mut gb = S::<16>::new();
    gb.set32(0, blob).set32(4, 68).set64(8, back.as_mut_ptr() as u64);
    check!(gb.io(card, MODE_GETPROPBLOB) == 0 && back == ids.mode, "getpropblob");

    let (w, h_) = (ids.w as u64, ids.h as u64);
    let plane_props = |fbid: u64, crtc: u64| {
        [
            (plane, p_fb, fbid),
            (plane, p_crtc, crtc),
            (plane, p_cx, 0),
            (plane, p_cy, 0),
            (plane, p_cw, w),
            (plane, p_ch, h_),
            (plane, p_sx, 0),
            (plane, p_sy, 0),
            (plane, p_sw, w << 16),
            (plane, p_sh, h_ << 16),
        ]
    };
    let crtc = ids.crtc as u64;
    let mut full: [(u32, u32, u64); 13] = [(0, 0, 0); 13];
    full[0] = (plane, p_fb, fb as u64);
    full[..10].copy_from_slice(&plane_props(fb as u64, crtc));
    full[10] = (ids.crtc, p_active, 1);
    full[11] = (ids.crtc, p_mode, blob as u64);
    full[12] = (ids.conn, p_conn_crtc, crtc);
    // Sorted by object for the helper: plane, crtc, connector.
    check_err!(atomic(card, ATOMIC_TEST_ONLY, &full, 0), EINVAL, "modeset_needs_allow_modeset");
    check_eq!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &full, 0), 0, "test_only_ok");
    check_eq!(get_crtc(card, ids.crtc).1, 0, "test_only_changes_nothing");

    let mut bad = full;
    bad[1] = (plane, p_crtc, 0);
    check_err!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &bad, 0), EINVAL, "fb_without_crtc");
    let mut bad = full;
    bad[8] = (plane, p_sw, (w + 1) << 16);
    check_err!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &bad, 0), ENOSPC, "src_outside_fb");
    let mut bad = full;
    bad[4] = (plane, p_cw, w / 2);
    check_err!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &bad, 0), EINVAL, "primary_must_cover_crtc");
    let mut bad = full;
    bad[0] = (plane, p_fb, 99999);
    check_err!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &bad, 0), ENOENT, "unknown_fb");
    check_err!(atomic(card, ATOMIC_TEST_ONLY, &[(77777, p_fb, 0)], 0), ENOENT, "unknown_object");
    let mut bad = full;
    bad[11] = (ids.crtc, p_mode, fb as u64);
    check_err!(atomic(card, ATOMIC_TEST_ONLY | ATOMIC_ALLOW_MODESET, &bad, 0), EINVAL, "mode_id_not_a_mode_blob");

    // The real modeset, with an out-fence.
    let mut out_fence: i32 = -7;
    let mut with_fence: [(u32, u32, u64); 14] = [(0, 0, 0); 14];
    with_fence[..12].copy_from_slice(&full[..12]);
    with_fence[12] = (ids.crtc, p_out, &mut out_fence as *mut i32 as u64);
    with_fence[13] = full[12];
    check_eq!(atomic(card, ATOMIC_ALLOW_MODESET, &with_fence, 0), 0, "atomic_modeset");
    check_eq!(get_crtc(card, ids.crtc), (fb, 1), "atomic_state_visible_legacy");
    check!(out_fence >= 0, "out_fence_fd", "{out_fence}");
    if out_fence >= 0 {
        check!(poll_in(out_fence, 0) & 1 != 0, "out_fence_signalled");
        let info = sync_info(out_fence);
        check!(info.is_ok_and(|i| i.0 == 1 && i.1 == 1 && i.2 == 1 && i.3 > 0), "out_fence_info");
        let mut l = [0u8; 64];
        let p = cpath!("/proc/self/fd/{}", out_fence);
        let n = readlink(p.as_ptr(), &mut l);
        check!(n > 0 && &l[..n as usize] == b"anon_inode:sync_file", "out_fence_is_sync_file", "{}", Bytes(&l[..n.max(0) as usize]));
        close(out_fence);
    }

    // Page flip through atomic: plane only, flip event (CRTC pulled in).
    let Some((fb2, h2)) = make_fb(card, ids, FMT_XRGB8888, 0x00ff_0000) else { return };
    check_eq!(atomic(card, ATOMIC_NONBLOCK | PAGE_FLIP_EVENT, &[(plane, p_fb, fb2 as u64)], 0xabcd), 0, "atomic_flip_event");
    let mut ev = [0u8; 32];
    let n = read(card, &mut ev);
    check!(n == 32 && ev[0] == 2 && u64::from_le_bytes(ev[8..16].try_into().unwrap()) == 0xabcd, "atomic_flip_event_read");
    check_err!(atomic(card, PAGE_FLIP_EVENT | ATOMIC_TEST_ONLY, &[(plane, p_fb, fb as u64)], 0), EINVAL, "event_with_test_only");
    check_err!(atomic(card, 0x8000, &[(plane, p_fb, fb as u64)], 0), EINVAL, "atomic_bad_flags");

    // IN_FENCE_FD: the commit waits for the fence.
    let sw = open(c"/sys/kernel/debug/sync/sw_sync".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    if sw < 0 {
        skip("in_fence_waits", "no sw_sync");
    } else {
        let mut cf = S::<40>::new();
        cf.set32(0, 1);
        cf.0[4..8].copy_from_slice(b"kms\0");
        check_eq!(cf.io(sw, SW_SYNC_IOC_CREATE_FENCE), 0, "sw_sync_fence");
        let fence = cf.u32(36) as i32;
        let pid = fork();
        if pid == 0 {
            sleep_ms(150);
            let one: u32 = 1;
            ioctl(sw, SW_SYNC_IOC_INC, &one as *const u32 as usize);
            exit(0);
        }
        let t0 = now_ns();
        let r = atomic(card, 0, &[(plane, p_fb, fb as u64), (plane, p_in, fence as u64)], 0);
        let dt = (now_ns() - t0) / 1_000_000;
        check!(r == 0 && dt >= 100, "in_fence_waits", "r={} {}ms", errname(r), dt);
        let mut st = 0;
        wait4(pid as i32, &mut st, 0);
        check_err!(atomic(card, 0, &[(plane, p_in, 12345)], 0), EINVAL, "in_fence_bad_fd");
        close(fence);
        close(sw);
    }

    // Turn everything off; the blob can go.
    let off = [(plane, p_fb, 0), (plane, p_crtc, 0), (ids.crtc, p_active, 0), (ids.crtc, p_mode, 0), (ids.conn, p_conn_crtc, 0)];
    check_eq!(atomic(card, ATOMIC_ALLOW_MODESET, &off, 0), 0, "atomic_disable");
    check_eq!(get_crtc(card, ids.crtc), (0, 0), "disabled");
    check_eq!(ioctl(card, MODE_DESTROYPROPBLOB, &blob as *const u32 as usize), 0, "destroy_blob");
    check_err!(ioctl(card, MODE_DESTROYPROPBLOB, &blob as *const u32 as usize), ENOENT, "destroy_blob_twice");
    rm_fb(card, fb);
    rm_fb(card, fb2);
    gem_close(card, h);
    gem_close(card, h2);
    let _ = EINVAL_;
}

/// A second client's framebuffers go away with its fd, and with them the
/// picture it showed.
fn test_close(ids: &Ids) {
    let Some(c2) = open_virtio(c"/dev/dri/card0") else { return };
    let Some((fb, _)) = make_fb(c2, ids, FMT_XRGB8888, 0x00ff_ffff) else { return };
    let conns = [ids.conn];
    let mut c = S::<104>::new();
    c.set64(0, conns.as_ptr() as u64).set32(8, 1).set32(12, ids.crtc).set32(16, fb).set32(32, 1);
    c.0[36..104].copy_from_slice(&ids.mode);
    check_eq!(c.io(c2, MODE_SETCRTC), 0, "second_client_setcrtc");
    let Some(c1) = open_virtio(c"/dev/dri/card0") else { return };
    let mut g = S::<28>::new();
    g.set32(0, fb);
    check_eq!(g.io(c1, MODE_GETFB), 0, "other_client_getfb");
    close(c2);
    check_err!(g.io(c1, MODE_GETFB), ENOENT, "fb_gone_with_fd");
    check_eq!(get_crtc(c1, ids.crtc), (0, 0), "display_off_with_fd");
    close(c1);
}

fn main() {
    let Some(card) = open_virtio(c"/dev/dri/card0") else {
        skip("all", "no virtio_gpu DRM device");
        return;
    };
    check_err!(atomic(card, ATOMIC_TEST_ONLY, &[], 0), EINVAL, "atomic_needs_client_cap");
    let Some(ids) = test_resources(card) else { return };
    let _ = ids.enc;
    let plane = test_planes(card, &ids);
    test_legacy(card, &ids);
    test_atomic(card, &ids, plane);
    test_close(&ids);
    close(card);
}
