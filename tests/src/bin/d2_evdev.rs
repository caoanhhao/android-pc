//! D2: input devices through uinput and evdev, as InputFlinger's
//! EventHub and getevent use them. A uinput device (UI_DEV_SETUP,
//! UI_ABS_SETUP, UI_SET_*BIT, UI_GET_SYSNAME) appears as inputN with an
//! eventM node; on that node: EVIOCGVERSION/GID/GNAME (truncation)/GPHYS/
//! GPROP/GBIT (types and codes)/GABS/GKEY/GLED/GMTSLOTS, EVIOCSCLOCKID
//! timestamps, reads of whole input_events (EINVAL for short buffers,
//! EAGAIN when empty), filtering of events that change nothing, packets
//! delivered at SYN_REPORT, a blocked read and epoll woken by another
//! process, EVIOCGRAB between two clients, EVIOCSMASK, LED events
//! written to the node coming back on the uinput fd, SYN_DROPPED on
//! overflow, and ENODEV / POLLHUP after UI_DEV_DESTROY. On the Rust
//! kernel also the virtio-input devices of the test VM (keyboard,
//! tablet): sysfs class/input, names, key and axis bits.
//!
//! The test device looks like a joystick (trigger-happy buttons, plain
//! axes, no BTN_TOUCH or INPUT_PROP_DIRECT) so a desktop on the Linux
//! host ignores it. Skipped when /dev/uinput cannot be opened.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("d2_evdev", main);

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const EV_LED: u16 = 0x11;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const BTN_A: u16 = 0x2c0; // BTN_TRIGGER_HAPPY1
const BTN_B: u16 = 0x2c1;
const KEY_A: u16 = 30;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const LED_CAPSL: u16 = 1;
const INPUT_PROP_BUTTONPAD: u16 = 2;
const CLOCK_MONOTONIC_ID: i32 = 1;

const UI_DEV_CREATE: u32 = 0x5501;
const UI_DEV_DESTROY: u32 = 0x5502;
const UI_DEV_SETUP: u32 = 0x405c_5503;
const UI_ABS_SETUP: u32 = 0x401c_5504;
const UI_GET_VERSION: u32 = 0x8004_552d;
const UI_SET_EVBIT: u32 = 0x4004_5564;
const UI_SET_KEYBIT: u32 = 0x4004_5565;
const UI_SET_ABSBIT: u32 = 0x4004_5567;
const UI_SET_LEDBIT: u32 = 0x4004_5569;
const UI_SET_PROPBIT: u32 = 0x4004_556e;

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

fn eioc(dir: u32, nr: u32, size: usize) -> u32 {
    dir << 30 | (size as u32) << 16 | (b'E' as u32) << 8 | nr
}

fn ui_sysname(len: usize) -> u32 {
    IOC_READ << 30 | (len as u32) << 16 | (b'U' as u32) << 8 | 44
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Ev {
    sec: i64,
    usec: i64,
    ty: u16,
    code: u16,
    value: i32,
}

fn enc(ty: u16, code: u16, value: i32) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[16..18].copy_from_slice(&ty.to_le_bytes());
    b[18..20].copy_from_slice(&code.to_le_bytes());
    b[20..24].copy_from_slice(&value.to_le_bytes());
    b
}

fn emit(fd: i32, evs: &[(u16, u16, i32)]) -> isize {
    let mut buf = [0u8; 24 * 16];
    for (i, &(t, c, v)) in evs.iter().enumerate() {
        buf[i * 24..i * 24 + 24].copy_from_slice(&enc(t, c, v));
    }
    write(fd, &buf[..evs.len() * 24])
}

/// Read what is queued (non-blocking fd); returns the events.
fn drain(fd: i32, out: &mut [Ev]) -> usize {
    let mut buf = [0u8; 24 * 64];
    let mut n = 0;
    loop {
        let r = read(fd, &mut buf);
        if r <= 0 {
            return n;
        }
        for e in buf[..r as usize].chunks_exact(24) {
            if n < out.len() {
                out[n] = Ev {
                    sec: i64::from_le_bytes(e[..8].try_into().unwrap()),
                    usec: i64::from_le_bytes(e[8..16].try_into().unwrap()),
                    ty: u16::from_le_bytes([e[16], e[17]]),
                    code: u16::from_le_bytes([e[18], e[19]]),
                    value: i32::from_le_bytes(e[20..24].try_into().unwrap()),
                };
            }
            n += 1;
        }
    }
}

