//! D1: the virtio-gpu DRM device and dumb buffers, as minigbm uses them.
//! /dev/dri/card0 and renderD128 (driver "virtio_gpu", sysfs class drm
//! entries with the uevent ueventd needs), GET_CAP / SET_CLIENT_CAP,
//! CREATE_DUMB (pitch, size, bad arguments), MAP_DUMB + mmap of the fake
//! offset (two mappings and a forked child share the pages), bad offsets,
//! per-file handles, GEM_CLOSE / DESTROY_DUMB, render-node restrictions
//! (KMS ioctls EACCES, dumb buffers allowed) and the virtgpu GETPARAM of
//! a 2D device.
//!
//! Needs a virtio-gpu (QEMU -device virtio-gpu-pci): skipped elsewhere.
#![no_std]
#![no_main]

use rt::drm::*;
use rt::*;

rt::test_main!("d1_dumb", main);

const VIRTGPU_GETPARAM: u32 = iowr(0x43, 16);

fn test_device(card: i32) {
    let mut name = [0u8; 32];
    check_eq!(version(card, &mut name), 10, "version_name_len");
    check!(&name[..10] == b"virtio_gpu", "version_name");
    let mut u = [0u8; 64];
    let mut q = S::<16>::new();
    q.set64(0, 64).set64(8, u.as_mut_ptr() as u64);
    check_ok!(q.io(card, GET_UNIQUE), "get_unique");
    check!(q.u64(0) > 0 && &u[..5] == b"0000:", "unique_is_pci_address", "len {}", q.u64(0));
    check_eq!(get_cap(card, CAP_DUMB_BUFFER), Ok(1), "cap_dumb_buffer");
    check_eq!(get_cap(card, CAP_PRIME), Ok(3), "cap_prime");
    check_eq!(get_cap(card, 0x7777), Err(-(EINVAL as isize)), "cap_unknown");
    check_eq!(set_client_cap(card, CLIENT_CAP_UNIVERSAL_PLANES, 1), 0, "client_cap_universal");
    check_eq!(set_client_cap(card, CLIENT_CAP_UNIVERSAL_PLANES, 2), -(EINVAL as isize), "client_cap_bad_value");
    check_eq!(set_client_cap(card, 0x7777, 1), -(EINVAL as isize), "client_cap_unknown");

    let mut b = [0u8; 256];
    let n = read_file(c"/sys/class/drm/card0/dev".as_ptr() as *const u8, &mut b);
    check!(n > 0 && trim(&b[..n.max(0) as usize]) == b"226:0", "sysfs_card0_dev", "{}", Bytes(&b[..n.max(0) as usize]));
    let n = read_file(c"/sys/class/drm/card0/uevent".as_ptr() as *const u8, &mut b);
    let ev = &b[..n.max(0) as usize];
    check!(ev.windows(17).any(|w| w == b"DEVNAME=dri/card0"), "sysfs_card0_uevent", "{}", Bytes(ev));
    let n = read_file(c"/sys/class/drm/renderD128/dev".as_ptr() as *const u8, &mut b);
    check!(n > 0 && trim(&b[..n.max(0) as usize]) == b"226:128", "sysfs_render_dev");
    let mut st = Stat::default();
    check!(
        fstatat(AT_FDCWD, c"/dev/dri/renderD128".as_ptr() as *const u8, &mut st, 0) == 0 && st.rdev == (226 << 8 | 128),
        "render_node_rdev"
    );
}

