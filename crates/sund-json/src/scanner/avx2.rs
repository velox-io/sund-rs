//! AVX2 implementation of chunk classification for x86_64.
//!
//! Uses a shuffle-LUT approach for byte classification.
//! Also provides `prefix_xor_x86` via PCLMULQDQ (carry-less multiply).
//!
//! Inline strategy (via `cfg_attr`):
//! - When the compiler globally has AVX2 (`-C target-cpu=native` or
//!   `-C target-feature=+avx2`): `#[inline(always)]`, zero call overhead.
//! - Otherwise: `#[target_feature(enable = ...)]` so the function body
//!   can use AVX2 intrinsics, but pays a real `call` per invocation
//!   (Rust forbids combining `target_feature` with `inline(always)`).

#![allow(clippy::undocumented_unsafe_blocks)]

use super::ChunkClass;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Compute prefix-XOR using `_mm_clmulepi64_si128` (PCLMULQDQ).
///
/// `clmul(v, ~0)` performs a carry-less multiply, which computes the
/// prefix-XOR when one operand is all-ones.
///
/// # Safety
///
/// Requires x86-64 with SSE2 and PCLMULQDQ support.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(target_feature = "avx2", inline(always))]
#[cfg_attr(not(target_feature = "avx2"), target_feature(enable = "sse2,pclmulqdq"))]
pub(crate) unsafe fn prefix_xor_x86(v: u64) -> u64 {
    let x = _mm_set_epi64x(0, v as i64);
    let ones = _mm_set_epi64x(0, -1i64);
    let r = _mm_clmulepi64_si128(x, ones, 0);
    _mm_cvtsi128_si64(r) as u64
}

/// Classify 64 input bytes using AVX2 shuffle-LUT approach.
///
/// # Safety
///
/// `buf` must point to at least 64 readable bytes.
/// Requires x86-64 with AVX2 support.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(target_feature = "avx2", inline(always))]
#[cfg_attr(not(target_feature = "avx2"), target_feature(enable = "avx2"))]
pub(crate) unsafe fn classify_chunk(buf: *const u8) -> ChunkClass {
    let v0 = _mm256_loadu_si256(buf as *const __m256i);
    let v1 = _mm256_loadu_si256(buf.add(32) as *const __m256i);

    // Backslash (0x5C)
    let bs_cmp = _mm256_set1_epi8(0x5Cu8 as i8);
    let bs0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, bs_cmp)) as u32;
    let bs1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, bs_cmp)) as u32;
    let backslash = (bs0 as u64) | ((bs1 as u64) << 32);

    // Quote (0x22)
    let qt_cmp = _mm256_set1_epi8(0x22u8 as i8);
    let qt0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, qt_cmp)) as u32;
    let qt1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, qt_cmp)) as u32;
    let raw_quote = (qt0 as u64) | ((qt1 as u64) << 32);

    // Low nibbles (shared for whitespace and operator LUTs)
    let low_mask = _mm256_set1_epi8(0x0F);
    let lo0 = _mm256_and_si256(v0, low_mask);
    let lo1 = _mm256_and_si256(v1, low_mask);

    // Whitespace
    // ws_lut: maps low nibble → the whitespace char with that nibble.
    // Compare the shuffled result with the original byte; match only if
    // both nibble and high byte agree (no false positives for non-ws).
    let ws_lut = _mm256_setr_epi8(
        0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0x09, 0x0A, 0, 0, 0x0D, 0, 0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0,
        0x09, 0x0A, 0, 0, 0x0D, 0, 0,
    );
    let ws0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_shuffle_epi8(ws_lut, lo0), v0)) as u32;
    let ws1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_shuffle_epi8(ws_lut, lo1), v1)) as u32;
    let whitespace = (ws0 as u64) | ((ws1 as u64) << 32);

    // Operators: ',', ':', '[', ']', '{', '}'
    // Three LUTs, each mapping low nibble → the target char. OR the three
    // match masks to cover all six operators.
    //   op_lut1: ',' (0x2C) at index 12, ':' (0x3A) at index 10
    //   op_lut2: '[' (0x5B) at index 11, ']' (0x5D) at index 13
    //   op_lut3: '{' (0x7B) at index 11, '}' (0x7D) at index 13
    let op_lut1 = _mm256_setr_epi8(
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3A, 0, 0x2C, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3A,
        0, 0x2C, 0, 0, 0,
    );
    let op_lut2 = _mm256_setr_epi8(
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x5B, 0, 0x5D, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0x5B, 0, 0x5D, 0, 0,
    );
    let op_lut3 = _mm256_setr_epi8(
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x7B, 0, 0x7D, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0x7B, 0, 0x7D, 0, 0,
    );

    let m1_0 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut1, lo0), v0);
    let m2_0 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut2, lo0), v0);
    let m3_0 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut3, lo0), v0);
    let op0 = _mm256_movemask_epi8(_mm256_or_si256(_mm256_or_si256(m1_0, m2_0), m3_0)) as u32;

    let m1_1 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut1, lo1), v1);
    let m2_1 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut2, lo1), v1);
    let m3_1 = _mm256_cmpeq_epi8(_mm256_shuffle_epi8(op_lut3, lo1), v1);
    let op1 = _mm256_movemask_epi8(_mm256_or_si256(_mm256_or_si256(m1_1, m2_1), m3_1)) as u32;

    let op = (op0 as u64) | ((op1 as u64) << 32);

    ChunkClass {
        backslash,
        raw_quote,
        whitespace,
        op,
    }
}
