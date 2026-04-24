//! Computed-goto JSON parser state machine.
//!
//! Ported from `ndec/impl/ndec.h`.
//!
//! On aarch64, the dispatch uses a real computed-goto via `asm_goto`: a
//! 4-instruction jump table (adr + ldrsw + add + br), matching the C
//! version exactly. On other architectures, falls back to a dense 9-arm
//! `match` with `unreachable_unchecked()`.

use crate::reactor::Reactor;
use crate::scalar::{
    match_false, match_null, match_true, number_span, string_span, KwResult, SpanStatus,
};
use crate::scanner::{advance_chunk, clear_lowest_bit, ctz64_empty, scan_chunk, ChunkResult};
use crate::types::*;

/// Sentinel for end-of-input within the structural bitmap scan.
const EOF: i32 = -1;

// Phase integer constants — match Phase enum discriminants.
const PH_ROOT_VALUE: u32 = 0;
const PH_OBJ_FIELD_OR_END: u32 = 1;
const PH_OBJ_FIELD_VALUE: u32 = 2;
const PH_OBJ_CONTINUE: u32 = 3;
const PH_ARR_ELEM_OR_END: u32 = 4;
const PH_ARR_ELEM_VALUE: u32 = 5;
const PH_ARR_CONTINUE: u32 = 6;
const PH_ROOT_DONE: u32 = 7;
const PH_SKIP_VALUE: u32 = 8;