fn test_dumb(card: i32) {
    let r = create_dumb(card, 64, 48, 32);
    check!(r.is_ok(), "create_dumb", "{:?}", r);
    let Ok((h, pitch, size)) = r else { return };
    check!(h != 0 && pitch >= 256 && size >= pitch as u64 * 48, "create_dumb_layout", "{h} {pitch} {size}");
    check_err!(create_dumb(card, 0, 48, 32).err().unwrap_or(0), EINVAL, "create_dumb_zero_width");
    check_err!(create_dumb(card, 64, 48, 0).err().unwrap_or(0), EINVAL, "create_dumb_zero_bpp");
    let Ok(off) = map_dumb(card, h) else {
        check!(false, "map_dumb");
        return;
    };
    check!(off != 0 && off % 4096 == 0, "map_dumb_offset", "{off:#x}");
    check_eq!(map_dumb(card, h), Ok(off), "map_dumb_stable");
    check_err!(map_dumb(card, 9999).err().unwrap_or(0), ENOENT, "map_dumb_bad_handle");
    let a = mmap(0, size as usize, PROT_READ | PROT_WRITE, MAP_SHARED, card, off as i64);
    check!(a > 0, "mmap_dumb", "{}", errname(a));
    let b = mmap(0, size as usize, PROT_READ | PROT_WRITE, MAP_SHARED, card, off as i64);
    if a <= 0 || b <= 0 {
        return;
    }
    let (a, b) = (a as *mut u32, b as *mut u32);
    let words = size as usize / 4;
    unsafe {
        check!(*a == 0 && *a.add(words - 1) == 0, "dumb_zeroed");
        for i in 0..words {
            *a.add(i) = i as u32 ^ 0x5a5a_0000;
        }
        check!((0..words).all(|i| *b.add(i) == i as u32 ^ 0x5a5a_0000), "two_mappings_share_pages");
    }
    // A forked child writes through its inherited mapping.
    let st = run_child(|| {
        unsafe { *b.add(7) = 0xdead_beef };
        0
    });
    check!(status_code(st) == 0 && unsafe { *a.add(7) } == 0xdead_beef, "fork_shares_mapping");
    munmap(a as *mut u8, size as usize);
    munmap(b as *mut u8, size as usize);
    // Unmapped and mapped again: the data stays with the buffer.
    if let Ok(p) = mmap_dumb(card, h, size) {
        check!(unsafe { *(p as *mut u32).add(5) } == 5 ^ 0x5a5a_0000, "remap_keeps_data");
        munmap(p, size as usize);
    }
    check_err!(
        mmap(0, 4096, PROT_READ, MAP_SHARED, card, 0x7000_0000),
        EINVAL,
        "mmap_unknown_offset"
    );
    // A second dumb buffer gets its own handle and offset.
    if let Ok((h2, _, s2)) = create_dumb(card, 100, 100, 32) {
        check!(h2 != h && map_dumb(card, h2).is_ok_and(|o| o != off && (o >= off + size || o + s2 <= off)), "second_buffer_distinct");
        check_eq!(ioctl(card, MODE_DESTROY_DUMB, &h2 as *const u32 as usize), 0, "destroy_dumb");
        check_err!(map_dumb(card, h2).err().unwrap_or(0), ENOENT, "destroyed_handle_gone");
    }
    // 8/16 bpp (minigbm uses 32 with an adjusted width; others work too).
    if let Ok((h3, p3, _)) = create_dumb(card, 30, 10, 16) {
        check_eq!(p3 >= 60, true, "create_dumb_16bpp_pitch");
        gem_close(card, h3);
    } else {
        check!(false, "create_dumb_16bpp");
    }
    check_eq!(gem_close(card, h), 0, "gem_close");
    check_err!(gem_close(card, h), EINVAL, "gem_close_twice");
}

fn test_render(card: i32) {
    let Some(rnd) = open_virtio(c"/dev/dri/renderD128") else {
        check!(false, "open_render_node");
        return;
    };
    check!(get_resources(rnd).is_err_and(|e| e == -(EACCES as isize)), "render_getresources_eacces");
    check_eq!(get_cap(rnd, CAP_CURSOR_WIDTH), Err(-(EINVAL as isize)), "render_kms_cap_einval");
    check_eq!(get_cap(rnd, CAP_PRIME), Ok(3), "render_cap_prime");
    let r = create_dumb(rnd, 32, 32, 32);
    check!(r.is_ok(), "render_create_dumb");
    if let Ok((h, _, size)) = r {
        // Handles are per open file.
        check_err!(map_dumb(card, h + 1000).err().unwrap_or(0), ENOENT, "handles_per_file");
        let p = mmap_dumb(rnd, h, size);
        check!(p.is_ok(), "render_mmap_dumb");
        if let Ok(p) = p {
            unsafe { *(p as *mut u32) = 77 };
            munmap(p, size as usize);
        }
    }
    // virtgpu GETPARAM: a 2D device (no 3D features, no blobs).
    let mut v: i32 = -1;
    let mut g = S::<16>::new();
    g.set64(0, 1).set64(8, &mut v as *mut i32 as u64);
    check!(g.io(rnd, VIRTGPU_GETPARAM) == 0 && v == 0, "virtgpu_getparam_3d_off", "v={v}");
    g.set64(0, 3);
    check!(g.io(rnd, VIRTGPU_GETPARAM) == 0 && v == 0, "virtgpu_getparam_no_blob");
    g.set64(0, 1000);
    check_err!(g.io(rnd, VIRTGPU_GETPARAM), EINVAL, "virtgpu_getparam_unknown");
    close(rnd);
}

fn main() {
    let Some(card) = open_virtio(c"/dev/dri/card0") else {
        skip("all", "no virtio_gpu DRM device");
        return;
    };
    test_device(card);
    test_dumb(card);
    test_render(card);
    close(card);
}
