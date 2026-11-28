//! Safe integer-to-decimal core: the part of `num::itoa` that is extracted to Lean and proved.
//!
//! No raw pointers, no escape hatches from the borrow checker, no traits, no iterators, no allocation: only fixed-size array
//! indexing, integer arithmetic and `if`/`while`, which is the subset Charon and Aeneas translate.
//! Every function writes into a 40-byte buffer, right-aligned (the last digit is always at index
//! 39), and returns the index of the first digit. Bytes before that index are scratch (leading
//! zeros of the chunk that was written) and are never part of the output.
//!
//! A faster SWAR-multiply variant of the 8-digit chunk exists (`bench/src/itoa_ladder.rs`, rung L3) and is
//! not used here: its bit-lane arithmetic is not yet proved without `Lean.ofReduceBool`.
//!
//! Layout trick: the digit count `n` is computed first, then the value is written by the narrowest
//! of four fixed-width paths (4, 8, 16 or 20 digits). Every store has a constant index, so there is
//! no bounds check and no dependent offset arithmetic on the hot path.
//!
//! Proved in `proofs/lean` (see `proofs/README.md`).

/// Length of the buffer every function here writes into.
pub const BUF_LEN: usize = 40;

/// The decimal digit pair of every number 0..=99, flat: `PAIRS[2 * n]` and `PAIRS[2 * n + 1]` are the two
/// ASCII bytes of `n`. (Flat, not `[[u8; 2]; 100]`: Aeneas turns a nested literal into a Lean term whose
/// elaboration exhausts memory, a flat one elaborates in seconds.)
pub const PAIRS: [u8; 200] = [
    b'0', b'0', b'0', b'1', b'0', b'2', b'0', b'3', b'0', b'4', b'0', b'5', b'0', b'6', b'0', b'7',
    b'0', b'8', b'0', b'9', b'1', b'0', b'1', b'1', b'1', b'2', b'1', b'3', b'1', b'4', b'1', b'5',
    b'1', b'6', b'1', b'7', b'1', b'8', b'1', b'9', b'2', b'0', b'2', b'1', b'2', b'2', b'2', b'3',
    b'2', b'4', b'2', b'5', b'2', b'6', b'2', b'7', b'2', b'8', b'2', b'9', b'3', b'0', b'3', b'1',
    b'3', b'2', b'3', b'3', b'3', b'4', b'3', b'5', b'3', b'6', b'3', b'7', b'3', b'8', b'3', b'9',
    b'4', b'0', b'4', b'1', b'4', b'2', b'4', b'3', b'4', b'4', b'4', b'5', b'4', b'6', b'4', b'7',
    b'4', b'8', b'4', b'9', b'5', b'0', b'5', b'1', b'5', b'2', b'5', b'3', b'5', b'4', b'5', b'5',
    b'5', b'6', b'5', b'7', b'5', b'8', b'5', b'9', b'6', b'0', b'6', b'1', b'6', b'2', b'6', b'3',
    b'6', b'4', b'6', b'5', b'6', b'6', b'6', b'7', b'6', b'8', b'6', b'9', b'7', b'0', b'7', b'1',
    b'7', b'2', b'7', b'3', b'7', b'4', b'7', b'5', b'7', b'6', b'7', b'7', b'7', b'8', b'7', b'9',
    b'8', b'0', b'8', b'1', b'8', b'2', b'8', b'3', b'8', b'4', b'8', b'5', b'8', b'6', b'8', b'7',
    b'8', b'8', b'8', b'9', b'9', b'0', b'9', b'1', b'9', b'2', b'9', b'3', b'9', b'4', b'9', b'5',
    b'9', b'6', b'9', b'7', b'9', b'8', b'9', b'9',
];

/// `POW10[i] = 10^i` for every power that fits in a `u64`.
pub const POW10: [u64; 20] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
    10_000_000_000_000_000_000,
];

/// Number of decimal digits of `v` (1 for 0). Always in 1..=20.
#[inline(always)]
pub fn digit_count(v: u64) -> usize {
    // x = max(v, 1); bits = floor(log2 x) + 1 is in 1..=64. 1233/4096 approximates log10(2) from
    // below closely enough that t = floor(bits * 1233 / 4096) is floor(log10 x) or one more, and the
    // table comparison corrects the second case.
    let x = if v == 0 { 1 } else { v };
    let bits = 64 - x.leading_zeros() as usize;
    let t = (bits * 1233) >> 12;
    t + 1 - (x < POW10[t]) as usize
}

