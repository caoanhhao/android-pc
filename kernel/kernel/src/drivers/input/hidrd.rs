//! HID report descriptors: the input fields of a descriptor and the
//! events their reports mean, enough for keyboards, mice and media keys
//! (what Bluetooth HID devices are, through `/dev/uhid`).
//!
//! The parser keeps the global items (usage page, logical range, report
//! size/count/id, push/pop) and the local usages of each Input main item,
//! making one [`Field`] per item. Mapped usages: keyboard page 7 (key
//! codes of [`KEYMAP`]), buttons (page 9: BTN_LEFT.., BTN_MISC.. past
//! five), generic desktop X/Y/wheel (relative only) and a few consumer
//! keys and AC Pan (horizontal wheel). Variable fields give a value per
//! usage (keys pressed while non-zero, relative axes as motion); array
//! fields list the usages pressed. Absolute pointers (tablets,
//! touchscreens), outputs and features are not mapped.

use alloc::vec::Vec;

use super::{EV_KEY, EV_REL};
use crate::drivers::usb::hid::KEYMAP;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const BTN_LEFT: u16 = 0x110;
const BTN_MISC: u16 = 0x100;

/// One Input main item.
#[derive(Clone, Debug)]
pub struct Field {
    pub report_id: u8,
    /// Bit offset in the report (after the report id byte).
    pub offset: usize,
    pub size: usize,
    pub count: usize,
    pub flags: u32,
    pub logical_min: i32,
    pub logical_max: i32,
    /// Usages (page << 16 | id), one per element for a variable field;
    /// for an array, `usage_min..` is what value `logical_min` means.
    pub usages: Vec<u32>,
    pub usage_min: u32,
}

impl Field {
    fn constant(&self) -> bool {
        self.flags & 1 != 0
    }

    fn variable(&self) -> bool {
        self.flags & 2 != 0
    }

    fn relative(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// A parsed descriptor.
#[derive(Default, Debug)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    pub numbered: bool,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u32,
    lmin: i32,
    lmax: i32,
    size: usize,
    count: usize,
    id: u8,
}

pub fn parse(rd: &[u8]) -> Descriptor {
    let mut d = Descriptor::default();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut umin, mut umax) = (None, None);
    // Bits used so far per report id.
    let mut bits = [0usize; 256];
    let mut i = 0;
    while i < rd.len() {
        let b = rd[i];
        if b == 0xfe {
            // Long item: skip.
            let n = rd.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + n;
            continue;
        }
        let n = [0, 1, 2, 4][(b & 3) as usize];
        if i + 1 + n > rd.len() {
            break;
        }
        let data = &rd[i + 1..i + 1 + n];
        let u = data.iter().rev().fold(0u32, |a, &x| a << 8 | x as u32);
        let s = match n {
            1 => data[0] as i8 as i32,
            2 => i16::from_le_bytes([data[0], data[1]]) as i32,
            4 => u as i32,
            _ => 0,
        };
        let full = |u: u32, g: &Globals| if n == 4 { u } else { g.page << 16 | u };
        match b & 0xfc {
            // Main: Input.
            0x80 => {
                let mut list: Vec<u32> = usages.clone();
                if let (Some(a), Some(z)) = (umin, umax) {
                    let (a, z): (u32, u32) = (a, z);
                    if z >= a && z - a < 1024 {
                        list.extend(a..=z);
                    }
                }
                let start = bits[g.id as usize];
                bits[g.id as usize] += g.size * g.count;
                d.fields.push(Field {
                    report_id: g.id,
                    offset: start,
                    size: g.size,
                    count: g.count,
                    flags: u,
                    logical_min: g.lmin,
                    logical_max: g.lmax,
                    usage_min: umin.unwrap_or_else(|| list.first().copied().unwrap_or(0)),
                    usages: list,
                });
                usages.clear();
                (umin, umax) = (None, None);
            }
            // Main: Output, Feature, Collection, End Collection.
            0x90 | 0xb0 | 0xa0 | 0xc0 => {
                usages.clear();
                (umin, umax) = (None, None);
            }
            0x04 => g.page = u,
            0x14 => g.lmin = s,
            0x24 => g.lmax = if g.lmin >= 0 && n < 4 { u as i32 } else { s },
            0x74 => g.size = u as usize,
            0x84 => {
                g.id = u as u8;
                d.numbered = true;
            }
            0x94 => g.count = u as usize,
            0xa4 => stack.push(g),
            0xb4 => g = stack.pop().unwrap_or(g),
            0x08 => usages.push(full(u, &g)),
            0x18 => umin = Some(full(u, &g)),
            0x28 => umax = Some(full(u, &g)),
            _ => {}
        }
        i += 1 + n;
    }
    d
}

