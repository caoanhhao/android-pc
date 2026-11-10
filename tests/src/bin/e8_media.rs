//! E8: video capture (V4L2) and audio capture (ALSA) as AOSP's external
//! camera HAL and audio HAL (tinyalsa) use them, against the kernel's
//! test camera and microphone (`testcam=1`, which --ktest sets) or, on
//! the Linux host, the V4L2 part against a real webcam (the oracle; the
//! ALSA part needs the test microphone and is skipped there).
//!
//! Video: QUERYCAP, ENUM_FMT, ENUM_FRAMESIZES/FRAMEINTERVALS, G/TRY/S_FMT
//! (nearest size), G/S_PARM, ENUMINPUT/G_INPUT, REQBUFS (MMAP) and
//! QUERYBUF + mmap of every buffer, QBUF twice (EINVAL), DQBUF before
//! streaming (EINVAL), STREAMON, poll and epoll readiness, ten YUYV frames
//! (full size, increasing sequence and monotonic timestamps; on the test
//! camera the colour bars and the frame number in the picture), STREAMOFF
//! (DQBUF EINVAL, POLLERR), another open's REQBUFS (EBUSY), MJPEG frames
//! (SOI ... EOI), closing a streaming fd and opening again.
//!
//! Audio: the card's control node (PVERSION, CARD_INFO, ELEM_LIST), the
//! capture PCM opened twice (EBUSY), INFO, HW_REFINE of everything
//! (channels 1-2, 8-48 kHz), HW_PARAMS 16 kHz mono (the HAL's built-in
//! mic) and SW_PARAMS (boundary), PREPARE, SYNC_PTR state, a non-blocking
//! READI starting the stream, half a second of the 1 kHz test tone
//! (amplitude, about 2000 zero crossings a second, wall time), STATUS,
//! an overrun (EPIPE, then PREPARE recovers), DROP, then 48 kHz stereo
//! (both channels equal, a period of 48 frames) and HW_FREE.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("e8_media", main);

const fn vioc(dir: u32, nr: u32, size: u32) -> u32 {
    dir << 30 | size << 16 | (b'V' as u32) << 8 | nr
}
const fn aioc(dir: u32, ty: u8, nr: u32, size: u32) -> u32 {
    dir << 30 | size << 16 | (ty as u32) << 8 | nr
}
const R: u32 = 2;
const W: u32 = 1;
const RW: u32 = 3;

const VIDIOC_QUERYCAP: u32 = vioc(R, 0, 104);
const VIDIOC_ENUM_FMT: u32 = vioc(RW, 2, 64);
const VIDIOC_G_FMT: u32 = vioc(RW, 4, 208);
const VIDIOC_S_FMT: u32 = vioc(RW, 5, 208);
const VIDIOC_REQBUFS: u32 = vioc(RW, 8, 20);
const VIDIOC_QUERYBUF: u32 = vioc(RW, 9, 88);
const VIDIOC_QBUF: u32 = vioc(RW, 15, 88);
const VIDIOC_DQBUF: u32 = vioc(RW, 17, 88);
const VIDIOC_STREAMON: u32 = vioc(W, 18, 4);
const VIDIOC_STREAMOFF: u32 = vioc(W, 19, 4);
const VIDIOC_G_PARM: u32 = vioc(RW, 21, 204);
const VIDIOC_S_PARM: u32 = vioc(RW, 22, 204);
const VIDIOC_ENUMINPUT: u32 = vioc(RW, 26, 80);
const VIDIOC_G_CTRL: u32 = vioc(RW, 27, 8);
const VIDIOC_G_INPUT: u32 = vioc(R, 38, 4);
const VIDIOC_TRY_FMT: u32 = vioc(RW, 64, 208);
const VIDIOC_ENUM_FRAMESIZES: u32 = vioc(RW, 74, 44);
const VIDIOC_ENUM_FRAMEINTERVALS: u32 = vioc(RW, 75, 52);

const MJPG: u32 = u32::from_le_bytes(*b"MJPG");
const YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const CAPTURE: u32 = 1;
const MMAP: u32 = 1;
const TEST_CAMERA: &[u8] = b"android-pc test camera";
const TEST_MIC: &[u8] = b"android-pc test microphone";

