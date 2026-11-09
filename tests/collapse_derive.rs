//! `#[derive(Collapse)]`: the zero-copy record bridge — and specifically that the derive's own
//! infrastructure does not collide with the record's field names.
//!
//! The View carries one accessor per field PLUS its own `open`/`len`/`is_empty`/`get`/`to_vec`,
//! and both land in the same inherent impl. A record with a field called `open` therefore failed
//! to compile with "duplicate definitions for `open`" — which is every OHLC bar in existence, and
//! the same trap was set for `len`, `get`, `to_vec` and `is_empty`.
//!
//! The infrastructure is now `collapse_*` (always present, always what generated code calls) and
//! the short names are aliases emitted only where the record has not claimed them. So a field wins
//! its own name and existing `View::open` callers keep working.

use wirefold::codec::web::CollapseElem;
use wirefold::Collapse;

/// The record that could not be derived: an OHLC bar. Every one of the five reserved short names
/// is claimed by a field, so NONE of the aliases may be emitted for this type.
#[derive(Collapse, Clone, Copy, Debug, PartialEq)]
struct Bar {
    ts: u64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    len: u32,
    get: u32,
    is_empty: bool,
}

/// A record claiming none of them — the alias path must still work here.
#[derive(Collapse, Clone, Debug, PartialEq)]
struct Plain {
    id: u64,
    label: String,
}

fn bars() -> Vec<Bar> {
    (0..64u64)
        .map(|i| Bar {
            ts: 1_700_000_000 + i,
            open: 10.0 + i as f64,
            high: 12.0 + i as f64,
            low: 9.0 + i as f64,
            close: 11.0 + i as f64,
            volume: 1000.0 + i as f64,
            len: i as u32,
            get: (i * 2) as u32,
            is_empty: i % 2 == 0,
        })
        .collect()
}

/// The regression: this type simply has to compile, and its field accessors have to be the
/// fields — not the derive's plumbing wearing their names.
#[test]
fn a_field_named_open_is_the_field_not_the_constructor() {
    let src = bars();
    let seed = Bar::collapse_pack(&src);
    let view = BarView::collapse_open(seed.as_bytes()).expect("packed table opens");

    assert_eq!(view.collapse_len(), 64);
    assert!(!view.collapse_is_empty());

    // Every claimed name resolves to the FIELD's accessor, taking a row index.
    assert_eq!(view.open(3), 13.0);
    assert_eq!(view.len(3), 3);
    assert_eq!(view.get(3), 6);
    assert!(!view.is_empty(3));
    assert_eq!(view.close(3), 14.0);
}

/// Nothing about the collision fix may change what the column stores.
#[test]
fn the_claimed_names_do_not_disturb_the_round_trip() {
    let src = bars();
    let seed = Bar::collapse_pack(&src);
    let view = BarView::collapse_open(seed.as_bytes()).expect("packed table opens");
    assert_eq!(view.collapse_to_vec(), src);
    assert_eq!(view.collapse_get(17), src[17]);
}

/// A record that claims none of the reserved names still gets the short aliases, so existing
/// callers written against `View::open`/`len`/`get`/`to_vec` keep compiling.
#[test]
fn a_record_claiming_no_reserved_name_still_gets_the_short_aliases() {
    let src = vec![
        Plain {
            id: 1,
            label: "a".into(),
        },
        Plain {
            id: 2,
            label: "bb".into(),
        },
    ];
    let seed = Plain::collapse_pack(&src);

    let view = PlainView::open(seed.as_bytes()).expect("the alias constructor still exists");
    assert_eq!(view.len(), 2);
    assert!(!view.is_empty());
    assert_eq!(view.get(1), src[1]);
    assert_eq!(view.to_vec(), src);

    // And the alias is the canonical method, not a second implementation of it.
    let canonical = PlainView::collapse_open(seed.as_bytes()).expect("canonical opens");
    assert_eq!(canonical.collapse_to_vec(), view.to_vec());
}

/// The point of the column: a flat scalar record packs to a fixed stride, so the table is far
/// smaller than one owned record per row.
#[test]
fn the_packed_column_is_smaller_than_the_owned_records() {
    let src = bars();
    let packed = Bar::collapse_pack(&src).as_bytes().len();
    let owned = src.len() * core::mem::size_of::<Bar>();
    assert!(
        packed <= owned,
        "packed column {packed} B must not exceed {owned} B of owned records"
    );
}

/// Every scalar field is its own contiguous column, so the table is one section per scalar field and one per
/// variable-length field, and a column is a zero-copy slice of exactly that field's values.
#[derive(Collapse, Clone, Debug, PartialEq)]
struct Reading {
    ts: u64,
    channel: u8,
    celsius: f64,
    ok: bool,
    label: String,
}

fn readings() -> Vec<Reading> {
    (0..37u64)
        .map(|i| Reading {
            ts: 1_000 + i * 3,
            channel: (i % 7) as u8,
            celsius: 20.0 + i as f64 / 4.0,
            ok: i % 3 != 0,
            label: format!("probe-{}", i % 5),
        })
        .collect()
}

