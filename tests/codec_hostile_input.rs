//! Hostile-input tests for the `codec` decoders (audit MEM-03/04/05/06/09/10, MQ-02).
//!
//! Every public decode entry point is fed deterministic, seeded garbage: truncations of valid
//! encodings, byte-mutated encodings, pure random bytes and headers that declare absurd lengths. A
//! decoder must neither panic nor ask the allocator for a huge block. The allocator below records the
//! largest request each thread makes and refuses anything above [`LIMIT`] (a refused request aborts
//! the process, exactly as a real out-of-memory does, so an unguarded length prefix fails loudly).
#![cfg(feature = "codec")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use wirefold::codec;

/// The largest single allocation any decoder may request for the tiny inputs used here (the
/// documented cap on constant-size-input expansion is far below this).
const LIMIT: usize = 256 << 20;

thread_local! {
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static LAST_PANIC: RefCell<String> = const { RefCell::new(String::new()) };
    static FUZZING: Cell<bool> = const { Cell::new(false) };
}

struct Tracking;

fn note(size: usize) -> bool {
    PEAK.with(|p| {
        if size > p.get() {
            p.set(size);
        }
    });
    size <= LIMIT
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !note(l.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if !note(l.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if !note(new) {
            return std::ptr::null_mut();
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

fn install_hook() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if FUZZING.with(Cell::get) {
                LAST_PANIC.with(|m| *m.borrow_mut() = info.to_string());
            } else {
                prev(info);
            }
        }));
    });
}

// ── deterministic generator ────────────────────────────────────────────────────────────────────
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn uvarint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn samples() -> Vec<Vec<u8>> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let noise: Vec<u8> = (0..1500).map(|_| rng.next() as u8).collect();
    let mut broken: Vec<u8> = (0..977)
        .map(|i| (i * 31 % 251) as u8)
        .collect::<Vec<_>>()
        .repeat(2);
    for _ in 0..40 {
        let p = rng.below(broken.len());
        broken[p] = rng.next() as u8;
    }
    vec![
        vec![],
        b"a".to_vec(),
        vec![0u8; 3000],
        b"the quick brown fox jumps over the lazy dog. ".repeat(40),
        noise,
        (0..=255u8).cycle().take(2000).collect(),
        broken,
    ]
}

/// Hostile headers: optional tag byte, then one or two varints that claim enormous sizes, then a
/// little tail. These are the "constant-size input, huge declared length" shapes.
fn hostile_headers() -> Vec<Vec<u8>> {
    let claims = [
        1u64 << 14,
        1 << 26,
        1 << 32,
        1 << 40,
        1 << 62,
        u64::MAX,
        u64::MAX / 2,
    ];
    let tags: [&[u8]; 8] = [&[], &[0], &[1], &[2], &[3], &[0xFE], &[0xFF], &[0x04]];
    let tails: [&[u8]; 3] = [&[], &[0, 0, 0, 0, 0, 0, 0, 0], &[0xFF; 16]];
    let mut out = Vec::new();
    for tag in tags {
        for &a in &claims {
            for tail in tails {
                let mut v = tag.to_vec();
                v.extend(uvarint(a));
                v.extend_from_slice(tail);
                out.push(v);
                for &b in &claims[..5] {
                    let mut v = tag.to_vec();
                    v.extend(uvarint(a));
                    v.extend(uvarint(b));
                    v.extend_from_slice(tail);
                    out.push(v);
                    let mut v = tag.to_vec();
                    v.extend(uvarint(1));
                    v.extend(uvarint(a));
                    v.extend(uvarint(b));
                    v.extend_from_slice(tail);
                    out.push(v);
                }
            }
        }
    }
    out
}

