//! Scalar fallback for chunk classification and prefix-XOR.
//!
//! Used on architectures without NEON or AVX2 SIMD support.
//! Correct but slow — byte-by-byte classification into the 4 bitmaps.

#![allow(dead_code)] // Only used on non-SIMD targets.

use super::ChunkClass;

// ---------------------------------------------------------------------------
// prefix_xor: shift-XOR cascade (generic fallback)
// ---------------------------------------------------------------------------

/// Compute prefix-XOR via the shift-XOR cascade.
///
/// bit i of result = XOR of bits 0..i of input.
/// Six cascading shifts cover all 64 bits.
#[inline(always)]
pub(crate) fn prefix_xor_generic(mut v: u64) -> u64 {
    v ^= v << 1;
    v ^= v << 2;
    v ^= v << 4;
    v ^= v << 8;
    v ^= v << 16;
    v ^= v << 32;
    v
}

// ---------------------------------------------------------------------------
// classify_chunk: scalar byte-by-byte fallback
// ---------------------------------------------------------------------------

/// Classify a single byte: returns `(is_backslash, is_quote, is_whitespace, is_op)`.
#[inline(always)]
fn classify_byte(b: u8) -> (bool, bool, bool, bool) {
    let is_backslash = b == 0x5C; // '\\'
    let is_quote = b == 0x22; // '"'
    let is_whitespace = b == b' ' || b == b'\t' || b == b'\n' || b == b'\r';
    let is_op = b == b',' || b == b':' || b == b'[' || b == b']' || b == b'{' || b == b'}';
    (is_backslash, is_quote, is_whitespace, is_op)
}

/// Classify 64 input bytes into four bitmaps (scalar fallback).
///
/// # Safety
///
/// `buf` must point to at least 64 readable bytes.
#[inline(always)]
pub(crate) unsafe fn classify_chunk(buf: *const u8) -> ChunkClass {
    let mut backslash: u64 = 0;
    let mut raw_quote: u64 = 0;
    let mut whitespace: u64 = 0;
    let mut op: u64 = 0;

    for i in 0..64u32 {
        let b = *buf.add(i as usize);
        let (is_bs, is_qt, is_ws, is_op) = classify_byte(b);
        let bit = 1u64 << i;
        if is_bs {
            backslash |= bit;
        }
        if is_qt {
            raw_quote |= bit;
        }
        if is_ws {
            whitespace |= bit;
        }
        if is_op {
            op |= bit;
        }
    }

    ChunkClass {
        backslash,
        raw_quote,
        whitespace,
        op,
    }
}
