//! The cryptography of the WPA2-Personal 4-way handshake, for the
//! simulated access point (`hwsim`): SHA-1, HMAC-SHA1, PBKDF2 (the PSK
//! from a passphrase), the IEEE 802.11 PRF, AES-128 (encryption only)
//! and the AES key wrap (RFC 3394) that protects the group key.

use alloc::vec::Vec;

pub fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let mut msg: Vec<u8> = Vec::with_capacity(total + 72);
    for p in parts {
        msg.extend_from_slice(p);
    }
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((total as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut wv = [0u32; 80];
        for i in 0..16 {
            wv[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            wv[i] = (wv[i - 3] ^ wv[i - 8] ^ wv[i - 14] ^ wv[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &x) in wv.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(x);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(&[key]));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut inner: Vec<&[u8]> = alloc::vec![&ipad];
    inner.extend_from_slice(parts);
    let ih = sha1(&inner);
    sha1(&[&opad, &ih])
}

/// The WPA PSK: PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32).
pub fn wpa_psk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for blk in 0..2u32 {
        let mut u = hmac_sha1(passphrase, &[ssid, &(blk + 1).to_be_bytes()]);
        let mut t = u;
        for _ in 1..4096 {
            u = hmac_sha1(passphrase, &[&u]);
            for i in 0..20 {
                t[i] ^= u[i];
            }
        }
        let n = if blk == 0 { 20 } else { 12 };
        out[blk as usize * 20..blk as usize * 20 + n].copy_from_slice(&t[..n]);
    }
    out
}

/// IEEE 802.11 PRF-(8·len) with HMAC-SHA1.
pub fn prf(key: &[u8], label: &[u8], data: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i = 0u8;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha1(key, &[label, &[0], data, &[i]]));
        i += 1;
    }
    out.truncate(len);
    out
}

const SBOX: [u8; 256] = {
    // Generated from the multiplicative inverse in GF(2^8) and the
    // affine transform, at compile time.
    let mut s = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p *= 3
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
        // q /= 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
};

fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

/// AES-128 encryption of one block.
pub fn aes128_encrypt(key: &[u8; 16], block: &mut [u8; 16]) {
    let mut rk = [0u8; 176];
    rk[..16].copy_from_slice(key);
    let mut rcon = 1u8;
    for i in 4..44 {
        let mut t = [rk[(i - 1) * 4], rk[(i - 1) * 4 + 1], rk[(i - 1) * 4 + 2], rk[(i - 1) * 4 + 3]];
        if i % 4 == 0 {
            t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
            rcon = xtime(rcon);
        }
        for j in 0..4 {
            rk[i * 4 + j] = rk[(i - 4) * 4 + j] ^ t[j];
        }
    }
    let s = block;
    for j in 0..16 {
        s[j] ^= rk[j];
    }
    for round in 1..11 {
        for b in s.iter_mut() {
            *b = SBOX[*b as usize];
        }
        // ShiftRows (column-major state).
        let t = *s;
        for c in 0..4 {
            for r in 0..4 {
                s[c * 4 + r] = t[((c + r) % 4) * 4 + r];
            }
        }
        if round != 10 {
            for c in 0..4 {
                let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
                let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                for r in 0..4 {
                    s[c * 4 + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                }
            }
        }
        for j in 0..16 {
            s[j] ^= rk[round * 16 + j];
        }
    }
}

/// AES key wrap (RFC 3394) of `plain` (a multiple of 8 bytes).
pub fn aes_wrap(kek: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let n = plain.len() / 8;
    let mut a = [0xa6u8; 8];
    let mut r: Vec<[u8; 8]> = plain.chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in 0..6 {
        for i in 0..n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes128_encrypt(kek, &mut b);
            let t = (n * j + i + 1) as u64;
            a.copy_from_slice(&b[..8]);
            for (k, x) in t.to_be_bytes().iter().enumerate() {
                a[k] ^= x;
            }
            r[i].copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    for x in r {
        out.extend_from_slice(&x);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn sha1_abc() {
        assert_eq!(sha1(&[b"abc"])[..4], [0xa9, 0x99, 0x3e, 0x36]);
    }

    #[test_case]
    fn aes_fips197() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let mut b: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        aes128_encrypt(&key, &mut b);
        assert_eq!(b, [0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a]);
    }

    #[test_case]
    fn key_wrap_rfc3394() {
        let kek: [u8; 16] = core::array::from_fn(|i| i as u8);
        let data: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        let w = aes_wrap(&kek, &data);
        assert_eq!(&w[..8], &[0x1f, 0xa6, 0x8b, 0x0a, 0x81, 0x12, 0xb4, 0x47]);
    }

    #[test_case]
    fn psk_ieee() {
        // IEEE 802.11i H.4.1: "password" / "IEEE".
        let p = wpa_psk(b"password", b"IEEE");
        assert_eq!(p[..4], [0xf4, 0x2c, 0x6f, 0xc5]);
    }
}
