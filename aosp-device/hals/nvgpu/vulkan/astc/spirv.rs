// Copyright 2026 The Android PC Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Vertex shader positions made invariant (SPIR-V `Invariant` on `BuiltIn Position`).
//!
//! Mobile games draw a depth pre-pass and then the same geometry again with
//! `depthCompareOp = EQUAL`, each pass with its own vertex shader. Without `invariant
//! gl_Position` nothing makes two shaders compute the same position bit for bit, and NVIDIA's
//! compiler (FMA contraction, instruction order) need not; mobile compilers happen to. Where
//! Winds Meet draws its TAA motion vectors that way (3 EQUAL pipelines). An experiment while
//! chasing its striped ponds (Phase L4): the stripes stayed (they are the rain ripples of its
//! water shader), so this is off unless `debug.nvvk.invariant_position` is 1.

const OP_ENTRY_POINT: u32 = 15;
const OP_DECORATE: u32 = 71;
const OP_MEMBER_DECORATE: u32 = 72;
const DECORATION_BUILT_IN: u32 = 11;
const DECORATION_INVARIANT: u32 = 18;
const BUILT_IN_POSITION: u32 = 0;
const EXECUTION_MODEL_VERTEX: u32 = 0;

/// `code` with `Invariant` added to every `BuiltIn Position` decoration, or None if it is not
/// a vertex-only module, has no position or already has it invariant.
pub(crate) fn invariant_position(code: &[u32]) -> Option<Vec<u32>> {
    if code.len() < 5 || code[0] != 0x0723_0203 {
        return None;
    }
    // (instruction index, its words, the instruction to insert after it)
    let mut inserts: Vec<(usize, [u32; 4], usize)> = Vec::new();
    let mut vertex_only = true;
    let mut has_entry = false;
    let mut invariant: Vec<(u32, Option<u32>)> = Vec::new();
    let mut i = 5;
    while i < code.len() {
        let (len, op) = ((code[i] >> 16) as usize, code[i] & 0xffff);
        if len == 0 || i + len > code.len() {
            return None;
        }
        let w = &code[i..i + len];
        match op {
            OP_ENTRY_POINT => {
                has_entry = true;
                vertex_only &= w[1] == EXECUTION_MODEL_VERTEX;
            }
            OP_DECORATE if len >= 4 && w[2] == DECORATION_BUILT_IN && w[3] == BUILT_IN_POSITION => {
                inserts.push((
                    i + len,
                    [(3 << 16) | OP_DECORATE, w[1], DECORATION_INVARIANT, 0],
                    3,
                ));
            }
            OP_MEMBER_DECORATE
                if len >= 5 && w[3] == DECORATION_BUILT_IN && w[4] == BUILT_IN_POSITION =>
            {
                inserts.push((
                    i + len,
                    [
                        (4 << 16) | OP_MEMBER_DECORATE,
                        w[1],
                        w[2],
                        DECORATION_INVARIANT,
                    ],
                    4,
                ));
            }
            OP_DECORATE if len >= 3 && w[2] == DECORATION_INVARIANT => invariant.push((w[1], None)),
            OP_MEMBER_DECORATE if len >= 4 && w[3] == DECORATION_INVARIANT => {
                invariant.push((w[1], Some(w[2])))
            }
            _ => {}
        }
        i += len;
    }
    // already invariant: leave those alone
    inserts.retain(|(_, w, _)| {
        let key = if w[0] & 0xffff == OP_DECORATE {
            (w[1], None)
        } else {
            (w[1], Some(w[2]))
        };
        !invariant.contains(&key)
    });
    if !has_entry || !vertex_only || inserts.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(code.len() + inserts.len() * 4);
    let mut from = 0;
    for (at, words, n) in &inserts {
        out.extend_from_slice(&code[from..*at]);
        out.extend_from_slice(&words[..*n]);
        from = *at;
    }
    out.extend_from_slice(&code[from..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_member_invariant() {
        let code = [
            0x0723_0203,
            0x0001_0000,
            0,
            10,
            0,
            (4 << 16) | OP_ENTRY_POINT,
            EXECUTION_MODEL_VERTEX,
            1,
            0x6e69_616d, // "main" (truncated, unused here)
            (5 << 16) | OP_MEMBER_DECORATE,
            7,
            0,
            DECORATION_BUILT_IN,
            BUILT_IN_POSITION,
        ];
        let out = invariant_position(&code).unwrap();
        assert_eq!(
            &out[14..],
            &[(4 << 16) | OP_MEMBER_DECORATE, 7, 0, DECORATION_INVARIANT]
        );
        assert!(invariant_position(&out).is_none());
    }
}
