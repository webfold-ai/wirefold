//! Extraction root for Charon and Aeneas.
//!
//! The module below is the real `src/num/itoa_core.rs`, included by `#[path]`, so the extracted
//! definitions are the ones the library builds. The wrappers only fix concrete types and give
//! Charon named roots; they restate no formula.
//!
//! Regenerate and check with `cargo run --manifest-path proofs/runner/Cargo.toml -- regen`.

#[path = "../../src/num/itoa_core.rs"]
pub mod itoa_core;

pub fn w_digit_count(v: u64) -> usize {
    itoa_core::digit_count(v)
}

pub fn w_fmt_u64(buf: &mut [u8; 40], v: u64) -> usize {
    itoa_core::fmt_u64(buf, v)
}

pub fn w_fmt_u32(buf: &mut [u8; 40], v: u32) -> usize {
    itoa_core::fmt_u32(buf, v)
}

pub fn w_fmt_i64(buf: &mut [u8; 40], v: i64) -> usize {
    itoa_core::fmt_i64(buf, v)
}

// The float core: `float_core` reaches its table through `super::float_tables`, so both are declared at
// the crate root, where `super` of `float_core` is.
#[path = "../../src/num/float_tables.rs"]
pub mod float_tables;
#[path = "../../src/num/float_core.rs"]
pub mod float_core;

pub fn w_rop(ghi: u64, glo: u64, cp: u64) -> u64 {
    float_core::rop(ghi, glo, cp)
}

pub fn w_flog10_pow2(q: i32) -> i32 {
    float_core::flog10_pow2(q)
}

pub fn w_flog10_three_quarters_pow2(q: i32) -> i32 {
    float_core::flog10_three_quarters_pow2(q)
}

pub fn w_flog2_pow10(e: i32) -> i32 {
    float_core::flog2_pow10(e)
}

pub fn w_strip_zeros(m: u64, e: i32) -> (u64, i32) {
    float_core::strip_zeros(m, e)
}

pub fn w_dec_f64(mant: u64, exp: u32, tie_even: bool) -> (u64, i32) {
    float_core::dec_f64(mant, exp, tie_even)
}

pub fn w_dec_f32(mant: u32, exp: u32, tie_even: bool) -> (u64, i32) {
    float_core::dec_f32(mant, exp, tie_even)
}
