//! Shortest round-trip float formatting on top of the safe core `num::float_core`.
//!
//! Two layouts of the same shortest digits:
//! - `FloatBuffer::format` / `float_to_string`: the `ryu` layout (`1.0`, `0.1`, `1e21`, `1.234e-7`),
//!   exponent notation outside `1e-5 <= |x| < 1e16` (`f32`: `1e-6`, `1e13`). Identical bytes to the
//!   `ryu` crate (checked exhaustively for `f32`, by differential test for `f64`).
//! - `FloatBuffer::format_display` / `float_display_string`: the layout of `core::fmt::Display`
//!   (`1`, `0.1`, `1000000000000000000000`, `0.0000001`, `-0`), identical to `x.to_string()`.
//!
//! Everything here is safe Rust. Digit generation is `float_core::shortest`; digits are written with
//! the proved integer writer `itoa_core::fmt_u64`.

use super::float_core;
use super::itoa_core;

const NAN: &str = "NaN";
const INF: &str = "inf";
const NEG_INF: &str = "-inf";

/// Output scratch: the longest `ryu`-layout text is 24 bytes; the rest is slack for constant-size copies.
const OUT_LEN: usize = 64;

/// A reusable stack buffer for formatting `f32` and `f64` in the `ryu` layout.
#[derive(Clone, Copy)]
pub struct FloatBuffer {
    bytes: [u8; OUT_LEN],
}

impl Default for FloatBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Sealed trait for `f32` and `f64`.
pub trait Float: Copy + private::Sealed {}
impl Float for f32 {}
impl Float for f64 {}

/// Decoded finite float: sign, and `(m, e)` with no trailing zeros in `m` (or zero).
pub struct Parts {
    pub neg: bool,
    pub zero: bool,
    pub m: u64,
    pub e: i32,
    /// layout thresholds: positional while `kk <= hi`, small numbers positional while `kk > lo`
    pub hi: i32,
    pub lo: i32,
}

mod private {
    use super::*;

    pub trait Sealed: Copy {
        fn nonfinite(self) -> Option<&'static str>;
        fn parts(self, tie_even: bool) -> Parts;
    }

    impl Sealed for f64 {
        #[inline]
        fn nonfinite(self) -> Option<&'static str> {
            let bits = self.to_bits();
            if bits & 0x7ff0_0000_0000_0000 != 0x7ff0_0000_0000_0000 {
                None
            } else if bits & 0x000f_ffff_ffff_ffff != 0 {
                Some(NAN)
            } else if bits >> 63 != 0 {
                Some(NEG_INF)
            } else {
                Some(INF)
            }
        }
        #[inline]
        fn parts(self, tie_even: bool) -> Parts {
            let bits = self.to_bits();
            let mant = bits & ((1 << 52) - 1);
            let exp = ((bits >> 52) & 0x7ff) as u32;
            let neg = bits >> 63 != 0;
            if exp == 0 && mant == 0 {
                return Parts {
                    neg,
                    zero: true,
                    m: 0,
                    e: 0,
                    hi: 16,
                    lo: -5,
                };
            }
            let (m, e) = float_core::dec_f64(mant, exp, tie_even);
            Parts {
                neg,
                zero: false,
                m,
                e,
                hi: 16,
                lo: -5,
            }
        }
    }

    impl Sealed for f32 {
        #[inline]
        fn nonfinite(self) -> Option<&'static str> {
            let bits = self.to_bits();
            if bits & 0x7f80_0000 != 0x7f80_0000 {
                None
            } else if bits & 0x007f_ffff != 0 {
                Some(NAN)
            } else if bits >> 31 != 0 {
                Some(NEG_INF)
            } else {
                Some(INF)
            }
        }
        #[inline]
        fn parts(self, tie_even: bool) -> Parts {
            let bits = self.to_bits();
            let mant = bits & ((1 << 23) - 1);
            let exp = (bits >> 23) & 0xff;
            let neg = bits >> 31 != 0;
            if exp == 0 && mant == 0 {
                return Parts {
                    neg,
                    zero: true,
                    m: 0,
                    e: 0,
                    hi: 13,
                    lo: -6,
                };
            }
            let (m, e) = float_core::dec_f32(mant, exp, tie_even);
            Parts {
                neg,
                zero: false,
                m,
                e,
                hi: 13,
                lo: -6,
            }
        }
    }
}

