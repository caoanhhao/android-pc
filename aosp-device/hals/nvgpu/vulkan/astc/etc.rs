// Copyright 2026 The Android PC Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! ETC2 and EAC block decoding (Khronos Data Format Specification 1.3, sections 21 and 22; the
//! same as OpenGL ES 3.0 annex C). NVIDIA's desktop driver has no ETC2 either
//! (`textureCompressionETC2 = VK_FALSE`), but still creates ETC2/EAC images, whose texels then
//! read as garbage: Where Winds Meet's terrain (Phase L4).
//!
//! Every block is 4x4 texels; texel `i` of a block's index bits is at x = i / 4, y = i % 4
//! (column-major). Decoded blocks are row-major: `[y * 4 + x]`.

/// The ETC1 intensity modifiers (`[a, b]`: the four values are +a, +b, -a, -b).
const ETC1_MODIFIERS: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

/// The T and H mode distances.
const DISTANCES: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

const EAC_MODIFIERS: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

/// Bits `hi..=lo` of `v`.
fn bits(v: u64, hi: u32, lo: u32) -> i32 {
    ((v >> lo) & ((1u64 << (hi - lo + 1)) - 1)) as i32
}

fn ext4(v: i32) -> i32 {
    v << 4 | v
}

fn ext5(v: i32) -> i32 {
    v << 3 | v >> 2
}

fn ext6(v: i32) -> i32 {
    v << 2 | v >> 4
}

fn ext7(v: i32) -> i32 {
    v << 1 | v >> 6
}

fn rgb(c: [i32; 3], d: i32) -> [u8; 4] {
    [
        (c[0] + d).clamp(0, 255) as u8,
        (c[1] + d).clamp(0, 255) as u8,
        (c[2] + d).clamp(0, 255) as u8,
        255,
    ]
}

/// The 2-bit index of texel `i` (column-major): MSB in bits 16..31, LSB in bits 0..15.
fn index2(v: u64, i: usize) -> usize {
    ((v >> (16 + i) & 1) << 1 | (v >> i & 1)) as usize
}

/// Decodes an ETC2 RGB8 block, or with `punchthrough` an ETC2 RGB8A1 block (whose bit 33 is
/// then the "opaque" bit), to row-major RGBA8 texels.
pub(crate) fn etc2_rgb(block: &[u8; 8], punchthrough: bool) -> [[u8; 4]; 16] {
    let v = u64::from_be_bytes(*block);
    let mut out = [[0u8; 4]; 16];
    let diff = punchthrough || v >> 33 & 1 == 1;
    let opaque = !punchthrough || v >> 33 & 1 == 1;
    let flip = v >> 32 & 1 == 1;
    // texel i (column-major) -> out[y * 4 + x]
    let put = |out: &mut [[u8; 4]; 16], i: usize, c: [u8; 4]| out[(i % 4) * 4 + i / 4] = c;

    let (c1, c2);
    if diff {
        let (r, g, b) = (bits(v, 63, 59), bits(v, 55, 51), bits(v, 47, 43));
        // 3-bit two's complement deltas
        let delta = |d: i32| (d << 29) >> 29;
        let (r2, g2, b2) = (
            r + delta(bits(v, 58, 56)),
            g + delta(bits(v, 50, 48)),
            b + delta(bits(v, 42, 40)),
        );
        if !(0..32).contains(&r2) {
            // T mode
            let c1 = [
                ext4(bits(v, 60, 59) << 2 | bits(v, 57, 56)),
                ext4(bits(v, 55, 52)),
                ext4(bits(v, 51, 48)),
            ];
            let c2 = [
                ext4(bits(v, 47, 44)),
                ext4(bits(v, 43, 40)),
                ext4(bits(v, 39, 36)),
            ];
            let d = DISTANCES[(bits(v, 35, 34) << 1 | bits(v, 32, 32)) as usize];
            let paint = [rgb(c1, 0), rgb(c2, d), rgb(c2, 0), rgb(c2, -d)];
            for i in 0..16 {
                let k = index2(v, i);
                put(
                    &mut out,
                    i,
                    if !opaque && k == 2 { [0; 4] } else { paint[k] },
                );
            }
            return out;
        }
        if !(0..32).contains(&g2) {
            // H mode
            let r1 = bits(v, 62, 59);
            let g1 = bits(v, 58, 56) << 1 | bits(v, 52, 52);
            let b1 = bits(v, 51, 51) << 3 | bits(v, 49, 47);
            let (r2, g2, b2) = (bits(v, 46, 43), bits(v, 42, 39), bits(v, 38, 35));
            let order = ((r1 << 8 | g1 << 4 | b1) >= (r2 << 8 | g2 << 4 | b2)) as i32;
            let d = DISTANCES[(bits(v, 34, 34) << 2 | bits(v, 32, 32) << 1 | order) as usize];
            let c1 = [ext4(r1), ext4(g1), ext4(b1)];
            let c2 = [ext4(r2), ext4(g2), ext4(b2)];
            let paint = [rgb(c1, d), rgb(c1, -d), rgb(c2, d), rgb(c2, -d)];
            for i in 0..16 {
                let k = index2(v, i);
                put(
                    &mut out,
                    i,
                    if !opaque && k == 2 { [0; 4] } else { paint[k] },
                );
            }
            return out;
        }
        if !(0..32).contains(&b2) {
            // planar mode (always opaque)
            let o = [
                ext6(bits(v, 62, 57)),
                ext7(bits(v, 56, 56) << 6 | bits(v, 54, 49)),
                ext6(bits(v, 48, 48) << 5 | bits(v, 44, 43) << 3 | bits(v, 41, 39)),
            ];
            let h = [
                ext6(bits(v, 38, 34) << 1 | bits(v, 32, 32)),
                ext7(bits(v, 31, 25)),
                ext6(bits(v, 24, 19)),
            ];
            let vv = [
                ext6(bits(v, 18, 13)),
                ext7(bits(v, 12, 6)),
                ext6(bits(v, 5, 0)),
            ];
            for y in 0..4i32 {
                for x in 0..4i32 {
                    let mut c = [0u8; 4];
                    for k in 0..3 {
                        let t = (x * (h[k] - o[k]) + y * (vv[k] - o[k]) + 4 * o[k] + 2) >> 2;
                        c[k] = t.clamp(0, 255) as u8;
                    }
                    c[3] = 255;
                    out[(y * 4 + x) as usize] = c;
                }
            }
            return out;
        }
        c1 = [ext5(r), ext5(g), ext5(b)];
        c2 = [ext5(r2), ext5(g2), ext5(b2)];
    } else {
        c1 = [
            ext4(bits(v, 63, 60)),
            ext4(bits(v, 55, 52)),
            ext4(bits(v, 47, 44)),
        ];
        c2 = [
            ext4(bits(v, 59, 56)),
            ext4(bits(v, 51, 48)),
            ext4(bits(v, 43, 40)),
        ];
    }
    let tables = [bits(v, 39, 37) as usize, bits(v, 36, 34) as usize];
    for i in 0..16 {
        let (x, y) = (i / 4, i % 4);
        let second = if flip { y >= 2 } else { x >= 2 };
        let (base, table) = if second {
            (c2, tables[1])
        } else {
            (c1, tables[0])
        };
        let [a, b] = ETC1_MODIFIERS[table];
        let k = index2(v, i);
        let c = match k {
            2 if !opaque => [0; 4],
            0 if !opaque => rgb(base, 0),
            0 => rgb(base, a),
            1 => rgb(base, b),
            2 => rgb(base, -a),
            _ => rgb(base, -b),
        };
        put(&mut out, i, c);
    }
    out
}