fn inputs_for(seeds: &[Vec<u8>], with_headers: bool) -> Vec<Vec<u8>> {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut out: Vec<Vec<u8>> = Vec::new();
    for s in seeds {
        out.push(s.clone());
        // every proper prefix of short encodings, ~100 sampled prefixes of long ones
        if s.len() <= 120 {
            for l in 0..s.len() {
                out.push(s[..l].to_vec());
            }
        } else {
            for _ in 0..100 {
                let l = rng.below(s.len());
                out.push(s[..l].to_vec());
            }
        }
        // mutations: flip 1..=4 bytes
        for _ in 0..30 {
            let mut m = s.clone();
            if m.is_empty() {
                break;
            }
            for _ in 0..=rng.below(4) {
                let p = rng.below(m.len());
                m[p] = rng.next() as u8;
            }
            out.push(m);
        }
        // mutation of the header only (the first 12 bytes steer every length decision)
        for _ in 0..30 {
            let mut m = s.clone();
            if m.is_empty() {
                break;
            }
            let p = rng.below(m.len().min(12));
            m[p] = rng.next() as u8;
            out.push(m);
        }
    }
    for _ in 0..300 {
        let l = rng.below(48);
        out.push((0..l).map(|_| rng.next() as u8).collect());
    }
    if with_headers {
        out.extend(hostile_headers());
    }
    out
}

/// Run `f` over every generated input; fail if any call panics or requests more than [`LIMIT`].
fn fuzz(name: &str, seeds: &[Vec<u8>], f: &dyn Fn(&[u8])) {
    install_hook();
    let mut panics = Vec::new();
    let mut huge = Vec::new();
    for input in inputs_for(seeds, true) {
        PEAK.with(|p| p.set(0));
        FUZZING.with(|c| c.set(true));
        let r = catch_unwind(AssertUnwindSafe(|| f(&input)));
        FUZZING.with(|c| c.set(false));
        if r.is_err() {
            panics.push((input.clone(), LAST_PANIC.with(|m| m.borrow().clone())));
        }
        let peak = PEAK.with(Cell::get);
        if peak > LIMIT {
            huge.push((input.len(), peak));
        }
    }
    let show = |b: &[u8]| {
        let h: String = b.iter().take(24).map(|x| format!("{x:02x}")).collect();
        format!(
            "{h}{} ({} bytes)",
            if b.len() > 24 { ".." } else { "" },
            b.len()
        )
    };
    assert!(
        panics.is_empty(),
        "{name}: {} inputs panicked; first: {} -> {}",
        panics.len(),
        show(&panics[0].0),
        panics[0].1
    );
    assert!(
        huge.is_empty(),
        "{name}: {} inputs requested more than {LIMIT} bytes (first: {:?})",
        huge.len(),
        huge[0]
    );
}

fn seeds_of(enc: impl Fn(&[u8]) -> Vec<u8>) -> Vec<Vec<u8>> {
    samples().iter().map(|s| enc(s)).collect()
}

macro_rules! fuzz_bytes {
    ($test:ident, $enc:expr, $dec:expr) => {
        #[test]
        fn $test() {
            let seeds = seeds_of($enc);
            fuzz(stringify!($test), &seeds, &|b: &[u8]| {
                let _ = $dec(b);
            });
        }
    };
}

// ── byte-stream decoders (Vec<u8> out) ─────────────────────────────────────────────────────────
fuzz_bytes!(
    fuzz_cm_decompress,
    |s| codec::cm::compress(codec::cm::Order::One, s),
    codec::cm::decompress
);
fuzz_bytes!(
    fuzz_lz_turbo,
    codec::lz::compress_turbo,
    codec::lz::decompress_turbo
);
fuzz_bytes!(
    fuzz_lz_seq,
    codec::lz::compress_seq,
    codec::lz::decompress_seq
);
fuzz_bytes!(
    fuzz_lz_seq_fast,
    codec::lz::compress_seq_fast,
    codec::lz::decompress_seq
);
fuzz_bytes!(
    fuzz_lz_decompress,
    codec::lz::compress,
    codec::lz::decompress
);
fuzz_bytes!(
    fuzz_lz_hybrid,
    codec::lz::compress_hybrid,
    codec::lz::decompress_hybrid
);
fuzz_bytes!(
    fuzz_readout,
    codec::readout::compress,
    codec::readout::decompress
);
fuzz_bytes!(fuzz_huf4h, codec::huf::compress4h, codec::huf::decompress4h);
fuzz_bytes!(fuzz_tans, codec::tans::compress, codec::tans::decompress);
fuzz_bytes!(fuzz_tans4, codec::tans::compress4, codec::tans::decompress4);
fuzz_bytes!(
    fuzz_tans4_o1,
    codec::tans::compress4_o1,
    codec::tans::decompress4_o1
);
fuzz_bytes!(fuzz_decompress, codec::compress, codec::decompress);
fuzz_bytes!(
    fuzz_decompress_framed,
    |s| codec::compress_at(codec::cm::Order::One, s),
    codec::decompress_framed
);
fuzz_bytes!(
    fuzz_decompress_shaped_dense,
    |s| codec::compress_shaped(codec::Bias::Dense, s),
    codec::decompress_shaped
);
fuzz_bytes!(
    fuzz_decompress_shaped_fast,
    |s| codec::compress_shaped(codec::Bias::Fast, s),
    codec::decompress_shaped
);

