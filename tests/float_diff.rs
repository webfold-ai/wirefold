//! Differential and exhaustive tests of the float formatter.
//!
//! Baselines: the `ryu` crate (same layout, byte for byte) and `core::fmt::Display` (the other
//! layout). Every check also parses the text back and compares the bit pattern.
//!
//! - `f32_exhaustive`: ALL 2^32 `f32` bit patterns. Not sampled, not ignored.
//! - `f64_*`: structured edge sets plus 4*10^7 random bit patterns (SYNTHETIC) per run.
//!
//! The shortest-and-closest property of the core is not proved in Lean; this file is its evidence.

use std::sync::atomic::{AtomicU64, Ordering};
use wirefold::num::{float_display_string, FloatBuffer};

fn check32(fb: &mut FloatBuffer, rb: &mut ryu::Buffer, bits: u32) -> Result<(), String> {
    let f = f32::from_bits(bits);
    let ours = fb.format(f);
    if !f.is_finite() {
        let want = rb.format(f);
        return if ours == want { Ok(()) } else { Err(format!("{bits:#010x}: {ours} vs {want}")) };
    }
    let want = rb.format_finite(f);
    if ours != want {
        return Err(format!("{bits:#010x}: ours {ours} ryu {want}"));
    }
    match ours.parse::<f32>() {
        Ok(p) if p.to_bits() == bits => Ok(()),
        other => Err(format!("{bits:#010x}: {ours} parses to {other:?}")),
    }
}

