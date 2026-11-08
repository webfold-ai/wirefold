//! `#[derive(Term)]`: struct <-> `SyntaxTerm` (JSON/BAML/TOON/POML), the serde-derive analog for
//! the universal term — flat scalars, `Option<_>`/`Vec<_>` (one level), and a nested struct that
//! itself derives `Term`.

use wirefold::codec::syntax::{parse_surface, render_surface, AsTerm, Surface};
use wirefold::Term;

#[derive(Term, Clone, Debug, PartialEq)]
struct Address {
    city: String,
    zip: u32,
}

#[derive(Term, Clone, Debug, PartialEq)]
struct Person {
    name: String,
    age: i64,
    active: bool,
    nickname: Option<String>,
    tags: Vec<String>,
    address: Address,
}

fn sample() -> Person {
    Person {
        name: "Lucas".to_string(),
        age: 34,
        active: true,
        nickname: Some("Lu".to_string()),
        tags: vec!["architect".to_string(), "sanguine".to_string()],
        address: Address {
            city: "Hobart".to_string(),
            zip: 7000,
        },
    }
}

#[test]
fn to_term_from_term_round_trips() {
    let p = sample();
    let term = p.to_term();
    let back = Person::from_term(&term).unwrap();
    assert_eq!(p, back);
}

#[test]
fn to_term_from_term_round_trips_with_none_and_empty_list() {
    let p = Person {
        name: "X".to_string(),
        age: 0,
        active: false,
        nickname: None,
        tags: Vec::new(),
        address: Address {
            city: "Nowhere".to_string(),
            zip: 0,
        },
    };
    let term = p.to_term();
    let back = Person::from_term(&term).unwrap();
    assert_eq!(p, back);
}

/// The derived `to_term`/`from_term` must round-trip through REAL JSON text (not just the
/// in-memory `SyntaxTerm`) -- proves the derive genuinely composes with `codec::syntax`'s own
/// parser/renderer, giving a real `serde_json`-equivalent path: struct -> JSON string -> struct.
#[test]
fn round_trips_through_real_json_text() {
    let p = sample();
    let json = render_surface(&p.to_term(), Surface::Json);
    assert!(json.contains("\"name\":\"Lucas\""), "json: {json}");
    assert!(json.contains("\"age\":34"), "json: {json}");
    assert!(json.contains("\"nickname\":\"Lu\""), "json: {json}");
    assert!(json.contains("\"city\":\"Hobart\""), "json: {json}");

    let parsed_term = parse_surface(Surface::Json, &json).unwrap();
    let back = Person::from_term(&parsed_term).unwrap();
    assert_eq!(p, back);
}

/// A hand-written JSON string (not produced by `to_term` at all) must parse into the correct
/// typed struct -- proves `from_term` genuinely decodes arbitrary conforming JSON, not just its
/// own encoder's output.
#[test]
fn from_term_decodes_hand_written_json() {
    let json = r#"{"name":"Ada","age":28,"active":true,"nickname":null,"tags":["math","logic"],"address":{"city":"London","zip":1}}"#;
    let term = parse_surface(Surface::Json, json).unwrap();
    let p = Person::from_term(&term).unwrap();
    assert_eq!(p.name, "Ada");
    assert_eq!(p.age, 28);
    assert!(p.active);
    assert_eq!(p.nickname, None);
    assert_eq!(p.tags, vec!["math".to_string(), "logic".to_string()]);
    assert_eq!(
        p.address,
        Address {
            city: "London".to_string(),
            zip: 1
        }
    );
}

#[test]
fn missing_required_field_is_a_real_error_not_a_panic() {
    let json = r#"{"name":"NoAge"}"#;
    let term = parse_surface(Surface::Json, json).unwrap();
    let result = Person::from_term(&term);
    assert!(
        result.is_err(),
        "a missing required field must be a decode error"
    );
}

/// The whole point of the `AsTerm` trait, not just the inherent methods: fully generic code
/// (a `Json<T>`-style extractor, e.g.) that only knows `T: AsTerm` -- never named `Person`
/// directly -- must still be able to round-trip through real JSON text.
fn generic_json_round_trip<T: AsTerm + PartialEq + std::fmt::Debug>(value: &T) {
    let json = render_surface(&value.to_term(), Surface::Json);
    let term = parse_surface(Surface::Json, &json).unwrap();
    let back = T::from_term(&term).unwrap();
    assert_eq!(value, &back);
}

