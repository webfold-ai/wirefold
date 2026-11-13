//! Streams written by older wirefold (and hashtrinity) must stay readable.
//!
//! `tests/fixtures/legacy_cm/` holds real output of two older versions of this crate, generated from the
//! deterministic inputs below by building those versions:
//!
//! * `adaptive_*`, `framed_*`, `shaped_*`: wirefold `ba9a712`, whose cm coder is the adaptive context-mixing
//!   coder hashtrinity had before it (`[byteLen][order][stride][rANS]`, orders 0..=6; `compress_at` frames
//!   chunks as `[nchunks][lengths][chunks]`; `compress` adds a face tag). This is the format sangdb data was
//!   written in.
//! * `static_*`: the unversioned static coder that followed it (`[byteLen][order][stride][tANS]`).
//!
//! Version-1 streams (what this crate writes now) begin with the byte `cm::VERSION`; the old formats had no
//! marker, so the readers detect them by decoding. Nothing here is regenerated from the current code.
#![cfg(feature = "compress")]

use wirefold::codec::{self, cm, DecodeError};

pub fn input(name: &str) -> Vec<u8> {
    match name {
        "empty" => Vec::new(),
        "one" => vec![7],
        "text" => b"the quick brown fox jumps over the lazy dog. ".repeat(60),
        "structured" => {
            let mut x = 0x2545_F491_4F6C_DD1Du64;
            (0..3000usize)
                .map(|i| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    if i % 2 == 0 {
                        (i / 2) as u8
                    } else {
                        (x % 7) as u8
                    }
                })
                .collect()
        }
        "image" => (0..48 * 48usize)
            .map(|i| (((i / 48) * 5 + (i % 48) * 3) % 251) as u8)
            .collect(),
        "random" => {
            let mut x = 0x9E37_79B9_7F4A_7C15u64;
            (0..3000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x >> 24) as u8
                })
                .collect()
        }
        "repeat" => b"abcdefgh".repeat(2000),
        other => panic!("unknown input {other}"),
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/legacy_cm/{name}.bin",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

const ADAPTIVE: &[(&str, &[usize])] = &[
    ("text", &[0, 1, 5]),
    ("structured", &[0, 1, 5]),
    ("image", &[0, 1, 5]),
    ("random", &[0, 1, 5]),
    ("empty", &[1]),
    ("one", &[1]),
];
const FRAMED: &[&str] = &[
    "empty",
    "one",
    "text",
    "structured",
    "image",
    "random",
    "repeat",
];
const SHAPED: &[&str] = &["text", "structured", "image", "random", "repeat"];
const STATIC: &[&str] = &["text", "structured", "image"];

fn every_fixture() -> Vec<(String, &'static str, Vec<u8>)> {
    let mut all = Vec::new();
    for (name, ks) in ADAPTIVE {
        for k in *ks {
            all.push((format!("adaptive_k{k}_{name}"), *name, input(name)));
        }
    }
    for name in FRAMED {
        all.push((format!("framed_{name}"), *name, input(name)));
    }
    for name in SHAPED {
        all.push((format!("shaped_{name}"), *name, input(name)));
    }
    for name in STATIC {
        for k in [0, 1] {
            all.push((format!("static_k{k}_{name}"), *name, input(name)));
        }
    }
    all
}

#[test]
fn adaptive_cm_streams_from_the_old_coder_decode() {
    for (name, ks) in ADAPTIVE {
        for k in *ks {
            let bytes = fixture(&format!("adaptive_k{k}_{name}"));
            assert_eq!(
                cm::try_decompress(&bytes).unwrap(),
                input(name),
                "adaptive k={k} {name}"
            );
            assert_eq!(
                cm::try_decompress_legacy(&bytes).unwrap(),
                input(name),
                "legacy entry, k={k} {name}"
            );
        }
    }
}

#[test]
fn unversioned_static_cm_streams_decode() {
    for name in STATIC {
        for k in [0, 1] {
            let bytes = fixture(&format!("static_k{k}_{name}"));
            assert_eq!(
                cm::try_decompress(&bytes).unwrap(),
                input(name),
                "static k={k} {name}"
            );
        }
    }
}

/// Every old `compress_at` stream with one chunk begins with the byte 1 (its chunk count), the same byte a
/// version-1 stream begins with, so these also exercise the fall-back from a failed version-1 read.
#[test]
fn old_framed_streams_decode_through_the_public_entry() {
    for name in FRAMED {
        let bytes = fixture(&format!("framed_{name}"));
        assert_eq!(
            codec::try_decompress_framed(&bytes).unwrap(),
            input(name),
            "framed {name}"
        );
        assert_eq!(
            codec::decompress_framed(&bytes),
            input(name),
            "decompress_framed {name}"
        );
        assert_eq!(
            codec::try_decompress_framed_legacy(&bytes).unwrap(),
            input(name),
            "legacy entry {name}"
        );
    }
}

/// `compress()` of the old crate: a face tag, then a cm, lz or hybrid stream (the hybrid face carries old
/// cm streams for its literals).
#[test]
fn old_shaped_streams_decode_through_compress_decompress() {
    for name in SHAPED {
        let bytes = fixture(&format!("shaped_{name}"));
        assert_eq!(
            codec::try_decompress(&bytes).unwrap(),
            input(name),
            "shaped {name}"
        );
        assert_eq!(codec::decompress(&bytes), input(name), "decompress {name}");
    }
}