const PCM_PVERSION: u32 = aioc(R, b'A', 0x00, 4);
const PCM_INFO: u32 = aioc(R, b'A', 0x01, 288);
const PCM_HW_REFINE: u32 = aioc(RW, b'A', 0x10, 608);
const PCM_HW_PARAMS: u32 = aioc(RW, b'A', 0x11, 608);
const PCM_HW_FREE: u32 = aioc(0, b'A', 0x12, 0);
const PCM_SW_PARAMS: u32 = aioc(RW, b'A', 0x13, 136);
const PCM_STATUS: u32 = aioc(R, b'A', 0x20, 152);
const PCM_SYNC_PTR: u32 = aioc(RW, b'A', 0x23, 136);
const PCM_PREPARE: u32 = aioc(0, b'A', 0x40, 0);
const PCM_DROP: u32 = aioc(0, b'A', 0x43, 0);
const PCM_READI_FRAMES: u32 = aioc(R, b'A', 0x51, 24);
const CTL_PVERSION: u32 = aioc(R, b'U', 0x00, 4);
const CTL_CARD_INFO: u32 = aioc(R, b'U', 0x01, 376);
const CTL_ELEM_LIST: u32 = aioc(RW, b'U', 0x10, 80);

fn g32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn g64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn p32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn p64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
fn io<const N: usize>(fd: i32, cmd: u32, b: &mut [u8; N]) -> isize {
    ioctl(fd, cmd, b.as_mut_ptr() as usize)
}

fn main() {
    video();
    audio();
}

// ---------------------------------------------------------------------------
// Video.

/// Open the test camera (or, on the host, the first capture device).
fn open_camera() -> Option<(i32, bool)> {
    let mut fallback = None;
    for i in 0..16 {
        let p = cpath!("/dev/video{}", i);
        let fd = open(p.as_ptr(), O_RDWR, 0);
        if fd < 0 {
            continue;
        }
        let fd = fd as i32;
        let mut cap = [0u8; 104];
        if io(fd, VIDIOC_QUERYCAP, &mut cap) == 0 && g32(&cap, 88) & 1 != 0 {
            if cfield(&cap[16..48]) == TEST_CAMERA {
                if let Some((f, _)) = fallback {
                    close(f);
                }
                return Some((fd, true));
            }
            if fallback.is_none() && !is_rust_kernel() {
                fallback = Some((fd, false));
                continue;
            }
        }
        close(fd);
    }
    fallback
}

fn fmt_req(fourcc: u32, w: u32, h: u32) -> [u8; 208] {
    let mut b = [0u8; 208];
    p32(&mut b, 0, CAPTURE);
    p32(&mut b, 8, w);
    p32(&mut b, 12, h);
    p32(&mut b, 16, fourcc);
    p32(&mut b, 20, 1);
    b
}

fn buf_req(i: u32) -> [u8; 88] {
    let mut b = [0u8; 88];
    p32(&mut b, 0, i);
    p32(&mut b, 4, CAPTURE);
    p32(&mut b, 60, MMAP);
    b
}

fn reqbufs(fd: i32, count: u32, memory: u32) -> (isize, u32) {
    let mut b = [0u8; 20];
    p32(&mut b, 0, count);
    p32(&mut b, 4, CAPTURE);
    p32(&mut b, 8, memory);
    let r = io(fd, VIDIOC_REQBUFS, &mut b);
    (r, g32(&b, 0))
}

fn stream(fd: i32, on: bool) -> isize {
    let mut b = [0u8; 4];
    p32(&mut b, 0, CAPTURE);
    io(fd, if on { VIDIOC_STREAMON } else { VIDIOC_STREAMOFF }, &mut b)
}

/// The frame sizes of a format.
fn sizes(fd: i32, fourcc: u32) -> ([(u32, u32); 32], usize) {
    let mut out = [(0, 0); 32];
    let mut n = 0;
    while n < 32 {
        let mut b = [0u8; 44];
        p32(&mut b, 0, n as u32);
        p32(&mut b, 4, fourcc);
        if io(fd, VIDIOC_ENUM_FRAMESIZES, &mut b) < 0 {
            break;
        }
        if g32(&b, 8) != 1 {
            break; // stepwise: not used by these devices
        }
        out[n] = (g32(&b, 12), g32(&b, 16));
        n += 1;
    }
    (out, n)
}

/// Mapped buffers: (address, length).
struct Bufs {
    maps: [(usize, usize); 8],
    n: usize,
}

impl Bufs {
    fn unmap(&mut self) {
        for &(a, l) in &self.maps[..self.n] {
            munmap(a as *mut u8, l);
        }
        self.n = 0;
    }
}