#[test]
fn fuzz_tans4_into_leaves_dst_untouched_on_garbage() {
    let seeds = seeds_of(codec::tans::compress4);
    fuzz("fuzz_tans4_into", &seeds, &|b: &[u8]| {
        let mut dst = vec![0xAB; 4];
        let before = dst.len();
        codec::tans::decompress4_into(b, &mut dst);
        assert!(dst.len() >= before && dst[..before] == [0xAB; 4]);
    });
}

#[test]
fn fuzz_gen_decoders() {
    let seeds = seeds_of(|s| s.to_vec());
    fuzz("fuzz_gen", &seeds, &|b: &[u8]| {
        for k in 0..10 {
            let _ = codec::gen::delta_decode(b, k);
            let _ = codec::gen::haar_decode(b, k);
            let _ = codec::gen::planes_decode(b, k);
        }
    });
}

// ── integer column decoders ────────────────────────────────────────────────────────────────────
fn i64_columns() -> Vec<Vec<i64>> {
    let mut rng = Rng(7);
    vec![
        vec![],
        vec![5],
        (0..500).collect(),
        (0..300).map(|i| i * i * 3 + 7).collect(),
        (0..400).map(|_| (rng.next() % 1000) as i64 - 500).collect(),
        (0..400).map(|_| rng.next() as i64).collect(),
        vec![9; 200],
    ]
}

macro_rules! fuzz_i64 {
    ($test:ident, $enc:expr, $dec:expr) => {
        #[test]
        fn $test() {
            let seeds: Vec<Vec<u8>> = i64_columns().iter().map(|c| $enc(c)).collect();
            fuzz(stringify!($test), &seeds, &|b: &[u8]| {
                let _ = $dec(b);
            });
        }
    };
}
fuzz_i64!(fuzz_bitunpack_i64, codec::bitpack_i64, codec::bitunpack_i64);
fuzz_i64!(fuzz_decode_i64, codec::encode_i64, codec::decode_i64);

#[test]
fn fuzz_get_at() {
    let seeds: Vec<Vec<u8>> = i64_columns().iter().map(|c| codec::encode_i64(c)).collect();
    fuzz("fuzz_get_at", &seeds, &|b: &[u8]| {
        for i in [0usize, 1, 2, 3, 100, usize::MAX] {
            let _ = codec::get_at(b, i);
        }
    });
}

// ── self-describing term decoders ──────────────────────────────────────────────────────────────
fn terms() -> Vec<codec::syntax::SyntaxTerm> {
    use codec::syntax::{Atom, SyntaxTerm};
    vec![
        SyntaxTerm::Nb,
        SyntaxTerm::B(Atom::Bool(true)),
        SyntaxTerm::B(Atom::Num("12345678901".into())),
        SyntaxTerm::B(Atom::Str("hello, world".repeat(5))),
        SyntaxTerm::Unknown,
        SyntaxTerm::Fourth(vec![
            SyntaxTerm::B(Atom::Num("1".into())),
            SyntaxTerm::B(Atom::Num("2.5".into())),
            SyntaxTerm::Nb,
        ]),
        SyntaxTerm::Bu(vec![
            ("name".into(), SyntaxTerm::B(Atom::Str("x".into()))),
            (
                "list".into(),
                SyntaxTerm::Fourth(vec![SyntaxTerm::Bu(vec![(
                    "k".into(),
                    SyntaxTerm::B(Atom::Bool(false)),
                )])]),
            ),
        ]),
    ]
}

#[test]
fn fuzz_msgpack_decode() {
    let seeds: Vec<Vec<u8>> = terms().iter().map(codec::msgpack::encode).collect();
    fuzz("fuzz_msgpack", &seeds, &|b: &[u8]| {
        let _ = codec::msgpack::decode(b);
    });
}