fn bit(b: &[u8], i: u16) -> bool {
    b.get(i as usize / 8).is_some_and(|x| x & (1 << (i % 8)) != 0)
}

fn int_ioctl(fd: i32, cmd: u32, v: i32) -> isize {
    ioctl(fd, cmd, &v as *const i32 as usize)
}

fn abs_setup(ui: i32, code: u16, min: i32, max: i32) -> isize {
    let mut b = [0u8; 28];
    b[..2].copy_from_slice(&code.to_le_bytes());
    b[8..12].copy_from_slice(&min.to_le_bytes());
    b[12..16].copy_from_slice(&max.to_le_bytes());
    ioctl(ui, UI_ABS_SETUP, b.as_ptr() as usize)
}

fn absinfo(fd: i32, code: u16) -> Option<[i32; 6]> {
    let mut b = [0i32; 6];
    if ioctl(fd, eioc(IOC_READ, 0x40 + code as u32, 24), b.as_mut_ptr() as usize) < 0 { None } else { Some(b) }
}

/// Make the uinput device; returns (uinput fd, sysname "inputN").
fn create(ui: i32) -> Option<[u8; 32]> {
    check_ok!(ioctl(ui, UI_SET_EVBIT, EV_KEY as usize), "ui_set_evbit");
    ioctl(ui, UI_SET_EVBIT, EV_ABS as usize);
    ioctl(ui, UI_SET_EVBIT, EV_LED as usize);
    ioctl(ui, UI_SET_EVBIT, EV_SYN as usize);
    check_ok!(ioctl(ui, UI_SET_KEYBIT, BTN_A as usize), "ui_set_keybit");
    ioctl(ui, UI_SET_KEYBIT, BTN_B as usize);
    for a in [ABS_X, ABS_Y, ABS_MT_SLOT, ABS_MT_POSITION_X, ABS_MT_TRACKING_ID] {
        ioctl(ui, UI_SET_ABSBIT, a as usize);
    }
    ioctl(ui, UI_SET_LEDBIT, LED_CAPSL as usize);
    check_ok!(ioctl(ui, UI_SET_PROPBIT, INPUT_PROP_BUTTONPAD as usize), "ui_set_propbit");
    check_err!(ioctl(ui, UI_SET_KEYBIT, 0x300), EINVAL, "ui_set_keybit_range");
    check_ok!(abs_setup(ui, ABS_X, -100, 1000), "ui_abs_setup");
    abs_setup(ui, ABS_Y, 0, 255);
    abs_setup(ui, ABS_MT_SLOT, 0, 3);
    abs_setup(ui, ABS_MT_POSITION_X, 0, 1000);
    abs_setup(ui, ABS_MT_TRACKING_ID, 0, 65535);
    let mut setup = [0u8; 92];
    setup[..8].copy_from_slice(&[0x06, 0, 0x34, 0x12, 0x78, 0x56, 2, 0]);
    setup[8..21].copy_from_slice(b"d2-evdev-test");
    check_ok!(ioctl(ui, UI_DEV_SETUP, setup.as_ptr() as usize), "ui_dev_setup");
    if check_ok!(ioctl(ui, UI_DEV_CREATE, 0), "ui_dev_create") < 0 {
        return None;
    }
    check_err!(ioctl(ui, UI_DEV_CREATE, 0), EINVAL, "ui_dev_create_twice");
    let mut name = [0u8; 32];
    let r = ioctl(ui, ui_sysname(32), name.as_mut_ptr() as usize);
    check!(r > 0 && name.starts_with(b"input"), "ui_get_sysname", "{} {}", r, Bytes(&name));
    Some(name)
}

