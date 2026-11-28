//! Float formatting: wirefold vs the `ryu` crate vs `zmij` vs `core::fmt`, on identical inputs.
//!
//! Output formats (stated per column, because they are not all the same text):
//! - "ryu layout": `1.0`, `0.1`, `1e21`, `1.234e-7` (wirefold `FloatBuffer::format`, `ryu::Buffer`; `zmij`
//!   is compared byte for byte and its difference count is reported, not assumed).
//! - "Display layout": `1`, `0.1`, `1000000000000000000000`, `0.0000001` (wirefold `float_display_string`
//!   vs `format!("{}")`; this is the text `f64::to_string()` returns, i.e. what the decoders emitted before).
//!
//! Pools are SYNTHETIC. Usage: float-bench [runs]  (default 3; ns/op median [min..max] over runs)

use std::hint::black_box;
use std::time::Instant;
use wirefold::num::{float_display_string, FloatBuffer};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn pools64(n: usize) -> Vec<(&'static str, Vec<f64>)> {
    let mut r = Rng(0x8543_1248_90ab_cdef);
    let finite = |b: u64| {
        let f = f64::from_bits(b);
        if f.is_finite() { f } else { 1.5 }
    };
    let uniform: Vec<f64> = (0..n).map(|_| finite(r.next())).collect();
    let json: Vec<f64> = (0..n)
        .map(|_| {
            let digits = 1 + r.next() % 6; // 1..=6 significant decimals after the point
            let scale = 10u64.pow(digits as u32);
            (r.next() % (1000 * scale)) as f64 / scale as f64 // prices / sensor readings
        })
        .collect();
    let ints: Vec<f64> = (0..n).map(|_| (1_700_000_000_000 + r.next() % 100_000_000_000) as f64).collect();
    let unit: Vec<f64> = (0..n).map(|_| (r.next() >> 11) as f64 / (1u64 << 53) as f64).collect();
    vec![
        ("f64 uniform random bits", uniform),
        ("f64 JSON-like decimals (prices, sensors)", json),
        ("f64 integer-valued (ms timestamps)", ints),
        ("f64 uniform [0,1)", unit),
    ]
}

fn pools32(n: usize) -> Vec<(&'static str, Vec<f32>)> {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let uniform: Vec<f32> = (0..n)
        .map(|_| {
            let f = f32::from_bits(r.next() as u32);
            if f.is_finite() { f } else { 1.5 }
        })
        .collect();
    let json: Vec<f32> = (0..n).map(|_| (r.next() % 100_000) as f32 / 100.0).collect();
    vec![("f32 uniform random bits", uniform), ("f32 two-decimal values", json)]
}

