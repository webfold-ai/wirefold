//! The float core's tables and exponent formulas, checked against exact big-integer arithmetic.
//!
//! `src/num/float_tables.rs` holds `G[j + 292] = ceil(10^j * 2^(125 - f))` as two `u64` limbs,
//! where `f = floor(log2(10^j))`, for every `j` in `-292..=324`. This test recomputes every entry
//! with arbitrary-precision arithmetic (no floating point) and compares. It also checks the three
//! fixed-point formulas the core uses to find `k` and `f` against exact comparisons over the whole
//! exponent range of `f64`. `WIREFOLD_EMIT_TABLE=1 cargo test --test float_tables emit` rewrites the table.

use wirefold::num::float_core::{
    flog10_pow2, flog10_three_quarters_pow2, flog2_pow10, G, J_MAX, J_MIN,
};

#[derive(Clone, PartialEq, Eq, Debug)]
struct Big(Vec<u32>); // little endian, no trailing zero limbs

impl Big {
    fn from_u64(v: u64) -> Big {
        let mut b = Big(vec![v as u32, (v >> 32) as u32]);
        b.trim();
        b
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for l in self.0.iter_mut() {
            let t = *l as u64 * m as u64 + carry;
            *l = t as u32;
            carry = t >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }
    fn shl(&mut self, bits: usize) {
        let (limbs, rem) = (bits / 32, bits % 32);
        if rem != 0 {
            let mut carry = 0u32;
            for l in self.0.iter_mut() {
                let t = (*l as u64) << rem | carry as u64;
                *l = t as u32;
                carry = (t >> 32) as u32;
            }
            if carry != 0 {
                self.0.push(carry);
            }
        }
        let mut v = vec![0u32; limbs];
        v.extend_from_slice(&self.0);
        self.0 = v;
        self.trim();
    }
    fn bit_len(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(&top) => (self.0.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }
    fn bit(&self, i: usize) -> bool {
        self.0
            .get(i / 32)
            .map_or(false, |l| (l >> (i % 32)) & 1 == 1)
    }
    fn cmp(&self, o: &Big) -> std::cmp::Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }
    fn sub(&mut self, o: &Big) {
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let t = self.0[i] as i64 - o.0.get(i).copied().unwrap_or(0) as i64 - borrow;
            self.0[i] = t as u32;
            borrow = (t < 0) as i64;
        }
        assert_eq!(borrow, 0);
        self.trim();
    }
    fn pow10(n: usize) -> Big {
        let mut b = Big::from_u64(1);
        for _ in 0..n {
            b.mul_small(10);
        }
        b
    }
    fn pow2(n: usize) -> Big {
        let mut b = Big::from_u64(1);
        b.shl(n);
        b
    }
    /// bits `[lo, lo + 128)` of self as u128
    fn window(&self, lo: usize) -> u128 {
        (0..128).fold(0u128, |a, i| a | (self.bit(lo + i) as u128) << i)
    }
}

/// floor(2^s / d) for a quotient known to fit in u128.
fn floor_div_pow2(s: usize, d: &Big) -> u128 {
    let mut rem = Big(vec![]);
    let mut q: u128 = 0;
    for i in (0..=s).rev() {
        rem.shl(1);
        if i == s {
            if rem.0.is_empty() {
                rem.0.push(1);
            } else {
                rem.0[0] |= 1;
            }
        }
        q <<= 1;
        if rem.cmp(d) != std::cmp::Ordering::Less {
            rem.sub(d);
            q |= 1;
        }
    }
    q
}

