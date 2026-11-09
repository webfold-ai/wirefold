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
