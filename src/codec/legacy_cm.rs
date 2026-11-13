//! Reader (and, for the round-trip tests, writer) of the context-mixing byte stream wirefold wrote up to
//! `ba9a712` (and hashtrinity before it): `[byteLen][order][stride][rANS]`, orders 0..=6, an adaptive
//! binary model with KN backoff and SSE stages. The coder was replaced by a static tANS one and the stream
//! given a version byte (`cm::VERSION`); this module is the old decoder, kept so data written by the old
//! coder stays readable. [`super::cm::try_decompress`] falls back to it for a stream that is not a
//! version-1 stream. It is not on any write path.
//!
//! The body is the old `cm` module verbatim apart from the header, the `allow`s below, the visibility and one
//! addition: the decoder checks the rANS end state (see the comment at the end of `bin_decode`).

#![allow(dead_code, clippy::all, clippy::pedantic, clippy::nursery)]

//! The context-mixing byte codec — a zero-dep port of `compressOk`/`decompressOk` in
//! `proof/Runtime/SangClone.lean`, with the context store standing on the framework's own law.
//!
//! α IS THE BIT (`UniversalEncoding`, Axiom XXII): a byte is 8 binary `B/U` collapses, coded
//! MSB-first down a bit-tree (`nd` from 1, `nd := 2·nd + bit`). For each bit, predict
//! `P(bit=1 | k-byte context, tree-node)` by an order-0 add-½ floor refined up the orders by the
//! binary KN backoff (`bin_blend`), then 3 SSE/APM stages, then a 2-symbol Null-cell rANS.
//!
//! THE CONTEXT STORE STANDS ON THE LIBRARY'S OWN PRIMITIVES — the dense/diffuse trinity, shape
//! selected per order by occupancy ([`OrderStore`]). Each order's `(n0,n1)` counter is a coordinate
//! key `(ctx % 256ᵒ)·256 + nd`; where the coordinate space `S_o` is small enough to be ~fully
//! occupied (`S_o ≤ 8n`) it rides an exact [`CoordMap`] (the **B** face, worst-probe-1, no fold),
//! and where it is a high-entropy sparse space it rides a [`FreqMap`] (the **NB** fold, `2⁻⁶⁴`
//! false-positive, load-managed growth). The exact-sparse `Table`/`Stored` (B/U) face is unused: the
//! codec never reads a key back and a `2⁻⁶⁴` mis-blend only costs a hair of ratio.
//!
//! This REPLACES the old fixed `2²²`-cap dense array: because [`FreqMap`] grows to hold the distinct
//! contexts (bounded by the buffer, not the alphabet) it de-collides the high orders the cap forced
//! to blend — a measured +6% (C4) / +9% (Lean) ratio. Lossless by construction: the maps are
//! seedless-deterministic (no `SipHash`, no random seed), so the identical encode/decode key streams // LOOP-EXEMPT: prose reference
//! grow byte-identical stores, and the count updates match.

use super::{read_uvarint, try_vec, DecodeError};
use crate::{CoordMap, FreqMap};

const PROB_M: u64 = 16384; // 1<<14 — the rANS frequency total
const RANS_L: u64 = 8_388_608; // 1<<23 — the rANS renorm floor

const SSE_BINS: usize = 33;
const SSE_LO: f64 = -8.0;
const SSE_HI: f64 = 8.0;
const SSE_RATE: f64 = 0.012;

const APM1_ROWS: u64 = 512;
const APM2_ROWS: u64 = 16384;
const APM3_ROWS: u64 = 4096;

const COUNT_CAP: u32 = 60000; // halve the cell when the count would overflow u16
const MAX_ORDER: usize = 6; // the context order is capped here (see `compress`) — so the lanes stack-alloc

// stretch (logit) and squash (sigmoid) are fixed monotone maps called 3×/bit (the APM) plus once per
// APM knot at init — tens of millions of transcendentals per MB. Tabulate them once (both sides use
// the SAME table → lossless) and read with linear interpolation: an L2 gather instead of a libm call.
const STRETCH_BITS: usize = 14; // 2^14+1 knots over p ∈ [0,1] — finer than the APM's own 33 bins
const SQUASH_BITS: usize = 14; // over s ∈ [SSE_LO, SSE_HI]

fn stretch_table() -> &'static [f64] {
    static T: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let n = 1usize << STRETCH_BITS;
        (0..=n)
            .map(|i| {
                let q = (i as f64 / n as f64).clamp(0.0001, 0.9999);
                (q / (1.0 - q)).ln()
            })
            .collect()
    })
}

fn squash_table() -> &'static [f64] {
    static T: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let n = 1usize << SQUASH_BITS;
        (0..=n)
            .map(|i| {
                let s = SSE_LO + (SSE_HI - SSE_LO) * i as f64 / n as f64;
                1.0 / (1.0 + (-s).exp())
            })
            .collect()
    })
}

#[inline]
fn stretch(p: f64) -> f64 {
    let n = 1usize << STRETCH_BITS;
    let x = p.clamp(0.0, 1.0) * n as f64;
    let i = (x as usize).min(n - 1);
    let t = stretch_table();
    t[i] + (t[i + 1] - t[i]) * (x - i as f64)
}

#[inline]
fn squash(s: f64) -> f64 {
    let n = 1usize << SQUASH_BITS;
    let x = (s.clamp(SSE_LO, SSE_HI) - SSE_LO) / (SSE_HI - SSE_LO) * n as f64;
    let i = (x as usize).min(n - 1);
    let t = squash_table();
    t[i] + (t[i + 1] - t[i]) * (x - i as f64)
}

fn bin_mods(k: usize) -> Vec<u64> {
    (0..=k).map(|o| 256u64.pow(o as u32)).collect()
}

/// Order `o`'s coordinate space `S_o` — how many `(ctx,nd)` keys it can address. Order 0 keys by the
/// tree-node `nd` (`<512`); order `o≥1` by `(ctx % 256ᵒ)·256 + nd`. Saturates for huge orders.
fn order_space(o: usize) -> u64 {
    if o == 0 {
        512
    } else {
        256u64
            .checked_pow(o as u32)
            .and_then(|m| m.checked_mul(256))
            .map_or(u64::MAX, |s| s + 512)
    }
}

