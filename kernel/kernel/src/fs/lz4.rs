//! LZ4 block decompression (the format erofs uses), with partial output:
//! decoding stops once `dst` is full, like LZ4_decompress_safe_partial.

/// Decode `src` into `dst`; returns the number of bytes produced, or
/// `None` for a malformed stream.
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let (mut i, mut o) = (0usize, 0usize);
    let read_len = |i: &mut usize, mut n: usize| -> Option<usize> {
        if n == 15 {
            loop {
                let b = *src.get(*i)?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Some(n)
    };
    while i < src.len() {
        let token = src[i];
        i += 1;
        let lit = read_len(&mut i, (token >> 4) as usize)?;
        let end = i.checked_add(lit)?;
        if end > src.len() {
            return None;
        }
        let n = lit.min(dst.len() - o);
        dst[o..o + n].copy_from_slice(&src[i..i + n]);
        o += n;
        i = end;
        if o == dst.len() || i == src.len() {
            return Some(o);
        }
        let off = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        if off == 0 || off > o {
            return None;
        }
        let mlen = read_len(&mut i, (token & 15) as usize)? + 4;
        let n = mlen.min(dst.len() - o);
        for k in 0..n {
            dst[o + k] = dst[o + k - off];
        }
        o += n;
        if o == dst.len() {
            return Some(o);
        }
    }
    Some(o)
}

#[test_case]
fn lz4_literals_and_overlapping_matches() {
    // "abcabcabcabc..." : 3 literals, then a match at offset 3 of 12 bytes,
    // then 5 trailing literals.
    let src = [0x38, b'a', b'b', b'c', 3, 0, 0x50, b'x', b'y', b'z', b'1', b'2'];
    let mut out = [0u8; 32];
    let n = decompress(&src, &mut out).unwrap();
    assert_eq!(&out[..n], b"abcabcabcabcabcxyz12");
    // Partial output stops in the middle of the match.
    let mut small = [0u8; 7];
    assert_eq!(decompress(&src, &mut small), Some(7));
    assert_eq!(&small, b"abcabca");
    // Offset past the output start is rejected.
    assert_eq!(decompress(&[0x10, b'a', 9, 0], &mut out), None);
}
