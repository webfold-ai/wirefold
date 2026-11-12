//! serde <-> `SyntaxTerm`: the bridge keeps serde_json's data model, so representative serde
//! shapes must round-trip through the term, through every text surface that can carry them, and
//! through MessagePack.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use wirefold::codec::syntax::{Atom, Surface, SyntaxTerm};
use wirefold::serde_bridge::{
    from_msgpack, from_surface, from_term, to_msgpack, to_surface, to_term,
};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
enum Shape {
    Unit,
    Newtype(u32),
    Pair(i8, String),
    Rect { w: f64, h: f64 },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Meters(f64);

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Record {
    id: u64,
    big: u128,
    neg: i64,
    ratio: f32,
    ok: bool,
    initial: char,
    name: String,
    nick: Option<String>,
    tags: Vec<String>,
    pair: (u8, String),
    shapes: Vec<Shape>,
    dist: Meters,
    by_name: BTreeMap<String, Vec<u16>>,
    by_id: BTreeMap<u32, bool>,
    nothing: (),
    raw: Vec<u8>,
}

fn sample() -> Record {
    Record {
        id: u64::MAX,
        big: u128::MAX,
        neg: i64::MIN,
        ratio: 0.1,
        ok: true,
        initial: 'λ',
        name: "wire \"fold\"\n\u{1F980}".into(),
        nick: None,
        tags: vec!["a".into(), "b".into()],
        pair: (255, "x".into()),
        shapes: vec![
            Shape::Unit,
            Shape::Newtype(9),
            Shape::Pair(-3, "p".into()),
            Shape::Rect { w: 2.0, h: 1e300 },
        ],
        dist: Meters(1.5),
        by_name: BTreeMap::from([("k".into(), vec![1, 2, 3])]),
        by_id: BTreeMap::from([(7, true), (11, false)]),
        nothing: (),
        raw: vec![0, 1, 254, 255],
    }
}

#[test]
fn term_round_trip() {
    let r = sample();
    let t = to_term(&r).unwrap();
    assert_eq!(from_term::<Record>(&t).unwrap(), r);
}

#[test]
fn json_round_trip_and_shape() {
    let r = sample();
    let json = to_surface(&r, Surface::Json).unwrap();
    assert!(json.contains("\"Rect\""), "{json}");
    assert_eq!(from_surface::<Record>(Surface::Json, &json).unwrap(), r);
}

#[test]
fn msgpack_round_trip() {
    let r = sample();
    let bytes = to_msgpack(&r).unwrap();
    assert_eq!(from_msgpack::<Record>(&bytes).unwrap(), r);
}

#[test]
fn yaml_round_trip() {
    let r = Shape::Rect { w: 1.5, h: 2.0 };
    let y = to_surface(&r, Surface::Yaml).unwrap();
    assert_eq!(from_surface::<Shape>(Surface::Yaml, &y).unwrap(), r);
}

#[test]
fn data_model_matches_the_table() {
    assert_eq!(to_term(&()).unwrap(), SyntaxTerm::Nb);
    assert_eq!(to_term(&None::<u8>).unwrap(), SyntaxTerm::Nb);
    assert_eq!(
        to_term(&Some(3u8)).unwrap(),
        SyntaxTerm::B(Atom::Num("3".into()))
    );
    assert_eq!(
        to_term(&1.0f64).unwrap(),
        SyntaxTerm::B(Atom::Num("1.0".into()))
    );
    assert_eq!(
        to_term(&Shape::Unit).unwrap(),
        SyntaxTerm::B(Atom::Str("Unit".into()))
    );
    assert_eq!(
        to_term(&Shape::Newtype(4)).unwrap(),
        SyntaxTerm::Bu(vec![(
            "Newtype".into(),
            SyntaxTerm::B(Atom::Num("4".into()))
        )])
    );
}

#[test]
fn floats_stay_floats() {
    let t = to_term(&2.0f64).unwrap();
    assert_eq!(from_term::<f64>(&t).unwrap().to_bits(), 2.0f64.to_bits());
    // An integer-looking number still deserialises as a float.
    let t = to_term(&2u8).unwrap();
    assert_eq!(from_term::<f64>(&t).unwrap(), 2.0);
}

#[test]
fn non_finite_floats_are_an_error() {
    assert!(to_term(&f64::NAN).is_err());
    assert!(to_term(&f32::INFINITY).is_err());
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let t = SyntaxTerm::B(Atom::Num("300".into()));
    assert!(from_term::<u8>(&t).is_err());
    let t = SyntaxTerm::B(Atom::Str("nope".into()));
    assert!(from_term::<Shape>(&t).is_err());
    let t = SyntaxTerm::Bu(vec![
        ("Unit".into(), SyntaxTerm::Nb),
        ("Newtype".into(), SyntaxTerm::Nb),
    ]);
    assert!(from_term::<Shape>(&t).is_err());
    assert!(from_term::<Record>(&SyntaxTerm::Nb).is_err());
}

#[test]
fn non_string_map_keys_are_rejected_on_the_way_out() {
    let m: HashMap<(u8, u8), u8> = HashMap::from([((1, 2), 3)]);
    assert!(to_term(&m).is_err());
}

#[test]
fn borrowed_strings_come_from_the_term() {
    #[derive(Deserialize)]
    struct Borrowed<'a> {
        s: &'a str,
    }
    let t = SyntaxTerm::Bu(vec![("s".into(), SyntaxTerm::B(Atom::Str("hi".into())))]);
    let b: Borrowed<'_> = from_term(&t).unwrap();
    assert_eq!(b.s, "hi");
}

#[test]
fn hash_maps_and_nested_options() {
    let m: HashMap<String, Option<Vec<i32>>> =
        HashMap::from([("a".into(), Some(vec![1, -2])), ("b".into(), None)]);
    let t = to_term(&m).unwrap();
    assert_eq!(
        from_term::<HashMap<String, Option<Vec<i32>>>>(&t).unwrap(),
        m
    );
}