/// The event (type, code) of a usage, if mapped.
pub fn map(usage: u32, relative: bool) -> Option<(u16, u16)> {
    let (page, id) = (usage >> 16, usage & 0xffff);
    match page {
        0x07 if id < 256 && KEYMAP[id as usize] != 0 => Some((EV_KEY, KEYMAP[id as usize] as u16)),
        0x09 if (1..=5).contains(&id) => Some((EV_KEY, BTN_LEFT + id as u16 - 1)),
        0x09 if (6..=16).contains(&id) => Some((EV_KEY, BTN_MISC + id as u16 - 6)),
        0x01 if relative => match id {
            0x30 => Some((EV_REL, REL_X)),
            0x31 => Some((EV_REL, REL_Y)),
            0x38 => Some((EV_REL, REL_WHEEL)),
            _ => None,
        },
        0x0c => match id {
            0x238 if relative => Some((EV_REL, REL_HWHEEL)),
            0xe2 => Some((EV_KEY, 113)),        // KEY_MUTE
            0xe9 => Some((EV_KEY, 115)),        // KEY_VOLUMEUP
            0xea => Some((EV_KEY, 114)),        // KEY_VOLUMEDOWN
            0xcd => Some((EV_KEY, 164)),        // KEY_PLAYPAUSE
            0xb5 => Some((EV_KEY, 163)),        // KEY_NEXTSONG
            0xb6 => Some((EV_KEY, 165)),        // KEY_PREVIOUSSONG
            0xb7 => Some((EV_KEY, 166)),        // KEY_STOPCD
            0x223 => Some((EV_KEY, 172)),       // KEY_HOMEPAGE
            0x224 => Some((EV_KEY, 158)),       // KEY_BACK
            0x6f => Some((EV_KEY, 225)),        // KEY_BRIGHTNESSUP
            0x70 => Some((EV_KEY, 224)),        // KEY_BRIGHTNESSDOWN
            _ => None,
        },
        _ => None,
    }
}

fn extract(r: &[u8], bit: usize, size: usize, signed: bool) -> i32 {
    let mut v: u64 = 0;
    for k in 0..size.min(32) {
        let b = bit + k;
        if b / 8 < r.len() && r[b / 8] >> (b % 8) & 1 != 0 {
            v |= 1 << k;
        }
    }
    if signed && size > 0 && size < 32 && v >> (size - 1) & 1 != 0 {
        (v | !0u64 << size) as i64 as i32
    } else {
        v as u32 as i32
    }
}

/// Decodes reports into events, remembering key state.
pub struct Decoder {
    pub desc: Descriptor,
    /// Keys down per field.
    down: Vec<Vec<u16>>,
}

impl Decoder {
    pub fn new(rd: &[u8]) -> Decoder {
        let desc = parse(rd);
        let down = desc.fields.iter().map(|_| Vec::new()).collect();
        Decoder { desc, down }
    }