/// REQBUFS `count`, QUERYBUF and mmap each, QBUF all.
fn setup_buffers(fd: i32, count: u32, sizeimage: u32, tag: &str) -> Bufs {
    let mut bufs = Bufs { maps: [(0, 0); 8], n: 0 };
    let (r, n) = reqbufs(fd, count, MMAP);
    check!(r == 0 && (2..=8).contains(&n), tag, "REQBUFS {} -> {} buffers", r, n);
    for i in 0..n.min(8) {
        let mut b = buf_req(i);
        if io(fd, VIDIOC_QUERYBUF, &mut b) < 0 {
            check!(false, "querybuf", "buffer {}", i);
            return bufs;
        }
        let (off, len) = (g32(&b, 64), g32(&b, 72));
        if i == 0 {
            check!(len >= sizeimage && g32(&b, 60) == MMAP, "querybuf", "length {} sizeimage {}", len, sizeimage);
        }
        let a = mmap(0, len as usize, PROT_READ | PROT_WRITE, MAP_SHARED, fd, off as i64);
        if a < 0 {
            check!(false, "mmap_buffer", "buffer {}: {}", i, errname(a));
            return bufs;
        }
        bufs.maps[i as usize] = (a as usize, len as usize);
        bufs.n = i as usize + 1;
        let mut q = buf_req(i);
        let r = io(fd, VIDIOC_QBUF, &mut q);
        if i == 0 {
            check_ok!(r, "qbuf");
            check!(g32(&q, 12) & 2 != 0, "qbuf_flags", "flags {:#x}", g32(&q, 12));
        }
    }
    let mut q = buf_req(0);
    check_err!(io(fd, VIDIOC_QBUF, &mut q), EINVAL, "qbuf_twice");
    bufs
}

/// DQBUF, waiting with poll; the buffer struct.
fn dqbuf(fd: i32) -> Option<[u8; 88]> {
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    if poll(&mut p, 3000) <= 0 || p[0].revents & POLLIN == 0 {
        return None;
    }
    let mut b = buf_req(0);
    (io(fd, VIDIOC_DQBUF, &mut b) == 0).then_some(b)
}

