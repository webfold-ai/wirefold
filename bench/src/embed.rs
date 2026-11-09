//! Embedding storage and scan experiment: how a dense embedding column should be stored, from the apeiron basis.
//!
//! What it measures, per storage format, on one corpus:
//!   - bytes per vector, and how much a general entropy coder (wirefold's static tANS, zstd) can still take off;
//!   - encode and decode throughput (static partition of the rows over `par::Pool`, no shared state);
//!   - exact top-k scan latency (each task scans a contiguous row range into its own top-k; the lists merge
//!     associatively), single-core ns per row, and recall@10 against the exact f32 answer;
//!   - the two-stage read sangdb's `mrl` face does: scan a small column, re-rank the shortlist at f32.
//!
//! The corpus is SYNTHETIC (clustered, with a decaying per-dimension variance spectrum so prefixes carry most
//! of the energy the way a Matryoshka-trained model's do; `iso` is the isotropic worst case with no structure).
//! No trained model is involved, so recall figures say how the formats behave on this generator, not on any
//! real embedding model. The sizes, throughputs and scan times do not depend on the data.
//!
//! Usage: embed [n] [corpus: mrl|iso] [queries]

use std::sync::Mutex;
use std::time::Instant;
use wirefold::codec::cm;
use wirefold::par::Pool;

const D: usize = 1024;
const K: usize = 10;
/// Shortlist multiplier of the two-stage read (sangdb's `mrl_search` default is `k * 8`; 10 is used here).
const RERANK: usize = 10;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    /// Two independent standard normals (Box-Muller).
    fn normal2(&mut self) -> (f32, f32) {
        let r = (-2.0 * self.unit().ln()).sqrt();
        let t = std::f64::consts::TAU * self.unit();
        ((r * t.cos()) as f32, (r * t.sin()) as f32)
    }
}

/// Run `f(chunk index, chunk)` over disjoint chunks of `data`, one pool task per chunk.
fn par_chunks_mut<T: Send>(data: &mut [T], chunk: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let slots: Vec<Mutex<&mut [T]>> = data.chunks_mut(chunk).map(Mutex::new).collect();
    Pool::global().dispatch(slots.len(), |t| f(t, &mut slots[t].lock().unwrap()));
}

#[derive(Clone, Copy, PartialEq)]
enum Corpus {
    Mrl,
    Iso,
}

const CLUSTERS: usize = 512;

fn spectrum(c: Corpus, j: usize) -> f32 {
    match c {
        Corpus::Mrl => 1.0 / ((1 + j) as f32).sqrt(),
        Corpus::Iso => 1.0,
    }
}

fn centroids(c: Corpus) -> Vec<f32> {
    let mut r = Rng(0xC0FFEE);
    let mut v = vec![0f32; CLUSTERS * D];
    for (j, x) in v.iter_mut().enumerate() {
        *x = r.normal2().0 * spectrum(c, j % D);
    }
    v
}

