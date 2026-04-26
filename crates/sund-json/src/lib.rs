//! sund-json: Suspendable JSON parser with SIMD structural scanning.
//!
//! This crate provides a zero-allocation, suspendable JSON parser
//! with SIMD structural scanning and a reactor-style callback interface.

pub mod parser;
pub mod reactor;
pub mod scalar;
pub mod scanner;
pub mod types;

pub mod bind;
pub mod stream;

pub use bind::{
    arena::Arena,
    number::parse_double,
    type_info::{Field, Kind, TypeInfo},
    unmarshal::{unmarshal, unmarshal_ex, UnmarshalError, UnmarshalOpts},
};
pub use reactor::*;
pub use stream::{Stream, StreamStatus};
pub use types::*;
