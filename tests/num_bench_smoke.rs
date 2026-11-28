//! Bench smoke, run by `cargo test` (the harness's blocking step): the same pools as `bench/src/itoa_ladder.rs`
//! and `bench/src/float_bench.rs`, 20,000 values each, checked byte for byte against `itoa`, `ryu` and
//! `core::fmt`, with a short timing printed (`--nocapture`). It asserts quality only: a timing assertion on a
//! shared machine would be flaky. The speed claims come from the full benches, not from here.

use std::hint::black_box;
use std::time::Instant;
use wirefold::num::{float_debug_string, float_display_string, FloatBuffer, IntBuffer};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const N: usize = 20_000;

fn timed<F: FnMut() -> usize>(label: &str, mut f: F) {
    let t = Instant::now();
    black_box(f());
    println!(
        "smoke {label}: {:.1} ns/op",
        t.elapsed().as_secs_f64() * 1e9 / N as f64
    );
}

#[test]
fn integer_pools_match_itoa_and_core_fmt() {
    let mut r = Rng(0x8543_1248_90ab_cdef);
    let uniform: Vec<u64> = (0..N).map(|_| r.next()).collect();
    let digits: Vec<u64> = (0..N)
        .map(|_| {
            let d = (r.next() % 19) as u32;
            let lo = 10u64.pow(d);
            lo + r.next() % (lo * 9)
        })
        .collect();
    let small: Vec<u64> = (0..N).map(|_| (r.next() >> 8) % 100).collect();
    for (name, pool) in [
        ("uniform", &uniform),
        ("digit-length", &digits),
        ("small", &small),
    ] {
        let (mut ours, mut its) = (IntBuffer::new(), itoa::Buffer::new());
        for &v in pool {
            assert_eq!(ours.format(v), its.format(v), "{name} {v}");
            assert_eq!(ours.format(v), v.to_string(), "{name} {v}");
            let i = v as i64;
            assert_eq!(ours.format(i), its.format(i), "{name} i64 {i}");
        }
        timed(&format!("u64 {name}"), || {
            let mut b = IntBuffer::new();
            pool.iter().map(|&v| b.format(black_box(v)).len()).sum()
        });
    }
}

#[test]
fn float_pools_match_ryu_and_core_fmt() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let bits: Vec<f64> = (0..N)
        .map(|_| f64::from_bits(r.next()))
        .filter(|f| f.is_finite())
        .collect();
    let dec: Vec<f64> = (0..N)
        .map(|_| (r.next() % 100_000_000) as f64 / 1000.0)
        .collect();
    for (name, pool) in [("bits", &bits), ("decimals", &dec)] {
        let (mut ours, mut theirs) = (FloatBuffer::new(), ryu::Buffer::new());
        for &f in pool {
            assert_eq!(ours.format(f), theirs.format(f), "{name} {f:e}");
            assert_eq!(float_display_string(f), f.to_string(), "{name} {f:e}");
            assert_eq!(float_debug_string(f), format!("{f:?}"), "{name} {f:e}");
        }
        timed(&format!("f64 {name} ryu layout"), || {
            let mut b = FloatBuffer::new();
            pool.iter().map(|&f| b.format(black_box(f)).len()).sum()
        });
    }
}
