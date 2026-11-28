//! In-tree integer-to-decimal formatter.
//!
//! The digit work is `num::itoa_core`: safe Rust, extracted to Lean with Charon and Aeneas and
//! proved there (`proofs/`). This file is the thin, unproved layer on top of it: the sealed
//! `Integer` trait, the reusable `IntBuffer`, and the conversion of the written bytes to `&str`
//! with the checked `core::str::from_utf8`. There is no unchecked variant and no feature flag for
//! one: the whole `num` module is `forbid(unsafe_code)`. Callers that do not need a `&str` can use
//! `IntBuffer::format_bytes`, which skips the conversion.

use super::itoa_core as core_;

/// A 40-byte stack buffer sufficient for formatting any integer up to u128 / i128.
#[derive(Clone, Copy)]
pub struct IntBuffer {
    bytes: [u8; core_::BUF_LEN],
}

impl Default for IntBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl IntBuffer {
    /// Create a new, reusable integer buffer.
    #[inline]
    pub fn new() -> Self {
        Self {
            bytes: [0; core_::BUF_LEN],
        }
    }

    /// Format an integer into this buffer and return the ASCII bytes (no UTF-8 check).
    #[inline]
    pub fn format_bytes<I: Integer>(&mut self, val: I) -> &[u8] {
        let start = val.write_into(&mut self.bytes);
        &self.bytes[start..]
    }

    /// Format an integer into this buffer and return the `&str`.
    #[inline]
    pub fn format<I: Integer>(&mut self, val: I) -> &str {
        match core::str::from_utf8(self.format_bytes(val)) {
            Ok(s) => s,
            Err(_) => "",
        }
    }
}

/// Sealed trait for integer primitives.
pub trait Integer: Copy + private::Sealed {}

macro_rules! impl_int {
    ($($t:ty),*) => {
        $(
            impl Integer for $t {}
        )*
    };
}

impl_int!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

impl<T: Integer> Integer for &T {}

mod private {
    use super::core_;

    pub trait Sealed: Copy {
        /// Write `self` right-aligned into `buf`; the index of the first byte.
        fn write_into(self, buf: &mut [u8; core_::BUF_LEN]) -> usize;
    }

    impl<T: Sealed> Sealed for &T {
        #[inline]
        fn write_into(self, buf: &mut [u8; core_::BUF_LEN]) -> usize {
            (*self).write_into(buf)
        }
    }

    macro_rules! formatted {
        ($($t:ty => $f:ident),*) => {$(
            impl Sealed for $t {
                #[inline]
                fn write_into(self, buf: &mut [u8; core_::BUF_LEN]) -> usize {
                    core_::$f(buf, self as _)
                }
            }
        )*};
    }
    formatted!(u8 => fmt_u64, u16 => fmt_u64, u32 => fmt_u64, u64 => fmt_u64, usize => fmt_u64, u128 => fmt_u128);
    formatted!(i8 => fmt_i64, i16 => fmt_i64, i32 => fmt_i64, i64 => fmt_i64, isize => fmt_i64, i128 => fmt_i128);
}

/// Format an integer directly to an allocated `String`.
#[inline]
pub fn int_to_string<I: Integer>(val: I) -> String {
    let mut buf = IntBuffer::new();
    buf.format(val).to_string()
}

/// Append a formatted integer to an existing `String`.
#[inline]
pub fn append_int<I: Integer>(out: &mut String, val: I) {
    let mut buf = IntBuffer::new();
    out.push_str(buf.format(val));
}