/// `n` unit vectors: a cluster centroid plus noise, both shaped by the spectrum. Rows are generated in
/// fixed chunks with per-chunk seeds, so the corpus does not depend on the thread count.
fn gen(c: Corpus, n: usize, seed: u64) -> Vec<f32> {
    let cent = centroids(c);
    let (wc, wn) = match c {
        Corpus::Mrl => (0.8f32, 0.6f32),
        Corpus::Iso => (0.0, 1.0),
    };
    let mut out = vec![0f32; n * D];
    par_chunks_mut(&mut out, 256 * D, |t, rows| {
        let mut r = Rng(seed ^ (t as u64).wrapping_mul(0xA24B_AED4_963E_E407));
        for row in rows.chunks_mut(D) {
            let cl = (r.next() % CLUSTERS as u64) as usize;
            let base = &cent[cl * D..(cl + 1) * D];
            for j in (0..D).step_by(2) {
                let (a, b) = r.normal2();
                row[j] = wc * base[j] + wn * a * spectrum(c, j);
                row[j + 1] = wc * base[j + 1] + wn * b * spectrum(c, j + 1);
            }
            normalize(row);
        }
    });
    out
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

// ---------- the rotation (sangdb's TurboQuant step: signs + normalised Walsh-Hadamard, twice) ----------

fn fwht(a: &mut [f32]) {
    let m = a.len();
    let mut len = 1;
    while len < m {
        let mut i = 0;
        while i < m {
            for j in i..i + len {
                let (x, y) = (a[j], a[j + len]);
                a[j] = x + y;
                a[j + len] = x - y;
            }
            i += len << 1;
        }
        len <<= 1;
    }
    let s = 1.0 / (m as f32).sqrt();
    a.iter_mut().for_each(|x| *x *= s);
}

struct Rot {
    s1: Vec<f32>,
    s2: Vec<f32>,
}
impl Rot {
    fn new() -> Rot {
        let mut r = Rng(0x5EED);
        let mut signs = || (0..D).map(|_| if r.next() & 1 == 0 { 1.0 } else { -1.0 }).collect::<Vec<f32>>();
        let s1 = signs();
        let s2 = signs();
        Rot { s1, s2 }
    }
    fn apply(&self, x: &[f32], out: &mut [f32]) {
        for j in 0..D {
            out[j] = x[j] * self.s1[j];
        }
        fwht(out);
        for j in 0..D {
            out[j] *= self.s2[j];
        }
        fwht(out);
    }
}

// ---------- half precision, by hand (stable Rust has no f16 type to lean on) ----------

fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let s = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32;
    let m = b & 0x7f_ffff;
    if e == 255 {
        return s | 0x7c00 | if m != 0 { 0x200 } else { 0 };
    }
    let ne = e - 127 + 15;
    if ne >= 31 {
        return s | 0x7c00;
    }
    if ne <= 0 {
        if ne < -10 {
            return s;
        }
        let m = m | 0x80_0000;
        let sh = (14 - ne) as u32;
        let mut r = m >> sh;
        let rem = m & ((1 << sh) - 1);
        let half = 1 << (sh - 1);
        if rem > half || (rem == half && r & 1 == 1) {
            r += 1;
        }
        return s | r as u16;
    }
    let mut r = ((ne as u32) << 10) | (m >> 13);
    let rem = m & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && r & 1 == 1) {
        r += 1;
    }
    s | r as u16
}

fn f16_to_f32(h: u16) -> f32 {
    let s = ((h as u32) & 0x8000) << 16;
    let e = ((h >> 10) & 0x1f) as u32;
    let m = (h & 0x3ff) as u32;
    if e == 0 {
        let v = m as f32 * (1.0 / 16_777_216.0);
        return if s != 0 { -v } else { v };
    }
    if e == 31 {
        return f32::from_bits(s | 0x7f80_0000 | (m << 13));
    }
    f32::from_bits(s | ((e + 112) << 23) | (m << 13))
}

// ---------- formats ----------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    F32,
    F16,
    /// int8 with a per-vector scale, over the first `dim` coordinates re-normalised (dim = D is the full vector).
    I8(usize),
    I4,
    /// 2 bits per coordinate after the rotation, fixed Gaussian Lloyd-Max levels (sangdb's TurboQuant cell).
    B2,
    /// the sign of each rotated coordinate.
    Bin,
}

const B2_LEVELS: [f32; 4] = [-1.510, -0.4528, 0.4528, 1.510];
const B2_EDGE: f32 = 0.9816;

impl Kind {
    fn name(self) -> String {
        match self {
            Kind::F32 => "f32".into(),
            Kind::F16 => "f16".into(),
            Kind::I8(D) => "int8".into(),
            Kind::I8(p) => format!("mrl{p} int8"),
            Kind::I4 => "int4".into(),
            Kind::B2 => "2-bit rot".into(),
            Kind::Bin => "binary rot".into(),
        }
    }
    fn stride(self) -> usize {
        match self {
            Kind::F32 => 4 * D,
            Kind::F16 => 2 * D,
            Kind::I8(p) => 4 + p,
            Kind::I4 => 4 + D / 2,
            Kind::B2 => D / 4,
            Kind::Bin => D / 8,
        }
    }
}