/// g(j) = ceil(10^j * 2^(125 - f)) with f = floor(log2 10^j)
fn g_exact(j: i32) -> u128 {
    if j >= 0 {
        let p = Big::pow10(j as usize);
        let f = p.bit_len() as i32 - 1;
        let e = 125 - f;
        // g = ceil(10^j * 2^e): exact when that product is an integer, so that exact ties stay exact
        if e >= 0 {
            let mut x = p.clone();
            x.shl(e as usize);
            x.window(0)
        } else {
            let low_zero = (0..(-e) as usize).all(|i| !p.bit(i));
            p.window((-e) as usize) + if low_zero { 0 } else { 1 }
        }
    } else {
        let d = Big::pow10((-j) as usize);
        let mut dm1 = d.clone();
        dm1.sub(&Big::from_u64(1));
        let s = 125 + dm1.bit_len();
        floor_div_pow2(s, &d) + 1
    }
}

/// largest k with 10^k * den <= num
fn floor_log10(num: &Big, den: &Big) -> i32 {
    let mut k: i32 = -400;
    while k < 400 {
        let next = k + 1;
        let ok = if next >= 0 {
            let mut l = Big::pow10(next as usize);
            mul_big(&mut l, den);
            l.cmp(num) != std::cmp::Ordering::Greater
        } else {
            let mut r = num.clone();
            mul_big(&mut r, &Big::pow10((-next) as usize));
            den.cmp(&r) != std::cmp::Ordering::Greater
        };
        if !ok {
            return k;
        }
        k = next;
    }
    panic!("out of range");
}

fn mul_big(a: &mut Big, b: &Big) {
    let mut out = vec![0u32; a.0.len() + b.0.len() + 1];
    for (i, &x) in a.0.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.0.iter().enumerate() {
            let t = out[i + j] as u64 + x as u64 * y as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut idx = i + b.0.len();
        while carry != 0 {
            let t = out[idx] as u64 + carry;
            out[idx] = t as u32;
            carry = t >> 32;
            idx += 1;
        }
    }
    a.0 = out;
    a.trim();
}

#[test]
fn table_range_is_what_the_core_needs() {
    assert_eq!(G.len() as i32, 2 * (J_MAX - J_MIN + 1));
    assert_eq!((J_MIN, J_MAX), (-292, 324));
}

#[test]
fn committed_table_is_exact() {
    for j in J_MIN..=J_MAX {
        let want = g_exact(j);
        let gi = ((j - J_MIN) as usize) * 2;
        let (hi, lo) = (G[gi], G[gi + 1]);
        let got = (hi as u128) << 64 | lo as u128;
        assert_eq!(got, want, "G[{j}] differs from exact arithmetic");
        assert!(got >> 125 == 1, "G[{j}] is not a 126-bit number");
    }
}

#[test]
fn exponent_formulas_are_exact() {
    for q in -1100..=1100i32 {
        let num = if q >= 0 {
            Big::pow2(q as usize)
        } else {
            Big::from_u64(1)
        };
        let den = if q >= 0 {
            Big::from_u64(1)
        } else {
            Big::pow2((-q) as usize)
        };
        assert_eq!(flog10_pow2(q), floor_log10(&num, &den), "flog10_pow2({q})");
        // floor(log10(3/4 * 2^q)) = floor(log10(3 * 2^(q-2)))
        let (n3, d3) = if q >= 2 {
            let mut n = Big::pow2((q - 2) as usize);
            n.mul_small(3);
            (n, Big::from_u64(1))
        } else {
            (Big::from_u64(3), Big::pow2((2 - q) as usize))
        };
        assert_eq!(
            flog10_three_quarters_pow2(q),
            floor_log10(&n3, &d3),
            "flog10_three_quarters_pow2({q})"
        );
    }
    for e in -350..=350i32 {
        // floor(log2(10^e))
        let (num, den) = if e >= 0 {
            (Big::pow10(e as usize), Big::from_u64(1))
        } else {
            (Big::from_u64(1), Big::pow10((-e) as usize))
        };
        // largest f with 2^f * den <= num
        let mut f: i32 = -1300;
        loop {
            let next = f + 1;
            let ok = if next >= 0 {
                let mut l = Big::pow2(next as usize);
                mul_big(&mut l, &den);
                l.cmp(&num) != std::cmp::Ordering::Greater
            } else {
                let mut r = num.clone();
                r.shl((-next) as usize);
                den.cmp(&r) != std::cmp::Ordering::Greater
            };
            if !ok {
                break;
            }
            f = next;
        }
        assert_eq!(flog2_pow10(e), f, "flog2_pow10({e})");
    }
}