#[test]
fn f32_exhaustive() {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16) as u64;
    let total = 1u64 << 32;
    let chunk = total / threads;
    let done = AtomicU64::new(0);
    let fails = std::sync::Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for t in 0..threads {
            let (done, fails) = (&done, &fails);
            s.spawn(move || {
                let (lo, hi) = (t * chunk, if t == threads - 1 { total } else { (t + 1) * chunk });
                let mut fb = FloatBuffer::new();
                let mut rb = ryu::Buffer::new();
                for bits in lo..hi {
                    if let Err(e) = check32(&mut fb, &mut rb, bits as u32) {
                        let mut f = fails.lock().unwrap();
                        if f.len() < 20 {
                            f.push(e);
                        }
                    }
                    // Display layout against core::fmt on every 64th pattern (all exponents, all classes)
                    if bits % 64 == 17 {
                        let f = f32::from_bits(bits as u32);
                        let (a, b) = (float_display_string(f), f.to_string());
                        if a != b {
                            let mut v = fails.lock().unwrap();
                            if v.len() < 20 {
                                v.push(format!("display {bits:#010x}: ours {a} core {b}"));
                            }
                        }
                    }
                }
                done.fetch_add(hi - lo, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(done.load(Ordering::Relaxed), total, "did not visit all 2^32 patterns");
    let f = fails.lock().unwrap();
    assert!(f.is_empty(), "{} failures, first: {:#?}", f.len(), *f);
}

fn check64(fb: &mut FloatBuffer, rb: &mut ryu::Buffer, bits: u64) -> Result<(), String> {
    let f = f64::from_bits(bits);
    let ours = fb.format(f);
    let want = rb.format(f);
    if ours != want {
        return Err(format!("{bits:#018x}: ours {ours} ryu {want}"));
    }
    if f.is_finite() {
        match ours.parse::<f64>() {
            Ok(p) if p.to_bits() == bits => {}
            other => return Err(format!("{bits:#018x}: {ours} parses to {other:?}")),
        }
        let (a, b) = (float_display_string(f), f.to_string());
        if a != b {
            return Err(format!("display {bits:#018x}: ours {a} core {b}"));
        }
    }
    Ok(())
}

fn run64(patterns: impl Iterator<Item = u64>, what: &str) {
    let mut fb = FloatBuffer::new();
    let mut rb = ryu::Buffer::new();
    let mut n = 0u64;
    for bits in patterns {
        if let Err(e) = check64(&mut fb, &mut rb, bits) {
            panic!("{what}: {e}");
        }
        n += 1;
    }
    assert!(n > 0);
}

/// every exponent with the mantissa edge cases, plus 1 ulp around each
fn edge_patterns() -> Vec<u64> {
    let mut v = Vec::new();
    for e in 0..=0x7ffu64 {
        for m in [0u64, 1, 2, 3, (1 << 51), (1 << 51) + 1, (1 << 52) - 2, (1 << 52) - 1, 0x5555_5555_5555_5, 0xaaaa_aaaa_aaaa_a] {
            for sign in [0u64, 1] {
                v.push(sign << 63 | e << 52 | m);
            }
        }
    }
    // powers of ten and their neighbours, and k / 10^j for small k, j
    for j in -330i32..=310 {
        let x: f64 = format!("1e{j}").parse().unwrap();
        let b = x.to_bits();
        v.extend([b.wrapping_sub(1), b, b + 1]);
        for k in [2u32, 3, 5, 7, 9, 11, 123, 99999] {
            let y: f64 = format!("{k}e{j}").parse().unwrap();
            v.push(y.to_bits());
        }
    }
    // integers around 2^53 and powers of two
    for p in 0..64 {
        let x = (1u64 << p) as f64;
        let b = x.to_bits();
        v.extend([b.wrapping_sub(1), b, b + 1]);
    }
    v
}

#[test]
fn f64_edges() {
    run64(edge_patterns().into_iter(), "edge");
}

#[test]
fn f64_short_decimals() {
    // every decimal with up to 6 digits at several exponents, parsed to the nearest double
    let mut pats = Vec::new();
    for e in [-300i32, -23, -7, -5, -4, -2, -1, 0, 1, 5, 15, 16, 17, 22, 23, 100, 300] {
        for k in (1u32..1_000_000).step_by(7) {
            pats.push(format!("{k}e{e}").parse::<f64>().unwrap().to_bits());
        }
    }
    run64(pats.into_iter(), "short decimals");
}

#[test]
fn f64_random_bits() {
    // xorshift64*, fixed seeds: SYNTHETIC, reproducible
    let threads = 4u64;
    std::thread::scope(|s| {
        for t in 0..threads {
            s.spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (t + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
                let n = 10_000_000;
                run64(
                    (0..n).map(move |_| {
                        x ^= x >> 12;
                        x ^= x << 25;
                        x ^= x >> 27;
                        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
                    }),
                    "random bits",
                );
            });
        }
    });
}

#[test]
fn display_layout_matches_core_for_f32_classes() {
    for v in [0.0f32, -0.0, 1.0, 0.5, 100.0, 1e-7, 1e21, 16777216.0, f32::MAX, f32::MIN_POSITIVE, f32::EPSILON, 3.4028235e38, 1.0e-45, 0.1, 123456.79] {
        assert_eq!(float_display_string(v), v.to_string(), "{v:e}");
    }
    for v in [0.0f64, -0.0, 1.0, 5e-324, f64::MAX, 1e22, 1e23, 0.3, 2.5e-5, 123456789012345680.0] {
        assert_eq!(float_display_string(v), v.to_string(), "{v:e}");
    }
}

#[test]
fn debug_layout_matches_core_fmt() {
    use wirefold::num::float_debug_string;
    for v in [0.0f64, -0.0, 1.0, 0.5, 100.0, 1e-4, 0.00009999, 1e-5, 1e15, 1e16, 9999999999999998.0, 1.5e-7, 1e21, f64::MAX, 5e-324, 0.1, 123456789.125] {
        assert_eq!(float_debug_string(v), format!("{v:?}"), "{v:e}");
    }
    for v in [0.0f32, -0.0, 1.0, 1e-4, 1e-5, 1e15, 1e16, 3.4028235e38, 1.0e-45, 16777216.0, 0.1] {
        assert_eq!(float_debug_string(v), format!("{v:?}"), "{v:e}");
    }
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..3_000_000 {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let f = f64::from_bits(x.wrapping_mul(0x2545_F491_4F6C_DD1D));
        if f.is_finite() {
            assert_eq!(float_debug_string(f), format!("{f:?}"), "{:#x}", f.to_bits());
        }
        let g = f32::from_bits((x >> 32) as u32);
        if g.is_finite() {
            assert_eq!(float_debug_string(g), format!("{g:?}"), "{:#x}", g.to_bits());
        }
    }
}
