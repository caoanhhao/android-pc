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
//! chasing its striped ground (Phase L4), which had another cause (16-bit render targets,
//! lib.rs), so this is off unless `debug.nvvk.invariant_position` is 1.

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

/// Debugging (`debug.nvvk.nan_debug`): `code` with the fragment output at location 0 replaced by
/// magenta where it is NaN (or everywhere with `solid`), or None if the module has no such vec4 output.
pub(crate) fn nan_to_magenta(code: &[u32], solid: bool) -> Option<Vec<u32>> {
    const OP_EXT_INST_IMPORT: u32 = 11;
    const OP_TYPE_BOOL: u32 = 20;
    const OP_TYPE_FLOAT: u32 = 22;
    const OP_TYPE_VECTOR: u32 = 23;
    const OP_FUNCTION: u32 = 54;
    const OP_VARIABLE: u32 = 59;
    const OP_STORE: u32 = 62;
    const DECORATION_LOCATION: u32 = 30;
    if code.len() < 5 || code[0] != 0x0723_0203 {
        return None;
    }
    let mut bound = code[3];
    let (mut glsl, mut float, mut vec4, mut bool_ty) = (0, 0, 0, 0);
    let mut loc0 = Vec::new();
    let mut outputs = Vec::new();
    let mut first_function = 0;
    let mut last_store = 0;
    let mut i = 5;
    while i < code.len() {
        let (len, op) = ((code[i] >> 16) as usize, code[i] & 0xffff);
        if len == 0 || i + len > code.len() {
            return None;
        }
        let w = &code[i..i + len];
        match op {
            OP_EXT_INST_IMPORT => {
                let bytes: Vec<u8> = w[2..].iter().flat_map(|x| x.to_le_bytes()).collect();
                if bytes.starts_with(b"GLSL.std.450") {
                    glsl = w[1];
                }
            }
            OP_TYPE_BOOL => bool_ty = w[1],
            OP_TYPE_FLOAT if w[2] == 32 => float = w[1],
            OP_TYPE_VECTOR if w[2] == float && float != 0 && w[3] == 4 => vec4 = w[1],
            OP_DECORATE if len >= 4 && w[2] == DECORATION_LOCATION && w[3] == 0 => loc0.push(w[1]),
            OP_VARIABLE if w[3] == 3 => outputs.push(w[2]), // StorageClass Output
            OP_FUNCTION if first_function == 0 => first_function = i,
            OP_STORE if loc0.contains(&w[1]) && outputs.contains(&w[1]) => last_store = i,
            _ => {}
        }
        i += len;
    }
    if glsl == 0 || float == 0 || vec4 == 0 || first_function == 0 || last_store == 0 {
        return None;
    }
    let mut id = || {
        bound += 1;
        bound - 1
    };
    let mut globals = Vec::new();
    if bool_ty == 0 {
        bool_ty = id();
        globals.extend_from_slice(&[(2 << 16) | OP_TYPE_BOOL, bool_ty]);
    }
    let (f0, f1, zero4, magenta) = (id(), id(), id(), id());
    const OP_CONSTANT: u32 = 43;
    const OP_CONSTANT_COMPOSITE: u32 = 44;
    globals.extend_from_slice(&[(4 << 16) | OP_CONSTANT, float, f0, 0f32.to_bits()]);
    globals.extend_from_slice(&[(4 << 16) | OP_CONSTANT, float, f1, 1f32.to_bits()]);
    globals.extend_from_slice(&[
        (7 << 16) | OP_CONSTANT_COMPOSITE,
        vec4,
        zero4,
        f0,
        f0,
        f0,
        f0,
    ]);
    globals.extend_from_slice(&[
        (7 << 16) | OP_CONSTANT_COMPOSITE,
        vec4,
        magenta,
        f1,
        f0,
        f1,
        f1,
    ]);
    let value = code[last_store + 2];
    let (d, n, t, x0, t4, c) = (id(), id(), id(), id(), id(), id());
    const OP_DOT: u32 = 148;
    const OP_IS_NAN: u32 = 156;
    const OP_SELECT: u32 = 169;
    const OP_COMPOSITE_CONSTRUCT: u32 = 80;
    const OP_EXT_INST: u32 = 12;
    const NMAX: u32 = 80;
    const FMIX: u32 = 46;
    let body = [
        (5 << 16) | OP_DOT,
        float,
        d,
        value,
        value,
        (4 << 16) | OP_IS_NAN,
        bool_ty,
        n,
        d,
        (6 << 16) | OP_SELECT,
        float,
        t,
        n,
        f1,
        f0,
        (7 << 16) | OP_EXT_INST,
        vec4,
        x0,
        glsl,
        NMAX,
        value,
        zero4,
        (7 << 16) | OP_COMPOSITE_CONSTRUCT,
        vec4,
        t4,
        t,
        t,
        t,
        t,
        (8 << 16) | OP_EXT_INST,
        vec4,
        c,
        glsl,
        FMIX,
        x0,
        magenta,
        t4,
    ];
    let mut out = Vec::with_capacity(code.len() + globals.len() + body.len());
    out.extend_from_slice(&code[..first_function]);
    out.extend_from_slice(&globals);
    out.extend_from_slice(&code[first_function..last_store]);
    out.extend_from_slice(&body);
    out.extend_from_slice(&[
        code[last_store],
        code[last_store + 1],
        if solid { magenta } else { c },
    ]);
    out.extend_from_slice(&code[last_store + 3..]);
    out[3] = bound;
    Some(out)
}
