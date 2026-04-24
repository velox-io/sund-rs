//! sund-bind: schema-driven JSON unmarshal on top of the sund parsing kernel.
//!
//! Ported from `ndec/impl/bind.h` and `ndec/impl/bind.c`.

pub mod arena;
pub mod number;
pub mod type_info;
pub mod unmarshal;

pub use arena::Arena;
pub use number::parse_double;
pub use type_info::{Field, Kind, TypeInfo};
pub use unmarshal::{unmarshal, unmarshal_ex, UnmarshalError, UnmarshalOpts};
