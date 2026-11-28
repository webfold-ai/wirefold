//! Integer ladder: every rung formats the same inputs, byte-for-byte equal to `itoa`'s output
//! (plain decimal, no sign for unsigned, `-` for negative, no padding, no exponent).
//!
//! rungs: L0 div-10 loop < L1 digit-pair LUT, dynamic offset (safe) < L2 wirefold core (fixed-position
//! stores, safe) < L3 the same with an SWAR multiply instead of the LUT. HEAD is the previous in-tree
//! `write_u64` (unsafe), kept as a reference. baselines: `itoa` and `core::fmt`.
//! Usage: itoa-ladder [rounds]   (default 3 runs; prints median and min/max over the runs)

#![allow(dead_code)]
#[path = "../../src/num/itoa_core.rs"]
mod core_safe;
#[path = "itoa_head_unsafe.rs"]
mod head;

use core_safe::{digit_count, BUF_LEN, PAIRS};
use std::hint::black_box;
use std::time::Instant;

// ---- L0: one digit at a time -------------------------------------------------------------
fn l0(buf: &mut [u8; BUF_LEN], mut v: u64) -> usize {
    let mut i = BUF_LEN;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return i;
        }
    }
}

// ---- L1: digit pairs, dynamic offset, bounds-checked -----------------------------------------
fn l1(buf: &mut [u8; BUF_LEN], mut v: u64) -> usize {
    let mut i = BUF_LEN;
    while v >= 100 {
        let r = (v % 100) as usize;
        v /= 100;
        i -= 2;
        buf[i] = PAIRS[2 * r];
        buf[i + 1] = PAIRS[2 * r + 1];
    }
    if v >= 10 {
        i -= 2;
        buf[i] = PAIRS[2 * v as usize];
        buf[i + 1] = PAIRS[2 * v as usize + 1];
    } else {
        i -= 1;
        buf[i] = b'0' + v as u8;
    }
    i
}

// ---- L2: the wirefold core -------------------------------------------------------------------
#[inline(always)]
fn put8_lut(buf: &mut [u8; BUF_LEN], at: usize, v: u32) {
    put4_lut(buf, at, v / 10_000);
    put4_lut(buf, at + 4, v % 10_000);
}
fn l2(buf: &mut [u8; BUF_LEN], v: u64) -> usize {
    let n = digit_count(v);
    if v < 10_000 {
        put4_lut(buf, 36, v as u32);
    } else if v < 100_000_000 {
        put8_lut(buf, 32, v as u32);
    } else if v < 10_000_000_000_000_000 {
        put8_lut(buf, 24, (v / 100_000_000) as u32);
        put8_lut(buf, 32, (v % 100_000_000) as u32);
    } else {
        let rest = v % 10_000_000_000_000_000;
        put4_lut(buf, 20, (v / 10_000_000_000_000_000) as u32);
        put8_lut(buf, 24, (rest / 100_000_000) as u32);
        put8_lut(buf, 32, (rest % 100_000_000) as u32);
    }
    BUF_LEN - n
}
fn l3s(buf: &mut [u8; BUF_LEN], v: u64) -> usize {
    core_safe::fmt_u64(buf, v)
}

