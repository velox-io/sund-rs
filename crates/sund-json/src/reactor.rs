//! Reactor trait: callback interface for JSON parsing events.

use crate::types::{RawStr, StrInfo, PROCEED};

/// Callback interface for JSON parsing events.
///
/// The parser invokes these methods as it encounters structural tokens and
/// scalar values.  Each method returns an `i32` **directive**:
///
/// | Value      | Meaning                                        |
/// |------------|------------------------------------------------|
/// | `PROCEED`  (0)  | Continue parsing normally.               |
/// | `SKIP`     (1)  | Skip the upcoming value (field value or array element). |
/// | `YIELD`    (-1) | Suspend the parser; return control to the caller.       |
/// | `<= -2`        | Abort with a user-defined error code.    |
///
/// All methods have default implementations that return [`PROCEED`], so
/// implementations only need to override the hooks they care about.
pub trait Reactor {
    /// Called when `{` is encountered.
    fn begin_object(&mut self) -> i32 {
        PROCEED
    }

    /// Called after the matching `}`.
    fn end_object(&mut self) -> i32 {
        PROCEED
    }

    /// Called for each object field key (before the value is parsed).
    fn object_field(&mut self, _key: StrInfo<'_>) -> i32 {
        PROCEED
    }

    /// Called when `[` is encountered.
    fn begin_array(&mut self) -> i32 {
        PROCEED
    }

    /// Called after the matching `]`.
    fn end_array(&mut self) -> i32 {
        PROCEED
    }

    /// Called before each array element is parsed.
    fn array_elem(&mut self) -> i32 {
        PROCEED
    }

    /// Called for a `null` literal.
    fn scalar_null(&mut self) -> i32 {
        PROCEED
    }

    /// Called for a `true` or `false` literal.
    fn scalar_bool(&mut self, _value: bool) -> i32 {
        PROCEED
    }

    /// Called for a number literal.  `raw` is the unparsed source text.
    fn scalar_number(&mut self, _raw: RawStr<'_>) -> i32 {
        PROCEED
    }

    /// Called for a string literal.
    fn scalar_string(&mut self, _s: StrInfo<'_>) -> i32 {
        PROCEED
    }
}

/// A no-op reactor for validate-only parsing.
///
/// Every hook returns [`PROCEED`], so the parser runs through the entire input
/// checking syntax without producing any output.  This is the Rust equivalent
/// of passing a `NULL` reactor pointer to the C `ndec_ctx_init`.
pub struct NullReactor;

impl Reactor for NullReactor {}