/// One order's `(n0,n1)` count store — the trinity's two OCCUPIED faces, selected per order by
/// occupancy. The exact-sparse `Table`/`Stored` (B/U) face is deliberately absent: the codec never
/// reads a key back and a `2⁻⁶⁴` mis-blend only costs a hair of ratio, so per the library's own
/// guidance it never reaches for the key-storing table.
///   * `Dense` (**B**, a [`CoordMap`]) — the coordinate space is small enough to be ~fully occupied    ///     (`S_o ≤ 8n`, ≥ one bit-event per coordinate on average); every key has an exact home, no fold.
///   * `Diffuse` (**NB**, [`FreqMap`]) — a high-entropy sparse space; the growing, load-managed fold
///     (`2⁻⁶⁴` false-positive). It resizes to keep load ~0.5 — that is what de-collides the high
///     orders the old fixed `2²²` cap forced to blend (the measured +6–9% ratio).
enum OrderStore {
    Dense(CoordMap<u64, (u16, u16)>),
    Diffuse(FreqMap<u64, (u16, u16)>),
}

/// The per-bit count merge — add the incoming `(n0,n1)` event into the cell, halving both on the
/// `u16` cap (the count-aging that also tracks non-stationary data). Commutative and deterministic,
/// so the encode/decode key streams build byte-identical stores → lossless.
#[inline(always)]
fn cell_merge(a: (u16, u16), b: (u16, u16)) -> (u16, u16) {
    let (mut n0, mut n1) = (
        u32::from(a.0) + u32::from(b.0),
        u32::from(a.1) + u32::from(b.1),
    );
    if n0 + n1 > COUNT_CAP {
        n0 >>= 1;
        n1 >>= 1;
    }
    (n0 as u16, n1 as u16)
}

impl OrderStore {
    /// Select the face for order `o` over an `nbytes` buffer: dense when the whole coordinate space
    /// sees ≥ 1 event on average (`S_o ≤ 8·nbytes`), else the diffuse fold sized to the distinct
    /// contexts (`≤ 8·nbytes`). Derived identically on encode and decode (both know `nbytes`).
    fn select(o: usize, nbytes: usize) -> Self {
        let events = (nbytes as u64).saturating_mul(8).max(512);
        let s_o = order_space(o);
        if s_o <= events {
            OrderStore::Dense(CoordMap::coordinate_with_capacity(s_o as usize))
        } else {
            // grow from a modest floor — the diffuse order's distinct keys are bounded by the buffer,
            // and FreqMap resize is amortized O(1); pre-allocating the worst case was the 1.25 GB blowup.
            OrderStore::Diffuse(FreqMap::with_capacity(1 << 12))
        }
    }

    #[inline(always)]
    fn get(&self, key: u64) -> (u16, u16) {
        match self {
            OrderStore::Dense(m) => m.get(&key).copied().unwrap_or((0, 0)),
            OrderStore::Diffuse(m) => m.get(&key).copied().unwrap_or((0, 0)),
        }
    }

    #[inline(always)]
    fn bump(&mut self, key: u64, bit: u64) {
        let inc = if bit == 1 { (0u16, 1u16) } else { (1u16, 0u16) };
        match self {
            // `cell_merge` is commutative, so the argument order of the two tables' merges is moot.
            OrderStore::Dense(m) => m.insert_with(key, inc, cell_merge),
            OrderStore::Diffuse(m) => m.insert_with(&key, inc, cell_merge),
        }
    }
}

/// The suffix-web counter — one `OrderStore` per order `0..=k`, each shape-selected from `nbytes`.
type Store = Vec<OrderStore>;

fn new_store(k: usize, nbytes: usize) -> Store {
    (0..=k).map(|o| OrderStore::select(o, nbytes)).collect()
}

const GOLD: u64 = 0x9E37_79B9_7F4A_7C15; // φ-hash mix for the match-context fold
const MCTX: usize = 6; // the generator's genome window — match on the last MCTX bytes
                       // engage the fast path only for matches LONGER than the CM's own order (`MAX_ORDER`) — past that the
                       // match predicts from context the CM structurally cannot see, so it no longer competes with (and
                       // loses to) the CM on the CM's turf. Measured insensitive in [8,32]: prose keeps full ratio, repeats
                       // stay fast — a floor set by the CM order, not a tuned ratio dial.
const MATCH_MIN: usize = 16;
const CONF_ROWS: usize = 64; // match-length buckets for the learned confidence

/// **The generator — the `B` arm (`Seed.Gen`), a closed-form O(1) read-off.** Predict the next byte
/// as the one that FOLLOWED the last occurrence of the current `MCTX`-byte context — one pointer
/// deref into the already-coded history, no table probe (the last-occurrence instance of the proven
/// generator interface `Gen.decode_encode`, lossless for any predictor). Where it fits, the residual
/// → 0 and the low-order prediction freq-pins → the B-face passthrough fires → the diffuse probes are
/// skipped (O(1) on the run). Its per-length confidence (`conf`) is LEARNED, not dialed. All state
/// derives from the coded prefix, so encode and decode build it identically — lossless.
struct MatchGen {
    htab: Vec<u32>, // hash(last MCTX bytes) → position+1 (0 = empty)
    hbits: u32,
    ptr: usize,            // the read-off source: predict `hist[ptr]`
    len: usize,            // current match length (consecutive bytes agreed)
    conf: Vec<(u16, u16)>, // [len bucket] → (miss, hit) — the learned strength of a match this long
    pb: i32,               // predicted byte for the current position, or -1 if none
}

impl MatchGen {
    fn new(nbytes: usize) -> Self {
        // size the table to stay L2-RESIDENT (2¹⁶ u32 = 256 KiB) — the generator's find must be an L2
        // hit, not a DRAM miss per byte (measured: a 64 MiB table cost +42% on match-poor prose for
        // no gain). Collisions just cost a stale prediction, which the learned `conf` down-weights.
        let hbits = ((nbytes.max(1) as u64).saturating_mul(2))
            .ilog2()
            .clamp(12, 16);
        MatchGen {
            htab: vec![0u32; 1usize << hbits],
            hbits,
            ptr: 0,
            len: 0,
            conf: vec![(0u16, 0u16); CONF_ROWS],
            pb: -1,
        }
    }

    /// The slot of the `MCTX`-byte context immediately BEFORE position `end` (uses only `hist[<end]`).
    #[inline]
    fn slot(hist: &[u8], end: usize, hbits: u32) -> Option<usize> {
        if end < MCTX {
            return None;
        }
        let mut h = 0u64;
        for &b in &hist[end - MCTX..end] {
            h = (h ^ u64::from(b)).wrapping_mul(GOLD);
        }
        Some((h.wrapping_mul(GOLD) >> (64 - hbits)) as usize)
    }

    /// Set the predicted byte for coding position `i`. If not already in a match, try to start one by
    /// looking up the current context's last occurrence (the O(1) read-off).
    #[inline]
    fn begin(&mut self, hist: &[u8], i: usize) {
        if self.len == 0 {
            if let Some(h) = Self::slot(hist, i, self.hbits) {
                let s = self.htab[h] as usize;
                if s != 0 && s - 1 < i {
                    self.ptr = s - 1;
                    self.len = 1;
                }
            }
        }
        self.pb = if self.len > 0 && self.ptr < i {
            i32::from(hist[self.ptr])
        } else {
            -1
        };
    }

