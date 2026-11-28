// Translated from C to Rust.
// Copyright 2018 Ulf Adams (Apache-2.0 / BSL-1.0)

#[inline]
pub fn decimal_length9(v: u32) -> u32 {
    debug_assert!(v < 1000000000);
    if v >= 100000000 {
        9
    } else if v >= 10000000 {
        8
    } else if v >= 1000000 {
        7
    } else if v >= 100000 {
        6
    } else if v >= 10000 {
        5
    } else if v >= 1000 {
        4
    } else if v >= 100 {
        3
    } else if v >= 10 {
        2
    } else {
        1
    }
}

#[inline]
pub fn decimal_length17(v: u64) -> u32 {
    debug_assert!(v < 100000000000000000);
    if v >= 10000000000000000 {
        17
    } else if v >= 1000000000000000 {
        16
    } else if v >= 100000000000000 {
        15
    } else if v >= 10000000000000 {
        14
    } else if v >= 1000000000000 {
        13
    } else if v >= 100000000000 {
        12
    } else if v >= 10000000000 {
        11
    } else if v >= 1000000000 {
        10
    } else if v >= 100000000 {
        9
    } else if v >= 10000000 {
        8
    } else if v >= 1000000 {
        7
    } else if v >= 100000 {
        6
    } else if v >= 10000 {
        5
    } else if v >= 1000 {
        4
    } else if v >= 100 {
        3
    } else if v >= 10 {
        2
    } else {
        1
    }
}

#[inline]
pub fn pow5bits(e: i32) -> i32 {
    debug_assert!(e >= 0);
    debug_assert!(e <= 3528);
    (((e as u32 * 1217359) >> 19) + 1) as i32
}

#[inline]
pub fn log10_pow2(e: i32) -> u32 {
    debug_assert!(e >= 0);
    debug_assert!(e <= 1650);
    (e as u32 * 78913) >> 18
}

#[inline]
pub fn log10_pow5(e: i32) -> u32 {
    debug_assert!(e >= 0);
    debug_assert!(e <= 2620);
    (e as u32 * 732923) >> 20
}
