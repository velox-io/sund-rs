//! SIMD structural bitmap scanner.
//!
//! Produces 64-bit structural bitmaps from 64-byte input chunks.
//! Algorithm: classify → escape resolution → string mask → merge.
//!
//! Platform selection uses `cfg(target_feature)` at the module level so
//! that platform-specific files contain **no** per-function
//! `#[target_feature]` annotations — every function can be
//! `#[inline(always)]` without conflict.
//!
//! - aarch64:                NEON + PMULL  (neon.rs)
//! - x86-64 with avx2 flag: AVX2 + PCLMULQDQ (avx2.rs)
//! - everything else:       scalar fallback  (generic.rs)
//!
//! To enable the AVX2 path, compile with `-C target-feature=+avx2,+pclmulqdq`
//! or `-C target-cpu=native`.

use crate::types::ScanState;

#[cfg(target_feature = "avx2")]
mod avx2;
mod generic;
#[cfg(target_arch = "aarch64")]
mod neon;

/// Per-chunk classification bitmaps (one bit per input byte, 64 bits = 64 bytes).
#[derive(Clone, Copy, Debug)]
pub struct ChunkClass {
    pub backslash: u64,
    pub raw_quote: u64,
    pub whitespace: u64,
    pub op: u64,
}

/// Result of scanning a single 64-byte chunk.
#[derive(Clone, Copy, Debug)]
pub struct ChunkResult {
    pub structural: u64,
    pub backslash: u64,
}

/// Result of escape resolution.
#[derive(Clone, Copy, Debug)]
pub struct EscapeResult {
    pub escaped: u64,
}

/// Result of advancing to the next chunk.
///
/// All by-value; on AArch64 the struct returns in x0/x1 so the caller never
/// spills `chunk_ptr` across the call.  On failure (insufficient data and
/// `!is_final`), `chunk_ptr` is returned unchanged; caller detects EOF via
/// `result.chunk_ptr == old_chunk_ptr`.
#[derive(Clone, Copy, Debug)]
pub struct AdvanceResult {
    pub chunk_ptr: *const u8,
    pub bits: u64,
    pub backslash: u64,
}

/// Compact result for advance_chunk_outlined: only 2 fields = 16 bytes = x0/x1.
/// Backslash is written via the out-pointer parameter.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct AdvanceResult2 {
    pub chunk_ptr: *const u8,
    pub bits: u64,
}

/// Clear the lowest set bit (corresponds to x86 BLSR).
#[inline(always)]
pub fn clear_lowest_bit(v: u64) -> u64 {
    v & v.wrapping_sub(1)
}

/// Branchless probe: writes the trailing-zero count to `*out_idx` and returns
/// `true` if `v` was zero (empty bitmap).
///
/// On x86-64 with BMI1, `tzcnt` sets CF=1 exactly when the source is 0,
/// giving one fewer instruction than `test + tzcnt`. The Rust
/// `trailing_zeros()` intrinsic compiles to `tzcnt` on modern targets
/// and returns 64 for zero input.
///
/// We tried three routes to recover the C reference's `tzcnt + jc` form
/// (~12 ns/iter on the 1332-byte payload, theoretically):
///
/// 1. Stable `asm!("tzcnt; setc", ...)` returning a `bool` — opaque
///    to LLVM, defeats CSE/DCE, static tzcnt count grew 30 → 75, base
///    regressed 476 → 516 ns.
/// 2. Nightly `asm_goto_with_outputs` jumping to a `return true`
///    block — emits the right `tzcnt + jb`, but LLVM lowers the
///    `callbr` IR pessimistically: it spills 4–5 callee-save
///    registers into the stack frame *before each* tzcnt site to
///    pin live-out values across the unknown branch. Static rsp
///    movs grew ~500 → 661, base regressed 476 → 553 ns.
/// 3. `naked_asm!`/extern "C" wrapper — eliminates LLVM optimiser
///    pessimisation around the asm, but turns the inlined helper into
///    an ABI call. Returning a Rust `bool` through `al` forces the
///    caller into `call + test al,al + jne` (3 insns), which is
///    *worse* than the current portable form's `test + je + tzcnt`
///    (also 3 insns) because it adds a real call/ret. CF cannot be
///    propagated across a function boundary in System V x86-64 ABI.
///
/// **The optimisation is structurally unreachable from Rust** until
/// LLVM grows `"=@ccc"`-style flag outputs for `asm!` (issue
/// rust-lang/rust#101019 family). For reference, clang has the same
/// limitation: written in C without the GCC `__asm__("=@ccc")`
/// extension, the equivalent code generates the identical 3-instruction
/// `test; je; tzcnt` sequence — ndec is fast specifically because it
/// hand-writes inline asm, not because C compilers fold the pattern
/// automatically.
#[inline(always)]
pub fn ctz64_empty(v: u64, out_idx: &mut u32) -> bool {
    *out_idx = v.trailing_zeros();
    v == 0
}