// ---- L3: fixed positions, SWAR multiply for the 8-digit chunks ------------------------------
#[inline(always)]
fn swar8(v: u32) -> u64 {
    let a = (v / 10_000) as u64;
    let b = (v % 10_000) as u64;
    let x = a | (b << 32);
    let t = ((x * 10486) >> 20) & 0x0000_007F_0000_007F;
    let x = ((x - t * 100) << 16) | t;
    let t2 = ((x * 103) >> 10) & 0x000F_000F_000F_000F;
    let x = ((x - t2 * 10) << 8) | t2;
    x + 0x3030_3030_3030_3030
}
#[inline(always)]
fn put8_swar(buf: &mut [u8; BUF_LEN], at: usize, v: u32) {
    buf[at..at + 8].copy_from_slice(&swar8(v).to_le_bytes());
}
#[inline(always)]
fn put4_lut(buf: &mut [u8; BUF_LEN], at: usize, v: u32) {
    let hi = (v / 100) as usize * 2;
    let lo = (v % 100) as usize * 2;
    buf[at] = PAIRS[hi];
    buf[at + 1] = PAIRS[hi + 1];
    buf[at + 2] = PAIRS[lo];
    buf[at + 3] = PAIRS[lo + 1];
}
fn l3(buf: &mut [u8; BUF_LEN], v: u64) -> usize {
    let n = digit_count(v);
    if v < 10_000 {
        put4_lut(buf, 36, v as u32);
    } else if v < 100_000_000 {
        put8_swar(buf, 32, v as u32);
    } else if v < 10_000_000_000_000_000 {
        put8_swar(buf, 24, (v / 100_000_000) as u32);
        put8_swar(buf, 32, (v % 100_000_000) as u32);
    } else {
        let rest = v % 10_000_000_000_000_000;
        put4_lut(buf, 20, (v / 10_000_000_000_000_000) as u32);
        put8_swar(buf, 24, (rest / 100_000_000) as u32);
        put8_swar(buf, 32, (rest % 100_000_000) as u32);
    }
    BUF_LEN - n
}


// ---- L4: SWAR + leading-zero count taken from the top chunk (no separate digit_count) -------
#[inline(always)]
fn lead_zeros8(w: u64) -> usize {
    // w holds 8 ASCII digit bytes, first digit in the lowest byte; the first non-'0' byte index
    ((w ^ 0x3030_3030_3030_3030).trailing_zeros() / 8) as usize
}
fn l4(buf: &mut [u8; BUF_LEN], v: u64) -> usize {
    if v < 100_000_000 {
        let w = swar8(v as u32);
        buf[32..40].copy_from_slice(&w.to_le_bytes());
        32 + lead_zeros8(w).min(7)
    } else if v < 10_000_000_000_000_000 {
        let hw = swar8((v / 100_000_000) as u32);
        let lw = swar8((v % 100_000_000) as u32);
        buf[24..32].copy_from_slice(&hw.to_le_bytes());
        buf[32..40].copy_from_slice(&lw.to_le_bytes());
        24 + lead_zeros8(hw)
    } else {
        let rest = v % 10_000_000_000_000_000;
        let top = (v / 10_000_000_000_000_000) as u32;
        put4_lut(buf, 20, top);
        buf[24..32].copy_from_slice(&swar8((rest / 100_000_000) as u32).to_le_bytes());
        buf[32..40].copy_from_slice(&swar8((rest % 100_000_000) as u32).to_le_bytes());
        20 + (top < 1000) as usize + (top < 100) as usize + (top < 10) as usize
    }
}