#[test]
fn msgpack_deep_nesting_is_an_error_not_a_stack_overflow() {
    // 200_000 nested one-element arrays: recursion depth must be bounded. The decoder stops at depth 128,
    // which needs about 95 KiB in a release build and about 0.9 MiB in a debug build (more under a
    // sanitizer), while 200,000 unbounded levels would need over 150 MB, so 8 MiB separates the two
    // with room on both sides.
    let mut bytes = vec![0x91u8; 200_000];
    bytes.push(0xc0);
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || codec::msgpack::decode(&bytes).is_err())
        .unwrap();
    assert!(h.join().expect("decode must not overflow the stack"));
}

#[test]
fn fuzz_decerealize_term() {
    let seeds: Vec<Vec<u8>> = terms().iter().map(codec::syntax::cerealize_term).collect();
    fuzz("fuzz_decerealize_term", &seeds, &|b: &[u8]| {
        let _ = codec::syntax::decerealize_term(b);
        let _ = codec::syntax::decerealize_http(b);
    });
}

#[test]
fn fuzz_proto_decode() {
    use codec::proto::{ProtoRegistry, ProtoSchema};
    use codec::syntax::{Atom, SyntaxTerm};
    const SRC: &str = r"
    message Inner { optional string s = 1; optional int32 n = 2; }
    message Outer {
      optional string name = 1;
      optional int32 count = 2;
      optional bool flag = 3;
      optional Inner inner = 4;
      repeated string tags = 5;
      optional bytes blob = 6;
    }";
    let reg = ProtoRegistry::parse_messages(SRC, &["Inner", "Outer"]);
    let schema = ProtoSchema::parse(SRC, "Outer").unwrap();
    let term = SyntaxTerm::Bu(vec![
        ("name".into(), SyntaxTerm::B(Atom::Str("abc".into()))),
        ("count".into(), SyntaxTerm::B(Atom::Num("300".into()))),
        ("flag".into(), SyntaxTerm::B(Atom::Bool(true))),
        (
            "inner".into(),
            SyntaxTerm::Bu(vec![("s".into(), SyntaxTerm::B(Atom::Str("zz".into())))]),
        ),
    ]);
    let seeds = vec![codec::proto::encode(&term, &schema, &reg)];
    fuzz("fuzz_proto", &seeds, &|b: &[u8]| {
        let _ = codec::proto::decode(b, &schema, &reg);
    });
}

// ── zero-copy wire views ───────────────────────────────────────────────────────────────────────
struct Aligned(Vec<u64>, usize);
fn aligned(b: &[u8]) -> Aligned {
    let mut w = vec![0u64; b.len().div_ceil(8).max(1)];
    // SAFETY: `w` owns at least `b.len()` bytes; u64 has no invalid bit patterns.
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), w.as_mut_ptr().cast::<u8>(), b.len()) };
    Aligned(w, b.len())
}
impl Aligned {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the allocation is `>= self.1` bytes and 8-aligned.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.1) }
    }
}

fn ragged_seed() -> Vec<u8> {
    let items: Vec<&[u8]> = vec![b"alpha", b"", b"be", b"gamma-delta"];
    codec::web::pack_ragged(&items).as_bytes().to_vec()
}

fn exercise_ragged(b: &[u8]) {
    let a = aligned(b);
    if let Ok(r) = codec::web::Ragged::open(a.bytes()) {
        for i in 0..r.len().min(10_000) {
            let _ = r.get(i);
            let _ = r.get_as::<u32>(i);
        }
    }
}

#[test]
fn fuzz_ragged_open_then_get() {
    let seeds = vec![ragged_seed()];
    fuzz("fuzz_ragged", &seeds, &exercise_ragged);
}

#[test]
fn ragged_interior_offsets_are_checked_at_open() {
    // [n=2][off0=0][off1=10][off2=5] + 5 leaf bytes: the audit's MEM-09 input.
    let mut b = Vec::new();
    for w in [2u64, 0, 10, 5] {
        b.extend_from_slice(&w.to_ne_bytes());
    }
    b.extend_from_slice(b"abcde");
    let a = aligned(&b);
    assert!(
        codec::web::Ragged::open(a.bytes()).is_err(),
        "non-monotone interior offsets must be rejected by open()"
    );
    exercise_ragged(&b);
}