/// Compute the prefix-XOR of a 64-bit value.
///
/// Dispatches to the best available implementation:
/// - aarch64: PMULL (polynomial multiply)
/// - x86-64:  PCLMULQDQ (carry-less multiply)
/// - fallback: shift-XOR cascade
#[inline(always)]
pub fn prefix_xor(v: u64) -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: On aarch64 we require NEON+AES (PMULL). The caller must
        // ensure the target supports these features (always true on Apple
        // Silicon; compile with +aes on other aarch64 targets).
        unsafe { neon::prefix_xor_neon(v) }
    }
    #[cfg(target_feature = "avx2")]
    {
        // SAFETY: PCLMULQDQ + SSE2 required. Compile with
        // -C target-feature=+avx2,+pclmulqdq or -C target-cpu=native.
        unsafe { avx2::prefix_xor_x86(v) }
    }
    #[cfg(not(any(target_arch = "aarch64", target_feature = "avx2")))]
    {
        generic::prefix_xor_generic(v)
    }
}

/// Classify 64 input bytes into four bitmaps: backslash, raw_quote,
/// whitespace, and structural operators.
///
/// # Safety
///
/// `buf` must point to at least 64 readable bytes.
#[inline(always)]
pub unsafe fn classify_chunk(buf: *const u8) -> ChunkClass {
    #[cfg(target_arch = "aarch64")]
    {
        neon::classify_chunk(buf)
    }
    #[cfg(target_feature = "avx2")]
    {
        avx2::classify_chunk(buf)
    }
    #[cfg(not(any(target_arch = "aarch64", target_feature = "avx2")))]
    {
        generic::classify_chunk(buf)
    }
}

/// Escape resolution using simdjson's ODD_BITS subtraction algorithm.
///
/// Given a backslash bitmap and cross-chunk carry (`state.prev_escape`),
/// computes which positions are escaped (the byte AFTER an odd-length
/// backslash run).
///
/// The key insight: shift backslashes left by 1 to get "maybe escaped",
/// OR with ODD_BITS (0xAA..AA), subtract backslashes to propagate through
/// runs, XOR with ODD_BITS to correct odd-aligned runs.
#[inline(always)]
pub fn compute_escaped(backslash: u64, state: &mut ScanState) -> EscapeResult {
    const ODD_BITS: u64 = 0xAAAA_AAAA_AAAA_AAAA;

    // Strip the first backslash if it was escaped by the previous chunk.
    let potential_escape = backslash & !state.prev_escape;

    // Core simdjson algorithm:
    // 1. Shift left to get "maybe escaped" positions
    // 2. OR with ODD_BITS to seed odd-bit positions
    // 3. Subtract potential_escape to propagate through runs
    // 4. XOR with ODD_BITS to correct odd-aligned runs
    let maybe_escaped = potential_escape << 1;
    let maybe_escaped_and_odd_bits = maybe_escaped | ODD_BITS;
    let even_series_codes_and_odd_bits = maybe_escaped_and_odd_bits.wrapping_sub(potential_escape);
    let escape_and_terminal_code = even_series_codes_and_odd_bits ^ ODD_BITS;

    // escaped = positions that are escaped by a real backslash.
    let escaped = escape_and_terminal_code ^ (backslash | state.prev_escape);

    // Cross-chunk carry: if the last backslash is a real escape (odd-run),
    // the first byte of the next chunk is escaped.
    let escape = escape_and_terminal_code & backslash;
    state.prev_escape = escape >> 63;

    EscapeResult { escaped }
}