#[inline(always)]
fn as_str(bytes: &[u8]) -> &str {
    match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => "",
    }
}

/// Write `n` (0..=999) in decimal at `out[at..]`, return the new index.
#[inline(always)]
fn put_exp(out: &mut [u8; OUT_LEN], mut at: usize, n: u32) -> usize {
    if n >= 100 {
        out[at] = b'0' + (n / 100) as u8;
        at += 1;
    }
    if n >= 10 {
        out[at] = b'0' + ((n / 10) % 10) as u8;
        at += 1;
    }
    out[at] = b'0' + (n % 10) as u8;
    at + 1
}

impl FloatBuffer {
    /// Create a new, reusable float buffer.
    #[inline]
    pub fn new() -> Self {
        Self {
            bytes: [0; OUT_LEN],
        }
    }

    /// Format any float in the `ryu` layout (`NaN`, `inf`, `-inf` included) and return the ASCII
    /// bytes, without the UTF-8 check.
    #[inline]
    pub fn format_bytes<F: Float>(&mut self, f: F) -> &[u8] {
        if let Some(s) = f.nonfinite() {
            return s.as_bytes();
        }
        let n = write_ryu(&mut self.bytes, f.parts(true));
        &self.bytes[..n]
    }

    /// Format any float in the layout of `core::fmt::Debug` (`1.0`, `1e16`, `1.5e-5`): positional for
    /// `1e-4 <= |x| < 1e16`, exponent notation outside. Identical bytes to `format!("{x:?}")`.
    #[inline]
    pub fn format_debug<F: Float>(&mut self, f: F) -> &str {
        if let Some(s) = f.nonfinite() {
            return s;
        }
        let mut p = f.parts(false);
        p.hi = 16;
        p.lo = -4;
        let n = write_ryu(&mut self.bytes, p);
        as_str(&self.bytes[..n])
    }

    /// Format any float in the `ryu` layout (`NaN`, `inf`, `-inf` included).
    #[inline]
    pub fn format<F: Float>(&mut self, f: F) -> &str {
        as_str(self.format_bytes(f))
    }

    /// Format a finite float in the `ryu` layout.
    #[inline]
    pub fn format_finite<F: Float>(&mut self, f: F) -> &str {
        self.format(f)
    }
}

/// Offset of the digit window inside the scratch buffer: digits are written right-aligned ending at
/// index `itoa_core::BUF_LEN` (40); the 32 bytes after that are slack so that constant-size copies
/// starting at the first digit never run off the end.
const SCRATCH_LEN: usize = itoa_core::BUF_LEN + 32;

/// The shortest digits of `m`: scratch buffer and index of the first digit.
#[inline(always)]
fn digits(m: u64) -> ([u8; SCRATCH_LEN], usize) {
    let mut tmp = [0u8; SCRATCH_LEN];
    let start = match tmp.first_chunk_mut::<{ itoa_core::BUF_LEN }>() {
        Some(head) => itoa_core::fmt_u64(head, m),
        None => itoa_core::BUF_LEN - 1,
    };
    (tmp, start)
}

const ZEROS16: [u8; 16] = [b'0'; 16];

fn write_ryu(out: &mut [u8; OUT_LEN], p: Parts) -> usize {
    out[0] = b'-';
    let i = p.neg as usize; // later writes overwrite the sign slot when positive
    if p.zero {
        out[i..i + 3].copy_from_slice(b"0.0");
        return i + 3;
    }
    let (tmp, start) = digits(p.m);
    let l = itoa_core::BUF_LEN - start;
    let kk = l as i32 + p.e; // 10^(kk-1) <= |x| < 10^kk
    if p.e >= 0 && kk <= p.hi {
        // 1234e2 -> 123400.0: digits, zero fill, ".0" (all constant-size copies)
        out[i..i + 16].copy_from_slice(&tmp[start..start + 16]);
        out[i + l..i + l + 16].copy_from_slice(&ZEROS16);
        let k = kk as usize;
        out[i + k] = b'.';
        out[i + k + 1] = b'0';
        i + k + 2
    } else if 0 < kk && kk <= p.hi {
        // 1234e-2 -> 12.34: copy the digits, slide the fraction one place right, drop the point in
        let k = kk as usize;
        out[i..i + 32].copy_from_slice(&tmp[start..start + 32]);
        out.copy_within(i + k..i + k + 32, i + k + 1);
        out[i + k] = b'.';
        i + l + 1
    } else if p.lo < kk && kk <= 0 {
        // 1234e-6 -> 0.001234
        out[i] = b'0';
        out[i + 1] = b'.';
        out[i + 2..i + 18].copy_from_slice(&ZEROS16);
        let off = i + 2 + (-kk) as usize;
        out[off..off + 32].copy_from_slice(&tmp[start..start + 32]);
        off + l
    } else {
        // 1e30, 1.234e33, 1.234e-7
        out[i] = tmp[start];
        let mut j = i + 1;
        if l > 1 {
            out[j] = b'.';
            out[j + 1..j + 33].copy_from_slice(&tmp[start + 1..start + 33]);
            j += l;
        }
        out[j] = b'e';
        j += 1;
        let x = kk - 1;
        if x < 0 {
            out[j] = b'-';
            j += 1;
        }
        put_exp(out, j, x.unsigned_abs())
    }
}

