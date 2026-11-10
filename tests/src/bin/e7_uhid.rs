//! E7: /dev/uhid, as Android's Bluetooth HID host uses it for Bluetooth
//! keyboards and mice. UHID_CREATE2 with a mouse's report descriptor
//! (numbered reports: five buttons, 12-bit relative X/Y, wheel) answers
//! UHID_START (numbered-reports flags) and UHID_OPEN on read; the device
//! shows up as an input device (bus 5, the ids and name given) whose
//! event node has the buttons and relative axes; a UHID_INPUT2 report
//! becomes BTN_LEFT, REL_X, REL_Y, REL_WHEEL and SYN_REPORT, a second one
//! the button release; reports of another id give nothing; UHID_DESTROY
//! answers UHID_CLOSE and UHID_STOP and the node goes away (ENODEV).
//! Skipped when /dev/uhid cannot be opened (needs root on a Linux host).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("e7_uhid", main);

const UHID_DESTROY: u32 = 1;
const UHID_START: u32 = 2;
const UHID_STOP: u32 = 3;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const EVENT_SIZE: usize = 4376;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const BTN_LEFT: u16 = 0x110;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const NAME: &[u8] = b"e7-uhid-test-mouse";
const EALREADY: i32 = 114;

/// Report id 2: five buttons, 3 bits padding, 12-bit X and Y, 8-bit wheel.
const MOUSE_RD: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, //
    0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, //
    0x81, 0x01, 0x05, 0x01, 0x16, 0x01, 0xf8, 0x26, 0xff, 0x07, 0x75, 0x0c, 0x95, 0x02, 0x09, 0x30, //
    0x09, 0x31, 0x81, 0x06, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x09, 0x38, 0x81, 0x06, //
    0xc0, 0xc0,
];

fn ev_type(e: &[u8]) -> u32 {
    u32::from_le_bytes([e[0], e[1], e[2], e[3]])
}

fn uhid_write(fd: i32, ty: u32, payload: &[u8]) -> isize {
    let mut e = [0u8; EVENT_SIZE];
    e[..4].copy_from_slice(&ty.to_le_bytes());
    e[4..4 + payload.len()].copy_from_slice(payload);
    write(fd, &e)
}

fn input2(fd: i32, report: &[u8]) -> isize {
    let mut p = [0u8; 2 + 64];
    p[..2].copy_from_slice(&(report.len() as u16).to_le_bytes());
    p[2..2 + report.len()].copy_from_slice(report);
    uhid_write(fd, UHID_INPUT2, &p)
}

/// The next uhid event's type (blocking up to `ms`).
fn uhid_read(fd: i32, ms: i32, out: &mut [u8; EVENT_SIZE]) -> Option<u32> {
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    if poll(&mut p, ms) <= 0 {
        return None;
    }
    let r = read(fd, out);
    if r == EVENT_SIZE as isize { Some(ev_type(out)) } else { None }
}

/// The /dev/input/eventM node of the input device named NAME.
fn find_event_node() -> Option<Path> {
    let d = open(c"/sys/class/input".as_ptr() as *const u8, O_RDONLY | O_DIRECTORY, 0);
    if d < 0 {
        return None;
    }
    let mut buf = [0u8; 4096];
    let n = getdents64(d as i32, &mut buf);
    close(d as i32);
    let mut off = 0;
    while off + 19 < n.max(0) as usize {
        let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
        let name = cfield(&buf[off + 19..off + reclen]);
        off += reclen;
        if !name.starts_with(b"input") {
            continue;
        }
        let p = cpath!("/sys/class/input/{}/name", Bytes(name));
        let f = open(p.as_ptr(), O_RDONLY, 0);
        if f < 0 {
            continue;
        }
        let mut nb = [0u8; 64];
        let r = read(f as i32, &mut nb);
        close(f as i32);
        if r <= 0 || !nb[..r as usize].starts_with(NAME) {
            continue;
        }
        // The eventM child of inputN.
        let dir = cpath!("/sys/class/input/{}", Bytes(name));
        let dd = open(dir.as_ptr(), O_RDONLY | O_DIRECTORY, 0);
        if dd < 0 {
            return None;
        }
        let mut b2 = [0u8; 2048];
        let m = getdents64(dd as i32, &mut b2);
        close(dd as i32);
        let mut o = 0;
        while o + 19 < m.max(0) as usize {
            let rl = u16::from_le_bytes([b2[o + 16], b2[o + 17]]) as usize;
            let en = cfield(&b2[o + 19..o + rl]);
            if en.starts_with(b"event") {
                return Some(cpath!("/dev/input/{}", Bytes(en)));
            }
            o += rl;
        }
    }
    None
}

fn bit(b: &[u8], i: u16) -> bool {
    b.get(i as usize / 8).is_some_and(|x| x & (1 << (i % 8)) != 0)
}

fn eioc_read(nr: u32, size: usize) -> u32 {
    2 << 30 | (size as u32) << 16 | (b'E' as u32) << 8 | nr
}

/// Read input_events (type, code, value) until `want` of them came or
/// a second passed.
fn read_events(fd: i32, want: usize, out: &mut [(u16, u16, i32); 16]) -> usize {
    let mut n = 0;
    let deadline = now_ns() + 1_000_000_000;
    let mut buf = [0u8; 24 * 16];
    while n < want && now_ns() < deadline {
        let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
        if poll(&mut p, 100) <= 0 {
            continue;
        }
        let r = read(fd, &mut buf);
        if r <= 0 {
            break;
        }
        for e in buf[..r as usize].chunks_exact(24) {
            if n < out.len() {
                out[n] = (
                    u16::from_le_bytes([e[16], e[17]]),
                    u16::from_le_bytes([e[18], e[19]]),
                    i32::from_le_bytes(e[20..24].try_into().unwrap()),
                );
                n += 1;
            }
        }
    }
    n
}