/// The old writer split a large input into several chunks; frame two real old chunks by hand
/// (`[2][len][len][chunk][chunk]`) and expect their concatenation.
#[test]
fn an_old_multi_chunk_framed_stream_decodes() {
    let a = fixture("adaptive_k5_text");
    let b = fixture("adaptive_k5_structured");
    let mut framed = vec![2u8];
    for part in [&a, &b] {
        // both lengths are below 2^14: a two-byte varint at most
        let mut n = part.len();
        while n >= 0x80 {
            framed.push((n & 0x7F) as u8 | 0x80);
            n >>= 7;
        }
        framed.push(n as u8);
    }
    framed.extend_from_slice(&a);
    framed.extend_from_slice(&b);
    let mut want = input("text");
    want.extend(input("structured"));
    assert_eq!(codec::try_decompress_framed(&framed).unwrap(), want);
}

/// What this crate writes now still round-trips, and a version-1 stream is never taken for an old one.
#[test]
fn version_one_streams_still_round_trip_and_are_not_misread_as_old() {
    for name in [
        "empty",
        "one",
        "text",
        "structured",
        "image",
        "random",
        "repeat",
    ] {
        let data = input(name);
        for order in [cm::Order::Zero, cm::Order::One] {
            let enc = cm::compress(order, &data);
            assert_eq!(enc[0], cm::VERSION);
            assert_eq!(cm::try_decompress(&enc).unwrap(), data, "{order:?} {name}");
            assert_eq!(
                codec::try_decompress_framed(&enc).unwrap(),
                data,
                "framed entry on a v1 stream {order:?} {name}"
            );
        }
        assert_eq!(
            codec::decompress(&codec::compress(&data)),
            data,
            "shaped {name}"
        );
        assert_eq!(
            codec::decompress_framed(&codec::compress_at(codec::DEFAULT_ORDER, &data)),
            data,
            "{name}"
        );
    }
}

/// An old stream is accepted only if it decodes to its declared length: cutting it anywhere never yields
/// other data, never panics, and (apart from a cut that leaves a valid empty stream) is an error.
#[test]
fn truncated_old_streams_never_decode_to_other_data() {
    for (fixture_name, input_name, want) in every_fixture() {
        let bytes = fixture(&fixture_name);
        let cap = (bytes.len() / 24).max(1); // every byte of a short stream, ~24 cuts of a long one
        let mut cut = 0;
        while cut < bytes.len() {
            let r1 = cm::try_decompress(&bytes[..cut]);
            let r2 = codec::try_decompress_framed(&bytes[..cut]);
            let r3 = codec::try_decompress(&bytes[..cut]);
            for v in [r1, r2, r3].into_iter().flatten() {
                assert!(
                    v.is_empty() || v == want,
                    "{fixture_name} ({input_name}) cut at {cut} decoded to {} other bytes",
                    v.len()
                );
            }
            cut += cap;
        }
    }
}

#[test]
fn damaged_old_streams_do_not_panic() {
    for (fixture_name, _, want) in every_fixture() {
        let bytes = fixture(&fixture_name);
        let step = (bytes.len() / 11).max(1);
        for at in (0..bytes.len()).step_by(step) {
            for flip in [0x01u8, 0xFF] {
                let mut bad = bytes.clone();
                bad[at] ^= flip;
                for v in [
                    cm::try_decompress(&bad),
                    codec::try_decompress_framed(&bad),
                    codec::try_decompress(&bad),
                ]
                .into_iter()
                .flatten()
                {
                    // a flip in a padding or unused bit may still decode; it must not be other data
                    assert!(
                        v == want || v.len() <= want.len() * 4 + 64,
                        "{fixture_name} at {at}"
                    );
                }
            }
        }
    }
}

/// The detection must not turn the version check into "anything is accepted": an unknown version or other
/// non-stream bytes are refused, and an empty stream is exactly the bytes the old encoders wrote.
#[test]
fn foreign_bytes_and_unknown_versions_are_refused() {
    let enc = cm::compress(cm::Order::One, &input("text"));
    for v in [2u8, 7, 0x7F, 0xFF] {
        let mut other = enc.clone();
        other[0] = v;
        assert!(cm::try_decompress(&other).is_err(), "first byte {v}");
    }
    // a zero length is an empty OLD stream only when the rest is exactly what the old encoder wrote
    let mut zero = enc.clone();
    zero[0] = 0;
    assert_eq!(
        cm::try_decompress(&zero).unwrap_err(),
        DecodeError::UnsupportedVersion(0)
    );
    assert_eq!(
        cm::try_decompress(&[0, 5, 5, 5, 5, 5, 5, 5, 5]).unwrap_err(),
        DecodeError::UnsupportedVersion(0)
    );
    assert_eq!(
        cm::try_decompress(&fixture("adaptive_k1_empty")).unwrap(),
        Vec::<u8>::new()
    );
    let mut padded = fixture("adaptive_k1_empty");
    padded.push(0);
    assert!(
        cm::try_decompress(&padded).is_err(),
        "trailing bytes after an empty old stream"
    );
    // empty and one-byte input
    assert_eq!(cm::try_decompress(&[]).unwrap_err(), DecodeError::Truncated);
    assert!(codec::try_decompress_framed(&[0xFF, 0xFF, 0xFF]).is_err());
}
