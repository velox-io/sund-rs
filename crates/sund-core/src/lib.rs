//! sund-core: Zero-allocation, suspendable JSON parsing kernel.
//!
//! This crate provides the low-level JSON parsing state machine with SIMD
//! structural scanning and a reactor-style callback interface.

pub mod kernel;
pub mod reactor;
pub mod scalar;
pub mod scanner;
pub mod types;

pub use reactor::*;
pub use types::*;