/// `"0"` repeated: the zero fill of the `Display` layout, pushed in one piece.
const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000\
                     0000000000000000000000000000000000000000000000000000000000000000\
                     0000000000000000000000000000000000000000000000000000000000000000\
                     0000000000000000000000000000000000000000000000000000000000000000\
                     0000000000000000000000000000000000000000000000000000000000000000";

#[inline(always)]
fn push_zeros(out: &mut String, mut n: usize) {
    while n > ZEROS.len() {
        out.push_str(ZEROS);
        n -= ZEROS.len();
    }
    out.push_str(&ZEROS[..n]);
}

/// Append the shortest round-trip text of `f` in the `core::fmt::Display` layout to `out`.
#[inline]
pub fn append_float_display<F: Float>(out: &mut String, f: F) {
    if let Some(s) = f.nonfinite() {
        out.push_str(s);
        return;
    }
    let p = f.parts(false);
    if p.zero {
        out.push_str(if p.neg { "-0" } else { "0" });
        return;
    }
    let (tmp, start) = digits(p.m);
    let l = itoa_core::BUF_LEN - start;
    let kk = l as i32 + p.e;
    let mut buf = [0u8; 40];
    buf[0] = b'-';
    let i = p.neg as usize;
    if p.e >= 0 {
        // digits then p.e zeros (can be 300+): push in pieces
        if p.neg {
            out.push('-');
        }
        out.push_str(as_str(&tmp[start..itoa_core::BUF_LEN]));
        push_zeros(out, p.e as usize);
    } else if kk > 0 {
        // 1234e-2 -> 12.34
        let k = kk as usize;
        buf[i..i + 32].copy_from_slice(&tmp[start..start + 32]);
        buf.copy_within(i + k..i + k + 16, i + k + 1);
        buf[i + k] = b'.';
        out.push_str(as_str(&buf[..i + l + 1]));
    } else {
        // 1234e-6 -> 0.001234; the zero fill is at most 324 - 17, so go through the string
        if p.neg {
            out.push('-');
        }
        out.push_str("0.");
        push_zeros(out, (-kk) as usize);
        out.push_str(as_str(&tmp[start..itoa_core::BUF_LEN]));
    }
}

/// The text of `f` exactly as `f.to_string()` (the `Display` layout), without `core::fmt`.
#[inline]
pub fn float_display_string<F: Float>(f: F) -> String {
    let mut s = String::with_capacity(24);
    append_float_display(&mut s, f);
    s
}

/// Format a float directly to an allocated `String`, `ryu` layout.
#[inline]
pub fn float_to_string<F: Float>(f: F) -> String {
    let mut buf = FloatBuffer::new();
    buf.format(f).to_string()
}

/// Append a formatted float to an existing `String`, `ryu` layout.
#[inline]
pub fn append_float<F: Float>(out: &mut String, f: F) {
    let mut buf = FloatBuffer::new();
    out.push_str(buf.format(f));
}

/// The text of `f` exactly as `format!("{f:?}")` (the `Debug` layout), without `core::fmt`.
#[inline]
pub fn float_debug_string<F: Float>(f: F) -> String {
    let mut buf = FloatBuffer::new();
    buf.format_debug(f).to_string()
}
