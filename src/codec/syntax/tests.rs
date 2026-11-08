//! The `codec::syntax` test suite, moved out of the root file whole. `super` is this module, so
//! the block's own `use super::*` reaches the root through the glob below.

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// `Path<String>`'s shape: a single-field `Bu` (one route param) reads back as a bare
    /// `String`, position-only (the field's declared NAME is discarded).
    #[test]
    fn string_as_term_reads_the_sole_field_of_a_single_entry_object() {
        let term = SyntaxTerm::Bu(vec![(
            "ip".to_string(),
            SyntaxTerm::B(Atom::Str("203.0.113.5".to_string())),
        )]);
        assert_eq!(String::from_term(&term).unwrap(), "203.0.113.5");
    }

    #[test]
    fn string_as_term_rejects_a_multi_field_object() {
        let term = SyntaxTerm::Bu(vec![
            ("a".to_string(), SyntaxTerm::B(Atom::Str("1".to_string()))),
            ("b".to_string(), SyntaxTerm::B(Atom::Str("2".to_string()))),
        ]);
        assert!(String::from_term(&term).is_err());
    }

    #[test]
    fn string_as_term_reads_a_bare_scalar_directly() {
        let term = SyntaxTerm::B(Atom::Str("hello".to_string()));
        assert_eq!(String::from_term(&term).unwrap(), "hello");
    }

    /// `Path<i64>`'s shape: a single-field `Bu` (one numeric route param, e.g. `/email/cancel/
    /// {id}` → `id: web::Path<i64>`) reads back as the parsed number, matching `pairs_to_term`'s
    /// own type-inference (a segment that parses as a number is stored as `Atom::Num`).
    #[test]
    fn i64_as_term_reads_the_sole_field_of_a_single_entry_object() {
        let term = SyntaxTerm::Bu(vec![(
            "id".to_string(),
            SyntaxTerm::B(Atom::Num("42".to_string())),
        )]);
        assert_eq!(i64::from_term(&term).unwrap(), 42);
    }

    #[test]
    fn bool_as_term_reads_the_sole_field_of_a_single_entry_object() {
        let term = SyntaxTerm::Bu(vec![("flag".to_string(), SyntaxTerm::B(Atom::Bool(true)))]);
        assert!(bool::from_term(&term).unwrap());
    }

    /// A struct field holding an `IpAddr` (e.g. rauthy's `IpBlacklistRequest`) round-trips
    /// through its own `to_term`/`from_term`, both IPv4 and IPv6.
    #[test]
    fn ip_addr_as_term_round_trips() {
        let v4: std::net::IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(std::net::IpAddr::from_term(&v4.to_term()).unwrap(), v4);

        let v6: std::net::IpAddr = "::1".parse().unwrap();
        assert_eq!(std::net::IpAddr::from_term(&v6.to_term()).unwrap(), v6);
    }

    /// `Path<(String, String)>`'s shape: a two-field `Bu` (two route params, e.g. `/users/
    /// {user_id}/picture/{picture_id}`) reads back positionally — field ORDER matters, names
    /// don't.
    #[test]
    fn tuple_as_term_reads_two_fields_by_position() {
        let term = SyntaxTerm::Bu(vec![
            (
                "user_id".to_string(),
                SyntaxTerm::B(Atom::Str("u-1".to_string())),
            ),
            (
                "picture_id".to_string(),
                SyntaxTerm::B(Atom::Str("p-2".to_string())),
            ),
        ]);
        let (user_id, picture_id) = <(String, String)>::from_term(&term).unwrap();
        assert_eq!(user_id, "u-1");
        assert_eq!(picture_id, "p-2");
    }

    /// `Path<(String, i64)>`'s shape — mixed types per position (e.g. `/theme/{client_id}/
    /// {timestamp}`), not just homogeneous strings.
    #[test]
    fn tuple_as_term_reads_mixed_types_by_position() {
        let term = SyntaxTerm::Bu(vec![
            (
                "client_id".to_string(),
                SyntaxTerm::B(Atom::Str("rauthy".to_string())),
            ),
            (
                "timestamp".to_string(),
                SyntaxTerm::B(Atom::Num("1700000000".to_string())),
            ),
        ]);
        let (client_id, timestamp) = <(String, i64)>::from_term(&term).unwrap();
        assert_eq!(client_id, "rauthy");
        assert_eq!(timestamp, 1_700_000_000);
    }

    #[test]
    fn tuple_as_term_rejects_the_wrong_field_count() {
        let term = SyntaxTerm::Bu(vec![(
            "only".to_string(),
            SyntaxTerm::B(Atom::Str("x".to_string())),
        )]);
        assert!(<(String, String)>::from_term(&term).is_err());
    }

    #[test]
    fn three_tuple_as_term_reads_three_fields_by_position() {
        let term = SyntaxTerm::Bu(vec![
            ("a".to_string(), SyntaxTerm::B(Atom::Str("1".to_string()))),
            ("b".to_string(), SyntaxTerm::B(Atom::Str("2".to_string()))),
            ("c".to_string(), SyntaxTerm::B(Atom::Str("3".to_string()))),
        ]);
        let (a, b, c) = <(String, String, String)>::from_term(&term).unwrap();
        assert_eq!((a.as_str(), b.as_str(), c.as_str()), ("1", "2", "3"));
    }

    /// A bare `Vec<T>` at the top level (e.g. `.json(some_vec)` in a handler returning a list) -
    /// the same `Fourth(...)` shape the derive already gives a `Vec<T>` STRUCT FIELD, now also
    /// available directly on `Vec<T>` itself.
    #[test]
    fn vec_as_term_round_trips_a_list_of_strings() {
        let v = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(Vec::<String>::from_term(&v.to_term()).unwrap(), v);
    }

    #[test]
    fn vec_as_term_round_trips_an_empty_list() {
        let v: Vec<i64> = vec![];
        assert_eq!(Vec::<i64>::from_term(&v.to_term()).unwrap(), v);
    }

    #[test]
    fn vec_as_term_rejects_a_non_list_term() {
        let term = SyntaxTerm::B(Atom::Str("not a list".to_string()));
        assert!(Vec::<String>::from_term(&term).is_err());
    }

    #[test]
    fn vec_as_term_round_trips_nested_tuples() {
        let v = vec![("a".to_string(), 1i64), ("b".to_string(), 2i64)];
        assert_eq!(Vec::<(String, i64)>::from_term(&v.to_term()).unwrap(), v);
    }

    /// A real semgrep rule (`~/projects/subsume_lint/semgrep-rules/python/distributed/
    /// security.yaml`, `require-encryption`), verified byte-for-byte against the actual file:
    /// nested block sequences under a `- id: ...` item, a doubly-nested mapping
    /// (`metavariable-pattern:`), and both `|` (clip) and `>-` (folded+strip) block scalars.
    #[test]
    fn parses_a_real_semgrep_rule_exactly() {
        let src = r#"rules:
- id: require-encryption
  patterns:
  - pattern: |
      distributed.security.Security(..., require_encryption=$VAL, ...)
  - metavariable-pattern:
      metavariable: $VAL
      pattern: |
        False
  - focus-metavariable: $VAL
  fix: |
    True
  message: >-
    Initializing a security context for Dask (`distributed`) without "require_encryption" keyword
    argument may silently fail to provide security.
  severity: WARNING
  metadata:
    cwe:
    - 'CWE-319: Cleartext Transmission of Sensitive Information'
    references:
    - https://distributed.dask.org/en/latest/tls.html?highlight=require_encryption#parameters
  languages:
  - python
"#;
        let term = parse_surface(Surface::Yaml, src).unwrap();
        let SyntaxTerm::Bu(top) = &term else {
            panic!("expected top-level mapping")
        };
        let SyntaxTerm::Fourth(rules) = field(top, "rules").unwrap() else {
            panic!("rules must be a sequence")
        };
        let SyntaxTerm::Bu(rule) = &rules[0] else {
            panic!("rule must be a mapping")
        };

        assert_eq!(field_str(rule, "id").unwrap(), "require-encryption");
        assert_eq!(field_str(rule, "fix").unwrap(), "True\n"); // `|` clip: keeps one trailing \n
        assert_eq!(
            field_str(rule, "message").unwrap(),
            "Initializing a security context for Dask (`distributed`) without \"require_encryption\" keyword argument may silently fail to provide security."
        ); // `>-` folded+strip: two lines joined by a space, no trailing \n
        assert_eq!(field_str(rule, "severity").unwrap(), "WARNING");

        let SyntaxTerm::Fourth(patterns) = field(rule, "patterns").unwrap() else {
            panic!("patterns must be a sequence")
        };
        assert_eq!(patterns.len(), 3);
        let SyntaxTerm::Bu(p0) = &patterns[0] else {
            panic!()
        };
        assert_eq!(
            field_str(p0, "pattern").unwrap(),
            "distributed.security.Security(..., require_encryption=$VAL, ...)\n"
        );
        let SyntaxTerm::Bu(p1) = &patterns[1] else {
            panic!()
        };
        let SyntaxTerm::Bu(mp) = field(p1, "metavariable-pattern").unwrap() else {
            panic!("nested mapping")
        };
        assert_eq!(field_str(mp, "metavariable").unwrap(), "$VAL");
        assert_eq!(field_str(mp, "pattern").unwrap(), "False\n");

        // a quoted list-item scalar containing its own colon (`'CWE-319: ...'`) must NOT be
        // mistaken for a mapping key — the whole quoted string is one scalar.
        let SyntaxTerm::Bu(meta) = field(rule, "metadata").unwrap() else {
            panic!()
        };
        let SyntaxTerm::Fourth(cwe) = field(meta, "cwe").unwrap() else {
            panic!()
        };
        assert_eq!(
            term_str(&cwe[0]).unwrap(),
            "CWE-319: Cleartext Transmission of Sensitive Information"
        );

        // a plain-scalar list item containing `:` and an UNSPACED `#` (a URL fragment) must
        // parse whole, not truncate at the `#` (YAML only treats `#` as a comment when it's
        // preceded by whitespace).
        let SyntaxTerm::Fourth(refs) = field(meta, "references").unwrap() else {
            panic!()
        };
        assert_eq!(
            term_str(&refs[0]).unwrap(),
            "https://distributed.dask.org/en/latest/tls.html?highlight=require_encryption#parameters"
        );
    }

    #[test]
    fn yaml_flow_collections_and_implicit_null() {
        // `{UUID}` is real YAML's implicit-null-value shorthand for `{UUID: null}` — found as a
        // genuine parse failure against the real semgrep-rules corpus (a kubernetes test
        // fixture) before this was handled.
        let term = parse_surface(
            Surface::Yaml,
            "data:\n  UUID: {UUID}\n  tags: [a, b, 'c:d']\n",
        )
        .unwrap();
        let SyntaxTerm::Bu(top) = &term else { panic!() };
        let SyntaxTerm::Bu(data) = field(top, "data").unwrap() else {
            panic!()
        };
        // the VALUE `{UUID}` is itself a flow mapping with one implicit-null entry (key "UUID"),
        // not `Nb` directly — `data.UUID` names that whole nested map.
        assert_eq!(
            field(data, "UUID"),
            Some(&SyntaxTerm::Bu(vec![("UUID".to_string(), SyntaxTerm::Nb)]))
        );
        let SyntaxTerm::Fourth(tags) = field(data, "tags").unwrap() else {
            panic!()
        };
        assert_eq!(term_str(&tags[0]).unwrap(), "a");
        assert_eq!(term_str(&tags[2]).unwrap(), "c:d");
    }

    #[test]
    fn yaml_scalars_and_sequence_dash_alignment() {
        // A sequence value is allowed to align with its own key's indent (not strictly deeper)
        // — the real YAML quirk semgrep's own rule files rely on throughout.
        let src = "a: 1\nb: true\nc: null\nlist:\n- x\n- y\n";
        let term = parse_surface(Surface::Yaml, src).unwrap();
        let SyntaxTerm::Bu(top) = &term else { panic!() };
        assert_eq!(field(top, "a"), Some(&SyntaxTerm::B(Atom::Num("1".into()))));
        assert_eq!(field(top, "b"), Some(&SyntaxTerm::B(Atom::Bool(true))));
        assert_eq!(field(top, "c"), Some(&SyntaxTerm::Nb));
        let SyntaxTerm::Fourth(list) = field(top, "list").unwrap() else {
            panic!()
        };
        assert_eq!(term_str(&list[0]).unwrap(), "x");
        assert_eq!(term_str(&list[1]).unwrap(), "y");
    }

    /// The entire real semgrep-rules corpus (2185 `.yaml`/`.yml` files cloned at
    /// `~/projects/subsume_lint/semgrep-rules`) parses cleanly — this is the same corpus the
    /// prior anchor-lift/structural-matcher work in `sangsec`/`slint` was measured against.
    /// `#[ignore]`d: depends on a local clone outside this repo, run explicitly to re-verify.
    #[test]
    #[ignore = "depends on a local semgrep-rules clone outside this repo; run explicitly to re-verify"]
    fn real_semgrep_rules_corpus_parses_cleanly() {
        let root = std::path::Path::new(&std::env::var("HOME").unwrap())
            .join("projects/subsume_lint/semgrep-rules");
        if !root.exists() {
            eprintln!("skip: {} not present", root.display());
            return;
        }
        let mut total = 0;
        let mut failed = Vec::new();
        fn walk(
            dir: &std::path::Path,
            total: &mut usize,
            failed: &mut Vec<(std::path::PathBuf, String)>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    walk(&p, total, failed);
                } else if p.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                    *total += 1;
                    let src = std::fs::read_to_string(&p).unwrap_or_default();
                    if let Err(e) = parse_surface(Surface::Yaml, &src) {
                        failed.push((p, e.msg));
                    }
                }
            }
        }
        walk(&root, &mut total, &mut failed);
        assert!(
            total > 2000,
            "expected the real corpus (~2185 files), found {total}"
        );
        assert!(
            failed.is_empty(),
            "{} of {} files failed to parse: {:?}",
            failed.len(),
            total,
            failed.first()
        );
    }

    fn call() -> HttpCall {
        HttpCall {
            method: "POST".into(),
            url: "https://api.example.test/v1/messages".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: SyntaxTerm::Bu(vec![
                ("model".into(), SyntaxTerm::B(Atom::Str("s1".into()))),
                (
                    "messages".into(),
                    SyntaxTerm::Fourth(vec![SyntaxTerm::Bu(vec![
                        ("role".into(), SyntaxTerm::B(Atom::Str("user".into()))),
                        ("content".into(), SyntaxTerm::B(Atom::Str("hello".into()))),
                    ])]),
                ),
            ]),
        }
    }

    #[test]
    fn json_baml_toon_poml_are_same_term() {
        let term = call().to_term();
        for surface in [
            Surface::Json,
            Surface::Baml,
            Surface::Csv,
            Surface::Toon,
            Surface::Poml,
        ] {
            let text = render_surface(&term, surface);
            let parsed = parse_surface(surface, &text).unwrap();
            assert_eq!(parsed, term, "{surface:?} failed round-trip: {text}");
        }
    }

    #[test]
    fn http_projection_roundtrips() {
        let c = call();
        assert_eq!(HttpCall::from_term(&c.to_term()).unwrap(), c);
    }

    #[test]
    fn cerealize_roundtrips_the_collapsed_term() {
        let term = call().to_term();
        let encoded = cerealize_term(&term);
        assert_eq!(decerealize_term(&encoded).unwrap(), term);
    }

    #[test]
    fn cerealize_roundtrips_http() {
        let c = call();
        let encoded = cerealize_http(&c);
        assert_eq!(decerealize_http(&encoded).unwrap(), c);
    }

    #[test]
    fn translate_json_to_all_surfaces() {
        let json = render_surface(&call().to_term(), Surface::Json);
        for target in [Surface::Baml, Surface::Csv, Surface::Toon, Surface::Poml] {
            let out = translate(&json, Surface::Json, target).unwrap();
            let back = parse_surface(target, &out).unwrap();
            assert_eq!(back, call().to_term());
        }
    }

    #[test]
    fn lint_generated_surfaces_are_clean() {
        let term = call().to_term();
        for surface in [
            Surface::Json,
            Surface::Baml,
            Surface::Csv,
            Surface::Toon,
            Surface::Poml,
        ] {
            let text = render_surface(&term, surface);
            assert_eq!(lint_surface(surface, &text), Vec::new(), "{surface:?}");
        }
    }

    #[test]
    fn lint_finds_duplicate_keys() {
        let term = SyntaxTerm::Bu(vec![
            ("a".into(), SyntaxTerm::Nb),
            ("a".into(), SyntaxTerm::B(Atom::Bool(true))),
        ]);
        let lints = lint_term(&term);
        assert!(lints.iter().any(|lint| {
            lint.level == LintLevel::Warning && lint.msg.contains("duplicate B/U key")
        }));
    }

    #[test]
    fn lint_http_shape_checks_required_fields() {
        let text = r#"{"method":"POST","headers":{},"body":null}"#;
        let lints = lint_http_surface(Surface::Json, text);
        assert!(lints
            .iter()
            .any(|lint| lint.level == LintLevel::Error && lint.msg.contains("missing field url")));
    }

    #[test]
    fn json_lint_rejects_bare_keys() {
        let lints = lint_surface(Surface::Json, "{method:\"POST\"}");
        assert!(lints.iter().any(|lint| {
            lint.level == LintLevel::Error && lint.msg.contains("expected quoted object key")
        }));
    }

    /// `\b`, `\f` and `\uXXXX` are real JSON escapes. Dropping the backslash and keeping the letter
    /// silently aliases the key `"\b"` onto the key `"b"` — which is exactly how a HuggingFace
    /// `tokenizer.json` loses the letter *b* to the control character. `\uXXXX` lands as
    /// a real Unicode scalar (surrogate pairs combined): the parser accumulates BYTES and decodes
    /// once at the end. An earlier form widened each byte to a `char` and mangled every multi-byte
    /// sequence.
    #[test]
    fn json_string_escapes_are_decoded_not_swallowed() {
        let t = parse_surface(Surface::Json, r#"{"\b":1,"b":2,"\f":3,"f":4}"#).unwrap();
        let keys: Vec<&str> = match &t {
            SyntaxTerm::Bu(v) => v.iter().map(|(k, _)| k.as_str()).collect(),
            _ => panic!("object"),
        };
        assert_eq!(keys, vec!["\u{8}", "b", "\u{c}", "f"]);

        let t = parse_surface(Surface::Json, r#"["\u0001","\u2581","\ud83d\ude00","\/"]"#).unwrap();
        let items: Vec<String> = match &t {
            SyntaxTerm::Fourth(v) => v
                .iter()
                .map(|x| match x {
                    SyntaxTerm::B(Atom::Str(s)) => s.clone(),
                    _ => panic!("string"),
                })
                .collect(),
            _ => panic!("array"),
        };
        let scalars = |s: &String| s.chars().map(|c| c as u32).collect::<Vec<_>>();
        assert_eq!(scalars(&items[0]), vec![0x01]);
        // ONE scalar, not three bytes-as-chars -- the accumulator decodes once at the end
        assert_eq!(scalars(&items[1]), vec![0x2581]);
        assert_eq!(scalars(&items[2]), vec![0x1F600]);
        assert_eq!(items[3], "/");
    }

    /// A literal (unescaped) multi-byte character decodes to its scalar, and to the *same* thing its
    /// `\uXXXX` spelling does — the escaped and raw forms of one string are one string. The earlier
    /// byte-widening reader failed the raw half: `中` arrived as its 3 continuation bytes read as 3
    /// separate Latin-1 codepoints.
    #[test]
    fn json_decodes_literal_and_escaped_nonascii_to_the_same_scalars() {
        let one = |src: &str| match parse_surface(Surface::Json, src).unwrap() {
            SyntaxTerm::Fourth(v) => match &v[0] {
                SyntaxTerm::B(Atom::Str(s)) => s.clone(),
                _ => panic!("string"),
            },
            _ => panic!("array"),
        };
        let scalars = |s: &str| s.chars().map(|c| c as u32).collect::<Vec<_>>();

        // 4 scalars, not 12 bytes-as-chars.
        assert_eq!(
            scalars(&one(r#"["中文测试"]"#)),
            vec![0x4E2D, 0x6587, 0x6D4B, 0x8BD5]
        );
        assert_eq!(one(r#"["中文测试"]"#), one(r#"["中文测试"]"#));
        // and above the BMP, where the escaped form is a surrogate pair: still one scalar, and still
        // the same string as the literal.
        assert_eq!(scalars(&one(r#"["😀"]"#)), vec![0x1F600]);
        assert_eq!(one(r#"["😀"]"#), one(r#"["😀"]"#));
    }

    /// A GPT-2 byte-level vocabulary spells each byte as its own printable char, and those chars are
    /// real scalars — `Ġ` is U+0120. Reading such a key gives the scalars *as written*: `Ã©` (how the
    /// two UTF-8 bytes of `é` are spelled) stays two chars and must NOT be folded into one `é`. This
    /// is the property `modelfold`'s tokenizer loader consumes; it needs no byte-preserving parser
    /// variant, only this one not decoding twice.
    #[test]
    fn json_reads_byte_level_vocab_keys_as_written() {
        let t = parse_surface(
            Surface::Json,
            "{\"\u{c3}\u{a9}\": 0, \"\u{120}\": 1, \"\u{c4}\u{a0}\": 2}",
        )
        .unwrap();
        let keys: Vec<Vec<u32>> = match &t {
            SyntaxTerm::Bu(v) => v
                .iter()
                .map(|(k, _)| k.chars().map(|c| c as u32).collect())
                .collect(),
            _ => panic!("object"),
        };
        assert_eq!(
            keys[0],
            vec![0xC3, 0xA9],
            "`Ã©` must stay two scalars, not collapse to `é`"
        );
        assert_eq!(keys[1], vec![0x120], "`Ġ` is one scalar U+0120");
        assert_eq!(
            keys[2],
            vec![0xC4, 0xA0],
            "and its byte-pair spelling stays distinct from it"
        );
    }
    // ── Turtle (RDF) ──

    #[test]
    fn turtle_parses_simple_triple_with_a_shorthand() {
        let term = parse_turtle("<http://ex/C> <http://ex/p> <http://ex/D> .").unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].0, "http://ex/C");
    }

    #[test]
    fn turtle_expands_prefixed_names_and_a_shorthand_for_rdf_type() {
        let ttl = r#"
@prefix ex: <http://example.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
ex:Alice a ex:Person .
ex:Alice ex:name "Alice" .
"#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        // Two subjects: Alice (Person, name) — `a` expands to rdf:type
        let alice = subs
            .iter()
            .find(|(s, _)| s == "http://example.org/Alice")
            .unwrap();
        let SyntaxTerm::Bu(preds) = &alice.1 else {
            panic!("expected Bu")
        };
        // two predicates: type + name
        assert!(preds
            .iter()
            .any(|(p, _)| p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"));
        let name_pred = preds
            .iter()
            .find(|(p, _)| p == "http://example.org/name")
            .unwrap();
        let SyntaxTerm::Fourth(objs) = &name_pred.1 else {
            panic!("expected Fourth")
        };
        assert_eq!(objs.len(), 1);
        assert_eq!(format!("{:?}", objs[0]), r#"B(Str("\"Alice\""))"#);
    }

    #[test]
    fn turtle_handles_semicolon_for_repeated_predicates() {
        let ttl = r#"
@prefix ex: <http://ex/> .
ex:bob ex:name "Bob" ;
      ex:age 42 ;
      a ex:Person .
"#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let bob = subs.iter().find(|(s, _)| s == "http://ex/bob").unwrap();
        let SyntaxTerm::Bu(preds) = &bob.1 else {
            panic!("expected Bu")
        };
        assert_eq!(
            preds.len(),
            3,
            "expected 3 distinct predicates, got {}",
            preds.len()
        );
    }

    #[test]
    fn turtle_handles_comma_for_multiple_objects_on_same_predicate() {
        let ttl = r#"
@prefix ex: <http://ex/> .
ex:alice ex:knows ex:bob , ex:carol , ex:dave .
"#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let alice = subs.iter().find(|(s, _)| s == "http://ex/alice").unwrap();
        let SyntaxTerm::Bu(preds) = &alice.1 else {
            panic!("expected Bu")
        };
        let knows = preds.iter().find(|(p, _)| p == "http://ex/knows").unwrap();
        let SyntaxTerm::Fourth(objs) = &knows.1 else {
            panic!("expected Fourth")
        };
        assert_eq!(objs.len(), 3);
    }

    #[test]
    fn turtle_handles_blank_nodes() {
        let ttl = r#"
@prefix ex: <http://ex/> .
_:b1 ex:temp 22.5 .
"#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        assert!(subs.iter().any(|(s, _)| s.starts_with("_:")));
    }

    #[test]
    fn turtle_handles_typed_and_lang_literals() {
        let ttl = r#"
@prefix ex: <http://ex/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:alice ex:age "30"^^xsd:integer .
ex:alice ex:greeting "hello"@en .
"#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        // alice has both predicates; both literals carry their type/lang markers
        let alice = subs.iter().find(|(s, _)| s == "http://ex/alice").unwrap();
        let SyntaxTerm::Bu(preds) = &alice.1 else {
            panic!("expected Bu");
        };
        let age = preds.iter().find(|(p, _)| p == "http://ex/age").unwrap();
        let SyntaxTerm::Fourth(age_objs) = &age.1 else {
            panic!("expected Fourth")
        };
        // prefix `xsd` should have been expanded to the full IRI
        assert!(age_objs.iter().any(|t| matches!(t, SyntaxTerm::B(Atom::Str(s)) if s.contains("^^http://www.w3.org/2001/XMLSchema#integer"))));
        let greeting = preds
            .iter()
            .find(|(p, _)| p == "http://ex/greeting")
            .unwrap();
        let SyntaxTerm::Fourth(greeting_objs) = &greeting.1 else {
            panic!("expected Fourth")
        };
        assert!(greeting_objs
            .iter()
            .any(|t| matches!(t, SyntaxTerm::B(Atom::Str(s)) if s.contains("@en"))));
    }

    #[test]
    fn turtle_handles_comments() {
        let ttl = "# a comment\n@prefix ex: <http://ex/> .\nex:x ex:y ex:z . # trailing\n";
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        assert!(subs.iter().any(|(s, _)| s == "http://ex/x"));
    }

    #[test]
    fn turtle_round_trip_renders_back() {
        let ttl = "@prefix ex: <http://ex/> .\nex:C ex:p ex:D .\n";
        let term = parse_turtle(ttl).unwrap();
        let rendered = render_turtle(&term);
        // Re-parse and verify we get the same subjects
        let term2 = parse_turtle(&rendered).unwrap();
        let SyntaxTerm::Bu(s1) = &term else { panic!() };
        let SyntaxTerm::Bu(s2) = &term2 else { panic!() };
        assert_eq!(s1.len(), s2.len());
        assert_eq!(s1[0].0, s2[0].0);
    }

    #[test]
    fn turtle_unknown_prefix_fails_loudly() {
        // No @prefix declaration for `ex:` — must fail, not silently drop
        let term = parse_turtle("ex:x ex:y ex:z .");
        assert!(term.is_err(), "expected unknown-prefix error, got {term:?}");
    }

    #[test]
    fn turtle_unicode_escape_in_literal() {
        let ttl = r#"@prefix ex: <http://ex/> . ex:greeting ex:value "h\u00e9llo" ."#;
        let term = parse_turtle(ttl).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let g = subs
            .iter()
            .find(|(s, _)| s == "http://ex/greeting")
            .unwrap();
        let SyntaxTerm::Bu(preds) = &g.1 else {
            panic!()
        };
        // the single predicate-object pair: the literal contains the é
        let any_literal = preds
            .iter()
            .flat_map(|(_, o)| {
                if let SyntaxTerm::Fourth(items) = o {
                    items.iter()
                } else {
                    [].iter()
                }
            })
            .find_map(|t| {
                if let SyntaxTerm::B(Atom::Str(s)) = t {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .expect("expected a literal string");
        assert!(
            any_literal.contains("héllo"),
            "expected héllo in literal, got {any_literal}"
        );
    }

    // ── RDF/XML ──

    #[test]
    fn rdfxml_parses_simple_description() {
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://ex/">
  <rdf:Description rdf:about="http://ex/alice">
    <ex:name>Alice</ex:name>
  </rdf:Description>
</rdf:RDF>"#;
        let term = parse_rdfxml(xml).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let alice = subs.iter().find(|(s, _)| s == "http://ex/alice").unwrap();
        let SyntaxTerm::Bu(preds) = &alice.1 else {
            panic!("expected Bu");
        };
        let name = preds.iter().find(|(p, _)| p.contains("name")).unwrap();
        let SyntaxTerm::Fourth(objs) = &name.1 else {
            panic!()
        };
        assert_eq!(objs.len(), 1);
    }

    #[test]
    fn rdfxml_handles_rdf_resource_for_iri_objects() {
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://ex/">
  <rdf:Description rdf:about="http://ex/alice">
    <ex:knows rdf:resource="http://ex/bob" />
    <ex:knows rdf:resource="http://ex/carol" />
  </rdf:Description>
</rdf:RDF>"#;
        let term = parse_rdfxml(xml).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let alice = subs.iter().find(|(s, _)| s == "http://ex/alice").unwrap();
        let SyntaxTerm::Bu(preds) = &alice.1 else {
            panic!("expected Bu");
        };
        let knows = preds.iter().find(|(p, _)| p.contains("knows")).unwrap();
        let SyntaxTerm::Fourth(objs) = &knows.1 else {
            panic!()
        };
        assert_eq!(objs.len(), 2);
        assert!(objs
            .iter()
            .any(|t| matches!(t, SyntaxTerm::B(Atom::Str(s)) if s == "http://ex/bob")));
        assert!(objs
            .iter()
            .any(|t| matches!(t, SyntaxTerm::B(Atom::Str(s)) if s == "http://ex/carol")));
    }

    #[test]
    fn rdfxml_handles_rdf_node_id_for_blank_nodes() {
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://ex/">
  <rdf:Description rdf:about="http://ex/alice">
    <ex:address rdf:nodeID="addr1" />
  </rdf:Description>
  <rdf:Description rdf:nodeID="addr1">
    <ex:city>Wonderland</ex:city>
  </rdf:Description>
</rdf:RDF>"#;
        let term = parse_rdfxml(xml).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        // The blank node becomes a _: prefixed subject
        assert!(subs.iter().any(|(s, _)| s.starts_with("_:addr1")));
    }

    #[test]
    fn rdfxml_parses_multiple_descriptions() {
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://ex/">
  <rdf:Description rdf:about="http://ex/a">
    <ex:p>1</ex:p>
  </rdf:Description>
  <rdf:Description rdf:about="http://ex/b">
    <ex:p>2</ex:p>
  </rdf:Description>
</rdf:RDF>"#;
        let term = parse_rdfxml(xml).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        assert_eq!(subs.len(), 2);
    }

    #[test]
    fn parse_surface_routes_turtle_and_rdfxml() {
        // Same term through parse_surface as through parse_turtle/parse_rdfxml
        let ttl = "@prefix ex: <http://ex/> .\nex:A ex:p ex:B .";
        let t1 = parse_surface(Surface::Turtle, ttl).unwrap();
        let t2 = parse_turtle(ttl).unwrap();
        assert_eq!(t1, t2);

        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:ex="http://ex/">
  <rdf:Description rdf:about="http://ex/A">
    <ex:p>1</ex:p>
  </rdf:Description>
</rdf:RDF>"#;
        let t3 = parse_surface(Surface::RdfXml, xml).unwrap();
        let t4 = parse_rdfxml(xml).unwrap();
        assert_eq!(t3, t4);
    }

    #[test]
    fn rdfxml_emits_implicit_rdf_type_for_owl_class_tag() {
        // OWL/RDF shorthand: `<owl:Class rdf:about="...">` is implicit
        // `... rdf:type owl:Class` for the subject. The parser emits the
        // explicit triple so downstream walkers (e.g. ontology lifters) see
        // the same shape they'd see from a Turtle source.
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Class rdf:about="http://ex/Foo">
    <rdfs:label>Foo</rdfs:label>
  </owl:Class>
</rdf:RDF>"#;
        let term = parse_rdfxml(xml).unwrap();
        let SyntaxTerm::Bu(subs) = &term else {
            panic!("expected Bu");
        };
        let foo = subs.iter().find(|(s, _)| s == "http://ex/Foo").unwrap();
        let SyntaxTerm::Bu(preds) = &foo.1 else {
            panic!()
        };
        // the implicit rdf:type triples should be present
        let type_iri = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
        let mut found_owl_class = false;
        for (p, val) in preds {
            if p == type_iri {
                if let SyntaxTerm::Fourth(items) = val {
                    for item in items {
                        if let SyntaxTerm::B(Atom::Str(s)) = item {
                            if s == "http://www.w3.org/2002/07/owl#Class" {
                                found_owl_class = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(found_owl_class, "expected implicit rdf:type owl:Class");
    }
}