/// The eventM node below /sys/class/input/<sysname>, opened.
fn open_event(sysname: &[u8], flags: i32) -> i32 {
    let s = cfield(sysname);
    let dir = cpath!("/sys/class/input/{}", Bytes(s));
    let d = open(dir.as_ptr(), O_RDONLY | O_DIRECTORY, 0);
    if d < 0 {
        return -1;
    }
    let mut buf = [0u8; 2048];
    let mut ev = [0u8; 16];
    let mut evlen = 0;
    let n = getdents64(d as i32, &mut buf);
    close(d as i32);
    let mut off = 0;
    while off + 19 < n.max(0) as usize {
        let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
        let name = cfield(&buf[off + 19..off + reclen]);
        if name.starts_with(b"event") && name.len() < 16 {
            ev[..name.len()].copy_from_slice(name);
            evlen = name.len();
        }
        off += reclen;
    }
    if evlen == 0 {
        return -1;
    }
    let p = cpath!("/dev/input/{}", Bytes(&ev[..evlen]));
    // udev on the host creates the node a moment later.
    for _ in 0..60 {
        let fd = open(p.as_ptr(), flags, 0);
        if fd >= 0 {
            return fd as i32;
        }
        sleep_ms(50);
    }
    -1
}

fn poll_in(fd: i32, ms: i32) -> i32 {
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    let r = poll(&mut p, ms);
    if r < 0 { r as i32 } else { p[0].revents as i32 }
}

fn now_mono() -> (i64, i64) {
    let mut ts = Timespec::default();
    clock_gettime(CLOCK_MONOTONIC, &mut ts);
    (ts.sec, ts.nsec)
}

fn queries(fd: i32) {
    let mut v = 0i32;
    check_ok!(ioctl(fd, eioc(IOC_READ, 0x01, 4), &mut v as *mut i32 as usize), "eviocgversion");
    check_eq!(v, 0x010001, "evdev_version");
    let mut id = [0u16; 4];
    ioctl(fd, eioc(IOC_READ, 0x02, 8), id.as_mut_ptr() as usize);
    check_eq!(id, [0x06, 0x1234, 0x5678, 2], "eviocgid");
    let mut name = [0u8; 64];
    let r = ioctl(fd, eioc(IOC_READ, 0x06, 64), name.as_mut_ptr() as usize);
    check!(r == 14 && cfield(&name) == b"d2-evdev-test", "eviocgname", "{} {}", r, Bytes(cfield(&name)));
    let mut short = [0xffu8; 8];
    let r = ioctl(fd, eioc(IOC_READ, 0x06, 4), short.as_mut_ptr() as usize);
    check!(r == 4 && &short[..4] == b"d2-e" && short[4] == 0xff, "eviocgname_truncated", "{}", r);
    check_err!(ioctl(fd, eioc(IOC_READ, 0x08, 64), name.as_mut_ptr() as usize), ENOENT, "eviocguniq_none");
    let mut prop = [0u8; 8];
    let r = ioctl(fd, eioc(IOC_READ, 0x09, 8), prop.as_mut_ptr() as usize);
    check!(r == 8 && bit(&prop, INPUT_PROP_BUTTONPAD) && !bit(&prop, 1), "eviocgprop", "{}", r);
    let mut evbits = [0u8; 8];
    let r = ioctl(fd, eioc(IOC_READ, 0x20, 8), evbits.as_mut_ptr() as usize);
    check!(r == 8 && bit(&evbits, EV_SYN) && bit(&evbits, EV_KEY) && bit(&evbits, EV_ABS) && bit(&evbits, EV_LED) && !bit(&evbits, 2), "eviocgbit_types", "{}", r);
    let mut keys = [0u8; 128];
    let r = ioctl(fd, eioc(IOC_READ, 0x20 + EV_KEY as u32, 128), keys.as_mut_ptr() as usize);
    check!(r == 96 && bit(&keys, BTN_A) && bit(&keys, BTN_B) && !bit(&keys, KEY_A), "eviocgbit_keys", "{}", r);
    let mut abs = [0u8; 8];
    ioctl(fd, eioc(IOC_READ, 0x20 + EV_ABS as u32, 8), abs.as_mut_ptr() as usize);
    check!(bit(&abs, ABS_X) && bit(&abs, ABS_MT_SLOT) && bit(&abs, ABS_MT_POSITION_X) && !bit(&abs, 2), "eviocgbit_abs");
    check_err!(ioctl(fd, eioc(IOC_READ, 0x20 + 0x14, 8), abs.as_mut_ptr() as usize), EINVAL, "eviocgbit_bad_type");
    let x = absinfo(fd, ABS_X);
    check!(x.is_some_and(|x| x[0] == 0 && x[1] == -100 && x[2] == 1000), "eviocgabs", "{:?}", x);
    check!(absinfo(fd, 2).is_some_and(|z| z == [0; 6]), "eviocgabs_unused_axis_zero");
    // EVIOCSABS changes the range.
    let new = [0i32, -200, 2000, 4, 8, 0];
    check_ok!(ioctl(fd, eioc(IOC_WRITE, 0xc0 + ABS_X as u32, 24), new.as_ptr() as usize), "eviocsabs");
    check!(absinfo(fd, ABS_X).is_some_and(|x| x[1] == -200 && x[2] == 2000 && x[3] == 4), "eviocsabs_read_back");
    check_ok!(int_ioctl(fd, eioc(IOC_WRITE, 0xa0, 4), CLOCK_MONOTONIC_ID), "eviocsclockid");
    check_err!(int_ioctl(fd, eioc(IOC_WRITE, 0xa0, 4), 5), EINVAL, "eviocsclockid_bad");
    check_err!(ioctl(fd, eioc(IOC_READ, 0xb0, 4), 0), EINVAL, "unknown_ioctl");
}