#[test]
fn each_scalar_field_is_a_contiguous_column() {
    let src = readings();
    let seed = Reading::collapse_pack(&src);
    let b = seed.as_bytes();
    let u64_at = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as usize;
    // four scalar fields and one string field: five sections
    assert_eq!(u64_at(0), 5);
    let sec = |j: usize| (u64_at(8 + 8 * j), u64_at(48 + 8 * j));
    // each scalar section is [count][n values], 8-aligned, and nothing but that field
    assert_eq!(sec(0).1, 8 + src.len() * 8); // ts
    assert_eq!(sec(1).1, 8 + src.len()); // channel
    assert_eq!(sec(2).1, 8 + src.len() * 8); // celsius
    assert_eq!(sec(3).1, 8 + src.len()); // ok (a byte)
    for j in 0..4 {
        assert_eq!(sec(j).0 % 8, 0, "section {j} starts 8-aligned");
        assert_eq!(u64_at(sec(j).0), src.len(), "section {j} carries the count");
    }
    // the first column's bytes are the ts values back to back
    let ts_bytes = &b[sec(0).0 + 8..sec(0).0 + sec(0).1];
    for (i, r) in src.iter().enumerate() {
        assert_eq!(&ts_bytes[i * 8..i * 8 + 8], &r.ts.to_le_bytes());
    }
    // and the typed views agree with the records and with the column slices
    let v = ReadingView::collapse_open(b).expect("opens");
    assert_eq!(v.ts_column(), src.iter().map(|r| r.ts).collect::<Vec<_>>());
    assert_eq!(
        v.celsius_column(),
        src.iter().map(|r| r.celsius).collect::<Vec<_>>()
    );
    assert_eq!(
        v.ok_column(),
        src.iter().map(|r| u8::from(r.ok)).collect::<Vec<_>>()
    );
    assert_eq!(v.collapse_to_vec(), src);
}

/// A record with no scalar field still has a readable record count, and opens and round-trips.
#[derive(Collapse, Clone, Debug, PartialEq)]
struct OnlyText {
    a: String,
    b: String,
}

#[test]
fn a_record_with_no_scalar_fields_round_trips() {
    let src: Vec<OnlyText> = (0..9)
        .map(|i| OnlyText {
            a: format!("a{i}"),
            b: "x".repeat(i),
        })
        .collect();
    let seed = OnlyText::collapse_pack(&src);
    let v = OnlyTextView::collapse_open(seed.as_bytes()).expect("opens");
    assert_eq!(v.collapse_len(), src.len());
    assert_eq!(v.collapse_to_vec(), src);
    let empty = OnlyText::collapse_pack(&[]);
    assert_eq!(
        OnlyTextView::collapse_open(empty.as_bytes())
            .unwrap()
            .collapse_len(),
        0
    );
}

/// Columns that disagree about the record count, or a section table with the wrong number of sections,
/// are refused at open rather than read.
#[test]
fn columns_that_disagree_are_refused() {
    use wirefold::codec::web::WireErr;
    let src = readings();
    let seed = Reading::collapse_pack(&src);
    let good = seed.as_bytes().to_vec();
    // a copy in an 8-aligned buffer, since `open` requires alignment
    let aligned = |bytes: &[u8]| -> Vec<u64> {
        let mut w = vec![0u64; bytes.len().div_ceil(8)];
        for (i, b) in bytes.iter().enumerate() {
            w[i / 8] |= u64::from(*b) << (8 * (i % 8));
        }
        w
    };
    let open = |bytes: &[u8]| {
        let w = aligned(bytes);
        let s: &[u8] = unsafe { std::slice::from_raw_parts(w.as_ptr().cast::<u8>(), bytes.len()) };
        ReadingView::collapse_open(s).map(|_| ())
    };
    assert_eq!(open(&good), Ok(()));
    // the second scalar column (one byte per record) is shortened by one record and made internally
    // consistent (its count word and its section length both say n - 1), so only the cross-column count
    // check can notice that it no longer matches the first column
    let u64_at =
        |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as usize;
    let second = u64_at(&good, 8 + 8);
    let mut bad = good.clone();
    bad[second..second + 8].copy_from_slice(&((src.len() as u64) - 1).to_le_bytes());
    let len_at = 8 + 8 * 5 + 8; // the section table's length entry for column 1
    bad[len_at..len_at + 8].copy_from_slice(&((8 + src.len() - 1) as u64).to_le_bytes());
    assert_eq!(open(&bad), Err(WireErr::BadLength));
    // a count word that disagrees with its own section's length is refused by the column itself
    let mut bad = good.clone();
    bad[second..second + 8].copy_from_slice(&((src.len() as u64) + 1).to_le_bytes());
    assert!(open(&bad).is_err());
    // a table with the wrong section count
    let mut bad = good.clone();
    bad[0..8].copy_from_slice(&4u64.to_le_bytes());
    assert_eq!(open(&bad), Err(WireErr::BadLength));
}