/// Write the four digits of `v` (which must be < 10^4) at `buf[at..at + 4]`, zero padded.
#[inline(always)]
fn put4(buf: &mut [u8; BUF_LEN], at: usize, v: u32) {
    let hi = ((v / 100) as usize) * 2;
    let lo = ((v % 100) as usize) * 2;
    buf[at] = PAIRS[hi];
    buf[at + 1] = PAIRS[hi + 1];
    buf[at + 2] = PAIRS[lo];
    buf[at + 3] = PAIRS[lo + 1];
}

/// Write the eight digits of `v` (which must be < 10^8) at `buf[at..at + 8]`, zero padded.
#[inline(always)]
fn put8(buf: &mut [u8; BUF_LEN], at: usize, v: u32) {
    put4(buf, at, v / 10_000);
    put4(buf, at + 4, v % 10_000);
}

/// Write the sixteen digits of `v` (which must be < 10^16) at `buf[at..at + 16]`, zero padded.
#[inline(always)]
fn put16(buf: &mut [u8; BUF_LEN], at: usize, v: u64) {
    let hi = (v / 100_000_000) as u32;
    let lo = (v % 100_000_000) as u32;
    put8(buf, at, hi);
    put8(buf, at + 8, lo);
}

/// Format `v` right-aligned into `buf`; returns the index of its first digit.
///
/// Postcondition (proved): `buf[r..40]` are the decimal digits of `v`, most significant first,
/// with no leading zero unless `v == 0`.
#[inline]
pub fn fmt_u64(buf: &mut [u8; BUF_LEN], v: u64) -> usize {
    let n = digit_count(v);
    if v < 10_000 {
        put4(buf, 36, v as u32);
    } else if v < 100_000_000 {
        put8(buf, 32, v as u32);
    } else if v < 10_000_000_000_000_000 {
        put16(buf, 24, v);
    } else {
        let rest = v % 10_000_000_000_000_000;
        let top = (v / 10_000_000_000_000_000) as u32;
        put4(buf, 20, top);
        put16(buf, 24, rest);
    }
    BUF_LEN - n
}

/// Format `v` right-aligned into `buf`; returns the index of its first digit.
#[inline]
pub fn fmt_u32(buf: &mut [u8; BUF_LEN], v: u32) -> usize {
    fmt_u64(buf, v as u64)
}

/// Format `v` in decimal with a leading `-` when negative; returns the index of the first byte.
///
/// `i64::MIN` is handled: the magnitude is `-(v + 1) + 1`, computed in `u64` after the negation, so
/// nothing overflows. (`unsigned_abs` would be an opaque external in the extraction.)
#[inline]
pub fn fmt_i64(buf: &mut [u8; BUF_LEN], v: i64) -> usize {
    let mag = if v < 0 {
        ((-(v + 1)) as u64) + 1
    } else {
        v as u64
    };
    let start = fmt_u64(buf, mag);
    if v < 0 {
        buf[start - 1] = b'-';
        start - 1
    } else {
        start
    }
}

/// Write the 19 digits of `r` (which must be < 10^19) ending just before `end` (the exclusive end
/// index, at least 20). The byte at `end - 20` is also overwritten with a scratch zero, so the
/// caller writes the chunks in increasing significance and the next chunk overwrites it.
#[inline(always)]
fn put19(buf: &mut [u8; BUF_LEN], end: usize, r: u64) {
    put4(buf, end - 20, (r / 10_000_000_000_000_000) as u32);
    let rest = r % 10_000_000_000_000_000;
    put8(buf, end - 16, (rest / 100_000_000) as u32);
    put8(buf, end - 8, (rest % 100_000_000) as u32);
}