/// The 3-bit index of texel `i` (column-major) of an EAC block, and the block's base,
/// multiplier and modifier table.
fn eac_parts(v: u64) -> (i32, i32, &'static [i32; 8]) {
    (
        bits(v, 63, 56),
        bits(v, 55, 52),
        &EAC_MODIFIERS[bits(v, 51, 48) as usize],
    )
}

fn eac_index(v: u64, i: usize) -> usize {
    (v >> (45 - 3 * i as u32) & 7) as usize
}

/// Decodes the EAC alpha block of an ETC2 RGBA8 block to row-major 8-bit alpha.
pub(crate) fn eac_alpha(block: &[u8; 8]) -> [u8; 16] {
    let v = u64::from_be_bytes(*block);
    let (base, mult, table) = eac_parts(v);
    let mut out = [0u8; 16];
    for i in 0..16 {
        let a = base + table[eac_index(v, i)] * mult;
        out[(i % 4) * 4 + i / 4] = a.clamp(0, 255) as u8;
    }
    out
}

/// Decodes an EAC R11 block to row-major 16-bit values: UNORM, or with `signed` SNORM (as
/// `i16` bits), the R16 value of the same number.
pub(crate) fn eac_r11(block: &[u8; 8], signed: bool) -> [u16; 16] {
    let v = u64::from_be_bytes(*block);
    let (base, mult, table) = eac_parts(v);
    let mut out = [0u16; 16];
    for i in 0..16 {
        let m = table[eac_index(v, i)];
        let value = if signed {
            let base = (base as u8 as i8 as i32).max(-127);
            let r = if mult == 0 {
                base * 8 + m
            } else {
                base * 8 + m * mult * 8
            };
            let r = r.clamp(-1023, 1023);
            let mag = r.abs();
            let ext = mag << 5 | mag >> 5;
            (if r < 0 { -ext } else { ext }) as i16 as u16
        } else {
            let r = if mult == 0 {
                base * 8 + 4 + m
            } else {
                base * 8 + 4 + m * mult * 8
            };
            let r = r.clamp(0, 2047);
            (r << 5 | r >> 6) as u16
        };
        out[(i % 4) * 4 + i / 4] = value;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn individual_mode_solid() {
        // individual mode, both subblocks 0x8 (0x88), table 0, all indices 0 (+2)
        let b = [0x88, 0x88, 0x88, 0x00, 0, 0, 0, 0];
        assert!(etc2_rgb(&b, false)
            .iter()
            .all(|c| *c == [0x8a, 0x8a, 0x8a, 255]));
    }

    #[test]
    fn eac_unorm_full_range() {
        // base 255, multiplier 0, table 0, index 7 (+14): clamps to 2047 -> 0xffff
        let b = [0xff, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert!(eac_r11(&b, false).iter().all(|&v| v == 0xffff));
    }
}