fn video() {
    let Some((fd, test)) = open_camera() else {
        skip("video", "no V4L2 capture device (run with testcam=1)");
        return;
    };
    let mut cap = [0u8; 104];
    check_ok!(io(fd, VIDIOC_QUERYCAP, &mut cap), "querycap");
    let (caps, dcaps) = (g32(&cap, 84), g32(&cap, 88));
    check!(
        caps & 0x8000_0000 != 0 && dcaps & 0x0400_0001 == 0x0400_0001 && !cfield(&cap[48..80]).is_empty(),
        "querycap_fields",
        "caps {:#x} device_caps {:#x} card {} bus {}",
        caps,
        dcaps,
        Bytes(cfield(&cap[16..48])),
        Bytes(cfield(&cap[48..80]))
    );
    println!("# e8_media: camera \"{}\" ({}, {})", Bytes(cfield(&cap[16..48])), Bytes(cfield(&cap[..16])), Bytes(cfield(&cap[48..80])));

    // Formats.
    let mut fourccs = [0u32; 8];
    let mut nf = 0;
    while nf < 8 {
        let mut b = [0u8; 64];
        p32(&mut b, 0, nf as u32);
        p32(&mut b, 4, CAPTURE);
        if io(fd, VIDIOC_ENUM_FMT, &mut b) < 0 {
            break;
        }
        let fc = g32(&b, 44);
        if fc == MJPG {
            check!(g32(&b, 8) & 1 != 0, "enum_fmt_compressed", "flags {:#x}", g32(&b, 8));
        }
        fourccs[nf] = fc;
        nf += 1;
    }
    let mut b = [0u8; 64];
    p32(&mut b, 0, nf as u32);
    p32(&mut b, 4, CAPTURE);
    check_err!(io(fd, VIDIOC_ENUM_FMT, &mut b), EINVAL, "enum_fmt_end");
    let has = |f: u32| fourccs[..nf].contains(&f);
    check!(has(MJPG) || has(YUYV), "enum_fmt", "{} formats", nf);
    if test {
        check_eq!((nf, fourccs[0], fourccs[1]), (2, MJPG, YUYV), "enum_fmt_test_camera");
    }

    // Sizes and intervals.
    let (ys, nys) = sizes(fd, YUYV);
    let (ms, nms) = sizes(fd, MJPG);
    check!(nys + nms > 0, "enum_framesizes", "{} YUYV, {} MJPEG", nys, nms);
    if test {
        check_eq!((nys, ys[0], ys[1], nms, ms[0]), (2, (640, 480), (320, 240), 1, (320, 240)), "enum_framesizes_test_camera");
    }
    let mut b = [0u8; 44];
    p32(&mut b, 4, u32::from_le_bytes(*b"XXXX"));
    check_err!(io(fd, VIDIOC_ENUM_FRAMESIZES, &mut b), EINVAL, "enum_framesizes_unknown");
    let (fmt, (w, h)) = if nys > 0 {
        // The smallest YUYV size.
        let mut s = ys[0];
        for &x in &ys[..nys] {
            if x.0 * x.1 < s.0 * s.1 {
                s = x;
            }
        }
        (YUYV, s)
    } else {
        (MJPG, ms[0])
    };
    let mut b = [0u8; 52];
    p32(&mut b, 4, fmt);
    p32(&mut b, 8, w);
    p32(&mut b, 12, h);
    let r = io(fd, VIDIOC_ENUM_FRAMEINTERVALS, &mut b);
    check!(r == 0 && g32(&b, 16) == 1 && g32(&b, 20) > 0 && g32(&b, 24) > 0, "enum_frameintervals", "{} type {} {}/{}", r, g32(&b, 16), g32(&b, 20), g32(&b, 24));

    // Formats: TRY does not change the current one; S_FMT does.
    let mut cur = [0u8; 208];
    p32(&mut cur, 0, CAPTURE);
    check_ok!(io(fd, VIDIOC_G_FMT, &mut cur), "g_fmt");
    let mut t = fmt_req(fmt, w + 13, h - 7);
    check_ok!(io(fd, VIDIOC_TRY_FMT, &mut t), "try_fmt");
    check_eq!((g32(&t, 8), g32(&t, 12), g32(&t, 16)), (w, h, fmt), "try_fmt_nearest");
    let mut again = [0u8; 208];
    p32(&mut again, 0, CAPTURE);
    io(fd, VIDIOC_G_FMT, &mut again);
    check!(again[..40] == cur[..40], "try_fmt_no_change");
    let mut s = fmt_req(fmt, w, h);
    check_ok!(io(fd, VIDIOC_S_FMT, &mut s), "s_fmt");
    let sizeimage = g32(&s, 28);
    if fmt == YUYV {
        check_eq!((g32(&s, 8), g32(&s, 12), g32(&s, 24), sizeimage), (w, h, w * 2, w * h * 2), "s_fmt_yuyv");
    }
    let mut g = [0u8; 208];
    p32(&mut g, 0, CAPTURE);
    io(fd, VIDIOC_G_FMT, &mut g);
    check_eq!((g32(&g, 8), g32(&g, 12), g32(&g, 16)), (w, h, fmt), "g_fmt_after_s_fmt");

    // 30 fps.
    let mut p = [0u8; 204];
    p32(&mut p, 0, CAPTURE);
    p32(&mut p, 12, 1);
    p32(&mut p, 16, 30);
    check_ok!(io(fd, VIDIOC_S_PARM, &mut p), "s_parm");
    let mut p = [0u8; 204];
    p32(&mut p, 0, CAPTURE);
    check_ok!(io(fd, VIDIOC_G_PARM, &mut p), "g_parm");
    let (num, den) = (g32(&p, 12), g32(&p, 16));
    check!(g32(&p, 4) & 0x1000 != 0 && num > 0 && den / num >= 15, "g_parm_timeperframe", "cap {:#x} {}/{}", g32(&p, 4), num, den);

    // Input.
    let mut inp = [0u8; 80];
    check_ok!(io(fd, VIDIOC_ENUMINPUT, &mut inp), "enuminput");
    check_eq!(g32(&inp, 36), 2, "enuminput_camera");
    p32(&mut inp, 0, 1);
    check_err!(io(fd, VIDIOC_ENUMINPUT, &mut inp), EINVAL, "enuminput_end");
    let mut gi = [0xffu8; 4];
    check_ok!(io(fd, VIDIOC_G_INPUT, &mut gi), "g_input");
    check_eq!(g32(&gi, 0), 0, "g_input_0");
    if test {
        let mut c = [0u8; 8];
        p32(&mut c, 0, 0x0098_0900); // V4L2_CID_BRIGHTNESS
        check_err!(io(fd, VIDIOC_G_CTRL, &mut c), EINVAL, "g_ctrl_none");
        check_err!(reqbufs(fd, 4, 2).0, EINVAL, "reqbufs_userptr");
    }

    // Not streaming yet.
    let mut bufs = setup_buffers(fd, 4, sizeimage, "reqbufs");
    let fl = fcntl(fd, F_GETFL, 0) as usize;
    fcntl(fd, F_SETFL, fl | O_NONBLOCK as usize);
    let mut d = buf_req(0);
    check_err!(io(fd, VIDIOC_DQBUF, &mut d), EINVAL, "dqbuf_not_streaming");
    fcntl(fd, F_SETFL, fl);

    // Another open cannot take the buffers.
    let index = video_index(fd);
    let other = open(cpath!("/dev/video{}", index).as_ptr(), O_RDWR, 0);
    if other >= 0 {
        check_err!(reqbufs(other as i32, 2, MMAP).0, EBUSY, "reqbufs_other_open");
        close(other as i32);
    }

    // Stream.
    let ep = epoll_create1(0) as i32;
    epoll_ctl(ep, EPOLL_CTL_ADD, fd, EPOLLIN, 7);
    check_ok!(stream(fd, true), "streamon");
    let mut evs = [EpollEvent { events: 0, data: 0 }; 2];
    let r = epoll_wait(ep, &mut evs, 3000);
    let (events, data) = (evs[0].events, evs[0].data);
    check!(r == 1 && events & EPOLLIN != 0 && data == 7, "epoll_frame", "epoll_wait {} events {:#x}", r, events);
    close(ep);
    let t0 = now_ns();
    let (mut last_seq, mut last_ts, mut last_n) = (None::<u32>, 0i64, None::<u32>);
    let mut good = 0;
    let mut content_ok = true;
    let frames = 10;
    for k in 0..frames {
        let Some(b) = dqbuf(fd) else {
            check!(false, "dqbuf", "frame {} did not come", k);
            break;
        };
        let (i, used, flags, seq) = (g32(&b, 0) as usize, g32(&b, 8), g32(&b, 12), g32(&b, 56));
        let ts = g64(&b, 24) as i64 * 1_000_000 + g64(&b, 32) as i64;
        let ok_seq = last_seq.is_none_or(|l| seq > l);
        let ok_ts = ts > last_ts && flags & 0xe000 == 0x2000;
        let ok_size = if fmt == YUYV { used == sizeimage } else { used > 0 && used <= sizeimage };
        if k == 0 || !(ok_seq && ok_ts && ok_size) {
            check!(ok_seq && ok_ts && ok_size, "dqbuf_frame", "frame {}: buffer {} {} bytes flags {:#x} seq {} ts {}", k, i, used, flags, seq, ts);
        }
        if ok_seq && ok_ts && ok_size {
            good += 1;
        }
        if test && fmt == YUYV && i < bufs.n {
            let px = unsafe { core::slice::from_raw_parts(bufs.maps[i].0 as *const u8, used as usize) };
            // The frame number in the first 16 even pixels' luma.
            let mut n = 0u32;
            for bit in 0..16 {
                if px[bit * 4] > 128 {
                    n |= 1 << bit;
                }
            }
            // The bottom row: eight bars, white to black.
            let row = &px[((h - 1) * w * 2) as usize..];
            let lumas = [235u8, 210, 170, 145, 106, 81, 41, 16];
            let bars = (0..8).all(|b| row[((b * w / 8 + w / 16) * 2) as usize & !3] == lumas[b as usize]);
            let ok = bars && last_n.is_none_or(|l| n > l);
            if !ok {
                content_ok = false;
                println!("# e8_media: frame {} number {} (previous {:?}), bars {}", k, n, last_n, bars);
            }
            last_n = Some(n);
        }
        last_seq = Some(seq);
        last_ts = ts;
        let mut q = buf_req(i as u32);
        io(fd, VIDIOC_QBUF, &mut q);
    }
    let elapsed = (now_ns() - t0) / 1_000_000;
    check_eq!(good, frames, "dqbuf_frames");
    if test {
        check!(content_ok, "frame_content");
        // Ten frames at 30 fps after the first: about 300 ms.
        check!((200..1500).contains(&elapsed), "frame_rate", "{} frames in {} ms", frames, elapsed);
    }
    println!("# e8_media: {} frames of {}x{} in {} ms", frames, w, h, elapsed);
    check_ok!(stream(fd, false), "streamoff");
    fcntl(fd, F_SETFL, fl | O_NONBLOCK as usize);
    let mut d = buf_req(0);
    check_err!(io(fd, VIDIOC_DQBUF, &mut d), EINVAL, "dqbuf_after_streamoff");
    fcntl(fd, F_SETFL, fl);
    let mut pf = [Pollfd { fd, events: POLLIN, revents: 0 }];
    poll(&mut pf, 0);
    check!(pf[0].revents & POLLERR != 0, "poll_after_streamoff", "revents {:#x}", pf[0].revents);
    bufs.unmap();
    check_eq!(reqbufs(fd, 0, MMAP), (0, 0), "reqbufs_free");

    // MJPEG.
    if nms > 0 {
        let mut s = fmt_req(MJPG, ms[0].0, ms[0].1);
        check_ok!(io(fd, VIDIOC_S_FMT, &mut s), "s_fmt_mjpeg");
        let size = g32(&s, 28);
        let mut bufs = setup_buffers(fd, 3, size, "reqbufs_mjpeg");
        check_ok!(stream(fd, true), "streamon_mjpeg");
        // Real cameras may send a damaged frame or two after STREAMON
        // (Linux hands them out with V4L2_BUF_FLAG_ERROR; the Rust kernel
        // drops them): three good frames out of six.
        let mut ok = 0;
        for _ in 0..6 {
            if ok == 3 {
                break;
            }
            let Some(b) = dqbuf(fd) else { break };
            let (i, used, flags) = (g32(&b, 0) as usize, g32(&b, 8) as usize, g32(&b, 12));
            if flags & 0x40 != 0 {
                println!("# e8_media: MJPEG frame with V4L2_BUF_FLAG_ERROR ({} bytes)", used);
            } else if i < bufs.n && used >= 4 {
                let d = unsafe { core::slice::from_raw_parts(bufs.maps[i].0 as *const u8, used) };
                // Some cameras pad after EOI: look for it near the end.
                let eoi = d[used.saturating_sub(64)..].windows(2).any(|x| x == [0xff, 0xd9]);
                if d[..2] == [0xff, 0xd8] && eoi {
                    ok += 1;
                } else {
                    println!("# e8_media: MJPEG frame {} bytes starts {:02x?}", used, &d[..4]);
                }
            }
            let mut q = buf_req(i as u32);
            io(fd, VIDIOC_QBUF, &mut q);
        }
        check_eq!(ok, 3, "mjpeg_frames");
        bufs.unmap();
        // Closing while streaming stops and frees everything.
        close(fd);
        let fd2 = open(cpath!("/dev/video{}", index).as_ptr(), O_RDWR, 0);
        check_ok!(fd2, "reopen");
        if fd2 >= 0 {
            check_eq!(reqbufs(fd2 as i32, 2, MMAP).0, 0, "reqbufs_after_close");
            reqbufs(fd2 as i32, 0, MMAP);
            close(fd2 as i32);
        }
    } else {
        close(fd);
    }
}

