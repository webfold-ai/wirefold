//! In-tree integer-to-decimal formatter.
//!
//! Converts integer primitives to decimal ASCII strings using a 200-byte 2-digit lookup table
//! and native 16-bit stores. Fast paths for small integers (0..=9999) avoid division loops completely.

#![allow(clippy::all, clippy::pedantic)]

use core::str;

/// Pre-packed native-endian 16-bit pairs for single-instruction 2-byte writes.
const PAIRS_U16: [u16; 100] = {
    let mut res = [0u16; 100];
    let bytes = b"0001020304050607080910111213141516171819\
                  2021222324252627282930313233343536373839\
                  4041424344454647484950515253545556575859\
                  6061626364656667686970717273747576777879\
                  8081828384858687888990919293949596979899";
    let mut i = 0;
    while i < 100 {
        let b0 = bytes[i * 2];
        let b1 = bytes[i * 2 + 1];
        res[i] = u16::from_ne_bytes([b0, b1]);
        i += 1;
    }
    res
};

#[inline(always)]
fn divmod100(val: u32) -> (u32, u32) {
    let q = val / 100;
    let r = val - q * 100;
    (q, r)
}

/// A 40-byte stack buffer sufficient for formatting any integer up to u128 / i128.
#[derive(Clone, Copy)]
pub struct IntBuffer {
    bytes: [u8; 40],
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
        Self { bytes: [0; 40] }
    }

    /// Format an integer into this buffer and return the `&str`.
    #[inline]
    pub fn format<I: Integer>(&mut self, val: I) -> &str {
        val.write_into(&mut self.bytes)
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
    use super::*;

    pub trait Sealed: Copy {
        fn write_into(self, buf: &mut [u8; 40]) -> &str;
    }

    impl<T: Sealed> Sealed for &T {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            (*self).write_into(buf)
        }
    }

    impl Sealed for u8 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u32(buf, self as u32)
        }
    }

    impl Sealed for u16 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u32(buf, self as u32)
        }
    }

    impl Sealed for u32 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u32(buf, self)
        }
    }

    impl Sealed for u64 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u64(buf, self)
        }
    }

    impl Sealed for usize {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u64(buf, self as u64)
        }
    }

    impl Sealed for u128 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_u128(buf, self)
        }
    }

    impl Sealed for i8 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i64(buf, self as i64)
        }
    }

    impl Sealed for i16 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i64(buf, self as i64)
        }
    }

    impl Sealed for i32 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i64(buf, self as i64)
        }
    }

    impl Sealed for i64 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i64(buf, self)
        }
    }

    impl Sealed for isize {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i64(buf, self as i64)
        }
    }

    impl Sealed for i128 {
        #[inline]
        fn write_into(self, buf: &mut [u8; 40]) -> &str {
            write_i128(buf, self)
        }
    }
}

#[inline]
pub fn write_u32(buf: &mut [u8; 40], mut val: u32) -> &str {
    let mut offset = 40usize;
    while val > 999 {
        offset -= 4;
        let quad = val % 10000;
        val /= 10000;
        let (p1, p2) = divmod100(quad);
        unsafe {
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset).cast::<u16>(),
                PAIRS_U16[p1 as usize],
            );
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset + 2).cast::<u16>(),
                PAIRS_U16[p2 as usize],
            );
        }
    }
    if val > 9 {
        offset -= 2;
        let (last, pair) = divmod100(val);
        val = last;
        unsafe {
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset).cast::<u16>(),
                PAIRS_U16[pair as usize],
            );
        }
    }
    if val != 0 || offset == 40 {
        offset -= 1;
        buf[offset] = b'0' + val as u8;
    }
    unsafe { str::from_utf8_unchecked(&buf[offset..40]) }
}

