//! Core types mapped from C `ndec/impl/types.h`.

use std::marker::PhantomData;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_DEPTH: usize = 256;

// ---------------------------------------------------------------------------
// ExitCode
// ---------------------------------------------------------------------------

/// Parser exit / error codes, matching `enum NdecExit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Ok = 0,
    Suspend = 1,
    ErrSyntax = 2,
    ErrEof = 3,
    ErrDepth = 4,
    ErrKeyword = 5,
    ErrTrailing = 6,
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

/// Frame phase, matching `enum NdecPhase`.
///
/// Each frame's phase describes "what this frame does next".
/// A parent writes its own continuation phase before pushing; the child's end
/// hook pops the stack and dispatches via `frames[depth - 1].phase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Phase {
    RootValue = 0,
    ObjectFieldOrEnd = 1,
    ObjectFieldValue = 2,
    ObjectContinueOrEnd = 3,
    ArrayElemOrEnd = 4,
    ArrayElemValue = 5,
    ArrayContinueOrEnd = 6,
    RootDone = 7,
    SkipValue = 8,
}

/// Total number of phase variants (for jump-table sizing, etc.).
pub const PHASE_COUNT: u32 = 9;

// ---------------------------------------------------------------------------
// ScanState
// ---------------------------------------------------------------------------

/// Cross-chunk carry state for the SIMD scanner, matching `NdecScanState`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ScanState {
    /// 0 or `!0` — were we inside a string at the end of the last chunk?
    pub prev_in_string: u64,
    /// 0 or 1 — was the last byte of the previous chunk an active escape?
    pub prev_escape: u64,
    /// 0 or 1 — was the last byte structural-or-whitespace?
    pub prev_structural_or_ws: u64,
    /// Has the caller signalled end-of-input?
    pub is_final: bool,
}

// ---------------------------------------------------------------------------
// Reactor directives
// ---------------------------------------------------------------------------

/// Continue parsing.
pub const PROCEED: i32 = 0;
/// Skip the upcoming value (object value or array element).
pub const SKIP: i32 = 1;
/// Suspend — return control to the caller.
pub const YIELD: i32 = -1;

// ---------------------------------------------------------------------------
// RawStr / StrInfo
// ---------------------------------------------------------------------------

/// A borrowed raw byte range, matching `NdecRawStr`.
///
/// Kept as a raw pointer + length (rather than `&[u8]`) for exact performance
/// parity with the C implementation.  The caller guarantees the pointer
/// remains valid for lifetime `'a`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct RawStr<'a> {
    pub ptr: *const u8,
    pub len: u32,
    _marker: PhantomData<&'a [u8]>,
}

impl<'a> RawStr<'a> {
    /// Create a new `RawStr` from a pointer and length.
    #[inline]
    pub fn new(ptr: *const u8, len: u32) -> Self {
        Self {
            ptr,
            len,
            _marker: PhantomData,
        }
    }

    /// Interpret the raw pointer+length as a byte slice.
    ///
    /// # Safety
    /// The caller must ensure `ptr` is valid for `len` bytes and that the
    /// resulting reference does not outlive the backing buffer.
    #[inline]
    pub unsafe fn as_bytes(&self) -> &'a [u8] {
        std::slice::from_raw_parts(self.ptr, self.len as usize)
    }

    /// Try to interpret the raw bytes as a UTF-8 `&str`.
    ///
    /// Returns `None` if the bytes are not valid UTF-8 or the pointer is null.
    #[inline]
    pub fn as_str(&self) -> Option<&'a str> {
        if self.ptr.is_null() {
            return None;
        }
        // SAFETY: The contract of RawStr guarantees ptr is valid for len bytes
        // during lifetime 'a.
        let bytes = unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) };
        std::str::from_utf8(bytes).ok()
    }
}