    /// Whether to code a byte-level hit flag: a live match, held ≥ `MATCH_MIN`, that has been RIGHT
    /// more often than wrong at this length (`hit > miss` in the learned `conf`). The flag replaces
    /// the CM only where the match out-predicts a coin — so prose's short, unreliable coincidences
    /// (which the CM models better) stay on the CM path, while reliable runs take the fast path.
    /// Self-calibrating: `finish` scores `conf` on EVERY prediction, engaged or not, so the gate
    /// warms up from evidence — a derived reliability cut, not a tuned threshold.
    #[inline]
    fn has_pred(&self) -> bool {
        if self.pb < 0 || self.len < MATCH_MIN {
            return false;
        }
        let (miss, hit) = self.conf[self.len.min(CONF_ROWS - 1)];
        hit > miss
    }

    /// The learned hit probability at the CURRENT match length — the match model's confidence, fed to
    /// the mixer as an input (for matches too short/uncertain for the bypass `has_pred`). `0.5` when no
    /// match is live (neutral). All from the learned `conf`, no dial.
    #[inline]
    fn phit_now(&self) -> f64 {
        if self.pb < 0 {
            return 0.5;
        }
        let (miss, hit) = self.conf[self.len.min(CONF_ROWS - 1)];
        (f64::from(hit) + 0.5) / (f64::from(hit) + f64::from(miss) + 1.0)
    }

    /// The rANS frequencies `(f_hit, f_miss)` for the hit flag — `p(hit)` learned for this match
    /// length (`conf[len]`), the derived strength. A long reliable match → `f_hit → probM` → the hit
    /// costs ~0 bits, and the whole byte is one O(1) event (no tree, no model probe).
    #[inline]
    fn hit_freqs(&self) -> (u64, u64) {
        let (miss, hit) = self.conf[self.len.min(CONF_ROWS - 1)];
        let p_hit = (f64::from(hit) + 0.5) / (f64::from(hit) + f64::from(miss) + 1.0);
        let f_hit = ((p_hit * PROB_M as f64) as u64).clamp(1, PROB_M - 1);
        (f_hit, PROB_M - f_hit)
    }