fn int_scale(x: &[f32], qmax: f32) -> f32 {
    let m = x.iter().fold(0f32, |a, v| a.max(v.abs()));
    if m == 0.0 {
        1.0
    } else {
        m / qmax
    }
}

fn encode_row(kind: Kind, rot: &Rot, x: &[f32], out: &mut [u8], tmp: &mut [f32]) {
    match kind {
        Kind::F32 => {
            for (o, v) in out.chunks_exact_mut(4).zip(x) {
                o.copy_from_slice(&v.to_le_bytes());
            }
        }
        Kind::F16 => {
            for (o, v) in out.chunks_exact_mut(2).zip(x) {
                o.copy_from_slice(&f32_to_f16(*v).to_le_bytes());
            }
        }
        Kind::I8(p) => {
            let t = &mut tmp[..p];
            t.copy_from_slice(&x[..p]);
            normalize(t);
            let s = int_scale(t, 127.0);
            out[..4].copy_from_slice(&s.to_le_bytes());
            for (o, v) in out[4..].iter_mut().zip(t.iter()) {
                *o = (v / s).round().clamp(-127.0, 127.0) as i8 as u8;
            }
        }
        Kind::I4 => {
            let s = int_scale(x, 7.0);
            out[..4].copy_from_slice(&s.to_le_bytes());
            for (o, p) in out[4..].iter_mut().zip(x.chunks_exact(2)) {
                let c = |v: f32| ((v / s).round().clamp(-7.0, 7.0) as i32 + 8) as u8;
                *o = c(p[0]) | (c(p[1]) << 4);
            }
        }
        Kind::B2 => {
            rot.apply(x, tmp);
            let g = (D as f32).sqrt();
            for (o, q) in out.iter_mut().zip(tmp.chunks_exact(4)) {
                let c = |v: f32| {
                    let z = v * g;
                    if z < -B2_EDGE {
                        0u8
                    } else if z < 0.0 {
                        1
                    } else if z < B2_EDGE {
                        2
                    } else {
                        3
                    }
                };
                *o = c(q[0]) | (c(q[1]) << 2) | (c(q[2]) << 4) | (c(q[3]) << 6);
            }
        }
        Kind::Bin => {
            rot.apply(x, tmp);
            for (o, q) in out.iter_mut().zip(tmp.chunks_exact(8)) {
                let mut b = 0u8;
                for (i, v) in q.iter().enumerate() {
                    b |= ((*v >= 0.0) as u8) << i;
                }
                *o = b;
            }
        }
    }
}

