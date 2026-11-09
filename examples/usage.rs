//! The examples from `docs/USAGE.md`, compiled and run so they stay correct.
//!
//! Run: `cargo run --example usage --all-features`

use wirefold::codec::syntax::{
    parse_surface, parse_surface_ref, render_surface, translate, Surface,
};
use wirefold::codec::web::CollapseElem;
use wirefold::{codec, Collapse, Term};

/// A flat record: packed column by column, readable in place.
#[derive(Collapse, Clone, Debug, PartialEq)]
struct Reading {
    ts: u64,
    sensor: u32,
    celsius: f64,
    label: String,
}

/// A structured record that converts to and from JSON, YAML, TOON and the other text surfaces.
#[derive(Term, Clone, Debug, PartialEq)]
struct Person {
    name: String,
    age: i64,
    tags: Vec<String>,
}

fn main() {
    // 1. Columnar zero-copy: pack a slice, open the bytes, read without decoding the rest.
    let readings: Vec<Reading> = (0..1000u64)
        .map(|i| Reading {
            ts: 1_700_000_000 + i,
            sensor: (i % 8) as u32,
            celsius: 20.0 + (i % 50) as f64 / 10.0,
            label: format!("probe-{}", i % 8),
        })
        .collect();
    let packed = Reading::collapse_pack(&readings);
    let bytes = packed.as_bytes(); // store or send these
    let view = ReadingView::collapse_open(bytes).expect("validates the layout, does not walk rows");
    assert_eq!(view.collapse_len(), 1000);
    assert_eq!(view.celsius(7), readings[7].celsius); // one field of one row, in place
    assert_eq!(view.collapse_to_vec(), readings); // or materialise everything
    println!(
        "columnar: {} records in {} bytes",
        readings.len(),
        bytes.len()
    );

    // 2. Hostile input is an error, not a panic: a corrupt buffer fails to open.
    assert!(ReadingView::collapse_open(&bytes[..bytes.len() / 2]).is_err());

    // 3. Structs to and from text surfaces.
    let p = Person {
        name: "Ada".into(),
        age: 36,
        tags: vec!["math".into(), "engines".into()],
    };
    let json = render_surface(&p.to_term(), Surface::Json);
    let back = Person::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(back, p);
    println!("json: {json}");

    // 4. Convert between surfaces without a struct.
    let yaml = translate(&json, Surface::Json, Surface::Yaml).unwrap();
    println!("yaml:\n{yaml}");

    // 5. Parse without copying: a term whose keys and scalars borrow from the input text. Strings
    // with no escapes, numbers and plain YAML scalars are slices of `json`, so no allocation.
    let borrowed = parse_surface_ref(Surface::Json, &json).unwrap();
    assert_eq!(
        borrowed.clone().into_owned(),
        parse_surface(Surface::Json, &json).unwrap()
    );
    let owned = borrowed.into_owned(); // copy the strings when the term must outlive `json`
    assert_eq!(Person::from_term(&owned).unwrap(), p);

    // 6. Entropy-coded compression of any byte buffer.
    let packed_z = codec::compress(bytes);
    assert_eq!(codec::decompress(&packed_z), bytes);
    println!("compress: {} -> {} bytes", bytes.len(), packed_z.len());
    // Untrusted input: the framed decoder returns an error instead of panicking.
    assert!(codec::try_decompress_framed(&[0xFF; 16]).is_err());

    // 7. MessagePack through the same universal term.
    // Object keys come back in sorted order, so compare through the struct, not the raw term.
    let mp = codec::msgpack::encode(&p.to_term());
    assert_eq!(
        Person::from_term(&codec::msgpack::decode(&mp).unwrap()).unwrap(),
        p
    );
    println!("msgpack: {} bytes", mp.len());
}
