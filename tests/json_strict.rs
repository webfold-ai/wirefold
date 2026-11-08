//! `Surface::Json` is the strict RFC 8259 parser (audit finding MQ-04); BAML/TOON values use the
//! loose dialect, which must keep accepting what it always did.
//!
//! The strict verdicts come from Python's `json` module (see `tests/fixtures/gen_json_strict_cases.py`
//! for how, and for the three deliberate deviations: duplicate keys, lone surrogates, depth > 128).
#![cfg(feature = "syntax")]

use wirefold::codec::syntax::{parse_surface, Atom, Surface, SyntaxTerm};

fn unhex(h: &str) -> String {
    let bytes: Vec<u8> = (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn strict_json_agrees_with_python_json_on_the_reference_corpus() {
    let corpus = include_str!("fixtures/json_strict_cases.txt");
    let (mut checked, mut wrong) = (0usize, Vec::new());
    for line in corpus.lines() {
        let (verdict, hex) = line.split_once(' ').unwrap();
        let input = unhex(hex);
        let accepted = parse_surface(Surface::Json, &input).is_ok();
        checked += 1;
        if accepted != (verdict == "A") {
            wrong.push(format!(
                "python says {verdict}, we say {}: {input:?}",
                if accepted { "A" } else { "R" }
            ));
        }
    }
    assert!(checked > 5000, "corpus went missing: {checked} cases");
    assert!(
        wrong.is_empty(),
        "{} of {checked} verdicts differ from Python, first 25:\n{}",
        wrong.len(),
        wrong
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn strict_json_named_cases_from_the_audit() {
    for bad in [
        "\"a\nb\"",
        "\"a\tb\"",
        "\"\\q\"",
        "01",
        "-01",
        "[00]",
        "[1,]",
        "{\"a\":1,}",
        "{\"a\":1,\"a\":2}",
    ] {
        assert!(parse_surface(Surface::Json, bad).is_err(), "{bad:?}");
    }
    for good in [
        "0",
        "-0",
        "0.5",
        "[0]",
        "\"a\\nb\"",
        "{\"a\":1,\"b\":2}",
        "[1,2]",
    ] {
        assert!(parse_surface(Surface::Json, good).is_ok(), "{good:?}");
    }
}

#[test]
fn strict_json_still_decodes_values_correctly() {
    let t = parse_surface(Surface::Json, "{\"a\":[1,\"x\\u00e9\\n\",true,null]}").unwrap();
    assert_eq!(
        t,
        SyntaxTerm::Bu(vec![(
            "a".into(),
            SyntaxTerm::Fourth(vec![
                SyntaxTerm::B(Atom::Num("1".into())),
                SyntaxTerm::B(Atom::Str("x\u{e9}\n".into())),
                SyntaxTerm::B(Atom::Bool(true)),
                SyntaxTerm::Nb,
            ])
        )])
    );
}

/// The loose dialect (BAML values, TOON values) is unchanged: bare words, `=` and `;` separators,
/// trailing commas, duplicate keys, leading zeros, raw control characters, unknown escapes.
#[test]
fn loose_dialect_keeps_its_behaviour() {
    let num = |s: &str| SyntaxTerm::B(Atom::Num(s.into()));
    let word = |s: &str| SyntaxTerm::B(Atom::Str(s.into()));
    let t = parse_surface(Surface::Baml, "baml {a = 1; b = [x, y,] c = \"q\\z\"}").unwrap();
    assert_eq!(
        t,
        SyntaxTerm::Bu(vec![
            ("a".into(), num("1")),
            ("b".into(), SyntaxTerm::Fourth(vec![word("x"), word("y")])),
            ("c".into(), word("qz")),
        ])
    );
    let t = parse_surface(Surface::Baml, "baml {a = 01, a = 2,}").unwrap();
    assert_eq!(
        t,
        SyntaxTerm::Bu(vec![("a".into(), num("01")), ("a".into(), num("2"))])
    );
    let t = parse_surface(Surface::Toon, "x = \"a\tb\"").unwrap();
    assert_eq!(t, SyntaxTerm::Bu(vec![("x".into(), word("a\tb"))]));
}