// ---- pools -----------------------------------------------------------------------------------
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn pools(n: usize) -> Vec<(&'static str, Vec<u64>)> {
    let mut r = Rng(0x8543_1248_90ab_cdef);
    let uniform: Vec<u64> = (0..n).map(|_| r.next()).collect();
    // digit count uniform on 1..=20, then a uniform value with exactly that many digits
    let random_len: Vec<u64> = (0..n)
        .map(|_| {
            let d = (r.next() % 20) as usize; // 0..=19 => d+1 digits
            let lo = core_safe::POW10[d];
            let span = if d == 19 { u64::MAX - lo } else { core_safe::POW10[d + 1] - lo };
            lo + r.next() % span
        })
        .collect();
    let small: Vec<u64> = (0..n)
        .map(|_| {
            let x = r.next();
            match x % 100 {
                0..=79 => (x >> 8) % 100,
                80..=94 => (x >> 8) % 10_000,
                _ => (x >> 8) % 100_000_000,
            }
        })
        .collect();
    let json: Vec<u64> = (0..n)
        .map(|_| match r.next() % 4 {
            0 => r.next() % 1_000_000,
            1 => 1_700_000_000_000 + r.next() % 100_000_000_000, // ms timestamps
            2 => r.next() % 100_000_000_000_000,
            _ => r.next() % 1000,
        })
        .collect();
    vec![
        ("u64 uniform random bits", uniform),
        ("u64 random digit length 1..20", random_len),
        ("u64 small-int-heavy (80% < 100)", small),
        ("u64 JSON-like (ids, ms timestamps)", json),
    ]
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

fn main() {
    let runs: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    const N: usize = 100_000;
    println!("# Integer ladder (SYNTHETIC pools, N={N}, {runs} runs; ns/op median [min..max over runs])");
    println!("# output format of every row: plain decimal digits, `-` prefix if negative, no padding (== itoa == core::fmt)");
    for (name, data) in pools(N) {
        // quality: byte-equality against itoa and core::fmt for every input, for every rung
        let mut fails = [0usize; 9];
        let mut ib = itoa::Buffer::new();
        let mut b = [0u8; BUF_LEN];
        for &v in &data {
            let want = ib.format(v);
            let s = format!("{v}");
            assert_eq!(want, s);
            for (k, f) in [l0 as fn(&mut [u8; BUF_LEN], u64) -> usize, l1, l2, l3, l4, l3s].iter().enumerate() {
                let i = f(&mut b, v);
                if &b[i..] != want.as_bytes() {
                    fails[k] += 1;
                }
            }
            let mut hb = [0u8; 40];
            if head::write_u64(&mut hb, v) != want {
                fails[6] += 1;
            }
        }
        let rungs: Vec<(&str, Box<dyn Fn(&[u64]) -> usize>)> = vec![
            ("L0 div-10 loop", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l0(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("L1 pair LUT, dynamic offset (safe)", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l1(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("L2 fixed stores, LUT (safe)", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l2(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("L3 fixed stores, SWAR, to_le_bytes (safe)", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l3(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("L4 SWAR + zero-count from top chunk (safe)", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l4(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("SHIPPED: wirefold itoa_core (== L2, safe, proved)", Box::new(|d: &[u64]| { let mut b = [0u8; BUF_LEN]; let mut t = 0; for &v in d { let i = l3s(&mut b, black_box(v)); black_box(&b[i..]); t += BUF_LEN - i; } t })),
            ("HEAD d806af3 write_u64 (unsafe)", Box::new(|d: &[u64]| { let mut b = [0u8; 40]; let mut t = 0; for &v in d { let s = head::write_u64(&mut b, black_box(v)); black_box(s); t += s.len(); } t })),
            ("itoa crate", Box::new(|d: &[u64]| { let mut b = itoa::Buffer::new(); let mut t = 0; for &v in d { let s = b.format(black_box(v)); black_box(s); t += s.len(); } t })),
            ("core::fmt", Box::new(|d: &[u64]| { use std::fmt::Write; let mut s = String::with_capacity(32); let mut t = 0; for &v in d { s.clear(); write!(s, "{}", black_box(v)).unwrap(); black_box(&s); t += s.len(); } t })),
        ];
        println!("\n## {name}  [SYNTHETIC]\n| rung | byte-diff vs itoa | ns/op median [min..max] | vs itoa |\n|---|---:|---:|---:|");
        let mut res: Vec<Vec<f64>> = vec![Vec::new(); rungs.len()];
        for _ in 0..runs {
            for (k, (_, f)) in rungs.iter().enumerate() {
                res[k].push(time(|| f(&data), 9, N));
            }
        }
        let med = |v: &Vec<f64>| { let mut s = v.clone(); s.sort_by(|a, b| a.partial_cmp(b).unwrap()); s[s.len() / 2] };
        let itoa_med = med(&res[7]);
        for (k, (label, _)) in rungs.iter().enumerate() {
            let q = if k < 7 { fails[k].to_string() } else { "0 (reference)".into() };
            let (mn, mx) = (res[k].iter().cloned().fold(f64::MAX, f64::min), res[k].iter().cloned().fold(0.0, f64::max));
            println!("| {label} | {q} | {:.2} [{mn:.2}..{mx:.2}] | {:.2}x |", med(&res[k]), itoa_med / med(&res[k]));
        }
    }
}
