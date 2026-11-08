//! Denial-of-service guards for the untrusted-input parsers (audit findings MQ-03 / MEM-07).
//!
//! Every test here feeds nesting that is linear in the input size. Before the depth limit these
//! aborted the whole process with `thread '...' has overflowed its stack` (SIGABRT, not catchable),
//! so a green run means "returns an error instead of aborting". The tests run on the default 2 MiB
//! test-thread stack, which is the harshest case.
#![cfg(feature = "syntax")]

use wirefold::codec::syntax::{parse_surface, Surface};

fn nested(open: &str, close: &str, n: usize, inner: &str) -> String {
    format!("{}{}{}", open.repeat(n), inner, close.repeat(n))
}

fn err_msg(surface: Surface, input: &str) -> String {
    match parse_surface(surface, input) {
        Ok(_) => panic!(
            "expected an error for nesting input of {} bytes",
            input.len()
        ),
        Err(e) => e.msg,
    }
}

#[test]
fn json_deeply_nested_arrays_error_instead_of_overflowing_the_stack() {
    for n in [1_000usize, 10_000, 1_000_000] {
        let msg = err_msg(Surface::Json, &"[".repeat(n));
        assert!(msg.contains("depth"), "n={n}: {msg}");
        // closed as well as unclosed
        let msg = err_msg(Surface::Json, &nested("[", "]", n, "1"));
        assert!(msg.contains("depth"), "n={n}: {msg}");
    }
}

#[test]
fn json_deeply_nested_objects_error_instead_of_overflowing_the_stack() {
    for n in [1_000usize, 100_000] {
        let input = format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n));
        let msg = err_msg(Surface::Json, &input);
        assert!(msg.contains("depth"), "n={n}: {msg}");
    }
}

#[test]
fn json_nesting_limit_is_128_like_serde_json() {
    assert!(parse_surface(Surface::Json, &nested("[", "]", 128, "")).is_ok());
    assert!(parse_surface(Surface::Json, &nested("[", "]", 129, "")).is_err());
    let obj = |n: usize| format!("{}null{}", "{\"k\":".repeat(n), "}".repeat(n));
    assert!(parse_surface(Surface::Json, &obj(128)).is_ok());
    assert!(parse_surface(Surface::Json, &obj(129)).is_err());
}

#[test]
fn baml_and_toon_share_the_json_nesting_limit() {
    let deep = nested("[", "]", 100_000, "1");
    assert!(err_msg(Surface::Baml, &format!("baml {deep}")).contains("depth"));
    assert!(err_msg(Surface::Toon, &format!("a = {deep}")).contains("depth"));
    // loose-mode objects too
    let deep = format!("{}1{}", "{a = ".repeat(100_000), "}".repeat(100_000));
    assert!(err_msg(Surface::Baml, &format!("baml {deep}")).contains("depth"));
}

#[test]
fn poml_deep_nesting_errors() {
    let n = 100_000;
    let list = format!(
        "<poml>{}<nb/>{}</poml>",
        "<list><item>".repeat(n),
        "</item></list>".repeat(n)
    );
    assert!(err_msg(Surface::Poml, &list).contains("depth"));
    let bu = format!(
        "<poml>{}<nb/>{}</poml>",
        "<bu><field name=\"a\">".repeat(n),
        "</field></bu>".repeat(n)
    );
    assert!(err_msg(Surface::Poml, &bu).contains("depth"));
}

#[test]
fn yaml_deep_nesting_errors() {
    // flow collections: input linear in depth
    let n = 100_000;
    let flow = format!("k: {}", nested("[", "]", n, "1"));
    assert!(err_msg(Surface::Yaml, &flow).contains("depth"));
    let flow = format!("k: {}1{}", "{a: ".repeat(n), "}".repeat(n));
    assert!(err_msg(Surface::Yaml, &flow).contains("depth"));
    // block mappings: indentation grows by one per level (quadratic input), 3000 levels is 4.5 MB
    let mut block = String::new();
    for i in 0..3_000 {
        block.push_str(&" ".repeat(i));
        block.push_str("k:\n");
    }
    block.push_str(&" ".repeat(3_000));
    block.push_str("v: 1\n");
    assert!(err_msg(Surface::Yaml, &block).contains("depth"));
}