#[test]
fn as_term_trait_bound_works_generically() {
    generic_json_round_trip(&sample());
    generic_json_round_trip(&Address {
        city: "Generic".to_string(),
        zip: 42,
    });
}

// -- Nested Option/Vec (beyond one level) --------------------------------

#[derive(Term, Clone, Debug, PartialEq)]
struct NestedShapes {
    maybe_list: Option<Vec<String>>,
    list_of_maybe: Vec<Option<i64>>,
    matrix: Vec<Vec<u32>>,
    addresses: Vec<Option<Address>>,
}

fn nested_sample() -> NestedShapes {
    NestedShapes {
        maybe_list: Some(vec!["a".to_string(), "b".to_string()]),
        list_of_maybe: vec![Some(1), None, Some(3)],
        matrix: vec![vec![1, 2], vec![], vec![3]],
        addresses: vec![
            Some(Address {
                city: "Hobart".to_string(),
                zip: 7000,
            }),
            None,
        ],
    }
}

#[test]
fn nested_option_vec_round_trips_through_real_json_text() {
    let n = nested_sample();
    let json = render_surface(&n.to_term(), Surface::Json);
    let back = NestedShapes::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(n, back, "json: {json}");
}

#[test]
fn nested_option_vec_round_trips_with_none_variants() {
    let n = NestedShapes {
        maybe_list: None,
        list_of_maybe: vec![None, None],
        matrix: vec![],
        addresses: vec![],
    };
    let json = render_surface(&n.to_term(), Surface::Json);
    let back = NestedShapes::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(n, back, "json: {json}");
}

// -- Enums (externally tagged) --------------------------------------------

#[derive(Term, Clone, Debug, PartialEq)]
enum Shape {
    Circle { radius: f64 },
    Square(f64),
    Point,
    Rectangle(f64, f64),
    Tagged(Vec<String>),
}

#[test]
fn enum_unit_variant_round_trips() {
    let s = Shape::Point;
    let json = render_surface(&s.to_term(), Surface::Json);
    assert_eq!(json, r#"{"Point":null}"#, "json: {json}");
    let back = Shape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(s, back);
}

#[test]
fn enum_single_tuple_variant_round_trips() {
    let s = Shape::Square(4.0);
    let json = render_surface(&s.to_term(), Surface::Json);
    let back = Shape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(s, back, "json: {json}");
}

#[test]
fn enum_multi_tuple_variant_round_trips() {
    let s = Shape::Rectangle(3.0, 4.0);
    let json = render_surface(&s.to_term(), Surface::Json);
    let back = Shape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(s, back, "json: {json}");
}

#[test]
fn enum_named_field_variant_round_trips() {
    let s = Shape::Circle { radius: 2.5 };
    let json = render_surface(&s.to_term(), Surface::Json);
    let back = Shape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(s, back, "json: {json}");
}

#[test]
fn enum_variant_with_list_payload_round_trips() {
    let s = Shape::Tagged(vec!["a".to_string(), "b".to_string()]);
    let json = render_surface(&s.to_term(), Surface::Json);
    let back = Shape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(s, back, "json: {json}");
}

#[test]
fn enum_unknown_variant_tag_is_a_real_error() {
    let json = r#"{"NotAVariant":null}"#;
    let term = parse_surface(Surface::Json, json).unwrap();
    assert!(Shape::from_term(&term).is_err());
}

/// An enum as a struct FIELD (not just top-level) -- proves enum support composes with the
/// struct derive, the actual common case (a status/kind field on a larger record).
#[derive(Term, Clone, Debug, PartialEq)]
struct WithShape {
    name: String,
    shape: Shape,
    alt_shapes: Vec<Shape>,
}

#[test]
fn enum_as_a_nested_struct_field_round_trips() {
    let w = WithShape {
        name: "figure".to_string(),
        shape: Shape::Circle { radius: 1.0 },
        alt_shapes: vec![Shape::Point, Shape::Square(2.0)],
    };
    let json = render_surface(&w.to_term(), Surface::Json);
    let back = WithShape::from_term(&parse_surface(Surface::Json, &json).unwrap()).unwrap();
    assert_eq!(w, back, "json: {json}");
}