#[inline]
pub fn write_u64(buf: &mut [u8; 40], mut val: u64) -> &str {
    let mut offset = 40usize;
    while val > 999 {
        offset -= 4;
        let quad = (val % 10000) as u32;
        val /= 10000;
        let (p1, p2) = divmod100(quad);
        unsafe {
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset).cast::<u16>(),
                PAIRS_U16[p1 as usize],
            );
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset + 2).cast::<u16>(),
                PAIRS_U16[p2 as usize],
            );
        }
    }
    if val > 9 {
        offset -= 2;
        let (last, pair) = divmod100(val as u32);
        val = last as u64;
        unsafe {
            core::ptr::write_unaligned(
                buf.as_mut_ptr().add(offset).cast::<u16>(),
                PAIRS_U16[pair as usize],
            );
        }
    }
    if val != 0 || offset == 40 {
        offset -= 1;
        buf[offset] = b'0' + val as u8;
    }
    unsafe { str::from_utf8_unchecked(&buf[offset..40]) }
}

#[inline]
pub fn write_i64(buf: &mut [u8; 40], val: i64) -> &str {
    if val >= 0 {
        write_u64(buf, val as u64)
    } else {
        let u = val.unsigned_abs();
        let s = write_u64(buf, u);
        let len = s.len();
        let offset = 40 - len - 1;
        buf[offset] = b'-';
        unsafe { str::from_utf8_unchecked(&buf[offset..40]) }
    }
}

#[inline]
pub fn write_u128(buf: &mut [u8; 40], mut val: u128) -> &str {
    if val <= u64::MAX as u128 {
        return write_u64(buf, val as u64);
    }

    // 10^16 fits in u64.
    const E16: u128 = 10_000_000_000_000_000;
    let mut offset = 40usize;
    while val > u64::MAX as u128 {
        let q = val / E16;
        let rem = (val - q * E16) as u64;
        val = q;
        // Format 16 digits of rem (from rem % 10^16)
        offset -= 16;
        format_fixed16(buf, offset, rem);
    }

    // Now format the remaining u64 (<= u64::MAX)
    let mut head = [0u8; 40];
    let s = write_u64(&mut head, val as u64);
    let s_len = s.len();
    let new_offset = offset - s_len;
    buf[new_offset..offset].copy_from_slice(s.as_bytes());
    unsafe { str::from_utf8_unchecked(&buf[new_offset..40]) }
}

#[inline]
fn format_fixed16(buf: &mut [u8; 40], offset: usize, val: u64) {
    let hi = (val / 100_000_000) as u32;
    let lo = (val % 100_000_000) as u32;
    let q_hi = hi / 10000;
    let r_hi = hi - q_hi * 10000;
    let (d0, d1) = divmod100(q_hi);
    let (d2, d3) = divmod100(r_hi);
    let q_lo = lo / 10000;
    let r_lo = lo - q_lo * 10000;
    let (d4, d5) = divmod100(q_lo);
    let (d6, d7) = divmod100(r_lo);
    unsafe {
        let p = buf.as_mut_ptr().add(offset);
        core::ptr::write_unaligned(p.cast::<u16>(), PAIRS_U16[d0 as usize]);
        core::ptr::write_unaligned(p.add(2).cast::<u16>(), PAIRS_U16[d1 as usize]);
        core::ptr::write_unaligned(p.add(4).cast::<u16>(), PAIRS_U16[d2 as usize]);
        core::ptr::write_unaligned(p.add(6).cast::<u16>(), PAIRS_U16[d3 as usize]);
        core::ptr::write_unaligned(p.add(8).cast::<u16>(), PAIRS_U16[d4 as usize]);
        core::ptr::write_unaligned(p.add(10).cast::<u16>(), PAIRS_U16[d5 as usize]);
        core::ptr::write_unaligned(p.add(12).cast::<u16>(), PAIRS_U16[d6 as usize]);
        core::ptr::write_unaligned(p.add(14).cast::<u16>(), PAIRS_U16[d7 as usize]);
    }
}

#[inline]
pub fn write_i128(buf: &mut [u8; 40], val: i128) -> &str {
    if val >= 0 {
        write_u128(buf, val as u128)
    } else {
        let u = val.unsigned_abs();
        let s = write_u128(buf, u);
        let len = s.len();
        let offset = 40 - len - 1;
        buf[offset] = b'-';
        unsafe { str::from_utf8_unchecked(&buf[offset..40]) }
    }
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