#[test]
fn rdfxml_nested_parse_type_resource_errors() {
    let n = 100_000;
    let doc = format!(
        "<rdf:RDF><rdf:Description rdf:about=\"x\">{}<p>v</p>{}</rdf:Description></rdf:RDF>",
        "<q rdf:parseType=\"Resource\">".repeat(n),
        "</q>".repeat(n)
    );
    assert!(err_msg(Surface::RdfXml, &doc).contains("depth"));
}

/// A chain `n` levels deep, built iteratively. Terms built in memory (not by a parser) can be
/// deeper than any parser allows, so the renderers must not recurse per level. The terms are leaked:
/// the derived recursive `Drop` of `SyntaxTerm` lives in `codec.rs` and is not what is under test.
fn deep_chain(n: usize, objects: bool) -> wirefold::codec::syntax::SyntaxTerm {
    use wirefold::codec::syntax::{Atom, SyntaxTerm};
    let mut t = SyntaxTerm::B(Atom::Num("1".into()));
    for _ in 0..n {
        t = if objects {
            SyntaxTerm::Bu(vec![("k".into(), t)])
        } else {
            SyntaxTerm::Fourth(vec![t])
        };
    }
    t
}

fn render_deep(surface: Surface, n: usize, objects: bool) -> String {
    let t = deep_chain(n, objects);
    let out = wirefold::codec::syntax::render_surface(&t, surface);
    std::mem::forget(t);
    out
}

#[test]
fn render_json_does_not_recurse_per_level() {
    assert_eq!(
        render_deep(Surface::Json, 200_000, false).len(),
        2 * 200_000 + 1
    );
    assert_eq!(
        render_deep(Surface::Json, 200_000, true).len(),
        200_000 * 6 + 1
    );
}

#[test]
fn render_baml_does_not_recurse_per_level() {
    let out = render_deep(Surface::Baml, 200_000, false);
    assert_eq!(
        out,
        format!("baml {}1{}\n", "[".repeat(200_000), "]".repeat(200_000))
    );
    // objects indent by depth, so the output is quadratic in depth: keep n modest
    let out = render_deep(Surface::Baml, 4_000, true);
    assert!(out.starts_with("baml {\n  k = {\n    k = {"));
    assert!(out.ends_with("}\n}\n"));
}

#[test]
fn render_poml_does_not_recurse_per_level() {
    let out = render_deep(Surface::Poml, 200_000, false);
    assert!(
        out.starts_with("<poml><list><item><list>") && out.ends_with("</item></list></poml>\n")
    );
    let out = render_deep(Surface::Poml, 200_000, true);
    assert!(out.starts_with("<poml><bu><field name=\"k\"><bu>"));
}

#[test]
fn render_toon_does_not_recurse_per_level() {
    let out = render_deep(Surface::Toon, 20_000, false);
    assert!(out.starts_with("$[0][0][0]") && out.trim_end().ends_with("= 1"));
    let out = render_deep(Surface::Toon, 20_000, true);
    assert!(out.starts_with("k.k.k") && out.trim_end().ends_with("= 1"));
}

#[test]
fn render_csv_does_not_recurse_per_level() {
    let out = render_deep(Surface::Csv, 4_000, false);
    assert!(out.starts_with("path,kind,value\n$,fourth,\n[0],fourth,\n[0][0],fourth,"));
    assert!(out.trim_end().ends_with(",num,1"));
    let out = render_deep(Surface::Csv, 4_000, true);
    assert!(out.starts_with("path,kind,value\n$,bu,\nk,bu,\nk.k,bu,"));
}

