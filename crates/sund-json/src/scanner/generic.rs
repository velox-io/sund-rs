//! Scalar fallback for chunk classification and prefix-XOR.
//!
//! Used on architectures without NEON or AVX2 SIMD support.
//! Uses SWAR (SIMD-Within-A-Register) tricks to process 8 bytes at a
//! time with plain u64 arithmetic — ~8× fewer iterations than the
//! naive byte-by-byte loop.

#![allow(dead_code)] // Only used on non-SIMD targets.

use super::ChunkClass;

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

// ── SWAR helpers ────────────────────────────────────────────────────

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// SWAR zero-byte detection: for each byte in `w` that is 0x00,
/// the corresponding high bit (0x80) is set in the result.
/// Classic Mycroft's trick: `(w - 0x01..01) & !w & 0x80..80`.
#[inline(always)]
const fn has_zero(w: u64) -> u64 {
    (w.wrapping_sub(LO)) & !w & HI
}

/// Detect bytes equal to `c` in a u64 word.
/// Returns a mask with the high bit set for each matching byte.
#[inline(always)]
const fn bytes_eq(w: u64, c: u8) -> u64 {
    has_zero(w ^ (LO.wrapping_mul(c as u64)))
}

/// Collapse the high-bit-per-byte mask into one bit per byte in the
/// low 8 bits of the result (bit i corresponds to byte i of the word).
///
/// `hi_mask` has bit 7 (0x80) set for each matching byte. The multiply
/// by `0x0002_0408_1020_4081` gathers bit 7 of byte *i* into bit
/// (56 + i) of the product; the final `>> 56` extracts bits 0..7.
#[inline(always)]
const fn pack8(hi_mask: u64) -> u64 {
    hi_mask.wrapping_mul(0x0002_0408_1020_4081) >> 56
}

/// Read a little-endian u64 from a raw pointer.
#[inline(always)]
unsafe fn read_u64_le(ptr: *const u8) -> u64 {
    let mut v = 0u64;
    core::ptr::copy_nonoverlapping(ptr, &mut v as *mut u64 as *mut u8, 8);
    u64::from_le(v)
}

// ── classify_chunk (SWAR) ───────────────────────────────────────────

/// Classify 64 input bytes into four bitmaps (SWAR fallback).
///
/// Processes 8 bytes at a time using integer arithmetic. Each iteration
/// detects backslash, quote, whitespace, and operator bytes within a
/// u64 word, then packs the results into the corresponding bitmap.
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

    for i in 0..8u32 {
        let w = read_u64_le(buf.add((i * 8) as usize));
        let shift = i * 8;

        // Single-character matches: XOR with broadcast, detect zeros.
        backslash |= pack8(bytes_eq(w, 0x5C)) << shift; // '\\'
        raw_quote |= pack8(bytes_eq(w, 0x22)) << shift; // '"'

        // Whitespace: 0x09 (tab), 0x0A (LF), 0x0D (CR), 0x20 (space)
        // Pack each match separately to avoid has_zero borrow interference.
        let ws = pack8(bytes_eq(w, 0x20))
            | pack8(bytes_eq(w, 0x09))
            | pack8(bytes_eq(w, 0x0A))
            | pack8(bytes_eq(w, 0x0D));
        whitespace |= ws << shift;

        // Operators: , : [ ] { }
        let ops = pack8(bytes_eq(w, b','))
            | pack8(bytes_eq(w, b':'))
            | pack8(bytes_eq(w, b'['))
            | pack8(bytes_eq(w, b']'))
            | pack8(bytes_eq(w, b'{'))
            | pack8(bytes_eq(w, b'}'));
        op |= ops << shift;
    }

    ChunkClass {
        backslash,
        raw_quote,
        whitespace,
        op,
    }
}