/// Reconstruct a row as f32 (for B2 and Bin, in the rotated domain: the inverse rotation is not applied).
fn decode_row(kind: Kind, b: &[u8], out: &mut [f32]) {
    match kind {
        Kind::F32 => {
            for (o, c) in out.iter_mut().zip(b.chunks_exact(4)) {
                *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
        }
        Kind::F16 => {
            for (o, c) in out.iter_mut().zip(b.chunks_exact(2)) {
                *o = f16_to_f32(u16::from_le_bytes([c[0], c[1]]));
            }
        }
        Kind::I8(_) => {
            let s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            for (o, c) in out.iter_mut().zip(&b[4..]) {
                *o = *c as i8 as f32 * s;
            }
        }
        Kind::I4 => {
            let s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            for (o, c) in out.chunks_exact_mut(2).zip(&b[4..]) {
                o[0] = ((c & 15) as i32 - 8) as f32 * s;
                o[1] = ((c >> 4) as i32 - 8) as f32 * s;
            }
        }
        Kind::B2 => {
            let g = 1.0 / (D as f32).sqrt();
            for (o, c) in out.chunks_exact_mut(4).zip(b) {
                for i in 0..4 {
                    o[i] = B2_LEVELS[((c >> (2 * i)) & 3) as usize] * g;
                }
            }
        }
        Kind::Bin => {
            let g = 1.0 / (D as f32).sqrt();
            for (o, c) in out.chunks_exact_mut(8).zip(b) {
                for i in 0..8 {
                    o[i] = if (c >> i) & 1 == 1 { g } else { -g };
                }
            }
        }
    }
}

struct Store {
    kind: Kind,
    n: usize,
    bytes: Vec<u8>,
}

fn encode_all(kind: Kind, rot: &Rot, x: &[f32]) -> Store {
    let n = x.len() / D;
    let st = kind.stride();
    let mut bytes = vec![0u8; n * st];
    const ROWS: usize = 1024;
    par_chunks_mut(&mut bytes, ROWS * st, |t, out| {
        let mut tmp = vec![0f32; D];
        for (i, o) in out.chunks_mut(st).enumerate() {
            let r = t * ROWS + i;
            encode_row(kind, rot, &x[r * D..(r + 1) * D], o, &mut tmp);
        }
    });
    Store { kind, n, bytes }
}

fn decode_all(s: &Store, out: &mut [f32]) {
    let st = s.kind.stride();
    const ROWS: usize = 1024;
    par_chunks_mut(out, ROWS * D, |t, o| {
        for (i, row) in o.chunks_mut(D).enumerate() {
            let r = t * ROWS + i;
            decode_row(s.kind, &s.bytes[r * st..(r + 1) * st], row);
        }
    });
}

// ---------- scoring ----------

struct Prep {
    /// full-precision query (rotated for B2 / Bin; prefix re-normalised for I8(p) is in `qi`)
    qf: Vec<f32>,
    qi: Vec<i8>,
    qs: f32,
    qbits: Vec<u64>,
}

fn prep(kind: Kind, rot: &Rot, q: &[f32]) -> Prep {
    let mut p = Prep { qf: Vec::new(), qi: Vec::new(), qs: 0.0, qbits: Vec::new() };
    match kind {
        Kind::F32 | Kind::F16 => p.qf = q.to_vec(),
        Kind::I8(d) => {
            let mut t = q[..d].to_vec();
            normalize(&mut t);
            let s = int_scale(&t, 127.0);
            p.qs = s;
            p.qi = t.iter().map(|v| (v / s).round().clamp(-127.0, 127.0) as i8).collect();
        }
        Kind::I4 => {
            let s = int_scale(q, 127.0);
            p.qs = s;
            p.qi = q.iter().map(|v| (v / s).round().clamp(-127.0, 127.0) as i8).collect();
        }
        Kind::B2 => {
            p.qf = vec![0.0; D];
            rot.apply(q, &mut p.qf);
        }
        Kind::Bin => {
            let mut t = vec![0.0; D];
            rot.apply(q, &mut t);
            p.qbits = t
                .chunks_exact(64)
                .map(|c| c.iter().enumerate().fold(0u64, |a, (i, v)| a | (((*v >= 0.0) as u64) << i)))
                .collect();
        }
    }
    p
}

struct Top {
    v: Vec<(f32, u32)>,
    k: usize,
    min: f32,
}
impl Top {
    fn new(k: usize) -> Top {
        Top { v: Vec::with_capacity(k + 1), k, min: f32::NEG_INFINITY }
    }
    #[inline]
    fn push(&mut self, s: f32, i: u32) {
        if s <= self.min {
            return;
        }
        let at = self.v.partition_point(|e| e.0 >= s);
        self.v.insert(at, (s, i));
        if self.v.len() > self.k {
            self.v.pop();
        }
        if self.v.len() == self.k {
            self.min = self.v[self.k - 1].0;
        }
    }
}

fn scan_range(s: &Store, p: &Prep, lo: usize, hi: usize, k: usize) -> Vec<(f32, u32)> {
    let st = s.kind.stride();
    let mut top = Top::new(k);
    for r in lo..hi {
        let b = &s.bytes[r * st..(r + 1) * st];
        let sc = match s.kind {
            Kind::F32 => b.chunks_exact(4).zip(&p.qf).map(|(c, q)| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) * q).sum::<f32>(),
            Kind::F16 => b.chunks_exact(2).zip(&p.qf).map(|(c, q)| f16_to_f32(u16::from_le_bytes([c[0], c[1]])) * q).sum::<f32>(),
            Kind::I8(_) => {
                let vs = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                let dot: i32 = b[4..].iter().zip(&p.qi).map(|(c, q)| (*c as i8 as i32) * (*q as i32)).sum();
                dot as f32 * vs * p.qs
            }
            Kind::I4 => {
                let vs = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                let mut dot = 0i32;
                for (c, q) in b[4..].iter().zip(p.qi.chunks_exact(2)) {
                    dot += ((c & 15) as i32 - 8) * q[0] as i32 + ((c >> 4) as i32 - 8) * q[1] as i32;
                }
                dot as f32 * vs * p.qs
            }
            Kind::B2 => {
                let mut acc = 0f32;
                for (c, q) in b.iter().zip(p.qf.chunks_exact(4)) {
                    let c = *c as usize;
                    acc += q[0] * B2_LEVELS[c & 3]
                        + q[1] * B2_LEVELS[(c >> 2) & 3]
                        + q[2] * B2_LEVELS[(c >> 4) & 3]
                        + q[3] * B2_LEVELS[c >> 6];
                }
                acc
            }
            Kind::Bin => {
                let mut ham = 0u32;
                for (c, q) in b.chunks_exact(8).zip(&p.qbits) {
                    ham += (u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) ^ q).count_ones();
                }
                D as f32 - 2.0 * ham as f32
            }
        };
        top.push(sc, r as u32);
    }
    top.v
}

