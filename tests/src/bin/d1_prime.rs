//! D1: dma-bufs and PRIME, the way gralloc buffers travel between the
//! allocator, SurfaceFlinger and the composer. PRIME_HANDLE_TO_FD (flags,
//! one dma-buf file per buffer), the dma-buf fd (`/dmabuf:` link, size by
//! lseek, mmap sharing pages with the DRM mapping, poll, DMA_BUF_IOCTL_SYNC
//! argument checks, SET_NAME, fdinfo, EXPORT_SYNC_FILE), PRIME_FD_TO_HANDLE
//! (same handle again in one file, a new one in another, a forked child
//! importing an inherited fd into its own render node and writing to it),
//! the buffer outliving its GEM handle, non-dma-buf fds refused, and the
//! system dma-heap (DMA_HEAP_IOCTL_ALLOC, its buffers mapped and imported
//! into DRM as framebuffers).
//!
//! The DRM parts need a virtio-gpu; the heap parts run wherever
//! /dev/dma_heap/system can be opened.
#![no_std]
#![no_main]

use rt::drm::*;
use rt::*;

rt::test_main!("d1_prime", main);

const SEEK_END: i32 = 2;

fn link_of(fd: i32, out: &mut [u8; 128]) -> usize {
    let p = cpath!("/proc/self/fd/{}", fd);
    readlink(p.as_ptr(), out).max(0) as usize
}