fn main() {
    let fd = open(c"/dev/uhid".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if fd < 0 {
        skip("uhid", "/dev/uhid cannot be opened");
        return;
    }
    let fd = fd as i32;
    let mut ev = [0u8; EVENT_SIZE];

    check_err!(write(fd, &[0u8; 2]), EINVAL, "uhid_short_write");
    check_err!(input2(fd, &[2, 0]), EINVAL, "uhid_input_before_create");

    // struct uhid_create2_req.
    let mut c = [0u8; 4372];
    c[..NAME.len()].copy_from_slice(NAME);
    c[128..136].copy_from_slice(b"e7-phys0");
    c[192..200].copy_from_slice(b"e7-uniq0");
    c[256..258].copy_from_slice(&(MOUSE_RD.len() as u16).to_le_bytes());
    c[258..260].copy_from_slice(&5u16.to_le_bytes()); // BUS_BLUETOOTH
    c[260..264].copy_from_slice(&0x1234u32.to_le_bytes());
    c[264..268].copy_from_slice(&0x5678u32.to_le_bytes());
    c[268..272].copy_from_slice(&0x0101u32.to_le_bytes());
    c[276..276 + MOUSE_RD.len()].copy_from_slice(MOUSE_RD);
    check_eq!(uhid_write(fd, UHID_CREATE2, &c), EVENT_SIZE as isize, "uhid_create2");
    check_err!(uhid_write(fd, UHID_CREATE2, &c), EALREADY, "uhid_create2_twice");
    let t = uhid_read(fd, 1000, &mut ev);
    check_eq!(t, Some(UHID_START), "uhid_start_event");
    let flags = u64::from_le_bytes(ev[4..12].try_into().unwrap());
    check_eq!(flags, 7, "uhid_start_numbered_reports");
    check_eq!(uhid_read(fd, 1000, &mut ev), Some(UHID_OPEN), "uhid_open_event");

    let Some(node) = find_event_node() else {
        check!(false, "uhid_input_device", "no input device named {}", Bytes(NAME));
        return;
    };
    let efd = open(node.as_ptr(), O_RDONLY | O_NONBLOCK | O_CLOEXEC, 0);
    if !check!(efd >= 0, "uhid_event_node_open", "{}", errname(efd)) {
        return;
    }
    let efd = efd as i32;
    let mut id = [0u16; 4];
    ioctl(efd, eioc_read(0x02, 8), id.as_mut_ptr() as usize);
    check_eq!(id, [5, 0x1234, 0x5678, 0x0101], "uhid_evdev_id");
    let mut keys = [0u8; 96];
    ioctl(efd, eioc_read(0x20 + EV_KEY as u32, 96), keys.as_mut_ptr() as usize);
    let mut rels = [0u8; 2];
    ioctl(efd, eioc_read(0x20 + EV_REL as u32, 2), rels.as_mut_ptr() as usize);
    check!(bit(&keys, BTN_LEFT) && bit(&keys, BTN_LEFT + 4) && !bit(&keys, BTN_LEFT + 5), "uhid_evdev_buttons");
    check!(bit(&rels, REL_X) && bit(&rels, REL_Y) && bit(&rels, REL_WHEEL), "uhid_evdev_axes");

    // Left button, X -3, Y +5, wheel -1.
    let xy = (-3i32 as u32 & 0xfff) | 5 << 12;
    let report = [2u8, 0x01, xy as u8, (xy >> 8) as u8, (xy >> 16) as u8, 0xff];
    check_eq!(input2(fd, &report), EVENT_SIZE as isize, "uhid_input2");
    let mut got = [(0u16, 0u16, 0i32); 16];
    let n = read_events(efd, 5, &mut got);
    check!(
        n == 5
            && got[..5] == [(EV_KEY, BTN_LEFT, 1), (EV_REL, REL_X, -3), (EV_REL, REL_Y, 5), (EV_REL, REL_WHEEL, -1), (EV_SYN, 0, 0)],
        "uhid_report_events",
        "{n} {:?}",
        &got[..n]
    );
    input2(fd, &[2, 0, 0, 0, 0, 0]);
    let n = read_events(efd, 2, &mut got);
    check!(n == 2 && got[..2] == [(EV_KEY, BTN_LEFT, 0), (EV_SYN, 0, 0)], "uhid_release_events", "{n} {:?}", &got[..n]);
    input2(fd, &[7, 1, 1, 1, 1, 1]);
    check_eq!(read_events(efd, 1, &mut got), 0, "uhid_other_report_id_ignored");

    check_eq!(uhid_write(fd, UHID_DESTROY, &[]), EVENT_SIZE as isize, "uhid_destroy");
    check_eq!(uhid_read(fd, 1000, &mut ev), Some(UHID_CLOSE), "uhid_close_event");
    check_eq!(uhid_read(fd, 1000, &mut ev), Some(UHID_STOP), "uhid_stop_event");
    check_err!(read(efd, &mut [0u8; 48]), ENODEV, "uhid_event_node_gone");
    close(efd);
    close(fd);
}