/// Static partition of the rows over the pool; each task owns a contiguous range and its own top-k. The
/// lists merge by (score desc, id asc), which is associative and commutative, so the answer does not depend
/// on the schedule.
fn scan_par(s: &Store, p: &Prep, k: usize) -> Vec<(f32, u32)> {
    let w = Pool::global().width().max(1);
    let per = s.n.div_ceil(w);
    let parts: Vec<Mutex<Vec<(f32, u32)>>> = (0..w).map(|_| Mutex::new(Vec::new())).collect();
    Pool::global().dispatch(w, |t| {
        let (lo, hi) = ((t * per).min(s.n), ((t + 1) * per).min(s.n));
        *parts[t].lock().unwrap() = scan_range(s, p, lo, hi, k);
    });
    let mut all: Vec<(f32, u32)> = parts.into_iter().flat_map(|m| m.into_inner().unwrap()).collect();
    all.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    all.truncate(k);
    all
}

fn exact_dot(f32s: &[f32], q: &[f32], id: u32) -> f32 {
    f32s[id as usize * D..(id as usize + 1) * D].iter().zip(q).map(|(a, b)| a * b).sum()
}

/// Coarse scan of `coarse`, then an f32 re-rank of the shortlist against the master vectors.
fn two_stage(coarse: &Store, cp: &Prep, master: &[f32], q: &[f32]) -> Vec<(f32, u32)> {
    let short = scan_par(coarse, cp, K * RERANK);
    let mut v: Vec<(f32, u32)> = short.iter().map(|&(_, id)| (exact_dot(master, q, id), id)).collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    v.truncate(K);
    v
}

fn recall(got: &[(f32, u32)], truth: &[(f32, u32)]) -> f64 {
    got.iter().filter(|g| truth.iter().any(|t| t.1 == g.1)).count() as f64 / truth.len() as f64
}

// ---------- measurement helpers ----------

fn median3(mut v: [f64; 3]) -> (f64, f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[1], v[0], v[2])
}

fn time3<R>(mut f: impl FnMut() -> R) -> (f64, f64, f64) {
    let mut t = [0.0; 3];
    for x in &mut t {
        let s = Instant::now();
        std::hint::black_box(f());
        *x = s.elapsed().as_secs_f64();
    }
    median3(t)
}

fn shuffle(d: &[u8], w: usize) -> Vec<u8> {
    let n = d.len() / w;
    let mut o = vec![0u8; d.len()];
    for i in 0..n {
        for j in 0..w {
            o[j * n + i] = d[i * w + j];
        }
    }
    o
}