fn events(ui: i32, fd: i32) {
    let mut evs = [Ev { sec: 0, usec: 0, ty: 0, code: 0, value: 0 }; 64];
    let mut small = [0u8; 16];
    check_err!(read(fd, &mut small), EINVAL, "read_short_buffer");
    check_err!(read(fd, &mut [0u8; 48]), EAGAIN, "read_empty_eagain");
    check_eq!(poll_in(fd, 0) & 1, 0, "poll_empty");

    // A key press: one packet, timestamped with CLOCK_MONOTONIC.
    let before = now_mono();
    check_eq!(emit(ui, &[(EV_KEY, BTN_A, 1), (EV_SYN, SYN_REPORT, 0)]), 48, "uinput_write_events");
    check!(poll_in(fd, 1000) & 1 != 0, "poll_readable");
    let n = drain(fd, &mut evs);
    check!(n == 2 && (evs[0].ty, evs[0].code, evs[0].value) == (EV_KEY, BTN_A, 1) && (evs[1].ty, evs[1].code) == (EV_SYN, SYN_REPORT), "read_key_packet", "{n} {:?}", &evs[..n.min(4)]);
    let t = evs[0].sec * 1_000_000 + evs[0].usec;
    let b = before.0 * 1_000_000 + before.1 / 1000;
    check!(t >= b && t < b + 5_000_000, "timestamp_monotonic", "{t} vs {b}");
    let mut keys = [0u8; 96];
    ioctl(fd, eioc(IOC_READ, 0x18, 96), keys.as_mut_ptr() as usize);
    check!(bit(&keys, BTN_A) && !bit(&keys, BTN_B), "eviocgkey_down");

    // Nothing changes: a second press, a zero-length packet, an
    // unsupported code are all dropped.
    emit(ui, &[(EV_KEY, BTN_A, 1), (EV_SYN, SYN_REPORT, 0)]);
    emit(ui, &[(EV_SYN, SYN_REPORT, 0)]);
    emit(ui, &[(EV_KEY, KEY_A, 1), (EV_SYN, SYN_REPORT, 0)]);
    check_eq!(drain(fd, &mut evs), 0, "unchanged_events_dropped");
    // Events are held until SYN_REPORT.
    emit(ui, &[(EV_KEY, BTN_A, 0), (EV_ABS, ABS_X, 500)]);
    check_eq!(drain(fd, &mut evs), 0, "held_until_syn");
    emit(ui, &[(EV_SYN, SYN_REPORT, 0)]);
    let n = drain(fd, &mut evs);
    check!(n == 3 && evs[0].code == BTN_A && evs[0].value == 0 && evs[1].ty == EV_ABS && evs[1].value == 500, "packet_at_syn", "{n}");
    check!(absinfo(fd, ABS_X).is_some_and(|x| x[0] == 500), "eviocgabs_value");
    ioctl(fd, eioc(IOC_READ, 0x18, 96), keys.as_mut_ptr() as usize);
    check!(!bit(&keys, BTN_A), "eviocgkey_up");

    // Multitouch slots.
    emit(ui, &[(EV_ABS, ABS_MT_SLOT, 1), (EV_ABS, ABS_MT_TRACKING_ID, 7), (EV_ABS, ABS_MT_POSITION_X, 321), (EV_SYN, SYN_REPORT, 0)]);
    let n = drain(fd, &mut evs);
    check!(n == 4 && evs[0].code == ABS_MT_SLOT && evs[0].value == 1 && evs[2].code == ABS_MT_POSITION_X, "mt_packet", "{n} {:?}", &evs[..n.min(4)]);
    let mut slots = [0i32; 5];
    slots[0] = ABS_MT_POSITION_X as i32;
    check_ok!(ioctl(fd, eioc(IOC_READ, 0x0a, 20), slots.as_mut_ptr() as usize), "eviocgmtslots");
    check!(slots[2] == 321, "mtslot_value", "{:?}", slots);
    let mut bad = [ABS_X as i32, 0, 0];
    check_err!(ioctl(fd, eioc(IOC_READ, 0x0a, 12), bad.as_mut_ptr() as usize), EINVAL, "eviocgmtslots_not_mt");
    check!(absinfo(fd, ABS_MT_SLOT).is_some_and(|s| s[0] == 1 && s[2] == 3), "eviocgabs_slot");

    // epoll and a blocked read, woken by another process.
    let ep = epoll_create1(O_CLOEXEC) as i32;
    check_ok!(epoll_ctl(ep, EPOLL_CTL_ADD, fd, EPOLLIN, 9), "epoll_add_evdev");
    let mut out = [EpollEvent { events: 0, data: 0 }; 2];
    check_eq!(epoll_wait(ep, &mut out, 0), 0, "epoll_idle");
    let pid = fork();
    if pid == 0 {
        sleep_ms(150);
        emit(ui, &[(EV_KEY, BTN_B, 1), (EV_SYN, SYN_REPORT, 0)]);
        sleep_ms(150);
        emit(ui, &[(EV_KEY, BTN_B, 0), (EV_SYN, SYN_REPORT, 0)]);
        exit(0);
    }
    check!(epoll_wait(ep, &mut out, 3000) == 1 && out[0].events & EPOLLIN != 0 && out[0].data == 9, "epoll_woken");
    drain(fd, &mut evs);
    let fl = fcntl(fd, 3 /* F_GETFL */, 0);
    fcntl(fd, 4 /* F_SETFL */, (fl as usize) & !(O_NONBLOCK as usize));
    let mut buf = [0u8; 24 * 4];
    let r = read(fd, &mut buf);
    check!(r == 48 && u16::from_le_bytes([buf[18], buf[19]]) == BTN_B && buf[20] == 0, "blocking_read_woken", "{r}");
    fcntl(fd, 4, fl as usize);
    let mut st = 0;
    wait4(pid as i32, &mut st, 0);
    close(ep);
}