/// Extended string info for callbacks that need escape metadata,
/// matching `NdecStrInfo`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct StrInfo<'a> {
    pub raw: RawStr<'a>,
    /// `true` iff the string content contains at least one backslash escape.
    pub has_escape: bool,
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// A single stack frame, matching `NdecFrame`.
///
/// Uses raw `u32` for `phase` (not the `Phase` enum) to match C's untyped
/// storage and allow the kernel to write arbitrary bit-patterns if needed.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Frame {
    pub phase: u32,
    /// Scratch slot — currently used only by `SkipValue` to persist
    /// `skip_depth` across suspend/resume.
    pub data: u32,
}

// ---------------------------------------------------------------------------
// Ctx
// ---------------------------------------------------------------------------

/// Parser context, matching `NdecCtx` (minus reactor/user_data).
///
/// In Rust the reactor is passed as a generic parameter to `parse()` rather
/// than stored inside the context, so the two C fields `reactor` and
/// `user_data` are absent here.
#[repr(C)]
pub struct Ctx {
    // -- Input buffer --
    pub buf: *const u8,
    pub buf_end: *const u8,

    // -- Hot cursor state (loaded into registers at entry, saved on exit) --
    pub cur_pos: *const u8,
    pub chunk_ptr: *const u8,
    pub structural_bits: u64,

    // -- Scanner carry state --
    pub scan_state: ScanState,

    // -- Exit / error --
    pub exit_code: i32,
    pub error_pos: u32,

    // -- Stack --
    pub depth: u32,
    pub frames: [Frame; MAX_DEPTH],
}

impl Ctx {
    /// Create a context matching C's `ndec_ctx_init`.
    ///
    /// Only zeroes the hot fields (~48 bytes); the `frames` array is left
    /// uninitialised — the kernel writes each frame before reading it.
    #[inline]
    pub fn new() -> Self {
        // SAFETY: We only leave `frames` uninitialised — it's a [Frame; 256]
        // array of plain u32 pairs with no drop/validity invariants. The
        // kernel always writes frames[i] via stack_push before reading it.
        // All other fields are explicitly initialised below.
        #[allow(invalid_value)]
        let mut ctx: Self = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        ctx.buf = std::ptr::null();
        ctx.buf_end = std::ptr::null();
        ctx.cur_pos = std::ptr::null();
        ctx.chunk_ptr = std::ptr::null();
        ctx.structural_bits = 0;
        ctx.scan_state = ScanState {
            prev_in_string: 0,
            prev_escape: 0,
            prev_structural_or_ws: 1,
            is_final: false,
        };
        ctx.exit_code = 0;
        ctx.error_pos = 0;
        ctx.depth = 0;
        ctx
    }

    /// Point the context at a new input buffer (raw pointer variant).
    ///
    /// Only updates `buf`, `buf_end`, and `is_final`. Scanner state
    /// (`cur_pos`, `chunk_ptr`, `structural_bits`, `scan_state` carries) is
    /// preserved for resume.
    #[inline]
    pub fn set_input(&mut self, buf: *const u8, len: u32, is_final: bool) {
        self.buf = buf;
        self.buf_end = unsafe { buf.add(len as usize) };
        self.scan_state.is_final = is_final;
    }

    /// Safe convenience: set input from a byte slice.
    #[inline]
    pub fn set_input_slice(&mut self, data: &[u8], is_final: bool) {
        self.buf = data.as_ptr();
        self.buf_end = unsafe { data.as_ptr().add(data.len()) };
        self.scan_state.is_final = is_final;
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("buf", &self.buf)
            .field("buf_end", &self.buf_end)
            .field("cur_pos", &self.cur_pos)
            .field("chunk_ptr", &self.chunk_ptr)
            .field("structural_bits", &self.structural_bits)
            .field("scan_state", &self.scan_state)
            .field("exit_code", &self.exit_code)
            .field("error_pos", &self.error_pos)
            .field("depth", &self.depth)
            // Don't dump the full 256-entry frame array — show active frames only.
            .field("frames[..depth]", &&self.frames[..self.depth as usize])
            .finish()
    }
}