/// The /dev/videoN number of an open camera (from /proc/self/fd).
fn video_index(fd: i32) -> i32 {
    let mut b = [0u8; 64];
    let p = cpath!("/proc/self/fd/{}", fd);
    let n = readlink(p.as_ptr(), &mut b);
    let s = &b[..n.max(0) as usize];
    s.strip_prefix(b"/dev/video").and_then(parse_int).unwrap_or(0) as i32
}

// ---------------------------------------------------------------------------
// Audio.

fn open_mic() -> Option<(i32, u32)> {
    for c in 0..8 {
        let fd = open(cpath!("/dev/snd/controlC{}", c).as_ptr(), O_RDWR, 0);
        if fd < 0 {
            continue;
        }
        let mut info = [0u8; 376];
        if io(fd as i32, CTL_CARD_INFO, &mut info) == 0 && cfield(&info[40..72]) == TEST_MIC {
            return Some((fd as i32, c));
        }
        close(fd as i32);
    }
    None
}

fn any_params() -> [u8; 608] {
    let mut b = [0u8; 608];
    b[4..260].fill(0xff);
    for i in 0..12 {
        p32(&mut b, 260 + i * 12 + 4, u32::MAX);
    }
    p32(&mut b, 512, u32::MAX); // rmask
    b
}