fn selfcheck(rot: &Rot) {
    // half precision round trip stays within half an ulp (2^-11 relative) on normal values
    let mut r = Rng(1);
    for _ in 0..100_000 {
        let v = (r.normal2().0) * 0.05;
        let back = f16_to_f32(f32_to_f16(v));
        assert!((back - v).abs() <= v.abs() * 4.9e-4 + 6.0e-8, "f16 {v} -> {back}");
    }
    // the rotation preserves norm and inner products
    let a = gen(Corpus::Mrl, 2, 7);
    let (mut ra, mut rb) = (vec![0f32; D], vec![0f32; D]);
    rot.apply(&a[..D], &mut ra);
    rot.apply(&a[D..], &mut rb);
    let d0: f32 = a[..D].iter().zip(&a[D..]).map(|(x, y)| x * y).sum();
    let d1: f32 = ra.iter().zip(&rb).map(|(x, y)| x * y).sum();
    assert!((d0 - d1).abs() < 1e-4, "rotation changed an inner product: {d0} vs {d1}");
    // the parallel scan is the serial scan, and a stored vector finds itself first at every format
    let x = gen(Corpus::Mrl, 5000, 9);
    for kind in [Kind::F32, Kind::F16, Kind::I8(D), Kind::I8(128), Kind::I4, Kind::B2, Kind::Bin] {
        let s = encode_all(kind, rot, &x);
        for qi in [3usize, 4097] {
            let q = &x[qi * D..(qi + 1) * D];
            let p = prep(kind, rot, q);
            let serial = scan_range(&s, &p, 0, s.n, K);
            let par = scan_par(&s, &p, K);
            let ids = |v: &[(f32, u32)]| v.iter().map(|e| e.1).collect::<Vec<_>>();
            assert_eq!(ids(&serial), ids(&par), "{kind:?} parallel scan differs");
            assert_eq!(par[0].1, qi as u32, "{kind:?} does not find a stored vector first");
        }
    }
}

