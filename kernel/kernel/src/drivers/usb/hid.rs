//! USB HID boot protocol (keyboards and mice): decoding the fixed boot
//! reports into input events, and the devices' capabilities.
//!
//! Keyboard report: modifier bits, a reserved byte, up to six usages of
//! pressed keys (all 0x01 on rollover error). Keys are mapped to Linux key
//! codes with the table of Linux's usbkbd/hid-input; presses and releases
//! come from the difference with the previous report. Mouse report:
//! button bits, signed X and Y motion, and (if the report is longer, as
//! most mice send even in boot mode) a signed wheel.

use super::super::input::{Caps, EV_KEY, EV_REL};

pub const BTN_LEFT: u16 = 0x110;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;

/// HID keyboard usage (page 7) -> Linux key code (Linux's usb_kbd_keycode).
#[rustfmt::skip]
pub const KEYMAP: [u8; 256] = [
      0,  0,  0,  0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38,
     50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,  2,  3,
      4,  5,  6,  7,  8,  9, 10, 11, 28,  1, 14, 15, 57, 12, 13, 26,
     27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64,
     65, 66, 67, 68, 87, 88, 99, 70,119,110,102,104,111,107,109,106,
    105,108,103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71,
     72, 73, 82, 83, 86,127,116,117,183,184,185,186,187,188,189,190,
    191,192,193,194,134,138,130,132,128,129,131,137,133,135,136,113,
    115,114,  0,  0,  0,121,  0, 89, 93,124, 92, 94, 95,  0,  0,  0,
    122,123, 90, 91, 85,  0,  0,  0,  0,  0,  0,  0,111,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     29, 42, 56,125, 97, 54,100,126,164,166,165,163,161,115,114,113,
    150,158,159,128,136,177,178,176,142,152,173,140,  0,  0,  0,  0,
];

/// Key codes of the eight modifier bits (usages 0xe0..0xe7).
fn modifier(bit: usize) -> u16 {
    KEYMAP[0xe0 + bit] as u16
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyboard,
    Mouse,
}

/// Turn a keyboard report into key events (`ev(type, code, value)`),
/// given the previous report. Returns false for a rollover-error report
/// (to be ignored, not remembered).
pub fn keyboard(prev: &[u8; 8], cur: &[u8], ev: &mut dyn FnMut(u16, u16, i32)) -> bool {
    if cur.len() < 3 || cur[2..].iter().take(6).all(|&u| u == 1) {
        return false;
    }
    let keys = |r: &[u8]| -> [u8; 6] {
        let mut k = [0u8; 6];
        for (i, &u) in r.iter().skip(2).take(6).enumerate() {
            k[i] = u;
        }
        k
    };
    let (old, new) = (keys(prev), keys(cur));
    let changed = prev[0] ^ cur[0];
    for bit in 0..8 {
        if changed & (1 << bit) != 0 {
            ev(EV_KEY, modifier(bit), (cur[0] >> bit & 1) as i32);
        }
    }
    for &u in old.iter().filter(|&&u| u > 3 && !new.contains(&u)) {
        if KEYMAP[u as usize] != 0 {
            ev(EV_KEY, KEYMAP[u as usize] as u16, 0);
        }
    }
    for &u in new.iter().filter(|&&u| u > 3 && !old.contains(&u)) {
        if KEYMAP[u as usize] != 0 {
            ev(EV_KEY, KEYMAP[u as usize] as u16, 1);
        }
    }
    true
}

/// Turn a mouse report into button and motion events; returns the new
/// button state.
pub fn mouse(prev_buttons: u8, cur: &[u8], ev: &mut dyn FnMut(u16, u16, i32)) -> u8 {
    let Some(&buttons) = cur.first() else { return prev_buttons };
    for bit in 0..3 {
        if (prev_buttons ^ buttons) & (1 << bit) != 0 {
            ev(EV_KEY, BTN_LEFT + bit, (buttons >> bit & 1) as i32);
        }
    }
    for (i, code) in [(1, REL_X), (2, REL_Y), (3, REL_WHEEL)] {
        if let Some(&v) = cur.get(i)
            && v != 0
        {
            ev(EV_REL, code, v as i8 as i32);
        }
    }
    buttons
}

/// What a boot keyboard or mouse reports.
pub fn caps(kind: Kind, name: &str) -> Caps {
    let mut c = Caps::new(name);
    c.ev.set(EV_KEY as usize, true);
    match kind {
        Kind::Keyboard => {
            for &k in KEYMAP.iter().filter(|&&k| k != 0) {
                c.codes[EV_KEY as usize].set(k as usize, true);
            }
        }
        Kind::Mouse => {
            c.ev.set(EV_REL as usize, true);
            for b in 0..3 {
                c.codes[EV_KEY as usize].set((BTN_LEFT + b) as usize, true);
            }
            for r in [REL_X, REL_Y, REL_WHEEL] {
                c.codes[EV_REL as usize].set(r as usize, true);
            }
        }
    }
    c
}

#[cfg(test)]
fn collect(f: impl FnOnce(&mut dyn FnMut(u16, u16, i32))) -> alloc::vec::Vec<(u16, u16, i32)> {
    let mut v = alloc::vec::Vec::new();
    f(&mut |t, c, x| v.push((t, c, x)));
    v
}

#[test_case]
fn hid_keyboard_reports() {
    let idle = [0u8; 8];
    // 'a' (usage 4) down with left shift, then 'b' added, then all up.
    let r1 = [0x02, 0, 4, 0, 0, 0, 0, 0];
    assert_eq!(collect(|e| assert!(keyboard(&idle, &r1, e))), [(EV_KEY, 42, 1), (EV_KEY, 30, 1)]);
    let r2 = [0x02, 0, 4, 5, 0, 0, 0, 0];
    assert_eq!(collect(|e| assert!(keyboard(&r1, &r2, e))), [(EV_KEY, 48, 1)]);
    assert_eq!(collect(|e| assert!(keyboard(&r2, &idle, e))), [(EV_KEY, 42, 0), (EV_KEY, 30, 0), (EV_KEY, 48, 0)]);
    // Rollover error: nothing, and the report is not kept.
    let err = [0, 0, 1, 1, 1, 1, 1, 1];
    assert!(collect(|e| assert!(!keyboard(&r1, &err, e))).is_empty());
    // Enter and right meta.
    assert_eq!(collect(|e| { keyboard(&idle, &[0x80, 0, 0x28, 0, 0, 0, 0, 0], e); }), [(EV_KEY, 126, 1), (EV_KEY, 28, 1)]);
}

#[test_case]
fn hid_mouse_reports() {
    let v = collect(|e| assert_eq!(mouse(0, &[1, 5, 0xfd, 0], e), 1));
    assert_eq!(v, [(EV_KEY, BTN_LEFT, 1), (EV_REL, REL_X, 5), (EV_REL, REL_Y, -3)]);
    let v = collect(|e| assert_eq!(mouse(1, &[2, 0, 0, 0xff], e), 2));
    assert_eq!(v, [(EV_KEY, BTN_LEFT, 0), (EV_KEY, BTN_LEFT + 1, 1), (EV_REL, REL_WHEEL, -1)]);
}
