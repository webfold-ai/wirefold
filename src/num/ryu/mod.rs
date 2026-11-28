//! In-tree Ryu float-to-decimal formatter.
//!
//! Generates the shortest decimal representation that round-trips back to the exact
//! IEEE 754 bit pattern when parsed with `str::parse`. Zero heap allocation.

#![allow(clippy::all, clippy::pedantic)]

pub mod common;
pub mod d2s;
pub mod d2s_full_table;
pub mod d2s_intrinsics;
pub mod digit_table;
pub mod f2s;
pub mod f2s_intrinsics;
pub mod pretty;

use core::str;

const NAN: &str = "NaN";
const INFINITY: &str = "inf";
const NEG_INFINITY: &str = "-inf";

/// A stack buffer of 24 bytes, sufficient for any formatted f32 or f64.
#[derive(Clone, Copy)]
pub struct FloatBuffer {
    bytes: [u8; 24],
}

impl Default for FloatBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl FloatBuffer {
    /// Create a new, reusable float buffer.
    #[inline]
    pub fn new() -> Self {
        Self { bytes: [0; 24] }
    }

    /// Format any float (handling NaN, inf, -inf, subnormals, 0.0, -0.0).
    #[inline]
    pub fn format<F: Float>(&mut self, f: F) -> &str {
        if f.is_nonfinite() {
            f.format_nonfinite()
        } else {
            self.format_finite(f)
        }
    }

    /// Format a finite float into this buffer.
    #[inline]
    pub fn format_finite<F: Float>(&mut self, f: F) -> &str {
        unsafe {
            let n = f.write_to_buffer(self.bytes.as_mut_ptr());
            debug_assert!(n <= self.bytes.len());
            str::from_utf8_unchecked(&self.bytes[..n])
        }
    }
}

/// Sealed trait for floating point types (f32, f64).
pub trait Float: Copy + private::Sealed {}

impl Float for f32 {}
impl Float for f64 {}

mod private {
    use super::*;

    pub trait Sealed: Copy {
        fn is_nonfinite(self) -> bool;
        fn format_nonfinite(self) -> &'static str;
        unsafe fn write_to_buffer(self, result: *mut u8) -> usize;
    }

    impl Sealed for f32 {
        #[inline]
        fn is_nonfinite(self) -> bool {
            const EXP_MASK: u32 = 0x7f80_0000;
            let bits = self.to_bits();
            bits & EXP_MASK == EXP_MASK
        }

        #[cold]
        #[inline]
        fn format_nonfinite(self) -> &'static str {
            const MANTISSA_MASK: u32 = 0x007f_ffff;
            const SIGN_MASK: u32 = 0x8000_0000;
            let bits = self.to_bits();
            if bits & MANTISSA_MASK != 0 {
                NAN
            } else if bits & SIGN_MASK != 0 {
                NEG_INFINITY
            } else {
                INFINITY
            }
        }

        #[inline]
        unsafe fn write_to_buffer(self, result: *mut u8) -> usize {
            pretty::format32(self, result)
        }
    }

    impl Sealed for f64 {
        #[inline]
        fn is_nonfinite(self) -> bool {
            const EXP_MASK: u64 = 0x7ff0_0000_0000_0000;
            let bits = self.to_bits();
            bits & EXP_MASK == EXP_MASK
        }

        #[cold]
        #[inline]
        fn format_nonfinite(self) -> &'static str {
            const MANTISSA_MASK: u64 = 0x000f_ffff_ffff_ffff;
            const SIGN_MASK: u64 = 0x8000_0000_0000_0000;
            let bits = self.to_bits();
            if bits & MANTISSA_MASK != 0 {
                NAN
            } else if bits & SIGN_MASK != 0 {
                NEG_INFINITY
            } else {
                INFINITY
            }
        }

        #[inline]
        unsafe fn write_to_buffer(self, result: *mut u8) -> usize {
            pretty::format64(self, result)
        }
    }
}

/// Format a float directly to an allocated `String`.
#[inline]
pub fn float_to_string<F: Float>(f: F) -> String {
    let mut buf = FloatBuffer::new();
    buf.format(f).to_string()
}

/// Append a formatted float to an existing `String`.
#[inline]
pub fn append_float<F: Float>(out: &mut String, f: F) {
    let mut buf = FloatBuffer::new();
    out.push_str(buf.format(f));
}