/// Scan a single 64-byte chunk: classify bytes, resolve escapes, compute
/// the in-string mask, and merge into a structural bitmap.
///
/// # Safety
///
/// `buf` must point to at least 64 readable bytes.
#[inline(always)]
pub unsafe fn scan_chunk(buf: *const u8, state: &mut ScanState) -> ChunkResult {
    let cls = classify_chunk(buf);

    // Fast path: most chunks have no backslashes.
    //
    // Cross-chunk escape carry: when the previous chunk ended with a
    // live backslash (prev_escape == 1), bit 0 of the current chunk's
    // quote bitmap is an escaped character (e.g. `\"` straddling the
    // boundary), not a real quote.  On the fast path (no backslashes),
    // `compute_escaped` is skipped so we must mask the carry manually.
    // On the slow path, `compute_escaped` already folds `prev_escape`
    // into `escaped` via `escaped = ... ^ (backslash | prev_escape)`.
    let real_quotes = if cls.backslash == 0 {
        let rq = cls.raw_quote;
        if state.prev_escape != 0 {
            state.prev_escape = 0;
            rq & !1u64
        } else {
            rq
        }
    } else {
        let esc = compute_escaped(cls.backslash, state);
        cls.raw_quote & !esc.escaped
    };

    let in_string = prefix_xor(real_quotes) ^ state.prev_in_string;
    // Arithmetic right shift: propagates the MSB → 0 or ~0.
    state.prev_in_string = ((in_string as i64) >> 63) as u64;

    let op = cls.op & !in_string;

    // Scalar start: follows structural/whitespace, is not itself
    // structural/ws/quote/in-string.
    let s = op | real_quotes;
    let follows = ((s | cls.whitespace) << 1) | state.prev_structural_or_ws;
    let scalar_start = follows & !cls.whitespace & !in_string & !s;

    state.prev_structural_or_ws = (s | cls.whitespace) >> 63;

    let structural = op | real_quotes | scalar_start;

    ChunkResult {
        structural,
        backslash: cls.backslash,
    }
}

/// Cold path: `remaining < 64` and `is_final`.  Pads the tail chunk in a
/// scratch buffer and scans it.  Split out so the hot path doesn't pay for
/// the stack array.
///
/// # Safety
///
/// * `next` must be readable for `remaining` bytes (`remaining > 0`).
/// * `remaining` must be in `1..64`.
#[inline(never)]
pub unsafe fn advance_chunk_tail(
    next: *const u8,
    remaining: isize,
    state: &mut ScanState,
) -> AdvanceResult {
    // Fill with spaces (0x20), then overwrite the first `remaining` bytes.
    let mut padded = [0x20u8; 64];
    core::ptr::copy_nonoverlapping(next, padded.as_mut_ptr(), remaining as usize);

    let r = scan_chunk(padded.as_ptr(), state);

    let mask = (1u64 << (remaining as u32)) - 1;
    AdvanceResult {
        chunk_ptr: next,
        bits: r.structural & mask,
        backslash: r.backslash & mask,
    }
}

/// Scan the next 64-byte chunk (`chunk_ptr + 64`).
///
/// When `!is_final` and remaining < 64, returns the input `chunk_ptr`
/// unchanged with `bits = 0`, signalling resume-needed.  `is_final` is
/// read from `state.is_final`.
///
/// # Safety
///
/// * `chunk_ptr` and `buf_end` must be valid pointers within (or one-past)
///   the same allocation.
/// * `chunk_ptr + 64 <= buf_end` when there is a full chunk available.
#[inline(always)]
pub unsafe fn advance_chunk(
    chunk_ptr: *const u8,
    buf_end: *const u8,
    state: &mut ScanState,
) -> AdvanceResult {
    let next = chunk_ptr.add(64);
    let remaining = buf_end.offset_from(next);

    if remaining >= 64 {
        let r = scan_chunk(next, state);
        return AdvanceResult {
            chunk_ptr: next,
            bits: r.structural,
            backslash: r.backslash,
        };
    }

    if remaining <= 0 || !state.is_final {
        return AdvanceResult {
            chunk_ptr,
            bits: 0,
            backslash: 0,
        };
    }

    advance_chunk_tail(next, remaining, state)
}

/// Outlined (noinline) variant of `advance_chunk` for use in string_span
/// and number_span. Returns a compact 2-field struct (fits in x0/x1 on
/// aarch64) and writes backslash to the caller-provided pointer.
///
/// # Safety
///
/// Same as `advance_chunk`. `bs_out` must be a valid writable pointer.
#[inline(never)]
pub unsafe fn advance_chunk_outlined(
    chunk_ptr: *const u8,
    buf_end: *const u8,
    state: &mut ScanState,
    bs_out: *mut u64,
) -> AdvanceResult2 {
    let next = chunk_ptr.add(64);
    let remaining = buf_end.offset_from(next);

    if remaining >= 64 {
        let r = scan_chunk(next, state);
        *bs_out = r.backslash;
        return AdvanceResult2 {
            chunk_ptr: next,
            bits: r.structural,
        };
    }

    if remaining <= 0 || !state.is_final {
        *bs_out = 0;
        return AdvanceResult2 { chunk_ptr, bits: 0 };
    }

    let ar = advance_chunk_tail(next, remaining, state);
    *bs_out = ar.backslash;
    AdvanceResult2 {
        chunk_ptr: ar.chunk_ptr,
        bits: ar.bits,
    }
}