fn gb(b: usize) -> f64 {
    b as f64 / 1e9
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let corpus = match a.get(2).map(String::as_str) {
        Some("iso") => Corpus::Iso,
        _ => Corpus::Mrl,
    };
    let nq: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);
    let w = Pool::global().width();
    let rot = Rot::new();
    selfcheck(&rot);
    println!("<!-- selfcheck passed -->");
    println!("## corpus {} n={n} d={D} queries={nq} threads={w}\n", if corpus == Corpus::Mrl { "mrl-like (synthetic)" } else { "isotropic (synthetic)" });

    let x = gen(corpus, n, 42);
    let queries = gen(corpus, nq, 4242);
    let f32s = encode_all(Kind::F32, &rot, &x);
    let truth: Vec<Vec<(f32, u32)>> = (0..nq)
        .map(|i| {
            let q = &queries[i * D..(i + 1) * D];
            scan_par(&f32s, &prep(Kind::F32, &rot, q), K)
        })
        .collect();

    let kinds = [Kind::F32, Kind::F16, Kind::I8(D), Kind::I4, Kind::B2, Kind::Bin, Kind::I8(256), Kind::I8(128), Kind::I8(64)];
    let mut rows = Vec::new();
    for kind in kinds {
        let (enc, _, _) = time3(|| encode_all(kind, &rot, &x));
        let s = encode_all(kind, &rot, &x);
        let mut dec = vec![0f32; n * D];
        let (decs, _, _) = if kind == Kind::F32 {
            // f32 "decode" is a copy into an owned buffer; borrowing the stored bytes costs nothing at all
            time3(|| {
                par_chunks_mut(&mut dec, 1024 * D, |t, o| {
                    for (i, row) in o.chunks_mut(D).enumerate() {
                        let r = t * 1024 + i;
                        decode_row(Kind::F32, &s.bytes[r * 4 * D..(r + 1) * 4 * D], row);
                    }
                })
            })
        } else if matches!(kind, Kind::I8(p) if p != D) {
            (f64::NAN, 0.0, 0.0)
        } else {
            time3(|| decode_all(&s, &mut dec))
        };
        drop(dec);

        // scan latency per query (all cores), medians of three passes over the queries
        let preps: Vec<Prep> = (0..nq).map(|i| prep(kind, &rot, &queries[i * D..(i + 1) * D])).collect();
        let (lat, lo, hi) = time3(|| preps.iter().for_each(|p| drop(std::hint::black_box(scan_par(&s, p, K)))));
        // one core, one quarter-ish slice, per row
        let slice = n / w;
        let (one, _, _) = time3(|| preps.iter().take(8).for_each(|p| drop(std::hint::black_box(scan_range(&s, p, 0, slice, K)))));
        let ns_row = one * 1e9 / (8.0 * slice as f64);
        // recall of the scan alone, and of scan + f32 re-rank
        let mut r_scan = 0.0;
        let mut r_two = 0.0;
        for i in 0..nq {
            let q = &queries[i * D..(i + 1) * D];
            r_scan += recall(&scan_par(&s, &preps[i], K), &truth[i]);
            r_two += recall(&two_stage(&s, &preps[i], &x, q), &truth[i]);
        }
        let (lat2, _, _) = time3(|| {
            for i in 0..nq {
                drop(std::hint::black_box(two_stage(&s, &preps[i], &x, &queries[i * D..(i + 1) * D])));
            }
        });
        // what an entropy coder can still take off the stored column (first 20k rows)
        let samp = &s.bytes[..s.bytes.len().min(20_000 * kind.stride())];
        let plane = match kind {
            Kind::F32 => Some(shuffle(samp, 4)),
            Kind::F16 => Some(shuffle(samp, 2)),
            _ => None,
        };
        let tans = cm::compress(cm::Order::Zero, plane.as_deref().unwrap_or(samp));
        let zs = zstd::encode_all(plane.as_deref().unwrap_or(samp), 3).unwrap();
        let (tdec, _, _) = time3(|| cm::decompress(&tans));
        let (zdec, _, _) = time3(|| zstd::decode_all(&zs[..]).unwrap());
        rows.push((kind, s.kind.stride(), enc, decs, lat, lo, hi, ns_row, r_scan / nq as f64, r_two / nq as f64, lat2, samp.len(), tans.len(), zs.len(), tdec, zdec, plane.is_some()));
    }

    let raw_mb = (n * D * 4) as f64 / 1e6;
    println!("### storage, encode and decode ({} MB of f32 in)\n", raw_mb as u64);
    println!("| format | B/vec | vs f32 | encode MB/s (in) | decode MB/s (f32 out) | tANS ratio | zstd-3 ratio | tANS dec MB/s | zstd dec MB/s |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for r in &rows {
        let (kind, st, enc, decs, _, _, _, _, _, _, _, sl, tl, zl, td, zd, shuf) = *r;
        let dec = if decs.is_nan() { "n/a".to_string() } else { format!("{:.0}", raw_mb / decs) };
        println!(
            "| {}{} | {} | {:.1}x | {:.0} | {} | {:.3} | {:.3} | {:.0} | {:.0} |",
            kind.name(),
            if shuf { " (byte planes)" } else { "" },
            st,
            (4 * D) as f64 / st as f64,
            raw_mb / enc,
            dec,
            tl as f64 / sl as f64,
            zl as f64 / sl as f64,
            sl as f64 / td / 1e6,
            sl as f64 / zd / 1e6
        );
    }
    println!("\n### scan: exact top-{K} over {n} vectors, per query (median of 3 passes of {nq} queries; range in brackets)\n");
    println!("| format | scan ms (all cores) | scan GB/s | 1-core ns/row | recall@{K}, scan only | recall@{K}, scan + f32 re-rank x{RERANK} | scan + re-rank ms |");
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for r in &rows {
        let (kind, st, _, _, lat, lo, hi, ns, rs, rt, lat2, ..) = *r;
        let q = nq as f64;
        println!(
            "| {} | {:.2} [{:.2}-{:.2}] | {:.1} | {:.0} | {:.3} | {:.3} | {:.2} |",
            kind.name(),
            lat * 1e3 / q,
            lo * 1e3 / q,
            hi * 1e3 / q,
            gb(n * st) / (lat / q),
            ns,
            rs,
            rt,
            lat2 * 1e3 / q
        );
    }
    println!("\nmachine-readable: threads={w} n={n}");
    for r in &rows {
        let (kind, st, _, _, lat, _, _, ns, _, rt, lat2, ..) = *r;
        println!("ROW\t{}\t{}\t{:.6}\t{:.1}\t{:.4}\t{:.6}", kind.name(), st, lat / nq as f64, ns, rt, lat2 / nq as f64);
    }
}
