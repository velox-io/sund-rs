//! Scalar token helpers: string, number, keyword.
//!
//! Each helper folds the streaming `is_final` check internally and returns a
//! small status enum, so the caller dispatches with a single match.

use crate::scanner::{
    advance_chunk, advance_chunk_outlined, clear_lowest_bit, ctz64_empty, AdvanceResult,
};
use crate::types::ScanState;

/// Result of matching a keyword (null / true / false).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KwResult {
    /// Atom matched.
    Ok = 0,
    /// Not enough bytes AND `!state.is_final`.
    Truncated = 1,
    /// Wrong content, or truncated under `is_final`.
    Bad = 2,
}

/// Status returned by `string_span` / `number_span`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SpanStatus {
    /// Closing quote found (string) / end boundary hit (number).
    Ok = 0,
    /// Ran out of data AND `!state.is_final`; caller must SUSPEND.
    Truncated = 1,
    /// `string_span` only: unclosed string under `is_final`.
    Invalid = 2,
}

/// Result of `string_span` / `number_span`. All by-value so the caller can
/// keep `bits` and `chunk_ptr` in registers across the call.
#[derive(Clone, Copy, Debug)]
pub struct SpanResult {
    /// Updated structural bits.
    pub bits: u64,
    /// Updated chunk base.
    pub chunk_ptr: *const u8,
    /// Position after token (NULL on error for string).
    pub end: *const u8,
    pub status: SpanStatus,
    /// Non-zero iff string content contains backslash escapes.
    pub has_escape: bool,
    /// Backslash bitmap for the final chunk (caller updates `bs_bits`).
    pub backslash: u64,
}

/// Compare 4 bytes at `src` against a 4-byte atom string.
/// Returns 0 on match. Compiler folds the constant at compile time.
///
/// # Safety
/// `src` must be readable for 4 bytes.
#[inline(always)]
unsafe fn str4_xor(src: *const u8, atom: &[u8; 4]) -> u32 {
    let sv = core::ptr::read_unaligned(src as *const u32);
    let av = u32::from_ne_bytes(*atom);
    sv ^ av
}

/// Match `null` at `cur_pos`.
///
/// # Safety
/// `cur_pos` and `buf_end` must be valid pointers within the same allocation.
#[inline(always)]
pub unsafe fn match_null(cur_pos: *const u8, buf_end: *const u8, state: &ScanState) -> KwResult {
    if buf_end >= cur_pos.add(4) {
        return if str4_xor(cur_pos, b"null") == 0 {
            KwResult::Ok
        } else {
            KwResult::Bad
        };
    }
    if state.is_final {
        KwResult::Bad
    } else {
        KwResult::Truncated
    }
}

/// Match `true` at `cur_pos`.
///
/// # Safety
/// `cur_pos` and `buf_end` must be valid pointers within the same allocation.
#[inline(always)]
pub unsafe fn match_true(cur_pos: *const u8, buf_end: *const u8, state: &ScanState) -> KwResult {
    if buf_end >= cur_pos.add(4) {
        return if str4_xor(cur_pos, b"true") == 0 {
            KwResult::Ok
        } else {
            KwResult::Bad
        };
    }
    if state.is_final {
        KwResult::Bad
    } else {
        KwResult::Truncated
    }
}

/// Match `false` at `cur_pos`.
///
/// # Safety
/// `cur_pos` and `buf_end` must be valid pointers within the same allocation.
#[inline(always)]
pub unsafe fn match_false(cur_pos: *const u8, buf_end: *const u8, state: &ScanState) -> KwResult {
    if buf_end >= cur_pos.add(5) {
        // Compare "alse" at cur_pos+1 (we already know *cur_pos == 'f').
        return if str4_xor(cur_pos.add(1), b"alse") == 0 {
            KwResult::Ok
        } else {
            KwResult::Bad
        };
    }
    if state.is_final {
        KwResult::Bad
    } else {
        KwResult::Truncated
    }
}

/// Find the closing quote of a JSON string. `bits` and `bs_bits` are passed
/// by value and returned in the result. `bs_bits` is the backslash bitmap for
/// the current chunk; the function ORs backslash bits across chunks into
/// `has_escape`.
///
/// If `advance_chunk` runs out of data:
/// - `!is_final` → `SpanStatus::Truncated` (caller SUSPENDs)
/// - `is_final`  → `SpanStatus::Invalid`  (caller errors out)
///
/// # Safety
/// All pointers must be valid within the same allocation.
#[inline(always)]
pub unsafe fn string_span(
    mut bits: u64,
    mut bs_bits: u64,
    buf_end: *const u8,
    mut chunk_ptr: *const u8,
    state: &mut ScanState,
) -> SpanResult {
    let mut has_escape = false;

    loop {
        let mut idx: u32 = 0;
        if !ctz64_empty(bits, &mut idx) {
            let hit = chunk_ptr.add(idx as usize);
            bits = clear_lowest_bit(bits);
            if *hit == b'"' {
                // Mask off backslashes that fall at or after the closing quote.
                let content_bs = bs_bits & ((1u64 << idx) - 1);
                has_escape |= content_bs != 0;
                return SpanResult {
                    bits,
                    chunk_ptr,
                    end: hit,
                    status: SpanStatus::Ok,
                    has_escape,
                    backslash: bs_bits,
                };
            }
            continue;
        }
        has_escape |= bs_bits != 0;
        let mut new_bs: u64 = 0;
        let ar = advance_chunk_outlined(chunk_ptr, buf_end, state, &mut new_bs);
        if ar.chunk_ptr == chunk_ptr {
            let st = if state.is_final {
                SpanStatus::Invalid
            } else {
                SpanStatus::Truncated
            };
            return SpanResult {
                bits: 0,
                chunk_ptr,
                end: core::ptr::null(),
                status: st,
                has_escape,
                backslash: 0,
            };
        }
        chunk_ptr = ar.chunk_ptr;
        bits = ar.bits;
        bs_bits = new_bs;
    }
}

/// Find the end of a JSON number. Does NOT consume the next structural.
///
/// When the span runs to `buf_end` without hitting a non-number byte:
/// - `!is_final` → `SpanStatus::Truncated` (caller SUSPENDs; more digits may come)
/// - `is_final`  → `SpanStatus::Ok`  (number ends at `buf_end`, commit it)
///
/// # Safety
/// All pointers must be valid within the same allocation.
#[inline(always)]
pub unsafe fn number_span(
    mut bits: u64,
    buf_end: *const u8,
    mut chunk_ptr: *const u8,
    state: &mut ScanState,
) -> SpanResult {
    loop {
        let mut idx: u32 = 0;
        if !ctz64_empty(bits, &mut idx) {
            let end = chunk_ptr.add(idx as usize);
            return SpanResult {
                bits,
                chunk_ptr,
                end,
                status: SpanStatus::Ok,
                has_escape: false,
                backslash: 0,
            };
        }
        let ar: AdvanceResult = advance_chunk(chunk_ptr, buf_end, state);
        if ar.chunk_ptr == chunk_ptr {
            let st = if state.is_final {
                SpanStatus::Ok
            } else {
                SpanStatus::Truncated
            };
            return SpanResult {
                bits: 0,
                chunk_ptr,
                end: buf_end,
                status: st,
                has_escape: false,
                backslash: 0,
            };
        }
        chunk_ptr = ar.chunk_ptr;
        bits = ar.bits;
    }
}