/// Golden output of the renderers, captured from the recursive implementations they replaced (so
/// the iterative rewrite is byte-for-byte the same on a term that mixes every shape).
#[test]
fn renderers_match_the_output_of_the_recursive_implementations() {
    use wirefold::codec::syntax::{render_surface, Atom, SyntaxTerm};
    let s = |v: &str| SyntaxTerm::B(Atom::Str(v.into()));
    let t = SyntaxTerm::Bu(vec![
        ("a".into(), SyntaxTerm::B(Atom::Num("1".into()))),
        (
            "b \"q\"".into(),
            SyntaxTerm::Fourth(vec![
                SyntaxTerm::Nb,
                SyntaxTerm::B(Atom::Bool(true)),
                s("x,\"y\"\n<&>"),
                SyntaxTerm::Fourth(vec![]),
                SyntaxTerm::Bu(vec![]),
            ]),
        ),
        (
            "c".into(),
            SyntaxTerm::Bu(vec![
                ("d".into(), SyntaxTerm::Unknown),
                (
                    "e".into(),
                    SyntaxTerm::Bu(vec![(
                        "f".into(),
                        SyntaxTerm::Fourth(vec![SyntaxTerm::Bu(vec![("g".into(), s("h"))])]),
                    )]),
                ),
            ]),
        ),
    ]);
    let expected = [
        (Surface::Json, "{\"a\":1,\"b \\\"q\\\"\":[null,true,\"x,\\\"y\\\"\\n<&>\",[],{}],\"c\":{\"d\":null,\"e\":{\"f\":[{\"g\":\"h\"}]}}}"),
        (Surface::Baml, "baml {\n  a = 1\n  b \"q\" = [nb, true, \"x,\\\"y\\\"\\n<&>\", [], {\n  }]\n  c = {\n    d = nb\n    e = {\n      f = [{\n        g = \"h\"\n      }]\n    }\n  }\n}\n"),
        (Surface::Csv, "path,kind,value\n$,bu,\na,num,1\n\"b \"\"q\"\"\",fourth,\n\"b \"\"q\"\"[0]\",nb,\n\"b \"\"q\"\"[1]\",bool,true\n\"b \"\"q\"\"[2]\",str,\"x,\"\"y\"\"\n<&>\"\n\"b \"\"q\"\"[3]\",fourth,\n\"b \"\"q\"\"[4]\",bu,\nc,bu,\nc.d,unknown,\nc.e,bu,\nc.e.f,fourth,\nc.e.f[0],bu,\nc.e.f[0].g,str,h\n"),
        (Surface::Toon, "a = 1\nb \"q\"[0] = nb\nb \"q\"[1] = true\nb \"q\"[2] = \"x,\\\"y\\\"\\n<&>\"\nc.d = nb\nc.e.f[0].g = \"h\"\n"),
        (Surface::Poml, "<poml><bu><field name=\"a\"><b kind=\"num\">1</b></field><field name=\"b &quot;q&quot;\"><list><item><nb/></item><item><b kind=\"bool\">true</b></item><item><b kind=\"str\">x,&quot;y&quot;\n&lt;&amp;&gt;</b></item><item><list></list></item><item><bu></bu></item></list></field><field name=\"c\"><bu><field name=\"d\"><nb/></field><field name=\"e\"><bu><field name=\"f\"><list><item><bu><field name=\"g\"><b kind=\"str\">h</b></field></bu></item></list></field></bu></field></bu></field></bu></poml>\n"),
    ];
    for (surface, want) in expected {
        assert_eq!(render_surface(&t, surface), want, "{surface:?}");
    }
}

/// TOON and CSV address values by path (`a.b[2].c`). A path of N segments is N levels of the tree
/// the parser builds (and then finishes and drops), so it is a nesting bound too.
#[test]
fn toon_and_csv_paths_have_a_depth_limit() {
    let toon = format!("{} = 1", vec!["a"; 100_000].join("."));
    assert!(err_msg(Surface::Toon, &toon).contains("depth"));
    let toon = format!("a{} = 1", "[0]".repeat(100_000));
    assert!(err_msg(Surface::Toon, &toon).contains("depth"));
    let csv = format!("path,kind,value\n{},num,1\n", vec!["a"; 100_000].join("."));
    assert!(err_msg(Surface::Csv, &csv).contains("depth"));
    // a path at the limit is fine
    let ok = format!("{} = 1", vec!["a"; 128].join("."));
    assert!(parse_surface(Surface::Toon, &ok).is_ok());
}

/// `a[N] = 1` made the parser allocate N list slots from one short line.
#[test]
fn toon_and_csv_sparse_indexes_cannot_amplify_memory() {
    assert!(parse_surface(Surface::Toon, "a[20000000] = 1").is_err());
    assert!(parse_surface(Surface::Csv, "path,kind,value\n[20000000],num,1\n").is_err());
    // small sparse indexes and ordinary dense lists still parse
    let t = parse_surface(Surface::Toon, "a[3] = 1").unwrap();
    assert_eq!(render(&t), "{\"a\":[null,null,null,1]}");
    let dense: String = (0..1000).map(|i| format!("a[{i}] = {i}\n")).collect();
    assert!(parse_surface(Surface::Toon, &dense).is_ok());
}

fn render(t: &wirefold::codec::syntax::SyntaxTerm) -> String {
    wirefold::codec::syntax::render_surface(t, Surface::Json)
}