#[test]
fn emit() {
    if std::env::var("WIREFOLD_EMIT_TABLE").is_err() {
        return;
    }
    let mut s = String::new();
    s.push_str("//! Generated by `WIREFOLD_EMIT_TABLE=1 cargo test --test float_tables emit`; checked exact by\n");
    s.push_str("//! `tests/float_tables.rs`. `G[2 * (j - J_MIN)]` and `G[2 * (j - J_MIN) + 1]` are the high and low limbs of `ceil(10^j * 2^(125 - f))`, `f = floor(log2 10^j)`.\n\n");
    s.push_str(&format!(
        "pub const J_MIN: i32 = {J_MIN};\npub const J_MAX: i32 = {J_MAX};\n\n"
    ));
    s.push_str(&format!(
        "pub const G: [u64; {}] = [\n",
        2 * (J_MAX - J_MIN + 1)
    ));
    for j in J_MIN..=J_MAX {
        let g = g_exact(j);
        s.push_str(&format!(
            "    0x{:016x}, 0x{:016x}, // 10^{j}\n",
            (g >> 64) as u64,
            g as u64
        ));
    }
    s.push_str("];\n");
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/num/float_tables.rs"),
        s,
    )
    .unwrap();
}

fn big_from_u128(v: u128) -> Big {
    let mut b = Big(vec![
        v as u32,
        (v >> 32) as u32,
        (v >> 64) as u32,
        (v >> 96) as u32,
    ]);
    b.trim();
    b
}

/// Independent of the generator above: check `(g - 1) * 2^(f-125) <= 10^j < g * 2^(f-125)`
/// (cross-multiplied to stay in integers) for every table entry.
#[test]
fn table_satisfies_its_defining_inequalities() {
    for j in J_MIN..=J_MAX {
        let gi = ((j - J_MIN) as usize) * 2;
        let (hi, lo) = (G[gi], G[gi + 1]);
        let g = (hi as u128) << 64 | lo as u128;
        // f = floor(log2 10^j); 10^j = A / B with A = 10^max(j,0), B = 10^max(-j,0)
        let f = flog2_pow10(j);
        let (a, b) = (
            Big::pow10(j.max(0) as usize),
            Big::pow10((-j).max(0) as usize),
        );
        // compare (g - 1) * 2^(f-125) * B  vs  A  vs  g * 2^(f-125) * B, multiplying through by 2^(125-f) when f < 125
        let e = 125 - f; // may be negative
        let side = |gv: u128| -> (Big, Big) {
            // returns (left, right) = (gv * B * 2^max(-e,0), A * 2^max(e,0)) so that left ? right  <=>  gv*2^(f-125)*B ? A
            let mut l = big_from_u128(gv);
            mul_big(&mut l, &b);
            l.shl((-e).max(0) as usize);
            let mut r = a.clone();
            r.shl(e.max(0) as usize);
            (l, r)
        };
        // g = ceil(x): (g - 1) < x <= g, i.e. (g-1)*2^(f-125) < 10^j <= g*2^(f-125)
        let (l_lo, r) = side(g - 1);
        assert!(
            l_lo.cmp(&r) == std::cmp::Ordering::Less,
            "G[{j}]: (g-1)*2^(f-125) >= 10^j"
        );
        let (l_hi, r) = side(g);
        assert!(
            l_hi.cmp(&r) != std::cmp::Ordering::Less,
            "G[{j}]: g*2^(f-125) < 10^j"
        );
    }
}