    /// After byte `i` is known: score the prediction into its length bucket, extend or break the
    /// match, and record this context→next-byte so future occurrences read it off. `hit` is whether
    /// the prediction was right (the caller already knows it — from the flag on the hot path).
    #[inline]
    fn finish(&mut self, hist: &[u8], i: usize) {
        if self.pb >= 0 {
            let hit = self.pb == i32::from(hist[i]);
            let bucket = self.len.min(CONF_ROWS - 1);
            let (mut miss, mut ht) = self.conf[bucket];
            if hit {
                ht = ht.saturating_add(1);
            } else {
                miss = miss.saturating_add(1);
            }
            if u32::from(miss) + u32::from(ht) > COUNT_CAP {
                miss >>= 1;
                ht >>= 1;
            }
            self.conf[bucket] = (miss, ht);
            if hit {
                self.len += 1;
                self.ptr += 1;
            } else {
                self.len = 0;
            }
        }
        if let Some(h) = Self::slot(hist, i, self.hbits) {
            self.htab[h] = (i + 1) as u32; // context-before-i was followed by byte i
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// THE LOGISTIC MIXER — learned weights in stretch (logit) space (the paq core the KN blend lacked)
// ═══════════════════════════════════════════════════════════════════
// The fixed KN backoff chain (`blend_one`) folds the orders in a FIXED hierarchy; it cannot learn
// that (say) the word model beats order-4 on English. The mixer replaces it with a single-layer
// logistic combiner: each model's prediction is stretched to a logit, the logits are weighted-summed
// (weights LEARNED per bit by the logistic gradient, selected by a context), and squashed back. The
// match model — a bypass before — is now an INPUT the mixer weighs, and a word model (context = the
// current partial word) joins the orders. This is the paq/cmix combiner; the weights are learned, not
// dialed (the `MIX_LR` step, like the match `conf`). Deterministic f64 on both sides → lossless.

// The exact orders `0..=k` (nested, correlated) are joined by HASHED high-order contexts — the
// long-range structure order ≤6 cannot reach (order 256^o overflows past 7 anyway). Each is a FNV
// hash of the last `ord` bytes, its own diffuse store; these are the orthogonal inputs the mixer needs.
const HI_ORDERS: [usize; 3] = [8, 12, 20];
/// Sparse (skip-gram) contexts `(a,b)` — hash of the bytes at `i−a`, `i−b` (a gap between them).
/// Orthogonal to the nested orders: catches a pattern that skips the immediately-preceding byte.
const SPARSE: [(usize, usize); 2] = [(1, 3), (2, 4)];
const N_HI: usize = HI_ORDERS.len() + SPARSE.len(); // 5 hashed contexts (3 high orders + 2 sparse)
const HI_BASE: usize = MAX_ORDER + 1; // inputs[HI_BASE .. HI_BASE+N_HI] are the hashed contexts
const WORD_IDX: usize = HI_BASE + N_HI;
const MATCH_IDX: usize = WORD_IDX + 1;
const N_INPUTS: usize = MATCH_IDX + 1; // orders 0..=6, 3 hi, word, match = 12
const MIX_CTX: usize = 512; // weight sets, selected by the tree node `nd` (the partial byte)
const MIX_LR: f64 = 0.01; // the mixer learning rate — the gradient step (measured sweet spot on enwik)
const WORD_MUL: u64 = 0x100_0000_01b3; // FNV-style rolling hash of the current word
const FNV_OFF: u64 = 0xcbf2_9ce4_8422_2325;

/// The hashed high-order context keys at position `i` — a FNV hash of the last `ord` bytes for each
/// `HI_ORDERS`. One read per byte (shared across its 8 bits); each order's space is separated so the
/// diffuse stores don't collide across orders.
#[inline]
fn high_ctx(hist: &[u8], i: usize) -> [u64; N_HI] {
    let mut h = [0u64; N_HI];
    for (j, &ord) in HI_ORDERS.iter().enumerate() {
        let start = i.saturating_sub(ord);
        let mut x = FNV_OFF;
        for &b in &hist[start..i] {
            x = (x ^ u64::from(b)).wrapping_mul(WORD_MUL);
        }
        h[j] = x.wrapping_mul(ord as u64 + 1);
    }
    for (j, &(a, b)) in SPARSE.iter().enumerate() {
        let ba = if i >= a { hist[i - a] } else { 0 };
        let bb = if i >= b { hist[i - b] } else { 0 };
        let mut x = (FNV_OFF ^ u64::from(ba)).wrapping_mul(WORD_MUL);
        x = (x ^ u64::from(bb)).wrapping_mul(WORD_MUL);
        h[HI_ORDERS.len() + j] = x.wrapping_mul((a * 31 + b) as u64 + 1);
    }
    h
}

// ── THE ICM STATE MAP — the bounded-count coordinate read out to a LEARNED, POOLED probability ──
// `NullCellCoding` codes a bit with a Null cell of norm P; the two outcomes are the orthogonal
// idempotents `ePlus/eMinus`. A context's `(n0,n1)` is the bounded fold of those projectors (aged at
// the cap — the recursion floor's bounded return). The ICM (cmix `indirect.cpp`) does NOT read the
// local ratio; it QUANTIZES the count to a state coordinate and reads the probability out of a global
// map LEARNED across every context sharing that state (`CoordinateReadout`: the state is the address,
// the prob is the readout). That pooling is the win — a context seen once as `(2,1)` uses the global
// `P(state)`, not its noisy local `2/3`. The paq 256-entry state table is one quantization of this
// manifold; ours is derived from the `(n0,n1)` return directly (bit-length buckets, 16×16 = 256).

const NSTATE: usize = 256; // 16 buckets of n0 × 16 of n1
const N_SM: usize = WORD_IDX + 1; // one StateMap per count-based input (orders + hi + word; not match)

/// Quantize a count to a bit-length bucket `0..=15` — the bounded coordinate of the fold (a raw count
/// is unbounded; its order of magnitude is the state that matters).
#[inline(always)]
fn qcount(c: u16) -> usize {
    (32 - (u32::from(c)).leading_zeros()).min(15) as usize
}

/// The state coordinate of a `(n0,n1)` cell — the bounded 2-D index the readout is keyed by.
#[inline(always)]
fn count_state(n0: u16, n1: u16) -> usize {
    qcount(n0) * 16 + qcount(n1)
}

/// A **StateMap** — the learned readout `state → P(bit=1)`, pooled across all contexts of one model.
/// Each state carries `(prob, count)`; the update rate slows as the state is seen (`1/(n+1.5)`), so it
/// learns fast then stabilizes — the adaptive readout, not a fixed formula.
struct StateMap {
    t: Vec<(f64, u16)>,
}
impl StateMap {
    fn new() -> Self {
        StateMap {
            t: vec![(0.5, 0u16); NSTATE],
        }
    }
    #[inline(always)]
    fn prob(&self, s: usize) -> f64 {
        self.t[s].0
    }
    #[inline(always)]
    fn learn(&mut self, s: usize, bit: u64) {
        let (p, n) = self.t[s];
        self.t[s] = (
            p + (bit as f64 - p) / (f64::from(n) + 1.5),
            (n + 1).min(1023),
        );
    }
}

/// The match model's logit input for the current tree node `nd`. If the predicted byte's prefix still
/// agrees with the path coded so far, the match predicts the next bit with strength `p_hit` (the
/// learned confidence); if the path has diverged (or no match), it is neutral (`0`).
#[inline]
fn match_input(pb: i32, p_hit: f64, nd: u64) -> f64 {
    if pb < 0 {
        return 0.0;
    }
    let bi = u64::from(nd.ilog2()); // bits of this byte coded so far (nd ∈ [1,255] ⇒ bi ∈ [0,7])
    let path = nd - (1u64 << bi);
    let pbu = pb as u64;
    if bi > 0 && (pbu >> (8 - bi)) != path {
        return 0.0; // the match's byte no longer matches the path — no prediction for this bit
    }
    let predbit = (pbu >> (7 - bi)) & 1;
    stretch(if predbit == 1 { p_hit } else { 1.0 - p_hit })
}

/// The per-bit prediction state carried from `predict` to `update` — the mixer inputs and selector,
/// the pre-APM mixed probability (the mixer trains on its own output), the word key, the APM coords.
struct MixState {
    inp: [f64; N_INPUTS],
    sel: usize,
    p_mix: f64,
    wkey: u64,
    hikeys: [u64; N_HI],
    states: [usize; N_SM], // the StateMap coordinate read for each count-based input
    apm: [(usize, f64); 3],
    gated: bool, // the monad passthrough — the cheap prediction was saturated, expensive lanes skipped
    cheap: usize, // the largest cheap order read (only 0..=cheap were touched when gated)
}

/// The cheap gate orders — the cache-resident low orders whose mix decides the monad passthrough.
const GATE_CHEAP: usize = 2;

/// The context-mixing model — the order stores, the word store, the learned mixer, the three APMs.
/// One object shared by encode and decode; both drive it identically off the coded prefix → lossless.
struct Model {
    store: Store,
    hi: Vec<FreqMap<u64, (u16, u16)>>, // the hashed high-order stores (one per HI_ORDERS)
    word: FreqMap<u64, (u16, u16)>,
    smaps: Vec<StateMap>, // the ICM readouts — one learned state→prob map per count-based input
    mixer: Vec<f64>,      // MIX_CTX × N_INPUTS, the learned logit weights
    apm1: Vec<f64>,
    apm2: Vec<f64>,
    apm3: Vec<f64>,
    k: usize,
    mods: Vec<u64>,
    lr: f64,
    gate: f64, // the monad band — skip the expensive lanes when the cheap prediction is within this of 0/1
}

impl Model {
    fn new(k: usize, nbytes: usize) -> Self {
        let lr = std::env::var("MIX_LR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(MIX_LR);
        Model {
            store: new_store(k, nbytes),
            hi: (0..N_HI).map(|_| FreqMap::with_capacity(1 << 12)).collect(),
            word: FreqMap::with_capacity(1 << 12),
            smaps: (0..N_SM).map(|_| StateMap::new()).collect(),
            mixer: vec![0.2; MIX_CTX * N_INPUTS], // a small positive prior on every model
            apm1: apm_init(APM1_ROWS),
            apm2: apm_init(APM2_ROWS),
            apm3: apm_init(APM3_ROWS),
            k,
            mods: bin_mods(k),
            lr,
            gate: std::env::var("GATE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0 / PROB_M as f64),
        }
    }

    /// Predict `P(bit=1)`. THE MONAD GATE: mix the cheap cache-resident low orders (`0..=cheap`) plus
    /// the match first; if that estimate is already within the coder's quantum `1/probM` of 0 or 1, the
    /// bit is at the monad — the expensive DIFFUSE probes (high orders, hashed contexts, word) cannot
    /// change the emitted rANS symbol — so carry it and SKIP them and the APM (ratio-neutral by the
    /// quantum, big speed). Otherwise gather the expensive lanes, full mix, APM ensemble. The gate is a
    /// function of the coded past, so encode and decode branch identically → lossless.
    #[inline]
    fn predict(
        &self,
        ctx: u64,
        hc: &[u64; N_HI],
        word_hash: u64,
        nd: u64,
        m_pb: i32,
        m_phit: f64,
    ) -> (f64, MixState) {
        let mut inp = [0.0f64; N_INPUTS];
        let mut states = [0usize; N_SM];
        let cheap = GATE_CHEAP.min(self.k);
        {
            let (n0, n1) = self.store[0].get(nd);
            let s = count_state(n0, n1);
            states[0] = s;
            inp[0] = stretch(self.smaps[0].prob(s));
        }
        for o in 1..=cheap {
            let key = (ctx % self.mods[o]) * 256 + nd;
            let (n0, n1) = self.store[o].get(key);
            let s = count_state(n0, n1);
            states[o] = s;
            inp[o] = stretch(self.smaps[o].prob(s));
        }
        inp[MATCH_IDX] = match_input(m_pb, m_phit, nd);
        let sel = (nd as usize) % MIX_CTX;
        let base = sel * N_INPUTS;
        // the cheap mix — only the low orders + match are nonzero so far
        let mut s = inp[MATCH_IDX] * self.mixer[base + MATCH_IDX];
        for (o, &x) in inp[..=cheap].iter().enumerate() {
            s += self.mixer[base + o] * x;
        }
        let p_cheap = squash(s);
        if p_cheap <= self.gate || p_cheap >= 1.0 - self.gate {
            let apm = [(0usize, 0.0f64); 3];
            return (
                p_cheap,
                MixState {
                    inp,
                    sel,
                    p_mix: p_cheap,
                    wkey: 0,
                    hikeys: [0; N_HI],
                    states,
                    apm,
                    gated: true,
                    cheap,
                },
            );
        }
        // off the monad — gather the expensive diffuse lanes and complete the mix
        for o in cheap + 1..=self.k {
            let key = (ctx % self.mods[o]) * 256 + nd;
            let (n0, n1) = self.store[o].get(key);
            let st = count_state(n0, n1);
            states[o] = st;
            inp[o] = stretch(self.smaps[o].prob(st));
        }
        let mut hikeys = [0u64; N_HI];
        for j in 0..N_HI {
            let key = hc[j].wrapping_mul(GOLD).wrapping_add(nd);
            hikeys[j] = key;
            let (n0, n1) = self.hi[j].get(&key).copied().unwrap_or((0, 0));
            let st = count_state(n0, n1);
            states[HI_BASE + j] = st;
            inp[HI_BASE + j] = stretch(self.smaps[HI_BASE + j].prob(st));
        }
        let wkey = word_hash.wrapping_mul(GOLD).wrapping_add(nd);
        {
            let (n0, n1) = self.word.get(&wkey).copied().unwrap_or((0, 0));
            let st = count_state(n0, n1);
            states[WORD_IDX] = st;
            inp[WORD_IDX] = stretch(self.smaps[WORD_IDX].prob(st));
        }
        let mut sfull = 0.0;
        for (i, &x) in inp.iter().enumerate() {
            sfull += self.mixer[base + i] * x;
        }
        let p_mix = squash(sfull);

        let (r1, b1, w1) = apm_apply(&self.apm1, nd % APM1_ROWS, p_mix);
        let q1 = (p_mix + 7.0 * r1) / 8.0;
        let sel2 = ((ctx % 16_777_216) * 769 + nd) % APM2_ROWS;
        let (r2, b2, w2) = apm_apply(&self.apm2, sel2, q1);
        let sel3 = ((ctx % 256) * 769 + nd) % APM3_ROWS;
        let (r3, b3, w3) = apm_apply(&self.apm3, sel3, q1);
        let out = (q1 + 7.0 * f64::midpoint(r2, r3)) / 8.0;
        (
            out,
            MixState {
                inp,
                sel,
                p_mix,
                wkey,
                hikeys,
                states,
                apm: [(b1, w1), (b2, w2), (b3, w3)],
                gated: false,
                cheap,
            },
        )
    }

    /// Train on the coded bit: the logistic gradient on the mixer weights (`err = bit − p_mix`), the
    /// three APM bumps, and the order + word count bumps. Same on encode and decode → lossless.
    #[inline]
    fn update(&mut self, ctx: u64, nd: u64, st: &MixState, bit: u64) {
        let err = bit as f64 - st.p_mix;
        let base = st.sel * N_INPUTS;
        let g = self.lr * err;
        for i in 0..N_INPUTS {
            self.mixer[base + i] += g * st.inp[i]; // gated: expensive inp are 0 → their weights untouched
        }
        // GATED: only the cheap lanes were read — bump just those, skip the expensive probes and APM
        // (the same skip on both sides ⇒ stores stay bit-identical ⇒ lossless).
        if st.gated {
            for o in 0..=st.cheap {
                self.smaps[o].learn(st.states[o], bit);
                let key = if o == 0 {
                    nd
                } else {
                    (ctx % self.mods[o]) * 256 + nd
                };
                self.store[o].bump(key, bit);
            }
            return;
        }
        apm_bump(&mut self.apm1, st.apm[0].0, st.apm[0].1, bit);
        apm_bump(&mut self.apm2, st.apm[1].0, st.apm[1].1, bit);
        apm_bump(&mut self.apm3, st.apm[2].0, st.apm[2].1, bit);
        // learn each StateMap readout at the state it was read from (pre-bump), then advance the
        // bounded-count state that keys it.
        for o in 0..=self.k {
            self.smaps[o].learn(st.states[o], bit);
        }
        for j in 0..N_HI {
            self.smaps[HI_BASE + j].learn(st.states[HI_BASE + j], bit);
        }
        self.smaps[WORD_IDX].learn(st.states[WORD_IDX], bit);
        self.store[0].bump(nd, bit);
        for o in 1..=self.k {
            let key = (ctx % self.mods[o]) * 256 + nd;
            self.store[o].bump(key, bit);
        }
        let inc = if bit == 1 { (0u16, 1u16) } else { (1u16, 0u16) };
        for j in 0..N_HI {
            self.hi[j].insert_with(&st.hikeys[j], inc, cell_merge);
        }
        self.word.insert_with(&st.wkey, inc, cell_merge);
    }
}

/// One lane of the binary KN backoff — the NB reserved-mass merge of an order's `(n0,n1)` into the
/// running `p1` (discount each nonzero count, reserve that mass for the lower lane).
#[inline]
fn p1_to_freq(p1: f64) -> (u64, u64) {
    let raw = (p1 * PROB_M as f64) as u64;
    let f1 = raw.clamp(1, PROB_M - 1);
    (PROB_M - f1, f1)
}

fn apm_init(n_rows: u64) -> Vec<f64> {
    let mut a = Vec::with_capacity(n_rows as usize * SSE_BINS);
    for _ in 0..n_rows {
        for b in 0..SSE_BINS {
            let s = SSE_LO + (SSE_HI - SSE_LO) * b as f64 / (SSE_BINS - 1) as f64;
            a.push(squash(s));
        }
    }
    a
}

#[inline(always)]
fn apm_apply(apm: &[f64], sel: u64, p1: f64) -> (f64, usize, f64) {
    let s = stretch(p1).clamp(SSE_LO, SSE_HI);
    let pos = (s - SSE_LO) / (SSE_HI - SSE_LO) * (SSE_BINS - 1) as f64;
    let lo = (pos as usize).min(SSE_BINS - 2);
    let w = pos - lo as f64;
    let base = sel as usize * SSE_BINS + lo;
    let p_lo = apm[base];
    let p_hi = apm[base + 1];
    (p_lo + (p_hi - p_lo) * w, base, w)
}

#[inline]
fn apm_bump(apm: &mut [f64], base: usize, w: f64, bit: u64) {
    let g = bit as f64;
    let nearer = if w < 0.5 { base } else { base + 1 };
    apm[nearer] += (g - apm[nearer]) * SSE_RATE;
}

#[inline]
fn roll_ctx(ctx: u64, s: u64, mod_k: u64) -> u64 {
    if mod_k == 1 {
        0
    } else {
        (ctx * 256 + s) % mod_k
    }
}

fn bin_model_scalars(k: usize, bytes: &[u8]) -> (Vec<u64>, Vec<u64>) {
    let mod_k = 256u64.pow(k as u32);
    let mut model = Model::new(k, bytes.len());
    let mut mg = MatchGen::new(bytes.len());
    let mut fs: Vec<u64> = Vec::with_capacity(bytes.len() * 8);
    let mut cs: Vec<u64> = Vec::with_capacity(bytes.len() * 8);
    let mut ctx: u64 = 0;
    let mut word_hash: u64 = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        mg.begin(bytes, i);
        // THE GENERATOR FAST PATH: where a match is reliable (bypass), code one hit flag and skip the
        // tree. Shorter/less-certain matches don't bypass — they feed the mixer as an input instead.
        let mut literal = true;
        if mg.has_pred() {
            let (f_hit, f_miss) = mg.hit_freqs();
            if i32::from(byte) == mg.pb {
                fs.push(f_hit);
                cs.push(f_miss);
                literal = false;
            } else {
                fs.push(f_miss);
                cs.push(0);
            }
        }
        if literal {
            let (m_pb, m_phit) = (mg.pb, mg.phit_now());
            let hc = high_ctx(bytes, i);
            let mut nd: u64 = 1;
            for bi in 0..8u32 {
                let bit = ((byte >> (7 - bi)) & 1) as u64;
                let (p1, st) = model.predict(ctx, &hc, word_hash, nd, m_pb, m_phit);
                let (f0, f1) = p1_to_freq(p1);
                if bit == 1 {
                    fs.push(f1);
                    cs.push(f0);
                } else {
                    fs.push(f0);
                    cs.push(0);
                }
                model.update(ctx, nd, &st, bit);
                nd = nd * 2 + bit;
            }
        }
        mg.finish(bytes, i);
        ctx = roll_ctx(ctx, byte as u64, mod_k);
        word_hash = if byte.is_ascii_alphabetic() {
            word_hash
                .wrapping_mul(WORD_MUL)
                .wrapping_add(u64::from(byte))
        } else {
            0
        };
    }
    (fs, cs)
}

#[inline(always)]
fn rans_encode_adaptive(fs: &[u64], cs: &[u64]) -> Vec<u8> {
    let mut x = RANS_L;
    let mut emit: Vec<u8> = Vec::new();
    let n = fs.len();
    for i in 0..n {
        let j = n - 1 - i;
        let f = fs[j];
        let c = cs[j];
        let xmax = ((RANS_L / PROB_M) << 8) * f;
        while x >= xmax {
            emit.push((x % 256) as u8);
            x /= 256;
        }
        x = (x / f) * PROB_M + (x % f) + c;
    }
    let mut out = Vec::with_capacity(emit.len() + 4);
    out.push((x % 256) as u8);
    out.push((x / 256 % 256) as u8);
    out.push((x / 65536 % 256) as u8);
    out.push((x / 16_777_216 % 256) as u8);
    for i in 0..emit.len() {
        out.push(emit[emit.len() - 1 - i]);
    }
    out
}

fn bin_decode(k: usize, stream: &[u8], n: usize) -> Result<Vec<u8>, DecodeError> {
    let mod_k = 256u64.pow(k as u32);
    let init = stream.get(..4).ok_or(DecodeError::Truncated)?;
    let mut x = u64::from(init[0])
        + u64::from(init[1]) * 256
        + u64::from(init[2]) * 65536
        + u64::from(init[3]) * 16_777_216;
    let mut model = Model::new(k, n);
    let mut mg = MatchGen::new(n);
    let mut pos = 4usize;
    let mut out: Vec<u8> = try_vec(n)?;
    let mut ctx: u64 = 0;
    let mut word_hash: u64 = 0;
    for i in 0..n {
        mg.begin(&out, i);
        let mut byte = 0u8;
        let mut literal = true;
        if mg.has_pred() {
            let (f_hit, f_miss) = mg.hit_freqs();
            let sl = x % PROB_M;
            let hit = sl >= f_miss; // symbol 1 = hit occupies [f_miss, probM)
            let (f, cum) = if hit { (f_hit, f_miss) } else { (f_miss, 0) };
            x = f * (x / PROB_M) + sl - cum;
            while x < RANS_L {
                x = x * 256 + u64::from(*stream.get(pos).ok_or(DecodeError::Truncated)?);
                pos += 1;
            }
            if hit {
                byte = mg.pb as u8;
                literal = false;
            }
        }
        if literal {
            let (m_pb, m_phit) = (mg.pb, mg.phit_now());
            let hc = high_ctx(&out, i);
            let mut nd: u64 = 1;
            for _ in 0..8 {
                let (p1, st) = model.predict(ctx, &hc, word_hash, nd, m_pb, m_phit);
                let (f0, f1) = p1_to_freq(p1);
                let sl = x % PROB_M;
                let bit: u64 = u64::from(sl >= f0);
                let (f, cum) = if bit == 1 { (f1, f0) } else { (f0, 0) };
                x = f * (x / PROB_M) + sl - cum;
                while x < RANS_L {
                    x = x * 256 + u64::from(*stream.get(pos).ok_or(DecodeError::Truncated)?);
                    pos += 1;
                }
                model.update(ctx, nd, &st, bit);
                nd = nd * 2 + bit;
            }
            byte = (nd - 256) as u8;
        }
        out.push(byte);
        mg.finish(&out, i);
        ctx = roll_ctx(ctx, byte as u64, mod_k);
        word_hash = if byte.is_ascii_alphabetic() {
            word_hash
                .wrapping_mul(WORD_MUL)
                .wrapping_add(u64::from(byte))
        } else {
            0
        };
    }
    // rANS integrity: the encoder starts from `RANS_L` and writes the final state first, so an intact
    // stream returns the decoder to exactly `RANS_L` with every byte consumed. The original decoder did
    // not check this and decoded any bytes with a plausible header to *something*; with the check,
    // damaged, truncated or foreign input is refused (which is what lets `cm::try_decompress` try
    // this decoder as a fall-back without turning a damaged new-format stream into silent garbage).
    if x != RANS_L || pos != stream.len() {
        return Err(DecodeError::Malformed);
    }
    Ok(out)
}

// ── varint header (LEB128, the operational Seed.serWord) ──
fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        if n < 128 {
            out.push(n as u8);
            break;
        }
        out.push((n % 128 + 128) as u8);
        n /= 128;
    }
}

fn get_varint(bs: &[u8], i: &mut usize) -> Result<u64, DecodeError> {
    read_uvarint(bs, i)
}

// ═══════════════════════════════════════════════════════════════════
// THE SPATIAL FLOOR — the `B` arm over a 2-D web (`Table::encircle`)
// ═══════════════════════════════════════════════════════════════════
// A 1-D coder reads an image as a byte stream and misses the vertical (row-above) correlation. The
// framework's floor is `encircle` — reduce a node's neighbour-web to one prediction. Here the web is
// the pixel's spatial neighbourhood {left, up, up-left} at the row stride, and the reduce is the MED
// (median-edge) read-off `web_encircle_is_collapse`: the argmin-residual neighbour. Predict each
// pixel from that encircle, code only the residual `U` (x = B/U at the pixel). The stride is read off
// the data's OWN resonance (min residual-entropy over candidate lags — the MDL choice, no format
// knowledge, no dial), so images/spectrograms/tables all lift the same way; 1-D data picks stride 0
// (no transform) because no lag beats the raw. Measured (256² grey): 1.81× → 2.28×, past PNG (2.07×).

const MAX_STRIDE: usize = 4096; // widest row stride the coarse scan probes; frame/motion lags come from its multiples

/// The MED (median-edge) prediction of `data[i]` from its causal spatial neighbours at stride `w`:
/// `left`, `up`, `up-left`. The JPEG-LS read-off — the encircle over the three-neighbour web (picks
/// `min`/`max` on an edge, the planar `a+b−c` on a gradient). Uses only indices `< i`, so the inverse
/// regenerates it from the pixels decoded so far.
#[inline]
fn med_predict(data: &[u8], i: usize, w: usize) -> u8 {
    let mut a = if i >= 1 { i32::from(data[i - 1]) } else { 0 }; // left
    let b = if i >= w { i32::from(data[i - w]) } else { 0 }; // up
    let mut c = if i > w { i32::from(data[i - w - 1]) } else { 0 }; // up-left
    if i.is_multiple_of(w) {
        a = b;
        c = b; // row start: no left neighbour
    }
    let pred = if c >= a.max(b) {
        a.min(b)
    } else if c <= a.min(b) {
        a.max(b)
    } else {
        a + b - c
    };
    pred as u8
}

/// Forward transform: pixel → residual (`data[i] − pred`, wrapping) at stride `w`.
fn med_forward(data: &[u8], w: usize) -> Vec<u8> {
    (0..data.len())
        .map(|i| data[i].wrapping_sub(med_predict(data, i, w)))
        .collect()
}

/// Inverse: residual → pixel, regenerating each prediction from the pixels already reconstructed.
fn med_inverse(resid: &[u8], w: usize) -> Vec<u8> {
    let mut data = vec![0u8; resid.len()];
    for i in 0..resid.len() {
        let pred = med_predict(&data, i, w);
        data[i] = resid[i].wrapping_add(pred);
    }
    data
}

/// Order-0 code length (bits) of a 256-histogram — the cheap compressibility proxy.
fn hist_bits(hist: &[u32; 256], total: u32) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let t = f64::from(total);
    hist.iter()
        .filter(|&&c| c > 0)
        .map(|&c| -f64::from(c) * (f64::from(c) / t).log2())
        .sum()
}

/// **Propose candidate strides off the data's own resonance.** A coarse scan (widening steps, so the
/// range reaches frame-size/motion lags cheaply) ranks lags by the order-0 entropy of their MED
/// residual; the TOP-K are refined to their exact minimum and returned (best-first). The row width of
/// an image, the motion-compensated frame lag of video, or empty for 1-D prose. Only PROPOSES — which
/// one (if any) actually helps is settled by coding a sample (`compress`), since order-0 can't tell a
/// MED win from an exact repeat the generator already handles, and the true minimum may not top the
/// order-0 rank (a spatial and a temporal lag both matter). Returning K makes the decide robust.
fn propose_strides(data: &[u8]) -> Vec<usize> {
    let n = data.len();
    if n < 4096 {
        return Vec::new();
    }
    let step = (n / 8192).max(1);
    let sample_bits = |w: usize, start: usize| -> f64 {
        let mut h = [0u32; 256];
        let mut tot = 0u32;
        let mut i = start;
        while i < n {
            let r = if w == 0 {
                data[i]
            } else {
                data[i].wrapping_sub(med_predict(data, i, w))
            };
            h[r as usize] += 1;
            tot += 1;
            i += step;
        }
        // bits PER SYMBOL — must normalize: large lags sample fewer positions (start = w+1), and
        // comparing total bits across sample counts spuriously favours the widest lag (fewest symbols).
        hist_bits(&h, tot) / f64::from(tot.max(1))
    };
    let base = sample_bits(0, 0);
    let max_w = (n / 2).min(MAX_STRIDE);
    // coarse grid: dense small lags → widening steps (few candidates, wide reach)
    let mut ranked: Vec<(f64, usize)> = Vec::new();
    let mut w = 2;
    while w <= max_w {
        ranked.push((sample_bits(w, w + 1), w));
        w += if w < 64 {
            1
        } else if w < 512 {
            8
        } else if w < 4096 {
            32
        } else {
            256
        };
    }
    // keep the K lags with the least residual entropy that also beat the raw by a real margin
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    const K: usize = 4;
    let mut out: Vec<usize> = Vec::new();
    for &(bits, w0) in ranked.iter().take(K) {
        if bits >= base * 0.97 {
            break; // no real gain past here (ranked ascending)
        }
        // refine to the exact minimum in a window around the coarse point (catches motion lags)
        let win = 260usize;
        let (lo, hi) = (w0.saturating_sub(win).max(2), (w0 + win).min(max_w));
        let mut best = (bits, w0);
        let mut w = lo;
        while w <= hi {
            let b = sample_bits(w, w + 1);
            if b < best.0 {
                best = (b, w);
            }
            w += 1;
        }
        if !out.contains(&best.1) {
            out.push(best.1);
        }
    }
    out
}

/// The coded size of `stream` at order `k` — used to DECIDE the spatial transform by measuring, on a
/// bounded sample, rather than trusting the order-0 proxy.
fn coded_size(k: usize, stream: &[u8]) -> usize {
    let (fs, cs) = bin_model_scalars(k, stream);
    rans_encode_adaptive(&fs, &cs).len()
}

/// Order-`k` binary compress: `[byteLen][k][stride][rANS]`. `k` capped at 6. When a 2-D stride is
/// proposed AND actually codes smaller on a sample (the generator doesn't already win), the pixels
/// are replaced by their MED spatial residual (the `encircle` floor) before coding — the byte coder
/// then rides the residual `U`. Otherwise `stride = 0` and the raw stream is coded (never a regression).
#[must_use]
pub fn compress(k: usize, data: &[u8]) -> Vec<u8> {
    let k = k.min(MAX_ORDER);
    // settle the transform by CODING a bounded sample: raw vs MED at each proposed stride, smallest
    // wins. A prefix sample, so `med_forward` on it matches the full transform's prefix. This is what
    // makes the choice robust — the generator's exact-repeat wins keep stride 0, images take their
    // spatial lag, video its motion-compensated temporal lag, all by measurement not proxy.
    let mut stride = 0usize;
    let cands = propose_strides(data);
    if !cands.is_empty() {
        let s = &data[..data.len().min(1 << 16)];
        let mut best = coded_size(k, s);
        for w in cands {
            let sz = coded_size(k, &med_forward(s, w));
            if sz < best {
                best = sz;
                stride = w;
            }
        }
    }
    let transformed;
    let stream: &[u8] = if stride > 0 {
        transformed = med_forward(data, stride);
        &transformed
    } else {
        data
    };
    let (fs, cs) = bin_model_scalars(k, stream);
    let enc = rans_encode_adaptive(&fs, &cs);
    let mut out = Vec::new();
    put_varint(&mut out, data.len() as u64);
    put_varint(&mut out, k as u64);
    put_varint(&mut out, stride as u64);
    out.extend_from_slice(&enc);
    out
}

/// Inverse of [`compress`] — reads `byteLen`, `k`, `stride`, decodes the stream, and (if a stride was
/// stored) inverts the MED spatial floor to recover the pixels.
///
/// Malformed input yields an empty `Vec` (never a panic); [`try_decompress`] says why.
#[must_use]
pub fn decompress(bs: &[u8]) -> Vec<u8> {
    try_decompress(bs).unwrap_or_default()
}

/// The most output bytes one coded byte can carry: every binary decision costs at least
/// `log2(PROB_M / (PROB_M - 1))` bits (the frequency clamp in `p1_to_freq`), and a byte is at
/// least one decision, so `n` output bytes need `n / (8 · PROB_M · ln 2)` stream bytes. The factor
/// below is that bound rounded up; a header that claims more cannot be a stream this coder wrote.
const MAX_EXPANSION: usize = 8 * PROB_M as usize;

/// Checked [`decompress`]: `Err` for truncated or inconsistent input. The declared output length
/// is bounded by the stream size ([`MAX_EXPANSION`]) before anything is allocated.
pub fn try_decompress(bs: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut p = 0usize;
    let n = get_varint(bs, &mut p)?;
    let k = get_varint(bs, &mut p)?;
    let stride = get_varint(bs, &mut p)?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let stream = &bs[p..];
    let n = usize::try_from(n).map_err(|_| DecodeError::TooLarge)?;
    if n > stream.len().saturating_mul(MAX_EXPANSION) {
        return Err(DecodeError::TooLarge);
    }
    let k = usize::try_from(k).map_err(|_| DecodeError::Malformed)?;
    if k > MAX_ORDER {
        return Err(DecodeError::Malformed);
    }
    let stride = usize::try_from(stride).map_err(|_| DecodeError::Malformed)?;
    let stream = bin_decode(k, stream, n)?;
    Ok(if stride > 0 {
        med_inverse(&stream, stride)
    } else {
        stream
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm_roundtrip() {
        for case in [
            vec![],
            vec![0u8; 4000],
            b"the quick brown fox jumps over the lazy dog. ".repeat(200),
            (0..=255u8).cycle().take(9000).collect(),
        ] {
            for k in [1usize, 3, 5] {
                let enc = compress(k, &case);
                assert_eq!(
                    decompress(&enc),
                    case,
                    "cm round-trip failed at k={k}, len={}",
                    case.len()
                );
            }
        }
    }

    /// The spatial floor must be lossless AND self-selecting: a synthetic 2-D gradient image round-
    /// trips through the MED transform (stride detected), while a 1-D text stream is left untransformed
    /// (stride 0 — no lag beats the raw).
    #[test]
    fn cm_spatial_floor() {
        let w = 128usize;
        let img: Vec<u8> = (0..w * 200)
            .map(|i| (((i / w) * 3 + (i % w) * 2) % 256) as u8)
            .collect();
        // the spatial floor must be lossless, and must actually shrink the 2-D image vs 1-D coding
        assert_eq!(
            decompress(&compress(5, &img)),
            img,
            "image round-trip failed"
        );
        assert!(
            !propose_strides(&img).is_empty(),
            "a 2-D gradient must propose a stride"
        );
        // structureless bytes must round-trip (the decide step keeps whichever is smaller)
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..20000)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s % 251) as u8
            })
            .collect();
        assert_eq!(
            decompress(&compress(5, &noise)),
            noise,
            "noise round-trip failed"
        );
    }

    /// The generator (match) B-arm must stay lossless where it fires hardest — long exact repeats,
    /// mixed with breaks. Both sides rebuild the match state from the coded prefix, so it round-trips.
    #[test]
    fn cm_generator_lossless() {
        let mut rng = 0x2545_F491_4F6C_DD1Du64;
        let block: Vec<u8> = (0..977).map(|i| (i * 31 % 251) as u8).collect();
        let mut data = block.repeat(40); // deep repetition — the generator's domain
        for _ in 0..3000 {
            // splice in breaks so matches start/stop, exercising begin/after_byte re-find
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let p = (rng as usize) % data.len();
            data[p] = (rng >> 33) as u8;
        }
        for k in [1usize, 3, 5, 6] {
            assert_eq!(
                decompress(&compress(k, &data)),
                data,
                "generator round-trip failed at k={k}"
            );
        }
    }
}
