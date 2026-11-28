//! Zero-dependency number formatters built for wirefold from an apeiron basis.
//!
//! Provides fast integer formatting (`itoa`-class, 2-digit lookup table with 16-bit stores and
//! small-integer fast paths) and shortest round-trip float formatting (`Ryu`-class, static power-of-5
//! split tables, bit-exact round-trip). Zero heap allocation.

#![allow(missing_docs)]
#![allow(clippy::all)]

pub mod itoa;
pub mod ryu;

pub use itoa::{
    append_int, int_to_string, write_i128, write_i64, write_u128, write_u32, write_u64, IntBuffer,
    Integer,
};
pub use ryu::{append_float, float_to_string, Float, FloatBuffer};
