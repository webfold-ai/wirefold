//! Pins the exact results of the surface parsers on edge cases: path syntax (TOON and CSV), the
//! duplicate-key check in JSON objects (narrow and wide), and JSON string decoding.
//!
//! The expected values were recorded from the parsers as they behaved before they were optimised
//! (single-pass path scan, stack-built containers, bulk string copy), and the optimised parsers
//! were also compared against the old ones on about two million generated and mutated inputs, so
//! these cases guard that no behaviour, error message included, changed.
#![cfg(feature = "syntax")]

use wirefold::codec::syntax::{parse_surface, render_surface, Surface};

/// `ok <json>` for a parsed document, `err <message>` otherwise.
fn show(surface: Surface, input: &str) -> String {
    match parse_surface(surface, input) {
        Ok(t) => format!("ok {}", render_surface(&t, Surface::Json)),
        Err(e) => format!("err {}", e.msg),
    }
}

#[test]
fn toon_paths() {
    let cases: &[(&str, &str)] = &[
        ("a.b = 1", r#"ok {"a":{"b":1}}"#),
        ("a[1] = 1", r#"ok {"a":[null,1]}"#),
        ("a..b = 1", r#"ok {"a":{"b":1}}"#),
        ("[0] = 1", "err path crosses non-list"),
        ("$ = 1", "ok 1"),
        ("$.a = 1", r#"ok {"$":{"a":1}}"#),
        ("a[0][1] = 1", r#"ok {"a":[[null,1]]}"#),
        ("a[0].b[1] = 2", r#"ok {"a":[{"b":[null,2]}]}"#),
        ("a[x] = 1", "err bad index"),
        ("a[ = 1", "err bad index path"),
        ("a[1]x = 1", "err bad path tail"),
        ("[1]a = 1", "err bad path tail"),
        ("a[+1] = 1", r#"ok {"a":[null,1]}"#),
        ("a[-1] = 1", "err bad index"),
        ("a[] = 1", "err bad index"),
        (".a = 1", r#"ok {"a":1}"#),
        ("a. = 1", r#"ok {"a":1}"#),
        ("a = 1\na.b = 2", "err path crosses non-object"),
        ("a[0] = 1\na.b = 2", "err path crosses non-object"),
        ("x y.z = \"v\"", r#"ok {"x y":{"z":"v"}}"#),
        ("é.ü[2] = true", r#"ok {"é":{"ü":[null,null,true]}}"#),
        (
            "a[1][2][3] = null",
            r#"ok {"a":[null,[null,null,[null,null,null,null]]]}"#,
        ),
    ];
    for (input, want) in cases {
        assert_eq!(&show(Surface::Toon, input), want, "TOON input {input:?}");
    }
}

#[test]
fn csv_rows_and_paths() {
    let cases: &[(&str, &str)] = &[
        ("path,kind,value\n$,bu,\n", "ok {}"),
        (
            "path,kind,value\na,num,1\nb,str,x\n",
            r#"ok {"a":1,"b":"x"}"#,
        ),
        ("path,kind,value\na[2],num,1\n", r#"ok {"a":[null,null,1]}"#),
        (
            "path,kind,value\n\"a,b\",str,\"x,y\"\n",
            r#"ok {"a,b":"x,y"}"#,
        ),
        (
            "path,kind,value\na,num\n",
            "err csv row must have exactly 3 columns",
        ),
        (
            "path,kind,value\na,str,\"un\n",
            "err unterminated csv quote",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(&show(Surface::Csv, input), want, "CSV input {input:?}");
    }
}

#[test]
fn json_duplicate_keys_narrow_and_wide() {
    // Narrow objects take the linear-scan path, wide ones switch to a hash set after 16 keys.
    // Both must report the same error and accept the same documents.
    assert_eq!(
        show(Surface::Json, r#"{"a":1,"a":2}"#),
        r#"err duplicate object key "a""#
    );
    let mut dup_at_17th = String::from("{");
    for i in 0..16 {
        dup_at_17th.push_str(&format!("\"k{i}\":0,"));
    }
    dup_at_17th.push_str("\"k0\":1}");
    assert_eq!(
        show(Surface::Json, &dup_at_17th),
        r#"err duplicate object key "k0""#
    );

    let mut wide_dup = String::from("{");
    for i in 0..40 {
        wide_dup.push_str(&format!("\"k{i}\":{i},"));
    }
    wide_dup.push_str("\"k3\":0}");
    assert_eq!(
        show(Surface::Json, &wide_dup),
        r#"err duplicate object key "k3""#
    );

    let wide_unique = format!(
        "{{{}}}",
        (0..40)
            .map(|i| format!("\"k{i}\":{i}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(
        show(Surface::Json, &wide_unique),
        format!("ok {wide_unique}")
    );

    // The same key in different objects is not a duplicate.
    assert_eq!(
        show(Surface::Json, r#"{"a":{"a":1},"b":{"a":2}}"#),
        r#"ok {"a":{"a":1},"b":{"a":2}}"#
    );
}

#[test]
fn json_string_decoding() {
    let cases: &[(&str, &str)] = &[
        (r#""plain""#, r#"ok "plain""#),
        (r#""esc\n\t\"q\"""#, r#"ok "esc\n\t\"q\"""#),
        (r#""é😀""#, "ok \"é😀\""),
        ("\"unterminated", "err unterminated string"),
        ("\"ctl\u{1}x\"", "err control character in string"),
        (r#""bad\q""#, "err invalid escape"),
        (r#""\ud800""#, "err unpaired surrogate"),
        ("\"日本語 text\"", "ok \"日本語 text\""),
        (r#""""#, r#"ok """#),
        (r#""a\\""#, r#"ok "a\\""#),
    ];
    for (input, want) in cases {
        assert_eq!(&show(Surface::Json, input), want, "JSON input {input:?}");
    }
}

/// A block scalar's first line sets its margin; a later line indented less than that can have a
/// multi-byte character where the margin falls. That used to panic (slicing inside the character),
/// which let untrusted YAML crash the process. It must return a result instead.
#[test]
fn yaml_block_scalar_with_a_multibyte_char_at_the_margin_does_not_panic() {
    for input in [
        "k: |\n    ab\n  日x\n",
        "k: |\n      abcdef\n  日本語\n  x\n",
        "- |\n    one\n  🙂 two\n",
        "a:\n  b: >\n      folded\n   é rest\n",
    ] {
        let r = std::panic::catch_unwind(|| parse_surface(Surface::Yaml, input).is_ok());
        assert!(r.is_ok(), "YAML parser panicked on {input:?}");
    }
}

mod borrowed {
    use std::borrow::Cow;
    use wirefold::codec::syntax::{
        parse_surface, parse_surface_ref, AtomRef, Surface, SyntaxTermRef,
    };

    /// Whether `text` is a slice of `input` (a borrow) rather than a copy. It takes the `Cow`
    /// itself because the question is which variant it is.
    #[allow(clippy::ptr_arg)]
    fn points_into(text: &Cow<'_, str>, input: &str) -> bool {
        matches!(text, Cow::Borrowed(s)
            if s.as_ptr() as usize >= input.as_ptr() as usize
                && s.as_ptr() as usize + s.len() <= input.as_ptr() as usize + input.len())
    }

    fn field<'t, 'a>(
        fields: &'t [(Cow<'a, str>, SyntaxTermRef<'a>)],
        key: &str,
    ) -> &'t SyntaxTermRef<'a> {
        &fields
            .iter()
            .find(|(k, _)| k == key)
            .expect("key present")
            .1
    }

    #[test]
    fn json_strings_keys_and_numbers_borrow_from_the_input() {
        let input =
            r#"{"name":"Ada","age":36,"pi":3.14,"esc":"a\nb","uni":"caf\u00e9","list":["x","y"]}"#;
        let SyntaxTermRef::Bu(fields) = parse_surface_ref(Surface::Json, input).unwrap() else {
            panic!("an object");
        };
        // every key is a slice of the input
        assert!(fields.iter().all(|(k, _)| points_into(k, input)));
        // plain strings and numbers are slices of the input
        let SyntaxTermRef::B(AtomRef::Str(name)) = field(&fields, "name") else {
            panic!("a string");
        };
        assert_eq!(name.as_ref(), "Ada");
        assert!(points_into(name, input), "a string with no escape borrows");
        let SyntaxTermRef::B(AtomRef::Num(age)) = field(&fields, "age") else {
            panic!("a number");
        };
        assert_eq!(age.as_ref(), "36");
        assert!(points_into(age, input), "a number borrows");
        // a string that needed unescaping is a copy, with the unescaped text
        for (key, want) in [("esc", "a\nb"), ("uni", "café")] {
            let SyntaxTermRef::B(AtomRef::Str(t)) = field(&fields, key) else {
                panic!("a string");
            };
            assert_eq!(t, want);
            assert!(
                matches!(t, Cow::Owned(_)),
                "{key} had an escape, so it is built"
            );
        }
        // strings inside a list borrow too
        let SyntaxTermRef::Fourth(items) = field(&fields, "list") else {
            panic!("a list");
        };
        for item in items {
            let SyntaxTermRef::B(AtomRef::Str(t)) = item else {
                panic!("a string");
            };
            assert!(points_into(t, input));
        }
    }

    #[test]
    fn yaml_plain_scalars_and_keys_borrow_from_the_input() {
        let input = "name: Ada\nage: 36\nquoted: \"plain text\"\nescaped: \"a\\nb\"\nsingle: 'it''s'\nlist:\n  - one\n  - two\n";
        let SyntaxTermRef::Bu(fields) = parse_surface_ref(Surface::Yaml, input).unwrap() else {
            panic!("a mapping");
        };
        assert!(fields.iter().all(|(k, _)| points_into(k, input)));
        let text_of = |key: &str| match field(&fields, key) {
            SyntaxTermRef::B(AtomRef::Str(t) | AtomRef::Num(t)) => t.clone(),
            other => panic!("a scalar, got {other:?}"),
        };
        assert!(points_into(&text_of("name"), input));
        assert!(points_into(&text_of("age"), input));
        assert!(
            points_into(&text_of("quoted"), input),
            "quotes around plain text still borrow"
        );
        assert!(
            matches!(text_of("escaped"), Cow::Owned(_)),
            "an escape means a copy"
        );
        assert!(
            matches!(text_of("single"), Cow::Owned(_)),
            "'' means a copy"
        );
        assert_eq!(text_of("single"), "it's");
        let SyntaxTermRef::Fourth(items) = field(&fields, "list") else {
            panic!("a sequence");
        };
        for item in items {
            let SyntaxTermRef::B(AtomRef::Str(t)) = item else {
                panic!("a string");
            };
            assert!(points_into(t, input));
        }
    }

    #[test]
    fn into_owned_equals_the_owned_parse_on_every_surface() {
        let docs: &[(Surface, &str)] = &[
            (
                Surface::Json,
                r#"{"a":[1,2.5,"x\ty",null,true],"b":{"c":"d"}}"#,
            ),
            (
                Surface::Yaml,
                "a:\n  - 1\n  - two\nb: {c: d, e: [f, g]}\nk: |\n  block\n  text\n",
            ),
            (Surface::Toon, "a.b = 1\na.c[1] = \"x\"\n"),
            (Surface::Csv, "path,kind,value\na,num,1\nb,str,x\n"),
            (
                Surface::Poml,
                "<poml><bu><field name=\"a\"><b kind=\"num\">1</b></field></bu></poml>",
            ),
            (Surface::Baml, "{ a = 1, b = \"x\" }"),
        ];
        for (surface, input) in docs {
            let owned = parse_surface(*surface, input);
            let borrowed = parse_surface_ref(*surface, input).map(SyntaxTermRef::into_owned);
            assert_eq!(owned, borrowed, "{surface:?} on {input:?}");
        }
    }
}
