//! Zero-dependency number formatters built for wirefold.
//!
//! Integers: `itoa`-class decimal formatting from a safe, formally verified core (`itoa_core`).
//! Floats: shortest round-trip formatting from a safe core (`float_core`) with the exact-arithmetic
//! lemmas proved and the shortest-and-closest property evidenced by an exhaustive `f32` sweep.
//! Zero heap allocation in the formatters.

#![forbid(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::all)]
#![allow(
    clippy::needless_pass_by_value,
    clippy::range_plus_one,
    clippy::bool_to_int_with_if,
    clippy::nonminimal_bool,
    clippy::needless_bitwise_bool
)]

pub mod float;
pub mod float_core;
pub mod float_tables;
pub mod itoa;
pub mod itoa_core;

pub use float::{
    append_float, append_float_display, float_debug_string, float_display_string, float_to_string,
    Float, FloatBuffer,
};
pub use itoa::{append_int, int_to_string, IntBuffer, Integer};
