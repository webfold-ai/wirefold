//! `#[derive(Collapse)]`: the columnar zero-copy bridge — and specifically that the derive's own
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