fn map(fd: i32, size: u64) -> *mut u32 {
    let p = mmap(0, size as usize, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if p < 0 { core::ptr::null_mut() } else { p as *mut u32 }
}

fn test_prime(card: i32) {
    let Ok((h, _, size)) = create_dumb(card, 256, 64, 32) else {
        check!(false, "create_dumb");
        return;
    };
    let Ok(dp) = mmap_dumb(card, h, size) else {
        check!(false, "mmap_dumb");
        return;
    };
    let dp = dp as *mut u32;
    unsafe { *dp.add(3) = 0x1111_2222 };

    check_err!(prime_to_fd(card, h, 0x40), EINVAL, "handle_to_fd_bad_flags");
    check_err!(prime_to_fd(card, 999, 0), ENOENT, "handle_to_fd_bad_handle");
    let fd = prime_to_fd(card, h, (O_CLOEXEC | O_RDWR) as u32) as i32;
    check!(fd >= 0, "handle_to_fd");
    if fd < 0 {
        return;
    }
    check_eq!(fcntl(fd, 1 /* F_GETFD */, 0), 1, "dmabuf_cloexec");
    let mut l = [0u8; 128];
    let n = link_of(fd, &mut l);
    check!(n >= 8 && &l[..8] == b"/dmabuf:", "dmabuf_link_name", "{}", Bytes(&l[..n]));
    check_eq!(lseek(fd, 0, SEEK_END), size as isize, "dmabuf_size_lseek");
    lseek(fd, 0, 0);
    let fd2 = prime_to_fd(card, h, 0) as i32;
    let (mut s1, mut s2) = (Stat::default(), Stat::default());
    fstat(fd, &mut s1);
    fstat(fd2, &mut s2);
    check!(fd2 >= 0 && fd2 != fd && s1.ino == s2.ino, "export_twice_same_buffer");
    close(fd2);

    let p = map(fd, size);
    check!(!p.is_null(), "dmabuf_mmap");
    if !p.is_null() {
        check_eq!(unsafe { *p.add(3) }, 0x1111_2222, "dmabuf_sees_dumb_data");
        unsafe { *p.add(4) = 0x3333_4444 };
        check_eq!(unsafe { *dp.add(4) }, 0x3333_4444, "dumb_sees_dmabuf_writes");
    }
    check!(poll_in(fd, 0) & 1 != 0, "dmabuf_poll_ready");

    // DMA_BUF_IOCTL_SYNC: START|RW, END|RW fine; no direction or unknown
    // bits are EINVAL.
    let sync = |flags: u64| ioctl(fd, DMA_BUF_IOCTL_SYNC, &flags as *const u64 as usize);
    check_eq!(sync(3), 0, "dmabuf_sync_start");
    check_eq!(sync(4 | 3), 0, "dmabuf_sync_end");
    check_err!(sync(0), EINVAL, "dmabuf_sync_no_direction");
    check_err!(sync(0x10 | 1), EINVAL, "dmabuf_sync_bad_flags");
    check_eq!(ioctl(fd, DMA_BUF_SET_NAME, c"scanout".as_ptr() as usize), 0, "dmabuf_set_name");
    let n = link_of(fd, &mut l);
    check!(&l[..n] == b"/dmabuf:scanout", "dmabuf_named_link", "{}", Bytes(&l[..n]));
    let mut fi = [0u8; 512];
    let path = cpath!("/proc/self/fdinfo/{}", fd);
    let n = read_file(path.as_ptr(), &mut fi).max(0) as usize;
    let fi = &fi[..n];
    check!(fi.windows(8).any(|w| w == b"exp_name") && fi.windows(7).any(|w| w == b"scanout"), "dmabuf_fdinfo", "{}", Bytes(fi));
    let mut es = S::<8>::new();
    es.set32(0, 1);
    check_eq!(es.io(fd, DMA_BUF_EXPORT_SYNC_FILE), 0, "dmabuf_export_sync_file");
    let sf = es.u32(4) as i32;
    check!(sf >= 0 && poll_in(sf, 0) & 1 != 0, "exported_sync_file_signalled");
    if sf >= 0 {
        close(sf);
    }

    // Import: same file -> same handle; another file -> its own handle.
    check_eq!(prime_to_handle(card, fd), h as isize, "fd_to_handle_same_file");
    let Some(rnd) = open_virtio(c"/dev/dri/renderD128") else { return };
    let h2 = prime_to_handle(rnd, fd);
    check!(h2 > 0, "fd_to_handle_render");
    if h2 > 0 {
        check_eq!(prime_to_handle(rnd, fd), h2, "fd_to_handle_dedup");
        if let Ok(rp) = mmap_dumb(rnd, h2 as u32, size) {
            check_eq!(unsafe { *(rp as *mut u32).add(4) }, 0x3333_4444, "import_maps_same_pages");
            munmap(rp, size as usize);
        } else {
            check!(false, "import_map");
        }
    }
    let mut pfd = [0i32; 2];
    pipe2(&mut pfd, O_CLOEXEC);
    check_err!(prime_to_handle(rnd, pfd[0]), EINVAL, "fd_to_handle_not_dmabuf");
    check_err!(prime_to_handle(rnd, 4000), EBADF, "fd_to_handle_bad_fd");
    close(pfd[0]);
    close(pfd[1]);

    // A child opens its own render node, imports the inherited fd and
    // writes through it.
    let st = run_child(|| {
        let Some(r) = open_virtio(c"/dev/dri/renderD128") else { return 2 };
        let ch = prime_to_handle(r, fd);
        if ch <= 0 {
            return 3;
        }
        match mmap_dumb(r, ch as u32, size) {
            Ok(q) => unsafe { *(q as *mut u32).add(100) = 0xc0ffee },
            Err(_) => return 4,
        }
        0
    });
    check!(status_code(st) == 0 && unsafe { *dp.add(100) } == 0xc0ffee, "child_import_writes", "status {}", status_code(st));

    // The buffer lives on in the dma-buf after the GEM handles close.
    gem_close(card, h);
    gem_close(rnd, h2 as u32);
    munmap(dp as *mut u8, size as usize);
    let h3 = prime_to_handle(card, fd);
    check!(h3 > 0, "reimport_after_close");
    if h3 > 0 {
        let fb = add_fb2(card, 256, 64, FMT_XRGB8888, h3 as u32, 1024, 0);
        check!(fb.is_ok(), "reimported_buffer_framebuffer");
        if let Ok(fb) = fb {
            rm_fb(card, fb);
        }
        if let Ok(q) = mmap_dumb(card, h3 as u32, size) {
            check_eq!(unsafe { *(q as *mut u32).add(3) }, 0x1111_2222, "reimport_keeps_data");
            munmap(q, size as usize);
        }
    }
    if !p.is_null() {
        munmap(p as *mut u8, size as usize);
    }
    close(fd);
    close(rnd);
}

fn test_heap(card: Option<i32>) {
    let heap = open(c"/dev/dma_heap/system".as_ptr() as *const u8, O_RDONLY | O_CLOEXEC, 0) as i32;
    if heap < 0 {
        skip("heap", "no /dev/dma_heap/system");
        return;
    }
    let alloc = |len: u64, fd_flags: u32, heap_flags: u64| {
        let mut a = S::<24>::new();
        a.set64(0, len).set32(12, fd_flags).set64(16, heap_flags);
        let r = a.io(heap, DMA_HEAP_IOCTL_ALLOC);
        if r < 0 { r } else { a.u32(8) as isize }
    };
    let size: u64 = 256 * 64 * 4;
    let fd = alloc(size, (O_RDWR | O_CLOEXEC) as u32, 0) as i32;
    check!(fd >= 0, "heap_alloc");
    check_err!(alloc(0, O_RDWR as u32, 0), EINVAL, "heap_alloc_zero");
    check_err!(alloc(4096, O_RDWR as u32, 1), EINVAL, "heap_alloc_bad_heap_flags");
    check_err!(alloc(4096, 0x40, 0), EINVAL, "heap_alloc_bad_fd_flags");
    if fd < 0 {
        return;
    }
    check_eq!(lseek(fd, 0, SEEK_END), size as isize, "heap_buffer_size");
    let p = map(fd, size);
    check!(!p.is_null(), "heap_mmap");
    if p.is_null() {
        return;
    }
    unsafe {
        check_eq!(*p.add(10), 0, "heap_zeroed");
        *p.add(10) = 0xabad_cafe;
    }
    let q = map(fd, size);
    check!(!q.is_null() && unsafe { *q.add(10) } == 0xabad_cafe, "heap_mappings_shared");
    let mut l = [0u8; 128];
    let n = link_of(fd, &mut l);
    check!(n >= 8 && &l[..8] == b"/dmabuf:", "heap_dmabuf_link");
    if let Some(card) = card {
        let h = prime_to_handle(card, fd);
        check!(h > 0, "heap_import_to_drm");
        if h > 0 {
            let fb = add_fb2(card, 256, 64, FMT_ABGR8888, h as u32, 1024, 0);
            check!(fb.is_ok(), "heap_buffer_framebuffer");
            if let Ok(fb) = fb {
                rm_fb(card, fb);
            }
            if let Ok(d) = mmap_dumb(card, h as u32, size) {
                check_eq!(unsafe { *(d as *mut u32).add(10) }, 0xabad_cafe, "heap_import_same_pages");
            }
            gem_close(card, h as u32);
        }
    }
    close(fd);
    close(heap);
}

fn main() {
    let card = open_virtio(c"/dev/dri/card0");
    match card {
        Some(c) => test_prime(c),
        None => skip("prime", "no virtio_gpu DRM device"),
    }
    test_heap(card);
    if let Some(c) = card {
        close(c);
    }
}