fn time<F: FnMut() -> usize>(mut f: F, reps: usize, n: usize) -> f64 {
    black_box(f());
    let mut v: Vec<f64> = (0..reps)
        .map(|_| {
            let t = Instant::now();
            black_box(f());
            t.elapsed().as_secs_f64() * 1e9 / n as f64
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn med(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

fn row(label: &str, diff: String, res: &[f64], base: f64) {
    let mn = res.iter().cloned().fold(f64::MAX, f64::min);
    let mx = res.iter().cloned().fold(0.0, f64::max);
    println!("| {label} | {diff} | {:.2} [{mn:.2}..{mx:.2}] | {:.2}x |", med(res), base / med(res));
}

fn main() {
    let runs: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    const N: usize = 100_000;
    println!("# Float formatting (SYNTHETIC pools, N={N}, {runs} runs; ns/op median [min..max])\n");
    for (name, data) in pools64(N) {
        let mut fb = FloatBuffer::new();
        let mut rb = ryu::Buffer::new();
        let mut zb = zmij::Buffer::new();
        let (mut d_ryu, mut d_zmij, mut d_parse, mut d_disp) = (0, 0, 0, 0);
        for &f in &data {
            let ours = fb.format(f).to_string();
            if ours != rb.format(f) {
                d_ryu += 1;
            }
            if ours != zb.format(f) {
                d_zmij += 1;
            }
            if ours.parse::<f64>().unwrap().to_bits() != f.to_bits() {
                d_parse += 1;
            }
            if float_display_string(f) != f.to_string() {
                d_disp += 1;
            }
        }
        println!("## {name}  [SYNTHETIC]\nquality: byte-diff vs ryu = {d_ryu}, vs zmij = {d_zmij}, parse round-trip failures = {d_parse}, Display-layout diff vs core::fmt = {d_disp}\n");
        println!("| contestant (output layout) | byte-diff vs ryu | ns/op median [min..max] | vs ryu |\n|---|---:|---:|---:|");
        type Fm<'a> = Box<dyn Fn(&[f64]) -> usize + 'a>;
        let rungs: Vec<(&str, String, Fm)> = vec![
            ("wirefold FloatBuffer::format (ryu layout)", "0".into(), Box::new(|d: &[f64]| { let mut b = FloatBuffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("ryu crate (ryu layout)", "0 (reference)".into(), Box::new(|d: &[f64]| { let mut b = ryu::Buffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("  (diagnostic) wirefold core only: dec_f64, no layout", "-".into(), Box::new(|d: &[f64]| { let mut t = 0; for &v in d { let b = black_box(v).to_bits(); let (m, e) = wirefold::num::float_core::dec_f64(b & ((1 << 52) - 1), ((b >> 52) & 0x7ff) as u32, true); black_box(m); t += e as usize; } t })),
            ("  (diagnostic) core + itoa_core::fmt_u64 of the digits, no layout", "-".into(), Box::new(|d: &[f64]| { let mut t = 0; let mut buf = [0u8; 40]; for &v in d { let b = black_box(v).to_bits(); let (m, e) = wirefold::num::float_core::dec_f64(b & ((1 << 52) - 1), ((b >> 52) & 0x7ff) as u32, true); let s = wirefold::num::itoa_core::fmt_u64(&mut buf, m); black_box(&buf[s..]); t += e as usize; } t })),
            ("  (diagnostic) wirefold format_bytes (no UTF-8 check)", "-".into(), Box::new(|d: &[f64]| { let mut b = FloatBuffer::new(); let mut t = 0; for &v in d { let s = b.format_bytes(black_box(v)); black_box(s); t += s.len(); } t })),
            ("zmij (its own layout)", d_zmij.to_string(), Box::new(|d: &[f64]| { let mut b = zmij::Buffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("wirefold float_display_string (Display layout)", format!("{d_disp} vs core::fmt"), Box::new(|d: &[f64]| { let mut t = 0; for &v in d { let s = float_display_string(black_box(v)); black_box(&s); t += s.len(); } t })),
            ("core::fmt Display via to_string (Display layout)", "reference".into(), Box::new(|d: &[f64]| { let mut t = 0; for &v in d { let s = black_box(v).to_string(); black_box(&s); t += s.len(); } t })),
        ];
        let mut res: Vec<Vec<f64>> = vec![Vec::new(); rungs.len()];
        for _ in 0..runs {
            for (k, (_, _, f)) in rungs.iter().enumerate() {
                res[k].push(time(|| f(&data), 9, N));
            }
        }
        let base = med(&res[1]);
        for (k, (label, diff, _)) in rungs.iter().enumerate() {
            row(label, diff.clone(), &res[k], base);
        }
        println!();
    }
    for (name, data) in pools32(N) {
        let mut fb = FloatBuffer::new();
        let mut rb = ryu::Buffer::new();
        let mut zb = zmij::Buffer::new();
        let (mut d_ryu, mut d_zmij) = (0, 0);
        for &f in &data {
            let ours = fb.format(f).to_string();
            if ours != rb.format(f) {
                d_ryu += 1;
            }
            if ours != zb.format(f) {
                d_zmij += 1;
            }
        }
        println!("## {name}  [SYNTHETIC]\nquality: byte-diff vs ryu = {d_ryu}, vs zmij = {d_zmij}\n");
        println!("| contestant (output layout) | byte-diff vs ryu | ns/op median [min..max] | vs ryu |\n|---|---:|---:|---:|");
        type Fm<'a> = Box<dyn Fn(&[f32]) -> usize + 'a>;
        let rungs: Vec<(&str, String, Fm)> = vec![
            ("wirefold FloatBuffer::format (ryu layout)", "0".into(), Box::new(|d: &[f32]| { let mut b = FloatBuffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("ryu crate (ryu layout)", "0 (reference)".into(), Box::new(|d: &[f32]| { let mut b = ryu::Buffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("zmij (its own layout)", d_zmij.to_string(), Box::new(|d: &[f32]| { let mut b = zmij::Buffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("core::fmt Display via to_string", "reference".into(), Box::new(|d: &[f32]| { let mut t = 0; for &v in d { let s = black_box(v).to_string(); black_box(&s); t += s.len(); } t })),
        ];
        let mut res: Vec<Vec<f64>> = vec![Vec::new(); rungs.len()];
        for _ in 0..runs {
            for (k, (_, _, f)) in rungs.iter().enumerate() {
                res[k].push(time(|| f(&data), 9, N));
            }
        }
        let base = med(&res[1]);
        for (k, (label, diff, _)) in rungs.iter().enumerate() {
            row(label, diff.clone(), &res[k], base);
        }
        println!();
    }
}