fn iv(b: &[u8], p: usize) -> (u32, u32) {
    let o = 260 + (p - 8) * 12;
    (g32(b, o), g32(b, o + 4))
}

fn set_iv(b: &mut [u8], p: usize, min: u32, max: u32) {
    let o = 260 + (p - 8) * 12;
    p32(b, o, min);
    p32(b, o + 4, max);
    p32(b, o + 8, 4);
}

const CHANNELS: usize = 10;
const RATE: usize = 11;
const PERIOD_SIZE: usize = 13;
const PERIODS: usize = 15;
const BUFFER_SIZE: usize = 17;

/// HW_PARAMS as tinyalsa's pcm_open does, then SW_PARAMS; the boundary.
fn hw_params(fd: i32, rate: u32, ch: u32, period: u32, periods: u32, stop: u64) -> Option<u64> {
    let mut b = any_params();
    // RW interleaved, S16_LE.
    b[4..36].fill(0);
    b[4] = 1 << 3;
    b[36..68].fill(0);
    b[36] = 1 << 2;
    set_iv(&mut b, CHANNELS, ch, ch);
    set_iv(&mut b, RATE, rate, rate);
    set_iv(&mut b, PERIODS, periods, periods);
    set_iv(&mut b, PERIOD_SIZE, period, u32::MAX);
    if check_ok!(io(fd, PCM_HW_PARAMS, &mut b), "hw_params") < 0 {
        return None;
    }
    check_eq!(
        (iv(&b, RATE), iv(&b, CHANNELS), iv(&b, PERIOD_SIZE), iv(&b, BUFFER_SIZE)),
        ((rate, rate), (ch, ch), (period, period), (period * periods, period * periods)),
        "hw_params_chosen"
    );
    let mut s = [0u8; 136];
    p32(&mut s, 0, 1); // tstamp_mode ENABLE
    p32(&mut s, 4, 1);
    p64(&mut s, 16, period as u64); // avail_min
    p64(&mut s, 32, 1); // start_threshold
    p64(&mut s, 40, stop);
    check_ok!(io(fd, PCM_SW_PARAMS, &mut s), "sw_params");
    let boundary = g64(&s, 64);
    check!(boundary >= (period * periods) as u64, "sw_params_boundary", "{}", boundary);
    Some(boundary)
}