    /// Every (type, code) the descriptor can report.
    pub fn capabilities(&self) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for f in self.desc.fields.iter().filter(|f| !f.constant()) {
            if f.variable() {
                for &u in &f.usages {
                    out.extend(map(u, f.relative()));
                }
            } else {
                let span = (f.logical_max - f.logical_min).clamp(0, 1023) as u32;
                for k in 0..=span {
                    out.extend(map(f.usage_min + k, false));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// One input report (with its id byte if the descriptor numbers
    /// reports): its events, without the SYN_REPORT.
    pub fn report(&mut self, r: &[u8], ev: &mut dyn FnMut(u16, u16, i32)) {
        let (id, body) = if self.desc.numbered {
            match r.split_first() {
                Some((&id, rest)) => (id, rest),
                None => return,
            }
        } else {
            (0, r)
        };
        for (fi, f) in self.desc.fields.iter().enumerate() {
            if f.report_id != id || f.constant() || f.size == 0 {
                continue;
            }
            let signed = f.logical_min < 0;
            let mut now: Vec<u16> = Vec::new();
            for k in 0..f.count {
                let v = extract(body, f.offset + k * f.size, f.size, signed);
                if f.variable() {
                    let Some(&u) = f.usages.get(k).or(f.usages.last()) else { continue };
                    match map(u, f.relative()) {
                        Some((EV_REL, code)) if v != 0 => ev(EV_REL, code, v),
                        Some((EV_KEY, code)) if v != 0 => now.push(code),
                        _ => {}
                    }
                } else if v >= f.logical_min && v <= f.logical_max {
                    let u = f.usage_min + (v - f.logical_min) as u32;
                    // Usage 0 (no key) and the keyboard's error codes 1-3.
                    if u & 0xffff == 0 || u >> 16 == 7 && u & 0xffff <= 3 {
                        continue;
                    }
                    if let Some((EV_KEY, code)) = map(u, false) {
                        now.push(code);
                    }
                }
            }
            let before = &self.down[fi];
            for &c in before.iter().filter(|c| !now.contains(c)) {
                ev(EV_KEY, c, 0);
            }
            for &c in now.iter().filter(|c| !before.contains(c)) {
                ev(EV_KEY, c, 1);
            }
            self.down[fi] = now;
        }
    }
}

/// A Bluetooth mouse's descriptor (Logitech-style, report id 2: five
/// buttons, 12-bit X/Y, wheel, AC Pan) and a keyboard's (boot layout,
/// report id 1, plus consumer keys with id 3).
#[cfg(test)]
const MOUSE_RD: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, //
    0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, //
    0x81, 0x01, 0x05, 0x01, 0x16, 0x01, 0xf8, 0x26, 0xff, 0x07, 0x75, 0x0c, 0x95, 0x02, 0x09, 0x30, //
    0x09, 0x31, 0x81, 0x06, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x09, 0x38, 0x81, 0x06, //
    0x05, 0x0c, 0x0a, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0, 0x00, 0x00,
];

#[cfg(test)]
const KEYBOARD_RD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, //
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, //
    0x75, 0x08, 0x15, 0x00, 0x26, 0xff, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2a, 0xff, 0x00, 0x81, 0x00, //
    0xc0, 0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x03, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, //
    0x02, 0x09, 0xe9, 0x09, 0xea, 0x81, 0x02, 0x95, 0x06, 0x81, 0x01, 0xc0, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00,
];

#[test_case]
fn hid_report_descriptor_mouse() {
    let mut d = Decoder::new(MOUSE_RD);
    let caps = d.capabilities();
    assert!(caps.contains(&(EV_KEY, BTN_LEFT)) && caps.contains(&(EV_KEY, BTN_LEFT + 4)));
    assert!(caps.contains(&(EV_REL, REL_X)) && caps.contains(&(EV_REL, REL_WHEEL)) && caps.contains(&(EV_REL, REL_HWHEEL)));
    let mut evs = Vec::new();
    // Left button, X = -3, Y = +5 (12-bit), wheel -1, no pan.
    let x = (-3i32 as u32) & 0xfff;
    let y = 5u32;
    let xy = x | y << 12;
    d.report(&[0x02, 0x01, xy as u8, (xy >> 8) as u8, (xy >> 16) as u8, 0xff, 0x00], &mut |t, c, v| evs.push((t, c, v)));
    assert_eq!(evs, [(EV_KEY, BTN_LEFT, 1), (EV_REL, REL_X, -3), (EV_REL, REL_Y, 5), (EV_REL, REL_WHEEL, -1)]);
    evs.clear();
    d.report(&[0x02, 0x00, 0, 0, 0, 0, 0], &mut |t, c, v| evs.push((t, c, v)));
    assert_eq!(evs, [(EV_KEY, BTN_LEFT, 0)]);
    // Another report id: ignored.
    evs.clear();
    d.report(&[0x05, 0x01], &mut |t, c, v| evs.push((t, c, v)));
    assert!(evs.is_empty());
}

#[test_case]
fn hid_report_descriptor_keyboard() {
    let mut d = Decoder::new(KEYBOARD_RD);
    let caps = d.capabilities();
    assert!(caps.contains(&(EV_KEY, 30)) && caps.contains(&(EV_KEY, 42)) && caps.contains(&(EV_KEY, 115)));
    let mut evs = Vec::new();
    // Left shift + 'a' (usage 4), then 'a' released and 'b' (5) pressed.
    d.report(&[0x01, 0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut |t, c, v| evs.push((t, c, v)));
    assert_eq!(evs, [(EV_KEY, 42, 1), (EV_KEY, 30, 1)]);
    evs.clear();
    d.report(&[0x01, 0x02, 0, 0x05, 0, 0, 0, 0, 0], &mut |t, c, v| evs.push((t, c, v)));
    assert_eq!(evs, [(EV_KEY, 30, 0), (EV_KEY, 48, 1)]);
    evs.clear();
    // Volume up on the consumer report.
    d.report(&[0x03, 0x01], &mut |t, c, v| evs.push((t, c, v)));
    assert_eq!(evs, [(EV_KEY, 115, 1)]);
}
