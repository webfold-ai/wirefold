//! Property tests (proptest) for the codec's round-trip laws.
//!
//! The oracle is the identity: `decode(encode(x)) == x`. Nothing here re-implements a codec. proptest shrinks a
//! failing input to a minimal one and persists it under `tests/properties.proptest-regressions` (commit that
//! file). Hostile-input behaviour (a decoder must return an error, never panic or over-allocate) stays in
//! `tests/codec_hostile_input.rs` and `fuzz/`; the two hostile properties below are the cheap in-`cargo test`
//! form of the same rule.
#![cfg(feature = "codec")]

use proptest::prelude::*;
use wirefold::codec::syntax::{Atom, SyntaxTerm};
use wirefold::codec::{
    bitpack_i64, bitunpack_i64, cm, compress, compress_at, decode_i64, decompress,
    decompress_framed, encode_i64, get_at, msgpack, try_decode_i64, try_decompress,
    try_decompress_framed,
};

fn bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..2048)
}

/// Text-shaped bytes: the coder picks a different face for these than for noise.
fn texty_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop::sample::select(b"abcde fghij\n{}[]\":,0123456789".to_vec()),
        0..2048,
    )
}

/// Columns of i64 across the whole range, including the extremes.
fn column() -> impl Strategy<Value = Vec<i64>> {
    prop::collection::vec(
        prop_oneof![
            any::<i64>(),
            -1_000..1_000_i64,
            Just(i64::MIN),
            Just(i64::MAX),
            Just(0)
        ],
        0..256,
    )
}

/// An exact degree-2 recurrence `a + b*i + c*i*i`, the shape `encode_i64` collapses to its seeds.
fn quadratic_column() -> impl Strategy<Value = Vec<i64>> {
    (
        -1_000_000_i64..1_000_000,
        -1_000_i64..1_000,
        -50_i64..50,
        0_usize..300,
    )
        .prop_map(|(a, b, c, n)| (0..n as i64).map(|i| a + b * i + c * i * i).collect())
}

fn atom() -> impl Strategy<Value = Atom> {
    prop_oneof![
        any::<bool>().prop_map(Atom::Bool),
        any::<i32>().prop_map(|n| Atom::Num(n.to_string())),
        ".{0,12}".prop_map(Atom::Str),
    ]
}

fn term() -> impl Strategy<Value = SyntaxTerm> {
    let leaf = prop_oneof![
        Just(SyntaxTerm::Nb),
        Just(SyntaxTerm::Unknown),
        atom().prop_map(SyntaxTerm::B)
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(SyntaxTerm::Fourth),
            prop::collection::vec((".{0,8}", inner), 0..4).prop_map(SyntaxTerm::Bu),
        ]
    })
}

/// The term with every map's keys in byte order (stable), the form msgpack writes and reads back.
fn canonical(t: &SyntaxTerm) -> SyntaxTerm {
    match t {
        SyntaxTerm::Fourth(xs) => SyntaxTerm::Fourth(xs.iter().map(canonical).collect()),
        SyntaxTerm::Bu(fields) => {
            let mut fs: Vec<(String, SyntaxTerm)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect();
            fs.sort_by(|a, b| a.0.cmp(&b.0));
            SyntaxTerm::Bu(fs)
        }
        other => other.clone(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn compress_then_decompress_is_the_identity(data in bytes()) {
        let packed = compress(&data);
        prop_assert_eq!(decompress(&packed), data.clone());
        prop_assert_eq!(try_decompress(&packed), Ok(data));
    }

    #[test]
    fn the_face_selector_round_trips_text_shaped_data(data in texty_bytes()) {
        prop_assert_eq!(decompress(&compress(&data)), data);
    }

    #[test]
    fn the_raw_cm_coder_round_trips_at_both_orders(data in bytes()) {
        for order in [cm::Order::Zero, cm::Order::One] {
            let packed = compress_at(order, &data);
            prop_assert_eq!(decompress_framed(&packed), data.clone(), "order {:?}", order);
        }
    }

    #[test]
    fn an_integer_column_round_trips_through_encode(vals in column()) {
        prop_assert_eq!(try_decode_i64(&encode_i64(&vals)), Ok(vals.clone()));
        prop_assert_eq!(decode_i64(&encode_i64(&vals)), vals);
    }

    #[test]
    fn an_integer_column_round_trips_through_bitpack(vals in column()) {
        prop_assert_eq!(bitunpack_i64(&bitpack_i64(&vals)), vals);
    }

    /// An exact recurrence is stored as its seeds, and random access evaluates the closed form: it must agree
    /// with the column it was built from at every index, and be absent past the end.
    #[test]
    fn closed_form_random_access_matches_the_column(vals in quadratic_column()) {
        let enc = encode_i64(&vals);
        for (i, v) in vals.iter().enumerate() {
            prop_assert_eq!(get_at(&enc, i), Some(*v), "index {}", i);
        }
        prop_assert_eq!(get_at(&enc, vals.len()), None);
        prop_assert_eq!(decode_i64(&enc), vals);
    }

    /// Maps go out with their keys in byte order (documented in `codec::msgpack`, for determinism), so the law is
    /// "the identity up to map-key order": decoding gives the canonical form, and encoding is blind to the order
    /// the keys arrived in. (The first version of this test asserted exact identity; proptest shrank the failure to
    /// `Bu([("a", Nb), ("\0", Nb)])`, which decodes with `"\0"` first. That is the documented behaviour, not a bug.)
    #[test]
    fn msgpack_round_trips_a_term_up_to_map_key_order(t in term()) {
        let bytes = msgpack::encode(&t);
        prop_assert_eq!(msgpack::decode(&bytes), Ok(canonical(&t)));
        prop_assert_eq!(msgpack::encode(&canonical(&t)), bytes);
    }

    /// Hostile bytes: an error is fine, a panic is not. (Allocation bounds live in `codec_hostile_input.rs`.)
    #[test]
    fn decoders_never_panic_on_arbitrary_bytes(data in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = try_decompress_framed(&data);
        let _ = try_decompress(&data);
        let _ = try_decode_i64(&data);
        let _ = msgpack::decode(&data);
    }
}