/// Read `frames` frames with READI on a non-blocking fd, polling.
fn read_frames(fd: i32, out: &mut [i16], ch: usize) -> isize {
    let frames = out.len() / ch;
    let mut done = 0;
    let t0 = now_ns();
    while done < frames && now_ns() - t0 < 3_000_000_000 {
        let mut x = [0u8; 24];
        p64(&mut x, 8, out[done * ch..].as_mut_ptr() as u64);
        p64(&mut x, 16, (frames - done) as u64);
        let r = io(fd, PCM_READI_FRAMES, &mut x);
        if r == 0 {
            done += g64(&x, 0) as usize;
        } else if r == -(EAGAIN as isize) {
            let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
            poll(&mut p, 500);
        } else {
            return r;
        }
    }
    done as isize
}

fn state(fd: i32) -> u32 {
    let mut s = [0u8; 136];
    p32(&mut s, 0, 2 | 4); // APPL | AVAIL_MIN: read only
    io(fd, PCM_SYNC_PTR, &mut s);
    g32(&s, 8)
}

fn audio() {
    let Some((ctl, card)) = open_mic() else {
        skip("audio", "no test microphone card (run with testcam=1)");
        return;
    };
    let mut v = [0u8; 4];
    check_ok!(io(ctl, CTL_PVERSION, &mut v), "ctl_pversion");
    let mut info = [0u8; 376];
    io(ctl, CTL_CARD_INFO, &mut info);
    check_eq!((g32(&info, 0), cfield(&info[8..24])), (card, &b"TestMic"[..]), "ctl_card_info");
    let mut l = [0u8; 80];
    check_ok!(io(ctl, CTL_ELEM_LIST, &mut l), "ctl_elem_list");
    check_eq!(g32(&l, 12), 0, "ctl_elem_count");
    close(ctl);

    let path = cpath!("/dev/snd/pcmC{}D0c", card);
    let fd = open(path.as_ptr(), O_RDWR | O_NONBLOCK, 0);
    if check_ok!(fd, "pcm_open") < 0 {
        return;
    }
    let fd = fd as i32;
    check_err!(open(path.as_ptr(), O_RDWR | O_NONBLOCK, 0), EBUSY, "pcm_open_twice");
    let mut v = [0u8; 4];
    check_ok!(io(fd, PCM_PVERSION, &mut v), "pcm_pversion");
    let mut pi = [0u8; 288];
    check_ok!(io(fd, PCM_INFO, &mut pi), "pcm_info");
    check_eq!((g32(&pi, 0), g32(&pi, 8), g32(&pi, 12)), (0, 1, card), "pcm_info_capture");
    let mut any = any_params();
    check_ok!(io(fd, PCM_HW_REFINE, &mut any), "hw_refine");
    check_eq!((iv(&any, CHANNELS), iv(&any, RATE), iv(&any, 8)), ((1, 2), (8000, 48000), (16, 16)), "hw_refine_ranges");

    // The HAL's built-in mic: 16 kHz mono, periods of 20 ms.
    if hw_params(fd, 16000, 1, 320, 4, u64::MAX / 2).is_none() {
        close(fd);
        return;
    }
    check_eq!(state(fd), 1, "state_setup");
    check_ok!(ioctl(fd, PCM_PREPARE, 0), "prepare");
    check_eq!(state(fd), 2, "state_prepared");
    let mut buf = [0i16; 8000];
    let mut x = [0u8; 24];
    p64(&mut x, 8, buf.as_mut_ptr() as u64);
    p64(&mut x, 16, 320);
    let r = io(fd, PCM_READI_FRAMES, &mut x);
    check!(r == -(EAGAIN as isize) || r == 0, "readi_starts", "{}", errname(r));
    check_eq!(state(fd), 3, "state_running");
    let t0 = now_ns();
    let n = read_frames(fd, &mut buf, 1);
    let ms = (now_ns() - t0) / 1_000_000;
    check_eq!(n, 8000, "readi_16k_mono");
    let peak = buf.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    let crossings = buf.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    // 0.5 s of 1 kHz: 1000 crossings; box-filtered peaks a bit below 8000.
    check!((5000..=8000).contains(&peak) && (900..=1100).contains(&crossings), "tone_16k", "peak {} crossings {}", peak, crossings);
    check!((350..2000).contains(&ms), "readi_wall_time", "0.5 s of audio in {} ms", ms);
    let mut st = [0u8; 152];
    check_ok!(io(fd, PCM_STATUS, &mut st), "status");
    check!(g32(&st, 0) == 3 && g64(&st, 48) >= g64(&st, 40), "status_running", "state {} hw {} appl {}", g32(&st, 0), g64(&st, 48), g64(&st, 40));
    let mut sp = [0u8; 136];
    p32(&mut sp, 0, 1 | 2 | 4); // HWSYNC, read appl and avail_min
    check_ok!(io(fd, PCM_SYNC_PTR, &mut sp), "sync_ptr");
    check!(g64(&sp, 16) >= 8000 && (g64(&sp, 24) > 0 || g64(&sp, 32) > 0), "sync_ptr_hw_ptr", "hw_ptr {} tstamp {}.{}", g64(&sp, 16), g64(&sp, 24), g64(&sp, 32));

    // Overrun: stop threshold at the buffer size, then stop reading.
    check_ok!(ioctl(fd, PCM_DROP, 0), "drop");
    check_eq!(state(fd), 1, "state_after_drop");
    hw_params(fd, 16000, 1, 320, 4, 1280);
    ioctl(fd, PCM_PREPARE, 0);
    read_frames(fd, &mut buf[..320], 1);
    sleep_ms(300);
    let mut x = [0u8; 24];
    p64(&mut x, 8, buf.as_mut_ptr() as u64);
    p64(&mut x, 16, 320);
    check_err!(io(fd, PCM_READI_FRAMES, &mut x), EPIPE, "overrun_epipe");
    check_ok!(ioctl(fd, PCM_PREPARE, 0), "prepare_after_overrun");
    check_eq!(read_frames(fd, &mut buf[..320], 1), 320, "read_after_overrun");
    ioctl(fd, PCM_DROP, 0);

    // 48 kHz stereo: the device's own format.
    hw_params(fd, 48000, 2, 1200, 4, u64::MAX / 2);
    ioctl(fd, PCM_PREPARE, 0);
    let n = read_frames(fd, &mut buf[..4800], 2);
    check_eq!(n, 2400, "readi_48k_stereo");
    let same = buf[..4800].chunks_exact(2).all(|f| f[0] == f[1]);
    // A triangle repeats every 48 frames (skip the first, interpolation
    // starts from silence).
    // (A reader that fell behind by more than the buffer loses frames:
    // allow for one gap.)
    let periodic = (1..2300).filter(|&i| buf[i * 2] == buf[(i + 48) * 2]).count();
    check!(same && periodic >= 2200, "tone_48k", "channels equal {} periodic {} of 2299, {:?}", same, periodic, &buf[200..208]);
    ioctl(fd, PCM_DROP, 0);
    check_ok!(ioctl(fd, PCM_HW_FREE, 0), "hw_free");
    check_eq!(state(fd), 0, "state_open");
    close(fd);
    let fd = open(path.as_ptr(), O_RDWR | O_NONBLOCK, 0);
    check_ok!(fd, "pcm_reopen");
    if fd >= 0 {
        close(fd as i32);
    }
}