#[test]
fn fuzz_sections_open_then_section() {
    let r = ragged_seed();
    let s = codec::web::pack_sections(&[&r, &r]).as_bytes().to_vec();
    fuzz("fuzz_sections", &[s], &|b: &[u8]| {
        let a = aligned(b);
        if let Ok(s) = codec::web::Sections::open(a.bytes()) {
            for j in 0..s.len().min(100) {
                let sec = s.section(j);
                let _ = codec::web::Ragged::open(sec).map(|r| {
                    for i in 0..r.len().min(1000) {
                        let _ = r.get(i);
                    }
                });
                let _ = codec::web::Wire::<u32>::open(sec).map(|w| w.as_slice().len());
            }
        }
    });
}

// ── MEM-03 / MEM-04 / MEM-05 / MEM-06: the audit's own inputs ───────────────────────────────────
#[test]
fn mem03_decompress4_into_does_not_expose_unwritten_bytes_after_a_panic() {
    install_hook();
    let mut dst = b"keep".to_vec();
    let r = catch_unwind(AssertUnwindSafe(|| {
        codec::tans::decompress4_into(&[0xE8, 0x07, 0x05], &mut dst)
    }));
    drop(r); // a panic is tolerated by this test; leaking uninitialised bytes is not
    assert_eq!(dst, b"keep", "dst must be untouched when decoding fails");
}