fn clients(ui: i32, fd: i32, sysname: &[u8]) {
    let mut evs = [Ev { sec: 0, usec: 0, ty: 0, code: 0, value: 0 }; 64];
    let fd2 = open_event(sysname, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    check!(fd2 >= 0, "second_client_open");
    drain(fd, &mut evs);
    // Both clients get each event.
    emit(ui, &[(EV_ABS, ABS_Y, 10), (EV_SYN, SYN_REPORT, 0)]);
    check!(drain(fd, &mut evs) == 2 && drain(fd2, &mut evs) == 2, "two_clients_both_get");
    // A grab sends them to the grabbing client only.
    check_ok!(ioctl(fd, eioc(IOC_WRITE, 0x90, 4), 1), "eviocgrab");
    check_err!(ioctl(fd2, eioc(IOC_WRITE, 0x90, 4), 1), EBUSY, "eviocgrab_busy");
    check_err!(ioctl(fd2, eioc(IOC_WRITE, 0x90, 4), 0), EINVAL, "eviocgrab_release_not_owner");
    emit(ui, &[(EV_ABS, ABS_Y, 11), (EV_SYN, SYN_REPORT, 0)]);
    check!(drain(fd, &mut evs) == 2 && drain(fd2, &mut evs) == 0, "grabbed_events_exclusive");
    check_ok!(ioctl(fd, eioc(IOC_WRITE, 0x90, 4), 0), "eviocgrab_release");
    emit(ui, &[(EV_ABS, ABS_Y, 12), (EV_SYN, SYN_REPORT, 0)]);
    check!(drain(fd, &mut evs) == 2 && drain(fd2, &mut evs) == 2, "released_both_get");

    // EVIOCSMASK: client 2 masks out ABS_Y, then the whole EV_KEY type.
    let codes = [0u8; 8]; // no ABS code enabled
    let mut m = [0u8; 16];
    m[..4].copy_from_slice(&(EV_ABS as u32).to_le_bytes());
    m[4..8].copy_from_slice(&8u32.to_le_bytes());
    m[8..16].copy_from_slice(&(codes.as_ptr() as u64).to_le_bytes());
    check_ok!(ioctl(fd2, eioc(IOC_WRITE, 0x93, 16), m.as_ptr() as usize), "eviocsmask");
    emit(ui, &[(EV_ABS, ABS_Y, 13), (EV_SYN, SYN_REPORT, 0)]);
    check!(drain(fd, &mut evs) == 2 && drain(fd2, &mut evs) == 0, "masked_code_dropped");
    let types = [!(1u8 << EV_KEY), 0, 0, 0, 0, 0, 0, 0];
    m[..4].copy_from_slice(&0u32.to_le_bytes());
    m[4..8].copy_from_slice(&3u32.to_le_bytes());
    check_err!(ioctl(fd2, eioc(IOC_WRITE, 0x93, 16), m.as_ptr() as usize), EINVAL, "eviocsmask_size_not_long");
    m[4..8].copy_from_slice(&8u32.to_le_bytes());
    m[8..16].copy_from_slice(&(types.as_ptr() as u64).to_le_bytes());
    check_ok!(ioctl(fd2, eioc(IOC_WRITE, 0x93, 16), m.as_ptr() as usize), "eviocsmask_types");
    emit(ui, &[(EV_KEY, BTN_B, 1), (EV_SYN, SYN_REPORT, 0)]);
    let (a, b) = (drain(fd, &mut evs), drain(fd2, &mut evs));
    check!(a == 2 && b == 0, "masked_type_dropped", "{a} {b} {:?}", evs[0]);
    emit(ui, &[(EV_KEY, BTN_B, 0), (EV_SYN, SYN_REPORT, 0)]);
    drain(fd, &mut evs);

    // An LED written to the node: the clients see it, and so does the
    // uinput side (the "device").
    check_eq!(emit(fd2, &[(EV_LED, LED_CAPSL, 1), (EV_SYN, SYN_REPORT, 0)]), 48, "evdev_write_led");
    let n = drain(fd, &mut evs);
    check!(n == 2 && evs[0].ty == EV_LED && evs[0].value == 1, "led_to_clients", "{n}");
    let mut leds = [0u8; 8];
    ioctl(fd, eioc(IOC_READ, 0x19, 8), leds.as_mut_ptr() as usize);
    check!(bit(&leds, LED_CAPSL), "eviocgled");
    let n = drain(ui, &mut evs);
    check!(n >= 1 && evs[0].ty == EV_LED && evs[0].code == LED_CAPSL && evs[0].value == 1, "led_to_uinput", "{n}");
    check_err!(write(fd2, &[0u8; 10]), EINVAL, "evdev_write_short");
    close(fd2);

    // Overflow: the queue is replaced by SYN_DROPPED.
    for i in 0..1500 {
        emit(ui, &[(EV_ABS, ABS_Y, i % 200), (EV_SYN, SYN_REPORT, 0)]);
    }
    let mut first = [0u8; 24];
    let r = read(fd, &mut first);
    let n = 1 + drain(fd, &mut evs);
    check!(r == 24 && u16::from_le_bytes([first[16], first[17]]) == EV_SYN && u16::from_le_bytes([first[18], first[19]]) == SYN_DROPPED && n < 2400, "overflow_syn_dropped", "{r} {n}");
}

fn virtio_devices() {
    // The test VM has virtio-keyboard/-tablet/-multitouch (run-qemu.sh).
    let mut found_kbd = false;
    let mut found_tablet = false;
    for i in 0..8 {
        let p = cpath!("/dev/input/event{}", i);
        let fd = open(p.as_ptr(), O_RDONLY | O_NONBLOCK | O_CLOEXEC, 0);
        if fd < 0 {
            continue;
        }
        let fd = fd as i32;
        let mut name = [0u8; 64];
        ioctl(fd, eioc(IOC_READ, 0x06, 64), name.as_mut_ptr() as usize);
        let mut keys = [0u8; 96];
        ioctl(fd, eioc(IOC_READ, 0x21, 96), keys.as_mut_ptr() as usize);
        check_err!(read(fd, &mut [0u8; 24]), EAGAIN, "virtio_device_idle");
        if cfield(&name) == b"QEMU Virtio Keyboard" {
            found_kbd = true;
            check!(bit(&keys, KEY_A) && bit(&keys, 116 /* KEY_POWER */), "virtio_keyboard_keys");
            let mut leds = [0u8; 8];
            ioctl(fd, eioc(IOC_READ, 0x31, 8), leds.as_mut_ptr() as usize);
            check!(bit(&leds, LED_CAPSL), "virtio_keyboard_leds");
            // An LED change goes to the device's status queue.
            let w = open(p.as_ptr(), O_WRONLY | O_CLOEXEC, 0) as i32;
            check_eq!(emit(w, &[(EV_LED, LED_CAPSL, 1), (EV_SYN, SYN_REPORT, 0)]), 48, "virtio_keyboard_set_led");
            emit(w, &[(EV_LED, LED_CAPSL, 0), (EV_SYN, SYN_REPORT, 0)]);
            close(w);
            let s = cpath!("/sys/class/input/event{}/device/name", i);
            let mut b = [0u8; 64];
            let n = read_file(s.as_ptr(), &mut b);
            check!(n > 0 && trim(&b[..n as usize]) == b"QEMU Virtio Keyboard", "sysfs_input_name", "{}", Bytes(&b[..n.max(0) as usize]));
            let u = cpath!("/sys/class/input/event{}/uevent", i);
            let n = read_file(u.as_ptr(), &mut b);
            let ue = &b[..n.max(0) as usize];
            check!(ue.windows(6).any(|w| w == b"MAJOR=") && ue.windows(19).any(|w| w == b"DEVNAME=input/event"), "sysfs_event_uevent", "{}", Bytes(ue));
        } else if cfield(&name) == b"QEMU Virtio Tablet" {
            found_tablet = true;
            check!(absinfo(fd, ABS_X).is_some_and(|x| x[2] == 32767), "virtio_tablet_abs");
            check!(bit(&keys, 0x110 /* BTN_LEFT */), "virtio_tablet_buttons");
        }
        close(fd);
    }
    check!(found_kbd, "virtio_keyboard_present");
    check!(found_tablet, "virtio_tablet_present");
}

fn main() {
    let ui = open(c"/dev/uinput".as_ptr() as *const u8, O_RDWR | O_NONBLOCK | O_CLOEXEC, 0);
    if is_rust_kernel() {
        virtio_devices();
    } else {
        skip("virtio_devices", "host input devices differ");
    }
    if ui < 0 {
        check!(!is_rust_kernel(), "uinput_open", "{}", errname(ui));
        skip("uinput", "cannot open /dev/uinput");
        return;
    }
    let ui = ui as i32;
    let mut v = 0u32;
    check!(ioctl(ui, UI_GET_VERSION, &mut v as *mut u32 as usize) == 0 && v >= 5, "ui_get_version", "{v}");
    let Some(sysname) = create(ui) else { return };
    let fd = open_event(&sysname, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if !check!(fd >= 0, "evdev_open", "{}", fd) {
        return;
    }
    queries(fd);
    events(ui, fd);
    clients(ui, fd, &sysname);
    check_ok!(ioctl(ui, UI_DEV_DESTROY, 0), "ui_dev_destroy");
    check_err!(read(fd, &mut [0u8; 48]), ENODEV, "read_after_destroy");
    check!(poll_in(fd, 0) & (POLLHUP as i32 | POLLERR as i32) != 0, "poll_after_destroy");
    check_err!(ioctl(fd, eioc(IOC_READ, 0x01, 4), &mut v as *mut u32 as usize), ENODEV, "ioctl_after_destroy");
    close(fd);
    close(ui);
}
