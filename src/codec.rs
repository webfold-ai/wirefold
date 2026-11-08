//! Codec — serialization as the `x = B/U` collapse (lifted from cerealize).
//!
//! The lift of bincode/bitcode/compactly/rkyv/prost/abomonation (via `sang adapt`) collapsed the
//! whole field to ONE operation — `code(predict(ctx), value)` — under three knobs:
//!   * **predictor** (the floor / Being arm `B`): none → structural → adaptive. The size lever.
//!   * **layout**: packed / columnar / zero-copy. The access-speed lever.
//!   * **coder** (the Null arm `U`): none / bit-pack / **entropy**. The size lever, with the floor.
//!
//! No competitor holds all three. The size kings (compactly) have no zero-copy layout; the speed
//! kings (rkyv/abomonation) have no predictor and no entropy coder. `.seed`'s own audit
//! (`theory/SEED_SPEC.md` §5.1) names the missing rung: *the entropy coder*. This module is that
//! rung, zero-dep — a range coder (the Null-arm entropy code) fed by the proven `quadSeq`/`triadGen`
//! floor (`proof/Substrate/Seed.lean`: `quadSeq_regenerates`, a quadratic stream → residual 0).
//!
//! Zero runtime dependencies. The competitors are dev-only — benchmarked against, never built on.

// The SIZE face (the `cm` coder + the floor) is unsafe-free. The SPEED face (`web`) confines the
// one cut to the metal — viewing bytes as a value at a coordinate (Axiom XI, the syscall-class cut)
// — to documented `unsafe` blocks in `web.rs`; everything else denies unchecked unsafe.
#![deny(unsafe_op_in_unsafe_fn)]

/// `#[derive(Collapse)]` — the columnar zero-copy bridge (feature `derive`). Generates
/// `T::collapse_encode(&[T]) -> Seed` + a zero-copy `TView` from a flat struct, so the column-gather
/// is generated, not hand-written. Build-time proc-macro; the runtime stays zero-dep.
#[cfg(feature = "derive")]
pub use wirefold_derive::Collapse;

// ════════════════════════════════════════════════════════════════════════════
// THE NULL-ARM CODER — the context-mixing entropy coder, LIFTED from SangClone (see `cm.rs`).
// `.seed`'s audit (SEED_SPEC §5.1) named the entropy coder the missing rung; refound-s built it
// (`Runtime/SangClone.lean` — ⚠ this cited a `compressOk` declaration that existed around
// `b45460c1` and does not survive at HEAD; the module is now implementation `def`s, so the claim
// below rests on the MEASUREMENT, not on a theorem): α = the bit, order-k KN backoff + SSE/APM
// + a 2-symbol Null-cell rANS,
// measured r5 ≈ 1.93 b/B on Lean source, past gzip -9. We lift that proven codec rather than
// reinvent it — the order-0 range coder this replaces was the reinvention.
// ════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "compress")]
pub mod cm {
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
            let out = (q1 + 7.0 * ((r2 + r3) / 2.0)) / 8.0;
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
        if i % w == 0 {
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
}
#[cfg(feature = "compress")]
pub mod gen {
    //! The generator stage — the `Kfam` basis above {copy} (`GeneratorBasis`, the external proof development).
    //!
    //! An LZ codec is `Kfam` over the copy generators alone; `basis_growth_strictly_wins` proves that
    //! basis floors an unbounded distance above K on affine-shaped streams (the ramp: every copy lag
    //! leaves the WHOLE stream as residual, one affine coordinate leaves none). This module is the
    //! first coordinate past copy: the byte-lane subtraction delta at lag `k` — `r[i] = x[i] − x[i−k]`
    //! (mod 256). Lane-structured data (f32 tensors at k=4, stereo PCM at k=4/8, counters) collapses
    //! toward its residual entropy; the residual stream feeds the ordinary faces, whose copy basis
    //! then works ON TOP of the new coordinate. Choice of `k` is the `Kfam` min, computed on a
    //! sample by MEASURED coded size — never a dial.
    //!
    //! `Kfam_cons_le` proves composing coordinates is free ALGEBRAICALLY (lossless, never worse than
    //! the shorter basis). It is NOT free INFORMATIONALLY: the generator stage and the copy face
    //! compete for the same redundancy. Measured (2026-07-11, `_genprobe`): stacking `delta1` on
    //! `planes4` cuts audio's order-0 entropy but WORSENS coded size — delta whitens the symbols and
    //! shatters the matches the copy face was harvesting (the byte-BPE lesson). The router applies
    //! ONE gated coordinate; a second pays only where the confirm gate (coded, not order-0) still
    //! shrinks — audio +0.8% via a gated 2nd pass, weights/prose/code decline it. Never stack on the
    //! order-0 metric alone: it lies for a codec with a copy face.

    /// Forward: `r[i] = x[i] − x[i−k]` (wrapping); the first `k` bytes are the seed, kept verbatim.
    /// `k = 0` is the identity (the empty transform — `Kfam`'s raw arm).
    #[must_use]
    pub fn delta_encode(data: &[u8], k: usize) -> Vec<u8> {
        if k == 0 || data.len() <= k {
            return data.to_vec();
        }
        let mut out = Vec::with_capacity(data.len());
        out.extend_from_slice(&data[..k]);
        for i in k..data.len() {
            out.push(data[i].wrapping_sub(data[i - k]));
        }
        out
    }

    /// Inverse: the lag-`k` prefix sum per lane — the REGROW (each value is generator-predicted from
    /// `k` back plus its residual; `genDecode` in silicon).
    #[must_use]
    pub fn delta_decode(res: &[u8], k: usize) -> Vec<u8> {
        if k == 0 || res.len() <= k {
            return res.to_vec();
        }
        let mut out = Vec::with_capacity(res.len());
        out.extend_from_slice(&res[..k]);
        for i in k..res.len() {
            let v = out[i - k].wrapping_add(res[i]);
            out.push(v);
        }
        out
    }

    /// The byte-exact Being rotor at lag `k` — the LIFTING realization of `BeingCellTransform`'s
    /// haar (`lift_lossless`, the external proof development): the rotation as two shears, each invertible over
    /// ℤ/256 for any half-map. Within each stride-`k` lane, consecutive pairs `(x, y)` become
    /// `(s, d)` with `d = y − x` (mod 256) and `s = x + (d as i8 >> 1)` — the signed half, so `s`
    /// approximates the pair's average exactly where the data is smooth (the monad point, where
    /// the quarter-turn decorrelates maximally: `haar_decorrelates`, `MonadMixing`). Odd lane
    /// tails pass through.
    ///
    /// NOT a router arm (removed 2026-07-11 after briefly landing as one): the KLT probe measured
    /// θ* = 45°±1° wherever byte-pair correlation exists — stationary streams never leave the
    /// monad, and at the byte grain delta dominates the level-1 lift (the s-channel is a midpoint
    /// at full amplitude). On every bench corpus the router's gate rejected it; an arm that cannot
    /// fire is inventory, not capability. The primitive stays (licensed by
    /// `BeingCellTransform.lift_lossless`, roundtrip-tested) for the data classes whose geometry
    /// is variance-asymmetric — 2D/multichannel — where the rotor family genuinely pays.
    #[must_use]
    pub fn haar_encode(data: &[u8], k: usize) -> Vec<u8> {
        if k == 0 {
            return data.to_vec();
        }
        let mut out = data.to_vec();
        let n = data.len();
        for lane in 0..k {
            let mut i = lane;
            while i + k < n {
                let (x, y) = (data[i], data[i + k]);
                let d = y.wrapping_sub(x);
                out[i] = x.wrapping_add(((d as i8) >> 1) as u8);
                out[i + k] = d;
                i += 2 * k;
            }
        }
        out
    }

    /// Inverse of [`haar_encode`] — un-shear, then un-difference (`unlift`).
    #[must_use]
    pub fn haar_decode(t: &[u8], k: usize) -> Vec<u8> {
        if k == 0 {
            return t.to_vec();
        }
        let mut out = t.to_vec();
        let n = t.len();
        for lane in 0..k {
            let mut i = lane;
            while i + k < n {
                let (s, d) = (t[i], t[i + k]);
                let x = s.wrapping_sub(((d as i8) >> 1) as u8);
                out[i] = x;
                out[i + k] = d.wrapping_add(x);
                i += 2 * k;
            }
        }
        out
    }

    /// Byte-plane transpose at stride `s`: plane `p` = bytes at positions ≡ `p` (mod `s`),
    /// planes concatenated — a pure PERMUTATION (`GeneratorBasis.planes4_lossless`: three
    /// applications of the proven stride-2 bijection). Exposes per-lane structure the row-major
    /// interleave hides: an f32 stream's exponent plane is skewed (H₀ ≈ 2.9 b/B) while its
    /// mantissa planes are uniform — mixed, every coder sees only the 7.4 blend.
    #[must_use]
    pub fn planes_encode(data: &[u8], s: usize) -> Vec<u8> {
        if s <= 1 {
            return data.to_vec();
        }
        let mut out = Vec::with_capacity(data.len());
        for p in 0..s {
            out.extend(data[p.min(data.len())..].iter().step_by(s));
        }
        out
    }

    /// Inverse of [`planes_encode`] — scatter the planes back to row-major.
    #[must_use]
    pub fn planes_decode(t: &[u8], s: usize) -> Vec<u8> {
        if s <= 1 {
            return t.to_vec();
        }
        let n = t.len();
        let mut out = vec![0u8; n];
        let mut idx = 0;
        for p in 0..s {
            let mut i = p;
            while i < n {
                out[i] = t[idx];
                idx += 1;
                i += s;
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn haar_roundtrip() {
            let mut st = 0xFACE_FEEDu64;
            for n in [0usize, 1, 2, 5, 9, 100_003] {
                let data: Vec<u8> = (0..n)
                    .map(|_| {
                        st ^= st << 13;
                        st ^= st >> 7;
                        st ^= st << 17;
                        st as u8
                    })
                    .collect();
                for k in [0usize, 1, 2, 4, 8] {
                    assert_eq!(
                        haar_decode(&haar_encode(&data, k), k),
                        data,
                        "n {n} lag {k}"
                    );
                }
            }
            // the monad witness: a constant lane decorrelates to (x, 0) pairs — the d channel
            // lands on the cone (haar_decorrelates's byte reading)
            let smooth = vec![100u8; 1000];
            let t = haar_encode(&smooth, 1);
            assert!(
                t.iter().skip(1).step_by(2).all(|&d| d == 0),
                "smooth d-channel must be zero"
            );
        }

        #[test]
        fn planes_roundtrip() {
            let mut s = 0xABCD_EF01_2345_6789u64;
            for n in [0usize, 1, 5, 8, 9, 100_003] {
                let data: Vec<u8> = (0..n)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 7;
                        s ^= s << 17;
                        s as u8
                    })
                    .collect();
                for k in [0usize, 1, 2, 3, 4, 8] {
                    assert_eq!(
                        planes_decode(&planes_encode(&data, k), k),
                        data,
                        "n {n} stride {k}"
                    );
                }
            }
        }

        #[test]
        fn delta_roundtrip() {
            let mut s = 0x1234_5678_9abc_def0u64;
            let data: Vec<u8> = (0..100_000)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    s as u8
                })
                .collect();
            for k in [0usize, 1, 2, 3, 4, 8, 16] {
                assert_eq!(delta_decode(&delta_encode(&data, k), k), data, "lag {k}");
            }
            // the ramp witness: an LE u32 counter's lag-4 residual is almost all zeros + carries
            let ramp: Vec<u8> = (0..50_000u32).flat_map(u32::to_le_bytes).collect();
            let r = delta_encode(&ramp, 4);
            #[allow(clippy::naive_bytecount)] // a test assertion; not worth a bytecount dep
            let zeros = r.iter().filter(|&&b| b == 0).count();
            assert!(
                zeros * 10 > r.len() * 7,
                "ramp lag-4 residual should be mostly zeros"
            );
            assert_eq!(delta_decode(&r, 4), ramp);
        }
    }
}
#[cfg(feature = "compress")]
pub mod lz {
    //! The LZ face — the suffix web's trinity read for THROUGHPUT (the LZMA structure, our parts).
    //!
    //! A back-reference is the trinity `{B, B/U, NB}` (see `proof/Substrate/Algebra/NullCell`):
    //!   * **B/U — the offset** (`CoordMap`: where this k-gram occurred before), with the rep-distance
    //!     SUB-TRINITY (`{rep0 monad, rep1..3 cone, new dark}` — recent offsets are cheap, the locality
    //!     sort of `SpectralFold`);
    //!   * **B — the run** ([`crate::codec::web::extend`]: how far the monad `1·s=s` carries the copy),
    //!     coded as ONE token that advances `L` — O(1) amortized per run, the LZ speed class;
    //!   * **NB — the literal** (the byte that broke the copy, the residual) — context-modelled.
    //!
    //! This is LZMA's exact shape. The one deviation is deliberate and is where we WIN: LZMA models its
    //! literal by order-1 + match-byte; we route the NB face through our context-mixer (denser). The token
    //! is coded three ways, each the readout its regime asks for (`EntropyFoldReadout`): the binary FLAGS
    //! (is_match/is_rep) ride the adaptive range coder (small state, tracks drift by recompute); the length/
    //! offset SLOTS (the skewed coordinate) are block-tANS'd (one table lookup/int, `table_is_the_fold`);
    //! the equiprobable low bits are a chunk copy (`BitW`, the uniform corner `uniform_code_is_copy`). The
    //! range coder is locked by `rc_roundtrip` before any model rides it.

    // ── the parse: B/U offset (match-finder + rep0) · B run (extend) · NB literal (order-1) ──

    use crate::codec::web::extend;

    const MIN_MATCH: usize = 4; // shortest NEW-offset match (must pay a full offset code)
    const MIN_REP: usize = 2; // shortest REP match — the offset is a cheap index, so short reps pay off
    const HBITS: u32 = 17; // the short (4-byte) hash cache — last position per k-gram
    const GOLD32: u32 = 0x9E37_79B1;
    const GOLD64: u64 = 0x9E37_79B9_7F4A_7C15;
    // the CM's bits/byte on the literal residual — the price of routing a byte to the NB face. A strong
    // context-mixer hits ~2–3 on structured data; used to weigh a match token against coding its bytes
    // as literals. (Derivable from the cone fraction / a running estimate; a conservative constant here.)
    const LIT_EST: usize = 3;

    #[inline]
    fn hash4(a: &[u8], i: usize) -> usize {
        let v = u32::from_le_bytes(a[i..i + 4].try_into().unwrap());
        (v.wrapping_mul(GOLD32) >> (32 - HBITS)) as usize
    }

    /// A 4-byte little-endian read — the chain guard's cheap compare (zstd's `MEM_read32`).
    #[inline(always)]
    fn read4(a: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(a[i..i + 4].try_into().unwrap())
    }

    /// Move rep index `j` to the front of the recent-offset list (LZMA's rep move-to-front): the offset
    /// just used becomes rep0, the cheapest to code next time.
    #[inline]
    fn mtf(reps: &mut [usize; 4], j: usize) {
        let v = reps[j];
        let mut k = j;
        while k > 0 {
            reps[k] = reps[k - 1];
            k -= 1;
        }
        reps[0] = v;
    }

    use super::{read_len, take_slice, try_zeroed, DecodeError, DEFAULT_MAX_OUTPUT};
    use crate::codec::bits::{BitR, BitW};

    /// The match-finder result: `(len, offset, rep_index)` where `rep_index = -1` for a NEW offset.
    struct Found {
        len: usize,
        off: usize,
        rep: i32,
    }

    // ── THE ROW FINDER — SimtSelect's movemask group-probe + CoordinateReadout's tag-confirm ──
    // Derived, not copied from zstd: the framework already proves this. A hash row holds `GROUP` recent
    // positions with a parallel row of 7-bit TAGS. Finding a match is one `SimtSelect.movemask` — compare
    // the target tag against all `GROUP` lanes at once (`_mm_cmpeq_epi8 + _mm_movemask_epi8`, the crate's
    // own `FreqLine.match_tag`) — then iterate the set lanes (`firstMatch` = tzcnt). The tag is a
    // NON-INJECTIVE coordinate (7 bits), so a lane match is a candidate, not a proof — `extend` confirms
    // it (`CoordinateReadout.partial_tag_false_positive`, the depth-2 readout). The dials are the
    // framework's: `GROUP = 16` = the SSE lane count (`GroupProbe.winnow_exact`, `∀ w`, schedule-free);
    // tag width 7 = maximal (`tag_width_is_maximal`, minimal false-positive) — neither tuned.
    //
    // RECALL IS SATURATED AT G=16 (measured 2026-07-11, code8 density): pushing the `FinderRecall` dial
    // beyond this point moves nothing. A finder-local LANES=32 (two 16-wide glances) at constant window
    // (rows halved) held code8 at 3.83 flat and cost enc; at double window (40 MiB, off-L3) STILL 3.83
    // and dec cratered 1334→628. zstd-6 reaches denser code with a SMALLER window — so the code density
    // gap is NOT finder memory (`FinderRecall` counts capacity; capacity is not the binding constraint
    // here). It lives in the parse/coder: the offset+extra-bit `raw` stream is 44.5% of code8 output;
    // the lever is offset COST (price-lazy `ParsePrice`, reps), not recall. G=16 stays.

    use crate::{group_tag_mask, GROUP}; // the SHARED SwissTable movemask + lane count — one primitive for
                                        // the hashmap (FreqLine) and this match finder (CoordinateReadout)

    /// Cap the row table so its footprint (rows·GROUP·5 B) stays L3-resident — the readout must be a CACHE
    /// HIT, not a DRAM miss per position (`CoordinateReadout.footprint`). The bound: rows·G·5 B ≤ L3
    /// (36 MiB here) ⇒ rows ≤ 2^18 at G=16 (20 MiB). The original cap 2^16 was set against a 40 MiB
    /// counterexample with the middle unprobed; the 2026-07-10 single-variable sweep (16/17/18) measured
    /// 18 dominant on every axis (prose 2.721→2.801 ratio at flat enc, dec +6%) — `FinderRecall`: window
    /// = rows×lanes, and residency, not row count, is the binding constraint. DERIVED, then verified.
    const ROW_BITS_CAP: u32 = 18;

    /// The row index and 7-bit tag of position `i` — disjoint windows of one mixed hash (`GroupProbe.tagWindow`:
    /// the tag is the LOW window, the row the high, so they never alias).
    #[inline]
    fn row_hash(a: &[u8], i: usize, row_bits: u32) -> (usize, u8) {
        let v = u64::from_le_bytes(unsafe { a.get_unchecked(i..i + 8) }.try_into().unwrap());
        let h = v.wrapping_mul(GOLD64);
        let row = (h >> (64 - row_bits)) as usize;
        let tag = (h & 0x7F) as u8; // 7 bits, high bit clear (never the 0x80 empty sentinel)
        (row, tag)
    }

    /// The row finder — `GROUP` recent positions per hash row + a parallel tag row, probed by movemask.
    struct RowFinder {
        pos: Vec<u32>, // rows × GROUP: position+1 (0 = empty)
        tags: Vec<u8>, // rows × GROUP: 7-bit tag (0x80 = empty)
        head: Vec<u8>, // per-row circular write cursor
        row_bits: u32,
        // the scan's RESERVATION VALUE (ParsePrice.geometric_scan_value/scan_value_antitone):
        // stop the lane walk once the best found crosses l* = log_{1/q}(mu/(c(1-q))) — a
        // threshold, logarithmic in the cost ratio; the inherited 512 was a stab. ME env =
        // calibration harness (hoisted here, read once per finder).
        match_enough: usize,
    }
    impl RowFinder {
        fn new(nbytes: usize) -> Self {
            // rows ≈ nbytes/GROUP so the table holds ~one slot per byte; clamp to a sane range.
            let row_bits = ((nbytes / GROUP).max(1) as u64)
                .ilog2()
                .clamp(12, ROW_BITS_CAP);
            let rows = 1usize << row_bits;
            RowFinder {
                pos: vec![0u32; rows * GROUP],
                tags: vec![0x80u8; rows * GROUP],
                head: vec![0u8; rows],
                row_bits,
                match_enough: std::env::var("ME")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(MATCH_ENOUGH),
            }
        }
        #[inline]
        fn insert(&mut self, data: &[u8], p: usize, n: usize) {
            if p + 8 > n {
                return;
            }
            let (row, tag) = row_hash(data, p, self.row_bits);
            let base = row * GROUP;
            unsafe {
                let slot = *self.head.get_unchecked(row) as usize;
                *self.pos.get_unchecked_mut(base + slot) = (p + 1) as u32;
                *self.tags.get_unchecked_mut(base + slot) = tag;
                *self.head.get_unchecked_mut(row) = ((slot + 1) & (GROUP - 1)) as u8;
            }
        }
        /// Pull the NEXT probe position's tag+pos rows toward L1 while other work runs — zstd's
        /// lazy_row issues the same pair of `prefetcht0` at hash time (objdump 0xe9b2e). At
        /// `ROW_BITS_CAP = 18` the table is 20 MiB L3-resident, so a cold probe is an L3 hit
        /// (~40 cy) the intervening insert/emit work can hide. (The old "prefetch is neutral"
        /// verdict was measured on the 5 MB L2-resident table — re-tested at 18.)
        #[inline]
        fn prefetch(&self, data: &[u8], i: usize, n: usize) {
            if i + 8 > n {
                return;
            }
            let (row, _) = row_hash(data, i, self.row_bits);
            let base = row * GROUP;
            #[cfg(target_arch = "x86_64")]
            unsafe {
                use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
                _mm_prefetch(self.tags.as_ptr().add(base).cast(), _MM_HINT_T0);
                _mm_prefetch(self.pos.as_ptr().add(base).cast(), _MM_HINT_T0);
            }
        }
        #[inline]
        fn index_span(&mut self, data: &[u8], from: usize, to: usize, n: usize) {
            for k in from..to {
                self.insert(data, k, n);
            }
        }
        /// Find the best token at `i`: the 4 reps (direct) and the row's `GROUP` candidates (movemask +
        /// tzcnt-iterate + `extend`-confirm). The confirm guard skips candidates that can't beat the best.
        #[inline]
        fn find(&self, data: &[u8], i: usize, n: usize, reps: &[usize; 4]) -> Found {
            let worth = |pos: usize, best: usize| {
                best < 4
                    || i + best >= n
                    || read4(data, i + best - 3) == read4(data, pos + best - 3)
            };
            let (mut brl, mut bro, mut bri) = (0usize, 0usize, -1i32);
            // the candidate GATE before the extend (zstd's MEM_read32 gate, selection-identical
            // grains: 2 bytes for reps — MIN_REP = 2, so shorter is never taken — and 4 bytes for
            // lanes — MIN_MATCH = 4). The ungated loop ran four extends per position and the
            // extend was the finder's hottest region (2026-07-11 annotate, 23% on the 8-byte step).
            for (j, &r) in reps.iter().enumerate() {
                if r > 0
                    && i >= r
                    && i + 2 <= n
                    && data[i] == data[i - r]
                    && data[i + 1] == data[i - r + 1]
                {
                    let l = extend(data, i, i - r);
                    if l > brl {
                        brl = l;
                        bro = r;
                        bri = j as i32;
                    }
                }
            }
            let (mut bnl, mut bno) = (0usize, 0usize);
            if i + 8 <= n {
                let anchor4 = read4(data, i);
                let (row, tag) = row_hash(data, i, self.row_bits);
                let base = row * GROUP;
                let mut m = group_tag_mask(&self.tags, base, tag);
                while m != 0 {
                    let lane = m.trailing_zeros() as usize;
                    m &= m - 1; // clear lowest set lane — the `firstMatch`/tzcnt iterate
                    let p = unsafe { *self.pos.get_unchecked(base + lane) } as usize;
                    if p != 0 && p - 1 < i && read4(data, p - 1) == anchor4 && worth(p - 1, bnl) {
                        let l = extend(data, i, p - 1);
                        if l > bnl {
                            bnl = l;
                            bno = i - (p - 1);
                            if l >= self.match_enough {
                                break;
                            }
                        }
                    }
                }
            }
            let rep_ok = bri >= 0 && brl >= MIN_REP;
            let new_ok = bnl >= MIN_MATCH;
            if rep_ok && (!new_ok || brl + 2 >= bnl) {
                Found {
                    len: brl,
                    off: bro,
                    rep: bri,
                }
            } else if new_ok {
                Found {
                    len: bnl,
                    off: bno,
                    rep: -1,
                }
            } else {
                Found {
                    len: 0,
                    off: 0,
                    rep: -1,
                }
            }
        }
    }

    const MATCH_ENOUGH: usize = 512; // stop the chain walk once a match this long is found

    #[inline]
    fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            if v < 128 {
                out.push(v as u8);
                break;
            }
            out.push((v % 128 + 128) as u8);
            v /= 128;
        }
    }

    // ── THE CELL TABLE — FinderRecall at lanes = 1, the ONE single-cell finder primitive ──
    // `FinderRecall`: a finder's memory is rows × lanes. The RowFinder is the lanes = 16 point;
    // every other face (fast long/short, turbo) is lanes = 1 — this struct, u32 cells with pos+1
    // in the low 24 bits and an 8-bit tag above (footprint beats touch-count, measured 2026-07-11:
    // u64 cells +4-5%, split pos/tag arrays in between). Positions wrap mod 2^24 SAFELY: a copy is
    // validated by the data (gate + lce), never by the finder's belief — a wrapped candidate either
    // truly matches or fails; the window degrades, exactness never.
    struct CellTable {
        cells: Vec<u32>,
        bits: u32,
    }
    impl CellTable {
        fn new(bits: u32) -> Self {
            CellTable {
                cells: vec![0u32; 1 << bits],
                bits,
            }
        }
        /// Probe with a mixed hash: returns `(pos+1, stored tag, slot)` — the caller gates on the
        /// tag (rejects only, `CoordinateReadout`) and confirms with the data.
        #[inline(always)]
        fn probe(&self, h: u64) -> (usize, u8, usize) {
            let b = (h >> (64 - self.bits)) as usize;
            let c = unsafe { *self.cells.get_unchecked(b) };
            ((c & 0xFF_FFFF) as usize, (c >> 24) as u8, b)
        }
        #[inline(always)]
        fn insert_at(&mut self, slot: usize, i: usize, tag: u8) {
            unsafe {
                *self.cells.get_unchecked_mut(slot) =
                    ((i + 1) as u32 & 0xFF_FFFF) | (u32::from(tag) << 24);
            }
        }
        #[inline(always)]
        fn insert(&mut self, h: u64, i: usize) {
            let b = (h >> (64 - self.bits)) as usize;
            self.insert_at(b, i, (h >> 8) as u8);
        }
    }

    // ── THE TURBO FACE — the single-cell finder, for RAW THROUGHPUT ──
    // The dense faces above pay for density (16-lane search, reps, entropy coding). The turbo face gives
    // ALL of that up for speed: ONE cell probed per position, no entropy — the throughput extreme, the
    // mirror of the CM face's density extreme.
    //   * the finder is one hash → one cell (`FinderRecall`: G = 1, so the retention window is the row
    //     count 2^TURBO_BITS — the capacity, counted, not tuned). The cell carries `pos` plus a stored
    //     fingerprint that gates the REJECT path only. NOTE the applicability boundary written in
    //     `CoordinateReadout` (2026-07-10): the match VALUE (its length) lives in the source bytes, so
    //     `lce` on every ACCEPT is mandatory under ANY confirm mechanism — a fingerprint (tag or stored
    //     k-gram) can only save the source touch on rejects, and which fingerprint wins is an empirical
    //     cell-layout question (footprint vs false-accept rate), measured in the 2×2 below, not derived.
    //   * the scan is a tight inner loop that advances until a match, so the (unpredictable) match/no-match
    //     branch mispredicts once PER MATCH, not per position (the mispredict concentrated, not scattered).
    //   * a miss-driven STRIDE skips low-participation regions — the finder striding over empty
    //     coordinates; incompressible data (f32 weights) flies.
    //   * the coder is a byte emit (lz4 token `[litlen:4|matchlen:4]` + varint spills + 3-byte offset),
    //     NO entropy — the uniform-copy extreme (`EntropyFoldReadout.uniform_code_is_copy`) taken whole.
    //     The 3-byte offset CAPS the window at 2^24: the finder REJECTS farther candidates (`TURBO_MAX_OFF`)
    //     — without the cap, offsets ≥ 16 MiB silently truncated into corruption (fixed 2026-07-10,
    //     locked by `turbo_far_window` on a 24 MiB input).

    const TURBO_BITS: u32 = 18; // hash-table log — the G=1 retention window (FinderRecall), L2-resident
    const TURBO_MINM: usize = 4; // shortest match: a 3-byte offset must pay for itself
    const TURBO_MAX_OFF: usize = 1 << 24; // the 3-byte-offset window — farther candidates are rejected

    #[inline(always)]
    fn read8(a: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(unsafe { a.get_unchecked(i..i + 8) }.try_into().unwrap())
    }

    /// The longest common extension at `(i, j)` — 8 bytes/step, the tail from the first differing byte
    /// (`xor` → `trailing_zeros >> 3`). The B-face run length (`extend`), unrolled for the turbo path.
    #[inline(always)]
    fn lce(a: &[u8], mut i: usize, mut j: usize, n: usize) -> usize {
        let s = i;
        while i + 8 <= n {
            let x = read8(a, i) ^ read8(a, j);
            if x != 0 {
                return i - s + (x.trailing_zeros() as usize >> 3);
            }
            i += 8;
            j += 8;
        }
        i - s
    }

    #[inline]
    fn put_len(o: &mut Vec<u8>, mut v: usize) {
        while v >= 255 {
            o.push(255);
            v -= 255;
        }
        o.push(v as u8);
    }
    #[inline]
    fn get_len(b: &[u8], p: &mut usize) -> Result<usize, DecodeError> {
        let mut v = 0usize;
        loop {
            let x = *b.get(*p).ok_or(DecodeError::Truncated)?;
            *p += 1;
            v = v.checked_add(usize::from(x)).ok_or(DecodeError::TooLarge)?;
            if x != 255 {
                return Ok(v);
            }
        }
    }

    /// The turbo finder: one 8-byte-gram hash → one `(pos, tag)` cell. The 2026-07-10 2×2 measurement
    /// (confirm mechanism × hash width, single-variable each) fixed both choices:
    ///   * **hash width owns the ratio** — H8 1.728/2.307 vs H4 1.347/1.828 (prose/code), IDENTICAL
    ///     across confirm mechanisms: the 8-byte anchor keys the G=1 retention window (`FinderRecall`:
    ///     same capacity, different retained POPULATION) on rare long-context grams instead of burning
    ///     it on " the"-class 4-grams.
    ///   * **the fingerprint owns the speed** — the 1-byte tag beat the stored 4-byte coordinate by
    ///     13–17% enc at equal ratio (5 B vs 8 B per cell, `footprint_mono`); the lce on ACCEPT is
    ///     mandatory either way (the `CoordinateReadout` applicability boundary: the match length lives
    ///     in the source bytes), and its first 8-byte XOR covers bytes 0–7 regardless, so the
    ///     coordinate's "pre-confirmed 4 bytes" bought nothing.
    fn compress_turbo_impl(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut ct = CellTable::new(TURBO_BITS);
        let mut out = Vec::with_capacity(n / 2 + 16);
        put_len(&mut out, n);
        let (mut i, mut lst, mut miss) = (0usize, 0usize, 0usize);
        if n >= 8 {
            'outer: loop {
                let (mpos, mlen, moff);
                loop {
                    if i + 8 > n {
                        break 'outer;
                    }
                    let h = read8(data, i).wrapping_mul(GOLD64);
                    let t = (h >> 8) as u8;
                    let (p, st, slot) = ct.probe(h);
                    ct.insert_at(slot, i, t);
                    if p != 0 && st == t && p - 1 < i && i - (p - 1) < TURBO_MAX_OFF {
                        // the tag gates the REJECT path only; lce confirms from 0 (a 2⁻⁸ false-accept
                        // pays one wasted extension) and measures the run — both jobs, one scan.
                        let l = lce(data, i, p - 1, n);
                        if l >= TURBO_MINM {
                            mpos = i;
                            mlen = l;
                            moff = i - (p - 1);
                            break;
                        }
                    }
                    miss += 1;
                    i += 1 + (miss >> 6); // stride: skip low-participation runs
                }
                let litlen = mpos - lst;
                let ml = mlen - TURBO_MINM;
                out.push(((litlen.min(15) << 4) | ml.min(15)) as u8);
                if litlen >= 15 {
                    put_len(&mut out, litlen - 15);
                }
                out.extend_from_slice(&data[lst..mpos]);
                out.extend_from_slice(&moff.to_le_bytes()[..3]);
                if ml >= 15 {
                    put_len(&mut out, ml - 15);
                }
                i = mpos + mlen;
                lst = i;
                miss = 0;
            }
        }
        // tail: the final literal run (matchlen nibble 0, no offset follows — decode stops at n)
        let litlen = n - lst;
        out.push((litlen.min(15) << 4) as u8);
        if litlen >= 15 {
            put_len(&mut out, litlen - 15);
        }
        out.extend_from_slice(&data[lst..n]);
        out
    }

    /// Turbo compress — the single-cell throughput face (see the section header). Frame is a self-framed
    /// byte stream: `[varint n]` then lz4-style sequences `[tok][litlen spill][literals][off:3][mlen spill]`.
    #[must_use]
    pub fn compress_turbo(data: &[u8]) -> Vec<u8> {
        compress_turbo_impl(data)
    }

    /// Copy `len` bytes inside `buf` from `src` to `dst` (`src < dst`), chunked with overshoot ≤ 31
    /// into the buffer's slack — the WILDCOPY (`uniform_code_is_copy` at the memory system: a copy
    /// is ONE uniform readout). The bounds proof is HOISTED to the single entry assert — the old
    /// per-16B slice-indexed loop re-proved it `len/16` times (4 cmp/ja per chunk) and was the
    /// hottest region of face decode (2026-07-11 annotate: ~30%, the Peano violation in the copy
    /// loop). Close overlap (`gap < 8`) is widened by MEMOIZED sub-offsets (`table_is_the_fold` at
    /// the copy: the widening pattern is a pure function of the offset, written down once — the
    /// dec32/dec64 tables, zstd_decompress_block.c:808, replacing our recomputed doubling loop);
    /// gap ≥ 8 self-replicates at stride 8, gap ≥ 16 at stride 16 (32 B strides measured WORSE,
    /// +2.4%: prose matches average ~17 B).
    ///
    /// REJECTED beside this (2026-07-11, do not retry at ≤ 8 MB history): the ring-of-8 match
    /// PREFETCH (zstd's Long decoder, STORED_SEQS=8 — decode ahead, track the future `op` as the
    /// running ll+ml sum, prefetch `out[pos−off]`): +7% prose / +10% code. zstd's own engage-gate
    /// (history > 16 MiB AND long-offset share ≥ 2.73%, zstd_decompress_block.c:2140) is the
    /// applicability boundary, confirmed from our side: at ≤ 8 MB the sources are L2/L3-resident
    /// and the decode-ahead shuffle costs more than the latency it hides. Retest ONLY behind that
    /// gate if we ever bench > 16 MiB inputs (`MemoryLevelParallelism.optimalDepth` sizes the ring
    /// at 8 for DRAM-class misses; L2/L3-class misses need depth ≤ 2, which the 4 in-flight
    /// sequences of the plain loop already provide).
    /// [`wildcopy_within`] for a copy whose operands come from untrusted input: the same guard with
    /// checked arithmetic, as an error instead of a panic.
    #[inline]
    fn wildcopy_checked(
        buf: &mut [u8],
        src0: usize,
        dst0: usize,
        len: usize,
    ) -> Result<(), DecodeError> {
        let end = dst0
            .checked_add(len)
            .and_then(|e| e.checked_add(31))
            .ok_or(DecodeError::Malformed)?;
        if src0 >= dst0 || end > buf.len() {
            return Err(DecodeError::Malformed);
        }
        wildcopy_within(buf, src0, dst0, len);
        Ok(())
    }

    #[inline]
    fn wildcopy_within(buf: &mut [u8], src0: usize, dst0: usize, len: usize) {
        // the ONE guard — proves every unchecked chunk below (and catches corrupt streams: a
        // wrapped `op - off` fails `src0 < dst0`)
        assert!(
            src0 < dst0 && dst0 + len + 31 <= buf.len(),
            "wildcopy bounds"
        ); // ≤31 B overshoot (the 2×16 tail)
        if dst0 - src0 >= 16 {
            unsafe { wildcopy_wide(buf.as_mut_ptr(), src0, dst0, len) }
        } else {
            unsafe { wildcopy_close(buf.as_mut_ptr(), src0, dst0, len) }
        }
    }

    /// The gap ≥ 16 arm: 16 B first chunk (almost always the whole copy), 32 B tail loop.
    /// SAFETY: caller proves `src0 < dst0`, `dst0 − src0 ≥ 16`, `dst0 + len + 31 ≤ buf len`.
    #[inline]
    unsafe fn wildcopy_wide(base: *mut u8, src0: usize, dst0: usize, len: usize) {
        unsafe {
            let mut src = base.add(src0);
            let mut dst = base.add(dst0);
            let end = dst.add(len);
            std::ptr::copy_nonoverlapping(src, dst, 16);
            dst = dst.add(16);
            src = src.add(16);
            while dst < end {
                std::ptr::copy_nonoverlapping(src, dst, 16);
                std::ptr::copy_nonoverlapping(src.add(16), dst.add(16), 16);
                src = src.add(32);
                dst = dst.add(32);
            }
        }
    }

    /// The gap < 16 arm: widen the offset (memoized sub-offset tables at gap < 8), then 8 B steps.
    /// SAFETY: caller proves `src0 < dst0`, `dst0 − src0 < 16`, `dst0 + len + 31 ≤ buf len`.
    #[inline]
    unsafe fn wildcopy_close(base: *mut u8, src0: usize, dst0: usize, len: usize) {
        unsafe {
            let mut src = base.add(src0);
            let mut dst = base.add(dst0);
            let end = dst.add(len);
            let off = dst0 - src0;
            if off < 8 {
                // spread the offset to ≥ 8: four byte-serial stores (each may read a byte the
                // previous one wrote — the RLE monad `1·s = s` unrolled), then one 4-byte copy at
                // the table-shifted source. New gap = off − DEC32[off] + DEC64[off] ∈ [8, 14].
                const DEC32: [usize; 8] = [0, 1, 2, 1, 4, 4, 4, 4];
                const DEC64: [usize; 8] = [8, 8, 8, 7, 8, 9, 10, 11];
                *dst = *src;
                *dst.add(1) = *src.add(1);
                *dst.add(2) = *src.add(2);
                *dst.add(3) = *src.add(3);
                src = src.add(DEC32[off]);
                std::ptr::copy_nonoverlapping(src, dst.add(4), 4);
                src = src.sub(DEC64[off]).add(8);
                dst = dst.add(8);
                while dst < end {
                    std::ptr::copy_nonoverlapping(src, dst, 8);
                    src = src.add(8);
                    dst = dst.add(8);
                }
            } else {
                // gap 8..15: 8-byte steps never read an unwritten byte
                while dst < end {
                    std::ptr::copy_nonoverlapping(src, dst, 8);
                    src = src.add(8);
                    dst = dst.add(8);
                }
            }
        }
    }

    /// Inverse of [`compress_turbo`]. Decode into a pre-sized buffer with slack via a cursor — the
    /// common token (short literal, short match, room everywhere) takes a FAST PATH: one dominating
    /// guard, then fixed-16/32-byte copies that compile to bare moves (the guard subsumes the bounds
    /// checks). Long/tail/overlap tokens take the careful path. No byte loops anywhere.
    ///
    /// Malformed input yields an empty `Vec` (never a panic); [`try_decompress_turbo`] says why.
    #[must_use]
    pub fn decompress_turbo(bytes: &[u8]) -> Vec<u8> {
        try_decompress_turbo(bytes).unwrap_or_default()
    }

    /// Checked [`decompress_turbo`]. Output per input byte is bounded (a match length grows by 255
    /// per extension byte), so the declared length is checked against the input size before the
    /// output buffer is allocated.
    pub fn try_decompress_turbo(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut p = 0usize;
        let n = get_len(bytes, &mut p)?;
        let blen = bytes.len();
        if n > blen.saturating_mul(260).saturating_add(64) {
            return Err(DecodeError::TooLarge);
        }
        let mut out = try_zeroed(n + 64)?; // slack absorbs every fast-path overshoot (≤ 47 B past a match end)
        let mut op = 0usize;
        while op < n {
            let tok = *bytes.get(p).ok_or(DecodeError::Truncated)?;
            p += 1;
            let litlen0 = (tok >> 4) as usize;
            let ml0 = (tok & 15) as usize;
            // FAST PATH: in-nibble lengths, input room for a 16 B literal overshoot + 3 offset bytes,
            // output room for the overshoots, and a non-overlapping match — two fixed copies.
            if litlen0 < 15 && ml0 < 15 && p + 19 <= blen && op + 49 <= n + 64 {
                let lit: [u8; 16] = bytes[p..p + 16].try_into().unwrap();
                out[op..op + 16].copy_from_slice(&lit);
                op += litlen0;
                p += litlen0;
                if op >= n {
                    break; // the tail token: ml nibble is padding, no offset follows
                }
                let off = (bytes[p] as usize)
                    | ((bytes[p + 1] as usize) << 8)
                    | ((bytes[p + 2] as usize) << 16);
                p += 3;
                if off == 0 || off > op {
                    return Err(DecodeError::Malformed); // the match must start inside the output
                }
                let ml = ml0 + TURBO_MINM; // ≤ 18
                if off >= 18 {
                    // fixed 32 B copy covers ml ≤ 18 with overshoot; src is ≥ 18 behind, no overlap
                    let src = op - off;
                    let a: [u8; 16] = out[src..src + 16].try_into().unwrap();
                    let b: [u8; 16] = out[src + 16..src + 32].try_into().unwrap();
                    out[op..op + 16].copy_from_slice(&a);
                    out[op + 16..op + 32].copy_from_slice(&b);
                } else {
                    wildcopy_within(&mut out, op - off, op, ml);
                }
                op += ml;
                continue;
            }
            // CAREFUL PATH: length spills, stream tail, tight output
            let mut litlen = litlen0;
            if litlen == 15 {
                litlen = litlen
                    .checked_add(get_len(bytes, &mut p)?)
                    .ok_or(DecodeError::TooLarge)?;
            }
            let lit = take_slice(bytes, &mut p, litlen)?;
            if litlen > n - op {
                return Err(DecodeError::Malformed); // more literals than the declared output
            }
            out[op..op + litlen].copy_from_slice(lit);
            op += litlen;
            if op >= n {
                break;
            }
            let o = take_slice(bytes, &mut p, 3)?;
            let off = (o[0] as usize) | ((o[1] as usize) << 8) | ((o[2] as usize) << 16);
            let mut ml = ml0;
            if ml == 15 {
                ml = ml
                    .checked_add(get_len(bytes, &mut p)?)
                    .ok_or(DecodeError::TooLarge)?;
            }
            ml += TURBO_MINM;
            if off == 0 || off > op || ml > n - op {
                return Err(DecodeError::Malformed);
            }
            wildcopy_checked(&mut out, op - off, op, ml)?;
            op += ml;
        }
        if op != n {
            return Err(DecodeError::Malformed);
        }
        out.truncate(n);
        Ok(out)
    }

    // ── THE SEQ FACE — the flagless sequence stream: zstd's shape, DERIVED (SequenceFraming) ──
    // A sequence is ⟨litlen, matchlen, offCode⟩ — one literal run closed by one match, the trailing run
    // last. NO flag bits anywhere: `SequenceFraming.sequence_framing_drops_the_flags` proves the framing
    // is a bijection with the flagged token stream (the flags are the litlens in unary — Peano marks the
    // framing carries as ONE field element each), so the old face's per-token adaptive flag chain coded
    // zero information and put a serial range coder on the decode hot path for nothing. Here every stream
    // is block-static (`table_is_the_fold`) and decode is pure readouts + copies, with NO adaptive state:
    //   * literals — one tans4 block (`kway_stream_decode_exact`: 4 states, one loop);
    //   * the three slot streams (ll/ml/off classes) — tans4 per 64K block (drift at block grain);
    //   * the class residuals — equiprobable low bits, ONE BitW chunk each (`uniform_code_is_copy`);
    //   * the copies — the turbo wildcopy.
    // The finder is the RowFinder (G=16: retention window = rows×lanes, `FinderRecall`) with the LZMA
    // rep candidates; the emitted offset maps through zstd-style REPCODES (offVal 0..2 = the three most
    // recent offsets — tiny exact slots — else off+2), no adaptive state, one predictable decode branch.

    const SEQ_MIN_MATCH: usize = 2; // ml is coded as ml−2: reps pay off from 2; new offsets need ≥ 4 (encoder policy)

    // ── THE GENERATOR ROUTER — the Kfam min over the basis above {copy}, measured, never dialed ──
    // `GeneratorBasis` (the external proof development): an LZ codec is Kfam at basis {copy}; growing the basis is
    // free (`Kfam_cons_le`) and strictly wins on generator-shaped streams (`basis_growth_strictly_
    // wins`). The Being cell ROTATES (a lossless permutation/bijection exposing lane structure), the
    // Null cell then collapses the residual (`null_cell_is_the_entropy_coder`: "the two cells are the
    // two halves of a codec"). The router IS the Kfam min: measure the coded size of a sample under
    // each coordinate, take the smallest. Measured winners (2026-07-10 genbasis): p4 on f32 lanes
    // (weights 1.080→1.175, audio 1.340→1.707), d4 on counters, identity on prose/code.

    /// The generator alphabet — the tag is carried in the seq frame. 0 = identity (Kfam's raw arm).
    /// Every non-id generator is length-preserving and bijective (`planes4_lossless`, delta = the
    /// lag-k prefix-sum inverse), so `rawLen` and the parse are untouched; decode inverts at the end.
    fn gen_apply(g: u8, d: &[u8]) -> Vec<u8> {
        match g {
            1 => crate::codec::gen::delta_encode(d, 1),
            2 => crate::codec::gen::delta_encode(d, 4),
            3 => crate::codec::gen::planes_encode(d, 4),
            4 => crate::codec::gen::planes_encode(d, 8),
            t => panic!("gen_apply on tag {t}"),
        }
    }
    fn gen_invert(g: u8, d: Vec<u8>) -> Vec<u8> {
        match g {
            0 => d,
            1 => crate::codec::gen::delta_decode(&d, 1),
            2 => crate::codec::gen::delta_decode(&d, 4),
            3 => crate::codec::gen::planes_decode(&d, 4),
            4 => crate::codec::gen::planes_decode(&d, 8),
            t => panic!("gen_invert on tag {t}"),
        }
    }

    /// The Kfam min on a ≤256 KiB sample, in TWO stages (the `CoordinateReadout` shape — the cheap
    /// probe gates REJECTS only; an accept pays the full measurement):
    ///
    /// 1. **The entropy gate**: blocked IDEAL size per arm — the Shannon sum over 64 KiB blocks,
    ///    `NullCellCoding.info_encode`'s exact length (the ALGEBRA prices the gate; only the accept
    ///    pays the realization). The block grain matters and is derived, not dialed: a transform's
    ///    gain lives in table LOCALITY (a p4 plane of the 256 KiB sample is 64 KiB — one block per
    ///    plane; a single-table probe re-mixes the planes and reads ~zero separation, measured
    ///    2026-07-11: p4 61191 vs id 61175 at one table, 240534 vs 243201 blocked). Prose/code
    ///    reject every transform here (subtraction raises text entropy; a permutation cannot lower
    ///    the global histogram) at five histogram passes' cost.
    /// 2. **The confirm**: the winning arm, if any, is re-measured against identity with the REAL
    ///    fast face on the sample (matches included — the gate cannot see that a transform destroys
    ///    n-grams). Only a strict win routes; ties keep identity, Kfam's free raw arm.
    ///
    /// Both stages are MEASUREMENTS of coded size — the theorem's own metric, never a shape rule.
    fn route_gen(data: &[u8]) -> u8 {
        if data.len() < 64 {
            return 0; // below the coded floor every arm is a copy — nothing to measure
        }
        let sample = &data[..data.len().min(LIT_BLOCK)];
        let ent = |t: &[u8]| {
            let mut bits = 0.0f64;
            for ch in t.chunks(LIT_BLOCK / 4) {
                let mut h = [0u32; 256];
                for &b in ch {
                    h[usize::from(b)] += 1;
                }
                let n = ch.len() as f64;
                for &c in &h {
                    if c > 0 {
                        bits -= f64::from(c) * (f64::from(c) / n).log2();
                    }
                }
            }
            bits
        };
        let mut cand = 0u8;
        let mut ce = ent(sample);
        for g in 1..=4u8 {
            let e = ent(&gen_apply(g, sample));
            if e < ce {
                ce = e;
                cand = g;
            }
        }
        if cand == 0 {
            return 0;
        }
        let zi = compress_seq_fast_inner(sample, 0).len();
        let zc = compress_seq_fast_inner(&gen_apply(cand, sample), 0).len();
        if zc < zi {
            cand
        } else {
            0
        }
    }

    /// Code a value into the seq slot alphabet: 0..15 EXACT (zero residual bits — reps, tiny lens, the
    /// mass of the distribution), else a bitlen class (slot 16+, top bit implicit, nbits−1 raw residual
    /// bits). One coder for all three fields — ll, ml−2, offVal.
    #[inline]
    fn enc_len16(v: u32, slots: &mut Vec<u8>, raw: &mut BitW) {
        if v < 16 {
            slots.push(v as u8);
        } else {
            let nbits = 32 - v.leading_zeros(); // ≥ 5
            slots.push((11 + nbits) as u8); // 16..=43
            raw.write(v & ((1 << (nbits - 1)) - 1), nbits - 1);
        }
    }
    /// REJECTED 2026-07-11: the BRANCHLESS `(base, nbits)` table form — `v = BASE[s] + raw.read(NB[s])`
    /// with `read(0) = 0`, zstd's ZSTD_seqSymbol shape taken further than zstd takes it. Branch misses
    /// fell 13.6M → 12.0M but cycles rose +13% Dense / +20% fast: the unconditional read puts the
    /// SERIAL `bitpos` chain (load → shift → `bitpos += n`) on all three fields of every sequence,
    /// where the `s < 16` branch skips it for the exact-slot majority. zstd guards `if (mlBits > 0)`
    /// around its own reads for the same reason (zstd_decompress_block.c ZSTD_decodeSequence) — the
    /// loop-carried dependency costs more than the mispredicts it removes. The dependency boundary,
    /// found a third time (huf interleave, o1 construction, now the bit cursor).
    #[inline]
    fn dec_len16(slots: &[u8], sp: &mut usize, raw: &mut BitR) -> u32 {
        // the caller checked `sp < slots.len()` (one slot per sequence) and every slot ≤ MAX_SLOT
        let s = slots[*sp];
        *sp += 1;
        if s < 16 {
            u32::from(s)
        } else {
            let nbits = u32::from(s) - 11;
            (1 << (nbits - 1)) | raw.read(nbits - 1)
        }
    }

    /// The slot-stream block: bounded BELOW by header amortization (the norm table must be ≪1% of a
    /// block's code), well below stream length so drift is tracked at block grain.
    const SLOT_BLOCK: usize = 1 << 16;
    /// The literal-stream block (bytes): header amortization bounds it below; per-region tables
    /// (drift, per-plane skew) bound it above.
    const LIT_BLOCK: usize = 1 << 18;

    /// tans4 per 64K-symbol block (drift tracking on the 4-stream kernel, small-alphabet headers).
    fn tans4_blocked_at(slots: &[u8], block: usize) -> Vec<u8> {
        let mut out = Vec::new();
        put_uvarint(&mut out, slots.len() as u64);
        for chunk in slots.chunks(block) {
            let c = crate::codec::tans::compress4(chunk);
            put_uvarint(&mut out, c.len() as u64);
            out.extend_from_slice(&c);
        }
        out
    }
    fn tans4_blocked(slots: &[u8]) -> Vec<u8> {
        tans4_blocked_at(slots, SLOT_BLOCK)
    }
    fn untans4_blocked(bytes: &[u8], pos: &mut usize) -> Result<Vec<u8>, DecodeError> {
        let total = read_len(bytes, pos)?;
        if total > DEFAULT_MAX_OUTPUT {
            return Err(DecodeError::TooLarge);
        }
        let mut out = Vec::new();
        while out.len() < total {
            let clen = read_len(bytes, pos)?;
            let block = take_slice(bytes, pos, clen)?;
            let before = out.len();
            crate::codec::tans::try_decompress4_into(block, &mut out)?;
            // a block that yields nothing would loop forever; one that overshoots is not a block we wrote
            if out.len() == before || out.len() > total {
                return Err(DecodeError::Malformed);
            }
        }
        Ok(out)
    }

    /// huf4 per literal block, 64 KiB grain — table LOCALITY one notch FINER than the tans
    /// literals: the huf header is 128 B (vs the tans norm table's ~10× more), so the fine grain
    /// costs 0.2% and keeps the router's plane transform separated even at the 256 KiB confirm
    /// sample, where each plane is 64 KiB (at LIT_BLOCK grain the sample's four planes shared one
    /// table, the confirm wrongly rejected p4, and the weights routing regressed — measured).
    fn huf_blocked(lits: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_uvarint(&mut out, lits.len() as u64);
        for chunk in lits.chunks(LIT_BLOCK / 4) {
            let c = crate::codec::huf::compress4h(chunk);
            put_uvarint(&mut out, c.len() as u64);
            out.extend_from_slice(&c);
        }
        out
    }
    fn unhuf_blocked(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut pos = 0usize;
        let total = read_len(bytes, &mut pos)?;
        if total > DEFAULT_MAX_OUTPUT {
            return Err(DecodeError::TooLarge);
        }
        let mut out = Vec::new();
        while out.len() < total {
            let clen = read_len(bytes, &mut pos)?;
            let block = take_slice(bytes, &mut pos, clen)?;
            let part = crate::codec::huf::try_decompress4h(block)?;
            if part.is_empty() || part.len() > total - out.len() {
                return Err(DecodeError::Malformed);
            }
            out.try_reserve(part.len())
                .map_err(|_| DecodeError::TooLarge)?;
            out.extend_from_slice(&part);
        }
        Ok(out)
    }

    /// Map an absolute offset through the 3-entry repcode list (encode side): `offVal` 0..2 name the
    /// recents (exact slots), else `off + 2`. The update mirrors the decoder exactly.
    #[inline]
    fn rep_encode(reps: &mut [usize; 3], off: usize) -> u32 {
        if off == reps[0] {
            0
        } else if off == reps[1] {
            reps.swap(0, 1);
            1
        } else if off == reps[2] {
            *reps = [reps[2], reps[0], reps[1]];
            2
        } else {
            *reps = [off, reps[0], reps[1]];
            (off + 2) as u32
        }
    }
    #[inline]
    fn rep_decode(reps: &mut [usize; 3], off_val: u32) -> usize {
        match off_val {
            0 => reps[0],
            1 => {
                reps.swap(0, 1);
                reps[0]
            }
            2 => {
                *reps = [reps[2], reps[0], reps[1]];
                reps[0]
            }
            v => {
                let off = (v - 2) as usize;
                *reps = [off, reps[0], reps[1]];
                off
            }
        }
    }

    /// The sequence emitter — ONE emit + frame for every effort level (the format is the face; the
    /// finder is the dial, `FinderRecall`).
    struct SeqEmit {
        lits: Vec<u8>,
        lls: Vec<u8>,
        mls: Vec<u8>,
        offs: Vec<u8>,
        raw: BitW,
        creps: [usize; 3],
        nseq: u64,
    }
    impl SeqEmit {
        fn new() -> Self {
            SeqEmit {
                lits: Vec::new(),
                lls: Vec::new(),
                mls: Vec::new(),
                offs: Vec::new(),
                raw: BitW::new(),
                creps: [1, 4, 8],
                nseq: 0,
            }
        }
        /// Emit one sequence ⟨ll, ml−2, offVal⟩ — no flags (`SequenceFraming`): the literal run
        /// `[lst, i)` closed by the match `(len, off)` at `i`.
        #[inline]
        fn push(&mut self, data: &[u8], lst: usize, i: usize, len: usize, off: usize) {
            enc_len16((i - lst) as u32, &mut self.lls, &mut self.raw);
            enc_len16((len - SEQ_MIN_MATCH) as u32, &mut self.mls, &mut self.raw);
            let ov = rep_encode(&mut self.creps, off);
            enc_len16(ov, &mut self.offs, &mut self.raw);
            self.lits.extend_from_slice(&data[lst..i]);
            self.nseq += 1;
        }
        /// Assemble the frame: `[rawLen][nSeq][trailLits][litMode][gen][litSec][llSec][mlSec][offSec]
        /// [rawBits]`. `lit_mode`: 0 = tans4 literals; 2 = CM literals (the hybrid — the density crown
        /// takes the residual the parse leaves, ONE format, one executor). `gen` = the router's basis
        /// coordinate (see `route_gen`); the caller parsed the TRANSFORMED stream, decode inverts last.
        fn finish(mut self, data: &[u8], lst: usize, lit_mode_in: u8, gen: u8) -> Vec<u8> {
            // The literal arm is a MEASURED Kfam min with ONE full-stream encode: fold the trail into
            // the literal stream, decide o0-vs-o1 on a ≤256 KiB sample (`context_table_is_the_fold` —
            // the o1 family wins on residues with byte structure, loses its headers on noise), encode
            // the winner once. The raw-trail arm (mode 0 — protects match-free noise from the coder's
            // ~2% expansion) is priced only when the trail is big enough to matter. Modes:
            // 0 = blocked-o0 lits + RAW trail · 2 = CM lits (hybrid) · 3 = blocked-o0, trail folded ·
            // 4 = context-o1, trail folded.
            let ntrail = data.len() - lst;
            let mut lit_mode = lit_mode_in;
            let mut trail: &[u8] = &[];
            let litc = if lit_mode == 2 {
                self.lits.extend_from_slice(&data[lst..]);
                crate::codec::compress_at(crate::codec::DEFAULT_ORDER, &self.lits)
            } else if lit_mode == 5 {
                // the FAST face's literals — the stateless corner BY FACE POLICY
                // (`PrefixCodeReadout`: no inter-symbol state, the decode chain is a shift; the
                // integer-bit density loss is the face's priced trade, like turbo's no-entropy).
                // `compress4h`'s raw arm still protects match-free noise from expansion.
                let mut folded = std::mem::take(&mut self.lits);
                folded.extend_from_slice(&data[lst..]);
                huf_blocked(&folded)
            } else {
                let nlits = self.lits.len();
                let mut folded = std::mem::take(&mut self.lits);
                folded.extend_from_slice(&data[lst..]);
                let sample = &folded[..folded.len().min(LIT_BLOCK)];
                let s0 = crate::codec::tans::compress4(sample).len();
                let s1 = crate::codec::tans::compress4_o1(sample).len();
                let (cand, mode) = if s1 < s0 {
                    (crate::codec::tans::compress4_o1(&folded), 4u8)
                } else {
                    (tans4_blocked_at(&folded, LIT_BLOCK), 3u8)
                };
                if ntrail > 4096 {
                    let base = tans4_blocked_at(&folded[..nlits], LIT_BLOCK);
                    if base.len() + ntrail < cand.len() {
                        lit_mode = 0;
                        trail = &data[lst..];
                        base
                    } else {
                        lit_mode = mode;
                        cand
                    }
                } else {
                    lit_mode = mode;
                    cand
                }
            };
            let llc = tans4_blocked(&self.lls);
            let mlc = tans4_blocked(&self.mls);
            let offc = tans4_blocked(&self.offs);
            let rawb = self.raw.finish();
            let mut out = Vec::with_capacity(
                litc.len() + llc.len() + mlc.len() + offc.len() + rawb.len() + trail.len() + 32,
            );
            put_uvarint(&mut out, data.len() as u64);
            put_uvarint(&mut out, self.nseq);
            put_uvarint(&mut out, (data.len() - lst) as u64); // trailLits count (source, both modes)
            out.push(lit_mode);
            out.push(gen);
            put_uvarint(&mut out, litc.len() as u64);
            out.extend_from_slice(&litc);
            out.extend_from_slice(&llc);
            out.extend_from_slice(&mlc);
            out.extend_from_slice(&offc);
            put_uvarint(&mut out, rawb.len() as u64);
            out.extend_from_slice(&rawb);
            out.extend_from_slice(trail);
            out
        }
    }

    /// Seq compress, Dense effort: the RowFinder parse (G=16 retention window, LZMA rep candidates,
    /// greedy) emitting flagless sequences. Same format as [`compress_seq_fast`]; ONE decoder serves
    /// both. `SEQ_LAZY` env = the deeper Kfam-min approximation (lookahead 1): measured in the o1 era
    /// (2026-07-11) prose 2.8675→2.9017 / code 3.8443→3.9082 at −29% enc, decode-cycles FLAT — the
    /// denser point when encode time is cheap; greedy stays the default (zstd-9's enc class).
    #[must_use]
    pub fn compress_seq(data: &[u8]) -> Vec<u8> {
        let g = route_gen(data);
        if g == 0 {
            return compress_seq_inner(data, 0);
        }
        compress_seq_inner(&gen_apply(g, data), g)
    }

    /// Seq compress, MAX effort: the cost-optimal parse ([`compress_seq_optimal`]) — the density crown
    /// of the seq family. Same format, same fast decode; the encode is slow (zstd-19 class) by
    /// construction. The compress-once-load-many profile (.seed serialisation): pay the parse once,
    /// win density AND decode speed on every load.
    #[must_use]
    pub fn compress_max(data: &[u8]) -> Vec<u8> {
        let g = route_gen(data);
        if g == 0 {
            return compress_seq_optimal(data, 0);
        }
        compress_seq_optimal(&gen_apply(g, data), g)
    }
    /// Derive the parse prices `(μ, L)` in QUARTER-BITS per byte from a 256 KiB sample —
    /// `ParsePrice.defer_wins_iff_offset_priced` consumes MEASURED rates, nothing is dialed:
    /// μ = the fast face's achieved rate on the sample (this input's residual rate);
    /// L = the o1 literal coder's achieved rate on the sample's greedy literals (a single-cell
    /// greedy mini-parse collects the literal POPULATION — raw sample bytes would overstate it).
    /// Sparse-literal fallback: `L = 2μ`, the zstd corner (`zstd_threshold_is_L_eq_two_mu`).
    fn derive_parse_prices(data: &[u8]) -> (i64, i64) {
        let sample = &data[..data.len().min(1 << 18)];
        let sn = sample.len().max(1);
        let muq = ((compress_seq_fast_inner(sample, 0).len() * 32 / sn) as i64).clamp(2, 32);
        let n = sample.len();
        let mut cell = CellTable::new(15);
        let mut lits: Vec<u8> = Vec::new();
        let (mut i, mut lst) = (0usize, 0usize);
        while i + 8 <= n {
            let h = read8(sample, i).wrapping_mul(GOLD64);
            let (p1, tag, slot) = cell.probe(h);
            cell.insert_at(slot, i, (h >> 8) as u8);
            if p1 > 0 && tag == (h >> 8) as u8 {
                let j = p1 - 1;
                if j < i && read4(sample, j) == read4(sample, i) {
                    let l = lce(sample, i, j, n);
                    if l >= 4 {
                        lits.extend_from_slice(&sample[lst..i]);
                        i += l;
                        lst = i;
                        continue;
                    }
                }
            }
            i += 1;
        }
        lits.extend_from_slice(&sample[lst..n]);
        let lq = if lits.len() >= 4096 {
            ((crate::codec::tans::compress4_o1(&lits).len() * 32 / lits.len()) as i64).clamp(2, 32)
        } else {
            2 * muq
        };
        (muq, lq)
    }

    fn compress_seq_inner(data: &[u8], gen: u8) -> Vec<u8> {
        let n = data.len();
        let mut rf = RowFinder::new(n);
        let mut freps = [0usize; 4]; // the FINDER's rep candidates (search hints, LZMA MTF)
        let mut em = SeqEmit::new();
        // hoisted: NEVER read env in the loop. Dense IS the density face, so price-lazy is ON by
        // default: `ParsePrice.defer_wins_iff_offset_priced` governs each deferral, the depth is only
        // the search bound. depth 2 = the proven μ=4,L=8 corner (`zstd_gain_rule_is_mu_four_L_eight`,
        // zstd-9's lazy). MEASURED 2026-07-11 (code8/prose8): greedy→depth2 code 3.824→3.942, prose
        // 2.877→2.94 at ~1.7× enc, decode +14%; the knee is depth 4 (code 3.964, +0.5% at +50% enc) —
        // env SEQ_LAZY overrides for that deeper search when encode time is free.
        let lazy_depth = std::env::var("SEQ_LAZY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2);
        let (muq, lq) = if lazy_depth > 0 {
            derive_parse_prices(data)
        } else {
            (4, 8)
        };
        let (mut i, mut lst) = (0usize, 0usize);
        while i < n {
            let f = rf.find(data, i, n, &freps);
            if f.len == 0 {
                rf.prefetch(data, i + 1, n); // next probe's rows ride the insert work
                rf.insert(data, i, n);
                i += 1;
                continue;
            }
            rf.insert(data, i, n);
            // THE PRICE-LAZY WALK (ParsePrice.defer_wins_iff_offset_priced): slide up to
            // `lazy_depth` positions past the current best start; a later match wins iff
            //   μ·l2 − 4·bl(off2) > μ·l1 − 4·bl(off1) + k·(L − μ)   (quarter-bit ints, k deferred)
            // with bl = offset residual bits (EXACT in our format: the offVal slot carries
            // bitlen−1 raw bits), bl = 0 for a rep. μ, L are DERIVED from this input; zstd's
            // lazy gain rule is the frozen corner μ=4, L=8 (zstd_gain_rule_is_mu_four_L_eight),
            // their depth-2 "+7" a recorded one-unit fudge below the exact 2·(L−μ).
            let (mut start, mut best) = (i, f);
            let mut walked = i; // last position inserted into the finder (span-index resumes after it)
            if lazy_depth > 0 && best.len < MATCH_ENOUGH {
                let price = |g: &Found| -> i64 {
                    let b = if g.rep >= 0 {
                        0
                    } else {
                        4 * i64::from(64 - (g.off as u64).leading_zeros())
                    };
                    g.len as i64 * muq - b
                };
                while walked + 1 < n && walked - start < lazy_depth {
                    walked += 1;
                    rf.insert(data, walked, n);
                    let f2 = rf.find(data, walked, n, &freps);
                    if f2.len > 0
                        && price(&f2) > price(&best) + (walked - start) as i64 * (lq - muq)
                    {
                        start = walked;
                        best = f2;
                        if best.len >= MATCH_ENOUGH {
                            break;
                        }
                    }
                }
            }
            // backward CATCH-UP (fresh offsets only — a rep's start is already pinned): bytes
            // equal to their offset-back twin belong to the copy, not the literal run
            if best.rep < 0 {
                while start > lst
                    && best.off < start
                    && data[start - 1] == data[start - 1 - best.off]
                {
                    start -= 1;
                    best.len += 1;
                }
            }
            em.push(data, lst, start, best.len, best.off);
            if best.rep >= 0 {
                mtf(&mut freps, best.rep as usize);
            } else {
                freps = [best.off, freps[0], freps[1], freps[2]];
            }
            let end = start + best.len;
            rf.prefetch(data, end, n); // next probe's rows ride the span-index + emit work
            rf.index_span(data, walked.max(start) + 1, end, n);
            i = end;
            lst = i;
        }
        em.finish(data, lst, 0, gen)
    }

    impl RowFinder {
        /// Collect ALL row candidates at `i` (offset, extend-length) for the optimal parse — the
        /// finder returns the full match SET, not just the greedy best, so the DP can price every
        /// offset. Reps are omitted (priced fresh in the DP; the emit's `rep_encode` still codes a
        /// coincidental rep cheaply, so the DP over-estimates and never under-counts).
        #[inline]
        fn candidates(&self, data: &[u8], i: usize, n: usize, out: &mut Vec<(usize, usize)>) {
            out.clear();
            if i + 8 > n {
                return;
            }
            let anchor4 = read4(data, i);
            let (row, tag) = row_hash(data, i, self.row_bits);
            let base = row * GROUP;
            let mut m = group_tag_mask(&self.tags, base, tag);
            while m != 0 {
                let lane = m.trailing_zeros() as usize;
                m &= m - 1;
                let p = unsafe { *self.pos.get_unchecked(base + lane) } as usize;
                if p != 0 && p - 1 < i && read4(data, p - 1) == anchor4 {
                    let l = extend(data, i, p - 1);
                    if l >= MIN_MATCH {
                        out.push((i - (p - 1), l));
                    }
                }
            }
        }
    }

    /// The number of match lengths per candidate the DP prices explicitly. Beyond a match this long,
    /// literals never win, so the maximal length is priced alone; below it, EVERY length is a distinct
    /// DP edge (the non-greedy alignment freedom — take fewer bytes now to reach a cheaper next match).
    const OPT_LEN_CAP: usize = 64;

    /// Seq compress, OPTIMAL effort: the cost-minimising parse (`ParsePrice` prices, a forward
    /// shortest-path DP over positions). Unlike greedy/lazy it prices EVERY offset by its bitlength,
    /// so it prefers cheaper/closer offsets — directly shrinking the offset budget (the 44.5% of the
    /// code8 output that is fresh-offset bits). Slow enc (zstd-19 class) by construction; decode is
    /// the SAME format, unchanged.
    fn compress_seq_optimal(data: &[u8], gen: u8) -> Vec<u8> {
        let n = data.len();
        if n < 256 {
            return compress_seq_inner(data, gen);
        }
        let mut rf = RowFinder::new(n);
        let (_muq, lq) = derive_parse_prices(data);
        let litq = lq.max(1); // quarter-bits per literal byte
        const SLOT_QB: i64 = 20; // ~5 bits: the tANS-coded slot symbol's average price
        let off_qb = |off: usize| -> i64 {
            let v = (off + 2) as u64; // enc_len16 codes off+2
            SLOT_QB
                + if v < 16 {
                    0
                } else {
                    4 * (63 - v.leading_zeros() as i64)
                }
        };
        let ml_qb = |mlv: usize| -> i64 {
            let v = mlv as u64;
            SLOT_QB
                + if v < 16 {
                    0
                } else {
                    4 * (63 - v.leading_zeros() as i64)
                }
        };
        const INF: i64 = i64::MAX / 4;
        let mut cost = vec![INF; n + 1];
        let mut bp = vec![(0u32, 0u32, 0u32); n + 1]; // (prev_pos, off | 0=literal, len)
        cost[0] = 0;
        let mut cand: Vec<(usize, usize)> = Vec::with_capacity(GROUP);
        for i in 0..n {
            let ci = cost[i];
            if ci >= INF {
                rf.insert(data, i, n);
                continue;
            }
            // literal edge
            if ci + litq < cost[i + 1] {
                cost[i + 1] = ci + litq;
                bp[i + 1] = (i as u32, 0, 0);
            }
            rf.insert(data, i, n);
            rf.candidates(data, i, n, &mut cand);
            for &(off, maxlen) in &cand {
                let base = ci + SLOT_QB + off_qb(off); // ll-symbol + off code
                let cap = maxlen.min(OPT_LEN_CAP);
                let mut l = MIN_MATCH;
                while l <= cap {
                    let c = base + ml_qb(l - SEQ_MIN_MATCH);
                    if c < cost[i + l] {
                        cost[i + l] = c;
                        bp[i + l] = (i as u32, off as u32, l as u32);
                    }
                    l += 1;
                }
                if maxlen > cap {
                    let c = base + ml_qb(maxlen - SEQ_MIN_MATCH);
                    if c < cost[i + maxlen] {
                        cost[i + maxlen] = c;
                        bp[i + maxlen] = (i as u32, off as u32, maxlen as u32);
                    }
                }
            }
        }
        // backtrack: collect (start, off, len) matches from n to 0
        let mut steps: Vec<(usize, usize, usize)> = Vec::new();
        let mut pos = n;
        while pos > 0 {
            let (prev, off, len) = bp[pos];
            if off != 0 {
                steps.push((prev as usize, off as usize, len as usize));
            }
            pos = prev as usize;
        }
        steps.reverse();
        // forward emit
        let mut em = SeqEmit::new();
        let mut lst = 0usize;
        for (start, off, len) in steps {
            em.push(data, lst, start, len, off);
            lst = start + len;
        }
        em.finish(data, lst, 0, gen)
    }

    /// Seq compress, Fast effort: the turbo single-cell finder (t8, `FinderRecall` G=1) driving the
    /// SAME sequence format — zstd-1's class, our parts.
    #[must_use]
    pub fn compress_seq_fast(data: &[u8]) -> Vec<u8> {
        let g = route_gen(data);
        if g == 0 {
            return compress_seq_fast_inner(data, 0);
        }
        compress_seq_fast_inner(&gen_apply(g, data), g)
    }
    fn compress_seq_fast_inner(data: &[u8], gen: u8) -> Vec<u8> {
        // THE DOUBLE-FAST FINDER (zstd_double_fast.c, read at source; our parts kept): a LONG
        // 8-byte anchor (rare, high-precision — keys the FinderRecall window on long context)
        // plus a SHORT 5-byte anchor (catches the 4-7 B matches a t8 finder cannot see at all).
        // Priority per position: rep0 (the monad) -> long -> short-then-try-longer-at-i+1 (their
        // _search_next_long: a short match is accepted only after the next position fails to
        // offer a long one). After EVERY match: backward CATCH-UP (the match start walks left
        // over pending literals — bytes that equal their offset-back twin belong to the copy,
        // not the literal run) and sparse in-match insertion (start+2, end-2 — their pattern;
        // full span indexing is the Dense face's job).
        let n = data.len();
        const SBITS: u32 = 17; // the short table: 2^17 cells, L2-resident
        let mut lcell = CellTable::new(TURBO_BITS); // the long 8-byte-anchor window
        let mut scell = CellTable::new(SBITS); // the short 5-byte-anchor window
        let mut em = SeqEmit::new();
        let (mut i, mut lst, mut miss) = (0usize, 0usize, 0usize);
        // THE MIXTURE-POSTERIOR STRIDE (StrideSampling.mixture_stride_sq, the external proof development):
        // s*(k)^2 = A·(k+1)(k+2) + B·(k+2) — the sqrt law at short runs (B = probe/lateness
        // cost), the LINEAR tail at long runs (slope sqrt(A) = the incompressibility odds —
        // the folklore ramp, now derived). Incremental bump, no sqrt, ~one compare per miss.
        // STRIDE_AB="an/ad,bn/bd" = the calibration harness (hoisted); frozen after the sweep.
        let ((an, ad), (bn, bd)) = std::env::var("STRIDE_AB")
            .ok()
            .and_then(|v| {
                let mut parts = v.split(',');
                let frac = |t: &str| -> Option<(u64, u64)> {
                    let mut it = t.split('/');
                    Some((
                        it.next()?.parse().ok()?,
                        it.next().unwrap_or("1").parse().ok()?,
                    ))
                };
                let a = frac(parts.next()?)?;
                let b = frac(parts.next()?)?;
                Some((a, b))
            })
            .unwrap_or(((1, 4096), (1, 16)));
        // the DEFERRAL threshold (ParsePrice.defer_wins_iff, the external proof development): defer a short
        // match for a longer long-match at i+1 iff l1 > l + t, t = ceil(L/mu - 1) — the literal's
        // coded price L over the stream's residual rate mu. zstd's uncommented "+1" is the corner
        // L = 2mu (zstd_threshold_is_L_eq_two_mu); our huf literals price L/mu AT the 0/1 boundary
        // (measured sweep 2026-07-11: t=0 weakly dominates — code 3.5906 vs 3.5894, prose flat,
        // weights/audio flat — the denser-literal direction the theorem predicts). Default 0,
        // derived + measured; DEFER_T = the calibration harness (hoisted).
        let defer_t = std::env::var("DEFER_T")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut stride = 1u64;
        macro_rules! h8 {
            ($i:expr) => {
                read8(data, $i).wrapping_mul(GOLD64)
            };
        }
        macro_rules! h5 {
            ($i:expr) => {
                (read8(data, $i) & 0xFF_FFFF_FFFF).wrapping_mul(GOLD64)
            };
        }
        if n >= 8 {
            'outer: loop {
                let (mut mpos, mut mlen, mut moff);
                loop {
                    if i + 8 > n {
                        break 'outer;
                    }
                    // the rep probe is DEAD at a match end (ParsePrice.rep_probe_at_match_end_
                    // is_dead): lce-maximality means the first byte already differs from one
                    // rep0-offset back — zstd's uncommented ip+1 rep check, derived. `i == lst`
                    // marks exactly that position (and the harmless stream start).
                    let r0 = em.creps[0];
                    if i != lst && i >= r0 && read4(data, i) == read4(data, i - r0) {
                        let l = lce(data, i, i - r0, n);
                        if l >= MIN_MATCH {
                            mpos = i;
                            mlen = l;
                            moff = r0;
                            break;
                        }
                    }
                    let hl = h8!(i);
                    let tl = (hl >> 8) as u8;
                    let (pl, sl, bl) = lcell.probe(hl);
                    let hs = h5!(i);
                    let ts = (hs >> 8) as u8;
                    let (ps, ss, bs) = scell.probe(hs);
                    lcell.insert_at(bl, i, tl);
                    scell.insert_at(bs, i, ts);
                    // long first — an 8-byte anchor hit is almost always the better program
                    if pl != 0 && sl == tl && pl - 1 < i {
                        let l = lce(data, i, pl - 1, n);
                        if l >= MIN_MATCH {
                            mpos = i;
                            mlen = l;
                            moff = i - (pl - 1);
                            break;
                        }
                    }
                    // short anchor: accept only if the NEXT position offers no long match
                    if ps != 0 && ss == ts && ps - 1 < i && read4(data, ps - 1) == read4(data, i) {
                        let l = lce(data, i, ps - 1, n);
                        if l >= MIN_MATCH {
                            mpos = i;
                            mlen = l;
                            moff = i - (ps - 1);
                            if i + 9 <= n {
                                let hl1 = h8!(i + 1);
                                let tl1 = (hl1 >> 8) as u8;
                                let (pl1, sl1, _) = lcell.probe(hl1);
                                if pl1 != 0 && sl1 == tl1 && pl1 - 1 < i + 1 {
                                    let l1 = lce(data, i + 1, pl1 - 1, n);
                                    if l1 > l + defer_t {
                                        mpos = i + 1;
                                        mlen = l1;
                                        moff = i + 1 - (pl1 - 1);
                                    }
                                }
                            }
                            break;
                        }
                    }
                    miss += 1;
                    let k = miss as u64;
                    while (stride + 1) * (stride + 1) * ad * bd
                        <= (k + 1) * (k + 2) * an * bd + (k + 2) * bn * ad
                    {
                        stride += 1;
                    }
                    i += stride as usize;
                }
                // backward catch-up: literals that equal their offset-back twin join the copy
                while mpos > lst && mpos > moff && data[mpos - 1] == data[mpos - 1 - moff] {
                    mpos -= 1;
                    mlen += 1;
                }
                em.push(data, lst, mpos, mlen, moff);
                let end = mpos + mlen;
                // sparse in-match insertion (start+2, end-2): keeps long matches findable without
                // the Dense face's full span indexing
                for k in [mpos + 2, end.saturating_sub(2)] {
                    if k + 8 <= n && k < end {
                        lcell.insert(h8!(k), k);
                        scell.insert(h5!(k), k);
                    }
                }
                i = end;
                lst = i;
                miss = 0;
                stride = 1;
            }
        }
        em.finish(data, lst, 5, gen) // huf literals — the Fast face's stateless corner
    }

    /// Inverse of [`compress_seq`] — bulk-pre-decode every stream (tans4), then ONE executor loop:
    /// per sequence, three slot reads + the raw residuals + a rep branch + two copies. No entropy
    /// table walk and no adaptive state inside the loop (the zstd decode shape, our parts).
    ///
    /// The pre-decoded slot `Vec`s are MEASURED cheaper than fusing the slot decode into the loop
    /// (zstd-style, no materialization): the fused pull-cursor was built and rejected 2026-07-11 —
    /// +17% cycles prose / +11% code, +12% INSTRUCTIONS at IPC ~4 (throughput-bound, not latency).
    /// The arrays cost ~1 insn/symbol each way (L2-resident, ~2 MB); any in-place cursor over the
    /// blocked 4-stream frames pays 3-4 insn/symbol in block/stream/raw-escape checks, and pulling
    /// quarters in order serializes the 4-way chain besides. zstd's fusion is free only because
    /// their block is ONE serial bit-container with always-on extra-bits; our format isn't.
    ///
    /// Malformed input yields an empty `Vec` (never a panic); [`try_decompress_seq`] says why.
    #[must_use]
    pub fn decompress_seq(bytes: &[u8]) -> Vec<u8> {
        try_decompress_seq(bytes).unwrap_or_default()
    }

    /// Checked [`decompress_seq`]. A match can legitimately expand a few bytes into many, so the
    /// declared length is capped at [`DEFAULT_MAX_OUTPUT`] rather than bounded by the input; the
    /// output buffer is only sized up front when the declared length is plausible for the input
    /// (≤ 64× it) and otherwise grows as sequences actually produce bytes.
    pub fn try_decompress_seq(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            return unsafe { decompress_seq_bmi2(bytes) };
        }
        decompress_seq_impl(bytes)
    }
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn decompress_seq_bmi2(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        decompress_seq_impl(bytes)
    }

    /// The highest slot symbol the encoder emits: 0..=15 exact, 16..=43 a bit-length class.
    const MAX_SLOT: u8 = 43;

    /// Grow the working output (zero-filled; its length is the writable extent) to at least `need`
    /// bytes, never past `cap` — the declared length plus slack.
    #[cold]
    fn grow_out(out: &mut Vec<u8>, need: usize, cap: usize) -> Result<(), DecodeError> {
        let target = need.max(out.len().saturating_mul(2)).min(cap);
        if target < need {
            return Err(DecodeError::Malformed);
        }
        out.try_reserve_exact(target - out.len())
            .map_err(|_| DecodeError::TooLarge)?;
        out.resize(target, 0);
        Ok(())
    }

    #[inline(always)]
    fn decompress_seq_impl(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut p = 0usize;
        let n = read_len(bytes, &mut p)?;
        let nseq = read_len(bytes, &mut p)?;
        let ntrail = read_len(bytes, &mut p)?;
        let lit_mode = take_slice(bytes, &mut p, 1)?[0];
        let gen = take_slice(bytes, &mut p, 1)?[0];
        if gen > 4 {
            return Err(DecodeError::Malformed);
        }
        let litlen = read_len(bytes, &mut p)?;
        // every sequence closes a match of at least SEQ_MIN_MATCH bytes
        if n > DEFAULT_MAX_OUTPUT || nseq > n / SEQ_MIN_MATCH || ntrail > n {
            return Err(DecodeError::TooLarge);
        }
        let lit_stream = take_slice(bytes, &mut p, litlen)?;
        let lits = if lit_mode == 2 {
            crate::codec::try_decompress_framed(lit_stream)?
        } else if lit_mode == 4 {
            crate::codec::tans::try_decompress4_o1(lit_stream)?
        } else if lit_mode == 5 {
            unhuf_blocked(lit_stream)?
        } else {
            let mut lp0 = 0usize;
            untans4_blocked(lit_stream, &mut lp0)?
        };
        let lls = untans4_blocked(bytes, &mut p)?;
        let mls = untans4_blocked(bytes, &mut p)?;
        let offs = untans4_blocked(bytes, &mut p)?;
        // one slot per sequence per stream, every slot a symbol the encoder can emit
        for s in [&lls, &mls, &offs] {
            if s.len() != nseq || s.iter().any(|&x| x > MAX_SLOT) {
                return Err(DecodeError::Malformed);
            }
        }
        let rawlen = read_len(bytes, &mut p)?;
        let mut raw = BitR::new(take_slice(bytes, &mut p, rawlen)?);
        let mut creps = [1usize, 4, 8];
        // wildcopy slack past the declared length; `out.len()` is the extent the loop may write
        let cap = n + 64;
        let init = if cap <= bytes.len().saturating_mul(64).saturating_add(4096) {
            cap
        } else {
            bytes.len().saturating_mul(64).saturating_add(4096)
        };
        let mut out = try_zeroed(init)?;
        let (mut op, mut lp) = (0usize, 0usize);
        let (mut llp, mut mlp, mut ofp) = (0usize, 0usize, 0usize);
        for _ in 0..nseq {
            // decode ALL THREE fields before any copy — the match-source address `op+ll−off` is
            // live before the literal move issues, so the OoO window overlaps its L2-class load
            // (annotate 2026-07-11: that ONE load held 26% of the executor) with the copies
            let ll = dec_len16(&lls, &mut llp, &mut raw) as usize;
            let ml = dec_len16(&mls, &mut mlp, &mut raw) as usize + SEQ_MIN_MATCH;
            let off_val = dec_len16(&offs, &mut ofp, &mut raw);
            let off = rep_decode(&mut creps, off_val);
            // ONE dominating edge filter (zstd's execSequenceEnd cut; `DeterministicTrips` — the
            // hoisted guard proves every copy below): literal room incl. the ≤31 B overshoot,
            // output room for both copies' overshoot, match source inside the written prefix.
            if lp + ll + 32 <= lits.len()
                && op + ll + ml + 32 <= out.len()
                && off >= 1
                && off <= op + ll
            {
                unsafe {
                    let lsrc = lits.as_ptr().add(lp);
                    let base = out.as_mut_ptr();
                    // literals: one UNCONDITIONAL 16 B move; the > 16 tail is the unlikely arm
                    // (zstd's bet, zstd_decompress_block.c ZSTD_execSequence: "litLength <= 16
                    // is nearly always true")
                    std::ptr::copy_nonoverlapping(lsrc, base.add(op), 16);
                    if ll > 16 {
                        let (mut s, mut d, e) =
                            (lsrc.add(16), base.add(op + 16), base.add(op + ll));
                        while d < e {
                            std::ptr::copy_nonoverlapping(s, d, 16);
                            s = s.add(16);
                            d = d.add(16);
                        }
                    }
                    op += ll;
                    lp += ll;
                    // match: the LIKELY wide arm first (nearly all offsets ≥ 16), overlap spread else
                    if off >= 16 {
                        wildcopy_wide(base, op - off, op, ml);
                    } else {
                        wildcopy_close(base, op - off, op, ml);
                    }
                    op += ml;
                }
            } else {
                // CAREFUL PATH: stream tail / long-literal squeeze / growth / corrupt input
                // (bounds-checked; every operand is untrusted, so the arithmetic is too)
                let end = op
                    .checked_add(ll)
                    .and_then(|x| x.checked_add(ml))
                    .filter(|&e| e <= n)
                    .ok_or(DecodeError::Malformed)?; // the sequences outrun the declared output
                let lend = lp
                    .checked_add(ll)
                    .filter(|&e| e <= lits.len())
                    .ok_or(DecodeError::Truncated)?;
                if off == 0 || off > op + ll {
                    return Err(DecodeError::Malformed);
                }
                if end + 64 > out.len() {
                    grow_out(&mut out, end + 64, cap)?;
                }
                out[op..op + ll].copy_from_slice(&lits[lp..lend]);
                op += ll;
                lp = lend;
                wildcopy_checked(&mut out, op - off, op, ml)?;
                op += ml;
            }
        }
        if raw.overrun() || ntrail > n - op {
            return Err(DecodeError::Truncated);
        }
        if op + ntrail != n {
            return Err(DecodeError::Malformed);
        }
        if out.len() < n {
            grow_out(&mut out, n, cap)?;
        }
        if lit_mode >= 2 {
            let t = lits.get(lp..lp + ntrail).ok_or(DecodeError::Truncated)?;
            out[op..op + ntrail].copy_from_slice(t);
        } else {
            let t = take_slice(bytes, &mut p, ntrail)?;
            out[op..op + ntrail].copy_from_slice(t);
        }
        out.truncate(n);
        Ok(gen_invert(gen, out)) // the Being-cell rotation undone LAST (encode applied it first)
    }

    // ── THE PUBLIC MID FACE — seq Dense under the historical names; hybrid = same format, CM literals ──

    /// The mid face: seq Dense (see [`compress_seq`]). The LZMA-shaped face this replaced lived here
    /// until 2026-07-10; `SequenceFraming` proves its per-token flags coded zero information, and the
    /// measured verdict was strict domination (ratio +3.9%, dec 3.3×, equal enc).
    #[must_use]
    pub fn compress(data: &[u8]) -> Vec<u8> {
        compress_seq(data)
    }
    /// Inverse of [`compress`]. Malformed input yields an empty `Vec`; [`try_decompress`] says why.
    #[must_use]
    pub fn decompress(bytes: &[u8]) -> Vec<u8> {
        decompress_seq(bytes)
    }

    /// Checked [`decompress`].
    pub fn try_decompress(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        try_decompress_seq(bytes)
    }

    /// Derive `lit_est` — the CM's ACTUAL bits/byte on the literal residual, so the hybrid router
    /// weighs a match against the CM's real price, not a dialed constant. Parse a prefix, CM-compress
    /// its literals, read the achieved rate. `LIT_EST` env forces a value.
    fn derive_lit_est(data: &[u8]) -> f64 {
        if let Some(v) = std::env::var("LIT_EST")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
        {
            return v;
        }
        let sample = &data[..data.len().min(1 << 19)];
        let (em, _lst) = parse_hybrid_seq(sample, LIT_EST as f64);
        if em.lits.len() < 4096 {
            return LIT_EST as f64;
        }
        let litc = crate::codec::cm::compress(crate::codec::DEFAULT_ORDER, &em.lits);
        (8.0 * litc.len() as f64 / em.lits.len() as f64).clamp(0.5, 8.0)
    }

    /// The hybrid parse — THE ROUTER: at each byte, take the match only when its token codes CHEAPER
    /// than the CM would code those bytes as literals (`lit_est` b/B). Prose's short far-matches route
    /// to the CM (density); repetition's long runs route to sequences (throughput). The mix is the
    /// data's shape, decided per byte, not dialed. Single-slot hash4 finder + rep0 (the price gate
    /// makes deep search pointless — the CM outbids marginal matches anyway).
    fn parse_hybrid_seq(data: &[u8], lit_est: f64) -> (SeqEmit, usize) {
        let n = data.len();
        let mut htab = vec![0u32; 1usize << HBITS];
        let mut em = SeqEmit::new();
        let (mut rep0, mut i, mut lst) = (0usize, 0usize, 0usize);
        while i < n {
            let (mut best_len, mut best_off) = (0usize, 0usize);
            if rep0 > 0 && i >= rep0 {
                let l = extend(data, i, i - rep0);
                if l >= MIN_MATCH {
                    best_len = l;
                    best_off = rep0;
                }
            }
            if i + MIN_MATCH <= n {
                let h = hash4(data, i);
                let p = htab[h] as usize;
                if p != 0 && p - 1 < i {
                    let l = extend(data, i, p - 1);
                    if l >= MIN_MATCH && l > best_len {
                        best_len = l;
                        best_off = i - (p - 1);
                    }
                }
                htab[h] = (i + 1) as u32;
            }
            let take = best_len >= MIN_MATCH && {
                let bitlen = |x: usize| 32 - (x as u32).leading_zeros();
                let lc = 2 * bitlen(best_len - MIN_MATCH + 1);
                let mc = if best_off == rep0 {
                    2 + lc
                } else {
                    2 + 2 * bitlen(best_off) + lc
                };
                (mc as f64) < best_len as f64 * lit_est
            };
            if take {
                em.push(data, lst, i, best_len, best_off);
                rep0 = best_off;
                let end = i + best_len;
                let mut k = i + 1;
                while k < end && k + MIN_MATCH <= n {
                    htab[hash4(data, k)] = (k + 1) as u32;
                    k += 1;
                }
                i = end;
                lst = i;
            } else {
                i += 1;
            }
        }
        (em, lst)
    }

    /// The hybrid: the price-routed parse, sequences + CM literals — ONE seq format (litMode 2), one
    /// executor; the density crown takes exactly the residual the parse leaves it.
    #[must_use]
    pub fn compress_hybrid(data: &[u8]) -> Vec<u8> {
        let lit_est = derive_lit_est(data);
        let (em, lst) = parse_hybrid_seq(data, lit_est);
        em.finish(data, lst, 2, 0)
    }
    /// Inverse of [`compress_hybrid`] — [`decompress_seq`] dispatches on litMode.
    #[must_use]
    pub fn decompress_hybrid(bytes: &[u8]) -> Vec<u8> {
        decompress_seq(bytes)
    }

    /// Checked [`decompress_hybrid`].
    pub fn try_decompress_hybrid(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        try_decompress_seq(bytes)
    }

    /// Probe harness — the Dense parse's literal residue (the stream the o1 coordinate would code).
    #[doc(hidden)]
    #[must_use]
    pub fn parse_lits(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut rf = RowFinder::new(n);
        let mut freps = [0usize; 4];
        let mut em = SeqEmit::new();
        let (mut i, mut lst) = (0usize, 0usize);
        while i < n {
            let f = rf.find(data, i, n, &freps);
            if f.len == 0 {
                rf.insert(data, i, n);
                i += 1;
                continue;
            }
            rf.insert(data, i, n);
            em.push(data, lst, i, f.len, f.off);
            if f.rep >= 0 {
                mtf(&mut freps, f.rep as usize);
            } else {
                freps = [f.off, freps[0], freps[1], freps[2]];
            }
            rf.index_span(data, i + 1, i + f.len, n);
            i += f.len;
            lst = i;
        }
        em.lits
    }

    /// Ablation harness — run ONLY the match-finder loop (find + insert + advance), no coding, no tANS.
    /// Returns total matched length. Used to isolate where compress time actually goes.
    #[doc(hidden)]
    #[must_use]
    pub fn parse_bench(data: &[u8]) -> usize {
        let n = data.len();
        let mut rf = RowFinder::new(n);
        let mut reps = [0usize; 4];
        let (mut i, mut total) = (0usize, 0usize);
        while i < n {
            let f = rf.find(data, i, n, &reps);
            rf.insert(data, i, n);
            if f.len == 0 {
                i += 1;
                continue;
            }
            if f.rep < 0 {
                reps = [f.off, reps[0], reps[1], reps[2]];
            }
            rf.index_span(data, i + 1, i + f.len, n);
            total += f.len;
            i += f.len;
        }
        total
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lz_roundtrip() {
            let cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![0u8; 5000],
                b"the quick brown fox. the quick brown dog. ".repeat(300),
                (0..40000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(),
                b"aaaaaaaaaaaaaaaaaaaaaaaaaX".repeat(500), // deep RLE (overlap copies)
            ];
            for c in cases {
                assert_eq!(
                    decompress(&compress(&c)),
                    c.clone(),
                    "lz round-trip failed, len {}",
                    c.len()
                );
                assert_eq!(
                    decompress_hybrid(&compress_hybrid(&c)),
                    c,
                    "hybrid round-trip failed, len {}",
                    c.len()
                );
                assert_eq!(
                    decompress_turbo(&compress_turbo(&c)),
                    c,
                    "turbo round-trip failed, len {}",
                    c.len()
                );
                assert_eq!(
                    decompress_seq(&compress_seq(&c)),
                    c,
                    "seq round-trip failed, len {}",
                    c.len()
                );
            }
        }

        /// The seq face on the awkward shapes: empty/tiny (no sequences, pure trail), all-literal
        /// (random), deep overlap RLE (rep0 + wildcopy doubling), rep-heavy structure, long runs
        /// (slot classes with residuals), and a >64K-sequence stream (slot blocking).
        #[test]
        fn seq_roundtrip() {
            let mut cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8],
                b"abc".to_vec(),
                vec![0u8; 200_000], // one giant overlap match (ml class + residual)
                b"abcabcab".repeat(60_000), // off<16 overlap, rep0-heavy
                (0..300_000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(), // ~no matches
                b"2026-07-10 INFO request id=1234 path=/api status=200\n".repeat(20_000), // rep cycling
            ];
            // many short matches → > 64K sequences → the slot-block boundary
            let mut s = 0xDEAD_BEEFu64;
            let mut v = Vec::with_capacity(1_500_000);
            while v.len() < 1_500_000 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                v.extend_from_slice(if s & 1 == 0 {
                    b"the quick brown "
                } else {
                    b"lazy dog jumped "
                });
                v.push((s >> 32) as u8);
            }
            cases.push(v);
            for c in &cases {
                assert_eq!(
                    &decompress_seq(&compress_seq(c)),
                    c,
                    "seq round-trip failed, len {}",
                    c.len()
                );
            }
        }

        /// The 3-byte-offset window: a candidate FARTHER than 2^24 must be rejected, not truncated.
        /// Construction: 24 MiB of xorshift bytes (no natural matches; the stride grows, so early table
        /// entries SURVIVE — few insertions per span) + the first 4 KiB repeated at the end. The repeat's
        /// anchors hit live cells at distance ~24 MiB > 2^24; without the `TURBO_MAX_OFF` reject these
        /// truncated into corruption. Locks the fix on every finder variant.
        #[test]
        fn turbo_far_window() {
            let n = 24 << 20;
            let mut d = Vec::with_capacity(n + 4096);
            let mut s = 0x9E37_79B9_7F4A_7C15u64;
            while d.len() < n {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                d.extend_from_slice(&s.to_le_bytes());
            }
            let head: Vec<u8> = d[..4096].to_vec();
            d.extend_from_slice(&head);
            let z = compress_turbo(&d);
            assert_eq!(decompress_turbo(&z), d, "turbo far-window corruption");
        }

        /// The turbo face on the awkward shapes: empty, tiny (< 8 B, no finder), deep overlap RLE, and
        /// high-entropy (stride-heavy). All must round-trip exactly — the byte coder has no entropy state.
        #[test]
        fn turbo_roundtrip() {
            let cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8],
                b"abc".to_vec(),
                vec![0u8; 100_000],
                b"abcabcab".repeat(50_000), // overlap copies (off < len)
                (0..200_000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(), // ~random
                b"the quick brown fox. ".repeat(5000),
            ];
            for c in cases {
                assert_eq!(
                    decompress_turbo(&compress_turbo(&c)),
                    c,
                    "turbo round-trip failed, len {}",
                    c.len()
                );
            }
        }
    }
}
#[cfg(feature = "compress")]
pub mod readout {
    //! The readout face — the Being arm as a BASIS of invertible transforms, fit by readout not search.
    //!
    //! `encode_i64` finds a degree-≤2 generator by `exact_deg2`: one O(n) pass that CHECKS a candidate
    //! closed form. That is not a search over programs — it is a READOUT of one coordinate. This face is
    //! that move with a basis: a handful of invertible byte transforms (the candidate coordinates), each
    //! fit by applying it ONCE and measuring the residual entropy (`Kfam` made operational — least
    //! description over the basis, `proof/Substrate/KolmogorovClosure.lean`). No parameter search: the
    //! coordinate is the transform that flattens the stream, found by reading each candidate, not by
    //! iterating one.
    //!
    //! Every transform is INVERTIBLE (a unit, mod 256 with wrapping) — the coordinate round-trips, so the
    //! `(coordinate, residual)` split is lossless: the same requirement as the hashmap's `PHI` fold
    //! (`CoordinateReadout.SeedSizing.decode_homeSlot_residual`, `PHI` a unit ⇒ the fold is a bijection).
    //! The readout IS `/`: divide out the coordinate transform; the residual is what `/` cannot invert
    //! (the null cone). Then COMPOSE — the readout finds the coordinate (Being `B`), cm codes the residual
    //! (Null `U`). `x = B/U`. When no transform flattens the stream (prose), the basis falls to identity
    //! and cm does the whole job: the residual that survives every transform is the dark fiber
    //! (`1 − coneFraction`, `SpectralFold`), the genuinely incompressible `U`.

    use super::{read_uvarint, take_slice, try_vec, DecodeError, DEFAULT_MAX_OUTPUT};

    /// A candidate coordinate — an invertible byte transform. `Stride(k)` subtracts the value `k` back
    /// (periodic/tabular structure); the rest are the difference/xor floors. All wrap mod 256 and keep
    /// their leading `seed` bytes verbatim so the inverse can walk forward.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Xform {
        Id,
        Delta1,
        Delta2,
        Xor1,
        Stride(u32),
    }

    impl Xform {
        /// The tag byte, self-delimiting with `Stride`'s `k` folded in above 4.
        fn tag(self) -> u8 {
            match self {
                Xform::Id => 0,
                Xform::Delta1 => 1,
                Xform::Delta2 => 2,
                Xform::Xor1 => 3,
                Xform::Stride(_) => 4,
            }
        }
        /// The count of leading bytes carried verbatim (the seeds the inverse starts from).
        fn seed_len(self) -> usize {
            match self {
                Xform::Id => 0,
                Xform::Delta1 | Xform::Xor1 => 1,
                Xform::Delta2 => 2,
                Xform::Stride(k) => k as usize,
            }
        }

        /// Apply the transform — `a` → residual. Leading `seed_len` bytes pass through; the rest is the
        /// difference from the transform's prediction. One pass, no allocation beyond the output.
        fn forward(self, a: &[u8]) -> Vec<u8> {
            let n = a.len();
            let s = self.seed_len().min(n);
            let mut r = Vec::with_capacity(n);
            r.extend_from_slice(&a[..s]);
            match self {
                Xform::Id => r.extend_from_slice(&a[s..]),
                Xform::Delta1 => {
                    for i in s..n {
                        r.push(a[i].wrapping_sub(a[i - 1]));
                    }
                }
                Xform::Delta2 => {
                    for i in s..n {
                        let p = a[i - 1].wrapping_mul(2).wrapping_sub(a[i - 2]);
                        r.push(a[i].wrapping_sub(p));
                    }
                }
                Xform::Xor1 => {
                    for i in s..n {
                        r.push(a[i] ^ a[i - 1]);
                    }
                }
                Xform::Stride(k) => {
                    let k = k as usize;
                    for i in s..n {
                        r.push(a[i].wrapping_sub(a[i - k]));
                    }
                }
            }
            r
        }

        /// Invert the transform — residual → `a`. The `from_coord`: walk forward, re-adding the
        /// prediction the forward pass subtracted. Lossless because each step is invertible mod 256.
        fn inverse(self, r: &[u8]) -> Vec<u8> {
            let n = r.len();
            let s = self.seed_len().min(n);
            let mut a = Vec::with_capacity(n);
            a.extend_from_slice(&r[..s]);
            match self {
                Xform::Id => a.extend_from_slice(&r[s..]),
                Xform::Delta1 => {
                    for i in s..n {
                        a.push(r[i].wrapping_add(a[i - 1]));
                    }
                }
                Xform::Delta2 => {
                    for i in s..n {
                        let p = a[i - 1].wrapping_mul(2).wrapping_sub(a[i - 2]);
                        a.push(r[i].wrapping_add(p));
                    }
                }
                Xform::Xor1 => {
                    for i in s..n {
                        a.push(r[i] ^ a[i - 1]);
                    }
                }
                Xform::Stride(k) => {
                    let k = k as usize;
                    for i in s..n {
                        a.push(r[i].wrapping_add(a[i - k]));
                    }
                }
            }
            a
        }
    }

    /// Order-0 entropy of a byte slice, in bits — the fit metric. A flatter residual (a coordinate that
    /// captured the structure) has lower entropy; the readout picks the minimum. This IS the fit: apply
    /// the candidate, read the entropy — no iteration.
    fn entropy_bits(r: &[u8]) -> f64 {
        if r.is_empty() {
            return 0.0;
        }
        let mut hist = [0u32; 256];
        for &b in r {
            hist[b as usize] += 1;
        }
        let n = r.len() as f64;
        let mut h = 0.0;
        for &c in &hist {
            if c != 0 {
                let p = f64::from(c) / n;
                h -= p * p.log2();
            }
        }
        h * n // total bits, not per-byte — so it weighs the whole stream
    }

    /// Is the residual (past its seeds) a single repeated byte? Then the stream is an EXACT generator
    /// under this transform — store the seed and collapse to O(1), no coder (the `const_from_almost_nothing`
    /// case, `KolmogorovClosure`). Returns the constant byte if so.
    fn constant_tail(r: &[u8], seed: usize) -> Option<u8> {
        let tail = r.get(seed..)?;
        match tail.first() {
            None => Some(0), // no tail: pure seed, treat as constant-0 residual
            Some(&c) => {
                if tail.iter().all(|&b| b == c) {
                    Some(c)
                } else {
                    None
                }
            }
        }
    }

    /// The basis of candidate coordinates tried on `a`. The fixed floors, plus a few strides fit by
    /// readout (each applied once); the detected best stride rides in `Stride(k)`.
    fn basis(n: usize) -> Vec<Xform> {
        let mut b = vec![Xform::Id, Xform::Delta1, Xform::Delta2, Xform::Xor1];
        // strides: powers of two up to the stream, plus small primes — periodic/tabular periods. Each is
        // one readout pass; no search over k, a fixed candidate set (the basis IS the freedom).
        for &k in &[2u32, 3, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
            if (k as usize) < n {
                b.push(Xform::Stride(k));
            }
        }
        b
    }

    const MODE_EXACT: u8 = 0; // the readout collapsed it: seed only, O(1) in N
    const MODE_FLOOR: u8 = 1; // readout + cm on the residual: B (coordinate) + U (coded)

    fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    fn get_uvarint(bytes: &[u8], pos: &mut usize) -> Result<u64, DecodeError> {
        read_uvarint(bytes, pos)
    }

    /// Compress by readout: fit every candidate coordinate (one pass each), take the flattest, and either
    /// COLLAPSE to the seed (exact generator) or code the residual with cm (`B` coordinate + `U` residual).
    /// Frame: `[stride k?][MODE][tag][k lo byte][ body ]`.
    #[must_use]
    pub fn compress(a: &[u8]) -> Vec<u8> {
        let n = a.len();
        // fit: the coordinate is the transform of least residual entropy — read each, don't search one.
        let mut best = Xform::Id;
        let mut best_bits = f64::INFINITY;
        let mut best_r = a.to_vec();
        for x in basis(n) {
            let r = x.forward(a);
            let bits = entropy_bits(&r);
            if bits < best_bits {
                best_bits = bits;
                best = x;
                best_r = r;
            }
        }
        let seed = best.seed_len().min(n);
        let mut out = Vec::new();
        let kbytes = if let Xform::Stride(k) = best { k } else { 0 };
        if let Some(c) = constant_tail(&best_r, seed) {
            // EXACT: the stream regenerates from its seeds under `best`. Store seeds + the constant.
            out.push(MODE_EXACT);
            out.push(best.tag());
            put_uvarint(&mut out, u64::from(kbytes));
            put_uvarint(&mut out, n as u64);
            out.extend_from_slice(&best_r[..seed]);
            out.push(c);
        } else {
            // FLOOR: the coordinate flattened it as far as a floor can; cm codes the residual U.
            out.push(MODE_FLOOR);
            out.push(best.tag());
            put_uvarint(&mut out, u64::from(kbytes));
            out.extend_from_slice(&crate::codec::cm::compress(
                crate::codec::DEFAULT_ORDER,
                &best_r,
            ));
        }
        out
    }

    /// Inverse of [`compress`]. Malformed input yields an empty `Vec` (never a panic);
    /// [`try_decompress`] says why.
    #[must_use]
    pub fn decompress(bytes: &[u8]) -> Vec<u8> {
        try_decompress(bytes).unwrap_or_default()
    }

    /// Checked [`decompress`]. The EXACT mode regenerates `n` bytes from a handful of seed bytes, so
    /// `n` is capped at [`DEFAULT_MAX_OUTPUT`] instead of being bounded by the input size.
    pub fn try_decompress(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let mut pos = 0usize;
        let mode = *take_slice(bytes, &mut pos, 1)?
            .first()
            .ok_or(DecodeError::Truncated)?;
        let tag = *take_slice(bytes, &mut pos, 1)?
            .first()
            .ok_or(DecodeError::Truncated)?;
        let k = u32::try_from(get_uvarint(bytes, &mut pos)?).map_err(|_| DecodeError::Malformed)?;
        let x = match tag {
            1 => Xform::Delta1,
            2 => Xform::Delta2,
            3 => Xform::Xor1,
            4 if k >= 1 => Xform::Stride(k), // a stride of 0 would read the byte being produced
            4 => return Err(DecodeError::Malformed),
            _ => Xform::Id,
        };
        if mode == MODE_EXACT {
            let n = usize::try_from(get_uvarint(bytes, &mut pos)?)
                .map_err(|_| DecodeError::TooLarge)?;
            if n > DEFAULT_MAX_OUTPUT {
                return Err(DecodeError::TooLarge);
            }
            let seed = x.seed_len().min(n);
            let mut r = try_vec(n)?;
            r.extend_from_slice(take_slice(bytes, &mut pos, seed)?);
            let c = *take_slice(bytes, &mut pos, 1)?
                .first()
                .ok_or(DecodeError::Truncated)?;
            r.resize(n, c); // the constant tail
            Ok(x.inverse(&r))
        } else {
            let r = crate::codec::cm::try_decompress(&bytes[pos..])?;
            Ok(x.inverse(&r))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn readout_roundtrip_and_collapse() {
            // generator streams collapse; genuine residual round-trips through the floor+cm path.
            let ramp: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(7)) as u8).collect(); // delta1 const
            let periodic: Vec<u8> = (0..50_000usize).map(|i| b"ABCDEFGH"[i % 8]).collect(); // stride-8 zero
            let prose = b"the meek shall inherit the earth. ".repeat(1500);
            let noise: Vec<u8> = (0..40_000u32)
                .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                .collect();
            for c in [&ramp, &periodic, &prose, &noise, &vec![], &vec![9u8; 3]] {
                assert_eq!(
                    &decompress(&compress(c)),
                    c,
                    "readout round-trip, len {}",
                    c.len()
                );
            }
            // the ramp and the periodic stream must COLLAPSE (MODE_EXACT) — the readout found the coordinate.
            assert_eq!(
                compress(&ramp)[0],
                MODE_EXACT,
                "ramp should collapse under delta1"
            );
            assert_eq!(
                compress(&periodic)[0],
                MODE_EXACT,
                "periodic should collapse under stride-8"
            );
            assert!(
                compress(&ramp).len() < 32,
                "ramp collapses to O(1): {} bytes",
                compress(&ramp).len()
            );
        }
    }
}

pub(crate) mod bits {
    //! The bit-field primitive — ONE spelling of the O(1) field-element chunk at a bit position
    //! (`DiscreteArithmetic`: an integer is a discrete field element; a k-bit read/write is one
    //! applicative, never a Peano bit loop). Three cursors over one law:
    //! [`BitW`] (ascending writer, accumulator + byte flush — also the tANS/huf encoders' inline
    //! shape), [`BitR`] (ascending reader, padded unaligned load), [`read_bits_at`] (absolute
    //! read at any bit position, the descending decoders' primitive). Buffers are padded by
    //! construction (≥ 8 B slack) so every read is one branchless load + shift.

    /// Forward chunk bit-writer: `write(v, n)` appends `v`'s low `n` bits LSB-first.
    pub(crate) struct BitW {
        pub acc: u64,
        pub cnt: u32,
        pub out: Vec<u8>,
    }
    impl BitW {
        pub fn new() -> Self {
            BitW {
                acc: 0,
                cnt: 0,
                out: Vec::new(),
            }
        }
        #[inline]
        pub fn write(&mut self, v: u32, n: u32) {
            self.acc |= (u64::from(v) & ((1u64 << n) - 1)) << self.cnt;
            self.cnt += n;
            while self.cnt >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.cnt -= 8;
            }
        }
        pub fn finish(mut self) -> Vec<u8> {
            if self.cnt > 0 {
                self.out.push(self.acc as u8);
            }
            self.out
        }
    }

    /// Forward reader — one padded unaligned u64 load + shift per read, branchless. Owns an
    /// 8-byte-padded copy of the input.
    pub(crate) struct BitR {
        buf: Vec<u8>,
        bitpos: usize,
        limit: usize, // the input's length in bits
    }
    impl BitR {
        pub fn new(inp: &[u8]) -> Self {
            let mut buf = Vec::with_capacity(inp.len() + 8);
            buf.extend_from_slice(inp);
            buf.extend_from_slice(&[0u8; 8]);
            BitR {
                buf,
                bitpos: 0,
                limit: inp.len() * 8,
            }
        }
        /// Read `n` (≤ 32) bits. Reading past the end of the input yields zeros and sticks
        /// [`Self::overrun`], so a decoder checks once at the end instead of per read.
        #[inline]
        pub fn read(&mut self, n: u32) -> u32 {
            debug_assert!(n <= 32);
            let next = self.bitpos + n as usize;
            if next > self.limit {
                self.bitpos = self.limit + 1; // sticky
                return 0;
            }
            let w = read_bits_at(&self.buf, self.bitpos);
            self.bitpos = next;
            (w & ((1u64 << n) - 1)) as u32
        }
        /// Whether any read ran past the end of the input.
        #[inline]
        pub fn overrun(&self) -> bool {
            self.bitpos > self.limit
        }
    }

    /// One unaligned u64 at BIT `bit` (buffer padded ≥ 8 B past the top) — ≥ 57 valid bits.
    /// The absolute-position read every decoder (tans4, tans4o1, huf4, the raw residuals) rides.
    #[inline(always)]
    pub(crate) fn read_bits_at(buf: &[u8], bit: usize) -> u64 {
        let byte = bit >> 3;
        u64::from_le_bytes(buf[byte..byte + 8].try_into().unwrap()) >> (bit & 7)
    }
}
pub mod huf {
    //! huf4 — the STATELESS coder corner (`PrefixCodeReadout.prefix_decode_exact`), 4-stream.
    //!
    //! The coder family's three regimes, all closed in the corpus: uniform = copy
    //! (`uniform_code_is_copy`), fractional-bit fold state = tANS (`table_is_the_fold`, the DENSITY
    //! corner — the register carries a phase, so decode serializes on the table load, ~5 cy/sym),
    //! and THIS — prefix-free integer-bit codes (the SPEED corner): no inter-symbol state, so the
    //! per-symbol decode chain is a register SHIFT (measured against zstd's huf_decompress_amd64.S,
    //! whose loop this mirrors: one unaligned load per 5-symbol group at max code length 11,
    //! 5·11 = 55 ≤ 57 guaranteed window bits — the cadence is derived from the length limit).
    //! The integer-bit rounding is the priced density loss; the FAST seq face buys speed with it.
    //!
    //! REJECTED (2026-07-11, do not retry in this layout): the X2 double-symbol table (zstd HUF
    //! 4X2; licence `PrefixCodeReadout.decodeOne_pair`) measured +13-14% decode — the pair grain
    //! pays only with UNCONDITIONAL 2-byte stores, which need per-stream slack; our four quarters
    //! are adjacent in one buffer, so the second byte's store is a data-dependent branch per
    //! lookup that eats the halved lookups. Same class as the fused-executor rejection: their
    //! trick's precondition is their workspace layout. The BRANCHLESS variant (unconditional
    //! pair store — safe: the all-streams rem ≥ 10 guard keeps both bytes in-quarter, garbage
    //! second bytes are overwritten by the next lookup or the exact tail) was ALSO gated: prose
    //! flat, code +1.3%. MEASURED mechanism (alternating protocol, insn counter — an earlier
    //! "chain-bound" reading here was asserted, not measured, and is retracted): this loop is
    //! INSTRUCTION-THROUGHPUT-bound — 14.3 insn/B at IPC 4.6 standalone (3.1 cyc/B; ~1/3 of
    //! fast-face decode) vs zstd's ~4-5 insn/sym asm; X2's pair grain adds ~4 insn/lookup
    //! (second store, variable advance, remainder bookkeeping), cancelling the halved lookups.
    //! The derived lever is INSN/SYM reduction — the bounds-check machinery around the
    //! per-symbol stores is the named surplus. X1 stays. (Also caught en route: a pair-emission
    //! group guard must check EVERY stream's remainder — variable emission diverges them; unit
    //! corpora missed it, the corpus gate panicked.)
    //!
    //! Frame: `[varint n][mode]` then `mode 0` = raw bytes, `mode 1` =
    //! `[128 B lens (4 bits each)][4× varint bitlens][4 streams]` — the contiguous 4-way split
    //! (`kway_stream_decode_exact`), canonical codes (sorted by (len, sym)), written bit-REVERSED
    //! LSB-first so the decode table indexes the raw window (`zlib`'s layout).

    use super::{read_uvarint, take_slice, try_vec, try_zeroed, DecodeError, DEFAULT_MAX_OUTPUT};

    const MAX_LEN: u32 = 11; // the length limit — 5 codes fit one 57-bit read window (5·11 = 55)
    const TBL: usize = 1 << MAX_LEN;

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            if v < 128 {
                out.push(v as u8);
                break;
            }
            out.push((v % 128 + 128) as u8);
            v /= 128;
        }
    }
    fn get_varint(b: &[u8], p: &mut usize) -> Result<u64, DecodeError> {
        read_uvarint(b, p)
    }

    /// Huffman code lengths from a histogram, length-limited to [`MAX_LEN`]. Tree by two-min
    /// pairing; overlong codes clipped and the Kraft sum repaired by lengthening the deepest
    /// still-short codes (each `len += 1` frees `2^(MAX_LEN−len−1)` slots — cheapest damage first).
    /// Kraft ≤ 1 suffices for a canonical prefix table (`PrefixCodeReadout.PrefixFree`).
    fn build_lens(hist: &[u32; 256]) -> [u8; 256] {
        let mut lens = [0u8; 256];
        let live: Vec<usize> = (0..256).filter(|&s| hist[s] > 0).collect();
        if live.len() == 1 {
            lens[live[0]] = 1;
            return lens;
        }
        // two-min pairing on a scratch forest: node = (freq, id); parents track children
        let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, usize)>> = live
            .iter()
            .map(|&s| std::cmp::Reverse((u64::from(hist[s]), s)))
            .collect();
        let mut kids: Vec<(usize, usize)> = vec![(usize::MAX, usize::MAX); 256];
        while heap.len() > 1 {
            let std::cmp::Reverse((f1, n1)) = heap.pop().unwrap();
            let std::cmp::Reverse((f2, n2)) = heap.pop().unwrap();
            kids.push((n1, n2));
            heap.push(std::cmp::Reverse((f1 + f2, kids.len() - 1)));
        }
        // depths by stack walk from the root
        let root = heap.pop().unwrap().0 .1;
        let mut stack = vec![(root, 0u32)];
        while let Some((n, d)) = stack.pop() {
            if n < 256 {
                lens[n] = d.min(MAX_LEN + 20) as u8; // clip later; keep u8-safe
            } else {
                let (a, b) = kids[n];
                stack.push((a, d + 1));
                stack.push((b, d + 1));
            }
        }
        // length-limit: clip, then repair Kraft (units of 2^-MAX_LEN; budget = TBL)
        let mut kraft: u64 = 0;
        for l in &mut lens {
            if *l > 0 {
                *l = (*l).min(MAX_LEN as u8);
                kraft += 1u64 << (MAX_LEN - u32::from(*l));
            }
        }
        while kraft > TBL as u64 {
            // lengthen the deepest code still below the limit — the smallest Kraft repair
            let s = (0..256)
                .filter(|&s| lens[s] > 0 && u32::from(lens[s]) < MAX_LEN)
                .max_by_key(|&s| lens[s])
                .expect("kraft repair needs a shortenable code");
            kraft -= 1u64 << (MAX_LEN - u32::from(lens[s]) - 1);
            lens[s] += 1;
        }
        lens
    }

    /// Canonical codes from lengths (sorted by (len, sym)), returned bit-REVERSED for LSB-first
    /// streams: `enc[s] = rev(code) | len << 12`.
    fn canonical(lens: &[u8; 256]) -> [u16; 256] {
        let mut order: Vec<usize> = (0..256).filter(|&s| lens[s] > 0).collect();
        order.sort_by_key(|&s| (lens[s], s));
        let mut enc = [0u16; 256];
        let (mut code, mut prev) = (0u32, 0u32);
        for &s in &order {
            let l = u32::from(lens[s]);
            code <<= l - prev;
            let rev = code.reverse_bits() >> (32 - l);
            enc[s] = (rev | (l << MAX_LEN)) as u16; // rev < 2^11 in bits 0..11, len in bits 11..15
            code += 1;
            prev = l;
        }
        enc
    }

    /// The decode table: index = the next 11 window bits (LSB-first); entry = `sym | len << 8`.
    fn build_dec(lens: &[u8; 256], enc: &[u16; 256], tbl: &mut [u16]) {
        for s in 0..256 {
            let l = u32::from(lens[s]);
            if l == 0 {
                continue;
            }
            let rev = u32::from(enc[s]) & (TBL as u32 - 1);
            let step = 1usize << l;
            let mut i = rev as usize;
            while i < TBL {
                tbl[i] = (l as u16) | ((s as u16) << 8); // len LOW (the raw entry is the shift count), sym HIGH — off-chain extract
                i += step;
            }
        }
    }

    use crate::codec::bits::read_bits_at;

    /// huf4 compress. Raw escape (mode 0) for tiny or incompressible inputs — the coder never expands.
    #[must_use]
    pub fn compress4h(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut out = Vec::with_capacity(n / 2 + 160);
        put_varint(&mut out, n as u64);
        if n < 64 {
            out.push(0);
            out.extend_from_slice(data);
            return out;
        }
        let mut hist = [0u32; 256];
        for &b in data {
            hist[usize::from(b)] += 1;
        }
        let lens = build_lens(&hist);
        let enc = canonical(&lens);
        // 4 contiguous quarters, each a forward LSB-first bit stream
        let q = n.div_ceil(4);
        let mut streams: [(Vec<u8>, usize); 4] = Default::default();
        for k in 0..4 {
            let part = &data[k * q..((k + 1) * q).min(n)];
            let (mut acc, mut cnt) = (0u64, 0u32);
            let mut bytes = Vec::with_capacity(part.len());
            for &b in part {
                let e = enc[usize::from(b)];
                let l = u32::from(e) >> MAX_LEN;
                acc |= u64::from(e & (TBL as u16 - 1)) << cnt;
                cnt += l;
                while cnt >= 8 {
                    bytes.push(acc as u8);
                    acc >>= 8;
                    cnt -= 8;
                }
            }
            let bits = bytes.len() * 8 + cnt as usize;
            if cnt > 0 {
                bytes.push(acc as u8);
            }
            streams[k] = (bytes, bits);
        }
        let coded: usize = streams.iter().map(|(b, _)| b.len()).sum();
        if coded + 160 >= n {
            out.push(0); // incompressible — the raw arm
            out.extend_from_slice(data);
            return out;
        }
        out.push(1);
        for i in 0..128 {
            out.push(lens[2 * i] | (lens[2 * i + 1] << 4));
        }
        for (_, bits) in &streams {
            put_varint(&mut out, *bits as u64);
        }
        for (bytes, _) in &streams {
            out.extend_from_slice(bytes);
        }
        out
    }

    /// Inverse of [`compress4h`] — the shift-chain loop: per symbol, one table read + a register
    /// shift (NO state chain, the speed corner); one unaligned load per 5-symbol group per stream,
    /// four streams interleaved for the load-port overlap.
    ///
    /// Malformed input yields an empty `Vec` (never a panic); [`try_decompress4h`] says why.
    #[must_use]
    pub fn decompress4h(bytes: &[u8]) -> Vec<u8> {
        try_decompress4h(bytes).unwrap_or_default()
    }

    /// Checked [`decompress4h`]. Every length is validated against the input before anything is
    /// allocated: a coded block has at least one bit per symbol, so `n` cannot exceed the stream bits.
    pub fn try_decompress4h(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            return unsafe { decompress4h_bmi2(bytes) };
        }
        decompress4h_impl(bytes)
    }
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn decompress4h_bmi2(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        decompress4h_impl(bytes)
    }
    #[inline(always)]
    fn decompress4h_impl(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut p = 0usize;
        let n = get_varint(bytes, &mut p)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        let n = usize::try_from(n).map_err(|_| DecodeError::TooLarge)?;
        let mode = *bytes.get(p).ok_or(DecodeError::Truncated)?;
        p += 1;
        if mode == 0 {
            return Ok(take_slice(bytes, &mut p, n)?.to_vec()); // raw escape: bounded by the input
        }
        let mut lens = [0u8; 256];
        let table = take_slice(bytes, &mut p, 128)?;
        for i in 0..128 {
            lens[2 * i] = table[i] & 15;
            lens[2 * i + 1] = table[i] >> 4;
        }
        // A prefix code: lengths within the limit and a Kraft sum that fits the table. Anything
        // else would make `canonical` hand out codes that overflow their length.
        let mut kraft = 0usize;
        for &l in &lens {
            if u32::from(l) > MAX_LEN {
                return Err(DecodeError::Malformed);
            }
            if l > 0 {
                kraft += 1usize << (MAX_LEN - u32::from(l));
            }
        }
        if kraft > TBL {
            return Err(DecodeError::Malformed);
        }
        let enc = canonical(&lens);
        let mut tbl = vec![0u16; TBL];
        build_dec(&lens, &enc, &mut tbl);
        let mut bits = [0usize; 4];
        for b in &mut bits {
            *b = usize::try_from(get_varint(bytes, &mut p)?).map_err(|_| DecodeError::TooLarge)?;
        }
        // Every symbol costs at least one bit, so the declared length is bounded by the stream bits.
        let total_bits = bits
            .iter()
            .try_fold(0usize, |a, &b| a.checked_add(b))
            .ok_or(DecodeError::TooLarge)?;
        if n > total_bits || n > DEFAULT_MAX_OUTPUT {
            return Err(DecodeError::TooLarge);
        }
        // one padded shadow of the streams region: absolute bit positions, no per-stream copies
        let streams = &bytes[p..];
        let mut buf = try_vec(streams.len() + 8)?;
        buf.extend_from_slice(streams);
        buf.extend_from_slice(&[0u8; 8]);
        let q = n.div_ceil(4);
        let mut pos = [0usize; 4]; // absolute bit cursors
        let mut lim = [0usize; 4]; // each stream's end bit
        let mut base = 0usize;
        for k in 0..4 {
            pos[k] = base;
            lim[k] = base
                .checked_add(bits[k])
                .filter(|&l| l <= streams.len() * 8)
                .ok_or(DecodeError::Truncated)?;
            base = base
                .checked_add(bits[k].div_ceil(8) * 8)
                .ok_or(DecodeError::Truncated)?;
        }
        if 3 * q > n {
            return Err(DecodeError::Malformed);
        }
        let mut out = try_zeroed(n)?;
        // THE CONSTRUCTED LOOP — huf_decompress_amd64.S translated, not approximated. Their body:
        // 4 insn/symbol, ALL state in registers, ONE bound check per 20 symbols with the trip
        // count known up front. Our previous loop disassembled to 10 insn/symbol: per-symbol
        // output bounds checks, state in stack ARRAYS (pos[k]/outp[k]/rem[k] traffic), and rustc
        // spilling the entry through the stack per symbol — a ~5 cy store-forward ON the shift
        // chain (this was the measured 21% be-bound and why deleting only the checks moved
        // nothing: the stack chain remained). The construction: X1 emits EXACTLY 5 per group, so
        // the trip count is deterministic — `groups = min-stream-count / 5`, proven once below;
        // the interior runs unchecked on scalar state; the tails stay checked and exact.
        let c3 = n - 3 * q;
        let groups = c3 / 5; // stream 3 is the shortest; all others cover ≥ groups·5
                             // the ONE proof the interior rides: every store lands in its own quarter, every read
                             // stays in the padded buffer
        assert!(
            3 * q + groups * 5 <= n && q >= groups * 5,
            "trip-count bound"
        );
        for k in 0..4 {
            assert!(
                pos[k] / 8 + 8 <= buf.len() && bits[k] <= buf.len() * 8,
                "bit-cursor bound"
            );
        }
        // A group consumes at most 5·MAX_LEN = 55 bits from its cursor. The interior runs only while
        // all four cursors still have that much room before their stream's end bit (`lim`), so the
        // unchecked 8-byte loads stay inside `buf` even for a corrupt stream whose symbols are longer
        // than the encoder's; the checked tail below finishes (or rejects) the rest.
        const GROUP_BITS: usize = 5 * MAX_LEN as usize;
        let (mut p0, mut p1, mut p2, mut p3) = (pos[0], pos[1], pos[2], pos[3]);
        let (mut o0, mut o1, mut o2, mut o3) = (0usize, q, 2 * q, 3 * q);
        let ob = out.as_mut_ptr();
        let tp = tbl.as_ptr();
        let bp = buf.as_ptr();
        macro_rules! g5 {
            ($p:ident, $o:ident) => {{
                let mut w = unsafe { bp.add($p >> 3).cast::<u64>().read_unaligned() } >> ($p & 7);
                let mut used = 0usize;
                let mut e = unsafe { *tp.add((w & (TBL as u64 - 1)) as usize) };
                unsafe {
                    *ob.add($o) = (e >> 8) as u8;
                    w >>= e & 63;
                    used += (e & 63) as usize;
                    e = *tp.add((w & (TBL as u64 - 1)) as usize);
                    *ob.add($o + 1) = (e >> 8) as u8;
                    w >>= e & 63;
                    used += (e & 63) as usize;
                    e = *tp.add((w & (TBL as u64 - 1)) as usize);
                    *ob.add($o + 2) = (e >> 8) as u8;
                    w >>= e & 63;
                    used += (e & 63) as usize;
                    e = *tp.add((w & (TBL as u64 - 1)) as usize);
                    *ob.add($o + 3) = (e >> 8) as u8;
                    w >>= e & 63;
                    used += (e & 63) as usize;
                    e = *tp.add((w & (TBL as u64 - 1)) as usize);
                    *ob.add($o + 4) = (e >> 8) as u8;
                    used += (e & 63) as usize;
                }
                $o += 5;
                $p += used;
            }};
        }
        // group-sequential per stream: the EXPLICIT symbol-level interleave (their asm's
        // FOR_EACH_STREAM_WITH_INDEX) was built and measured +2.5% WORSE here — the OoO window
        // already overlaps the four group chains, and eight extra live values re-spill; that
        // last inch is hand register allocation, which is exactly why zstd ships the loop as .S.
        let mut done = 0usize; // groups actually run
        while done < groups
            && p0 + GROUP_BITS <= lim[0]
            && p1 + GROUP_BITS <= lim[1]
            && p2 + GROUP_BITS <= lim[2]
            && p3 + GROUP_BITS <= lim[3]
        {
            g5!(p0, o0);
            g5!(p1, o1);
            g5!(p2, o2);
            g5!(p3, o3);
            done += 1;
        }
        // exact checked tails (each stream's remaining count is deterministic)
        let mut pos = [p0, p1, p2, p3];
        let mut outp = [o0, o1, o2, o3];
        let rem = [q - done * 5, q - done * 5, q - done * 5, c3 - done * 5];
        for k in 0..4 {
            for _ in 0..rem[k] {
                if pos[k] >= lim[k] {
                    return Err(DecodeError::Truncated); // the stream ended before its symbols did
                }
                let w = read_bits_at(&buf, pos[k]);
                let e = tbl[(w & (TBL as u64 - 1)) as usize];
                out[outp[k]] = (e >> 8) as u8;
                outp[k] += 1;
                pos[k] += usize::from(e & 63);
            }
            if pos[k] > lim[k] {
                return Err(DecodeError::Truncated);
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn huf4_roundtrip() {
            let mut cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8; 1],
                vec![9u8; 63], // raw escape boundary
                vec![9u8; 64], // single-symbol coded (1 bit/sym)
                vec![0u8; 5000],
                b"the quick brown fox jumps over the lazy dog. ".repeat(300),
                (0..40000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(), // ~uniform → raw arm
                (0..=255u8).cycle().take(9001).collect(),
                (0..=255u8).cycle().take(9002).collect(),
                (0..=255u8).cycle().take(9003).collect(),
            ];
            // a deep skew (forces the length limit + Kraft repair)
            let mut s = 0x1234_5678u32;
            cases.push(
                (0..80000)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        // geometric-ish: mostly 'e', a heavy tail of rare bytes
                        match s % 1000 {
                            0..=699 => b'e',
                            700..=899 => b' ',
                            v => (v % 256) as u8,
                        }
                    })
                    .collect(),
            );
            for c in &cases {
                assert_eq!(
                    &decompress4h(&compress4h(c)),
                    c,
                    "huf4 round-trip, len {}",
                    c.len()
                );
            }
        }
    }
}
#[cfg(feature = "codec")]
pub mod msgpack {
    //! MessagePack — the applicative α=msgpack over the SAME `SyntaxTerm` every other surface
    //! (JSON/BAML/TOON/POML/proto) already uses. Unlike `proto`, msgpack is self-describing (every
    //! value carries its own type tag on the wire), so `decode` needs no schema — the schema-guided
    //! step lives one layer up, in whatever assigns meaning to a decoded `Bu`/`Fourth` shape (e.g.
    //! `a sibling Terraform-plugin client`'s cty attribute types).
    //!
    //! This is the wire format real Terraform/OpenTofu core and providers actually exchange resource
    //! values in (`tfplugin6.DynamicValue.msgpack`) — confirmed by reading
    //! `terraform-plugin-go`'s own `tftypes/value_msgpack.go` live this session (`NewDynamicValue`
    //! calls `Value.MarshalMsgPack`, not JSON). One extra state beyond the other surfaces:
    //! `SyntaxTerm::Unknown` ("known to exist, not yet known what it is" — a Computed attribute
    //! during planning) is encoded as msgpack's ext type (`0xd4 0x00 0x00`, Terraform's own choice —
    //! msgpack itself has no such primitive), and ANY ext byte on decode is read back as `Unknown`,
    //! matching go-cty's own conservative rule ("assume all extensions are unknown values").
    //!
    //! Numbers keep `Atom::Num`'s existing decimal-string representation losslessly: encode picks
    //! the narrowest exact form (int64, else float64, else the decimal string itself — matching
    //! `marshalMsgPackNumber`'s own fallback order), decode renders whatever numeric wire type it
    //! finds back to that same decimal string. Object-shaped `Bu` values are written with keys
    //! sorted (matching `marshalMsgPackObject`'s convention for determinism); decode preserves
    //! whatever order the wire has, which msgpack maps make legal either way.

    use super::syntax::{Atom, SyntaxError, SyntaxTerm};

    fn put_u8(out: &mut Vec<u8>, code: u8, b: u8) {
        out.push(code);
        out.push(b);
    }

    fn put_be16(out: &mut Vec<u8>, code: u8, v: u16) {
        out.push(code);
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn put_be32(out: &mut Vec<u8>, code: u8, v: u32) {
        out.push(code);
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn put_be64(out: &mut Vec<u8>, code: u8, v: u64) {
        out.push(code);
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn encode_len(
        out: &mut Vec<u8>,
        len: usize,
        fix_base: u8,
        fix_max: usize,
        code8: Option<u8>,
        code16: u8,
        code32: u8,
    ) {
        if len <= fix_max {
            out.push(fix_base | len as u8);
        } else if let (Some(c8), true) = (code8, u8::try_from(len).is_ok()) {
            put_u8(out, c8, len as u8);
        } else if u16::try_from(len).is_ok() {
            put_be16(out, code16, len as u16);
        } else {
            put_be32(out, code32, len as u32);
        }
    }

    fn encode_str(out: &mut Vec<u8>, s: &str) {
        let bytes = s.as_bytes();
        encode_len(out, bytes.len(), 0xa0, 0x1f, Some(0xd9), 0xda, 0xdb);
        out.extend_from_slice(bytes);
    }

    fn encode_int(out: &mut Vec<u8>, v: i64) {
        if (0..=127).contains(&v) {
            out.push(v as u8);
        } else if (-32..0).contains(&v) {
            out.push((v as i8) as u8);
        } else if let Ok(v8) = i8::try_from(v) {
            out.push(0xd0);
            out.push(v8 as u8);
        } else if let Ok(v16) = i16::try_from(v) {
            put_be16(out, 0xd1, v16 as u16);
        } else if let Ok(v32) = i32::try_from(v) {
            put_be32(out, 0xd2, v32 as u32);
        } else {
            put_be64(out, 0xd3, v as u64);
        }
    }

    /// `Atom::Num`'s decimal string → the narrowest exact msgpack wire form, matching
    /// `marshalMsgPackNumber`'s own fallback order: exact int64, else exact float64, else the
    /// decimal string itself (arbitrary-precision numbers cty represents but msgpack's native
    /// numeric types cannot).
    fn encode_num(out: &mut Vec<u8>, s: &str) {
        if let Ok(i) = s.parse::<i64>() {
            encode_int(out, i);
            return;
        }
        if let Ok(f) = s.parse::<f64>() {
            if f.to_string() == s || format!("{f}") == s {
                put_be64(out, 0xcb, f.to_bits());
                return;
            }
        }
        encode_str(out, s);
    }

    fn encode_into(term: &SyntaxTerm, out: &mut Vec<u8>) {
        match term {
            SyntaxTerm::Nb => out.push(0xc0),
            SyntaxTerm::Unknown => out.extend_from_slice(&[0xd4, 0x00, 0x00]),
            SyntaxTerm::B(Atom::Bool(b)) => out.push(if *b { 0xc3 } else { 0xc2 }),
            SyntaxTerm::B(Atom::Num(s)) => encode_num(out, s),
            SyntaxTerm::B(Atom::Str(s)) => encode_str(out, s),
            SyntaxTerm::Fourth(items) => {
                encode_len(out, items.len(), 0x90, 0x0f, None, 0xdc, 0xdd);
                for item in items {
                    encode_into(item, out);
                }
            }
            SyntaxTerm::Bu(fields) => {
                let mut sorted: Vec<&(String, SyntaxTerm)> = fields.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                encode_len(out, sorted.len(), 0x80, 0x0f, None, 0xde, 0xdf);
                for (k, v) in sorted {
                    encode_str(out, k);
                    encode_into(v, out);
                }
            }
        }
    }

    /// `SyntaxTerm` → msgpack bytes — the wire form real Terraform core/providers exchange as
    /// `DynamicValue.msgpack`.
    #[must_use]
    pub fn encode(term: &SyntaxTerm) -> Vec<u8> {
        let mut out = Vec::new();
        encode_into(term, &mut out);
        out
    }

    fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], SyntaxError> {
        let slice = pos
            .checked_add(n)
            .and_then(|end| bytes.get(*pos..end))
            .ok_or_else(|| SyntaxError::new("truncated msgpack"))?;
        *pos += n;
        Ok(slice)
    }

    fn take_byte(bytes: &[u8], pos: &mut usize) -> Result<u8, SyntaxError> {
        let b = *bytes
            .get(*pos)
            .ok_or_else(|| SyntaxError::new("truncated msgpack"))?;
        *pos += 1;
        Ok(b)
    }

    #[allow(clippy::too_many_arguments)] // an internal wire-decode helper; the fields are the format
    fn decode_len(
        bytes: &[u8],
        pos: &mut usize,
        code: u8,
        fix_base: u8,
        fix_mask: u8,
        code8: Option<u8>,
        code16: u8,
        code32: u8,
    ) -> Result<usize, SyntaxError> {
        if code & !fix_mask == fix_base {
            return Ok((code & fix_mask) as usize);
        }
        if Some(code) == code8 {
            return Ok(take_byte(bytes, pos)? as usize);
        }
        if code == code16 {
            return Ok(u16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as usize);
        }
        if code == code32 {
            return Ok(u32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as usize);
        }
        Err(SyntaxError::new(format!(
            "bad msgpack length code {code:#x}"
        )))
    }

    fn decode_str(bytes: &[u8], pos: &mut usize, code: u8) -> Result<String, SyntaxError> {
        let len = decode_len(bytes, pos, code, 0xa0, 0x1f, Some(0xd9), 0xda, 0xdb)?;
        let raw = take(bytes, pos, len)?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// msgpack bytes → `SyntaxTerm`, self-describing (no schema needed — every msgpack value carries
    /// its own type tag). Any ext-type byte decodes to `Unknown`, matching go-cty's own rule of
    /// treating every extension as an unknown value, not just Terraform's specific `0xd4 00 00`.
    pub fn decode(bytes: &[u8]) -> Result<SyntaxTerm, SyntaxError> {
        let mut pos = 0usize;
        let term = decode_at(bytes, &mut pos, 0)?;
        Ok(term)
    }

    /// Deepest array/map nesting `decode` follows: it recurses once per level, so an untrusted
    /// buffer of nested openers (one byte each) must not be allowed to exhaust the stack.
    const MAX_DEPTH: usize = 128;

    /// A container length from the wire, checked against the bytes that remain: every element
    /// takes at least one byte, so a larger claim is a lie — rejected before any reservation.
    fn check_count(bytes: &[u8], pos: usize, len: usize) -> Result<usize, SyntaxError> {
        if len > bytes.len().saturating_sub(pos) {
            return Err(SyntaxError::new("truncated msgpack"));
        }
        Ok(len)
    }

    fn decode_at(bytes: &[u8], pos: &mut usize, depth: usize) -> Result<SyntaxTerm, SyntaxError> {
        if depth > MAX_DEPTH {
            return Err(SyntaxError::new("msgpack nested too deeply"));
        }
        let code = take_byte(bytes, pos)?;
        match code {
            0xc0 => Ok(SyntaxTerm::Nb),
            0xc2 => Ok(SyntaxTerm::B(Atom::Bool(false))),
            0xc3 => Ok(SyntaxTerm::B(Atom::Bool(true))),
            // fixint (positive 0x00..=0x7f, negative 0xe0..=0xff)
            0x00..=0x7f => Ok(SyntaxTerm::B(Atom::Num((code as i64).to_string()))),
            0xe0..=0xff => Ok(SyntaxTerm::B(Atom::Num((code as i8 as i64).to_string()))),
            0xd0 => Ok(SyntaxTerm::B(Atom::Num(
                (take_byte(bytes, pos)? as i8 as i64).to_string(),
            ))),
            0xd1 => Ok(SyntaxTerm::B(Atom::Num(
                (i16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as i64).to_string(),
            ))),
            0xd2 => Ok(SyntaxTerm::B(Atom::Num(
                (i32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as i64).to_string(),
            ))),
            0xd3 => Ok(SyntaxTerm::B(Atom::Num(
                i64::from_be_bytes(take(bytes, pos, 8)?.try_into().unwrap()).to_string(),
            ))),
            0xcc => Ok(SyntaxTerm::B(Atom::Num(
                (take_byte(bytes, pos)? as i64).to_string(),
            ))),
            0xcd => Ok(SyntaxTerm::B(Atom::Num(
                (u16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as i64).to_string(),
            ))),
            0xce => Ok(SyntaxTerm::B(Atom::Num(
                (u32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as i64).to_string(),
            ))),
            0xcf => Ok(SyntaxTerm::B(Atom::Num(
                u64::from_be_bytes(take(bytes, pos, 8)?.try_into().unwrap()).to_string(),
            ))),
            0xca => {
                let f = f32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap());
                Ok(SyntaxTerm::B(Atom::Num(f.to_string())))
            }
            0xcb => {
                let f = f64::from_be_bytes(take(bytes, pos, 8)?.try_into().unwrap());
                Ok(SyntaxTerm::B(Atom::Num(f.to_string())))
            }
            // fixstr / str8 / str16 / str32
            0xa0..=0xbf | 0xd9 | 0xda | 0xdb => {
                Ok(SyntaxTerm::B(Atom::Str(decode_str(bytes, pos, code)?)))
            }
            // bin8/16/32 — treated as a string (raw bytes; cty attribute values that are `bytes`,
            // e.g. Schema.Attribute.type's own cty-type spec, are read as UTF-8 by their consumer)
            0xc4 => {
                let len = take_byte(bytes, pos)? as usize;
                Ok(SyntaxTerm::B(Atom::Str(
                    String::from_utf8_lossy(take(bytes, pos, len)?).into_owned(),
                )))
            }
            0xc5 => {
                let len = u16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as usize;
                Ok(SyntaxTerm::B(Atom::Str(
                    String::from_utf8_lossy(take(bytes, pos, len)?).into_owned(),
                )))
            }
            0xc6 => {
                let len = u32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as usize;
                Ok(SyntaxTerm::B(Atom::Str(
                    String::from_utf8_lossy(take(bytes, pos, len)?).into_owned(),
                )))
            }
            // fixarray / array16 / array32
            0x90..=0x9f | 0xdc | 0xdd => {
                let len = decode_len(bytes, pos, code, 0x90, 0x0f, None, 0xdc, 0xdd)?;
                let len = check_count(bytes, *pos, len)?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(decode_at(bytes, pos, depth + 1)?);
                }
                Ok(SyntaxTerm::Fourth(items))
            }
            // fixmap / map16 / map32
            0x80..=0x8f | 0xde | 0xdf => {
                let len = decode_len(bytes, pos, code, 0x80, 0x0f, None, 0xde, 0xdf)?;
                let len = check_count(bytes, *pos, len)?;
                let mut fields = Vec::with_capacity(len);
                for _ in 0..len {
                    let key_code = take_byte(bytes, pos)?;
                    let key = decode_str(bytes, pos, key_code)?;
                    fields.push((key, decode_at(bytes, pos, depth + 1)?));
                }
                Ok(SyntaxTerm::Bu(fields))
            }
            // fixext1..fixext16, ext8/16/32 — go-cty's own rule: every extension is Unknown.
            0xd4..=0xd8 => {
                let n = 1usize << (code - 0xd4);
                take(bytes, pos, 1 + n)?; // 1 type byte + n data bytes
                Ok(SyntaxTerm::Unknown)
            }
            0xc7 => {
                let len = take_byte(bytes, pos)? as usize;
                take(bytes, pos, 1 + len)?;
                Ok(SyntaxTerm::Unknown)
            }
            0xc8 => {
                let len = u16::from_be_bytes(take(bytes, pos, 2)?.try_into().unwrap()) as usize;
                take(bytes, pos, 1 + len)?;
                Ok(SyntaxTerm::Unknown)
            }
            0xc9 => {
                let len = u32::from_be_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as usize;
                take(bytes, pos, 1 + len)?;
                Ok(SyntaxTerm::Unknown)
            }
            other => Err(SyntaxError::new(format!(
                "unsupported msgpack code {other:#x}"
            ))),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn from_hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }

        fn hex_of(bytes: &[u8]) -> String {
            use std::fmt::Write as _;
            bytes.iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
        }

        #[test]
        fn null_and_unknown_encode_to_the_real_go_cty_bytes() {
            assert_eq!(hex_of(&encode(&SyntaxTerm::Nb)), "c0");
            // Terraform's own msgPackUnknownVal.MarshalMsgpack() — read live from
            // terraform-plugin-go's tftypes/value_msgpack.go this session.
            assert_eq!(hex_of(&encode(&SyntaxTerm::Unknown)), "d40000");
        }

        #[test]
        fn unknown_round_trips() {
            let bytes = encode(&SyntaxTerm::Unknown);
            assert_eq!(decode(&bytes).unwrap(), SyntaxTerm::Unknown);
        }

        #[test]
        fn small_int_uses_fixint_matching_real_msgpack() {
            // vmihailenco/msgpack (the library terraform-plugin-go uses) encodes small
            // non-negative ints as a single positive-fixint byte, same as any conformant
            // msgpack encoder — the MessagePack spec's own required minimal form.
            let bytes = encode(&SyntaxTerm::B(Atom::Num("5".to_string())));
            assert_eq!(hex_of(&bytes), "05");
            assert_eq!(
                decode(&bytes).unwrap(),
                SyntaxTerm::B(Atom::Num("5".to_string()))
            );
        }

        #[test]
        fn string_round_trips_through_real_fixstr_encoding() {
            // "bar" is 3 bytes -> fixstr (0xa0 | 3 = 0xa3) + the 3 bytes, per spec.
            let bytes = encode(&SyntaxTerm::B(Atom::Str("bar".to_string())));
            assert_eq!(hex_of(&bytes), "a3626172");
            assert_eq!(
                decode(&bytes).unwrap(),
                SyntaxTerm::B(Atom::Str("bar".to_string()))
            );
        }

        #[test]
        fn object_shaped_map_round_trips_sorted() {
            // {"foo": "bar"} — a real null_resource.triggers value, matching what a real
            // provider sends/expects on the wire (fixmap len=1, fixstr "foo", fixstr "bar").
            let term = SyntaxTerm::Bu(vec![(
                "foo".to_string(),
                SyntaxTerm::B(Atom::Str("bar".to_string())),
            )]);
            let bytes = encode(&term);
            assert_eq!(hex_of(&bytes), "81a3666f6fa3626172");
            assert_eq!(decode(&bytes).unwrap(), term);
        }

        #[test]
        fn nested_resource_value_round_trips() {
            // Shaped like null_resource's own state: {id: unknown, triggers: {foo: "bar"}}. The exact
            // bytes below came from a real Go program run live this session, using
            // `github.com/vmihailenco/msgpack/v5` — the SAME library terraform-plugin-go uses — via
            // the identical EncodeMapLen/EncodeString/Encode(msgPackUnknownType{}) call sequence
            // `marshalMsgPackObject`/`marshalMsgPack` themselves make (keys sorted: "id" < "triggers").
            const REAL_GO_MSGPACK_HEX: &str = "82a26964d40000a8747269676765727381a3666f6fa3626172";
            let term = SyntaxTerm::Bu(vec![
                ("id".to_string(), SyntaxTerm::Unknown),
                (
                    "triggers".to_string(),
                    SyntaxTerm::Bu(vec![(
                        "foo".to_string(),
                        SyntaxTerm::B(Atom::Str("bar".to_string())),
                    )]),
                ),
            ]);
            let bytes = encode(&term);
            assert_eq!(
                hex_of(&bytes),
                REAL_GO_MSGPACK_HEX,
                "must match the real vmihailenco/msgpack output exactly"
            );

            let decoded = decode(&from_hex(REAL_GO_MSGPACK_HEX)).unwrap();
            assert_eq!(
                decoded, term,
                "must reconstruct the identical value from the real Go bytes"
            );
        }

        #[test]
        fn list_round_trips() {
            let term = SyntaxTerm::Fourth(vec![
                SyntaxTerm::B(Atom::Str("a".to_string())),
                SyntaxTerm::B(Atom::Str("b".to_string())),
            ]);
            let bytes = encode(&term);
            assert_eq!(hex_of(&bytes), "92a161a162");
            assert_eq!(decode(&bytes).unwrap(), term);
        }

        #[test]
        fn large_string_uses_str8_past_fixstr_range() {
            let s = "x".repeat(32); // 32 > fixstr's 31-byte max -> str8
            let bytes = encode(&SyntaxTerm::B(Atom::Str(s.clone())));
            assert_eq!(bytes[0], 0xd9);
            assert_eq!(bytes[1], 32);
            assert_eq!(decode(&bytes).unwrap(), SyntaxTerm::B(Atom::Str(s)));
        }

        #[test]
        fn any_extension_type_decodes_as_unknown_matching_go_ctys_conservative_rule() {
            // go-cty's msgpackUnmarshal: "as with go-cty, assume all extensions are unknown
            // values" — not just Terraform's specific ext-type-0 marker. A fixext2 (type=5,
            // 2 data bytes) must ALSO decode to Unknown, not error.
            let bytes = from_hex("d505aabb");
            assert_eq!(decode(&bytes).unwrap(), SyntaxTerm::Unknown);
        }

        #[test]
        fn negative_number_round_trips() {
            // Real vmihailenco/msgpack output for `enc(-42)`, captured live this session.
            let term = SyntaxTerm::B(Atom::Num("-42".to_string()));
            let bytes = encode(&term);
            assert_eq!(
                hex_of(&bytes),
                "d0d6",
                "must match the real vmihailenco/msgpack int8 encoding"
            );
            assert_eq!(decode(&bytes).unwrap(), term);
        }

        #[test]
        fn float_round_trips() {
            // Real vmihailenco/msgpack output for `enc(3.5)`, captured live this session.
            let term = SyntaxTerm::B(Atom::Num("3.5".to_string()));
            let bytes = encode(&term);
            assert_eq!(
                hex_of(&bytes),
                "cb400c000000000000",
                "must match the real vmihailenco/msgpack float64 encoding"
            );
            assert_eq!(decode(&bytes).unwrap(), term);
        }
    }
}
#[cfg(feature = "codec")]
pub mod proto {
    //! Protocol Buffers wire format — the applicative α=proto2-wire over the SAME `SyntaxTerm` every
    //! other surface (JSON/BAML/TOON/POML) already uses, not a new value type. Unlike those, protobuf's
    //! wire bytes aren't self-describing (a field's number carries no type), so encode/decode need a
    //! schema alongside the term — parsed straight from the real `.proto` text (`ProtoSchema::parse`),
    //! never hand-authored. Nested/embedded message fields (extremely common in real schemas — a
    //! `SecurityContext` embeds a `Capabilities`, etc.) are the same recursion: the field's wire bytes
    //! ARE another message's `encode`/`decode`, resolved by type name against a `ProtoRegistry`.
    //!
    //! Zero-dep, on purpose: protobuf's wire format is a plain binary framing scheme (a tag varint,
    //! then a value framed by its wire type) — the same class of problem as HTTP/1.1 chunked-transfer
    //! framing, not a cryptographic protocol. There is nothing here that needs a vetted external crate
    //! the way TLS does; the varint codec below is the exact LEB128 `write_uvarint`/`read_uvarint` this
    //! module's parent already carries for `Seed`'s own word format — reused, not reinvented.
    //!
    //! Verified byte-for-byte against real production output: `k8s.io/api`'s
    //! `AWSElasticBlockStoreVolumeSource{VolumeID:"vol-123",FSType:"ext4",Partition:1,ReadOnly:true}`,
    //! marshaled by the real (gogo-proto generated) Go `Marshal()`, is
    //! `0a07766f6c2d31323312046578743418012001` — encoding the same value through this module produces
    //! the identical bytes, both directions (see `tests::matches_real_k8s_wire_bytes`), and the same
    //! holds one level of nesting deeper for `SecurityContext{Capabilities{...}}`
    //! (`tests::matches_real_k8s_wire_bytes_nested_message`).

    use super::syntax::{Atom, SyntaxTerm};
    use super::{read_uvarint, take_slice, write_uvarint};

    /// A field's Go/proto kind — fixes the wire type (varint vs length-delimited) and how to read/write
    /// the value. `Message` names another schema, resolved at encode/decode time via a `ProtoRegistry`
    /// (a message type can't carry its own nested schema inline — real `.proto` files are mutually
    /// recursive, e.g. `PodSpec` ↔ `Container` ↔ `Volume` ↔ …). `Map(key, value)` is real protobuf's
    /// `map<K, V>` sugar — on the wire it's `repeated` an implicit two-field `{key=1, value=2}` entry
    /// message (see `encode_map_field`/`decode_map_entry`), never a distinct wire type of its own.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ProtoKind {
        Int32,
        Int64,
        Bool,
        String,
        Bytes,
        Message(String),
        Map(Box<ProtoKind>, Box<ProtoKind>),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ProtoField {
        pub number: u32,
        pub name: String,
        pub kind: ProtoKind,
        pub repeated: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct ProtoSchema {
        pub fields: Vec<ProtoField>,
    }

    /// Message-name → schema, for resolving `ProtoKind::Message` fields. A flat list, not a
    /// `HashMap` — a schema registry is small and built once per program, not a hot-path lookup table
    /// (that's what apeiron's `Table` is for).
    #[derive(Clone, Debug, Default)]
    pub struct ProtoRegistry {
        schemas: Vec<(String, ProtoSchema)>,
    }

    impl ProtoRegistry {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        pub fn add(&mut self, name: impl Into<String>, schema: ProtoSchema) -> &mut Self {
            self.schemas.push((name.into(), schema));
            self
        }

        #[must_use]
        pub fn get(&self, name: &str) -> Option<&ProtoSchema> {
            self.schemas.iter().find(|(n, _)| n == name).map(|(_, s)| s)
        }

        /// Parse and register every named message from real `.proto` source in one pass — the usual
        /// entry point when a message embeds others (`Capabilities`, `SELinuxOptions`, … all live in
        /// the SAME `generated.proto`).
        #[must_use]
        pub fn parse_messages(proto_src: &str, message_names: &[&str]) -> Self {
            let mut reg = Self::new();
            for name in message_names {
                if let Some(schema) = ProtoSchema::parse(proto_src, name) {
                    reg.add(*name, schema);
                }
            }
            reg
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ProtoError {
        pub msg: String,
    }

    impl ProtoError {
        fn new(msg: impl Into<String>) -> Self {
            Self { msg: msg.into() }
        }
    }

    impl ProtoSchema {
        fn field_by_number(&self, n: u32) -> Option<&ProtoField> {
            self.fields.iter().find(|f| f.number == n)
        }

        #[must_use]
        pub fn field_by_name(&self, name: &str) -> Option<&ProtoField> {
            self.fields.iter().find(|f| f.name == name)
        }

        /// Find ONE `message Name { … }` block's inner text (braces excluded) within `text`, brace-depth
        /// matched so a nested `message`/`enum` inside doesn't end the search early.
        fn find_message_block<'a>(text: &'a str, name: &str) -> Option<&'a str> {
            let start_marker = format!("message {name} {{");
            let start = text.find(&start_marker)? + start_marker.len();
            let body = &text[start..];
            let mut depth = 1i32;
            let mut end = body.len();
            for (i, c) in body.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Some(&body[..end])
        }

        /// Parse ONE `message Name { … }` block out of real `.proto` source text — the field list only
        /// (`optional`/`required`/`repeated` `type` `name` `= number;`), everything else (package,
        /// imports, options, comments, nested messages) skipped, not misparsed: this schema is exactly
        /// what the wire codec needs, nothing more. A `type` that isn't a recognised scalar keyword and
        /// doesn't name a nested `enum` declared in this SAME block is a `Message(type)` field,
        /// resolved later against a `ProtoRegistry`. A nested enum (extremely common in real schemas —
        /// `CheckPermissionResponse`'s own `Permissionship`, e.g.) is real protobuf's `Int32`: it's a
        /// plain varint on the wire, NOT a length-delimited embedded message — an earlier version
        /// treated every non-scalar type name as `Message`, so an enum field silently vanished on
        /// encode (`registry.get("Permissionship")` found nothing, and `encode_one`'s `Message` arm
        /// returns early on a missing schema) rather than actually being wrong output.
        ///
        /// `message_name` may be dot-qualified (`"GetProviderSchema.Response"`) to reach a NESTED
        /// message unambiguously — real schemas reuse plain names like `Response`/`Request` across many
        /// unrelated outer messages (tfplugin6.proto alone has 16 distinct `message Response { … }`
        /// blocks), so an unqualified lookup for one of those would silently grab whichever happens to
        /// appear FIRST in the file rather than the one actually meant. Each qualifying segment narrows
        /// the search text to that outer message's own block before searching for the next.
        #[must_use]
        pub fn parse(proto_src: &str, message_name: &str) -> Option<Self> {
            let mut scope = proto_src;
            let mut block = "";
            for seg in message_name.split('.') {
                block = Self::find_message_block(scope, seg)?;
                scope = block;
            }
            // An enum is real protobuf's `Int32` on the wire — a plain varint, never a length-delimited
            // embedded message — REGARDLESS of whether it's declared inside this message (`Schema`'s own
            // `NestedBlock.NestingMode`) or at file scope (`StringKind`, referenced by `Schema.Block`/
            // `Schema.Attribute` but declared outside `Schema` entirely): both are checked, since a type
            // name is either declared as a message OR an enum somewhere in the file, never guessable
            // from local context alone. Checking `proto_src` covers file-scope; checking `block` covers
            // a sibling nested enum in the SAME message that another field references by its bare name.
            let is_enum = |name: &str| {
                let marker = format!("enum {name} {{");
                block.contains(&marker) || proto_src.contains(&marker)
            };
            let resolve_scalar_or_message = |ty: &str| -> ProtoKind {
                match ty {
                    "int32" | "sint32" | "uint32" | "sfixed32" | "fixed32" | "float" => {
                        ProtoKind::Int32
                    }
                    "int64" | "sint64" | "uint64" | "sfixed64" | "fixed64" | "double" => {
                        ProtoKind::Int64
                    }
                    "bool" => ProtoKind::Bool,
                    "bytes" => ProtoKind::Bytes,
                    "string" => ProtoKind::String,
                    other if is_enum(other) => ProtoKind::Int32,
                    other => ProtoKind::Message(other.to_string()),
                }
            };

            let mut fields = Vec::new();
            // A message's own field list must skip lines belonging to NESTED `message`/`enum` blocks
            // (`Schema` wraps `Block`/`Attribute`/`NestedBlock`; each has its own field numbering) —
            // without this, a nested message's field lines leak into the outer one's list, and a
            // number COLLISION (`Schema.block = 2` vs the nested `Block.attributes = 2`) makes
            // `field_by_number` return whichever happens to appear first in the raw text, not the
            // outer message's real field. `nested_depth` skips every line strictly inside such a
            // block (an enum's `NAME = number;` constants read exactly like a field declaration
            // syntactically, so enums must be skipped too, not just messages).
            let mut nested_depth: i32 = 0;
            for raw_line in block.lines() {
                let line = raw_line.split("//").next().unwrap_or(raw_line).trim();
                if nested_depth > 0 {
                    nested_depth +=
                        line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    continue;
                }
                if line.starts_with("message ") || line.starts_with("enum ") {
                    nested_depth +=
                        line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    continue;
                }
                if line.is_empty() || !line.ends_with(';') {
                    continue;
                }
                let line = line.trim_end_matches(';').trim();
                let Some((decl, number_str)) = line.rsplit_once('=') else {
                    continue;
                };
                let Ok(number) = number_str.trim().parse::<u32>() else {
                    continue;
                };
                let decl = decl.trim();
                // `[optional|required|repeated] type name` — the label is optional in proto3 text too.
                let (repeated, rest) = if let Some(r) = decl.strip_prefix("repeated ") {
                    (true, r.trim())
                } else if let Some(r) = decl
                    .strip_prefix("optional ")
                    .or_else(|| decl.strip_prefix("required "))
                {
                    (false, r.trim())
                } else {
                    (false, decl)
                };
                // `map<K, V> name` has its own internal comma/space, so it's parsed before the plain
                // whitespace-split path below (which would otherwise mis-tokenize it into 3+ pieces).
                if let Some(after_map) = rest.strip_prefix("map<") {
                    let Some(close) = after_map.find('>') else {
                        continue;
                    };
                    let Some((key_ty, val_ty)) = after_map[..close].split_once(',') else {
                        continue;
                    };
                    let name = after_map[close + 1..].trim();
                    if name.is_empty() {
                        continue;
                    }
                    let key_kind = resolve_scalar_or_message(key_ty.trim());
                    let val_kind = resolve_scalar_or_message(val_ty.trim());
                    fields.push(ProtoField {
                        number,
                        name: name.to_string(),
                        kind: ProtoKind::Map(Box::new(key_kind), Box::new(val_kind)),
                        repeated: false,
                    });
                    continue;
                }
                let toks: Vec<&str> = rest.split_whitespace().collect();
                let [ty, name] = toks.as_slice() else {
                    continue;
                };
                let kind = resolve_scalar_or_message(ty);
                fields.push(ProtoField {
                    number,
                    name: (*name).to_string(),
                    kind,
                    repeated,
                });
            }
            Some(Self { fields })
        }
    }

    fn wire_type(kind: &ProtoKind) -> u64 {
        match kind {
            ProtoKind::Int32 | ProtoKind::Int64 | ProtoKind::Bool => 0,
            ProtoKind::String | ProtoKind::Bytes | ProtoKind::Message(_) | ProtoKind::Map(_, _) => {
                2
            }
        }
    }

    /// The synthetic two-field `{key = 1, value = 2}` schema every map entry is wire-shaped as — real
    /// protobuf's own `map<K, V>` desugaring, reused here so a map entry is encoded/decoded by the
    /// SAME `encode`/`decode` recursion as any other embedded message, not a parallel code path.
    fn map_entry_schema(key_kind: &ProtoKind, val_kind: &ProtoKind) -> ProtoSchema {
        ProtoSchema {
            fields: vec![
                ProtoField {
                    number: 1,
                    name: "key".to_string(),
                    kind: key_kind.clone(),
                    repeated: false,
                },
                ProtoField {
                    number: 2,
                    name: "value".to_string(),
                    kind: val_kind.clone(),
                    repeated: false,
                },
            ],
        }
    }

    /// A decoded map-entry key term, rendered to the string that indexes the outer `SyntaxTerm::Bu` —
    /// real protobuf map keys are always a scalar (string/int32/int64/bool; never `Message`), so every
    /// case here is a direct textual rendering, not a lossy approximation.
    fn key_term_to_string(term: &SyntaxTerm) -> String {
        match term {
            SyntaxTerm::B(Atom::Str(s)) => s.clone(),
            SyntaxTerm::B(Atom::Num(n)) => n.clone(),
            SyntaxTerm::B(Atom::Bool(b)) => b.to_string(),
            _ => String::new(),
        }
    }

    /// The `Atom` a `Bu` map's own key string becomes when writing that key back out, per the map's
    /// declared key kind — the inverse of `key_term_to_string`.
    fn key_string_to_atom(kind: &ProtoKind, k: &str) -> Atom {
        match kind {
            ProtoKind::Bool => Atom::Bool(k == "true"),
            ProtoKind::Int32 | ProtoKind::Int64 => Atom::Num(k.to_string()),
            _ => Atom::Str(k.to_string()),
        }
    }

    /// Raw bytes → a lossless `String`, one `char` per byte (0..=255, all valid Unicode scalar
    /// values) — NOT UTF-8 decoding. `ProtoKind::Bytes` fields (e.g. `DynamicValue.msgpack`,
    /// arbitrary binary) are not valid UTF-8 in general, so `String::from_utf8_lossy` would corrupt
    /// them (replacing invalid sequences with U+FFFD); this bijection round-trips every byte value
    /// exactly, at the cost of no other surface being able to render it meaningfully. `pub` so a
    /// consumer building a `Bytes`-kind field's `SyntaxTerm` value (e.g. a `DynamicValue.msgpack`
    /// carrying real msgpack bytes) uses the SAME mapping `encode_one`/`decode` do, rather than a
    /// second, possibly-mismatched implementation.
    #[must_use]
    pub fn bytes_to_str(bytes: &[u8]) -> String {
        bytes.iter().map(|&b| b as char).collect()
    }

    /// The inverse of `bytes_to_str` — every char must be in the 0..=255 range for this to be a
    /// faithful reconstruction, which holds for any string `bytes_to_str` produced.
    #[must_use]
    pub fn bytes_from_str(s: &str) -> Vec<u8> {
        s.chars().map(|c| c as u32 as u8).collect()
    }

    fn atom_str(a: &Atom) -> Option<&str> {
        match a {
            Atom::Str(s) => Some(s),
            _ => None,
        }
    }

    fn atom_i64(a: &Atom) -> Option<i64> {
        match a {
            Atom::Num(s) => s.parse().ok(),
            Atom::Bool(b) => Some(i64::from(*b)),
            Atom::Str(_) => None,
        }
    }

    /// `SyntaxTerm::Bu` (field name → value) → the wire bytes, per `schema`, resolving any nested
    /// `Message` fields against `registry`. Fields absent from the term, or whose value is `Nb`, are
    /// omitted — proto2 optional-field semantics.
    #[must_use]
    pub fn encode(term: &SyntaxTerm, schema: &ProtoSchema, registry: &ProtoRegistry) -> Vec<u8> {
        let SyntaxTerm::Bu(entries) = term else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // proto field order on the wire is conventionally ascending by number (gogo-proto's
        // `MarshalToSizedBuffer` writes descending internally but the buffer is filled backwards, so
        // the DECODED wire order is ascending) — walk the SCHEMA, not the term's own field order, so
        // wire output matches production regardless of what order the term's map iterates in.
        let mut ordered: Vec<&ProtoField> = schema.fields.iter().collect();
        ordered.sort_by_key(|f| f.number);
        for field in ordered {
            let Some((_, value)) = entries.iter().find(|(k, _)| k == &field.name) else {
                continue;
            };
            encode_field(&mut out, field, value, registry);
        }
        out
    }

    fn encode_field(
        out: &mut Vec<u8>,
        field: &ProtoField,
        value: &SyntaxTerm,
        registry: &ProtoRegistry,
    ) {
        if let ProtoKind::Map(key_kind, val_kind) = &field.kind {
            let SyntaxTerm::Bu(map_entries) = value else {
                return;
            };
            let entry_schema = map_entry_schema(key_kind, val_kind);
            let tag = (u64::from(field.number) << 3) | 2; // Map is always length-delimited, see `wire_type`
            for (k, v) in map_entries {
                let key_term = SyntaxTerm::B(key_string_to_atom(key_kind, k));
                let entry_term = SyntaxTerm::Bu(vec![
                    ("key".to_string(), key_term),
                    ("value".to_string(), v.clone()),
                ]);
                let entry_bytes = encode(&entry_term, &entry_schema, registry);
                write_uvarint(out, tag);
                write_uvarint(out, entry_bytes.len() as u64);
                out.extend_from_slice(&entry_bytes);
            }
            return;
        }
        if field.repeated {
            let SyntaxTerm::Fourth(items) = value else {
                return;
            };
            for item in items {
                encode_one(out, field, item, registry);
            }
            return;
        }
        encode_one(out, field, value, registry);
    }

    fn encode_one(
        out: &mut Vec<u8>,
        field: &ProtoField,
        value: &SyntaxTerm,
        registry: &ProtoRegistry,
    ) {
        let tag = (u64::from(field.number) << 3) | wire_type(&field.kind);
        match &field.kind {
            ProtoKind::Bool | ProtoKind::Int32 | ProtoKind::Int64 => {
                let SyntaxTerm::B(atom) = value else { return };
                let Some(v) = atom_i64(atom) else { return };
                write_uvarint(out, tag);
                write_uvarint(out, v as u64); // plain (non-zigzag) varint — matches proto2 int32/int64/bool
            }
            ProtoKind::String => {
                let SyntaxTerm::B(atom) = value else { return };
                let Some(s) = atom_str(atom) else { return };
                write_uvarint(out, tag);
                write_uvarint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            ProtoKind::Bytes => {
                let SyntaxTerm::B(atom) = value else { return };
                let Some(s) = atom_str(atom) else { return };
                let raw = bytes_from_str(s);
                write_uvarint(out, tag);
                write_uvarint(out, raw.len() as u64);
                out.extend_from_slice(&raw);
            }
            ProtoKind::Message(type_name) => {
                let Some(nested_schema) = registry.get(type_name) else {
                    return;
                };
                let nested_bytes = encode(value, nested_schema, registry);
                write_uvarint(out, tag);
                write_uvarint(out, nested_bytes.len() as u64);
                out.extend_from_slice(&nested_bytes);
            }
            ProtoKind::Map(_, _) => {} // handled in `encode_field` before reaching here — a map needs zero-or-more entries, not one
        }
    }

    /// The wire bytes → `SyntaxTerm::Bu`, per `schema`, resolving any nested `Message` fields against
    /// `registry`. An unknown field number (not in `schema`) is skipped by its wire type, matching real
    /// protobuf's forward-compatibility rule — never an error.
    pub fn decode(
        bytes: &[u8],
        schema: &ProtoSchema,
        registry: &ProtoRegistry,
    ) -> Result<SyntaxTerm, ProtoError> {
        decode_at(bytes, schema, registry, 0)
    }

    /// Deepest message nesting `decode` follows: a self-referential schema lets an untrusted buffer
    /// of nested length-delimited fields recurse once per two bytes, so depth is bounded explicitly.
    const MAX_PROTO_DEPTH: usize = 64;

    fn decode_at(
        bytes: &[u8],
        schema: &ProtoSchema,
        registry: &ProtoRegistry,
        depth: usize,
    ) -> Result<SyntaxTerm, ProtoError> {
        if depth > MAX_PROTO_DEPTH {
            return Err(ProtoError::new("message nested too deeply"));
        }
        let mut pos = 0usize;
        let mut fields: Vec<(String, SyntaxTerm)> = Vec::new();
        while pos < bytes.len() {
            let wire_err = |_| ProtoError::new("truncated or oversized varint");
            let tag = read_uvarint(bytes, &mut pos).map_err(wire_err)?;
            let field_number = (tag >> 3) as u32;
            let wt = tag & 0x7;
            let known = schema.field_by_number(field_number);
            match wt {
                0 => {
                    let v = read_uvarint(bytes, &mut pos).map_err(wire_err)?;
                    if let Some(f) = known {
                        push_scalar(&mut fields, f, v);
                    }
                }
                2 => {
                    let len = usize::try_from(read_uvarint(bytes, &mut pos).map_err(wire_err)?)
                        .map_err(|_| ProtoError::new("truncated length-delimited field"))?;
                    let slice = take_slice(bytes, &mut pos, len)
                        .map_err(|_| ProtoError::new("truncated length-delimited field"))?;
                    if let Some(f) = known {
                        match &f.kind {
                            ProtoKind::Message(type_name) => {
                                let nested_schema = registry.get(type_name).ok_or_else(|| {
                                    ProtoError::new(format!("unknown message type {type_name}"))
                                })?;
                                let nested = decode_at(slice, nested_schema, registry, depth + 1)?;
                                push_value(&mut fields, f, nested);
                            }
                            ProtoKind::Map(key_kind, val_kind) => {
                                let entry_schema = map_entry_schema(key_kind, val_kind);
                                let entry = decode_at(slice, &entry_schema, registry, depth + 1)?;
                                let SyntaxTerm::Bu(entry_fields) = entry else {
                                    return Err(ProtoError::new(
                                        "map entry did not decode to a {key,value} pair",
                                    ));
                                };
                                let key = entry_fields
                                    .iter()
                                    .find(|(k, _)| k == "key")
                                    .map_or_else(String::new, |(_, v)| key_term_to_string(v));
                                let val = entry_fields
                                    .iter()
                                    .find(|(k, _)| k == "value")
                                    .map_or(SyntaxTerm::Nb, |(_, v)| v.clone());
                                push_map_entry(&mut fields, f, key, val);
                            }
                            ProtoKind::Bytes => {
                                push_value(
                                    &mut fields,
                                    f,
                                    SyntaxTerm::B(Atom::Str(bytes_to_str(slice))),
                                );
                            }
                            _ => {
                                let s = String::from_utf8_lossy(slice).into_owned();
                                push_value(&mut fields, f, SyntaxTerm::B(Atom::Str(s)));
                            }
                        }
                    }
                }
                // 64-bit / 32-bit fixed — not modelled by `ProtoKind` yet, skipped per the
                // forward-compat rule (but the skipped bytes must exist)
                1 => {
                    take_slice(bytes, &mut pos, 8)
                        .map_err(|_| ProtoError::new("truncated fixed64 field"))?;
                }
                5 => {
                    take_slice(bytes, &mut pos, 4)
                        .map_err(|_| ProtoError::new("truncated fixed32 field"))?;
                }
                _ => return Err(ProtoError::new(format!("unknown wire type {wt}"))),
            }
        }
        Ok(SyntaxTerm::Bu(fields))
    }

    fn push_scalar(fields: &mut Vec<(String, SyntaxTerm)>, f: &ProtoField, v: u64) {
        let term = match f.kind {
            ProtoKind::Bool => SyntaxTerm::B(Atom::Bool(v != 0)),
            _ => SyntaxTerm::B(Atom::Num((v as i64).to_string())),
        };
        push_value(fields, f, term);
    }

    fn push_value(fields: &mut Vec<(String, SyntaxTerm)>, f: &ProtoField, term: SyntaxTerm) {
        if !f.repeated {
            fields.push((f.name.clone(), term));
            return;
        }
        if let Some((_, SyntaxTerm::Fourth(items))) = fields.iter_mut().find(|(k, _)| k == &f.name)
        {
            items.push(term);
        } else {
            fields.push((f.name.clone(), SyntaxTerm::Fourth(vec![term])));
        }
    }

    /// Merge one decoded `(key, value)` map entry into `f.name`'s `SyntaxTerm::Bu` — real protobuf
    /// map semantics: entries for the SAME field number accumulate into one map, and a repeated key
    /// (legal on the wire, if unusual) is last-one-wins, not appended twice.
    fn push_map_entry(
        fields: &mut Vec<(String, SyntaxTerm)>,
        f: &ProtoField,
        key: String,
        val: SyntaxTerm,
    ) {
        if let Some((_, SyntaxTerm::Bu(map_entries))) =
            fields.iter_mut().find(|(k, _)| k == &f.name)
        {
            if let Some(existing) = map_entries.iter_mut().find(|(k, _)| *k == key) {
                existing.1 = val;
            } else {
                map_entries.push((key, val));
            }
        } else {
            fields.push((f.name.clone(), SyntaxTerm::Bu(vec![(key, val)])));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const REAL_PROTO: &str = r"
    message AWSElasticBlockStoreVolumeSource {
      optional string volumeID = 1;
      optional string fsType = 2;
      optional int32 partition = 3;
      optional bool readOnly = 4;
    }

    message Capabilities {
      repeated string add = 1;
      repeated string drop = 2;
    }

    message SecurityContext {
      optional Capabilities capabilities = 1;
      optional bool privileged = 2;
    }
    ";

        fn real_value() -> SyntaxTerm {
            SyntaxTerm::Bu(vec![
                (
                    "volumeID".into(),
                    SyntaxTerm::B(Atom::Str("vol-123".into())),
                ),
                ("fsType".into(), SyntaxTerm::B(Atom::Str("ext4".into()))),
                ("partition".into(), SyntaxTerm::B(Atom::Num("1".into()))),
                ("readOnly".into(), SyntaxTerm::B(Atom::Bool(true))),
            ])
        }

        /// The real, measured wire bytes from `k8s.io/api`'s actual (gogo-proto generated) Go
        /// `Marshal()` on this exact value — captured live against `go test`, not invented.
        const REAL_K8S_WIRE_HEX: &str = "0a07766f6c2d31323312046578743418012001";

        fn from_hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }

        fn hex_of(bytes: &[u8]) -> String {
            use std::fmt::Write as _;
            bytes.iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
        }

        #[test]
        fn schema_parses_the_real_proto_field_list() {
            let schema = ProtoSchema::parse(REAL_PROTO, "AWSElasticBlockStoreVolumeSource")
                .expect("schema parses");
            assert_eq!(schema.fields.len(), 4);
            assert_eq!(schema.field_by_name("volumeID").unwrap().number, 1);
            assert_eq!(
                schema.field_by_name("partition").unwrap().kind,
                ProtoKind::Int32
            );
            assert_eq!(
                schema.field_by_name("readOnly").unwrap().kind,
                ProtoKind::Bool
            );
        }

        #[test]
        fn matches_real_k8s_wire_bytes() {
            let schema = ProtoSchema::parse(REAL_PROTO, "AWSElasticBlockStoreVolumeSource")
                .expect("schema parses");
            let registry = ProtoRegistry::new();
            let encoded = encode(&real_value(), &schema, &registry);
            assert_eq!(
                hex_of(&encoded),
                REAL_K8S_WIRE_HEX,
                "must match real Go Marshal() output exactly"
            );

            let decoded =
                decode(&from_hex(REAL_K8S_WIRE_HEX), &schema, &registry).expect("decode succeeds");
            assert_eq!(
                decoded,
                real_value(),
                "must reconstruct the identical value from real Go's bytes"
            );
        }

        #[test]
        fn nested_message_field_detected_by_schema() {
            let schema = ProtoSchema::parse(REAL_PROTO, "SecurityContext").expect("schema parses");
            assert_eq!(
                schema.field_by_name("capabilities").unwrap().kind,
                ProtoKind::Message("Capabilities".to_string())
            );
        }

        /// Real, measured: `k8s.io/api`'s `SecurityContext{Capabilities:&Capabilities{Add:["NET_ADMIN"]},
        /// Privileged:true}`, marshaled by the real Go `Marshal()`, captured live against `go test` —
        /// `0a0b0a094e45545f41444d494e1001` — and reconstructed exactly by decode.
        #[test]
        fn matches_real_k8s_wire_bytes_nested_message() {
            const REAL_NESTED_WIRE_HEX: &str = "0a0b0a094e45545f41444d494e1001";
            let registry =
                ProtoRegistry::parse_messages(REAL_PROTO, &["Capabilities", "SecurityContext"]);
            let sc_schema = registry.get("SecurityContext").unwrap();

            let value = SyntaxTerm::Bu(vec![
                (
                    "capabilities".into(),
                    SyntaxTerm::Bu(vec![(
                        "add".into(),
                        SyntaxTerm::Fourth(vec![SyntaxTerm::B(Atom::Str("NET_ADMIN".into()))]),
                    )]),
                ),
                ("privileged".into(), SyntaxTerm::B(Atom::Bool(true))),
            ]);

            let encoded = encode(&value, sc_schema, &registry);
            assert_eq!(
                hex_of(&encoded),
                REAL_NESTED_WIRE_HEX,
                "must match real Go Marshal() output exactly"
            );

            let decoded = decode(&from_hex(REAL_NESTED_WIRE_HEX), sc_schema, &registry).unwrap();
            assert_eq!(
                decoded, value,
                "must reconstruct the identical nested value from real Go's bytes"
            );
        }

        /// Real, measured: `google.golang.org/protobuf/encoding/protowire`'s own `AppendTag`/
        /// `AppendString`/`AppendVarint`/`AppendBytes` — the same primitives protoc-gen-go's generated
        /// `Marshal()` calls into — building a `map<string, int32>` field 5 = `{"a":1, "b":2}`:
        /// `2a050a016110012a050a01621002` (a tiny Go program run live this session, not invented). Each
        /// entry is the implicit `{key=1 string, value=2 int32}` message the protobuf spec desugars
        /// `map<K,V>` into, framed as its own repeated occurrence of field 5.
        #[test]
        fn matches_real_go_protowire_map_bytes() {
            const REAL_MAP_WIRE_HEX: &str = "2a050a016110012a050a01621002";
            let schema = ProtoSchema {
                fields: vec![ProtoField {
                    number: 5,
                    name: "counts".to_string(),
                    kind: ProtoKind::Map(Box::new(ProtoKind::String), Box::new(ProtoKind::Int32)),
                    repeated: false,
                }],
            };
            let registry = ProtoRegistry::new();
            let value = SyntaxTerm::Bu(vec![(
                "counts".to_string(),
                SyntaxTerm::Bu(vec![
                    ("a".to_string(), SyntaxTerm::B(Atom::Num("1".to_string()))),
                    ("b".to_string(), SyntaxTerm::B(Atom::Num("2".to_string()))),
                ]),
            )]);

            let encoded = encode(&value, &schema, &registry);
            assert_eq!(
                hex_of(&encoded),
                REAL_MAP_WIRE_HEX,
                "must match real Go protowire output exactly"
            );

            let decoded =
                decode(&from_hex(REAL_MAP_WIRE_HEX), &schema, &registry).expect("decode succeeds");
            assert_eq!(
                decoded, value,
                "must reconstruct the identical map from real Go's bytes"
            );
        }

        /// A map whose value is a `Message`, not a scalar — `map<string, Schema>`, exactly
        /// `tfplugin6.GetProviderSchema.Response.resource_schemas`'s own shape, the concrete need this
        /// extension exists for. Round-trip only (no external capture for this one — the point is
        /// confirming `ProtoKind::Message` inside a map entry recurses through the SAME registry
        /// lookup a top-level `Message` field already uses, not a parallel path).
        #[test]
        fn map_with_message_values_round_trips() {
            const SCHEMA_SRC: &str = r"
    message Attr {
      string type = 1;
    }
    message Response {
      map<string, Attr> resource_schemas = 2;
    }
    ";
            let registry = ProtoRegistry::parse_messages(SCHEMA_SRC, &["Attr", "Response"]);
            let schema = registry.get("Response").unwrap();
            assert_eq!(
                schema.field_by_name("resource_schemas").unwrap().kind,
                ProtoKind::Map(
                    Box::new(ProtoKind::String),
                    Box::new(ProtoKind::Message("Attr".to_string()))
                )
            );

            let value = SyntaxTerm::Bu(vec![(
                "resource_schemas".to_string(),
                SyntaxTerm::Bu(vec![
                    (
                        "null_resource".to_string(),
                        SyntaxTerm::Bu(vec![(
                            "type".to_string(),
                            SyntaxTerm::B(Atom::Str("object".to_string())),
                        )]),
                    ),
                    (
                        "null_data".to_string(),
                        SyntaxTerm::Bu(vec![(
                            "type".to_string(),
                            SyntaxTerm::B(Atom::Str("string".to_string())),
                        )]),
                    ),
                ]),
            )]);

            let encoded = encode(&value, schema, &registry);
            let decoded = decode(&encoded, schema, &registry).expect("decode succeeds");
            assert_eq!(
                decoded, value,
                "message-valued map entries must round-trip through the registry recursion"
            );
        }

        /// `bytes` fields used to be decoded with `String::from_utf8_lossy`, same as `string` fields
        /// — silently CORRUPTING any bytes value that isn't valid UTF-8 (replacing invalid sequences
        /// with U+FFFD, `\u{FFFD}`). This is not hypothetical: `tfplugin6.DynamicValue.msgpack` is
        /// exactly a `bytes` field carrying genuinely non-UTF-8 binary — e.g. Terraform's own
        /// "unknown value" marker is the 3 raw bytes `0xd4 0x00 0x00`, and `0xd4` alone is an invalid
        /// UTF-8 continuation byte. Confirms the fix: every byte value 0..=255 round-trips exactly.
        #[test]
        fn bytes_field_round_trips_genuinely_non_utf8_binary() {
            const SCHEMA_SRC: &str = r"
    message DynamicValue {
        bytes msgpack = 1;
    }
    ";
            let schema = ProtoSchema::parse(SCHEMA_SRC, "DynamicValue").expect("schema parses");
            let registry = ProtoRegistry::new();

            // Every byte 0..=255 in one blob, including Terraform's real "unknown" marker bytes.
            let raw: Vec<u8> = (0u8..=255).collect();
            let value = SyntaxTerm::Bu(vec![(
                "msgpack".to_string(),
                SyntaxTerm::B(Atom::Str(bytes_to_str(&raw))),
            )]);

            let encoded = encode(&value, &schema, &registry);
            // The wire bytes must contain the RAW bytes, not a UTF-8/lossy re-encoding of them.
            assert!(
                encoded.windows(raw.len()).any(|w| w == raw.as_slice()),
                "wire bytes must contain the exact raw byte sequence"
            );

            let decoded = decode(&encoded, &schema, &registry).expect("decode succeeds");
            let SyntaxTerm::Bu(fields) = &decoded else {
                panic!("expected Bu")
            };
            let SyntaxTerm::B(Atom::Str(s)) = &fields[0].1 else {
                panic!("expected a string atom")
            };
            assert_eq!(
                bytes_from_str(s),
                raw,
                "every byte 0..=255 must round-trip exactly, no lossy substitution"
            );
        }

        /// `map<string, X>` was previously silently DROPPED by the parser (its `map<string,` /
        /// `X>` / `name` tokens didn't match the plain `[type, name]` destructure) — confirms the
        /// field now actually appears in the parsed schema at all, the regression this whole
        /// extension exists to fix.
        #[test]
        fn map_field_no_longer_silently_dropped_by_parser() {
            const SCHEMA_SRC: &str = r"
    message GetProviderSchemaResponse {
      map<string, Schema> resource_schemas = 2;
      map<string, Schema> data_source_schemas = 3;
    }
    ";
            let schema =
                ProtoSchema::parse(SCHEMA_SRC, "GetProviderSchemaResponse").expect("schema parses");
            assert_eq!(
                schema.fields.len(),
                2,
                "both map fields must be present, not silently skipped"
            );
            assert_eq!(schema.field_by_name("resource_schemas").unwrap().number, 2);
            assert_eq!(
                schema.field_by_name("data_source_schemas").unwrap().kind,
                ProtoKind::Map(
                    Box::new(ProtoKind::String),
                    Box::new(ProtoKind::Message("Schema".to_string()))
                )
            );
        }

        /// The real ambiguity `ProtoSchema::parse` used to fall into: this excerpt is the ACTUAL text
        /// of two adjacent messages from the real `tfplugin6.5.proto` (vendored this session from
        /// `hashicorp/terraform`'s own published protocol file) — `GetProviderSchema` and
        /// `ValidateProviderConfig` each declare their OWN nested `message Response { … }` with
        /// completely different fields (the real file has 16 such `Response` messages total). An
        /// unqualified `ProtoSchema::parse(src, "Response")` would silently grab whichever comes FIRST
        /// in the file — this confirms the dot-qualified form reaches the SPECIFIC one meant, in both
        /// directions.
        #[test]
        fn dot_qualified_name_resolves_the_correct_nested_response_not_the_first_one_in_the_file() {
            const REAL_TFPLUGIN6_EXCERPT: &str = r"
    message GetProviderSchema {
        message Request {
        }
        message Response {
            Schema provider = 1;
            map<string, Schema> resource_schemas = 2;
            map<string, Schema> data_source_schemas = 3;
            map<string, Function> functions = 7;
            repeated Diagnostic diagnostics = 4;
            Schema provider_meta = 5;
            ServerCapabilities server_capabilities = 6;
        }
    }

    message ValidateProviderConfig {
        message Request {
            DynamicValue config = 1;
        }
        message Response {
            repeated Diagnostic diagnostics = 2;
        }
    }
    ";
            let gps_response =
                ProtoSchema::parse(REAL_TFPLUGIN6_EXCERPT, "GetProviderSchema.Response")
                    .expect("GetProviderSchema.Response must resolve");
            assert_eq!(
                gps_response.fields.len(),
                7,
                "GetProviderSchema.Response has 7 fields, not ValidateProviderConfig.Response's 1"
            );
            assert_eq!(
                gps_response.field_by_name("resource_schemas").unwrap().kind,
                ProtoKind::Map(
                    Box::new(ProtoKind::String),
                    Box::new(ProtoKind::Message("Schema".to_string()))
                )
            );

            let vpc_response =
                ProtoSchema::parse(REAL_TFPLUGIN6_EXCERPT, "ValidateProviderConfig.Response")
                    .expect("ValidateProviderConfig.Response must resolve");
            assert_eq!(
                vpc_response.fields.len(),
                1,
                "the OTHER Response must not be confused with GetProviderSchema's"
            );
            assert_eq!(vpc_response.field_by_name("diagnostics").unwrap().number, 2);
        }

        /// A REAL bug this session, found live via `a sibling Terraform-plugin client` decoding a real provider's
        /// schema into garbage: parsing an OUTER message that itself wraps NESTED messages used to
        /// leak the nested messages' own field lines into the outer one's field list — with no
        /// per-block scoping, `block.lines()` saw every `type name = number;`-shaped line in the
        /// ENTIRE nested hierarchy, not just the outer message's own top-level fields. This is
        /// catastrophic exactly when field NUMBERS collide across nesting levels — which the real
        /// `tfplugin6.proto`/`tfplugin5.proto` `Schema` message does: `Schema.block = 2` collides
        /// with the nested `Schema.Block.attributes = 2`, so `field_by_number(2)` returned whichever
        /// happened to appear FIRST in the raw text (the nested one, since nested messages are
        /// declared before the outer message's own fields in this real file) — decoding a real
        /// provider's `Schema` value then recursed using the WRONG type (`Message("Attribute")`
        /// instead of `Message("Block")`), corrupting every attribute into raw sub-message bytes
        /// under a spurious `type` key. This excerpt is `Schema`'s REAL shape (trimmed).
        #[test]
        fn parsing_a_message_does_not_leak_nested_messages_own_fields() {
            const REAL_SCHEMA_EXCERPT: &str = r"
    message Schema {
        message Block {
            int64 version = 1;
            repeated Attribute attributes = 2;
            repeated NestedBlock block_types = 3;
        }

        message Attribute {
            string name = 1;
            bytes type = 2;
            string description = 3;
            bool required = 4;
        }

        message NestedBlock {
            enum NestingMode {
                INVALID = 0;
                SINGLE = 1;
                LIST = 2;
            }
            string type_name = 1;
            Block block = 2;
            NestingMode nesting = 3;
        }

        int64 version = 1;
        Block block = 2;
    }
    ";
            let schema =
                ProtoSchema::parse(REAL_SCHEMA_EXCERPT, "Schema").expect("Schema must resolve");
            assert_eq!(schema.fields.len(), 2, "Schema has exactly 2 fields of its OWN — version and block, not the nested messages' fields too");
            assert_eq!(
                schema.field_by_name("version").unwrap().kind,
                ProtoKind::Int64
            );
            assert_eq!(
                schema.field_by_name("block").unwrap().kind,
                ProtoKind::Message("Block".to_string()),
                "field 2 must be Schema's own `block` (a Block message), not the nested Block's own field 2 (`attributes`, an Attribute list)"
            );

            // The nested messages, parsed via their qualified paths, must each have ONLY their own
            // fields — not contaminated by their SIBLING nested messages either.
            let block = ProtoSchema::parse(REAL_SCHEMA_EXCERPT, "Schema.Block")
                .expect("Schema.Block must resolve");
            assert_eq!(block.fields.len(), 3);
            assert_eq!(
                block.field_by_name("attributes").unwrap().kind,
                ProtoKind::Message("Attribute".to_string())
            );

            let nested_block = ProtoSchema::parse(REAL_SCHEMA_EXCERPT, "Schema.NestedBlock")
                .expect("Schema.NestedBlock must resolve");
            assert_eq!(
                nested_block.fields.len(),
                3,
                "NestedBlock's own 3 fields, not its sibling enum's 3 constants too"
            );
            assert_eq!(
                nested_block.field_by_name("nesting").unwrap().kind,
                ProtoKind::Int32,
                "NestingMode is a nested enum (a varint), not accidentally treated as a message"
            );
        }
    }
}
#[cfg(feature = "codec")]
pub mod syntax {
    //! Collapsed syntax for prompt/http surfaces.
    //!
    //! JSON, BAML, CSV, TOON, POML, and YAML are treated as applicatives over one term:
    //! NB = absent/null, B = scalar, B/U = keyed relation/object, 4th = ordered list.
    //!
    //! This file holds the term itself, the `AsTerm` conversions, the public entry points and the
    //! binary term codec. The per-surface machinery lives beside it and is re-exported from here, so
    //! every `codec::syntax::…` path is unchanged: [`surfaces`] (JSON/BAML/CSV/TOON/POML),
    //! [`yaml`], [`rdf`] (Turtle + RDF/XML).

    mod rdf;
    mod surfaces;
    mod yaml;

    #[cfg(test)]
    mod tests;

    pub use rdf::{parse_rdfxml, parse_turtle, render_rdfxml, render_turtle};
    pub use yaml::parse_yaml;

    use super::{compress, decompress};
    use surfaces::{
        parse_baml, parse_csv, parse_poml, parse_toon, render_baml_term, render_csv, render_json,
        render_poml, render_toon, JsonParser,
    };
    use yaml::render_yaml;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum SyntaxTerm {
        Nb,
        B(Atom),
        Bu(Vec<(String, SyntaxTerm)>),
        Fourth(Vec<SyntaxTerm>),
        /// "Known to exist, not yet known what it is" — genuinely distinct from `Nb` (explicitly
        /// absent). No surface here (JSON/BAML/CSV/TOON/POML/YAML) has this concept; it exists for `cty`
        /// (Terraform's value system, `codec::msgpack`), where a Computed attribute during planning
        /// is neither null nor any concrete value — e.g. a resource's `id` before it's created.
        /// Surfaces without the concept degrade it to their own absent representation (`Nb`'s), never
        /// silently to a wrong concrete value.
        Unknown,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Atom {
        Bool(bool),
        Num(String),
        Str(String),
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Surface {
        Json,
        Baml,
        /// Path/value CSV — a flat, spreadsheet-friendly view of the term tree.
        Csv,
        Toon,
        Poml,
        /// Block-style YAML — the format semgrep rules, OWASP ZAP's Automation Framework config,
        /// Kubernetes manifests, and most CI/tool rule-files actually ship in. See [`parse_yaml`]
        /// for the honest scope (a real, wide subset — not the full YAML 1.2 spec).
        Yaml,
        /// RDF 1.1 Turtle — the wire format SEPA, SAREF, OEO, Brick, and most published
        /// OWL/RDF ontologies ship in. See [`parse_turtle`] for the honest scope.
        Turtle,
        /// RDF/XML — the older OWL wire format (used by Protégé's default exporter and many
        /// legacy ontologies). See [`parse_rdfxml`] for the honest scope.
        RdfXml,
    }

    /// A type convertible to/from the universal term — implemented by every `#[derive(Term)]` type
    /// (in `wirefold_derive`) as well as `HttpCall` (hand-written, below). Lets GENERIC code (a
    /// JSON-body extractor, a config loader) work over `T: AsTerm` for any struct that derives or
    /// hand-implements the conversion, rather than requiring one concrete type.
    pub trait AsTerm: Sized {
        fn to_term(&self) -> SyntaxTerm;
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError>;
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct HttpCall {
        pub method: String,
        pub url: String,
        pub headers: Vec<(String, String)>,
        pub body: SyntaxTerm,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct SyntaxError {
        pub msg: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum LintLevel {
        Error,
        Warning,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct SyntaxLint {
        pub level: LintLevel,
        pub path: String,
        pub msg: String,
    }

    impl SyntaxError {
        pub fn new(msg: impl Into<String>) -> Self {
            Self { msg: msg.into() }
        }
    }

    impl HttpCall {
        #[must_use]
        pub fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Bu(vec![
                (
                    "method".into(),
                    SyntaxTerm::B(Atom::Str(self.method.clone())),
                ),
                ("url".into(), SyntaxTerm::B(Atom::Str(self.url.clone()))),
                (
                    "headers".into(),
                    SyntaxTerm::Bu(
                        self.headers
                            .iter()
                            .map(|(k, v)| (k.clone(), SyntaxTerm::B(Atom::Str(v.clone()))))
                            .collect(),
                    ),
                ),
                ("body".into(), self.body.clone()),
            ])
        }

        pub fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let SyntaxTerm::Bu(fields) = term else {
                return Err(SyntaxError::new("http call must be a B/U object"));
            };
            let method = field_str(fields, "method")?.to_string();
            let url = field_str(fields, "url")?.to_string();
            let headers = match field(fields, "headers") {
                Some(SyntaxTerm::Bu(hs)) => hs
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), term_str(v)?.to_string())))
                    .collect::<Result<Vec<_>, SyntaxError>>()?,
                Some(SyntaxTerm::Nb) | None => Vec::new(),
                _ => return Err(SyntaxError::new("headers must be a B/U object")),
            };
            let body = field(fields, "body").cloned().unwrap_or(SyntaxTerm::Nb);
            Ok(Self {
                method,
                url,
                headers,
                body,
            })
        }
    }

    /// The identity conversion — lets generic `T: AsTerm` code (a `Json<T>` extractor, e.g.) work
    /// with the untyped term directly (`Json<SyntaxTerm>`), not just a `#[derive(Term)]` struct.
    impl AsTerm for SyntaxTerm {
        fn to_term(&self) -> SyntaxTerm {
            self.clone()
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            Ok(term.clone())
        }
    }

    /// A flat string-keyed map — query-string/path-param shape (every value a string on the wire,
    /// no nesting) — as a `Bu` of `Atom::Str`/`Atom::Num` values, decoded back via `term_str`
    /// (which already accepts either `Atom` variant, so this round-trips regardless of whether the
    /// term's values were built as `Str` or type-inferred `Num`). Lets `Query<T>`/`Path<T>`-style
    /// extractors default to `T = HashMap<String, String>` (untyped access) while ALSO supporting
    /// `T = SomeTypedStruct` (via `#[derive(Term)]`) from the exact same conversion.
    #[allow(clippy::implicit_hasher)] // deliberately the std-default HashMap — the untyped extractor default
    impl AsTerm for std::collections::HashMap<String, String> {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Bu(
                self.iter()
                    .map(|(k, v)| (k.clone(), SyntaxTerm::B(Atom::Str(v.clone()))))
                    .collect(),
            )
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let SyntaxTerm::Bu(fields) = term else {
                return Err(SyntaxError::new("expected a B/U object"));
            };
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), term_str(v)?.to_string())))
                .collect()
        }
    }

    /// A single positional value — actix-shaped `Path<String>`'s use case: a route with exactly one
    /// dynamic segment (`/blacklist/{ip}`), extracted by POSITION rather than by the segment's
    /// declared name (unlike the `HashMap<String,String>`/`#[derive(Term)]` cases above, which are
    /// name-keyed). `Path`'s router builds a `Bu` whose entries are in the route pattern's
    /// left-to-right declaration order (see bedrock's `PathParams`/`match_pattern`), so "first entry"
    /// here means "the one dynamic segment," regardless of what it was named in the pattern.
    impl AsTerm for String {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::B(Atom::Str(self.clone()))
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            match term {
                SyntaxTerm::B(Atom::Str(s) | Atom::Num(s)) => Ok(s.clone()),
                SyntaxTerm::Bu(fields) => match fields.as_slice() {
                    [(_, only)] => term_str(only).map(str::to_string),
                    _ => Err(SyntaxError::new(
                        "expected exactly one positional value, found a different count",
                    )),
                },
                _ => Err(SyntaxError::new(
                    "expected a scalar string or a single-field object",
                )),
            }
        }
    }

    /// A single positional numeric/bool value — same role as `AsTerm for String` above, for
    /// `Path<i64>`/`Path<bool>`-shaped routes (e.g. `/email/cancel/{id}` → `id: web::Path<i64>`).
    /// `pairs_to_term` (bedrock's `actix.rs`) already type-infers a captured segment into
    /// `Atom::Num`/`Atom::Bool` where it parses as one, so this reads that back rather than
    /// re-parsing from a string every time.
    macro_rules! positional_scalar_as_term {
        ($ty:ty) => {
            impl AsTerm for $ty {
                fn to_term(&self) -> SyntaxTerm {
                    SyntaxTerm::B(Atom::Num(self.to_string()))
                }
                fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
                    let s = match term {
                        SyntaxTerm::B(Atom::Num(s) | Atom::Str(s)) => s.as_str(),
                        SyntaxTerm::Bu(fields) => match fields.as_slice() {
                            [(_, only)] => term_num(only).or_else(|_| term_str(only))?,
                            _ => return Err(SyntaxError::new(
                                "expected exactly one positional value, found a different count",
                            )),
                        },
                        _ => {
                            return Err(SyntaxError::new(concat!(
                                "expected a scalar ",
                                stringify!($ty),
                                " or a single-field object"
                            )))
                        }
                    };
                    s.parse::<$ty>().map_err(|e| {
                        SyntaxError::new(format!(
                            "cannot parse {:?} as {}: {e}",
                            s,
                            stringify!($ty)
                        ))
                    })
                }
            }
        };
    }

    positional_scalar_as_term!(i64);
    positional_scalar_as_term!(i32);
    positional_scalar_as_term!(u64);
    positional_scalar_as_term!(u32);
    positional_scalar_as_term!(f64);

    impl AsTerm for bool {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::B(Atom::Bool(*self))
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            match term {
                SyntaxTerm::B(Atom::Bool(b)) => Ok(*b),
                SyntaxTerm::Bu(fields) => match fields.as_slice() {
                    [(_, only)] => term_bool(only),
                    _ => Err(SyntaxError::new(
                        "expected exactly one positional value, found a different count",
                    )),
                },
                _ => Err(SyntaxError::new(
                    "expected a scalar bool or a single-field object",
                )),
            }
        }
    }

    /// The unit payload — a value that carries no information, encoded as the empty B/U record.
    ///
    /// This exists so a generic container parameterised by a payload type works when the caller
    /// wants no payload at all: `()` is the natural default for `VectorCollection<P>` and its kin,
    /// and without this impl such a container cannot be encoded at its own default. A blanket impl
    /// plus a unit impl overlaps (E0119), so the unit case has to be written out here rather than
    /// worked around at each call site.
    ///
    /// `from_term` accepts anything: there is exactly one value of type `()`, so no input can
    /// disagree with it, and rejecting a shape would be inventing information the type does not have.
    impl AsTerm for () {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Bu(Vec::new())
        }
        fn from_term(_term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            Ok(())
        }
    }

    /// A struct field (or `Path<IpAddr>`) holding a parsed IP address, e.g. rauthy's own
    /// `IpBlacklistRequest`/`UserRevokeParams` — round-trips through its standard string form.
    impl AsTerm for std::net::IpAddr {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::B(Atom::Str(self.to_string()))
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let s = match term {
                SyntaxTerm::B(Atom::Str(s) | Atom::Num(s)) => s.as_str(),
                SyntaxTerm::Bu(fields) => match fields.as_slice() {
                    [(_, only)] => term_str(only)?,
                    _ => {
                        return Err(SyntaxError::new(
                            "expected exactly one positional value, found a different count",
                        ))
                    }
                },
                _ => {
                    return Err(SyntaxError::new(
                        "expected a scalar IP address or a single-field object",
                    ))
                }
            };
            s.parse::<std::net::IpAddr>()
                .map_err(|e| SyntaxError::new(format!("cannot parse {s:?} as an IP address: {e}")))
        }
    }

    /// Two-segment positional extraction — `Path<(A, B)>` for ANY per-position `AsTerm` types (e.g.
    /// `Path<(String, String)>` on `/users/{user_id}/picture/{picture_id}`, or `Path<(String, i64)>`
    /// on `/theme/{client_id}/{timestamp}` — mixed types, not just homogeneous strings), read by
    /// pattern-declaration position, names discarded. Each position's own `AsTerm::from_term` already
    /// accepts a bare scalar term directly (see `impl AsTerm for String`/numeric types above), so this
    /// just hands each field's term straight to the corresponding type, no re-wrapping needed.
    impl<A: AsTerm, B: AsTerm> AsTerm for (A, B) {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Bu(vec![
                ("0".to_string(), self.0.to_term()),
                ("1".to_string(), self.1.to_term()),
            ])
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let SyntaxTerm::Bu(fields) = term else {
                return Err(SyntaxError::new(
                    "expected an object with positional fields",
                ));
            };
            if fields.len() != 2 {
                return Err(SyntaxError::new("expected exactly 2 positional values"));
            }
            Ok((A::from_term(&fields[0].1)?, B::from_term(&fields[1].1)?))
        }
    }

    /// Three-segment positional extraction, for the rare route with three dynamic segments.
    impl<A: AsTerm, B: AsTerm, C: AsTerm> AsTerm for (A, B, C) {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Bu(vec![
                ("0".to_string(), self.0.to_term()),
                ("1".to_string(), self.1.to_term()),
                ("2".to_string(), self.2.to_term()),
            ])
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let SyntaxTerm::Bu(fields) = term else {
                return Err(SyntaxError::new(
                    "expected an object with positional fields",
                ));
            };
            if fields.len() != 3 {
                return Err(SyntaxError::new("expected exactly 3 positional values"));
            }
            Ok((
                A::from_term(&fields[0].1)?,
                B::from_term(&fields[1].1)?,
                C::from_term(&fields[2].1)?,
            ))
        }
    }

    /// A bare `Vec<T>` at the top level (a `.json(some_vec)` response body, not a struct field) — the
    /// derive macro already encodes a `Vec<T>` FIELD this exact way (`FieldShape::ListOf`, see
    /// `wirefold_derive::to_term_expr`/`from_term_result_expr`); this is the same `Fourth(...)`
    /// shape made available directly on `Vec<T>` itself, since the derive only ever generates an impl
    /// for the named struct/enum wrapping a field, never for `Vec` as a standalone type.
    impl<T: AsTerm> AsTerm for Vec<T> {
        fn to_term(&self) -> SyntaxTerm {
            SyntaxTerm::Fourth(self.iter().map(AsTerm::to_term).collect())
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            let SyntaxTerm::Fourth(items) = term else {
                return Err(SyntaxError::new("expected a list"));
            };
            items.iter().map(T::from_term).collect()
        }
    }

    impl AsTerm for HttpCall {
        fn to_term(&self) -> SyntaxTerm {
            HttpCall::to_term(self)
        }
        fn from_term(term: &SyntaxTerm) -> Result<Self, SyntaxError> {
            HttpCall::from_term(term)
        }
    }

    #[must_use]
    pub fn render_surface(term: &SyntaxTerm, surface: Surface) -> String {
        match surface {
            Surface::Json => render_json(term),
            Surface::Baml => format!("baml {}\n", render_baml_term(term, 0)),
            Surface::Csv => render_csv(term),
            Surface::Toon => render_toon(term),
            Surface::Poml => render_poml(term),
            Surface::Yaml => render_yaml(term, 0),
            Surface::Turtle => render_turtle(term),
            Surface::RdfXml => render_rdfxml(term),
        }
    }

    pub fn parse_surface(surface: Surface, input: &str) -> Result<SyntaxTerm, SyntaxError> {
        match surface {
            Surface::Json => JsonParser::new(input, false).parse(),
            Surface::Baml => parse_baml(input),
            Surface::Csv => parse_csv(input),
            Surface::Toon => parse_toon(input),
            Surface::Poml => parse_poml(input),
            Surface::Yaml => parse_yaml(input),
            Surface::Turtle => parse_turtle(input),
            Surface::RdfXml => parse_rdfxml(input),
        }
    }

    pub fn translate(input: &str, from: Surface, to: Surface) -> Result<String, SyntaxError> {
        parse_surface(from, input).map(|term| render_surface(&term, to))
    }

    #[must_use]
    pub fn lint_term(term: &SyntaxTerm) -> Vec<SyntaxLint> {
        let mut out = Vec::new();
        lint_term_at("$", term, &mut out);
        out
    }

    #[must_use]
    pub fn lint_surface(surface: Surface, input: &str) -> Vec<SyntaxLint> {
        match parse_surface(surface, input) {
            Ok(term) => {
                let mut out = lint_term(&term);
                lint_render(surface, &term, &mut out);
                out
            }
            Err(err) => vec![SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: err.msg,
            }],
        }
    }

    #[must_use]
    pub fn lint_http_surface(surface: Surface, input: &str) -> Vec<SyntaxLint> {
        match parse_surface(surface, input) {
            Ok(term) => {
                let mut out = lint_term(&term);
                lint_http_term(&term, &mut out);
                lint_render(surface, &term, &mut out);
                out
            }
            Err(err) => vec![SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: err.msg,
            }],
        }
    }

    pub fn validate_surface(surface: Surface, input: &str) -> Result<SyntaxTerm, Vec<SyntaxLint>> {
        match parse_surface(surface, input) {
            Ok(term) => {
                let mut lints = lint_term(&term);
                lint_render(surface, &term, &mut lints);
                if lints.iter().any(|lint| lint.level == LintLevel::Error) {
                    Err(lints)
                } else {
                    Ok(term)
                }
            }
            Err(err) => Err(vec![SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: err.msg,
            }]),
        }
    }

    #[must_use]
    pub fn cerealize_term(term: &SyntaxTerm) -> Vec<u8> {
        compress(&encode_term(term))
    }

    pub fn decerealize_term(bytes: &[u8]) -> Result<SyntaxTerm, SyntaxError> {
        let raw = decompress(bytes);
        let mut pos = 0usize;
        let term = decode_term(&raw, &mut pos)?;
        if pos != raw.len() {
            return Err(SyntaxError::new("trailing term bytes"));
        }
        Ok(term)
    }

    #[must_use]
    pub fn cerealize_http(call: &HttpCall) -> Vec<u8> {
        cerealize_term(&call.to_term())
    }

    pub fn decerealize_http(bytes: &[u8]) -> Result<HttpCall, SyntaxError> {
        HttpCall::from_term(&decerealize_term(bytes)?)
    }

    /// Find `key` among a `Bu`'s fields — the one lookup every accessor below is built from. `pub`
    /// so a `#[derive(Term)]`-generated `from_term` (in `wirefold_derive`) can use the same
    /// primitive `HttpCall::from_term` is hand-written against, rather than re-deriving field lookup.
    pub fn field<'a>(fields: &'a [(String, SyntaxTerm)], key: &str) -> Option<&'a SyntaxTerm> {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn field_str<'a>(
        fields: &'a [(String, SyntaxTerm)],
        key: &str,
    ) -> Result<&'a str, SyntaxError> {
        field(fields, key)
            .ok_or_else(|| SyntaxError::new(format!("missing field {key}")))
            .and_then(term_str)
    }

    /// A field's `bool` scalar — `#[derive(Term)]`'s bool-typed-field accessor.
    pub fn field_bool(fields: &[(String, SyntaxTerm)], key: &str) -> Result<bool, SyntaxError> {
        field(fields, key)
            .ok_or_else(|| SyntaxError::new(format!("missing field {key}")))
            .and_then(term_bool)
    }

    /// A field's `Num` scalar (the raw digit string — the caller parses it to its concrete numeric
    /// type; `Atom::Num` carries no width/signedness of its own) — `#[derive(Term)]`'s
    /// integer/float-typed-field accessor.
    pub fn field_num<'a>(
        fields: &'a [(String, SyntaxTerm)],
        key: &str,
    ) -> Result<&'a str, SyntaxError> {
        field(fields, key)
            .ok_or_else(|| SyntaxError::new(format!("missing field {key}")))
            .and_then(term_num)
    }

    /// An OPTIONAL field: absent or `Nb` both mean `None`; anything else is decoded with `f`. Used
    /// by `#[derive(Term)]` for `Option<T>` fields — matches this module's own `Nb` = absent
    /// convention rather than requiring the field to be present-but-null.
    pub fn field_opt<'a, T>(
        fields: &'a [(String, SyntaxTerm)],
        key: &str,
        f: impl FnOnce(&'a SyntaxTerm) -> Result<T, SyntaxError>,
    ) -> Result<Option<T>, SyntaxError> {
        match field(fields, key) {
            None | Some(SyntaxTerm::Nb) => Ok(None),
            Some(term) => f(term).map(Some),
        }
    }

    /// A field's `Fourth` (list) — `#[derive(Term)]`'s `Vec<T>`-typed-field accessor. Absent is
    /// treated as an empty list (matching `Option`'s absent = `None`), not an error — most JSON
    /// producers omit an empty array field rather than emit `[]` explicitly.
    pub fn field_list<'a>(
        fields: &'a [(String, SyntaxTerm)],
        key: &str,
    ) -> Result<&'a [SyntaxTerm], SyntaxError> {
        match field(fields, key) {
            None | Some(SyntaxTerm::Nb) => Ok(&[]),
            Some(SyntaxTerm::Fourth(items)) => Ok(items),
            _ => Err(SyntaxError::new(format!("field {key} must be a list"))),
        }
    }

    pub fn term_str(term: &SyntaxTerm) -> Result<&str, SyntaxError> {
        match term {
            SyntaxTerm::B(Atom::Str(s) | Atom::Num(s)) => Ok(s),
            _ => Err(SyntaxError::new("expected scalar string")),
        }
    }

    pub fn term_bool(term: &SyntaxTerm) -> Result<bool, SyntaxError> {
        match term {
            SyntaxTerm::B(Atom::Bool(b)) => Ok(*b),
            _ => Err(SyntaxError::new("expected a bool")),
        }
    }

    pub fn term_num(term: &SyntaxTerm) -> Result<&str, SyntaxError> {
        match term {
            SyntaxTerm::B(Atom::Num(s)) => Ok(s),
            _ => Err(SyntaxError::new("expected a number")),
        }
    }

    fn lint_term_at(path: &str, term: &SyntaxTerm, out: &mut Vec<SyntaxLint>) {
        match term {
            SyntaxTerm::Nb | SyntaxTerm::B(_) | SyntaxTerm::Unknown => {}
            SyntaxTerm::Bu(fields) => {
                let mut seen: Vec<&str> = Vec::new();
                for (key, value) in fields {
                    if key.is_empty() {
                        out.push(SyntaxLint {
                            level: LintLevel::Error,
                            path: path.into(),
                            msg: "empty B/U key".into(),
                        });
                    }
                    if seen.contains(&key.as_str()) {
                        out.push(SyntaxLint {
                            level: LintLevel::Warning,
                            path: format!("{path}.{key}"),
                            msg: "duplicate B/U key shadows an earlier field".into(),
                        });
                    } else {
                        seen.push(key);
                    }
                    lint_term_at(&child_path(path, key), value, out);
                }
            }
            SyntaxTerm::Fourth(items) => {
                for (idx, value) in items.iter().enumerate() {
                    lint_term_at(&format!("{path}[{idx}]"), value, out);
                }
            }
        }
    }

    fn lint_http_term(term: &SyntaxTerm, out: &mut Vec<SyntaxLint>) {
        match HttpCall::from_term(term) {
            Ok(call) => {
                if call.method.trim().is_empty() {
                    out.push(SyntaxLint {
                        level: LintLevel::Error,
                        path: "$.method".into(),
                        msg: "HTTP method is empty".into(),
                    });
                }
                if call.url.trim().is_empty() {
                    out.push(SyntaxLint {
                        level: LintLevel::Error,
                        path: "$.url".into(),
                        msg: "HTTP URL is empty".into(),
                    });
                }
                for (key, _) in &call.headers {
                    if key.trim().is_empty() {
                        out.push(SyntaxLint {
                            level: LintLevel::Error,
                            path: "$.headers".into(),
                            msg: "HTTP header name is empty".into(),
                        });
                    }
                }
            }
            Err(err) => out.push(SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: format!("invalid HTTP call: {}", err.msg),
            }),
        }
    }

    fn lint_render(surface: Surface, term: &SyntaxTerm, out: &mut Vec<SyntaxLint>) {
        let rendered = render_surface(term, surface);
        match parse_surface(surface, &rendered) {
            Ok(round) if round == *term => {}
            Ok(_) => out.push(SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: "canonical rendering does not parse to the same term".into(),
            }),
            Err(err) => out.push(SyntaxLint {
                level: LintLevel::Error,
                path: "$".into(),
                msg: format!("canonical rendering is unparsable: {}", err.msg),
            }),
        }
    }

    fn child_path(path: &str, key: &str) -> String {
        if path == "$" {
            format!("$.{key}")
        } else {
            format!("{path}.{key}")
        }
    }

    fn encode_term(term: &SyntaxTerm) -> Vec<u8> {
        let mut out = Vec::new();
        encode_into(term, &mut out);
        out
    }

    fn encode_into(term: &SyntaxTerm, out: &mut Vec<u8>) {
        match term {
            SyntaxTerm::Nb => out.push(0),
            SyntaxTerm::B(Atom::Bool(v)) => {
                out.push(1);
                out.push(*v as u8);
            }
            SyntaxTerm::B(Atom::Num(v)) => {
                out.push(2);
                put_bytes(out, v.as_bytes());
            }
            SyntaxTerm::B(Atom::Str(v)) => {
                out.push(3);
                put_bytes(out, v.as_bytes());
            }
            SyntaxTerm::Bu(fields) => {
                out.push(4);
                put_uvarint(out, fields.len() as u64);
                for (k, v) in fields {
                    put_bytes(out, k.as_bytes());
                    encode_into(v, out);
                }
            }
            SyntaxTerm::Fourth(items) => {
                out.push(5);
                put_uvarint(out, items.len() as u64);
                for v in items {
                    encode_into(v, out);
                }
            }
            SyntaxTerm::Unknown => out.push(6),
        }
    }

    /// Deepest `Bu`/`Fourth` nesting `decode_term` accepts: the decoder recurses once per level, so
    /// an untrusted buffer of nested openers must not be allowed to exhaust the stack.
    const MAX_TERM_DEPTH: usize = 128;

    fn decode_term(bytes: &[u8], pos: &mut usize) -> Result<SyntaxTerm, SyntaxError> {
        decode_term_at(bytes, pos, 0)
    }

    fn decode_term_at(
        bytes: &[u8],
        pos: &mut usize,
        depth: usize,
    ) -> Result<SyntaxTerm, SyntaxError> {
        if depth > MAX_TERM_DEPTH {
            return Err(SyntaxError::new("term nested too deeply"));
        }
        let tag = *bytes
            .get(*pos)
            .ok_or_else(|| SyntaxError::new("short term"))?;
        *pos += 1;
        Ok(match tag {
            0 => SyntaxTerm::Nb,
            1 => {
                let v = *bytes
                    .get(*pos)
                    .ok_or_else(|| SyntaxError::new("short bool"))?
                    != 0;
                *pos += 1;
                SyntaxTerm::B(Atom::Bool(v))
            }
            2 => SyntaxTerm::B(Atom::Num(get_string(bytes, pos)?)),
            3 => SyntaxTerm::B(Atom::Str(get_string(bytes, pos)?)),
            4 => {
                let n = get_count(bytes, pos)?;
                let mut fields = Vec::with_capacity(n);
                for _ in 0..n {
                    fields.push((
                        get_string(bytes, pos)?,
                        decode_term_at(bytes, pos, depth + 1)?,
                    ));
                }
                SyntaxTerm::Bu(fields)
            }
            5 => {
                let n = get_count(bytes, pos)?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(decode_term_at(bytes, pos, depth + 1)?);
                }
                SyntaxTerm::Fourth(items)
            }
            6 => SyntaxTerm::Unknown,
            _ => return Err(SyntaxError::new("bad term tag")),
        })
    }

    fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
        put_uvarint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    fn get_string(bytes: &[u8], pos: &mut usize) -> Result<String, SyntaxError> {
        let n = usize::try_from(get_uvarint(bytes, pos)?)
            .map_err(|_| SyntaxError::new("short string"))?;
        let raw = super::take_slice(bytes, pos, n).map_err(|_| SyntaxError::new("short string"))?;
        String::from_utf8(raw.to_vec()).map_err(|_| SyntaxError::new("utf8"))
    }

    /// An element count: every element is at least one byte on the wire, so a count beyond the
    /// remaining input is a lie — reject it before reserving anything for it.
    fn get_count(bytes: &[u8], pos: &mut usize) -> Result<usize, SyntaxError> {
        let n = usize::try_from(get_uvarint(bytes, pos)?)
            .map_err(|_| SyntaxError::new("bad element count"))?;
        if n > bytes.len().saturating_sub(*pos) {
            return Err(SyntaxError::new("element count exceeds input"));
        }
        Ok(n)
    }

    fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }

    fn get_uvarint(bytes: &[u8], pos: &mut usize) -> Result<u64, SyntaxError> {
        super::read_uvarint(bytes, pos).map_err(|_| SyntaxError::new("bad varint"))
    }
}
pub mod tans {
    //! tANS — the entropy fold realized as a READOUT (a table), per `EntropyFoldReadout`.
    //!
    //! `NullCellCoding` proves the coder is the Null-cell product fold, Shannon-optimal, and (over ideal
    //! ℝ) tableless. But a real coder runs a BOUNDED state; that bounded-state fold's per-symbol transition
    //! is a pure function, so it is memoized into a TABLE (`SharedFold`) that gives the identical code, and
    //! the table read is a `CoordinateReadout` (the state is the address). This module is that table:
    //! per literal, one lookup + a few bits, where the range coder recomputes 8 renorm loops. Same fold
    //! (`foldl_agrees`), read out instead of recomputed — `ReadoutIsNotSearch` on the coder.
    //!
    //! Static order-0 over a byte alphabet (the FSE construction: spread by `TABLESTEP`, `symbolTT`
    //! deltas, the state tables). Encode is LIFO (reverse), decode forward — a bit-addressable stack makes
    //! push/pop exact inverses by construction. Locked by `tans_roundtrip` before anything rides it.

    const TABLE_LOG: u32 = 12; // 4096 states — the bounded coder register (the readout's coordinate space)

    /// A bit-addressable stack: `push(v,n)` then `pop(n)` returns `v` exactly (LIFO). tANS encodes in
    /// reverse and decodes forward, so the coder stream IS a stack; this makes the two provably inverse.
    struct BitStack {
        words: Vec<u64>,
        top: usize, // bit index of the stack top
    }
    impl BitStack {
        fn new() -> Self {
            BitStack {
                words: vec![0u64],
                top: 0,
            }
        }
        fn from_bytes(bytes: &[u8], bits: usize) -> Self {
            let mut words = vec![0u64; bytes.len() / 8 + 2];
            for (i, &b) in bytes.iter().enumerate() {
                words[i >> 3] |= u64::from(b) << ((i & 7) * 8);
            }
            BitStack { words, top: bits }
        }
        #[inline]
        fn push(&mut self, v: u64, n: u32) {
            let w = self.top >> 6;
            let off = (self.top & 63) as u32;
            if w + 1 >= self.words.len() {
                self.words.push(0);
                self.words.push(0);
            }
            let m = v & mask(n);
            self.words[w] |= m << off;
            // branchless straddle: `(m >> (63-off)) >> 1` = `m >> (64-off)` without the off=0 UB shift
            self.words[w + 1] |= (m >> (63 - off)) >> 1;
            self.top += n as usize;
        }
        #[inline]
        fn pop(&mut self, n: u32) -> u64 {
            self.top -= n as usize;
            let w = self.top >> 6;
            let off = (self.top & 63) as u32;
            let mut v = self.words[w] >> off;
            if off + n > 64 {
                v |= self.words[w + 1] << (64 - off);
            }
            v & mask(n)
        }
        fn into_bytes(self) -> (Vec<u8>, usize) {
            let nbytes = self.top.div_ceil(8);
            let mut out = Vec::with_capacity(nbytes);
            for i in 0..nbytes {
                out.push((self.words[i >> 3] >> ((i & 7) * 8)) as u8);
            }
            (out, self.top)
        }
    }

    #[inline]
    fn mask(n: u32) -> u64 {
        if n == 0 {
            0
        } else {
            (1u64 << n) - 1
        }
    }

    #[inline]
    fn highbit(x: u32) -> u32 {
        x.ilog2()
    }

    /// `FSE_TABLESTEP` — the spread stride that distributes symbol values pseudo-uniformly across the state
    /// table (fastcompression.blogspot's distribution).
    #[inline]
    fn table_step(size: usize) -> usize {
        (size >> 1) + (size >> 3) + 3
    }

    /// Normalize the histogram so the slot counts sum to EXACTLY `tableSize`, every seen symbol ≥ 1. The
    /// rounding remainder is absorbed by the most frequent symbol (FSE's largest-symbol correction).
    fn normalize(hist: &[u32; 256], total: u32, table_size: u32) -> [u16; 256] {
        let mut norm = [0u16; 256];
        let (mut sum, mut maxs, mut maxc) = (0i64, 0usize, 0u32);
        for s in 0..256 {
            if hist[s] == 0 {
                continue;
            }
            if hist[s] > maxc {
                maxc = hist[s];
                maxs = s;
            }
            let p = ((u64::from(hist[s]) * u64::from(table_size)) / u64::from(total)) as u16;
            let p = p.max(1);
            norm[s] = p;
            sum += i64::from(p);
        }
        // Absorb the remainder into the largest symbol (it has the slack to give or take).
        //
        // The `.max(1)` floor is what keeps every seen symbol representable — but it also means
        // this single absorption can FAIL to close the gap. When many distinct symbols were each
        // clamped UP to 1 at the `p.max(1)` above (a near-uniform byte histogram — row-major `f32`
        // bytes are exactly this), `sum` overshoots `table_size` by more than the largest symbol
        // has to give back, and the floor pins it at 1 with the total still too high.
        //
        // Leaving it there breaks this function's own contract ("sum to EXACTLY `tableSize`"), and
        // the breakage surfaces two frames away: `build_enc`'s cumulative start `cur[s]` then walks
        // past the end of `state_table`, panicking at `idx == table_size`. So close the gap here,
        // where the invariant lives, rather than making the consumer defensive about it.
        let old_max = i64::from(norm[maxs]);
        let new_max = (old_max + (i64::from(table_size) - sum)).max(1);
        norm[maxs] = new_max as u16;
        sum += new_max - old_max;

        // Still over: shave one slot at a time off whichever symbol currently has the most, never
        // taking a seen symbol below 1. Terminates because each pass removes exactly one slot and
        // the overshoot is bounded by the alphabet (≤ 256), while `table_log` is floored at what
        // the alphabet needs — so a symbol with slack always exists until the sum lands.
        while sum > i64::from(table_size) {
            let mut victim = usize::MAX;
            let mut most = 1u16;
            for (s, &n) in norm.iter().enumerate() {
                if n > most {
                    most = n;
                    victim = s;
                }
            }
            if victim == usize::MAX {
                break; // every seen symbol is already at its floor of 1 — nothing left to give
            }
            norm[victim] -= 1;
            sum -= 1;
        }
        debug_assert!(
            sum <= i64::from(table_size),
            "normalize must not overshoot the table: sum {sum} > table_size {table_size}"
        );
        norm
    }

    /// The per-symbol encode transform (`symbolTT`): `(deltaNbBits, deltaFindState)`.
    #[derive(Clone, Copy, Default)]
    struct SymTT {
        delta_nb_bits: i64,
        delta_find_state: i64,
    }

    /// Build the encode tables from the normalized counts: the spread, the state table (next-state per
    /// slot), and the per-symbol transform. Mirrors `FSE_buildCTable`.
    fn build_enc(norm: &[u16; 256], table_log: u32) -> (Vec<u16>, [SymTT; 256]) {
        let table_size = 1usize << table_log;
        let mask = table_size - 1;
        let step = table_step(table_size);
        // cumulative starts + spread
        let mut cumul = [0u32; 257];
        for s in 0..256 {
            cumul[s + 1] = cumul[s] + u32::from(norm[s]);
        }
        let mut symbol = vec![0u8; table_size];
        let mut pos = 0usize;
        for (s, &n) in norm.iter().enumerate() {
            for _ in 0..n {
                symbol[pos] = s as u8;
                pos = (pos + step) & mask;
            }
        }
        // state table: sorted by symbol order, gives the next state value (tableSize + slot)
        let mut state_table = vec![0u16; table_size];
        let mut cur = cumul;
        for (u, &s) in symbol.iter().enumerate() {
            let idx = cur[s as usize];
            cur[s as usize] += 1;
            state_table[idx as usize] = (table_size + u) as u16;
        }
        // per-symbol transform
        let mut tt = [SymTT::default(); 256];
        let mut total: i64 = 0;
        for s in 0..256 {
            match norm[s] {
                0 => {}
                1 => {
                    tt[s].delta_nb_bits = (i64::from(table_log) << 16) - (table_size as i64);
                    tt[s].delta_find_state = total - 1;
                    total += 1;
                }
                nc => {
                    let max_bits_out = table_log - highbit(u32::from(nc) - 1);
                    let min_state_plus = (u32::from(nc) << max_bits_out) as i64;
                    tt[s].delta_nb_bits = (i64::from(max_bits_out) << 16) - min_state_plus;
                    tt[s].delta_find_state = total - i64::from(nc);
                    total += i64::from(nc);
                }
            }
        }
        (state_table, tt)
    }

    /// One decode-table slot: the symbol emitted, the bits to read, and the next-state base.
    #[derive(Clone, Copy, Default)]
    struct DEntry {
        symbol: u8,
        nb_bits: u8,
        new_state: u16,
    }

    /// One-pass PACKED decode-table build: spread into a reusable scratch, emit `sym|nb<<8|base<<16`
    /// directly — no DEntry intermediate, no second packing pass (the 2026-07-11 annotate put the
    /// pack pass + spread at ~19% of face decode; `table_is_the_fold` prices the table once, so the
    /// build must cost one pass, not three).
    fn build_dec_packed(norm: &[u16; 256], table_log: u32, scratch: &mut Vec<u8>, out: &mut [u32]) {
        let table_size = 1usize << table_log;
        let mask = table_size - 1;
        let step = table_step(table_size);
        scratch.clear();
        scratch.resize(table_size, 0);
        let mut pos = 0usize;
        for (s, &n) in norm.iter().enumerate() {
            for _ in 0..n {
                scratch[pos] = s as u8;
                pos = (pos + step) & mask;
            }
        }
        let mut symbol_next = [0u16; 256];
        symbol_next.copy_from_slice(norm);
        for u in 0..table_size {
            let sy = scratch[u];
            let next_state = symbol_next[usize::from(sy)];
            symbol_next[usize::from(sy)] += 1;
            let nb = table_log - highbit(u32::from(next_state));
            // Layout: nb[0..4] · sym[8..16] · base[16..]. nb in the LOW nibble so the hot decode loop
            // extracts it with one `& 0xF` (on the state chain); sym rides bits 8..16 read only by the
            // off-chain store. nb ≤ table_log ≤ 12 fits 4 bits.
            out[u] = nb
                | (u32::from(sy) << 8)
                | ((((u32::from(next_state)) << nb) - table_size as u32) << 16);
        }
    }

    /// Build the decode table from the normalized counts — mirrors `FSE_buildDTable`. The SAME spread, so
    /// it is the inverse of the encode table.
    fn build_dec(norm: &[u16; 256], table_log: u32) -> Vec<DEntry> {
        let table_size = 1usize << table_log;
        let mask = table_size - 1;
        let step = table_step(table_size);
        let mut symbol_next = [0u16; 256];
        symbol_next.copy_from_slice(norm);
        let mut symbol = vec![0u8; table_size];
        let mut pos = 0usize;
        for (s, &n) in norm.iter().enumerate() {
            for _ in 0..n {
                symbol[pos] = s as u8;
                pos = (pos + step) & mask;
            }
        }
        let mut dt = vec![DEntry::default(); table_size];
        for u in 0..table_size {
            let s = symbol[u];
            let next_state = symbol_next[s as usize];
            symbol_next[s as usize] += 1;
            let nb_bits = table_log - highbit(u32::from(next_state));
            dt[u] = DEntry {
                symbol: s,
                nb_bits: nb_bits as u8,
                new_state: ((u32::from(next_state) << nb_bits) - table_size as u32) as u16,
            };
        }
        dt
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            if v < 128 {
                out.push(v as u8);
                break;
            }
            out.push((v % 128 + 128) as u8);
            v /= 128;
        }
    }
    /// A declared bit or byte count as `usize`.
    fn get_len(b: &[u8], p: &mut usize) -> Result<usize, DecodeError> {
        read_len(b, p)
    }

    /// The table-log range every coded block this module writes lies in (`compress4`/`o1` floor it at
    /// 5, the order-0 coder fixes it at [`TABLE_LOG`]); the spread stride is only a permutation (odd)
    /// from 5 up, and a table of `2^log` entries must stay a sane size.
    fn check_table_log(table_log: u32) -> Result<(), DecodeError> {
        if (5..=TABLE_LOG).contains(&table_log) {
            Ok(())
        } else {
            Err(DecodeError::Malformed)
        }
    }

    /// Read `max_sym + 1` little-endian `u16` normalized counts (symbols above `max_sym` are 0) and
    /// check they fill the table exactly — the invariant `normalize` establishes and every table
    /// build below relies on (a short or long table would leave slots unset or index past the end).
    fn read_norm(
        bytes: &[u8],
        p: &mut usize,
        max_sym: usize,
        table_log: u32,
    ) -> Result<[u16; 256], DecodeError> {
        let raw = take_slice(bytes, p, 2 * (max_sym + 1))?;
        let mut norm = [0u16; 256];
        let mut sum = 0u32;
        for (n, c) in norm.iter_mut().zip(raw.chunks_exact(2)) {
            *n = u16::from_le_bytes([c[0], c[1]]);
            sum += u32::from(*n);
        }
        if sum != 1u32 << table_log {
            return Err(DecodeError::Malformed);
        }
        Ok(norm)
    }

    /// An upper bound on the symbols `total_bits` of stream can carry under `norm`: a symbol of count
    /// `c < T` costs at least `log2(T/(T-1))` bits, so `n ≤ bits · T`. A symbol owning the whole table
    /// costs nothing, and a block of it is legitimately unbounded by its input: then only the output
    /// cap applies.
    fn max_symbols(norm: &[u16; 256], table_log: u32, total_bits: usize) -> usize {
        let t = 1usize << table_log;
        if norm.iter().any(|&c| usize::from(c) == t) {
            DEFAULT_MAX_OUTPUT
        } else {
            total_bits.saturating_mul(t).min(DEFAULT_MAX_OUTPUT)
        }
    }

    /// tANS-compress a byte stream: `[n][tableLog][256 norms][nbits][stack bytes]`. Static order-0.
    #[must_use]
    pub fn compress(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut out = Vec::new();
        put_varint(&mut out, n as u64);
        if n == 0 {
            return out;
        }
        let mut hist = [0u32; 256];
        for &b in data {
            hist[usize::from(b)] += 1;
        }
        // pick the table log (≤ TABLE_LOG, and ≥ what the alphabet needs)
        let table_log = TABLE_LOG;
        let table_size = 1u32 << table_log;
        let norm = normalize(&hist, n as u32, table_size);
        out.push(table_log as u8);
        for n in &norm {
            out.extend_from_slice(&n.to_le_bytes());
        }
        let (state_table, tt) = build_enc(&norm, table_log);

        let mut bs = BitStack::new();
        let mut value = table_size; // init state = tableSize (low end of [tableSize, 2·tableSize))
                                    // encode in REVERSE (LIFO)
        for &b in data.iter().rev() {
            let s = usize::from(b);
            let nb = ((i64::from(value) + tt[s].delta_nb_bits) >> 16) as u32;
            bs.push(u64::from(value) & mask(nb), nb);
            let idx = ((i64::from(value >> nb)) + tt[s].delta_find_state) as usize;
            value = u32::from(state_table[idx]);
        }
        // flush the final state (its low tableLog bits = state − tableSize, decode's initial state)
        bs.push(u64::from(value), table_log);
        let (bytes, bits) = bs.into_bytes();
        put_varint(&mut out, bits as u64);
        out.extend_from_slice(&bytes);
        out
    }

    /// Inverse of [`compress`]. Malformed input yields an empty `Vec` (never a panic);
    /// [`try_decompress`] says why.
    #[must_use]
    pub fn decompress(bytes: &[u8]) -> Vec<u8> {
        try_decompress(bytes).unwrap_or_default()
    }

    /// Checked [`decompress`]. Every length and table is validated before it is used: the table must
    /// fill its `2^log` slots exactly, the stream must hold the bits it declares, and `n` is bounded
    /// by what those bits can carry (or by [`DEFAULT_MAX_OUTPUT`] for a single-symbol table).
    pub fn try_decompress(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut p = 0usize;
        let n = get_len(bytes, &mut p)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        let table_log = u32::from(
            *take_slice(bytes, &mut p, 1)?
                .first()
                .ok_or(DecodeError::Truncated)?,
        );
        check_table_log(table_log)?;
        let norm = read_norm(bytes, &mut p, 255, table_log)?;
        let dt = build_dec(&norm, table_log);
        let bits = get_len(bytes, &mut p)?;
        let stream = &bytes[p..];
        if bits > stream.len().saturating_mul(8) || bits < table_log as usize {
            return Err(DecodeError::Truncated);
        }
        if n > max_symbols(&norm, table_log, bits) {
            return Err(DecodeError::TooLarge);
        }
        let mut bs = BitStack::from_bytes(stream, bits);

        let mut out = try_vec(n)?;
        // the final-state was pushed last → pop it first as the initial decode state
        let mut state = bs.pop(table_log) as usize;
        for _ in 0..n {
            let e = dt[state];
            out.push(e.symbol);
            let nb = u32::from(e.nb_bits);
            if bs.top < nb as usize {
                return Err(DecodeError::Truncated); // the stream ran dry before the symbols did
            }
            let low = bs.pop(nb) as usize;
            state = usize::from(e.new_state) + low;
        }
        Ok(out)
    }

    // ── tans4 — the 4-stream kernel: ONE shared table, four states in one interleaved loop ──
    // `SharedFold.kway_stream_decode_exact`: a contiguous split (`chunks_flatten` — the parts
    // reassemble the message) decoded by k independent bounded-state folds under ANY interleaved
    // schedule is exact. Four states advancing in one loop overlap the per-symbol serial chain
    // (state → table → bits → state, ~6-8 cycles) across streams — the ILP the single state
    // cannot have. One histogram, one norm header, one table: the four sub-streams share the
    // block's distribution (they are the SAME source), so the header is paid once.
    //
    // REJECTED (2026-07-11, measured flat both corpora, both run orders — do not retry): the
    // HUF-style REGISTER bit-container (huf_decompress_amd64.S: one 8-byte reload per symbol
    // group instead of a per-symbol unaligned load). Flat because the tANS chain is serialized
    // on the TABLE load (state → entry → next state, ~5 cy L1), so the bit-read beside it is off
    // the critical path either way. Huffman's container pays only because Huffman HAS no state
    // chain — its next address is the container top itself (a register shift): that is the
    // structural speed gap between the coders, priced against tANS's density, not removable
    // by porting their loop shape.

    use super::{read_len, take_slice, try_vec, DecodeError, DEFAULT_MAX_OUTPUT};
    use crate::codec::bits::read_bits_at;

    /// Encode the four streams in ONE interleaved loop (reverse order per stream) — the same
    /// 4-chain ILP as the decode, `kway_stream_decode_exact` read encoder-side. Streams 0-2 are
    /// length `q`, stream 3 is the (≤ q) remainder, so the loop runs two phases.
    fn enc4(
        parts: [&[u8]; 4],
        state_table: &[u16],
        tt: &[SymTT; 256],
        table_log: u32,
    ) -> [(Vec<u8>, usize); 4] {
        let q = parts[0].len();
        let c3 = parts[3].len();
        // the FORWARD accumulator (zstd BIT_addBits/flushBits shape, read from bitstream.h:177):
        // BitStack's push already appended LSB-first at ascending positions — its stack-ness is
        // only the decoder's pop. Same bytes, no Vec<u64> straddle ORs, no growth branch: the
        // coder's reverse stream is the same O(1) field-element write as the raw bits (BitW).
        let mut acc = [0u64; 4];
        let mut cnt = [0u32; 4];
        let mut out4: [Vec<u8>; 4] = Default::default();
        for (k, o) in out4.iter_mut().enumerate() {
            o.reserve(parts[k].len() + 8);
        }
        let mut val = [1u32 << table_log; 4];
        macro_rules! estep {
            ($k:expr, $j:expr) => {{
                let s = usize::from(parts[$k][$j]);
                let nb = ((i64::from(val[$k]) + tt[s].delta_nb_bits) >> 16) as u32;
                acc[$k] |= (u64::from(val[$k]) & mask(nb)) << cnt[$k];
                cnt[$k] += nb;
                while cnt[$k] >= 8 {
                    out4[$k].push(acc[$k] as u8);
                    acc[$k] >>= 8;
                    cnt[$k] -= 8;
                }
                let idx = ((i64::from(val[$k] >> nb)) + tt[s].delta_find_state) as usize;
                val[$k] = u32::from(state_table[idx]);
            }};
        }
        for j in (c3..q).rev() {
            estep!(0, j);
            estep!(1, j);
            estep!(2, j);
        }
        for j in (0..c3).rev() {
            estep!(0, j);
            estep!(1, j);
            estep!(2, j);
            estep!(3, j);
        }
        let mut out: [(Vec<u8>, usize); 4] = Default::default();
        for k in 0..4 {
            acc[k] |= (u64::from(val[k]) & mask(table_log)) << cnt[k];
            cnt[k] += table_log;
            while cnt[k] >= 8 {
                out4[k].push(acc[k] as u8);
                acc[k] >>= 8;
                cnt[k] -= 8;
            }
            let bits = out4[k].len() * 8 + cnt[k] as usize;
            if cnt[k] > 0 {
                out4[k].push(acc[k] as u8);
            }
            out[k] = (std::mem::take(&mut out4[k]), bits);
        }
        out
    }

    /// tans4 compress: `[varint n]` then either the RAW escape (`tableLog = 0`, tiny inputs) or
    /// `[tableLog][maxSym][(maxSym+1)×u16 norms][4× varint bits][4 streams]` — the contiguous
    /// 4-way split, one shared table. The alphabet cap keeps slot-stream headers ~10× smaller
    /// than the fixed 512 B.
    #[must_use]
    pub fn compress4(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut out = Vec::with_capacity(n / 2 + 64);
        put_varint(&mut out, n as u64);
        if n == 0 {
            return out;
        }
        if n < 32 {
            out.push(0); // RAW escape — a table costs more than the data
            out.extend_from_slice(data);
            return out;
        }
        let mut hist = [0u32; 256];
        for &b in data {
            hist[usize::from(b)] += 1;
        }
        let max_sym = (0..256).rev().find(|&s| hist[s] > 0).unwrap_or(0);
        // The table log is DERIVED, not fixed at 12: bounded BELOW by the alphabet's coordinate floor
        // (`EntropyFoldReadout.minimal_table_log`: clog2(alphabet) ≈ highbit(max_sym)+1 — states below
        // this can't name every symbol) and set by the COUNT for precision (log2(n)−2, the FSE rule:
        // more data justifies a wider register). A ~40-symbol / few-K-symbol slot block lands under 12
        // and drops build/spread + fits L1; high-count 256-symbol literals stay at 12.
        let alpha_floor = highbit((max_sym as u32).max(2)) + 1;
        let table_log = highbit(n as u32)
            .saturating_sub(2)
            .clamp(alpha_floor, TABLE_LOG)
            .max(5);
        let norm = normalize(&hist, n as u32, 1 << table_log);
        out.push(table_log as u8);
        out.push(max_sym as u8);
        for n in &norm[..=max_sym] {
            out.extend_from_slice(&n.to_le_bytes());
        }
        let (state_table, tt) = build_enc(&norm, table_log);
        let q = n.div_ceil(4);
        let parts = [
            &data[..q],
            &data[q..2 * q],
            &data[2 * q..3 * q],
            &data[3 * q..],
        ];
        let coded = enc4(parts, &state_table, &tt, table_log);
        for (_, bits) in &coded {
            put_varint(&mut out, *bits as u64);
        }
        for (bytes, _) in &coded {
            out.extend_from_slice(bytes);
        }
        out
    }

    /// Inverse of [`compress4`] — the four states advance in ONE interleaved loop (exact under any
    /// schedule, `kway_stream_decode_exact`), each writing its contiguous quarter.
    ///
    /// Malformed input yields an empty `Vec` (never a panic); [`try_decompress4`] says why.
    #[must_use]
    pub fn decompress4(bytes: &[u8]) -> Vec<u8> {
        try_decompress4(bytes).unwrap_or_default()
    }

    /// Checked [`decompress4`].
    pub fn try_decompress4(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::new();
        try_decompress4_into(bytes, &mut out)?;
        Ok(out)
    }

    /// Decode one 4-stream block directly onto `dst`'s tail — no per-block temp `Vec` and no
    /// zero-fill of the output (the four stream quarters cover every byte in `[start, start+n)`
    /// before any read, so the region is written-not-zeroed). `untans4_blocked` was decoding into
    /// a fresh `Vec` per block and copying it into the accumulator (a whole-stream memcpy + an
    /// `n`-byte memset the decode then overwrites); this removes both. Byte-identical output.
    /// Runtime BMI2 dispatch — SOVEREIGN multiversioning, not a baked-in `target-cpu=native` binary
    /// (which SIGILLs on any CPU lacking the ISA). The bit loop wants two ops the branch-heavy generic
    /// codegen misses: `bzhi(w, nb)` = the branchless low-`nb`-bit mask, and `shrx` = the flag-free
    /// variable shift. Detect `bmi2` at runtime (cached), dispatch to the `#[target_feature]` variant
    /// where present, generic fallback otherwise (and on every non-x86 target — where the same core
    /// lowers to `ubfx`/`lsrv` on ARM, the Zbb/Zbs equivalents on RISC-V). The core is
    /// `#[inline(always)]`, so it inlines into BOTH wrappers and each gets its target's codegen — this
    /// IS "our ISA primitive lowers to the native op per target", the L3-L4 travel-reduction reading.
    ///
    /// Malformed input leaves `dst` exactly as it was (never a panic, never exposed uninitialised
    /// bytes); [`try_decompress4_into`] says why.
    pub fn decompress4_into(bytes: &[u8], dst: &mut Vec<u8>) {
        let _ = try_decompress4_into(bytes, dst);
    }

    /// Checked [`decompress4_into`]. On `Err`, `dst` is untouched: the output is decoded into spare
    /// capacity and the length is set only once every byte of it has been written.
    pub fn try_decompress4_into(bytes: &[u8], dst: &mut Vec<u8>) -> Result<(), DecodeError> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            return unsafe { decompress4_into_bmi2(bytes, dst) };
        }
        decompress4_into_impl(bytes, dst)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn decompress4_into_bmi2(bytes: &[u8], dst: &mut Vec<u8>) -> Result<(), DecodeError> {
        decompress4_into_impl(bytes, dst)
    }

    #[inline(always)]
    fn decompress4_into_impl(bytes: &[u8], dst: &mut Vec<u8>) -> Result<(), DecodeError> {
        let mut p = 0usize;
        let n = get_len(bytes, &mut p)?;
        if n == 0 {
            return Ok(());
        }
        let table_log = u32::from(
            *take_slice(bytes, &mut p, 1)?
                .first()
                .ok_or(DecodeError::Truncated)?,
        );
        if table_log == 0 {
            let raw = take_slice(bytes, &mut p, n)?; // RAW escape: bounded by the input
            dst.try_reserve(n).map_err(|_| DecodeError::TooLarge)?;
            dst.extend_from_slice(raw);
            return Ok(());
        }
        check_table_log(table_log)?;
        let max_sym = usize::from(
            *take_slice(bytes, &mut p, 1)?
                .first()
                .ok_or(DecodeError::Truncated)?,
        );
        let norm = read_norm(bytes, &mut p, max_sym, table_log)?;
        let mut bits = [0usize; 4];
        for b in &mut bits {
            *b = get_len(bytes, &mut p)?;
            if *b < table_log as usize {
                return Err(DecodeError::Truncated); // each stream carries at least its initial state
            }
        }
        let total_bits = bits
            .iter()
            .try_fold(0usize, |a, &b| a.checked_add(b))
            .ok_or(DecodeError::TooLarge)?;
        if n > max_symbols(&norm, table_log, total_bits) {
            return Err(DecodeError::TooLarge);
        }
        let q = n.div_ceil(4);
        if 3 * q > n {
            return Err(DecodeError::Malformed); // a coded block always has n ≥ 32
        }
        // packed decode table: sym | nb<<8 | base<<16 — built in ONE fused pass. Allocated at DOUBLE
        // size, top half zero: `tans_state_bounded` proves a valid stream keeps state < table_size, so
        // the top half is never read; a CORRUPT stream reaches at most 2·table_size (base ≤ table_size
        // from the valid table + bits < 2^nb ≤ table_size) and lands on a zero entry (sym0 nb0 base0 →
        // state 0, recovers) — so the per-symbol `& tmask` index guard is removed from the hot loop's
        // load-address chain, SAFELY, no corruption UB. (`read_norm` proved the table is valid: the
        // counts fill it exactly.)
        let mut tbl = vec![0u32; 2usize << table_log];
        let mut scratch = Vec::new();
        build_dec_packed(
            &norm,
            table_log,
            &mut scratch,
            &mut tbl[..1usize << table_log],
        );
        // slice the four streams; each gets its own padded buffer (8 B slack for the unaligned load)
        let mut bufs: [Vec<u8>; 4] = Default::default();
        for k in 0..4 {
            let blen = bits[k].div_ceil(8);
            let src = take_slice(bytes, &mut p, blen)?;
            let mut buf = try_vec(blen + 8)?;
            buf.extend_from_slice(src);
            buf.extend_from_slice(&[0u8; 8]);
            bufs[k] = buf;
        }
        let c3 = n - 3 * q; // the last stream's (possibly shorter) count
                            // pop the initial states (pushed last)
        let mut top = [0usize; 4];
        let mut st = [0usize; 4];
        for k in 0..4 {
            top[k] = bits[k] - table_log as usize;
            st[k] = (read_bits_at(&bufs[k], top[k]) & mask(table_log)) as usize;
        }
        // The output is written into SPARE CAPACITY and published (`set_len`) only after every byte
        // has been written: an `Err` or a panic anywhere below leaves `dst` as the caller had it.
        let start = dst.len();
        dst.try_reserve(n).map_err(|_| DecodeError::TooLarge)?;
        let (mut o0, mut o1, mut o2, mut o3) = (0usize, q, 2 * q, 3 * q);
        // THE CONSTRUCTED LOOP (DeterministicTrips, the external proof development; the huf-loop method):
        // scalar state (the array/spill traffic was the measured surplus), unchecked stores —
        // each stream writes exactly its quarter, one bound proof — masked table access, and
        // the REGISTER container (one 8-byte load per 4 symbols; retried here after measuring
        // flat inside the OLD loop shape, where the stack chain masked it). Variable bit
        // consumption means the trip count is NOT deterministic (the theorem's named boundary):
        // the group guard is per-group on scalars — rem ≥ 4 ∧ top ≥ 56 — and the exact tails
        // run the checked per-symbol step.
        let (mut t0, mut t1, mut t2, mut t3) = (top[0], top[1], top[2], top[3]);
        let (mut s0, mut s1, mut s2, mut s3) = (st[0], st[1], st[2], st[3]);
        let (mut r0, mut r1, mut r2, mut r3) = (q, q, q, c3);
        assert!(3 * q <= n && tbl.len() == 2usize << table_log);
        let tp = tbl.as_ptr();
        // write into dst's spare capacity (offsets o0..o3 are relative to `start`); `try_reserve`
        // above made `start + n` bytes of capacity available
        let ob = unsafe { dst.as_mut_ptr().add(start) };
        macro_rules! g4 {
            ($buf:expr, $t:ident, $s:ident, $o:ident) => {{
                let b0 = ($t >> 3) - 7;
                let acc = unsafe { $buf.as_ptr().add(b0).cast::<u64>().read_unaligned() };
                // track the SHIFT amount `sh = t - apos` directly (decrement by nb) instead of
                // updating t and recomputing (t - apos) each symbol — collapses two chain subs to
                // one; t is restored once per group for the reload and guard. apos = b0*8.
                let mut sh = $t - b0 * 8;
                for _ in 0..4 {
                    // no per-symbol mask: state < table_size by construction (tans_state_bounded),
                    // and the 2×-size table absorbs any corrupt overshoot (< 2·table_size) safely
                    let e = unsafe { *tp.add($s) };
                    unsafe {
                        *ob.add($o) = (e >> 8) as u8; // sym in bits 8..16 (off-chain store)
                    }
                    $o += 1;
                    let nb = (e & 0xF) as u32; // nb in the low nibble: one AND on the state chain
                    sh -= nb as usize;
                    let w = acc >> sh;
                    $s = ((e >> 16) as usize) + (w & mask(nb)) as usize;
                }
                $t = sh + b0 * 8; // restore t = sh + apos for the next group's reload + guard
            }};
        }
        while r0 >= 4 && r3 >= 4 && t0 >= 56 && t1 >= 56 && t2 >= 56 && t3 >= 56 {
            g4!(bufs[0], t0, s0, o0);
            g4!(bufs[1], t1, s1, o1);
            g4!(bufs[2], t2, s2, o2);
            g4!(bufs[3], t3, s3, o3);
            r0 -= 4;
            r1 -= 4;
            r2 -= 4;
            r3 -= 4;
        }
        let mut top = [t0, t1, t2, t3];
        let mut st = [s0, s1, s2, s3];
        let mut op = [o0, o1, o2, o3];
        let rem = [r0, r1, r2, r3];
        for k in 0..4 {
            for _ in 0..rem[k] {
                let e = tbl[st[k]];
                unsafe { *ob.add(op[k]) = (e >> 8) as u8 };
                op[k] += 1;
                let nb = (e & 0xF) as u32;
                top[k] = top[k]
                    .checked_sub(nb as usize)
                    .ok_or(DecodeError::Truncated)?; // the stream ran dry before its symbols did
                let w = read_bits_at(&bufs[k], top[k]);
                st[k] = ((e >> 16) as usize) + (w & mask(nb)) as usize;
            }
        }
        // SAFETY: every symbol loop above ran to completion, so the four quarters — which partition
        // [0, n) — have all been written; `try_reserve` guaranteed the capacity. u8 has no invalid
        // bit patterns. Nothing before this point changed `dst`'s length.
        unsafe { dst.set_len(start + n) };
        Ok(())
    }

    // ── tans4o1 — the CONTEXT-SHAPED table family: o1 tANS as order-0 on a larger coordinate ──
    // `EntropyFoldReadout.context_table_is_the_fold`: instantiate the fold's State with
    // Register × PrevClass and `table_is_the_fold` is already about context-indexed families —
    // one table per (class, register) coordinate, same fold, identical code. The previous byte
    // maps through a rank-derived CLASS (transmitted, 256 B): full o1 would pay 256 norm headers
    // on a small residue stream, the classes pay C. Same 4-stream interleave
    // (`kway_stream_decode_exact`), each stream carrying its own running context.

    /// Number of context classes: the top `O1_C − 1` most frequent previous-bytes get their own
    /// class, the rest pool into class 0. The probe (2026-07-10, prose/code parse residue):
    /// C=32 captures ~10.4 of o1-full's 14% literal-bit saving at 1/8 the header cost.
    const O1_C: usize = 32;

    /// o1 compress: `[varint n][tableLog|0=RAW][256 B class map][per-class maxSym+norms]
    /// [4× varint bits][4 streams]`. Context = previous byte in stream order (class 0 at each
    /// sub-stream head — encoder and decoder agree).
    #[must_use]
    pub fn compress4_o1(data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let mut out = Vec::with_capacity(n / 2 + 64);
        put_varint(&mut out, n as u64);
        if n == 0 {
            return out;
        }
        if n < 4096 {
            out.push(0); // RAW escape — C norm tables need a real stream to amortize
            out.extend_from_slice(data);
            return out;
        }
        let table_log = TABLE_LOG;
        // rank-derived class map from the global o0 histogram
        let mut o0 = [0u32; 256];
        for &b in data {
            o0[usize::from(b)] += 1;
        }
        let mut rank: Vec<usize> = (0..256).collect();
        rank.sort_by_key(|&b| std::cmp::Reverse(o0[b]));
        let mut cls = [0u8; 256];
        for (r, &b) in rank.iter().enumerate() {
            if r < O1_C - 1 {
                cls[b] = (r + 1) as u8;
            }
        }
        // per-class histograms over (prev-class → symbol), sub-stream heads at class 0
        let q = n.div_ceil(4);
        let parts = [
            &data[..q],
            &data[q..2 * q],
            &data[2 * q..3 * q],
            &data[3 * q..],
        ];
        let mut hist = vec![[0u32; 256]; O1_C];
        for part in &parts {
            let mut c = 0usize;
            for &b in *part {
                hist[c][usize::from(b)] += 1;
                c = usize::from(cls[usize::from(b)]);
            }
        }
        out.push(table_log as u8);
        out.extend_from_slice(&cls);
        // build per-class tables; empty classes get a degenerate 1-symbol table
        let mut encs = Vec::with_capacity(O1_C);
        for h in &hist {
            let total: u32 = h.iter().sum();
            let norm = if total == 0 {
                let mut nz = [0u16; 256];
                nz[0] = 1 << table_log;
                nz
            } else {
                normalize(h, total, 1 << table_log)
            };
            let max_sym = (0..256).rev().find(|&s| norm[s] > 0).unwrap_or(0);
            out.push(max_sym as u8);
            for n in &norm[..=max_sym] {
                out.extend_from_slice(&n.to_le_bytes());
            }
            encs.push(build_enc(&norm, table_log));
        }
        // encode each stream in REVERSE: at position j the table is the class of part[j-1]
        let mut coded: Vec<(Vec<u8>, usize)> = Vec::with_capacity(4);
        for part in &parts {
            let mut bs = BitStack::new();
            bs.words.reserve(part.len() / 4 + 4);
            let mut value = 1u32 << table_log;
            for j in (0..part.len()).rev() {
                let ctx = if j == 0 {
                    0
                } else {
                    usize::from(cls[usize::from(part[j - 1])])
                };
                let (state_table, tt) = &encs[ctx];
                let sym = usize::from(part[j]);
                let nb = ((i64::from(value) + tt[sym].delta_nb_bits) >> 16) as u32;
                bs.push(u64::from(value) & mask(nb), nb);
                let idx = ((i64::from(value >> nb)) + tt[sym].delta_find_state) as usize;
                value = u32::from(state_table[idx]);
            }
            bs.push(u64::from(value), table_log);
            coded.push(bs.into_bytes());
        }
        for (_, bits) in &coded {
            put_varint(&mut out, *bits as u64);
        }
        for (bytes, _) in &coded {
            out.extend_from_slice(bytes);
        }
        out
    }

    /// Inverse of [`compress4_o1`] — four streams in one loop, each threading its own context
    /// through the class map; the table select rides the decoded symbol (the extended coordinate).
    /// Malformed input yields an empty `Vec` (never a panic); [`try_decompress4_o1`] says why.
    #[must_use]
    pub fn decompress4_o1(bytes: &[u8]) -> Vec<u8> {
        try_decompress4_o1(bytes).unwrap_or_default()
    }

    /// Checked [`decompress4_o1`].
    pub fn try_decompress4_o1(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") {
            return unsafe { decompress4_o1_bmi2(bytes) };
        }
        decompress4_o1_impl(bytes)
    }
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "bmi2")]
    unsafe fn decompress4_o1_bmi2(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        decompress4_o1_impl(bytes)
    }
    #[inline(always)]
    fn decompress4_o1_impl(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut p = 0usize;
        let n = get_len(bytes, &mut p)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        let table_log = u32::from(
            *take_slice(bytes, &mut p, 1)?
                .first()
                .ok_or(DecodeError::Truncated)?,
        );
        if table_log == 0 {
            return Ok(take_slice(bytes, &mut p, n)?.to_vec()); // RAW escape: bounded by the input
        }
        check_table_log(table_log)?;
        let tsize = 1usize << table_log;
        let mut cls = [0u8; 256];
        cls.copy_from_slice(take_slice(bytes, &mut p, 256)?);
        if cls.iter().any(|&c| usize::from(c) >= O1_C) {
            return Err(DecodeError::Malformed); // a class names one of the O1_C tables
        }
        // per-class packed decode tables, contiguous class-major — fused builds, one shared scratch
        let mut tbl = vec![0u32; O1_C * tsize];
        let mut scratch = Vec::new();
        for c in 0..O1_C {
            let max_sym = usize::from(
                *take_slice(bytes, &mut p, 1)?
                    .first()
                    .ok_or(DecodeError::Truncated)?,
            );
            let norm = read_norm(bytes, &mut p, max_sym, table_log)?;
            build_dec_packed(
                &norm,
                table_log,
                &mut scratch,
                &mut tbl[c * tsize..(c + 1) * tsize],
            );
            // REPACK to the fused o1 entry (EntropyFoldReadout.o1_context_fuses): the next context is
            // `cls[sym]`, a function of the emitted symbol — store it IN the entry so the hot loop READS
            // it, not recomputes it (the `ctx = cls[sym]` load was removable Action, ReadoutIsNotSearch).
            // Layout: sym[0..8] · nb[8..12] (nb ≤ table_log = 12) · nextClass[12..17] · base[17..29].
            for e in &mut tbl[c * tsize..(c + 1) * tsize] {
                let sym = ((*e >> 8) & 0xFF) as usize; // base layout is now nb[0..4]·sym[8..16]·base[16..]
                let nb = *e & 0xF;
                let base = *e >> 16;
                *e = (sym as u32) | (nb << 8) | (u32::from(cls[sym]) << 12) | (base << 17);
            }
        }
        let mut bits = [0usize; 4];
        for b in &mut bits {
            *b = get_len(bytes, &mut p)?;
            if *b < table_log as usize {
                return Err(DecodeError::Truncated); // each stream carries at least its initial state
            }
        }
        // A class table that owns all its slots costs no bits, so (as in `tans4`) only the output cap
        // bounds `n` by itself; otherwise the stream bits do.
        if n > DEFAULT_MAX_OUTPUT {
            return Err(DecodeError::TooLarge);
        }
        let q = n.div_ceil(4);
        if 3 * q > n {
            return Err(DecodeError::Malformed);
        }
        let mut bufs: [Vec<u8>; 4] = Default::default();
        for k in 0..4 {
            let blen = bits[k].div_ceil(8);
            let src = take_slice(bytes, &mut p, blen)?;
            let mut buf = try_vec(blen + 8)?;
            buf.extend_from_slice(src);
            buf.extend_from_slice(&[0u8; 8]);
            bufs[k] = buf;
        }
        let c3 = n - 3 * q;
        let mut out = super::try_zeroed(n)?;
        let mut top = [0usize; 4];
        let mut st = [0usize; 4];
        let mut ctx = [0usize; 4];
        for k in 0..4 {
            top[k] = bits[k] - table_log as usize;
            st[k] = (read_bits_at(&bufs[k], top[k]) & mask(table_log)) as usize;
        }
        let (mut o0, mut o1, mut o2, mut o3) = (0usize, q, 2 * q, 3 * q);
        // NOT constructed (DeterministicTrips's register boundary, measured +2.5% WORSE when
        // tried 2026-07-11): o1's context chain (symbol -> class load -> table address) adds a
        // live scalar per stream and lengthens the serial chain; with the register container the
        // pressure spills and the constructed shape loses. The plain interleaved loop stays.
        macro_rules! step {
            ($k:expr, $o:ident) => {{
                let e = tbl[(ctx[$k] << table_log) + st[$k]]; // shift, not runtime MUL on the addr chain
                out[$o] = e as u8;
                $o += 1;
                ctx[$k] = ((e >> 12) & 0x1F) as usize; // next class READ from the entry (o1_context_fuses)
                let nb = ((e >> 8) & 0xF) as u32;
                top[$k] = top[$k]
                    .checked_sub(nb as usize)
                    .ok_or(DecodeError::Truncated)?; // the stream ran dry before its symbols did
                let w = read_bits_at(&bufs[$k], top[$k]);
                st[$k] = ((e >> 17) as usize) + (w & mask(nb)) as usize;
            }};
        }
        for _ in 0..c3 {
            step!(0, o0);
            step!(1, o1);
            step!(2, o2);
            step!(3, o3);
        }
        for _ in c3..q {
            step!(0, o0);
            step!(1, o1);
            step!(2, o2);
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A near-uniform byte histogram used to panic in `build_enc` at `idx == table_size`.
        ///
        /// The path: `p.max(1)` clamps many low-frequency symbols UP to one slot each, `sum`
        /// overshoots `table_size`, and the single largest-symbol absorption cannot give back
        /// enough because its own `.max(1)` floor pins it. `normalize` then returned counts summing
        /// to MORE than the table, and `build_enc`'s cumulative start walked off `state_table`.
        ///
        /// The reproducer is the real one that found it: row-major interleaved `f32` bytes, whose
        /// exponent and mantissa bytes spread almost evenly over the alphabet.
        #[test]
        fn a_near_uniform_histogram_normalizes_within_the_table() {
            let mut f32_bytes: Vec<u8> = Vec::with_capacity(768);
            for i in 0..192u32 {
                // Values chosen to spread the mantissa bytes, not to mean anything.
                let v = (i as f32) * 1.000_123_4 + 0.5;
                f32_bytes.extend_from_slice(&v.to_le_bytes());
            }
            assert_eq!(f32_bytes.len(), 768);

            // The direct unit: normalize must never claim more slots than the table has.
            let mut hist = [0u32; 256];
            for &b in &f32_bytes {
                hist[b as usize] += 1;
            }
            for table_log in 8..=TABLE_LOG {
                let table_size = 1u32 << table_log;
                let norm = normalize(&hist, f32_bytes.len() as u32, table_size);
                let sum: u32 = norm.iter().map(|&n| u32::from(n)).sum();
                assert!(
                    sum <= table_size,
                    "table_log {table_log}: normalize claimed {sum} slots of {table_size}"
                );
                // Every symbol that occurs must still be representable.
                for s in 0..256 {
                    assert_eq!(
                        hist[s] > 0,
                        norm[s] > 0,
                        "table_log {table_log}: symbol {s} representability changed"
                    );
                }
                // The table it feeds must build without walking off its own end.
                let _ = build_enc(&norm, table_log);
            }

            // And the entry point that actually panicked: `compress` reaches tANS through the `lz`
            // face. Round-trip, so this guards correctness and not merely the absence of a panic.
            let packed = crate::codec::compress(&f32_bytes);
            assert_eq!(
                crate::codec::decompress(&packed),
                f32_bytes,
                "compress/decompress must round-trip a near-uniform f32 byte column"
            );
        }

        #[test]
        fn tans4_o1_roundtrip() {
            let mut cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8; 100], // RAW escape
                vec![9u8; 5000],
                b"the quick brown fox jumps over the lazy dog. ".repeat(400),
                (0..50000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(),
                (0..=255u8).cycle().take(20001).collect(),
            ];
            let mut s = 0x9999_1111u32;
            cases.push(
                (0..60000)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        if s % 3 == 0 {
                            b' '
                        } else {
                            b'a' + (s % 26) as u8
                        }
                    })
                    .collect(),
            );
            for c in &cases {
                assert_eq!(
                    &decompress4_o1(&compress4_o1(c)),
                    c,
                    "tans4o1 round-trip, len {}",
                    c.len()
                );
            }
        }

        #[test]
        fn tans4_roundtrip() {
            let mut cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8; 1],
                vec![9u8; 31], // RAW escape boundary
                vec![9u8; 32], // smallest coded
                vec![0u8; 5000],
                b"the quick brown fox jumps over the lazy dog. ".repeat(300),
                (0..40000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(),
                (0..=255u8).cycle().take(9001).collect(), // n % 4 = 1 (tail fixup path)
                (0..=255u8).cycle().take(9002).collect(),
                (0..=255u8).cycle().take(9003).collect(),
            ];
            let mut s = 0x1234_5678u32;
            cases.push(
                (0..50000)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        if s % 4 == 0 {
                            (s % 7) as u8
                        } else {
                            b'e'
                        }
                    })
                    .collect(),
            );
            for c in &cases {
                assert_eq!(
                    &decompress4(&compress4(c)),
                    c,
                    "tans4 round-trip, len {}",
                    c.len()
                );
            }
        }

        #[test]
        fn tans_roundtrip() {
            let mut cases: Vec<Vec<u8>> = vec![
                vec![],
                vec![7u8; 1],
                vec![0u8; 5000],
                b"the quick brown fox jumps over the lazy dog. ".repeat(300),
                (0..40000u32)
                    .map(|x| (x.wrapping_mul(2654435761) >> 24) as u8)
                    .collect(),
                (0..=255u8).cycle().take(9000).collect(),
            ];
            // a skewed distribution (few symbols dominate — where tANS earns its keep)
            let mut s = 0x1234_5678u32;
            cases.push(
                (0..50000)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        if s % 4 == 0 {
                            (s % 7) as u8
                        } else {
                            b'e'
                        }
                    })
                    .collect(),
            );
            for c in &cases {
                assert_eq!(
                    &decompress(&compress(c)),
                    c,
                    "tans round-trip, len {}",
                    c.len()
                );
            }
        }
    }
}

/// THE SPEED FACE — the `Web` coordinate primitive over the seed layout (storage = execution).
/// Access is an `O(1)` coordinate read (`Web.getNode`) into the decoded structure; the decode cost
/// is paid up front, not per access. Substrate-invariant by `preduceAt_schedule_free`.
#[cfg(feature = "codec")]
pub mod web {
    //! THE SPEED FACE — the `Web` coordinate primitive (`proof/Substrate/Algebra/Web/Web.lean`) over a flat
    //! seed buffer. NOT a hand-rolled rkyv clone: the access *is* `Web.getNode` — a coordinate read,
    //! O(1), no decode (storage = execution; the stored `.seed` bytes ARE the layout). The proven
    //! differentiators over rkyv/abomonation, which are CPU-RAM-only and unverified:
    //!
    //!   * **substrate-invariant** (`Web.preduceAt_schedule_free`): the reduce is invariant under any
    //!     relabeling of which compute unit handles which coordinate — cores / GPU lanes / federated
    //!     machines are a *free* knob, the answer unchanged. Measured operationally: the same emit
    //!     program retargeted CPU→GPU at 591× (RTX 5080), cross-ISA verified (x86/ARM64/RISC-V).
    //!   * **independent reads** (`Web.node_independent`): node `i`'s read is invisible to `j ≠ i`, so
    //!     reads parallelize with no interference — and each can be `pread` at its own coordinate from
    //!     storage without a whole-blob load (the streaming law; measured .seed RAM 9.4→5.1 GB).
    //!   * **O(1) validation = encircle** (`Web.encircle_iff`), not a structural walk: validity is a
    //!     constant invariant (length identity + alignment), and the per-node floor check is the
    //!     residual reduce — the "regex→O(1)" of buffer validation.
    //!
    //! The one cut to the metal — viewing bytes as a value at a coordinate — is confined to the
    //! documented `unsafe` here (Axiom XI: the cast IS the syscall-class cut). Native-endian payload,
    //! the cost of zero copy (rkyv without its endian feature is the same).

    use core::marker::PhantomData;
    use core::mem::{align_of, size_of};

    /// A plain-old-data node state: fixed `#[repr(C)]` layout, no padding bytes, no pointers, every
    /// bit-pattern a valid value (so validation is the constant length+alignment check, never a walk).
    ///
    /// # Safety
    /// Implementors MUST be `#[repr(C)]` (or `repr(transparent)`), contain only `Pod` fields with no
    /// padding, hold no pointers/references, and admit every bit-pattern of their size. Then casting
    /// `align`-correct, correctly-sized bytes to `&[Self]` is sound.
    pub unsafe trait Pod: Copy + 'static {}

    macro_rules! pod_prims { ($($t:ty),*) => { $( unsafe impl Pod for $t {} )* } }
    pod_prims!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);
    // Byte arrays are Pod (every bitpattern valid, align 1, no padding) — the derive packs a record's
    // scalar fields into a fixed `[u8; N]` block, stored as `Wire<[u8; N]>`.
    unsafe impl<const N: usize> Pod for [u8; N] {}

    /// View a `Pod` value as its bytes — the one cut (reinterpret). The derive uses it to pack a
    /// record's scalar fields (primitives AND nested POD structs, uniformly) into the byte block.
    #[must_use]
    pub fn pod_bytes<T: Pod>(v: &T) -> &[u8] {
        // SAFETY: T: Pod ⇒ all size_of::<T>() bytes are initialised (no padding); immutable, tied to v.
        unsafe { core::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()) }
    }

    /// Read a `Pod` value out of (possibly unaligned) bytes — copies `size_of::<T>()` bytes.
    #[must_use]
    pub fn pod_read<T: Pod>(b: &[u8]) -> T {
        assert!(b.len() >= size_of::<T>(), "pod_read: short buffer");
        // SAFETY: length checked; T: Pod ⇒ any bitpattern is a valid value; read_unaligned copies.
        unsafe { b.as_ptr().cast::<T>().read_unaligned() }
    }

    /// Why a buffer is not a valid `Wire<T>` (all O(1) to detect).
    #[derive(Debug, PartialEq, Eq)]
    pub enum WireErr {
        TooShort,
        Misaligned,
        BadLength,
        BadAlignType,
        /// An enum column holds a tag no variant owns (derive-generated views).
        BadTag,
        /// A `String` column holds bytes that are not valid UTF-8 (derive-generated views).
        BadUtf8,
    }

    /// An owned, 8-aligned seed buffer: `[count: u64 LE][T; count]`. The serialised form IS the runtime
    /// layout (storage = execution); `pack` is a memcpy of the node states (the `idFloor` — exact, no
    /// compression: this is the speed face, mutually exclusive with the entropy-coded size face).
    pub struct Seed {
        words: Vec<u64>, // 8-byte-aligned backing store
        len: usize,      // valid byte length
    }

    impl Default for Seed {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Seed {
        /// An empty, reusable seed buffer — pass to [`pack_into`] in a loop to amortize allocation.
        #[must_use]
        pub fn new() -> Self {
            Seed {
                words: Vec::new(),
                len: 0,
            }
        }

        /// The seed bytes — 8-aligned, ready to `Wire::open` with zero copy, or `pread` by coordinate.
        #[must_use]
        pub fn as_bytes(&self) -> &[u8] {
            // SAFETY: `words` owns `nwords*8 ≥ len` bytes, 8-aligned (Vec<u64>); lifetime tied to &self.
            unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.len) }
        }
    }

    /// Pack a node-state slice into a fresh seed buffer (`Web` node states by coordinate). Requires
    /// `align_of::<T>() ≤ 8`; a memcpy, no per-element work — the fast serialise.
    #[must_use]
    pub fn pack<T: Pod>(vals: &[T]) -> Seed {
        let mut seed = Seed::new();
        pack_into(&mut seed, vals);
        seed
    }

    /// Serialize into a **reusable** buffer — amortizes the allocation across calls (rkyv's arena
    /// pattern) AND writes directly: NO `vec![0; n]` zero-init memset before the memcpy (that doubled
    /// the write traffic). This is the fix for the serialize gap to rkyv's `to_bytes_in`.
    pub fn pack_into<T: Pod>(seed: &mut Seed, vals: &[T]) {
        assert!(
            align_of::<T>() <= 8 && 8 % align_of::<T>() == 0,
            "Pod align must divide 8"
        );
        let nbytes = 8 + std::mem::size_of_val(vals);
        let nwords = nbytes.div_ceil(8).max(1);
        if seed.words.capacity() < nwords {
            seed.words = Vec::with_capacity(nwords);
        }
        // SAFETY: capacity ≥ nwords; u64 is plain bytes (no drop). We zero the tail word (so its ≤7
        // padding bytes are defined) and then write [0, nbytes) = count + data; `as_bytes` exposes only
        // [0, nbytes), and words[0..nwords-1] are fully covered since (nwords-1)*8 < nbytes — no uninit.
        unsafe {
            seed.words.set_len(nwords);
        }
        seed.words[nwords - 1] = 0;
        seed.len = nbytes;
        // SAFETY: words holds nwords*8 ≥ nbytes bytes, 8-aligned; write exactly [0, nbytes).
        let dst = unsafe {
            core::slice::from_raw_parts_mut(seed.words.as_mut_ptr().cast::<u8>(), nbytes)
        };
        dst[0..8].copy_from_slice(&(vals.len() as u64).to_le_bytes());
        // SAFETY: T: Pod ⇒ every one of its `size` bytes is initialised (no padding); read as bytes.
        let src = unsafe {
            core::slice::from_raw_parts(vals.as_ptr().cast::<u8>(), std::mem::size_of_val(vals))
        };
        dst[8..].copy_from_slice(src);
    }

    /// A validated zero-copy view over seed bytes — the `Web` of node states by coordinate. `open` is
    /// the O(1) encircle (length+alignment invariant); `get`/`as_slice` are the coordinate reads.
    pub struct Wire<'a, T: Pod> {
        data: &'a [u8], // the T payload (after the 8-byte count header)
        count: usize,
        _t: PhantomData<&'a [T]>,
    }

    impl<'a, T: Pod> Wire<'a, T> {
        /// O(1) validation — the `encircle` structural invariant: header present, base 8-aligned (so the
        /// payload is `align_of::<T>()`-aligned), and the length identity `8 + count·size == len` holds.
        /// No per-element walk; this is rkyv's bytecheck collapsed to a constant check.
        pub fn open(buf: &'a [u8]) -> Result<Self, WireErr> {
            if align_of::<T>() > 8 || 8 % align_of::<T>() != 0 {
                return Err(WireErr::BadAlignType);
            }
            if buf.len() < 8 {
                return Err(WireErr::TooShort);
            }
            if (buf.as_ptr() as usize) % 8 != 0 {
                return Err(WireErr::Misaligned);
            }
            let mut hdr = [0u8; 8];
            hdr.copy_from_slice(&buf[0..8]);
            let count = u64::from_le_bytes(hdr) as usize;
            let size = size_of::<T>();
            match count.checked_mul(size).and_then(|b| b.checked_add(8)) {
                Some(total) if total == buf.len() => Ok(Wire {
                    data: &buf[8..],
                    count,
                    _t: PhantomData,
                }),
                _ => Err(WireErr::BadLength),
            }
        }

        /// The node states as a borrowed slice — the coordinate cast, O(1), zero copy.
        #[must_use]
        pub fn as_slice(&self) -> &'a [T] {
            // SAFETY: `open` checked the base is 8-aligned and `align_of::<T>()` divides 8, so `data` is
            // `T`-aligned; and `data.len() == count·size_of::<T>()`. `T: Pod` ⇒ every bit-pattern valid.
            unsafe { core::slice::from_raw_parts(self.data.as_ptr().cast::<T>(), self.count) }
        }

        /// `Web.getNode` — read node `i` by its coordinate, O(1), no decode.
        #[must_use]
        pub fn get(&self, i: usize) -> &'a T {
            &self.as_slice()[i]
        }

        #[must_use]
        pub fn len(&self) -> usize {
            self.count
        }

        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.count == 0
        }

        /// `Web.preduce` — map each node to a commutative monoid and combine. **Schedule-free**
        /// (`preduceAt_schedule_free`): the order of combination is free, so this fold parallelises
        /// across cores/GPU/federated units (chunk by `node_independent`) with the same result — the
        /// substrate is a free knob. (Sequential here; the proof is the license to partition it.)
        pub fn preduce<M>(
            &self,
            zero: M,
            lift: impl Fn(&T) -> M,
            combine: impl Fn(M, M) -> M,
        ) -> M {
            let mut acc = zero;
            for x in self.as_slice() {
                acc = combine(acc, lift(x));
            }
            acc
        }

        /// `Web.encircle_iff` — every node rests at its floor iff the (non-negative) residual reduce is
        /// zero. The semantic O(1)-per-node validation, parallel: the whole web closes on the flaw at
        /// once, never a sequential search.
        pub fn encircle(&self, residual: impl Fn(&T) -> f64) -> bool {
            self.as_slice().iter().all(|x| residual(x) == 0.0)
        }

        /// `Web.sweep` — the edge-reading COUPLE, the B/U middle face (§2b) between `pmap` (self, `map`)
        /// and `preduce` (collapse, `reduce`). Every node updates at once from its own state AND its
        /// neighbours' states, read off THIS immutable snapshot — synchronous: reads never see the
        /// sweep's own writes, which is exactly why it parallelises (`node_independent`). `adj(i)` gives
        /// node `i`'s neighbour coordinates; `merge(i, self_i, neighbours)` is its new state — the
        /// attention / message-passing layer as a web move (softmax = `softmax_is_bulk`). Returns a
        /// fresh `Seed` of new node states; the snapshot is untouched.
        ///
        /// Depth is GENERATIVE (`Web.sweep_depth_is_radius`): `k` sweeps reach radius `k`, so information
        /// crosses the graph in hops — unlike `pmap`, whose depth collapses (`pmapAt_comp`). Map+reduce
        /// is the dichotomy; map+**sweep**+reduce is the rule of three.
        ///
        /// `adj(i)` is a **contiguous coordinate range** — the neighbourhood as a slice of the snapshot,
        /// so the merge reads it with ZERO per-node allocation. Causal attention is `0..i+1`, a window is
        /// `i.saturating_sub(w)..i+1`; a sparse/knot neighbourhood is the gathered (compacted) kept set,
        /// contiguous by construction. `merge(i, self_i, &neighbours)` is the new state — attention /
        /// message-passing, softmax = `softmax_is_bulk`.
        pub fn sweep(
            &self,
            adj: impl Fn(usize) -> core::ops::Range<usize>,
            merge: impl Fn(usize, &T, &[T]) -> T,
        ) -> Seed {
            let src = self.as_slice();
            let out: Vec<T> = (0..src.len())
                .map(|i| merge(i, &src[i], &src[adj(i)]))
                .collect();
            pack(&out)
        }
    }

    /// **Register-resident validation — the bytecheck done O(1) per coordinate** (the lesson from the
    /// 18.5 GB/s XOR-compare). The "model" — the valid byte range `[lo, hi]` — lives in registers; the
    /// test is a *branchless* compare (`b - lo > hi - lo` ⇒ out of range), the positions are
    /// independent, and there is NO early-exit branch and NO table — so the loads pipeline and the
    /// compiler SIMD-vectorizes it to memory bandwidth. The opposite of the DFA/hash-table walk whose
    /// L2 gather is the latency-bound drag. This is the "regex→O(1)" validation (e.g. enum tags in
    /// `[0,K)`, ASCII fields) for typed/nested buffers, register-resident, no gather, no carried state.
    #[must_use]
    pub fn all_in_range(bytes: &[u8], lo: u8, hi: u8) -> bool {
        let span = hi.wrapping_sub(lo);
        let mut bad = 0u8;
        for &b in bytes {
            bad |= u8::from(b.wrapping_sub(lo) > span); // branchless; OR-reduce, no early exit
        }
        bad == 0
    }

    /// **`Web::extend` — the longest common extension: the RUN, the Being face of the suffix web.**
    ///
    /// From two coordinates `i` (the current position) and `j` (an earlier occurrence, `j < i`) in a byte
    /// web, return how far their FORWARD spans agree — the length of the **null cone** (`a = b`, `NCell`'s
    /// `a² − b² = 0`) held until the **NB flaw** (`NB = 0`, the first byte that differs). This is the one
    /// piece an LZ back-reference is missing from the trinity we already have: [`crate::CoordMap`] gives
    /// the source coordinate (the **B/U** offset — "where did this context occur before"), `extend` gives
    /// how far the **monad** carries (`1·s = s`, the copy), and the flaw ends the run. A match of length
    /// `L` is then ONE token that advances `L` — O(1) amortized per run, the LZ throughput class the
    /// per-bit context coder cannot reach, because it never re-reads the copied bytes.
    ///
    /// Word-at-a-time: XOR two 8-byte reads; a nonzero XOR means the flaw is inside this word, at byte
    /// `trailing_zeros/8` (little-endian) — no per-byte branch on the hot path, so it runs at memory
    /// bandwidth. Overlapping spans (`j + L > i`, the RLE case `"aaaa"`) are read straight from the
    /// already-present buffer, so the extension is just the common prefix — exactly right.
    #[must_use]
    pub fn extend(a: &[u8], i: usize, j: usize) -> usize {
        let n = a.len();
        let (mut p, mut q) = (i, j);
        while p + 8 <= n && q + 8 <= n {
            let x = u64::from_le_bytes(a[p..p + 8].try_into().unwrap());
            let y = u64::from_le_bytes(a[q..q + 8].try_into().unwrap());
            let d = x ^ y;
            if d != 0 {
                return (p - i) + (d.trailing_zeros() as usize >> 3); // the flaw: first differing byte
            }
            p += 8;
            q += 8;
        }
        while p < n && q < n && a[p] == a[q] {
            p += 1;
            q += 1;
        }
        p - i
    }

    // ════════════════════════════════════════════════════════════════════════════
    // NESTED / VARIABLE-LENGTH — the (skeleton, leaves) scatter-gather layout (interformat_is_zero_copy)
    // ════════════════════════════════════════════════════════════════════════════
    // The inter-format proof showed a serialised Term factors into (skeleton, payload). Here that is
    // operational for variable-length data: the OFFSETS are the skeleton (the gather descriptor / iovec
    // — built once, format-invariant), the concatenated item bytes are the scattered leaf buffers.
    // Access is a coordinate read into the offset array → a zero-copy SUB-SLICE, O(1), NO coalescing
    // copy. This is how nested types (strings, `Vec<T>`, variable records — the benchmark's `log`/`mk48`)
    // get the same zero-copy access as POD. Native-endian, the cost of zero copy.

    /// Pack a ragged array (variable-length items) into a seed buffer.
    /// Layout: `[n: u64][off₀ … off_n : u64]` (n+1 offsets, off₀=0, off_n = leaves length) `[leaves…]`.
    #[must_use]
    // Deliberate uninit backing store: `Vec<u64>` (no drop), fully overwritten in [0, nbytes); only the
    // ≤7 tail-padding bytes stay undefined and `as_bytes` never exposes them (see the SAFETY note below).
    #[allow(clippy::uninit_vec)]
    pub fn pack_ragged(items: &[&[u8]]) -> Seed {
        let n = items.len();
        let header_words = 1 + (n + 1); // count + (n+1) offsets, all u64 → 8-aligned leaves start
        let leaves_len: usize = items.iter().map(|s| s.len()).sum();
        let nbytes = header_words * 8 + leaves_len;
        let nwords = nbytes.div_ceil(8).max(1);
        // No full-buffer memset (it would double the write traffic over the leaves bulk). The header
        // words [0, header_words) are all set below and the leaves region [header_words*8, nbytes) is
        // fully overwritten — leaves are contiguous (no inter-item gaps), so the ONLY undefined bytes are
        // the ≤7 tail-padding bytes of the last word, which `as_bytes` (len = nbytes) never exposes.
        let mut words: Vec<u64> = Vec::with_capacity(nwords);
        // SAFETY: capacity ≥ nwords; u64 is plain bytes (no drop). We zero the tail word (so its padding
        // is defined), set the full header, and overwrite the entire leaves region — no uninit in [0, nbytes).
        unsafe {
            words.set_len(nwords);
        }
        words[nwords - 1] = 0;
        words[0] = n as u64;
        let mut off = 0u64;
        for (k, it) in items.iter().enumerate() {
            words[1 + k] = off;
            off += it.len() as u64;
        }
        words[1 + n] = off;
        // SAFETY: `words` owns nwords*8 ≥ nbytes bytes, 8-aligned; we write exactly the leaves region.
        let dst =
            unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), nbytes) };
        let mut p = header_words * 8;
        for it in items {
            dst[p..p + it.len()].copy_from_slice(it);
            p += it.len();
        }
        Seed { words, len: nbytes }
    }

    /// A validated zero-copy view of a ragged array — the (skeleton=offsets, leaves=data) scatter-gather.
    pub struct Ragged<'a> {
        offs: &'a [u64], // the gather descriptor (iovec): n+1 byte-offsets into `leaves`
        leaves: &'a [u8],
    }

    impl<'a> Ragged<'a> {
        /// Open — boundary invariant (`off₀=0`, `off_n=leaves.len()`) + alignment + the offsets are
        /// monotone (one O(n) pass over the descriptor, not the payload). Every offset is therefore
        /// within the leaves, so [`Self::get`] cannot panic for an in-range index, even on untrusted
        /// bytes.
        pub fn open(buf: &'a [u8]) -> Result<Self, WireErr> {
            if buf.len() < 8 {
                return Err(WireErr::TooShort);
            }
            if (buf.as_ptr() as usize) % 8 != 0 {
                return Err(WireErr::Misaligned);
            }
            // SAFETY: 8-aligned, ≥8 bytes; read the count word (native-endian, the zero-copy convention).
            let n = unsafe { *buf.as_ptr().cast::<u64>() } as usize;
            let off_bytes = n
                .checked_add(1)
                .and_then(|c| c.checked_mul(8))
                .ok_or(WireErr::BadLength)?;
            let header = off_bytes.checked_add(8).ok_or(WireErr::BadLength)?;
            if buf.len() < header {
                return Err(WireErr::BadLength);
            }
            // SAFETY: buf is 8-aligned, the offset region [8, header) holds n+1 u64s in bounds.
            let offs =
                unsafe { core::slice::from_raw_parts(buf.as_ptr().add(8).cast::<u64>(), n + 1) };
            let leaves = &buf[header..];
            if offs[0] != 0 || offs[n] as usize != leaves.len() {
                return Err(WireErr::BadLength);
            }
            let r = Ragged { offs, leaves };
            if !r.validate() {
                return Err(WireErr::BadLength);
            }
            Ok(r)
        }

        #[must_use]
        pub fn len(&self) -> usize {
            self.offs.len() - 1
        }

        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }

        /// Item `i` — a zero-copy sub-slice into the leaf buffer (coordinate read, O(1), no copy).
        #[must_use]
        pub fn get(&self, i: usize) -> &'a [u8] {
            &self.leaves[self.offs[i] as usize..self.offs[i + 1] as usize]
        }

        /// Item `i` cast to `&[T]` (zero-copy) when its bytes are `T`-aligned and a whole number of `T`;
        /// `None` otherwise (then copy). Typed nested access without decode.
        #[must_use]
        pub fn get_as<T: Pod>(&self, i: usize) -> Option<&'a [T]> {
            let s = self.get(i);
            let sz = size_of::<T>();
            if sz == 0 || s.len() % sz != 0 || (s.as_ptr() as usize) % align_of::<T>() != 0 {
                return None;
            }
            // SAFETY: checked alignment and exact multiple of size_of::<T>(); T: Pod (any bitpattern ok).
            Some(unsafe { core::slice::from_raw_parts(s.as_ptr().cast::<T>(), s.len() / sz) })
        }

        /// The register-resident bytecheck for untrusted input: the offsets are monotone and in range.
        /// O(1) per coordinate (a branchless `≤` over the descriptor), not a walk of the payload.
        #[must_use]
        pub fn validate(&self) -> bool {
            let mut ok = true;
            for w in self.offs.windows(2) {
                ok &= w[0] <= w[1];
            }
            ok && (*self.offs.last().unwrap() as usize) <= self.leaves.len()
        }
    }

    // ════════════════════════════════════════════════════════════════════════════
    // COMPOSITION — a nested record table = scalar Wire + a Ragged per variable field, one buffer
    // ════════════════════════════════════════════════════════════════════════════
    // The (skeleton, leaves) factoring, nested: a `Vec<Record>` where Record = fixed scalars + M
    // variable fields becomes M+1 columns — one scalar `Wire` (section 0) and one `Ragged` per variable
    // field (sections 1..) — concatenated into ONE buffer with 8-aligned section starts. Access record
    // `i` = `wire.get(i)` + `ragged_j.get(i)`: all zero-copy coordinate reads, O(1), no decode, no
    // coalescing. This is the columnar zero-copy layout, built from the two primitives.

    /// Concatenate self-contained sub-buffers (each a `Wire`/`Ragged`) into one buffer with 8-aligned
    /// section starts. Layout: `[n: u64][len₀…len_{n-1} : u64][§0 (8-aligned)][§1 (8-aligned)]…`.
    /// Header: `[n: u64][start₀…start_{n-1} : u64][len₀…len_{n-1} : u64]` — starts AND lens stored, so
    /// `Sections::open` BORROWS them (zero allocation, O(1)) rather than recomputing per node. At depth
    /// (many opens) that allocation was the drag vs rkyv's relative-pointer deref.
    #[must_use]
    #[allow(clippy::uninit_vec)] // `Vec<u64>`, no drop, fully overwritten in [0, nbytes); see SAFETY below.
    pub fn pack_sections(sections: &[&[u8]]) -> Seed {
        let n = sections.len();
        let header = 8 * (1 + 2 * n);
        let mut starts = Vec::with_capacity(n);
        let mut pos = header;
        for s in sections {
            pos = (pos + 7) & !7; // 8-align each section start so it opens zero-copy
            starts.push(pos);
            pos += s.len();
        }
        let total = pos;
        let nwords = total.div_ceil(8).max(1);
        // No full-buffer memset over the section bulk. as_bytes (len = total) exposes header + sections +
        // the ≤7-byte 8-alignment GAPS between sections, so only the gaps need zeroing (the sections are
        // fully overwritten). Zero the tail word FIRST (defensive; before the header writes, which sit in
        // the low words and would otherwise be clobbered when total ≤ header).
        let mut words: Vec<u64> = Vec::with_capacity(nwords);
        // SAFETY: capacity ≥ nwords; u64 is plain bytes (no drop). Tail word + header + every gap + every
        // section are all written below ⇒ no uninit byte in [0, total).
        unsafe {
            words.set_len(nwords);
        }
        words[nwords - 1] = 0;
        words[0] = n as u64;
        for (j, s) in sections.iter().enumerate() {
            words[1 + j] = starts[j] as u64;
            words[1 + n + j] = s.len() as u64;
        }
        // SAFETY: words holds total≥ bytes, 8-aligned; write each section at its 8-aligned start.
        let dst =
            unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), total) };
        let mut prev_end = header; // header region is fully set via the words[] writes above
        for (j, s) in sections.iter().enumerate() {
            for b in &mut dst[prev_end..starts[j]] {
                *b = 0; // the alignment gap (≤7 bytes) — exposed by as_bytes, never an opened section
            }
            dst[starts[j]..starts[j] + s.len()].copy_from_slice(s);
            prev_end = starts[j] + s.len();
        }
        Seed { words, len: total }
    }

    /// A zero-copy view of a sectioned buffer — each `section(j)` is an 8-aligned sub-slice ready to
    /// `Wire::open` / `Ragged::open`. Open BORROWS the stored offsets — zero allocation, so recursive
    /// (deep-nested) navigation pays no per-node alloc.
    pub struct Sections<'a> {
        buf: &'a [u8],
        starts: &'a [u64],
        lens: &'a [u64],
    }

    impl<'a> Sections<'a> {
        pub fn open(buf: &'a [u8]) -> Result<Self, WireErr> {
            if buf.len() < 8 || (buf.as_ptr() as usize) % 8 != 0 {
                return Err(WireErr::Misaligned);
            }
            // SAFETY: 8-aligned, ≥8 bytes; read the section count.
            let n = unsafe { *buf.as_ptr().cast::<u64>() } as usize;
            let header = n
                .checked_mul(2)
                .and_then(|c| c.checked_add(1))
                .and_then(|c| c.checked_mul(8))
                .ok_or(WireErr::BadLength)?;
            if buf.len() < header {
                return Err(WireErr::BadLength);
            }
            // SAFETY: starts at [8, 8+8n), lens at [8+8n, 8+16n) — both n u64s in bounds, 8-aligned.
            let starts =
                unsafe { core::slice::from_raw_parts(buf.as_ptr().add(8).cast::<u64>(), n) };
            let lens = unsafe {
                core::slice::from_raw_parts(buf.as_ptr().add(8 + 8 * n).cast::<u64>(), n)
            };
            for j in 0..n {
                let s = starts[j] as usize;
                if s % 8 != 0
                    || (s as u64)
                        .checked_add(lens[j])
                        .map_or(true, |e| e as usize > buf.len())
                {
                    return Err(WireErr::BadLength);
                }
            }
            Ok(Sections { buf, starts, lens })
        }

        #[must_use]
        pub fn len(&self) -> usize {
            self.starts.len()
        }

        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.starts.is_empty()
        }

        /// Section `j` — an 8-aligned sub-slice, ready to `Wire::open` / `Ragged::open` (zero-copy).
        #[must_use]
        pub fn section(&self, j: usize) -> &'a [u8] {
            &self.buf[self.starts[j] as usize..(self.starts[j] + self.lens[j]) as usize]
        }
    }

    /// Build a nested-record table in ONE aligned buffer, ONE pass — no intermediate `Seed`s, no
    /// re-copy, no full-buffer memset (the serialize-gap fix for the composition). Section 0 = the
    /// scalar `Wire`; sections 1.. = a `Ragged` per variable field (`var_fields[k][i]` = field k of
    /// record i). Open with [`Sections`] then `Wire`/`Ragged` per section, exactly as `pack_sections`.
    #[must_use]
    #[allow(clippy::uninit_vec)] // `Vec<u64>`, no drop, fully overwritten in [0, nbytes); see SAFETY below.
    pub fn pack_record_table<S: Pod>(scalars: &[S], var_fields: &[&[&[u8]]]) -> Seed {
        assert!(
            align_of::<S>() <= 8 && 8 % align_of::<S>() == 0,
            "Pod align must divide 8"
        );
        let n = scalars.len();
        let _ssize = size_of::<S>();
        let nsec = 1 + var_fields.len();
        let mut sec_len = Vec::with_capacity(nsec);
        sec_len.push(8 + std::mem::size_of_val(scalars)); // scalar Wire: [count][data]
        for f in var_fields {
            let leaves: usize = f.iter().map(|s| s.len()).sum();
            sec_len.push(8 + (n + 1) * 8 + leaves); // Ragged: [n][off₀..off_n][leaves]
        }
        let header = 8 * (1 + 2 * nsec); // count + n starts + n lens (matches pack_sections)
        let mut start = Vec::with_capacity(nsec);
        let mut pos = header;
        for &l in &sec_len {
            pos = (pos + 7) & !7;
            start.push(pos);
            pos += l;
        }
        let total = pos;
        let nwords = total.div_ceil(8).max(1);
        let mut words: Vec<u64> = Vec::with_capacity(nwords);
        // SAFETY: capacity ≥ nwords; u64 is plain bytes. We write the full header + every section; the
        // ≤7-byte inter-section gaps and tail are never read (Sections::open reads only the header and
        // each [start, start+len)), so leaving them uninitialised is sound — and avoids the memset.
        unsafe {
            words.set_len(nwords);
        }
        let dst =
            unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), total) };
        dst[0..8].copy_from_slice(&(nsec as u64).to_le_bytes());
        for j in 0..nsec {
            dst[8 + j * 8..16 + j * 8].copy_from_slice(&(start[j] as u64).to_le_bytes());
            dst[8 + nsec * 8 + j * 8..16 + nsec * 8 + j * 8]
                .copy_from_slice(&(sec_len[j] as u64).to_le_bytes());
        }
        // section 0 — scalar Wire (count + the scalar bytes, one memcpy)
        let s0 = start[0];
        dst[s0..s0 + 8].copy_from_slice(&(n as u64).to_le_bytes());
        // SAFETY: S: Pod ⇒ all bytes initialised; copy the scalar slice as bytes.
        let src = unsafe {
            core::slice::from_raw_parts(
                scalars.as_ptr().cast::<u8>(),
                std::mem::size_of_val(scalars),
            )
        };
        dst[s0 + 8..s0 + 8 + std::mem::size_of_val(scalars)].copy_from_slice(src);
        // sections 1.. — one Ragged per variable field (offsets then leaves, written in place)
        for (k, f) in var_fields.iter().enumerate() {
            let sj = start[1 + k];
            dst[sj..sj + 8].copy_from_slice(&(n as u64).to_le_bytes());
            let mut off = 0u64;
            let mut p = sj + 8;
            for it in *f {
                dst[p..p + 8].copy_from_slice(&off.to_le_bytes());
                off += it.len() as u64;
                p += 8;
            }
            dst[p..p + 8].copy_from_slice(&off.to_le_bytes());
            p += 8;
            for it in *f {
                dst[p..p + it.len()].copy_from_slice(it);
                p += it.len();
            }
        }
        Seed { words, len: total }
    }

    // ════════════════════════════════════════════════════════════════════════════
    // THE ONE-PASS ARENA — size (measure, no alloc) + emit (write in place)
    // ════════════════════════════════════════════════════════════════════════════
    // `pack_ragged`/`pack_sections` each allocate a fresh `Seed`; composing them for a nested record
    // allocates one `Seed` per node (O(nodes) malloc/mmap). The arena split fixes that: `*_size` is pure
    // arithmetic over the structure, so a parent can sum its children's sizes, make ONE allocation, and
    // have every child `*_emit` into its slot. Same byte layout as the `pack_*` forms (so the views open
    // unchanged); the difference is who allocates — one buffer for the whole tree, not one per node.

    /// Bytes the section-table layout (`pack_sections`) produces for sections of the given sizes.
    #[must_use]
    pub fn sections_size(sizes: &[usize]) -> usize {
        let n = sizes.len();
        let mut pos = 8 * (1 + 2 * n);
        for &s in sizes {
            pos = (pos + 7) & !7;
            pos += s;
        }
        pos
    }

    /// Write the section-table header into `dst` (count + starts + lens), zero the ≤7-byte inter-section
    /// alignment gaps (exposed by `as_bytes`), and return each section's 8-aligned start. The caller then
    /// writes each section's bytes at `starts[j]`. The in-place, one-allocation form of `pack_sections`.
    pub fn emit_sections_header(dst: &mut [u8], sizes: &[usize]) -> Vec<usize> {
        let n = sizes.len();
        let header = 8 * (1 + 2 * n);
        let mut starts = Vec::with_capacity(n);
        let mut pos = header;
        for &s in sizes {
            pos = (pos + 7) & !7;
            starts.push(pos);
            pos += s;
        }
        dst[0..8].copy_from_slice(&(n as u64).to_le_bytes());
        for j in 0..n {
            dst[8 + j * 8..16 + j * 8].copy_from_slice(&(starts[j] as u64).to_le_bytes());
            dst[8 + n * 8 + j * 8..16 + n * 8 + j * 8]
                .copy_from_slice(&(sizes[j] as u64).to_le_bytes());
        }
        let mut prev = header;
        for j in 0..n {
            for b in &mut dst[prev..starts[j]] {
                *b = 0;
            }
            prev = starts[j] + sizes[j];
        }
        starts
    }

    /// Bytes a ragged array of items with the given byte-lengths occupies.
    #[must_use]
    pub fn ragged_size(item_lens: &[usize]) -> usize {
        let n = item_lens.len();
        8 * (1 + (n + 1)) + item_lens.iter().sum::<usize>()
    }

    /// Write a ragged array (`[n][off₀..off_n][leaves]`) into `dst` (len == `ragged_size`). The in-place,
    /// one-allocation form of `pack_ragged` (no inter-item gaps, so nothing to zero).
    pub fn emit_ragged(dst: &mut [u8], items: &[&[u8]]) {
        let n = items.len();
        let header = 8 * (1 + (n + 1));
        dst[0..8].copy_from_slice(&(n as u64).to_le_bytes());
        let mut off = 0u64;
        for (k, it) in items.iter().enumerate() {
            dst[8 + k * 8..16 + k * 8].copy_from_slice(&off.to_le_bytes());
            off += it.len() as u64;
        }
        dst[8 + n * 8..16 + n * 8].copy_from_slice(&off.to_le_bytes());
        let mut p = header;
        for it in items {
            dst[p..p + it.len()].copy_from_slice(it);
            p += it.len();
        }
    }

    /// The element of a `Vec<U>` column — the compile-time dispatch that lets `#[derive(Collapse)]`
    /// handle `Vec<U>` whether `U` is `Pod` (→ a `Wire`, viewed as `&[U]`) or itself a `Collapse`-derived
    /// record (→ a sub-table, viewed as `UView`). The derive emits the `Collapse` arm; the blanket below
    /// gives the `Pod` arm. This is what makes nesting recurse to any depth through the derive — and, via
    /// `collapse_size`/`collapse_emit`, in ONE allocation for the whole tree (no per-node `Seed`).
    pub trait CollapseElem: Sized {
        /// The zero-copy view of a packed `&[Self]`: `&'a [Self]` for `Pod`, `SelfView<'a>` for records.
        type View<'a>;
        /// Bytes the packed `&[Self]` sub-buffer occupies (8-aligned total) — pure arithmetic, NO alloc.
        fn collapse_size(items: &[Self]) -> usize;
        /// Write the packed form into `dst` (len == `collapse_size`, base 8-aligned). Children write in
        /// place at their offsets — no intermediate `Seed`.
        fn collapse_emit(items: &[Self], dst: &mut [u8]);
        /// Open a packed sub-buffer to its zero-copy view.
        fn collapse_open(bytes: &[u8]) -> Result<Self::View<'_>, WireErr>;
        /// ONE 8-aligned allocation (sized by `collapse_size`), filled by `collapse_emit`. The composition
        /// of `size`+`emit` across the derive means the whole nested tree is this single allocation.
        #[must_use]
        #[allow(clippy::uninit_vec)] // `Vec<u64>`, no drop, fully overwritten by collapse_emit; see SAFETY below.
        fn collapse_pack(items: &[Self]) -> Seed {
            let len = Self::collapse_size(items);
            let nwords = len.div_ceil(8).max(1);
            let mut words: Vec<u64> = Vec::with_capacity(nwords);
            // SAFETY: capacity ≥ nwords; u64 is plain bytes (no drop). `collapse_emit` writes all of
            // [0, len); the tail word's ≤7 padding bytes (past len, never exposed by `as_bytes`) are
            // zeroed here so the backing store holds no uninitialised word.
            unsafe {
                words.set_len(nwords);
            }
            words[nwords - 1] = 0;
            let dst =
                unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), len) };
            Self::collapse_emit(items, dst);
            Seed { words, len }
        }
    }

    // Pod arm: a `Vec<U: Pod>` column is a `Wire<U>` per record; the view is the cast slice `&[U]`.
    impl<T: Pod> CollapseElem for T {
        type View<'a> = &'a [T];
        fn collapse_size(items: &[T]) -> usize {
            8 + std::mem::size_of_val(items)
        }
        fn collapse_emit(items: &[T], dst: &mut [u8]) {
            dst[0..8].copy_from_slice(&(items.len() as u64).to_le_bytes());
            // SAFETY: T: Pod ⇒ every one of its bytes is initialised (no padding); read as bytes.
            let src = unsafe {
                core::slice::from_raw_parts(
                    items.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(items),
                )
            };
            dst[8..8 + src.len()].copy_from_slice(src);
        }
        fn collapse_open(bytes: &[u8]) -> Result<&[T], WireErr> {
            Ok(Wire::<T>::open(bytes)?.as_slice())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn extend_lce_is_the_run() {
            // the copy runs along the null cone (a == b) to the flaw (first difference)
            let a = b"abcabcabcX"; // "abc" thrice then X
            assert_eq!(extend(a, 3, 0), 6); // a[3..]="abcabcX" vs a[0..] agree "abcabc", flaw at X
            assert_eq!(extend(a, 1, 0), 0); // 'b' vs 'a' — flaw immediately
                                            // overlapping (RLE): the source overtakes into the copy, still just the common prefix
            assert_eq!(extend(b"aaaa", 1, 0), 3);
            // exercise the word-at-a-time path + the tail
            let mut big = vec![7u8; 100];
            big.push(9);
            assert_eq!(extend(&big, 50, 0), 50); // 50 sevens agree, then a[100]=9 vs a[50]=7
                                                 // a flaw INSIDE a word (byte 3 of the second u64 pair), little-endian trailing_zeros
            let mut v: Vec<u8> = (0..64).map(|k| (k % 16) as u8).collect(); // period-16
            v[16 + 11] = 200; // break the copy at offset 11
            assert_eq!(extend(&v, 16, 0), 11); // v[16..] vs v[0..] agree 11, flaw at the 200
        }

        #[derive(Clone, Copy, PartialEq, Debug)]
        #[repr(C)]
        struct V3 {
            x: f32,
            y: f32,
            z: f32,
        }
        // SAFETY: #[repr(C)], all-f32 (Pod) fields, no padding (3×4=12 bytes), no pointers.
        unsafe impl Pod for V3 {}

        #[test]
        fn coordinate_roundtrip_zero_copy() {
            let nodes: Vec<V3> = (0..1000)
                .map(|i| V3 {
                    x: i as f32,
                    y: (2 * i) as f32,
                    z: (3 * i) as f32,
                })
                .collect();
            let seed = pack(&nodes);
            // storage = execution: open the bytes (O(1) validation) and read by coordinate, no decode.
            let w = Wire::<V3>::open(seed.as_bytes()).expect("valid");
            assert_eq!(w.len(), nodes.len());
            for (i, node) in nodes.iter().enumerate() {
                assert_eq!(*w.get(i), *node); // getNode — coordinate read
            }
            assert_eq!(w.as_slice(), nodes.as_slice());
        }

        #[test]
        fn open_rejects_garbage_in_o1() {
            let seed = pack(&[1u32, 2, 3, 4]);
            let bytes = seed.as_bytes();
            // truncated → BadLength (a constant check, not a walk)
            assert!(matches!(
                Wire::<u32>::open(&bytes[..bytes.len() - 1]),
                Err(WireErr::BadLength)
            ));
            // a wrong element type whose size doesn't divide the payload → BadLength
            assert!(Wire::<u64>::open(bytes).is_err());
        }

        #[test]
        fn range_validation() {
            assert!(all_in_range(b"hello WORLD 123", 0x20, 0x7e));
            assert!(!all_in_range(b"hi\x00there", 0x20, 0x7e));
            assert!(all_in_range(&[], 0, 0));
            assert!(all_in_range(&[5, 6, 7], 5, 7));
            assert!(!all_in_range(&[5, 6, 8], 5, 7));
        }

        #[test]
        fn preduce_and_encircle() {
            let nodes: Vec<i64> = (1..=100).collect();
            let seed = pack(&nodes);
            let w = Wire::<i64>::open(seed.as_bytes()).unwrap();
            // Web.preduce: sum every node (schedule-free → parallelisable across any substrate)
            let total = w.preduce(0i64, |&x| x, |a, b| a + b);
            assert_eq!(total, 5050);
            // Web.encircle: "every node at its floor" — here floor = 0; not all zero ⇒ false
            assert!(!w.encircle(|&x| (x as f64).abs()));
            let zeros = pack(&[0i64; 8]);
            assert!(Wire::<i64>::open(zeros.as_bytes())
                .unwrap()
                .encircle(|&x| (x as f64).abs()));
        }

        #[test]
        fn sweep_depth_is_radius() {
            // Web.sweep_depth_is_radius: two webs on a path 0→1→2 differing ONLY at node 2. One sweep
            // can't distinguish them at node 0 (radius 1); two sweeps can (radius 2 — node 2's state
            // reached node 0 in two hops). Pointwise map collapses; the edge-reading sweep does not.
            // path 0→1→2 as contiguous ranges: node i reads node i+1 (single-element range), node 2 none.
            let adj = |i: usize| if i < 2 { i + 1..i + 2 } else { 3..3 };
            // merge: adopt the first neighbour's state, else keep own (headD over the neighbourhood).
            let merge = |_i: usize, s: &u8, nbrs: &[u8]| nbrs.first().copied().unwrap_or(*s);
            let sweep = |seed: &Seed| Wire::<u8>::open(seed.as_bytes()).unwrap().sweep(adj, merge);
            let node0 = |seed: &Seed| *Wire::<u8>::open(seed.as_bytes()).unwrap().get(0);
            let (w1, w2) = (pack(&[0u8, 0, 0]), pack(&[0u8, 0, 1])); // differ only at node 2
            let (a1, b1) = (sweep(&w1), sweep(&w2));
            assert_eq!(
                node0(&a1),
                node0(&b1),
                "one sweep: node 0 cannot see node 2 (radius 1)"
            );
            let (a2, b2) = (sweep(&a1), sweep(&b1));
            assert_ne!(
                node0(&a2),
                node0(&b2),
                "two sweeps: node 2 reached node 0 (radius 2, generative)"
            );
        }

        #[test]
        fn sweep_is_causal_attention() {
            // `sweep` computes softmax attention: node state = (q,k,v); each position reads the causal
            // neighbourhood `0..=i` and the merge is softmax(q_i·k_j)·v_j. Must equal a hand-rolled loop.
            const D: usize = 4;
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct Qkv {
                q: [f32; D],
                k: [f32; D],
                v: [f32; D],
            }
            unsafe impl Pod for Qkv {}
            let mut s: u64 = 0x1234_5678;
            let mut rnd = || {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            };
            let n = 6;
            let nodes: Vec<Qkv> = (0..n)
                .map(|_| Qkv {
                    q: [rnd(), rnd(), rnd(), rnd()],
                    k: [rnd(), rnd(), rnd(), rnd()],
                    v: [rnd(), rnd(), rnd(), rnd()],
                })
                .collect();
            let dot = |a: &[f32; D], b: &[f32; D]| (0..D).map(|i| a[i] * b[i]).sum::<f32>();
            // reference: standard causal softmax attention, output per position.
            let reference: Vec<[f32; D]> = (0..n)
                .map(|i| {
                    let sc: Vec<f32> = (0..=i).map(|j| dot(&nodes[i].q, &nodes[j].k)).collect();
                    let mx = sc.iter().cloned().fold(f32::MIN, f32::max);
                    let ex: Vec<f32> = sc.iter().map(|&x| (x - mx).exp()).collect();
                    let z: f32 = ex.iter().sum();
                    let mut o = [0f32; D];
                    for (j, &e) in ex.iter().enumerate() {
                        for d in 0..D {
                            o[d] += e / z * nodes[j].v[d];
                        }
                    }
                    o
                })
                .collect();
            // the SAME attention as one Web sweep: adj = causal range, merge = softmax over neighbours.
            let seed = pack(&nodes);
            let out = Wire::<Qkv>::open(seed.as_bytes()).unwrap().sweep(
                |i| 0..i + 1,
                |_i, me: &Qkv, nbrs: &[Qkv]| {
                    let sc: Vec<f32> = nbrs.iter().map(|nb| dot(&me.q, &nb.k)).collect();
                    let mx = sc.iter().cloned().fold(f32::MIN, f32::max);
                    let ex: Vec<f32> = sc.iter().map(|&x| (x - mx).exp()).collect();
                    let z: f32 = ex.iter().sum();
                    let mut o = [0f32; D];
                    for (nb, &e) in nbrs.iter().zip(&ex) {
                        for d in 0..D {
                            o[d] += e / z * nb.v[d];
                        }
                    }
                    Qkv {
                        q: me.q,
                        k: me.k,
                        v: o,
                    } // attention output in the v slot
                },
            );
            let got = Wire::<Qkv>::open(out.as_bytes()).unwrap();
            for i in 0..n {
                for d in 0..D {
                    assert!(
                        (got.get(i).v[d] - reference[i][d]).abs() < 1e-6,
                        "sweep attention ≠ reference at ({i},{d})"
                    );
                }
            }
        }

        #[test]
        fn ragged_zero_copy_variable_length() {
            // variable-length records (the `log`/nested case): pack → open (O(1)) → coordinate read.
            let items: Vec<&[u8]> = vec![
                b"GET /index.html 200",
                b"",
                b"POST /api/v1/login 401 unauthorized",
                b"GET /favicon.ico 404",
                &[0u8, 1, 2, 3, 255, 128],
            ];
            let seed = pack_ragged(&items);
            let r = Ragged::open(seed.as_bytes()).expect("valid");
            assert!(r.validate());
            assert_eq!(r.len(), items.len());
            for (i, it) in items.iter().enumerate() {
                assert_eq!(r.get(i), *it, "zero-copy coordinate read of item {i}");
            }
            // typed zero-copy: a ragged of V3 arrays, each item byte-len a multiple of 12 and 4-aligned.
            let a = [
                V3 {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                },
                V3 {
                    x: 4.0,
                    y: 5.0,
                    z: 6.0,
                },
            ];
            let b = [V3 {
                x: 7.0,
                y: 8.0,
                z: 9.0,
            }];
            let pa = pack(&a); // each inner already an aligned POD slice; reuse its payload bytes
            let abytes = &pa.as_bytes()[8..]; // drop the count header → just the V3 bytes
            let pb = pack(&b);
            let bbytes = &pb.as_bytes()[8..];
            let meshes: Vec<&[u8]> = vec![abytes, bbytes];
            let seed2 = pack_ragged(&meshes);
            let r2 = Ragged::open(seed2.as_bytes()).unwrap();
            assert_eq!(r2.get_as::<V3>(0), Some(&a[..]));
            assert_eq!(r2.get_as::<V3>(1), Some(&b[..]));
        }

        #[test]
        fn nested_record_composition() {
            // a `log`-like table: a scalar Wire + 2 Ragged string fields, composed into ONE buffer,
            // each record accessed zero-copy by coordinate (scalars + 2 variable fields).
            #[derive(Clone, Copy, PartialEq, Debug)]
            #[repr(C)]
            struct Sc {
                size: u64,
                code: u16,
                addr: [u8; 4],
                _pad: [u8; 2],
            }
            // SAFETY: #[repr(C)], 8+2+4+2=16 bytes, no implicit padding, all Pod fields.
            unsafe impl Pod for Sc {}
            let scalars = vec![
                Sc {
                    size: 100,
                    code: 200,
                    addr: [10, 0, 0, 1],
                    _pad: [0; 2],
                },
                Sc {
                    size: 50,
                    code: 404,
                    addr: [10, 0, 0, 2],
                    _pad: [0; 2],
                },
            ];
            let users: Vec<&[u8]> = vec![b"alice", b"bob"];
            let reqs: Vec<&[u8]> = vec![b"GET /", b"POST /login"];
            let (ws, us, rs) = (pack(&scalars), pack_ragged(&users), pack_ragged(&reqs));
            let table = pack_sections(&[ws.as_bytes(), us.as_bytes(), rs.as_bytes()]);

            let sec = Sections::open(table.as_bytes()).unwrap();
            assert_eq!(sec.len(), 3);
            let w = Wire::<Sc>::open(sec.section(0)).unwrap();
            let u = Ragged::open(sec.section(1)).unwrap();
            let r = Ragged::open(sec.section(2)).unwrap();
            for i in 0..scalars.len() {
                assert_eq!(*w.get(i), scalars[i]); // scalars zero-copy
                assert_eq!(u.get(i), users[i]); // variable field 1 zero-copy
                assert_eq!(r.get(i), reqs[i]); // variable field 2 zero-copy
            }
        }

        #[test]
        fn deep_recursive_nesting() {
            // ARBITRARY DEPTH by composition (no new mechanism): corpus → Vec<Doc> → Vec<Para> →
            // Vec<Word>. `Sections` 8-aligns each sub-buffer, so its sections are THEMSELVES openable as
            // Sections/Ragged — the (skeleton, leaves) scatter-gather nests. Zero-copy at every level.
            let corpus: Vec<Vec<Vec<&[u8]>>> = vec![
                vec![vec![b"the".as_ref(), b"cat"], vec![b"sat", b"down"]],
                vec![vec![b"hello", b"world"], vec![], vec![b"!"]],
            ];
            // pack bottom-up: words → Ragged (raw); paras → Sections (aligned, of Raggeds); docs → Sections.
            let doc_seeds: Vec<Seed> = corpus
                .iter()
                .map(|doc| {
                    let para_seeds: Vec<Seed> = doc.iter().map(|p| pack_ragged(p)).collect();
                    let refs: Vec<&[u8]> = para_seeds.iter().map(Seed::as_bytes).collect();
                    pack_sections(&refs)
                })
                .collect();
            let doc_refs: Vec<&[u8]> = doc_seeds.iter().map(Seed::as_bytes).collect();
            let corpus_seed = pack_sections(&doc_refs);

            // navigate 3 levels deep, every hop a zero-copy coordinate read
            let docs = Sections::open(corpus_seed.as_bytes()).unwrap();
            assert_eq!(docs.len(), corpus.len());
            for (i, doc) in corpus.iter().enumerate() {
                let paras = Sections::open(docs.section(i)).unwrap();
                assert_eq!(paras.len(), doc.len());
                for (j, para) in doc.iter().enumerate() {
                    let words = Ragged::open(paras.section(j)).unwrap();
                    assert_eq!(words.len(), para.len());
                    for (k, word) in para.iter().enumerate() {
                        assert_eq!(words.get(k), *word); // zero-copy, 3 levels down
                    }
                }
            }
        }

        #[test]
        fn enum_tagged_union() {
            // An enum = a TAG column (Wire) + a variant-PAYLOAD column (Ragged): a 2-field record
            // table. No new primitive (like deep nesting). Event = Click{i32,i32} | Key{u32} | Text(&[u8]).
            #[derive(Clone, Copy, PartialEq, Debug)]
            #[repr(C)]
            struct Tag {
                tag: u16,
                _p: [u8; 6],
            }
            // SAFETY: #[repr(C)], 8 bytes, no padding, Pod.
            unsafe impl Pod for Tag {}
            let mut click = Vec::new();
            click.extend_from_slice(&3i32.to_le_bytes());
            click.extend_from_slice(&4i32.to_le_bytes());
            let key = 65u32.to_le_bytes().to_vec();
            let text = b"hello".to_vec();
            let tags = vec![
                Tag { tag: 0, _p: [0; 6] },
                Tag { tag: 1, _p: [0; 6] },
                Tag { tag: 2, _p: [0; 6] },
            ];
            let payloads: Vec<&[u8]> = vec![&click, &key, &text];
            let seed = pack_record_table(&tags, &[&payloads]);

            let s = Sections::open(seed.as_bytes()).unwrap();
            let w = Wire::<Tag>::open(s.section(0)).unwrap();
            let p = Ragged::open(s.section(1)).unwrap();
            // dispatch on the tag, read the variant payload zero-copy
            assert_eq!(w.get(0).tag, 0);
            let pl = p.get(0);
            assert_eq!(i32::from_le_bytes(pl[0..4].try_into().unwrap()), 3);
            assert_eq!(i32::from_le_bytes(pl[4..8].try_into().unwrap()), 4);
            assert_eq!(w.get(1).tag, 1);
            assert_eq!(u32::from_le_bytes(p.get(1).try_into().unwrap()), 65);
            assert_eq!(w.get(2).tag, 2);
            assert_eq!(p.get(2), b"hello");
        }

        #[test]
        fn record_table_one_pass() {
            // the one-pass builder produces the same openable table as the pack_sections composition.
            #[derive(Clone, Copy, PartialEq, Debug)]
            #[repr(C)]
            struct Sc {
                a: u64,
                b: u32,
                _p: u32,
            }
            // SAFETY: #[repr(C)], 8+4+4=16, no padding, Pod fields.
            unsafe impl Pod for Sc {}
            let sc = vec![
                Sc { a: 1, b: 2, _p: 0 },
                Sc { a: 3, b: 4, _p: 0 },
                Sc { a: 5, b: 6, _p: 0 },
            ];
            let f0: Vec<&[u8]> = vec![b"x", b"yy", b"zzz"];
            let f1: Vec<&[u8]> = vec![b"alpha", b"", b"beta"];
            let t = pack_record_table(&sc, &[&f0, &f1]);
            let s = Sections::open(t.as_bytes()).unwrap();
            let w = Wire::<Sc>::open(s.section(0)).unwrap();
            let r0 = Ragged::open(s.section(1)).unwrap();
            let r1 = Ragged::open(s.section(2)).unwrap();
            for i in 0..sc.len() {
                assert_eq!(*w.get(i), sc[i]);
                assert_eq!(r0.get(i), f0[i]);
                assert_eq!(r1.get(i), f1[i]);
            }
        }
    }
}
pub use syntax::{
    cerealize_http, cerealize_term, decerealize_http, decerealize_term, lint_http_surface,
    lint_surface, lint_term, parse_surface, render_surface, translate, validate_surface, AsTerm,
    HttpCall, LintLevel, Surface, SyntaxError, SyntaxLint, SyntaxTerm,
};

/// The default context order — r5 is the upstream sweet spot (past gzip -9).
pub const DEFAULT_ORDER: usize = 5;

/// Chunk size for the parallel split: ≥ this per chunk keeps the cross-chunk ratio loss small (each
/// chunk restarts the adaptive model, so bigger chunks lose less context). Below it, one chunk.
const PAR_CHUNK: usize = 1 << 20;

/// Peak-memory ceiling for the parallel split. The coder holds ~256× the chunk size resident (the
/// O(n) rANS scalar buffers dominate), so `MEM_BUDGET / (256·PAR_CHUNK)` chunks run at once and the
/// rest wait — DRAM stays bounded no matter how big the file is (a 100 MB file no longer wants 20 GB).
const MEM_BUDGET: usize = 3usize << 30;

/// **Compress `bytes`.** The public entry routes through the [`Bias::Balanced`] face-selector
/// ([`compress_shaped`]): it MEASURES cm/lz/hybrid on a calibration sample and sends the whole stream
/// to the right face for the data's shape — dense on text, fast on the repetitive. For a fixed face
/// use [`compress_shaped`] with an explicit [`Bias`], or [`compress_at`] for the raw parallel cm.
/// Self-delimiting; inverse is [`decompress`].
#[must_use]
pub fn compress(bytes: &[u8]) -> Vec<u8> {
    compress_shaped(Bias::Balanced, bytes)
}

/// Inverse of [`compress`] — reads the face tag and dispatches (see [`decompress_shaped`]).
/// Malformed input yields an empty `Vec`; [`try_decompress`] says why.
#[must_use]
pub fn decompress(bytes: &[u8]) -> Vec<u8> {
    decompress_shaped(bytes)
}

/// Checked [`decompress`].
pub fn try_decompress(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    try_decompress_shaped(bytes)
}

/// Entropy-code `bytes` at order `k`, **split across the crate's schedule-free worker pool**
/// ([`crate::par::Pool`], `Web.preduceAt_schedule_free`): the buffer divides into ≤ `cores` chunks of
/// ≥ [`PAR_CHUNK`], each coded on its own core — near-linear CPU speedup at a small cross-chunk ratio
/// cost (nil below `PAR_CHUNK`). Framed `[nchunks][complen…][chunk bytes…]`.
#[must_use]
pub fn compress_at(k: usize, bytes: &[u8]) -> Vec<u8> {
    use crate::par::{Pool, SendPtr};
    let pool = Pool::global();
    let nch = (bytes.len() / PAR_CHUNK).clamp(1, pool.width().max(1));
    let chunk = bytes.len().div_ceil(nch);
    let mut parts: Vec<Vec<u8>> = (0..nch).map(|_| Vec::new()).collect();
    if nch <= 1 {
        parts[0] = cm::compress(k, bytes);
    } else {
        // process the chunks in WAVES of `wave` so peak DRAM stays under MEM_BUDGET regardless of file
        // size (all chunks resident at once is the ~256×len blowup).
        let wave = (MEM_BUDGET / (256 * chunk.max(1))).clamp(1, nch);
        let pptr = SendPtr(parts.as_mut_ptr());
        let mut base = 0;
        while base < nch {
            let this = wave.min(nch - base);
            pool.dispatch(this, |t| {
                let i = base + t;
                let lo = i * chunk;
                if lo < bytes.len() {
                    let hi = (lo + chunk).min(bytes.len());
                    // SAFETY: task t writes only parts[base+t]; distinct t ⇒ disjoint.
                    unsafe { *pptr.add(i) = cm::compress(k, &bytes[lo..hi]) };
                }
            });
            base += this;
        }
    }
    let mut out = Vec::new();
    write_uvarint(&mut out, nch as u64);
    for p in &parts {
        write_uvarint(&mut out, p.len() as u64);
    }
    for p in &parts {
        out.extend_from_slice(p);
    }
    out
}

/// Inverse of [`compress_at`] — unframes the chunks and decodes them across the pool. This is the
/// FRAMED decoder for a raw parallel-cm stream (no face tag); the public [`decompress`] adds the tag.
///
/// Malformed input yields an empty `Vec` (never a panic); [`try_decompress_framed`] says why.
#[must_use]
pub fn decompress_framed(bytes: &[u8]) -> Vec<u8> {
    try_decompress_framed(bytes).unwrap_or_default()
}

/// Checked [`decompress_framed`]. The chunk count and every chunk length are validated against the
/// input before anything is allocated, and each chunk is decoded by the bounded [`cm::try_decompress`].
pub fn try_decompress_framed(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    use crate::par::{Pool, SendPtr};
    let mut pos = 0usize;
    let nch = read_len(bytes, &mut pos)?;
    if nch == 0 {
        return Ok(Vec::new());
    }
    // each chunk length is at least one varint byte, so a count beyond the remaining input is a lie
    if nch > bytes.len() - pos {
        return Err(DecodeError::Truncated);
    }
    let mut lens: Vec<usize> = try_vec(nch)?;
    for _ in 0..nch {
        lens.push(read_len(bytes, &mut pos)?);
    }
    let mut offs: Vec<(usize, usize)> = try_vec(nch)?;
    let mut o = pos;
    for &l in &lens {
        let end = o
            .checked_add(l)
            .filter(|&e| e <= bytes.len())
            .ok_or(DecodeError::Truncated)?;
        offs.push((o, end));
        o = end;
    }
    let mut parts: Vec<Result<Vec<u8>, DecodeError>> = (0..nch).map(|_| Ok(Vec::new())).collect();
    if nch <= 1 {
        parts[0] = cm::try_decompress(&bytes[offs[0].0..offs[0].1]);
    } else {
        let pool = Pool::global();
        // wave-cap by the largest chunk so decode DRAM stays bounded too (the tables are per-chunk).
        let maxc = lens.iter().copied().max().unwrap_or(1).max(1);
        let wave = (MEM_BUDGET / (256 * maxc)).clamp(1, nch);
        let pptr = SendPtr(parts.as_mut_ptr());
        let mut base = 0;
        while base < nch {
            let this = wave.min(nch - base);
            pool.dispatch(this, |t| {
                let (a, b) = offs[base + t];
                // SAFETY: task t writes only parts[base+t]; distinct t ⇒ disjoint.
                unsafe { *pptr.add(base + t) = cm::try_decompress(&bytes[a..b]) };
            });
            base += this;
        }
    }
    let mut total = 0usize;
    for part in &parts {
        let part = part.as_ref().map_err(|&e| e)?;
        total = total
            .checked_add(part.len())
            .filter(|&t| t <= DEFAULT_MAX_OUTPUT)
            .ok_or(DecodeError::TooLarge)?;
    }
    let mut out = try_vec(total)?;
    for part in parts {
        out.extend_from_slice(&part?);
    }
    Ok(out)
}

// ════════════════════════════════════════════════════════════════════════════
// THE FACE-SELECTOR — route the whole stream to the right face by MEASURED shape.
//
// Three faces on a speed/density ladder:
//   * cm     — the density crown at SMALL scale, slowest (context-mixer; every byte modelled).
//   * hybrid — near-cm density at LZ speed on repetitive data (LZ tokens + cm literals).
//   * lz      — loosest, fastest, but GLOBAL-window (its match-finder spans the whole file).
// We MEASURE the three on a calibration sample (their real coded sizes) and route by that, never
// guess by heuristic. The speed/density preference is a named `Bias` (a level, like zstd's), not a
// buried constant.
//
// KNOWN BLIND SPOT (measured, honest). The sample can misrank two SCALE-DEPENDENT effects it cannot
// see: (1) `compress_at` chunks cm at PAR_CHUNK and RESTARTS the model per chunk, so on large data
// with long-range redundancy cm loses cross-chunk context — measured 2.82× density loss on 8 MB C4
// (10.32× single-chunk → 3.66× chunked); a <PAR_CHUNK sample is one chunk and shows none of it.
// (2) lz's global window finds MORE matches at full scale than in any window. So the sample is biased
// FOR chunked-cm and AGAINST lz on large redundant streams. Dispersing the sample (head/mid/tail)
// narrows (2) but not (1). The real fix is the parallel hybrid (global-lz dedup → chunked-cm on the
// residual), which folds the long-range redundancy BEFORE the chunk restarts, so they stop mattering
// — not yet built. Until then the selector is sound for homogeneous streams and small files.
// ════════════════════════════════════════════════════════════════════════════

/// The speed/density preference — the one honest knob a lossless codec must expose (the rungs of the
/// ladder are measured; which rung you want is yours to say). `Balanced` is the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Bias {
    /// Smallest output, whatever the cost — always the context-mixer crown.
    Dense,
    /// Take the faster face only when it stays within [`BALANCE_TOL`] of the densest — the knee.
    #[default]
    Balanced,
    /// Fastest face that still compresses — the pure suffix-web tokens.
    Fast,
}

/// The face tags written as byte 0 of a shaped stream (decode dispatches on it).
const FACE_CM: u8 = 0;
const FACE_LZ: u8 = 1;
const FACE_HYBRID: u8 = 2;

/// Calibration budget — read the shape from this many bytes, drawn as dispersed windows. Set ABOVE
/// [`PAR_CHUNK`] so the cm rung in the sample is itself CHUNKED (its restart penalty shows) and lz
/// sees enough span to find cross-chunk matches — the two scale effects the router must rank right.
const CALIB: usize = 4 << 20;

/// `Balanced`'s definition: take a faster face when the EXTRA bytes it costs (over the densest face)
/// are within this fraction of the RAW input — how much of the original you will trade for speed.
/// Measured against raw, not against the winner: on 1000×-crushed data a "4% larger than best" gap is
/// 0.004% of the file (take the fast face); on prose the same faster face costs 14% of the file (keep
/// the crown). 0.5% — the one named knob; the rungs it chooses between are all MEASURED.
const BALANCE_TOL: f64 = 0.005;

/// Build the calibration sample — up to [`CALIB`] bytes drawn as three dispersed windows
/// (head / middle / tail) so cross-region redundancy is visible to lz's global match-finder, not
/// just the head's local statistics. A file at or below `CALIB` is used whole (borrowed, no copy).
fn calib_sample(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;
    if bytes.len() <= CALIB {
        return Cow::Borrowed(bytes);
    }
    let w = CALIB / 3;
    let mid = bytes.len() / 2 - w / 2;
    let tail = bytes.len() - w;
    let mut s = Vec::with_capacity(w * 3);
    s.extend_from_slice(&bytes[..w]);
    s.extend_from_slice(&bytes[mid..mid + w]);
    s.extend_from_slice(&bytes[tail..]);
    Cow::Owned(s)
}

/// The coded bodies of `sample` under each face, in tag order `[cm, lz, hybrid]` — the measured
/// rungs. When the sample IS the whole input (a file ≤ `CALIB`), these bodies are the FINAL ones, so
/// the caller keeps the winner and never recompresses.
fn face_bodies(sample: &[u8]) -> [Vec<u8>; 3] {
    [
        compress_at(DEFAULT_ORDER, sample),
        lz::compress(sample),
        lz::compress_hybrid(sample),
    ]
}

/// Pick the face for `bias` given the measured `[cm, lz, hybrid]` sample sizes. `Dense` = the
/// smallest; `Fast` = lz if it compresses at all; `Balanced` = the fastest face (lz > hybrid > cm by
/// speed) whose size is within `BALANCE_TOL` of the smallest.
fn select_face(bias: Bias, sizes: [usize; 3], raw: usize) -> u8 {
    let best = sizes.iter().copied().min().unwrap();
    // the extra bytes a face costs over the densest, allowed as a fraction of raw (Balanced) or 0
    // (Dense — exact tie only). lz first, then hybrid: prefer the faster face when it qualifies.
    let ceil = match bias {
        Bias::Dense => best,
        Bias::Balanced => best + (raw as f64 * BALANCE_TOL) as usize,
        Bias::Fast => return FACE_LZ,
    };
    if sizes[FACE_LZ as usize] <= ceil {
        FACE_LZ
    } else if sizes[FACE_HYBRID as usize] <= ceil {
        FACE_HYBRID
    } else {
        FACE_CM
    }
}

/// **The face-selector.** Measure the three faces on a calibration sample, route the WHOLE stream to
/// the one `bias` wants, tag it. `[face byte][face-coded body]`. Lossless; decode reads the tag.
#[must_use]
pub fn compress_shaped(bias: Bias, bytes: &[u8]) -> Vec<u8> {
    // Fast skips the weigh-in — the seq-fast encoder under the same FACE_LZ tag (one seq format,
    // one decoder for every effort; the finder is the dial, `FinderRecall`).
    if bias == Bias::Fast {
        let mut out = vec![FACE_LZ];
        out.extend_from_slice(&lz::compress_seq_fast(bytes));
        return out;
    }
    let sample = calib_sample(bytes);
    let bodies = face_bodies(&sample);
    let sizes = [bodies[0].len(), bodies[1].len(), bodies[2].len()];
    let face = select_face(bias, sizes, sample.len());
    let mut out = vec![face];
    // If the sample IS the whole input (file ≤ CALIB), the winning body is already computed — keep it,
    // don't recompress. Otherwise the sample was a proxy; code the whole stream with the chosen face.
    if bytes.len() <= CALIB {
        out.extend_from_slice(&bodies[face as usize]);
    } else {
        match face {
            FACE_LZ => out.extend_from_slice(&lz::compress(bytes)),
            FACE_HYBRID => out.extend_from_slice(&lz::compress_hybrid(bytes)),
            _ => out.extend_from_slice(&compress_at(DEFAULT_ORDER, bytes)),
        }
    }
    out
}

/// Inverse of [`compress_shaped`] — read the face tag, dispatch to that face's decoder. Malformed
/// input yields an empty `Vec`; [`try_decompress_shaped`] says why.
#[must_use]
pub fn decompress_shaped(bytes: &[u8]) -> Vec<u8> {
    try_decompress_shaped(bytes).unwrap_or_default()
}

/// Checked [`decompress_shaped`].
pub fn try_decompress_shaped(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let Some((&face, body)) = bytes.split_first() else {
        return Ok(Vec::new());
    };
    match face {
        FACE_LZ => lz::try_decompress(body),
        FACE_HYBRID => lz::try_decompress_hybrid(body),
        _ => try_decompress_framed(body),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// THE FLOOR — the Being arm. `quadSeq`/`triadGen` (proven `quadSeq_regenerates`): predict each
// value as the order-2 extrapolation of the last three; store only the residual. On a
// linear/quadratic stream (ids, counters, slow drift) the residual is 0 — "from almost nothing".
// ════════════════════════════════════════════════════════════════════════════

/// The prediction of `v[i]` from the already-known prefix, at floor `order` ∈ {0,1,2}. The floor
/// is the only freedom; it is **selected** per column (below), never assumed (`SangClone.induce`'s
/// dual-finder `best := min` — the NB select, `δS=0`). Same on encode and decode.
///   * order 0 — none (store the value; right for noise, where a predictor amplifies it),
///   * order 1 — delta (right for monotone / slowly-drifting streams),
///   * order 2 — `quadSeq` quadratic extrapolation (right for counters / accelerating streams).
#[inline]
fn predict_at(order: u8, v: &[i64], i: usize) -> i64 {
    match order {
        0 => 0,
        1 => {
            if i >= 1 {
                v[i - 1]
            } else {
                0
            }
        }
        _ => match i {
            0 => 0,
            1 => v[0],
            2 => v[1].wrapping_mul(2).wrapping_sub(v[0]),
            _ => v[i - 1]
                .wrapping_mul(3)
                .wrapping_sub(v[i - 2].wrapping_mul(3))
                .wrapping_add(v[i - 3]),
        },
    }
}

/// The floor orders the select tries.
const FLOORS: [u8; 3] = [0, 1, 2];

/// Residual byte-stream under one floor: `quadSeq`/delta/none → zigzag → varint.
fn residual_bytes_at(order: u8, vals: &[i64]) -> Vec<u8> {
    let mut rb = Vec::with_capacity(vals.len() * 2);
    for i in 0..vals.len() {
        let r = vals[i].wrapping_sub(predict_at(order, vals, i));
        write_uvarint(&mut rb, zigzag(r));
    }
    rb
}

/// LEB128 length of a value — the proxy cost (bytes the residual would take), no coding.
fn uvarint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 128 {
        v >>= 7;
        n += 1;
    }
    n
}

/// **FIND the closed form, don't iterate.** A column is an exact degree-≤2 recurrence (constant /
/// arithmetic / quadratic — counters, ids, indices, timestamps) iff every value from index 3 on is
/// the order-2 extrapolation of the prior three (`quadSeq_regenerates`). One cheap pass, early-out.
fn exact_deg2(vals: &[i64]) -> bool {
    (3..vals.len()).all(|i| vals[i] == predict_at(2, vals, i))
}

/// **FIND the floor without iterating the CM coder.** Pick the order whose residual stream is
/// smallest (fewest varint bytes) in one O(n) proxy pass — not 3 full context-mixing encodes.
fn best_floor_by_proxy(vals: &[i64]) -> u8 {
    let mut best_order = 0u8;
    let mut best_cost = usize::MAX;
    for &d in &FLOORS {
        let cost: usize = (0..vals.len())
            .map(|i| uvarint_len(zigzag(vals[i].wrapping_sub(predict_at(d, vals, i)))))
            .sum();
        if cost < best_cost {
            best_cost = cost;
            best_order = d;
        }
    }
    best_order
}

const EXACT_TAG: u8 = 0xFF; // distinct from the floor orders 0/1/2

// ════════════════════════════════════════════════════════════════════════════
// THE BIT-PACK CODER — the Null arm's *speed* face (the design's middle knob, `none/bit-pack/
// entropy`). Floor (delta/quadSeq — the Being arm) removes the structure; FFOR (frame-of-reference +
// fixed-width bit-pack) stores the residual at its minimal bit width. This is FastLanes/pcodec's exact
// shape — no entropy model, so it runs at memory speed, and on a genuinely random column it lands at
// the bit-width floor (≈ the entropy of a uniform range). The entropy coder (`encode_i64`) still wins
// *ratio* where a model helps; this wins *speed* where the residual is already near-uniform.
// ════════════════════════════════════════════════════════════════════════════

const BITPACK_TAG: u8 = 0xFE;

/// Floor-then-FFOR: select the floor (`best_floor_by_proxy`), take the zigzag residual, subtract its
/// min (frame of reference), and pack each at `width = bits(max−min)`. Header: tag, order, len, min,
/// width. A constant/exact-recurrence column → `width = 0` → header only. No entropy model → fast.
#[must_use]
pub fn bitpack_i64(vals: &[i64]) -> Vec<u8> {
    // Exact degree-≤2 recurrence (ids/counters/timestamps): the floor removes *everything*, so store
    // the ≤3 seeds + length — the same O(1) seed form `encode_i64` uses, at recurrence-check speed.
    // (Single-block FFOR can't reach this: the first 1-3 boundary residuals are outliers that would
    // set the block width; patched/block FFOR is the general fix, this is the exact shortcut.)
    if exact_deg2(vals) {
        let mut out = vec![EXACT_TAG];
        write_uvarint(&mut out, vals.len() as u64);
        for &v in vals.iter().take(3) {
            write_uvarint(&mut out, zigzag(v));
        }
        return out;
    }
    // Select the floor by the FFOR *width* it produces — the bits bit-pack actually pays — not the
    // varint proxy (which ties floors that FFOR ranks very differently: a constant residual under one
    // floor with a single outlier under another cost the same in varints but wildly different in
    // width). This is the δS=0 select on the coder's own cost.
    let width_under = |order: u8| -> (u64, u64) {
        let (mut mn, mut mx) = (u64::MAX, 0u64);
        for i in 0..vals.len() {
            let r = zigzag(vals[i].wrapping_sub(predict_at(order, vals, i)));
            mn = mn.min(r);
            mx = mx.max(r);
        }
        if vals.is_empty() {
            (0, 0)
        } else {
            (mn, mx)
        }
    };
    let order = *FLOORS
        .iter()
        .min_by_key(|&&o| {
            let (mn, mx) = width_under(o);
            if mx == mn {
                0u32
            } else {
                64 - (mx - mn).leading_zeros()
            }
        })
        .unwrap();
    let res: Vec<u64> = (0..vals.len())
        .map(|i| zigzag(vals[i].wrapping_sub(predict_at(order, vals, i))))
        .collect();
    let min = res.iter().copied().min().unwrap_or(0);
    let max = res.iter().copied().max().unwrap_or(0);
    let width = if max == min {
        0
    } else {
        (64 - (max - min).leading_zeros()) as usize
    };

    let mut out = vec![BITPACK_TAG, order];
    write_uvarint(&mut out, vals.len() as u64);
    write_uvarint(&mut out, min);
    out.push(width as u8);
    if width > 0 {
        let mut acc: u128 = 0;
        let mut nbits: u32 = 0;
        for &r in &res {
            acc |= ((r - min) as u128) << nbits;
            nbits += width as u32;
            while nbits >= 8 {
                out.push((acc & 0xff) as u8);
                acc >>= 8;
                nbits -= 8;
            }
        }
        if nbits > 0 {
            out.push((acc & 0xff) as u8);
        }
    }
    out
}

/// The most values the integer-column decoders will regenerate from a header alone (a constant or
/// exact-recurrence column stores no per-value bytes, so its length is not bounded by the input).
/// A larger declared length is refused before anything is allocated; the `*_with_limit` decoders
/// take an explicit cap. Columns whose values are stored (bit-packed, entropy-coded) are bounded by
/// their input instead and are not subject to this cap.
pub const DEFAULT_MAX_COLUMN_LEN: usize = 1 << 24;

/// Inverse of [`bitpack_i64`]. Malformed input (or a header-only column longer than
/// [`DEFAULT_MAX_COLUMN_LEN`]) yields an empty `Vec`; [`try_bitunpack_i64`] says why.
#[must_use]
pub fn bitunpack_i64(bytes: &[u8]) -> Vec<i64> {
    try_bitunpack_i64(bytes).unwrap_or_default()
}

/// Checked [`bitunpack_i64`] with the default [`DEFAULT_MAX_COLUMN_LEN`].
pub fn try_bitunpack_i64(bytes: &[u8]) -> Result<Vec<i64>, DecodeError> {
    try_bitunpack_i64_with_limit(bytes, DEFAULT_MAX_COLUMN_LEN)
}

/// Regenerate `len` values of an exact degree-≤2 recurrence from its ≤3 seeds. `len` comes straight
/// from the header, so it is capped by `max_len` before anything is reserved.
fn exact_recurrence(rest: &[u8], max_len: usize) -> Result<Vec<i64>, DecodeError> {
    let mut pos = 0usize;
    let len = read_len(rest, &mut pos)?;
    if len > max_len {
        return Err(DecodeError::TooLarge);
    }
    let mut vals: Vec<i64> = try_vec(len)?;
    for _ in 0..len.min(3) {
        vals.push(unzigzag(read_uvarint(rest, &mut pos)?));
    }
    while vals.len() < len {
        let i = vals.len();
        vals.push(predict_at(2, &vals, i)); // the order-2 recurrence regenerates degree ≤2
    }
    Ok(vals)
}

/// [`try_bitunpack_i64`] with an explicit cap on header-only columns (`width = 0` and the exact
/// recurrence form). Bit-packed columns are bounded by the packed bytes actually present.
pub fn try_bitunpack_i64_with_limit(bytes: &[u8], max_len: usize) -> Result<Vec<i64>, DecodeError> {
    let Some((&tag, rest)) = bytes.split_first() else {
        return Ok(Vec::new());
    };
    if tag == EXACT_TAG {
        // The exact-recurrence shortcut — same seed reconstruction as `decode_i64`.
        return exact_recurrence(rest, max_len);
    }
    if tag != BITPACK_TAG {
        return Err(DecodeError::Malformed);
    }
    let Some((&order, rest)) = rest.split_first() else {
        return Err(DecodeError::Truncated);
    };
    let mut pos = 0usize;
    let len = read_len(rest, &mut pos)?;
    let min = read_uvarint(rest, &mut pos)?;
    let width = usize::from(*rest.get(pos).ok_or(DecodeError::Truncated)?);
    pos += 1;
    let packed = &rest[pos..];
    if width > 64 {
        return Err(DecodeError::Malformed);
    }

    if width == 0 {
        if len > max_len {
            return Err(DecodeError::TooLarge);
        }
        let mut vals: Vec<i64> = try_vec(len)?;
        for i in 0..len {
            let r = unzigzag(min);
            vals.push(r.wrapping_add(predict_at(order, &vals, i)));
        }
        return Ok(vals);
    }
    // Every value is stored: `len · width` bits must be present, which also bounds `len` by the input.
    let need_bits = len.checked_mul(width).ok_or(DecodeError::TooLarge)?;
    if need_bits > packed.len().saturating_mul(8) {
        return Err(DecodeError::Truncated);
    }
    let mut vals: Vec<i64> = try_vec(len)?;
    let mask: u128 = (1u128 << width) - 1;
    let mut acc: u128 = 0;
    let mut nbits: u32 = 0;
    let mut bi = 0usize;
    for i in 0..len {
        while nbits < width as u32 {
            // `need_bits` guarantees the bytes are there
            acc |= u128::from(packed[bi]) << nbits;
            nbits += 8;
            bi += 1;
        }
        let v = (acc & mask) as u64;
        acc >>= width;
        nbits -= width as u32;
        let r = unzigzag(v.wrapping_add(min));
        vals.push(r.wrapping_add(predict_at(order, &vals, i)));
    }
    Ok(vals)
}

/// Encode an integer column — **find, don't iterate**. If the column is an exact degree-≤2
/// recurrence, store the closed form: its ≤3 seeds + the length, **O(1) output regardless of N**
/// (a million sequential ids → ~4 bytes, the `quadSeq` collapse, no per-element coding). Otherwise
/// FIND the best floor in one O(n) proxy pass and code its residual ONCE. 1-byte tag.
#[must_use]
pub fn encode_i64(vals: &[i64]) -> Vec<u8> {
    if exact_deg2(vals) {
        let mut out = vec![EXACT_TAG];
        write_uvarint(&mut out, vals.len() as u64);
        for &v in vals.iter().take(3) {
            write_uvarint(&mut out, zigzag(v));
        }
        return out;
    }
    let order = best_floor_by_proxy(vals);
    let mut out = vec![order];
    out.extend_from_slice(&cm::compress(
        DEFAULT_ORDER,
        &residual_bytes_at(order, vals),
    ));
    out
}

/// Inverse of [`encode_i64`]. `EXACT_TAG` → regenerate from the seeds by the order-2 recurrence
/// (exact for degree ≤2); else reconstruct the residual under the tagged floor. Malformed input (or an
/// exact column longer than [`DEFAULT_MAX_COLUMN_LEN`]) yields an empty `Vec`; [`try_decode_i64`]
/// says why.
#[must_use]
pub fn decode_i64(bytes: &[u8]) -> Vec<i64> {
    try_decode_i64(bytes).unwrap_or_default()
}

/// Checked [`decode_i64`] with the default [`DEFAULT_MAX_COLUMN_LEN`].
pub fn try_decode_i64(bytes: &[u8]) -> Result<Vec<i64>, DecodeError> {
    try_decode_i64_with_limit(bytes, DEFAULT_MAX_COLUMN_LEN)
}

/// [`try_decode_i64`] with an explicit cap on the exact-recurrence form (the only one whose length is
/// not bounded by its input).
pub fn try_decode_i64_with_limit(bytes: &[u8], max_len: usize) -> Result<Vec<i64>, DecodeError> {
    let Some((&tag, rest)) = bytes.split_first() else {
        return Ok(Vec::new());
    };
    if tag == EXACT_TAG {
        return exact_recurrence(rest, max_len);
    }
    if tag > 2 {
        return Err(DecodeError::Malformed); // `encode_i64` writes a floor order 0..=2
    }
    let order = tag;
    let residual_bytes = cm::try_decompress(rest)?;
    let mut vals: Vec<i64> = Vec::new();
    let mut pos = 0usize;
    while pos < residual_bytes.len() {
        let r = unzigzag(read_uvarint(&residual_bytes, &mut pos)?);
        let i = vals.len();
        vals.push(r.wrapping_add(predict_at(order, &vals, i)));
    }
    Ok(vals)
}

/// **O(1) random access — find, don't iterate the decode.** For an exact-recurrence column, read
/// element `i` by evaluating the degree-≤2 closed form (Newton forward difference) at coordinate
/// `i` directly — no regeneration, no decompression, O(1). Returns `None` if the column was stored
/// floored (then access needs the residual stream; use [`decode_i64`]) or the header is malformed.
#[must_use]
pub fn get_at(bytes: &[u8], i: usize) -> Option<i64> {
    let (&tag, rest) = bytes.split_first()?;
    if tag != EXACT_TAG {
        return None;
    }
    let mut pos = 0usize;
    let len = read_len(rest, &mut pos).ok()?;
    if i >= len {
        return None;
    }
    let k = len.min(3);
    let mut seed = [0i64; 3];
    for s in seed.iter_mut().take(k) {
        *s = unzigzag(read_uvarint(rest, &mut pos).ok()?);
    }
    // v(i) = v0 + Δ¹·i + Δ²·C(i,2): the degree-≤2 closed form, exact (mod 2⁶⁴), O(1).
    let d1 = if k >= 2 {
        seed[1].wrapping_sub(seed[0])
    } else {
        0
    };
    let d2 = if k >= 3 {
        seed[2]
            .wrapping_sub(seed[1].wrapping_mul(2))
            .wrapping_add(seed[0])
    } else {
        0
    };
    let tri = if i >= 1 {
        (((i as u128) * (i as u128 - 1)) / 2) as i64
    } else {
        0
    };
    Some(
        seed[0]
            .wrapping_add(d1.wrapping_mul(i as i64))
            .wrapping_add(d2.wrapping_mul(tri)),
    )
}

// ── varint + zigzag (the LEB128 word↔byte of Seed.lean: serWord/deWord) ──────

pub(crate) fn write_uvarint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

/// Why a compressed or encoded buffer was rejected by a checked decoder (`try_*`). The unchecked-looking
/// entry points (`decompress`, `decode_i64`, ...) keep their `Vec`/`Option` signatures and return an
/// empty value for any of these; use the `try_*` form to tell malformed input from legitimately
/// empty output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended before a header, table or stream the format promises.
    Truncated,
    /// The input is structurally invalid (bad tag, out-of-range symbol, inconsistent table, ...).
    Malformed,
    /// A declared length exceeds the decode limit (or the input-size bound that applies to it) or
    /// the allocation for it could not be made. Nothing was allocated for it.
    TooLarge,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DecodeError::Truncated => "truncated input",
            DecodeError::Malformed => "malformed input",
            DecodeError::TooLarge => "declared length exceeds the decode limit",
        })
    }
}

impl std::error::Error for DecodeError {}

/// The default cap on the output of a decoder whose output size is not bounded by its input size (an
/// LZ match or a single-symbol entropy block can legitimately expand a few bytes into many). A
/// declared length above the cap is rejected before anything is allocated; the `*_with_limit`
/// decoders take an explicit cap instead.
pub const DEFAULT_MAX_OUTPUT: usize = 1 << 30;

/// Checked LEB128 read (the inverse of [`write_uvarint`]): at most 10 bytes, and the tenth may only
/// carry the top bit of a `u64`. Never panics, never overflows.
pub(crate) fn read_uvarint(bytes: &[u8], pos: &mut usize) -> Result<u64, DecodeError> {
    let mut shift = 0u32;
    let mut acc = 0u64;
    loop {
        let b = *bytes.get(*pos).ok_or(DecodeError::Truncated)?;
        *pos += 1;
        let low = u64::from(b & 0x7f);
        if shift == 63 && low > 1 {
            return Err(DecodeError::Malformed);
        }
        acc |= low << shift;
        if b & 0x80 == 0 {
            return Ok(acc);
        }
        shift += 7;
        if shift > 63 {
            return Err(DecodeError::Malformed);
        }
    }
}

/// [`read_uvarint`] as a `usize` (a length or count); a value that does not fit is `TooLarge`.
pub(crate) fn read_len(bytes: &[u8], pos: &mut usize) -> Result<usize, DecodeError> {
    usize::try_from(read_uvarint(bytes, pos)?).map_err(|_| DecodeError::TooLarge)
}

/// `bytes[*pos..*pos + n]` with checked arithmetic, advancing `pos`.
pub(crate) fn take_slice<'a>(
    bytes: &'a [u8],
    pos: &mut usize,
    n: usize,
) -> Result<&'a [u8], DecodeError> {
    let end = pos.checked_add(n).ok_or(DecodeError::Truncated)?;
    let s = bytes.get(*pos..end).ok_or(DecodeError::Truncated)?;
    *pos = end;
    Ok(s)
}

/// A `Vec` with room for `n` elements, or `TooLarge` instead of aborting the process.
pub(crate) fn try_vec<T>(n: usize) -> Result<Vec<T>, DecodeError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| DecodeError::TooLarge)?;
    Ok(v)
}

/// `vec![0u8; n]`, fallibly.
pub(crate) fn try_zeroed(n: usize) -> Result<Vec<u8>, DecodeError> {
    let mut v = try_vec::<u8>(n)?;
    v.resize(n, 0);
    Ok(v)
}

#[inline]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// ════════════════════════════════════════════════════════════════════════════
// THE UNIVERSAL ENCODING — Polymorphic.Term, the one form everything lifts to.
// Faithful port of proof/Substrate/Ski/TermCore.lean (the 3-constructor Term) and
// UniverseSyntaxSerialisation.lean (serialise/deserialise, round-trip proven). This is the
// answer to "fields out of a type": you don't traverse the type, you LIFT any value to one
// Term over a leaf-α and code that (Axiom XX — the lift can't branch on what a symbol means).
//
// Serialise splits a Term into the proven shape `[atom B U, ext, ext, …]`: the token TAGS are
// the structure (the Being skeleton), the α-VALUES are the leaves (the Null content). The
// floor+coder above run on the leaf stream; the skeleton entropy-codes to ~nothing when flat.
// ════════════════════════════════════════════════════════════════════════════

/// The polymorphic bulk Term — three constructors (`TermCore.lean`). The *spec* form; the
/// operational form at scale is the flat token list (`lift_to_tokens`), since a deep nested
/// `Term` is a left-spine the proofs themselves treat as "a non-empty list of tokens".
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Term<A> {
    /// `K B U` — the initial constant cell (the head atom).
    K(A, A),
    /// `S_routeF prev outer` — extend left.
    SRouteF(Box<Term<A>>, A),
    /// `S_routeG outer prev` — extend right.
    SRouteG(A, Box<Term<A>>),
}

/// The structural unit of universe-syntax serialisation (`Token`, `UniverseSyntaxSerialisation`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Token<A> {
    Atom(A, A),
    ExtendF(A),
    ExtendG(A),
}

/// `universalLiftCons` — the canonical lift of an α-stream of length ≥ 2: `K(s0,s1)` extended
/// outward by `S_routeF` for each later element. Format-agnostic (`universalLift_map`).
#[must_use]
pub fn universal_lift<A: Clone>(stream: &[A]) -> Option<Term<A>> {
    if stream.len() < 2 {
        return None; // no K-atom can be formed (matches `universalLift` on lists of length < 2)
    }
    let mut t = Term::K(stream[0].clone(), stream[1].clone());
    for x in &stream[2..] {
        t = Term::SRouteF(Box::new(t), x.clone());
    }
    Some(t)
}

/// `serialise : Term α → List (Token α)`. Iterative (the spine is collected outermost→innermost,
/// then reversed) so it matches the recursive Lean `serialise prev ++ [ext outer]`.
#[must_use]
pub fn serialise<A: Clone>(t: &Term<A>) -> Vec<Token<A>> {
    let mut exts: Vec<Token<A>> = Vec::new();
    let mut cur = t;
    let head = loop {
        match cur {
            Term::K(b, u) => break Token::Atom(b.clone(), u.clone()),
            Term::SRouteF(prev, outer) => {
                exts.push(Token::ExtendF(outer.clone()));
                cur = prev;
            }
            Term::SRouteG(outer, prev) => {
                exts.push(Token::ExtendG(outer.clone()));
                cur = prev;
            }
        }
    };
    exts.reverse(); // collected root→K (outer-first); serialise order is innermost-first
    let mut out = Vec::with_capacity(exts.len() + 1);
    out.push(head);
    out.extend(exts);
    out
}

/// `deserialise : List (Token α) → Option (Term α)`. `None` for empty or non-atom-headed lists.
#[must_use]
pub fn deserialise<A: Clone>(tokens: &[Token<A>]) -> Option<Term<A>> {
    let (b, u) = match tokens.first()? {
        Token::Atom(b, u) => (b.clone(), u.clone()),
        _ => return None,
    };
    let mut t = Term::K(b, u);
    for tk in &tokens[1..] {
        match tk {
            Token::Atom(_, _) => return None, // atoms are invalid mid-list
            Token::ExtendF(o) => t = Term::SRouteF(Box::new(t), o.clone()),
            Token::ExtendG(o) => t = Term::SRouteG(o.clone(), t.into()),
        }
    }
    Some(t)
}

/// The flat operational lift — `serialise ∘ universalLift` built directly, no deep nesting
/// (proven equal by `serialise_universalLiftCons`): `[Atom(s0,s1), ExtendF(s2), …]`.
#[must_use]
pub fn lift_to_tokens<A: Clone>(stream: &[A]) -> Option<Vec<Token<A>>> {
    if stream.len() < 2 {
        return None;
    }
    let mut out = Vec::with_capacity(stream.len() - 1);
    out.push(Token::Atom(stream[0].clone(), stream[1].clone()));
    for x in &stream[2..] {
        out.push(Token::ExtendF(x.clone()));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_roundtrip_matches_lean() {
        // deserialise (serialise t) = some t, on each constructor shape (deserialise_serialise).
        let terms: Vec<Term<i32>> = vec![
            Term::K(1, 2),
            Term::SRouteF(Box::new(Term::K(1, 2)), 3),
            Term::SRouteG(9, Box::new(Term::SRouteF(Box::new(Term::K(1, 2)), 3))),
        ];
        for t in &terms {
            // every serialised term begins with an atom (serialise_head_is_atom)
            assert!(matches!(serialise(t).first(), Some(Token::Atom(_, _))));
            assert_eq!(deserialise(&serialise(t)).as_ref(), Some(t));
        }
    }

    #[test]
    fn universal_lift_and_flat_tokens_agree() {
        // the flat lift equals serialise(universalLift(..)) (serialise_universalLiftCons),
        // and lifts of length < 2 are None (universalLift_singleton/_nil).
        assert_eq!(universal_lift::<i32>(&[]), None);
        assert_eq!(universal_lift(&[7]), None);
        let s = [10, 20, 30, 40, 50];
        let viaterm = serialise(&universal_lift(&s).unwrap());
        assert_eq!(viaterm, lift_to_tokens(&s).unwrap());
        // round-trip the stream through the universe-syntax
        let toks = lift_to_tokens(&s).unwrap();
        let t = deserialise(&toks).unwrap();
        assert_eq!(t, universal_lift(&s).unwrap());
    }

    #[test]
    fn byte_roundtrip() {
        for case in [
            vec![],
            vec![0u8; 1000],
            b"the quick brown fox jumps over the lazy dog".to_vec(),
            (0..=255u8).cycle().take(5000).collect(),
        ] {
            assert_eq!(decompress(&compress(&case)), case);
        }
    }

    #[test]
    fn shaped_roundtrip_every_bias_every_shape() {
        // the face-selector must be lossless for every bias on every shape, and the tag must route.
        let prose = b"the meek shall inherit. ".repeat(2000);
        let repeat = vec![0u8; 40_000];
        let mixed = {
            let mut v = prose.clone();
            v.extend(b"AABBCCDD".repeat(3000));
            v
        };
        let cases: Vec<&[u8]> = vec![b"", b"x", &prose, &repeat, &mixed];
        for bias in [Bias::Dense, Bias::Balanced, Bias::Fast] {
            for c in &cases {
                let enc = compress_shaped(bias, c);
                assert_eq!(
                    &decompress_shaped(&enc),
                    c,
                    "shaped round-trip {bias:?} len {}",
                    c.len()
                );
            }
        }
        // Fast must actually route to the lz face on repetitive data; Dense must never exceed cm.
        assert_eq!(compress_shaped(Bias::Fast, &repeat)[0], FACE_LZ);
        let dense = compress_shaped(Bias::Dense, &prose);
        assert!(dense.len() <= 1 + compress_at(DEFAULT_ORDER, &prose).len());
    }

    #[test]
    fn i64_roundtrip() {
        let cases: Vec<Vec<i64>> = vec![
            vec![],
            vec![42],
            (0..1000).collect(),                // perfectly linear → residual 0
            (0..1000).map(|n| n * n).collect(), // quadratic → residual 0 after warmup
            vec![-5, 3, -100, 1 << 40, -(1 << 50), 0, 7], // arbitrary
        ];
        for c in cases {
            assert_eq!(
                decode_i64(&encode_i64(&c)),
                c,
                "column roundtrip failed: {c:?}"
            );
        }
    }

    #[test]
    fn floor_collapses_structured_columns() {
        // sequential ids (delta 1) and a quadratic both collapse far below raw 8 bytes/value.
        let ids: Vec<i64> = (1000..2000).collect();
        let enc = encode_i64(&ids);
        let raw = ids.len() * 8;
        assert!(
            enc.len() * 20 < raw,
            "floor should crush a linear column: {} vs raw {}",
            enc.len(),
            raw
        );
    }

    #[test]
    fn exact_recurrence_is_o1_storage_and_random_access() {
        // an exact recurrence column → O(1) output (seed only) AND O(1) random access by coordinate.
        let arith: Vec<i64> = (0..50_000).map(|i| 1000 + 7 * i).collect();
        let quad: Vec<i64> = (0..50_000).map(|i| 3 * i * i - 5 * i + 11).collect();
        for c in [&arith, &quad] {
            let enc = encode_i64(c);
            assert!(
                enc.len() <= 12,
                "exact column must store O(1): {} bytes",
                enc.len()
            );
            assert_eq!(decode_i64(&enc), *c); // whole-column round-trip
            for &i in &[0usize, 1, 2, 3, 17, 9999, c.len() - 1] {
                assert_eq!(get_at(&enc, i), Some(c[i]), "O(1) random access at {i}");
            }
            assert_eq!(get_at(&enc, c.len()), None); // out of range
        }
        // a noisy column stores floored → random access not O(1) (None; caller uses decode_i64).
        let noisy = vec![5i64, 100, -3, 42, 7, 999, -250, 8];
        let enc = encode_i64(&noisy);
        assert_eq!(decode_i64(&enc), noisy);
        assert_eq!(get_at(&enc, 0), None);
    }
}