#[test]
fn mem04_hostile_length_prefixes_return_instead_of_aborting() {
    // framed: n = 2^40, order 0, stride 0
    let mut framed = vec![codec::cm::VERSION];
    framed.extend(uvarint(1 << 40));
    framed.extend([0, 0, 0, 0]);
    assert!(codec::decompress_framed(&framed).is_empty());
    // cm: n = 2^40, order 1, stride = 0, 8 stream bytes
    let mut cm = vec![codec::cm::VERSION];
    cm.extend(uvarint(1 << 40));
    cm.extend([1, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(codec::cm::decompress(&cm).is_empty());
    // decode_i64 EXACT: len = 2^40
    let mut exact = vec![0xFF];
    exact.extend(uvarint(1 << 40));
    assert!(codec::decode_i64(&exact).is_empty());
    // msgpack array32 with 2^32-1 elements
    assert!(codec::msgpack::decode(&[0xdd, 0xff, 0xff, 0xff, 0xff]).is_err());
}

#[test]
fn mem05_truncated_input_returns_empty_or_none() {
    let data = b"the quick brown fox jumps over the lazy dog. ".repeat(20);
    let enc = codec::lz::compress_turbo(&data);
    assert_eq!(
        codec::lz::decompress_turbo(&enc),
        data,
        "valid input still round-trips"
    );
    for cut in [0, 1, 2, enc.len() / 2, enc.len() - 1] {
        let _ = codec::lz::decompress_turbo(&enc[..cut]); // must not panic
    }
    // get_at on a truncated EXACT header
    assert_eq!(codec::get_at(&[0xFF], 0), None);
    assert_eq!(codec::get_at(&[0xFF, 0x80], 0), None);
}

#[test]
fn mem06_constant_size_input_cannot_demand_unbounded_output() {
    // BITPACK: [tag, order, len = 2^26, min = 0, width = 1] and no packed bytes at all.
    let mut b = vec![0xFE, 0x00];
    b.extend(uvarint(1 << 26));
    b.extend([0x00, 0x01]);
    let out = codec::bitunpack_i64(&b);
    assert!(
        out.len() <= 8,
        "8 input bytes expanded to {} values",
        out.len()
    );
    // EXACT with a length beyond the documented cap
    let mut e = vec![0xFF];
    e.extend(uvarint(1 << 40));
    assert!(codec::bitunpack_i64(&e).len() <= (1 << 24));
}

// ── the checked `try_*` variants: Ok on valid input, a typed error on malformed input ─────────────
#[test]
fn try_variants_round_trip_valid_input() {
    let data = b"the quick brown fox jumps over the lazy dog. ".repeat(60);
    assert_eq!(
        codec::cm::try_decompress(&codec::cm::compress(codec::cm::Order::One, &data)).unwrap(),
        data
    );
    assert_eq!(
        codec::lz::try_decompress_turbo(&codec::lz::compress_turbo(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::lz::try_decompress(&codec::lz::compress(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::lz::try_decompress_hybrid(&codec::lz::compress_hybrid(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::readout::try_decompress(&codec::readout::compress(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::huf::try_decompress4h(&codec::huf::compress4h(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::tans::try_decompress(&codec::tans::compress(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::tans::try_decompress4(&codec::tans::compress4(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::tans::try_decompress4_o1(&codec::tans::compress4_o1(&data.repeat(3))).unwrap(),
        data.repeat(3)
    );
    assert_eq!(
        codec::try_decompress(&codec::compress(&data)).unwrap(),
        data
    );
    assert_eq!(
        codec::try_decompress_framed(&codec::compress_at(codec::cm::Order::One, &data)).unwrap(),
        data
    );
    let col: Vec<i64> = (0..300).map(|i| i * 7 - 40).collect();
    assert_eq!(
        codec::try_decode_i64(&codec::encode_i64(&col)).unwrap(),
        col
    );
    let noisy: Vec<i64> = (0..300).map(|i| (i * 7919) % 1013).collect();
    assert_eq!(
        codec::try_bitunpack_i64(&codec::bitpack_i64(&noisy)).unwrap(),
        noisy
    );
}

#[test]
fn try_variants_report_malformed_input() {
    use codec::DecodeError;
    let data = b"the quick brown fox jumps over the lazy dog. ".repeat(60);
    let enc = codec::cm::compress(codec::cm::Order::One, &data);
    assert_eq!(
        codec::cm::try_decompress(&enc[..enc.len() / 2]).unwrap_err(),
        DecodeError::Truncated
    );
    let mut huge = vec![codec::cm::VERSION];
    huge.extend(uvarint(1 << 40));
    huge.extend([1, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        codec::cm::try_decompress(&huge).unwrap_err(),
        DecodeError::TooLarge
    );
    assert_eq!(
        codec::tans::try_decompress4(&[0xE8, 0x07, 0x05]).unwrap_err(),
        DecodeError::Truncated
    );
    let mut exact = vec![0xFF];
    exact.extend(uvarint(1 << 40));
    assert_eq!(
        codec::try_decode_i64(&exact).unwrap_err(),
        DecodeError::TooLarge
    );
    // an explicit cap: the same header-only column is fine when the caller allows it
    let mut small = vec![0xFF];
    small.extend(uvarint(1000));
    small.extend([2, 2, 2]);
    assert_eq!(
        codec::try_decode_i64_with_limit(&small, 1000)
            .unwrap()
            .len(),
        1000
    );
    assert_eq!(
        codec::try_decode_i64_with_limit(&small, 999).unwrap_err(),
        DecodeError::TooLarge
    );
}

// ── derive-generated decoders (MEM-10, MQ-02) ──────────────────────────────────────────────────
#[cfg(feature = "derive")]
mod derived {
    use super::*;
    use wirefold::codec::web::CollapseElem;
    use wirefold::Collapse;

    #[derive(Collapse, Clone, Debug, PartialEq)]
    struct Row {
        id: u64,
        flag: bool,
        name: String,
        tags: Vec<u32>,
    }

    #[derive(Collapse, Clone, Debug, PartialEq)]
    enum Ev {
        Unit,
        One(u32),
        Two { x: u8, y: u64 },
    }

    fn rows() -> Vec<Row> {
        (0..6u64)
            .map(|i| Row {
                id: i,
                flag: i % 2 == 0,
                name: format!("row-{i}"),
                tags: (0..i as u32).collect(),
            })
            .collect()
    }

    fn evs() -> Vec<Ev> {
        vec![Ev::Unit, Ev::One(7), Ev::Two { x: 1, y: 2 }, Ev::One(9)]
    }

    fn open_all_rows(b: &[u8]) {
        let a = aligned(b);
        if let Ok(v) = RowView::collapse_open(a.bytes()) {
            let _ = v.collapse_to_vec();
            for i in 0..v.collapse_len().min(1000) {
                let _ = v.get_ref(i);
            }
        }
    }

    fn open_all_evs(b: &[u8]) {
        let a = aligned(b);
        if let Ok(v) = EvView::open(a.bytes()) {
            let _ = v.to_vec();
        }
    }

    #[test]
    fn fuzz_derived_struct_decoder() {
        let seeds = vec![Row::collapse_pack(&rows()).as_bytes().to_vec()];
        fuzz("fuzz_derived_struct", &seeds, &open_all_rows);
    }

    #[test]
    fn fuzz_derived_enum_decoder() {
        let seeds = vec![Ev::collapse_pack(&evs()).as_bytes().to_vec()];
        fuzz("fuzz_derived_enum", &seeds, &open_all_evs);
    }

    /// MEM-10: a tag byte no variant owns must be rejected, not reach `unreachable!()`.
    #[test]
    fn unknown_enum_tag_is_rejected() {
        let mut b = Ev::collapse_pack(&evs()).as_bytes().to_vec();
        // section 0 is the tag Wire<u8>: [count][tags..]; find it through the Sections header.
        let a = aligned(&b);
        let s = codec::web::Sections::open(a.bytes()).unwrap();
        let tag_sec = s.section(0).as_ptr() as usize - a.bytes().as_ptr() as usize;
        b[tag_sec + 8] = 200;
        open_all_evs(&b);
        let a = aligned(&b);
        assert!(EvView::open(a.bytes()).is_err());
    }

    /// MEM-10: non-UTF-8 bytes in a `String` column must be an error, not a panic.
    #[test]
    fn invalid_utf8_in_string_field_is_rejected() {
        let mut b = Row::collapse_pack(&rows()).as_bytes().to_vec();
        let pos = b.windows(5).position(|w| w == b"row-3").unwrap();
        b[pos] = 0xFF;
        b[pos + 1] = 0xFE;
        open_all_rows(&b);
        let a = aligned(&b);
        assert!(RowView::collapse_open(a.bytes()).is_err());
        assert!(Row::collapse_decode(a.bytes()).is_err());
    }

    /// The derived `collapse_decode` returns `Result` and round-trips valid data.
    #[test]
    fn collapse_decode_round_trips_and_rejects_garbage() {
        let src = rows();
        let seed = Row::collapse_pack(&src);
        assert_eq!(Row::collapse_decode(seed.as_bytes()).unwrap(), src);
        let seed = Ev::collapse_pack(&evs());
        assert_eq!(Ev::collapse_decode(seed.as_bytes()).unwrap(), evs());
        assert!(Row::collapse_decode(&[]).is_err());
        assert!(Ev::collapse_decode(&[1, 2, 3]).is_err());
    }
}

#[test]
fn cm_stream_version_order_and_header_are_checked() {
    use codec::cm::{self, Order, VERSION};
    use codec::DecodeError;
    let data = b"the quick brown fox jumps over the lazy dog. ".repeat(60);
    // every supported order round-trips, and the stream begins with the version byte
    for order in [Order::Zero, Order::One] {
        let enc = cm::compress(order, &data);
        assert_eq!(enc[0], VERSION);
        assert_eq!(cm::try_decompress(&enc).unwrap(), data, "{order:?}");
    }
    let enc = cm::compress(Order::One, &data);
    // an unknown version is refused by name, whatever follows
    for v in [0u8, VERSION + 1, 0xFF] {
        let mut other = enc.clone();
        other[0] = v;
        assert_eq!(
            cm::try_decompress(&other).unwrap_err(),
            DecodeError::UnsupportedVersion(v)
        );
    }
    // a stream written before the version byte existed (it began with the length) is not read
    assert!(cm::try_decompress(&enc[1..]).is_err());
    // truncated headers: nothing, the version only, then each header varint cut short
    assert_eq!(cm::try_decompress(&[]).unwrap_err(), DecodeError::Truncated);
    assert_eq!(
        cm::try_decompress(&[VERSION]).unwrap_err(),
        DecodeError::Truncated
    );
    assert!(cm::try_decompress(&[VERSION, 5]).is_err());
    assert!(cm::try_decompress(&[VERSION, 5, 1]).is_err());
    // an order value outside the enum is malformed, never silently mapped to another order: take a
    // valid stream (a short input, so the length is one varint byte and the order byte is at 2) and
    // change only that byte
    let short = b"abcabcabcabcabc".repeat(6);
    let valid = cm::compress(Order::One, &short);
    assert_eq!((valid[1], valid[2]), (short.len() as u8, 1));
    assert_eq!(cm::try_decompress(&valid).unwrap(), short);
    for bad in [2u8, 3, 5, 127] {
        let mut b = valid.clone();
        b[2] = bad;
        assert_eq!(cm::try_decompress(&b).unwrap_err(), DecodeError::Malformed);
    }
}
