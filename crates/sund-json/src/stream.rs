//! Thin suspend/resume stream driver around the JSON parser.
//!
//! `Stream` wraps the parser's suspend/resume loop, feeding segments of a byte
//! stream one at a time. Each `feed()` call pushes the parser forward until
//! "input exhausted / top-level value complete / error", then returns
//! `DONE` / `NEED_MORE` / `ERROR`.
//!
//! On `NEED_MORE`, the caller must relocate `[tail, tail+tail_len)` to the
//! start of the next buffer and append new bytes after it.

use crate::parser;
use crate::reactor::Reactor;
use crate::types::{Ctx, ExitCode};

/// Status returned by `Stream::feed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatus {
    /// Top-level JSON value parsed successfully.
    Done = 0,
    /// Input exhausted mid-parse; more data needed.
    NeedMore = 1,
    /// Parse error. Check `ctx().exit_code` for the error code.
    Error = 2,
}

const F_DONE: u32 = 0x01;
const F_ERROR: u32 = 0x02;

/// Streaming JSON parser wrapping the sund parser.
///
/// The stream owns a `Ctx` and drives the parser's suspend/resume cycle.
/// Call `feed()` repeatedly with buffer segments; on `NeedMore`, move the
/// unconsumed tail to the front of your next buffer.
pub struct Stream {
    ctx: Ctx,
    flags: u32,
}

impl Stream {
    /// Create a new stream.
    ///
    /// The caller should have already called `ctx.set_input()` or will call
    /// `feed()` with data.
    pub fn new() -> Self {
        Self {
            ctx: Ctx::new(),
            flags: 0,
        }
    }

    /// Access the underlying `Ctx` (e.g. to inspect `exit_code` / `error_pos`).
    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    /// Mutable access to the underlying `Ctx`.
    pub fn ctx_mut(&mut self) -> &mut Ctx {
        &mut self.ctx
    }

    /// Feed one contiguous buffer segment.
    ///
    /// - `data`: current buffer span; must remain valid for the duration of
    ///   this call.
    /// - `is_final`: `true` iff this is the last segment (EOF).
    /// - `reactor`: the reactor to dispatch events to.
    ///
    /// Returns `(status, tail)` where `tail` is a sub-slice of `data`
    /// representing unconsumed bytes. On `NeedMore`, relocate `tail` to the
    /// start of your next buffer and append fresh data.
    pub fn feed<R: Reactor>(
        &mut self,
        data: &[u8],
        is_final: bool,
        reactor: &mut R,
    ) -> (StreamStatus, &[u8]) {
        if self.flags & F_ERROR != 0 {
            return (StreamStatus::Error, &[]);
        }
        if self.flags & F_DONE != 0 {
            return (StreamStatus::Done, &[]);
        }

        let ctx = &mut self.ctx;

        ctx.cur_pos = data.as_ptr();
        ctx.chunk_ptr = data.as_ptr();
        ctx.structural_bits = 0;
        ctx.scan_state.prev_in_string = 0;
        ctx.scan_state.prev_escape = 0;
        ctx.scan_state.prev_structural_or_ws = 1;
        ctx.set_input_slice(data, is_final);

        // SAFETY: ctx was initialised via Ctx::new() and input set via set_input_slice() above.
        unsafe { parser::parse(ctx, reactor) };

        let data_ptr = data.as_ptr();
        let data_end = unsafe { data_ptr.add(data.len()) };
        let mut cur = ctx.cur_pos;
        if cur < data_ptr {
            cur = data_ptr;
        }
        if cur > data_end {
            cur = data_end;
        }

        let tail_len = unsafe { data_end.offset_from(cur) } as usize;
        let tail = unsafe { std::slice::from_raw_parts(cur, tail_len) };

        let st = match ctx.exit_code {
            c if c == ExitCode::Ok as i32 => {
                self.flags |= F_DONE;
                StreamStatus::Done
            }
            c if c == ExitCode::Suspend as i32 => {
                if is_final {
                    ctx.exit_code = ExitCode::ErrEof as i32;
                    self.flags |= F_ERROR;
                    StreamStatus::Error
                } else {
                    StreamStatus::NeedMore
                }
            }
            _ => {
                self.flags |= F_ERROR;
                StreamStatus::Error
            }
        };

        (st, tail)
    }

    /// Convenience: treat `data` as the complete input.
    ///
    /// Equivalent to `feed(data, true, reactor).0`.
    pub fn feed_all<R: Reactor>(&mut self, data: &[u8], reactor: &mut R) -> StreamStatus {
        self.feed(data, true, reactor).0
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}
