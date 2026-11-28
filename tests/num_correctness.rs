use wirefold::num::{FloatBuffer, IntBuffer};

#[test]
fn test_integers_exhaustive_small() {
    let mut ib = IntBuffer::new();
    let mut itoa_b = itoa::Buffer::new();

    for i in 0..=200_000u32 {
        let ours = ib.format(i);
        let expected = itoa_b.format(i);
        assert_eq!(ours, expected, "mismatch for u32 {i}");
        assert_eq!(ours, i.to_string());
    }

    for i in -100_000..=100_000i32 {
        let ours = ib.format(i);
        let expected = itoa_b.format(i);
        assert_eq!(ours, expected, "mismatch for i32 {i}");
        assert_eq!(ours, i.to_string());
    }
}

#[test]
fn test_integers_boundaries() {
    let mut ib = IntBuffer::new();
    let mut itoa_b = itoa::Buffer::new();

    macro_rules! check {
        ($val:expr) => {{
            let v = $val;
            let ours = ib.format(v);
            let expected = itoa_b.format(v);
            assert_eq!(ours, expected, "mismatch for {}", v);
            assert_eq!(ours, v.to_string(), "mismatch vs Display for {}", v);
        }};
    }

    check!(0u8);
    check!(u8::MAX);
    check!(0u16);
    check!(u16::MAX);
    check!(0u32);
    check!(u32::MAX);
    check!(0u64);
    check!(u64::MAX);
    check!(0u128);
    check!(u128::MAX);

    check!(0i8);
    check!(i8::MIN);
    check!(i8::MAX);
    check!(0i16);
    check!(i16::MIN);
    check!(i16::MAX);
    check!(0i32);
    check!(i32::MIN);
    check!(i32::MAX);
    check!(0i64);
    check!(i64::MIN);
    check!(i64::MAX);
    check!(0i128);
    check!(i128::MIN);
    check!(i128::MAX);

    // Powers of 2 +/- 1
    for p in 0..64 {
        let v: u64 = 1u64 << p;
        check!(v);
        if v > 0 {
            check!(v - 1);
        }
        if v < u64::MAX {
            check!(v + 1);
        }
    }

    for p in 0..128 {
        let v: u128 = 1u128 << p;
        check!(v);
        if v > 0 {
            check!(v - 1);
        }
        if v < u128::MAX {
            check!(v + 1);
        }
    }

    // Powers of 10 +/- 1
    let mut p10: u64 = 1;
    for _ in 0..19 {
        check!(p10);
        check!(p10 - 1);
        if p10 < u64::MAX - 1 {
            check!(p10 + 1);
        }
        p10 = p10.saturating_mul(10);
    }

    let mut p10_128: u128 = 1;
    for _ in 0..38 {
        check!(p10_128);
        check!(p10_128 - 1);
        if p10_128 < u128::MAX - 1 {
            check!(p10_128 + 1);
        }
        p10_128 = p10_128.saturating_mul(10);
    }
}

#[test]
fn test_floats_special_cases() {
    let mut fb = FloatBuffer::new();

    assert_eq!(fb.format(0.0f64), "0.0");
    assert_eq!(fb.format(-0.0f64), "-0.0");
    assert_eq!(fb.format(f64::INFINITY), "inf");
    assert_eq!(fb.format(f64::NEG_INFINITY), "-inf");
    assert_eq!(fb.format(f64::NAN), "NaN");

    assert_eq!(fb.format(0.0f32), "0.0");
    assert_eq!(fb.format(-0.0f32), "-0.0");
    assert_eq!(fb.format(f32::INFINITY), "inf");
    assert_eq!(fb.format(f32::NEG_INFINITY), "-inf");
    assert_eq!(fb.format(f32::NAN), "NaN");
}

#[test]
fn test_floats_subnormals_and_extremes() {
    let mut fb = FloatBuffer::new();
    let mut ryu_b = ryu::Buffer::new();

    // Smallest subnormal
    let subnormal_min = f64::from_bits(1);
    let s = fb.format(subnormal_min);
    assert_eq!(s, ryu_b.format(subnormal_min));
    assert_eq!(s.parse::<f64>().unwrap().to_bits(), 1);

    // Largest subnormal
    let subnormal_max = f64::from_bits(0x000f_ffff_ffff_ffff);
    let s = fb.format(subnormal_max);
    assert_eq!(s, ryu_b.format(subnormal_max));
    assert_eq!(s.parse::<f64>().unwrap().to_bits(), subnormal_max.to_bits());

    // Smallest normal
    let normal_min = f64::from_bits(0x0010_0000_0000_0000);
    let s = fb.format(normal_min);
    assert_eq!(s, ryu_b.format(normal_min));
    assert_eq!(s.parse::<f64>().unwrap().to_bits(), normal_min.to_bits());

    // Largest finite
    let max = f64::MAX;
    let s = fb.format(max);
    assert_eq!(s, ryu_b.format(max));
    assert_eq!(s.parse::<f64>().unwrap().to_bits(), max.to_bits());

    // Epsilon
    let eps = f64::EPSILON;
    let s = fb.format(eps);
    assert_eq!(s, ryu_b.format(eps));
    assert_eq!(s.parse::<f64>().unwrap().to_bits(), eps.to_bits());

    // Powers of 2 round trip
    for e in -1074..=1023 {
        let f = (2.0f64).powi(e);
        let s = fb.format(f);
        assert_eq!(s, ryu_b.format(f), "power of 2 mismatch 2^{e}");
        assert_eq!(s.parse::<f64>().unwrap().to_bits(), f.to_bits());
    }

    // f32 powers of 2 round trip
    for e in -149..=127 {
        let f = (2.0f32).powi(e);
        let s = fb.format(f);
        assert_eq!(s, ryu_b.format(f), "f32 power of 2 mismatch 2^{e}");
        assert_eq!(s.parse::<f32>().unwrap().to_bits(), f.to_bits());
    }
}

#[test]
fn test_floats_random_samples() {
    let mut fb = FloatBuffer::new();
    let mut ryu_b = ryu::Buffer::new();

    // 100,000 pseudo-random 64-bit float patterns (using simple LCG)
    let mut state: u64 = 0xdeadbeefcafe;
    for _ in 0..100_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let f = f64::from_bits(state);
        let ours = fb.format(f);
        let expected = ryu_b.format(f);
        assert_eq!(ours, expected, "mismatch on f64 bits 0x{:016x}", state);

        if f.is_finite() {
            let parsed = ours.parse::<f64>().unwrap();
            assert_eq!(
                parsed.to_bits(),
                f.to_bits(),
                "round-trip fail on 0x{:016x}",
                state
            );
        }
    }

    // 100,000 pseudo-random 32-bit float patterns
    let mut state32: u32 = 0x12345678;
    for _ in 0..100_000 {
        state32 = state32.wrapping_mul(1664525).wrapping_add(1013904223);
        let f = f32::from_bits(state32);
        let ours = fb.format(f);
        let expected = ryu_b.format(f);
        assert_eq!(ours, expected, "mismatch on f32 bits 0x{:08x}", state32);

        if f.is_finite() {
            let parsed = ours.parse::<f32>().unwrap();
            assert_eq!(
                parsed.to_bits(),
                f.to_bits(),
                "round-trip fail on f32 0x{:08x}",
                state32
            );
        }
    }
}

