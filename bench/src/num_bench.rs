//! Comprehensive benchmark harness comparing wirefold's in-tree number formatters
//! against `itoa`, `ryu`, and `core::fmt` (std `Display`) on identical held-out inputs.

use std::hint::black_box;
use std::time::Instant;
use wirefold::num::{FloatBuffer, IntBuffer};

fn time_ns<F: FnMut() -> usize>(mut f: F, iters: usize, batch_size: usize) -> f64 {
    // Warmup
    for _ in 0..2 {
        black_box(f());
    }
    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        let val = black_box(f());
        let el = t0.elapsed();
        black_box(val);
        times.push(el.as_secs_f64() * 1e9 / batch_size as f64);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2] // Median
}

fn main() {
    const N: usize = 100_000;

    println!("# Number Formatting Benchmark Report\n");
    println!("Evaluation of wirefold in-tree formatters against itoa, ryu, and core::fmt (std).\n");
    println!("| Input (held-out) | Label | Quality (failures) | Ours (ns/op) | itoa / ryu (ns/op) | core::fmt (ns/op) | Speedup vs itoa/ryu |");
    println!("|---|---|---:|---:|---:|---:|---:|");

    // -------------------------------------------------------------------------
    // 1. u64 uniform random bits [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0x8543_1248_90ab_cdef;
        let data: Vec<u64> = (0..N)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            })
            .collect();

        // Quality check
        let mut fail = 0usize;
        let mut ib = IntBuffer::new();
        for &v in &data {
            let s = ib.format(v);
            if s.parse::<u64>().unwrap() != v {
                fail += 1;
            }
        }

        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ib.format(v).len();
            }
            total
        }, 7, N);

        let mut itoa_b = itoa::Buffer::new();
        let t_itoa = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += itoa_b.format(v).len();
            }
            total
        }, 7, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 7, N);

        println!(
            "| u64 uniform random bits | SYNTHETIC | {fail} | {t_ours:.2} | {t_itoa:.2} | {t_std:.2} | {:.2}x |",
            t_itoa / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 2. u64 small-int-heavy [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0x1234_5678_9abcdef0;
        let data: Vec<u64> = (0..N)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let pick = rng % 100;
                if pick < 80 {
                    rng % 100 // 0..99
                } else if pick < 95 {
                    rng % 10_000 // 100..9999
                } else {
                    rng % 100_000_000
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut ib = IntBuffer::new();
        for &v in &data {
            let s = ib.format(v);
            if s.parse::<u64>().unwrap() != v {
                fail += 1;
            }
        }

        let mut itoa_b = itoa::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ib.format(v).len();
            }
            total
        }, 7, N);

        let t_itoa = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += itoa_b.format(v).len();
            }
            total
        }, 7, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 7, N);

        println!(
            "| u64 small-int-heavy (80% < 100) | SYNTHETIC | {fail} | {t_ours:.2} | {t_itoa:.2} | {t_std:.2} | {:.2}x |",
            t_itoa / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 3. u64 realistic JSON-number distribution [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0x9876_5432_10fe_dcba;
        let data: Vec<u64> = (0..N)
            .map(|i| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                match i % 5 {
                    0 => 200,                  // HTTP 200
                    1 => 404,                  // HTTP 404
                    2 => 1_700_000_000 + (rng % 100_000), // Epoch timestamp
                    3 => rng % 50,             // Small count
                    _ => 10_000 + (rng % 50_000), // Item ID
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut ib = IntBuffer::new();
        for &v in &data {
            let s = ib.format(v);
            if s.parse::<u64>().unwrap() != v {
                fail += 1;
            }
        }

        let mut itoa_b = itoa::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ib.format(v).len();
            }
            total
        }, 7, N);

        let t_itoa = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += itoa_b.format(v).len();
            }
            total
        }, 7, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 7, N);

        println!(
            "| u64 JSON distribution (IDs/timestamps) | SYNTHETIC | {fail} | {t_ours:.2} | {t_itoa:.2} | {t_std:.2} | {:.2}x |",
            t_itoa / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 4. u64 worst cases (19-20 digits) [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0xa5a5_a5a5_5a5a_5a5a;
        let data: Vec<u64> = (0..N)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                10_000_000_000_000_000_000u64.saturating_add(rng % 8_000_000_000_000_000_000)
            })
            .collect();

        let mut fail = 0usize;
        let mut ib = IntBuffer::new();
        for &v in &data {
            let s = ib.format(v);
            if s.parse::<u64>().unwrap() != v {
                fail += 1;
            }
        }

        let mut itoa_b = itoa::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ib.format(v).len();
            }
            total
        }, 7, N);

        let t_itoa = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += itoa_b.format(v).len();
            }
            total
        }, 7, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 7, N);

        println!(
            "| u64 worst cases (19-20 digits) | SYNTHETIC | {fail} | {t_ours:.2} | {t_itoa:.2} | {t_std:.2} | {:.2}x |",
            t_itoa / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 5. i64 small-int-heavy [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: i64 = 0x2468_ace0_1357_9bdf;
        let data: Vec<i64> = (0..N)
            .map(|_| {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                (rng % 200) - 100 // -100..99
            })
            .collect();

        let mut fail = 0usize;
        let mut ib = IntBuffer::new();
        for &v in &data {
            let s = ib.format(v);
            if s.parse::<i64>().unwrap() != v {
                fail += 1;
            }
        }

        let mut itoa_b = itoa::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ib.format(v).len();
            }
            total
        }, 7, N);

        let t_itoa = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += itoa_b.format(v).len();
            }
            total
        }, 7, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 7, N);

        println!(
            "| i64 small-int-heavy (-100..100) | SYNTHETIC | {fail} | {t_ours:.2} | {t_itoa:.2} | {t_std:.2} | {:.2}x |",
            t_itoa / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 6. f64 uniform random bits [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0xdead_beef_cafe_babe;
        let data: Vec<f64> = (0..N)
            .map(|_| {
                loop {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let f = f64::from_bits(rng);
                    if f.is_finite() {
                        return f;
                    }
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut fb = FloatBuffer::new();
        for &v in &data {
            let s = fb.format(v);
            if s.parse::<f64>().unwrap().to_bits() != v.to_bits() {
                fail += 1;
            }
        }

        let mut ryu_b = ryu::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += fb.format(v).len();
            }
            total
        }, 5, N);

        let t_ryu = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ryu_b.format(v).len();
            }
            total
        }, 5, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 5, N);

        println!(
            "| f64 uniform random bits | SYNTHETIC | {fail} | {t_ours:.2} | {t_ryu:.2} | {t_std:.2} | {:.2}x |",
            t_ryu / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 7. f64 realistic JSON-number distribution [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u64 = 0x55aa_33cc_0f0f_1234;
        let data: Vec<f64> = (0..N)
            .map(|i| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                match i % 4 {
                    0 => (rng % 1000) as f64 / 10.0, // Temperature/price e.g. 37.5
                    1 => (rng % 10000) as f64 / 10000.0, // Percentage 0.0000..0.9999
                    2 => 1_700_000_000.0 + (rng % 1_000_000) as f64 / 1000.0, // Timestamp with ms
                    _ => (rng % 100) as f64, // Small integer as float
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut fb = FloatBuffer::new();
        for &v in &data {
            let s = fb.format(v);
            if s.parse::<f64>().unwrap().to_bits() != v.to_bits() {
                fail += 1;
            }
        }

        let mut ryu_b = ryu::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += fb.format(v).len();
            }
            total
        }, 5, N);

        let t_ryu = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ryu_b.format(v).len();
            }
            total
        }, 5, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 5, N);

        println!(
            "| f64 JSON distribution (sensor/price) | SYNTHETIC | {fail} | {t_ours:.2} | {t_ryu:.2} | {t_std:.2} | {:.2}x |",
            t_ryu / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 8. f64 worst cases (subnormals + 17-digit boundaries) [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let data: Vec<f64> = (0..N)
            .map(|i| {
                if i < 1000 {
                    f64::from_bits((i + 1) as u64) // Subnormals
                } else if i < 2000 {
                    (2.0f64).powi((i as i32 % 2000) - 1000) // Powers of 2
                } else {
                    f64::from_bits(0x0010_0000_0000_0000 | ((i as u64) << 10))
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut fb = FloatBuffer::new();
        for &v in &data {
            let s = fb.format(v);
            if s.parse::<f64>().unwrap().to_bits() != v.to_bits() {
                fail += 1;
            }
        }

        let mut ryu_b = ryu::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += fb.format(v).len();
            }
            total
        }, 5, N);

        let t_ryu = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ryu_b.format(v).len();
            }
            total
        }, 5, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 5, N);

        println!(
            "| f64 worst cases (subnormals/powers) | SYNTHETIC | {fail} | {t_ours:.2} | {t_ryu:.2} | {t_std:.2} | {:.2}x |",
            t_ryu / t_ours
        );
    }

    // -------------------------------------------------------------------------
    // 9. f32 uniform random bits [SYNTHETIC]
    // -------------------------------------------------------------------------
    {
        let mut rng: u32 = 0xbeef_1234;
        let data: Vec<f32> = (0..N)
            .map(|_| {
                loop {
                    rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    let f = f32::from_bits(rng);
                    if f.is_finite() {
                        return f;
                    }
                }
            })
            .collect();

        let mut fail = 0usize;
        let mut fb = FloatBuffer::new();
        for &v in &data {
            let s = fb.format(v);
            if s.parse::<f32>().unwrap().to_bits() != v.to_bits() {
                fail += 1;
            }
        }

        let mut ryu_b = ryu::Buffer::new();
        let mut buf_std = String::with_capacity(32);
        let t_ours = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += fb.format(v).len();
            }
            total
        }, 5, N);

        let t_ryu = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                total += ryu_b.format(v).len();
            }
            total
        }, 5, N);

        let t_std = time_ns(|| {
            let mut total = 0;
            for &v in &data {
                buf_std.clear();
                use std::fmt::Write as _;
                write!(buf_std, "{v}").unwrap();
                total += buf_std.len();
            }
            total
        }, 5, N);

        println!(
            "| f32 uniform random bits | SYNTHETIC | {fail} | {t_ours:.2} | {t_ryu:.2} | {t_std:.2} | {:.2}x |",
            t_ryu / t_ours
        );
    }
}