/// Parse JSON from `ctx`, dispatching events to `reactor`.
///
/// # Safety
///
/// The pointers stored in `ctx` (`buf`, `buf_end`, `cur_pos`, `chunk_ptr`)
/// must be valid for reads within the input buffer.
pub unsafe fn parse<R: Reactor>(ctx: &mut Ctx, reactor: &mut R) {
    let buf = ctx.buf;
    let buf_end = ctx.buf_end;
    let mut cur_pos = ctx.cur_pos.sub(1);
    let mut chunk_ptr = ctx.chunk_ptr;
    let mut bits = ctx.structural_bits;
    let mut bs_bits: u64 = 0;
    let mut scan_state = ctx.scan_state;
    let mut depth = ctx.depth;
    let frames = ctx.frames.as_mut_ptr();

    // ------------------------------------------------------------------
    // Inline macro equivalents
    // ------------------------------------------------------------------

    macro_rules! cur_offset {
        () => {
            (cur_pos as usize - buf as usize) as u32
        };
    }
    macro_rules! save_and_return {
        ($code:expr) => {{
            ctx.cur_pos = cur_pos.add(1);
            ctx.chunk_ptr = chunk_ptr;
            ctx.structural_bits = bits;
            ctx.scan_state = scan_state;
            ctx.depth = depth;
            ctx.exit_code = $code;
            return;
        }};
    }
    macro_rules! top_frame {
        () => {
            &mut *frames.add(depth as usize - 1)
        };
    }
    macro_rules! stack_push {
        ($child_phase:expr) => {{
            if depth >= MAX_DEPTH as u32 {
                ctx.error_pos = cur_offset!();
                ctx.cur_pos = cur_pos;
                ctx.chunk_ptr = chunk_ptr;
                ctx.structural_bits = bits;
                ctx.scan_state = scan_state;
                ctx.depth = depth;
                ctx.exit_code = ExitCode::ErrDepth as i32;
                return;
            }
            (*frames.add(depth as usize)).phase = $child_phase;
            (*frames.add(depth as usize)).data = 0;
            depth += 1;
        }};
    }
    macro_rules! stack_pop {
        () => {
            depth -= 1;
        };
    }
    macro_rules! error_exit {
        ($code:expr, $pos:expr) => {{
            ctx.error_pos = $pos;
            ctx.cur_pos = cur_pos;
            ctx.chunk_ptr = chunk_ptr;
            ctx.structural_bits = bits;
            ctx.scan_state = scan_state;
            ctx.depth = depth;
            ctx.exit_code = $code;
            return;
        }};
    }
    macro_rules! yield_or_error {
        ($directive:expr, $resume_phase:expr) => {{
            let d = $directive;
            if d == YIELD {
                (*frames.add(depth as usize - 1)).phase = $resume_phase;
                if bits == 0 {
                    let effective = if cur_pos < buf_end { cur_pos } else { buf_end };
                    if effective > chunk_ptr {
                        chunk_ptr = effective;
                    }
                }
                ctx.cur_pos = cur_pos;
                ctx.chunk_ptr = chunk_ptr;
                ctx.structural_bits = bits;
                ctx.scan_state = scan_state;
                ctx.depth = depth;
                ctx.exit_code = ExitCode::Suspend as i32;
                return;
            }
            error_exit!(d, cur_offset!());
        }};
    }
    macro_rules! suspend_next {
        ($phase_val:expr) => {{
            cur_pos = cur_pos.add(1);
            if depth > 0 {
                (*frames.add(depth as usize - 1)).phase = $phase_val;
            }
            ctx.cur_pos = cur_pos;
            ctx.chunk_ptr = chunk_ptr;
            ctx.structural_bits = bits;
            ctx.scan_state = scan_state;
            ctx.depth = depth;
            ctx.exit_code = ExitCode::Suspend as i32;
            return;
        }};
    }
    macro_rules! suspend_here {
        ($phase_val:expr) => {{
            if depth > 0 {
                (*frames.add(depth as usize - 1)).phase = $phase_val;
            }
            ctx.cur_pos = cur_pos;
            ctx.chunk_ptr = chunk_ptr;
            ctx.structural_bits = bits;
            ctx.scan_state = scan_state;
            ctx.depth = depth;
            ctx.exit_code = ExitCode::Suspend as i32;
            return;
        }};
    }
    macro_rules! suspend_at {
        ($phase_val:expr, $ptr:expr) => {{
            cur_pos = $ptr;
            if depth > 0 {
                (*frames.add(depth as usize - 1)).phase = $phase_val;
            }
            ctx.cur_pos = cur_pos;
            ctx.chunk_ptr = chunk_ptr;
            ctx.structural_bits = bits;
            ctx.scan_state = scan_state;
            ctx.depth = depth;
            ctx.exit_code = ExitCode::Suspend as i32;
            return;
        }};
    }
    macro_rules! next_structural {
        () => {{
            loop {
                let mut idx: u32 = 0;
                if !ctz64_empty(bits, &mut idx) {
                    cur_pos = chunk_ptr.add(idx as usize);
                    bits = clear_lowest_bit(bits);
                    break *cur_pos as i32;
                }
                let ar = advance_chunk(chunk_ptr, buf_end, &mut scan_state);
                if ar.chunk_ptr == chunk_ptr {
                    break EOF;
                }
                chunk_ptr = ar.chunk_ptr;
                bits = ar.bits;
                // Keep bs_bits in sync with current chunk. The value is
                // consumed by string_span; on non-string paths it is simply
                // overwritten on the next advance, which is correct.
                bs_bits = ar.backslash;
            }
        }};
    }
    /// Like `next_structural!` but does not update `bs_bits`.
    /// Used on paths that never call `string_span` (ROOT_DONE, SKIP container loops).
    macro_rules! next_structural_skip {
        () => {{
            loop {
                let mut idx: u32 = 0;
                if !ctz64_empty(bits, &mut idx) {
                    cur_pos = chunk_ptr.add(idx as usize);
                    bits = clear_lowest_bit(bits);
                    break *cur_pos as i32;
                }
                let ar = advance_chunk(chunk_ptr, buf_end, &mut scan_state);
                if ar.chunk_ptr == chunk_ptr {
                    break EOF;
                }
                chunk_ptr = ar.chunk_ptr;
                bits = ar.bits;
            }
        }};
    }
    macro_rules! match_keyword {
        ($match_fn:ident, $advance_by:expr, $resume_phase:expr) => {{
            let kw = $match_fn(cur_pos, buf_end, &scan_state);
            if kw != KwResult::Ok {
                if kw == KwResult::Truncated {
                    suspend_here!($resume_phase);
                }
                error_exit!(ExitCode::ErrKeyword as i32, cur_offset!());
            }
            cur_pos = cur_pos.add($advance_by);
        }};
    }
    macro_rules! parse_string_span {
        ($out_end:ident, $out_has_escape:ident, $resume_phase:expr, $rollback_pos:expr) => {
            let _sr = string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
            bits = _sr.bits;
            chunk_ptr = _sr.chunk_ptr;
            bs_bits = _sr.backslash;
            let $out_end = _sr.end;
            let $out_has_escape = _sr.has_escape;
            if _sr.status != SpanStatus::Ok {
                if _sr.status == SpanStatus::Truncated {
                    suspend_at!($resume_phase, $rollback_pos);
                }
                error_exit!(ExitCode::ErrEof as i32, cur_offset!());
            }
        };
    }
    macro_rules! parse_number_span {
        ($out_end:ident, $resume_phase:expr, $rollback_pos:expr) => {
            let _sr = number_span(bits, buf_end, chunk_ptr, &mut scan_state);
            bits = _sr.bits;
            chunk_ptr = _sr.chunk_ptr;
            let $out_end = _sr.end;
            if _sr.status == SpanStatus::Truncated {
                suspend_at!($resume_phase, $rollback_pos);
            }
        };
    }

    // ------------------------------------------------------------------
    // Bootstrap
    // ------------------------------------------------------------------

    let initial_phase: u32;

    if depth != 0 {
        if bits == 0 {
            let len = buf_end.offset_from(chunk_ptr);
            if len >= 64 {
                let r: ChunkResult = scan_chunk(chunk_ptr, &mut scan_state);
                bits = r.structural;
                bs_bits = r.backslash;
            } else if (scan_state).is_final && buf_end > chunk_ptr {
                let mut padded = [0x20u8; 64];
                core::ptr::copy_nonoverlapping(chunk_ptr, padded.as_mut_ptr(), len as usize);
                let r: ChunkResult = scan_chunk(padded.as_ptr(), &mut scan_state);
                let mask = (1u64 << (len as u32)) - 1;
                bits = r.structural & mask;
                bs_bits = r.backslash & mask;
            }
        }
        initial_phase = (*frames.add(depth as usize - 1)).phase;
    } else {
        chunk_ptr = buf;
        let len = buf_end.offset_from(chunk_ptr);
        if len >= 64 {
            let r: ChunkResult = scan_chunk(chunk_ptr, &mut scan_state);
            bits = r.structural;
            bs_bits = r.backslash;
        } else if (scan_state).is_final && buf_end > chunk_ptr {
            let mut padded = [0x20u8; 64];
            core::ptr::copy_nonoverlapping(chunk_ptr, padded.as_mut_ptr(), len as usize);
            let r: ChunkResult = scan_chunk(padded.as_ptr(), &mut scan_state);
            let mask = (1u64 << (len as u32)) - 1;
            bits = r.structural & mask;
            bs_bits = r.backslash & mask;
        } else {
            bits = 0;
            bs_bits = 0;
        }
        (*frames.add(0)).phase = PH_ROOT_VALUE;
        (*frames.add(0)).data = 0;
        depth = 1;
        initial_phase = PH_ROOT_VALUE;
    }

    // ------------------------------------------------------------------
    // Main dispatch loop — dense 9-entry jump table
    // ------------------------------------------------------------------

    let mut current_phase = initial_phase;

    'dispatch: loop {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "adr {base}, 1000f",
                "ldrsw {off}, [{base}, {phase:w}, sxtw #2]",
                "add {base}, {base}, {off}",
                "br {base}",
                ".p2align 2",
                "1000:",
                ".long {L0} - 1000b",
                ".long {L1} - 1000b",
                ".long {L2} - 1000b",
                ".long {L3} - 1000b",
                ".long {L4} - 1000b",
                ".long {L5} - 1000b",
                ".long {L6} - 1000b",
                ".long {L7} - 1000b",
                ".long {L8} - 1000b",
                base = out(reg) _,
                off = out(reg) _,
                phase = in(reg) current_phase as u64,
                L0 = label { unsafe {
                let ch = next_structural!();
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ROOT_DONE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ROOT_DONE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == EOF {
                    if (scan_state).is_final {
                        error_exit!(
                            ExitCode::ErrEof as i32,
                            (buf_end as usize - buf as usize) as u32
                        );
                    }
                    suspend_next!(PH_ROOT_VALUE);
                }
                // Root scalar (rare) — inlined, no sentinel phase.
                let ch = *cur_pos as i32;
                if ch == b'"' as i32 {
                    let str_start = cur_pos.add(1);
                    let sr = string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                    bits = sr.bits;
                    chunk_ptr = sr.chunk_ptr;
                    bs_bits = sr.backslash;
                    if sr.status != SpanStatus::Ok {
                        if sr.status == SpanStatus::Truncated {
                            suspend_here!(PH_ROOT_VALUE);
                        }
                        error_exit!(ExitCode::ErrEof as i32, cur_offset!());
                    }
                    let si = StrInfo {
                        raw: RawStr::new(str_start, (sr.end as usize - str_start as usize) as u32),
                        has_escape: sr.has_escape,
                    };
                    cur_pos = sr.end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ROOT_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ROOT_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ROOT_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let num_start = cur_pos;
                    parse_number_span!(end, PH_ROOT_VALUE, num_start);
                    let raw = RawStr::new(num_start, (end as usize - num_start as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }},
                L1 = label { unsafe {
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let quote_pos = cur_pos;
                    let key_start = cur_pos.add(1);
                    parse_string_span!(end, has_esc, PH_OBJ_FIELD_OR_END, quote_pos);
                    let colon = next_structural!();
                    if colon != b':' as i32 {
                        if colon == EOF {
                            suspend_at!(PH_OBJ_FIELD_OR_END, quote_pos);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    let key = StrInfo {
                        raw: RawStr::new(key_start, (end as usize - key_start as usize) as u32),
                        has_escape: has_esc,
                    };
                    let d = reactor.object_field(key);
                    if d != PROCEED {
                        if d == SKIP {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            top_frame!().data = 0;
                            current_phase = PH_SKIP_VALUE;
                            continue 'dispatch;
                        }
                        yield_or_error!(d, PH_OBJ_FIELD_VALUE);
                    }
                    current_phase = PH_OBJ_FIELD_VALUE;
                    continue 'dispatch;
                }
                if ch == b'}' as i32 {
                    cur_pos = cur_pos.add(1);
                    stack_pop!();
                    let d = reactor.end_object();
                    if d < 0 {
                        yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                    }
                    current_phase = (*frames.add(depth as usize - 1)).phase;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_OBJ_FIELD_OR_END);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }},
                L2 = label { unsafe {
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_OBJ_FIELD_VALUE, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_OBJ_FIELD_VALUE, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_OBJ_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_OBJ_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_OBJ_FIELD_VALUE);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }},
                L3 = label { unsafe {
                'obj_loop: loop {
                    let ch = next_structural!();
                    if ch == b',' as i32 {
                        let comma_pos = cur_pos;
                        let nch = next_structural!();
                        if nch == EOF {
                            suspend_at!(PH_OBJ_CONTINUE, comma_pos);
                        }
                        if nch != b'"' as i32 {
                            error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                        }
                        let key_start = cur_pos.add(1);
                        parse_string_span!(end, has_esc, PH_OBJ_CONTINUE, comma_pos);
                        let colon = next_structural!();
                        if colon != b':' as i32 {
                            if colon == EOF {
                                suspend_at!(PH_OBJ_CONTINUE, comma_pos);
                            }
                            error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                        }
                        let key = StrInfo {
                            raw: RawStr::new(key_start, (end as usize - key_start as usize) as u32),
                            has_escape: has_esc,
                        };
                        let d = reactor.object_field(key);
                        if d != PROCEED {
                            if d == SKIP {
                                top_frame!().phase = PH_OBJ_CONTINUE;
                                top_frame!().data = 0;
                                current_phase = PH_SKIP_VALUE;
                                continue 'dispatch;
                            }
                            yield_or_error!(d, PH_OBJ_FIELD_VALUE);
                        }
                        // Inline the value parsing (Phase 2) directly here
                        let ch = next_structural!();
                        if ch == b'"' as i32 {
                            let vb = cur_pos;
                            let ss = cur_pos.add(1);
                            parse_string_span!(vend, he, PH_OBJ_FIELD_VALUE, vb);
                            let si = StrInfo {
                                raw: RawStr::new(ss, (vend as usize - ss as usize) as u32),
                                has_escape: he,
                            };
                            cur_pos = vend.add(1);
                            let d = reactor.scalar_string(si);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop; // Stay in Phase 3 tight loop
                        }
                        if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                            let ns = cur_pos;
                            parse_number_span!(vend, PH_OBJ_FIELD_VALUE, ns);
                            let raw = RawStr::new(ns, (vend as usize - ns as usize) as u32);
                            cur_pos = vend;
                            let d = reactor.scalar_number(raw);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop; // Stay in Phase 3 tight loop
                        }
                        // Non-scalar value: dispatch normally
                        if ch == b'{' as i32 {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            stack_push!(PH_OBJ_FIELD_OR_END);
                            let d = reactor.begin_object();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                            }
                            current_phase = PH_OBJ_FIELD_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'[' as i32 {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            stack_push!(PH_ARR_ELEM_OR_END);
                            let d = reactor.begin_array();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_ELEM_OR_END);
                            }
                            current_phase = PH_ARR_ELEM_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'n' as i32 {
                            match_keyword!(match_null, 4, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_null();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == b't' as i32 {
                            match_keyword!(match_true, 4, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_bool(true);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == b'f' as i32 {
                            match_keyword!(match_false, 5, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_bool(false);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == EOF {
                            suspend_next!(PH_OBJ_FIELD_VALUE);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    if ch == b'}' as i32 {
                        cur_pos = cur_pos.add(1);
                        stack_pop!();
                        let d = reactor.end_object();
                        if d < 0 {
                            yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                        }
                        current_phase = (*frames.add(depth as usize - 1)).phase;
                        continue 'dispatch;
                    }
                    if ch == EOF {
                        suspend_here!(PH_OBJ_CONTINUE);
                    }
                    error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }
                }},
                L4 = label { unsafe {
                let ch = next_structural!();
                if ch == b']' as i32 {
                    cur_pos = cur_pos.add(1);
                    stack_pop!();
                    let d = reactor.end_array();
                    if d < 0 {
                        yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                    }
                    current_phase = (*frames.add(depth as usize - 1)).phase;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_ARR_ELEM_OR_END);
                }
                let d = reactor.array_elem();
                if d != PROCEED {
                    if d == SKIP {
                        top_frame!().phase = PH_ARR_CONTINUE;
                        // skip_consumed_value: cur_pos on first byte already
                        // Inline skip_dispatch for the consumed case
                        let ch2 = *cur_pos as i32;
                        if ch2 == b'"' as i32 {
                            let qp = cur_pos;
                            let sr =
                                string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                            bits = sr.bits;
                            chunk_ptr = sr.chunk_ptr;
                            bs_bits = sr.backslash;
                            if sr.status != SpanStatus::Ok {
                                suspend_at!(PH_SKIP_VALUE, qp);
                            }
                            cur_pos = sr.end.add(1);
                            current_phase = PH_ARR_CONTINUE;
                            continue 'dispatch;
                        }
                        if ch2 != b'{' as i32 && ch2 != b'[' as i32 {
                            cur_pos = cur_pos.add(1);
                            current_phase = PH_ARR_CONTINUE;
                            continue 'dispatch;
                        }
                        top_frame!().data = 1;
                        // Enter skip_container via PH_SKIP_VALUE
                        current_phase = PH_SKIP_VALUE;
                        continue 'dispatch;
                    }
                    error_exit!(d, cur_offset!());
                }
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_ARR_ELEM_OR_END, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_ARR_ELEM_OR_END, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }},
                L5 = label { unsafe {
                let d = reactor.array_elem();
                if d != PROCEED {
                    if d == SKIP {
                        top_frame!().phase = PH_ARR_CONTINUE;
                        top_frame!().data = 0;
                        current_phase = PH_SKIP_VALUE;
                        continue 'dispatch;
                    }
                    error_exit!(d, cur_offset!());
                }
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_ARR_ELEM_VALUE, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_ARR_ELEM_VALUE, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_ARR_ELEM_VALUE);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }},
                L6 = label { unsafe {
                'arr_loop: loop {
                    let ch = next_structural!();
                    if ch == b',' as i32 {
                        // Inline Phase 5 (ARRAY_ELEM_VALUE)
                        let d = reactor.array_elem();
                        if d != PROCEED {
                            if d == SKIP {
                                top_frame!().phase = PH_ARR_CONTINUE;
                                top_frame!().data = 0;
                                current_phase = PH_SKIP_VALUE;
                                continue 'dispatch;
                            }
                            error_exit!(d, cur_offset!());
                        }
                        let ch = next_structural!();
                        if ch == b'"' as i32 {
                            let vb = cur_pos;
                            let ss = cur_pos.add(1);
                            parse_string_span!(end, he, PH_ARR_ELEM_VALUE, vb);
                            let si = StrInfo {
                                raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                                has_escape: he,
                            };
                            cur_pos = end.add(1);
                            let d = reactor.scalar_string(si);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                            let ns = cur_pos;
                            parse_number_span!(end, PH_ARR_ELEM_VALUE, ns);
                            let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                            cur_pos = end;
                            let d = reactor.scalar_number(raw);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        // Non-scalar: dispatch
                        if ch == b'{' as i32 {
                            top_frame!().phase = PH_ARR_CONTINUE;
                            stack_push!(PH_OBJ_FIELD_OR_END);
                            let d = reactor.begin_object();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                            }
                            current_phase = PH_OBJ_FIELD_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'[' as i32 {
                            top_frame!().phase = PH_ARR_CONTINUE;
                            stack_push!(PH_ARR_ELEM_OR_END);
                            let d = reactor.begin_array();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_ELEM_OR_END);
                            }
                            current_phase = PH_ARR_ELEM_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'n' as i32 {
                            match_keyword!(match_null, 4, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_null();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b't' as i32 {
                            match_keyword!(match_true, 4, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_bool(true);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b'f' as i32 {
                            match_keyword!(match_false, 5, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_bool(false);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == EOF {
                            suspend_next!(PH_ARR_ELEM_VALUE);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    if ch == b']' as i32 {
                        cur_pos = cur_pos.add(1);
                        stack_pop!();
                        let d = reactor.end_array();
                        if d < 0 {
                            yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                        }
                        current_phase = (*frames.add(depth as usize - 1)).phase;
                        continue 'dispatch;
                    }
                    if ch == EOF {
                        suspend_here!(PH_ARR_CONTINUE);
                    }
                    error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }
                }},
                L7 = label { unsafe {
                let ch = next_structural_skip!();
                if ch == EOF {
                    stack_pop!();
                    save_and_return!(ExitCode::Ok as i32);
                }
                error_exit!(ExitCode::ErrTrailing as i32, cur_offset!());
                }},
                L8 = label { unsafe {
                let resume_phase = top_frame!().phase;
                if top_frame!().data > 0 {
                    // Resuming inside a container skip.
                    let mut skip_depth = top_frame!().data;
                    loop {
                        let ch = next_structural_skip!();
                        if ch == b'{' as i32 || ch == b'[' as i32 {
                            skip_depth += 1;
                        } else if ch == b'}' as i32 || ch == b']' as i32 {
                            skip_depth -= 1;
                            if skip_depth == 0 {
                                cur_pos = cur_pos.add(1);
                                current_phase = resume_phase;
                                continue 'dispatch;
                            }
                        } else if ch == EOF {
                            top_frame!().data = skip_depth;
                            suspend_next!(PH_SKIP_VALUE);
                        }
                    }
                }
                // Fresh skip: get first structural.
                let ch = next_structural!();
                if ch == EOF {
                    if (scan_state).is_final {
                        error_exit!(ExitCode::ErrEof as i32, cur_offset!());
                    }
                    suspend_next!(PH_SKIP_VALUE);
                }
                // skip_dispatch
                let ch = *cur_pos as i32;
                if ch == b'"' as i32 {
                    let qp = cur_pos;
                    let sr = string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                    bits = sr.bits;
                    chunk_ptr = sr.chunk_ptr;
                    bs_bits = sr.backslash;
                    if sr.status != SpanStatus::Ok {
                        suspend_at!(PH_SKIP_VALUE, qp);
                    }
                    cur_pos = sr.end.add(1);
                    current_phase = resume_phase;
                    continue 'dispatch;
                }
                if ch != b'{' as i32 && ch != b'[' as i32 {
                    cur_pos = cur_pos.add(1);
                    current_phase = resume_phase;
                    continue 'dispatch;
                }
                // Container: enter skip loop.
                let mut skip_depth: u32 = 1;
                loop {
                    let ch = next_structural_skip!();
                    if ch == b'{' as i32 || ch == b'[' as i32 {
                        skip_depth += 1;
                    } else if ch == b'}' as i32 || ch == b']' as i32 {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            cur_pos = cur_pos.add(1);
                            current_phase = resume_phase;
                            continue 'dispatch;
                        }
                    } else if ch == EOF {
                        top_frame!().data = skip_depth;
                        suspend_next!(PH_SKIP_VALUE);
                    }
                }
                }},
            );
            core::hint::unreachable_unchecked();
        }

        #[cfg(not(target_arch = "aarch64"))]
        match current_phase {
            // 0: ROOT_VALUE
            0 => {
                let ch = next_structural!();
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ROOT_DONE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ROOT_DONE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == EOF {
                    if (scan_state).is_final {
                        error_exit!(
                            ExitCode::ErrEof as i32,
                            (buf_end as usize - buf as usize) as u32
                        );
                    }
                    suspend_next!(PH_ROOT_VALUE);
                }
                // Root scalar (rare) — inlined, no sentinel phase.
                let ch = *cur_pos as i32;
                if ch == b'"' as i32 {
                    let str_start = cur_pos.add(1);
                    let sr = string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                    bits = sr.bits;
                    chunk_ptr = sr.chunk_ptr;
                    bs_bits = sr.backslash;
                    if sr.status != SpanStatus::Ok {
                        if sr.status == SpanStatus::Truncated {
                            suspend_here!(PH_ROOT_VALUE);
                        }
                        error_exit!(ExitCode::ErrEof as i32, cur_offset!());
                    }
                    let si = StrInfo {
                        raw: RawStr::new(str_start, (sr.end as usize - str_start as usize) as u32),
                        has_escape: sr.has_escape,
                    };
                    cur_pos = sr.end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ROOT_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ROOT_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ROOT_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let num_start = cur_pos;
                    parse_number_span!(end, PH_ROOT_VALUE, num_start);
                    let raw = RawStr::new(num_start, (end as usize - num_start as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ROOT_DONE);
                    }
                    current_phase = PH_ROOT_DONE;
                    continue 'dispatch;
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
            }

            // 1: OBJECT_FIELD_OR_END
            1 => {
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let quote_pos = cur_pos;
                    let key_start = cur_pos.add(1);
                    parse_string_span!(end, has_esc, PH_OBJ_FIELD_OR_END, quote_pos);
                    let colon = next_structural!();
                    if colon != b':' as i32 {
                        if colon == EOF {
                            suspend_at!(PH_OBJ_FIELD_OR_END, quote_pos);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    let key = StrInfo {
                        raw: RawStr::new(key_start, (end as usize - key_start as usize) as u32),
                        has_escape: has_esc,
                    };
                    let d = reactor.object_field(key);
                    if d != PROCEED {
                        if d == SKIP {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            top_frame!().data = 0;
                            current_phase = PH_SKIP_VALUE;
                            continue 'dispatch;
                        }
                        yield_or_error!(d, PH_OBJ_FIELD_VALUE);
                    }
                    current_phase = PH_OBJ_FIELD_VALUE;
                    continue 'dispatch;
                }
                if ch == b'}' as i32 {
                    cur_pos = cur_pos.add(1);
                    stack_pop!();
                    let d = reactor.end_object();
                    if d < 0 {
                        yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                    }
                    current_phase = (*frames.add(depth as usize - 1)).phase;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_OBJ_FIELD_OR_END);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
            }

            // 2: OBJECT_FIELD_VALUE
            2 => {
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_OBJ_FIELD_VALUE, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_OBJ_FIELD_VALUE, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_OBJ_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_OBJ_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_OBJ_FIELD_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_CONTINUE);
                    }
                    current_phase = PH_OBJ_CONTINUE;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_OBJ_FIELD_VALUE);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
            }

            // 3: OBJECT_CONTINUE_OR_END
            // Fused tight loop: comma → key → colon → value → repeat
            // Avoids re-dispatching to Phase 2 for scalar values.
            3 => {
                'obj_loop: loop {
                    let ch = next_structural!();
                    if ch == b',' as i32 {
                        let comma_pos = cur_pos;
                        let nch = next_structural!();
                        if nch == EOF {
                            suspend_at!(PH_OBJ_CONTINUE, comma_pos);
                        }
                        if nch != b'"' as i32 {
                            error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                        }
                        let key_start = cur_pos.add(1);
                        parse_string_span!(end, has_esc, PH_OBJ_CONTINUE, comma_pos);
                        let colon = next_structural!();
                        if colon != b':' as i32 {
                            if colon == EOF {
                                suspend_at!(PH_OBJ_CONTINUE, comma_pos);
                            }
                            error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                        }
                        let key = StrInfo {
                            raw: RawStr::new(key_start, (end as usize - key_start as usize) as u32),
                            has_escape: has_esc,
                        };
                        let d = reactor.object_field(key);
                        if d != PROCEED {
                            if d == SKIP {
                                top_frame!().phase = PH_OBJ_CONTINUE;
                                top_frame!().data = 0;
                                current_phase = PH_SKIP_VALUE;
                                continue 'dispatch;
                            }
                            yield_or_error!(d, PH_OBJ_FIELD_VALUE);
                        }
                        // Inline the value parsing (Phase 2) directly here
                        let ch = next_structural!();
                        if ch == b'"' as i32 {
                            let vb = cur_pos;
                            let ss = cur_pos.add(1);
                            parse_string_span!(vend, he, PH_OBJ_FIELD_VALUE, vb);
                            let si = StrInfo {
                                raw: RawStr::new(ss, (vend as usize - ss as usize) as u32),
                                has_escape: he,
                            };
                            cur_pos = vend.add(1);
                            let d = reactor.scalar_string(si);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop; // Stay in Phase 3 tight loop
                        }
                        if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                            let ns = cur_pos;
                            parse_number_span!(vend, PH_OBJ_FIELD_VALUE, ns);
                            let raw = RawStr::new(ns, (vend as usize - ns as usize) as u32);
                            cur_pos = vend;
                            let d = reactor.scalar_number(raw);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop; // Stay in Phase 3 tight loop
                        }
                        // Non-scalar value: dispatch normally
                        if ch == b'{' as i32 {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            stack_push!(PH_OBJ_FIELD_OR_END);
                            let d = reactor.begin_object();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                            }
                            current_phase = PH_OBJ_FIELD_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'[' as i32 {
                            top_frame!().phase = PH_OBJ_CONTINUE;
                            stack_push!(PH_ARR_ELEM_OR_END);
                            let d = reactor.begin_array();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_ELEM_OR_END);
                            }
                            current_phase = PH_ARR_ELEM_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'n' as i32 {
                            match_keyword!(match_null, 4, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_null();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == b't' as i32 {
                            match_keyword!(match_true, 4, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_bool(true);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == b'f' as i32 {
                            match_keyword!(match_false, 5, PH_OBJ_FIELD_VALUE);
                            let d = reactor.scalar_bool(false);
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_CONTINUE);
                            }
                            continue 'obj_loop;
                        }
                        if ch == EOF {
                            suspend_next!(PH_OBJ_FIELD_VALUE);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    if ch == b'}' as i32 {
                        cur_pos = cur_pos.add(1);
                        stack_pop!();
                        let d = reactor.end_object();
                        if d < 0 {
                            yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                        }
                        current_phase = (*frames.add(depth as usize - 1)).phase;
                        continue 'dispatch;
                    }
                    if ch == EOF {
                        suspend_here!(PH_OBJ_CONTINUE);
                    }
                    error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }
            }

            // 4: ARRAY_ELEM_OR_END
            4 => {
                let ch = next_structural!();
                if ch == b']' as i32 {
                    cur_pos = cur_pos.add(1);
                    stack_pop!();
                    let d = reactor.end_array();
                    if d < 0 {
                        yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                    }
                    current_phase = (*frames.add(depth as usize - 1)).phase;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_ARR_ELEM_OR_END);
                }
                let d = reactor.array_elem();
                if d != PROCEED {
                    if d == SKIP {
                        top_frame!().phase = PH_ARR_CONTINUE;
                        // skip_consumed_value: cur_pos on first byte already
                        // Inline skip_dispatch for the consumed case
                        let ch2 = *cur_pos as i32;
                        if ch2 == b'"' as i32 {
                            let qp = cur_pos;
                            let sr =
                                string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                            bits = sr.bits;
                            chunk_ptr = sr.chunk_ptr;
                            bs_bits = sr.backslash;
                            if sr.status != SpanStatus::Ok {
                                suspend_at!(PH_SKIP_VALUE, qp);
                            }
                            cur_pos = sr.end.add(1);
                            current_phase = PH_ARR_CONTINUE;
                            continue 'dispatch;
                        }
                        if ch2 != b'{' as i32 && ch2 != b'[' as i32 {
                            cur_pos = cur_pos.add(1);
                            current_phase = PH_ARR_CONTINUE;
                            continue 'dispatch;
                        }
                        top_frame!().data = 1;
                        // Enter skip_container via PH_SKIP_VALUE
                        current_phase = PH_SKIP_VALUE;
                        continue 'dispatch;
                    }
                    error_exit!(d, cur_offset!());
                }
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_ARR_ELEM_OR_END, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_ARR_ELEM_OR_END, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ARR_ELEM_OR_END);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
            }

            // 5: ARRAY_ELEM_VALUE
            5 => {
                let d = reactor.array_elem();
                if d != PROCEED {
                    if d == SKIP {
                        top_frame!().phase = PH_ARR_CONTINUE;
                        top_frame!().data = 0;
                        current_phase = PH_SKIP_VALUE;
                        continue 'dispatch;
                    }
                    error_exit!(d, cur_offset!());
                }
                let ch = next_structural!();
                if ch == b'"' as i32 {
                    let vb = cur_pos;
                    let ss = cur_pos.add(1);
                    parse_string_span!(end, he, PH_ARR_ELEM_VALUE, vb);
                    let si = StrInfo {
                        raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                        has_escape: he,
                    };
                    cur_pos = end.add(1);
                    let d = reactor.scalar_string(si);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                    let ns = cur_pos;
                    parse_number_span!(end, PH_ARR_ELEM_VALUE, ns);
                    let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                    cur_pos = end;
                    let d = reactor.scalar_number(raw);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'{' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_OBJ_FIELD_OR_END);
                    let d = reactor.begin_object();
                    if d < 0 {
                        yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                    }
                    current_phase = PH_OBJ_FIELD_OR_END;
                    continue 'dispatch;
                }
                if ch == b'[' as i32 {
                    top_frame!().phase = PH_ARR_CONTINUE;
                    stack_push!(PH_ARR_ELEM_OR_END);
                    let d = reactor.begin_array();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_ELEM_OR_END);
                    }
                    current_phase = PH_ARR_ELEM_OR_END;
                    continue 'dispatch;
                }
                if ch == b'n' as i32 {
                    match_keyword!(match_null, 4, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_null();
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b't' as i32 {
                    match_keyword!(match_true, 4, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_bool(true);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == b'f' as i32 {
                    match_keyword!(match_false, 5, PH_ARR_ELEM_VALUE);
                    let d = reactor.scalar_bool(false);
                    if d < 0 {
                        yield_or_error!(d, PH_ARR_CONTINUE);
                    }
                    current_phase = PH_ARR_CONTINUE;
                    continue 'dispatch;
                }
                if ch == EOF {
                    suspend_next!(PH_ARR_ELEM_VALUE);
                }
                error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
            }

            // 6: ARRAY_CONTINUE_OR_END
            // Fused tight loop: comma → array_elem → value → repeat
            6 => {
                'arr_loop: loop {
                    let ch = next_structural!();
                    if ch == b',' as i32 {
                        // Inline Phase 5 (ARRAY_ELEM_VALUE)
                        let d = reactor.array_elem();
                        if d != PROCEED {
                            if d == SKIP {
                                top_frame!().phase = PH_ARR_CONTINUE;
                                top_frame!().data = 0;
                                current_phase = PH_SKIP_VALUE;
                                continue 'dispatch;
                            }
                            error_exit!(d, cur_offset!());
                        }
                        let ch = next_structural!();
                        if ch == b'"' as i32 {
                            let vb = cur_pos;
                            let ss = cur_pos.add(1);
                            parse_string_span!(end, he, PH_ARR_ELEM_VALUE, vb);
                            let si = StrInfo {
                                raw: RawStr::new(ss, (end as usize - ss as usize) as u32),
                                has_escape: he,
                            };
                            cur_pos = end.add(1);
                            let d = reactor.scalar_string(si);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b'-' as i32 || (ch >= b'0' as i32 && ch <= b'9' as i32) {
                            let ns = cur_pos;
                            parse_number_span!(end, PH_ARR_ELEM_VALUE, ns);
                            let raw = RawStr::new(ns, (end as usize - ns as usize) as u32);
                            cur_pos = end;
                            let d = reactor.scalar_number(raw);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        // Non-scalar: dispatch
                        if ch == b'{' as i32 {
                            top_frame!().phase = PH_ARR_CONTINUE;
                            stack_push!(PH_OBJ_FIELD_OR_END);
                            let d = reactor.begin_object();
                            if d < 0 {
                                yield_or_error!(d, PH_OBJ_FIELD_OR_END);
                            }
                            current_phase = PH_OBJ_FIELD_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'[' as i32 {
                            top_frame!().phase = PH_ARR_CONTINUE;
                            stack_push!(PH_ARR_ELEM_OR_END);
                            let d = reactor.begin_array();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_ELEM_OR_END);
                            }
                            current_phase = PH_ARR_ELEM_OR_END;
                            continue 'dispatch;
                        }
                        if ch == b'n' as i32 {
                            match_keyword!(match_null, 4, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_null();
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b't' as i32 {
                            match_keyword!(match_true, 4, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_bool(true);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == b'f' as i32 {
                            match_keyword!(match_false, 5, PH_ARR_ELEM_VALUE);
                            let d = reactor.scalar_bool(false);
                            if d < 0 {
                                yield_or_error!(d, PH_ARR_CONTINUE);
                            }
                            continue 'arr_loop;
                        }
                        if ch == EOF {
                            suspend_next!(PH_ARR_ELEM_VALUE);
                        }
                        error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                    }
                    if ch == b']' as i32 {
                        cur_pos = cur_pos.add(1);
                        stack_pop!();
                        let d = reactor.end_array();
                        if d < 0 {
                            yield_or_error!(d, (*frames.add(depth as usize - 1)).phase);
                        }
                        current_phase = (*frames.add(depth as usize - 1)).phase;
                        continue 'dispatch;
                    }
                    if ch == EOF {
                        suspend_here!(PH_ARR_CONTINUE);
                    }
                    error_exit!(ExitCode::ErrSyntax as i32, cur_offset!());
                }
            }

            // 7: ROOT_DONE
            7 => {
                let ch = next_structural_skip!();
                if ch == EOF {
                    stack_pop!();
                    save_and_return!(ExitCode::Ok as i32);
                }
                error_exit!(ExitCode::ErrTrailing as i32, cur_offset!());
            }

            // 8: SKIP_VALUE — self-contained skip logic
            8 => {
                let resume_phase = top_frame!().phase;
                if top_frame!().data > 0 {
                    // Resuming inside a container skip.
                    let mut skip_depth = top_frame!().data;
                    loop {
                        let ch = next_structural_skip!();
                        if ch == b'{' as i32 || ch == b'[' as i32 {
                            skip_depth += 1;
                        } else if ch == b'}' as i32 || ch == b']' as i32 {
                            skip_depth -= 1;
                            if skip_depth == 0 {
                                cur_pos = cur_pos.add(1);
                                current_phase = resume_phase;
                                continue 'dispatch;
                            }
                        } else if ch == EOF {
                            top_frame!().data = skip_depth;
                            suspend_next!(PH_SKIP_VALUE);
                        }
                    }
                }
                // Fresh skip: get first structural.
                let ch = next_structural!();
                if ch == EOF {
                    if (scan_state).is_final {
                        error_exit!(ExitCode::ErrEof as i32, cur_offset!());
                    }
                    suspend_next!(PH_SKIP_VALUE);
                }
                // skip_dispatch
                let ch = *cur_pos as i32;
                if ch == b'"' as i32 {
                    let qp = cur_pos;
                    let sr = string_span(bits, bs_bits, buf_end, chunk_ptr, &mut scan_state);
                    bits = sr.bits;
                    chunk_ptr = sr.chunk_ptr;
                    bs_bits = sr.backslash;
                    if sr.status != SpanStatus::Ok {
                        suspend_at!(PH_SKIP_VALUE, qp);
                    }
                    cur_pos = sr.end.add(1);
                    current_phase = resume_phase;
                    continue 'dispatch;
                }
                if ch != b'{' as i32 && ch != b'[' as i32 {
                    cur_pos = cur_pos.add(1);
                    current_phase = resume_phase;
                    continue 'dispatch;
                }
                // Container: enter skip loop.
                let mut skip_depth: u32 = 1;
                loop {
                    let ch = next_structural_skip!();
                    if ch == b'{' as i32 || ch == b'[' as i32 {
                        skip_depth += 1;
                    } else if ch == b'}' as i32 || ch == b']' as i32 {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            cur_pos = cur_pos.add(1);
                            current_phase = resume_phase;
                            continue 'dispatch;
                        }
                    } else if ch == EOF {
                        top_frame!().data = skip_depth;
                        suspend_next!(PH_SKIP_VALUE);
                    }
                }
            }

            // SAFETY: phase is always 0..=8 by construction.
            _ => {
                core::hint::unreachable_unchecked();
            }
        }
    }
}