/// Format a `u128` right-aligned into `buf`; returns the index of its first digit.
/// Values that fit in `u64` take the `u64` path.
#[inline]
pub fn fmt_u128(buf: &mut [u8; BUF_LEN], v: u128) -> usize {
    if v <= u64::MAX as u128 {
        return fmt_u64(buf, v as u64);
    }
    const E19: u128 = 10_000_000_000_000_000_000;
    let q = v / E19; // < 2^128 / 10^19 < 3.5 * 10^19, so q needs one more split
    put19(buf, 40, (v - q * E19) as u64);
    let top = q / E19; // <= 34
    put19(buf, 21, (q - top * E19) as u64);
    if top == 0 {
        // q < 10^19 fits in a u64; its digits are the high part
        return 21 - digit_count((q - top * E19) as u64);
    }
    let pair = (top as usize) * 2;
    buf[0] = PAIRS[pair];
    buf[1] = PAIRS[pair + 1];
    if top < 10 {
        1
    } else {
        0
    }
}

/// Format an `i128` with a leading `-` when negative; returns the index of the first byte.
#[inline]
pub fn fmt_i128(buf: &mut [u8; BUF_LEN], v: i128) -> usize {
    let start = fmt_u128(buf, v.unsigned_abs());
    if v < 0 {
        buf[start - 1] = b'-';
        start - 1
    } else {
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: u64) -> String {
        let mut b = [0u8; BUF_LEN];
        let i = fmt_u64(&mut b, v);
        String::from_utf8(b[i..].to_vec()).unwrap()
    }

    #[test]
    fn digit_count_matches_std() {
        for v in 0..2_000_000u64 {
            assert_eq!(digit_count(v), v.to_string().len(), "{v}");
        }
        for e in 0..20 {
            let p = POW10[e];
            for v in [p.wrapping_sub(1), p, p + 1] {
                if e == 0 && v == u64::MAX {
                    continue;
                }
                assert_eq!(digit_count(v), v.to_string().len(), "{v}");
            }
        }
        assert_eq!(digit_count(u64::MAX), 20);
    }

    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn s128(v: u128) -> String {
        let mut b = [0u8; BUF_LEN];
        let i = fmt_u128(&mut b, v);
        String::from_utf8(b[i..].to_vec()).unwrap()
    }

    fn si64(v: i64) -> String {
        let mut b = [0u8; BUF_LEN];
        let i = fmt_i64(&mut b, v);
        String::from_utf8(b[i..].to_vec()).unwrap()
    }

    #[test]
    fn u64_boundaries_and_random() {
        for e in 0..20 {
            let p = POW10[e];
            for v in [p - 1, p, p + 1] {
                assert_eq!(s(v), v.to_string());
            }
        }
        for v in [
            u64::MAX,
            u64::MAX - 1,
            u64::MAX / 2,
            1 << 63,
            99_999_999,
            100_000_000,
            9_999,
            10_000,
        ] {
            assert_eq!(s(v), v.to_string());
        }
        let mut st = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..3_000_000 {
            let v = rng(&mut st) >> (rng(&mut st) % 64);
            assert_eq!(s(v), v.to_string(), "{v}");
        }
    }

    #[test]
    fn i64_all_edges() {
        for v in [
            i64::MIN,
            i64::MIN + 1,
            -1,
            0,
            1,
            i64::MAX,
            -10,
            -99,
            -100,
            -9_999_999_999_999_999_999i128 as i64,
        ] {
            assert_eq!(si64(v), v.to_string());
        }
        let mut st = 0xD1B5_4A32_D192_ED03u64;
        for _ in 0..2_000_000 {
            let v = (rng(&mut st) as i64) >> (rng(&mut st) % 64);
            assert_eq!(si64(v), v.to_string(), "{v}");
        }
    }

    #[test]
    fn u128_boundaries_and_random() {
        for v in [
            u128::MAX,
            u128::MAX - 1,
            u64::MAX as u128,
            u64::MAX as u128 + 1,
            1u128 << 64,
            1u128 << 127,
        ] {
            assert_eq!(s128(v), v.to_string());
        }
        let mut p: u128 = 1;
        while let Some(n) = p.checked_mul(10) {
            for v in [p - 1, p, p + 1] {
                assert_eq!(s128(v), v.to_string(), "{v}");
            }
            p = n;
        }
        let mut st = 0xA076_1D64_78BD_642Fu64;
        for _ in 0..1_000_000 {
            let v = (((rng(&mut st) as u128) << 64) | rng(&mut st) as u128) >> (rng(&mut st) % 128);
            assert_eq!(s128(v), v.to_string(), "{v}");
        }
    }

    #[test]
    fn fmt_matches_std_small() {
        for v in 0..2_000_000u64 {
            assert_eq!(s(v), v.to_string(), "{v}");
        }
    }
}
