//! `wirefold_facet`: Facet <-> `SyntaxTerm`, schema extraction, and the emitters.
//!
//! The cross-language tests run Rust -> JSON -> {Python, Go, TypeScript} -> JSON -> Rust and
//! compare the result with the original value. They need the language toolchains and skip with a
//! printed notice when one is missing; set `WIREFOLD_REQUIRE_TOOLCHAINS=1` to make a missing one a
//! failure (CI does).

use facet::Facet;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use wirefold::codec::syntax::Surface;
use wirefold_facet::{
    emit, from_msgpack, from_surface, from_term, to_msgpack, to_surface, to_term, Def, EnumRepr,
    Schema, Ty, VariantKind,
};

/// A point in the plane.
#[derive(Facet, Debug, PartialEq, Clone)]
struct Point {
    /// Horizontal position.
    x: f64,
    y: f64,
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Meters(f64);

#[derive(Facet, Debug, PartialEq, Clone)]
#[repr(u8)]
enum Level {
    Low,
    #[facet(rename = "mid")]
    Medium,
    High,
}

#[derive(Facet, Debug, PartialEq, Clone)]
#[repr(u8)]
enum Shape {
    Dot,
    Circle(f64),
    Seg(Point, Point),
    Rect { w: f64, h: f64 },
}

#[derive(Facet, Debug, PartialEq, Clone)]
#[repr(u8)]
#[facet(tag = "kind")]
enum Event {
    Start,
    Move { dx: i32, dy: i32 },
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Inner {
    a: u8,
    b: String,
}

/// facet-value writes an internally tagged newtype variant but cannot read it back, so these are
/// checked one way: Rust writes, each language decodes and re-encodes, and the two JSON documents
/// must be equal.
#[derive(Facet, Debug, PartialEq, Clone)]
#[repr(u8)]
#[facet(tag = "kind")]
enum Wrapped {
    Nothing,
    Plain { n: u8 },
    Wrap(Inner),
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Outbound {
    one: Wrapped,
    many: Vec<Wrapped>,
}

fn outbound() -> Outbound {
    Outbound {
        one: Wrapped::Wrap(Inner {
            a: 1,
            b: "x".into(),
        }),
        many: vec![
            Wrapped::Nothing,
            Wrapped::Plain { n: 2 },
            Wrapped::Wrap(Inner {
                a: 3,
                b: "y".into(),
            }),
        ],
    }
}

/// Key order and number spelling differ between languages; MessagePack of the parsed term fixes both.
fn canonical(json: &str) -> Vec<u8> {
    wirefold::codec::msgpack::encode(
        &wirefold::codec::syntax::parse_surface(Surface::Json, json).unwrap(),
    )
}

#[derive(Facet, Debug, PartialEq, Clone)]
#[repr(u8)]
#[facet(tag = "t", content = "c")]
enum Msg {
    Ping,
    Text(String),
    Pair(u8, String),
    Data { id: u64 },
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Tree {
    value: i32,
    children: Vec<Tree>,
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Extra {
    note: String,
    rank: Option<u8>,
}

#[derive(Facet, Debug, PartialEq, Clone)]
struct Doc {
    id: u64,
    big: u128,
    neg: i64,
    ratio: f64,
    ok: bool,
    name: String,
    nick: Option<String>,
    raw: Vec<u8>,
    tags: Vec<String>,
    scores: BTreeMap<String, Vec<u16>>,
    pair: (u8, String),
    #[facet(rename = "renamed")]
    original: u8,
    #[facet(skip)]
    hidden: u8,
    dist: Meters,
    level: Level,
    shapes: Vec<Shape>,
    events: Vec<Event>,
    msgs: Vec<Msg>,
    tree: Tree,
    page: Page<Point>,
    #[facet(flatten)]
    extra: Extra,
}

fn sample() -> Doc {
    Doc {
        id: u64::MAX,
        big: u128::MAX,
        neg: i64::MIN,
        ratio: 0.25,
        ok: true,
        name: "wire \"fold\"\n\u{1F980}".into(),
        nick: None,
        raw: vec![0, 1, 254, 255],
        tags: vec!["a".into(), "b".into()],
        scores: BTreeMap::from([("k".into(), vec![1, 2, 3])]),
        pair: (7, "p".into()),
        original: 9,
        hidden: 0,
        dist: Meters(1.5),
        level: Level::Medium,
        shapes: vec![
            Shape::Dot,
            Shape::Circle(2.5),
            Shape::Seg(Point { x: 0.0, y: 1.0 }, Point { x: 2.0, y: 3.0 }),
            Shape::Rect { w: 4.0, h: 5.0 },
        ],
        events: vec![Event::Start, Event::Move { dx: -1, dy: 2 }],
        msgs: vec![
            Msg::Ping,
            Msg::Text("hi".into()),
            Msg::Pair(3, "q".into()),
            Msg::Data { id: 42 },
        ],
        tree: Tree {
            value: 1,
            children: vec![
                Tree {
                    value: 2,
                    children: vec![],
                },
                Tree {
                    value: 3,
                    children: vec![Tree {
                        value: 4,
                        children: vec![],
                    }],
                },
            ],
        },
        page: Page {
            items: vec![Point { x: 1.0, y: 1.0 }],
            next: Some("n".into()),
        },
        extra: Extra {
            note: "flat".into(),
            rank: Some(2),
        },
    }
}

// ---- conversion ----------------------------------------------------------------------------

#[test]
fn term_json_msgpack_round_trips() {
    let d = sample();
    assert_eq!(from_term::<Doc>(&to_term(&d).unwrap()).unwrap(), d);
    let json = to_surface(&d, Surface::Json).unwrap();
    assert_eq!(from_surface::<Doc>(Surface::Json, &json).unwrap(), d);
    // YAML only on a plain value: wirefold's YAML renderer does not yet quote strings with
    // newlines or quotes, nor write an empty list, so the rich `Doc` does not survive it.
    let p = Point { x: 1.5, y: -2.0 };
    let yaml = to_surface(&p, Surface::Yaml).unwrap();
    assert_eq!(from_surface::<Point>(Surface::Yaml, &yaml).unwrap(), p);
    let mp = to_msgpack(&d).unwrap();
    assert_eq!(from_msgpack::<Doc>(&mp).unwrap(), d);
}

#[test]
fn wire_form_matches_the_documented_shapes() {
    let json = to_surface(&sample(), Surface::Json).unwrap();
    for needle in [
        "\"renamed\":9",
        "\"Seg\":[{",
        "\"kind\":\"Move\"",
        "\"t\":\"Text\",\"c\":\"hi\"",
        "\"mid\"",
        "\"note\":\"flat\"",
        "\"340282366920938463463374607431768211455\"",
        "\"dist\":[1.5]",
    ] {
        assert!(json.contains(needle), "missing {needle} in {json}");
    }
    assert!(!json.contains("hidden"), "{json}");
}

#[test]
fn bad_input_is_an_error() {
    assert!(from_surface::<Doc>(Surface::Json, "{}").is_err());
    assert!(from_surface::<Level>(Surface::Json, "\"nope\"").is_err());
    assert!(from_surface::<Point>(Surface::Json, "{\"x\":1,\"y\":\"two\"}").is_err());
    assert!(to_term(&f64::NAN).is_err());
}

#[test]
fn hostile_nesting_does_not_overflow_the_stack() {
    let mut s = String::new();
    for _ in 0..10_000 {
        s.push('[');
    }
    for _ in 0..10_000 {
        s.push(']');
    }
    // Whatever the parser decides, the conversion must return rather than recurse without bound.
    if let Ok(t) = wirefold::codec::syntax::parse_surface(Surface::Json, &s) {
        assert!(from_term::<Vec<u8>>(&t).is_err());
    }
}

// ---- schema --------------------------------------------------------------------------------

#[test]
fn schema_describes_the_wire_form() {
    let s = Schema::of::<Doc>();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    let names: Vec<&str> = s.defs.iter().map(|d| d.name.as_str()).collect();
    for n in ["Doc", "Meters", "Level", "Shape", "Event", "Msg", "Tree"] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    assert!(names.contains(&"Doc"), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("Page")), "{names:?}");

    let Def::Struct { fields } = &s.get("Doc").unwrap().def else {
        panic!()
    };
    let by = |n: &str| {
        fields
            .iter()
            .find(|f| f.name == n)
            .unwrap_or_else(|| panic!("no field {n}"))
    };
    assert_eq!(by("big").ty, Ty::BigInt { signed: false });
    assert_eq!(
        by("renamed").ty,
        Ty::Int {
            bits: 8,
            signed: false
        }
    );
    assert_eq!(by("nick").ty, Ty::Option(Box::new(Ty::String)));
    assert!(by("nick").optional());
    assert!(fields.iter().all(|f| f.name != "hidden"));
    // flatten inlines the fields of `Extra`
    assert_eq!(by("note").ty, Ty::String);
    assert_eq!(
        by("rank").ty,
        Ty::Option(Box::new(Ty::Int {
            bits: 8,
            signed: false
        }))
    );
    assert!(fields.iter().all(|f| f.name != "extra"));
    assert_eq!(
        by("pair").ty,
        Ty::Tuple(vec![
            Ty::Int {
                bits: 8,
                signed: false
            },
            Ty::String
        ])
    );

    assert!(s.get("Level").unwrap().def.is_unit_enum());
    let Def::Enum { variants, .. } = &s.get("Level").unwrap().def else {
        panic!()
    };
    assert_eq!(variants[1].name, "mid");
    let Def::Enum { repr, variants } = &s.get("Event").unwrap().def else {
        panic!()
    };
    assert_eq!(*repr, EnumRepr::Internal { tag: "kind".into() });
    assert!(matches!(variants[1].kind, VariantKind::Struct(_)));
    let o = Schema::of::<Outbound>();
    let Def::Enum { variants, .. } = &o.get("Wrapped").unwrap().def else {
        panic!()
    };
    assert!(matches!(variants[2].kind, VariantKind::Newtype(Ty::Ref(_))));
    let Def::Enum { repr, .. } = &s.get("Msg").unwrap().def else {
        panic!()
    };
    assert_eq!(
        *repr,
        EnumRepr::Adjacent {
            tag: "t".into(),
            content: "c".into()
        }
    );
    let Def::Struct { fields } = &s.get("Tree").unwrap().def else {
        panic!()
    };
    assert_eq!(fields[1].ty, Ty::List(Box::new(Ty::Ref("Tree".into()))));
    assert!(matches!(s.get("Meters").unwrap().def, Def::Tuple { .. }));
}

#[derive(Facet)]
#[repr(u8)]
#[facet(untagged)]
#[allow(dead_code)]
enum Either {
    A(u8),
    B(String),
}

#[derive(Facet)]
#[allow(dead_code)]
struct Holder {
    e: Either,
}

#[test]
fn what_cannot_be_described_is_reported_not_guessed() {
    let s = Schema::of::<Holder>();
    assert_eq!(s.warnings.len(), 1, "{:?}", s.warnings);
    assert!(s.warnings[0].contains("untagged"));
    let Def::Struct { fields } = &s.get("Holder").unwrap().def else {
        panic!()
    };
    assert_eq!(fields[0].ty, Ty::Any);
    assert!(s.get("Either").is_none());
}

#[test]
fn same_name_different_types_do_not_collide() {
    mod a {
        use facet::Facet;
        #[derive(Facet)]
        pub struct Config {
            pub x: u8,
        }
    }
    mod b {
        use facet::Facet;
        #[derive(Facet)]
        pub struct Config {
            pub y: String,
        }
    }
    let mut s = Schema::new();
    s.add::<a::Config>();
    s.add::<b::Config>();
    assert_eq!(s.defs.len(), 2);
    assert_ne!(s.defs[0].name, s.defs[1].name);
}

#[test]
fn emitters_produce_every_definition() {
    let s = Schema::of::<Doc>();
    let py = emit::python(&s);
    let ts = emit::typescript(&s);
    let go = emit::go(&s, "bindings");
    let js = emit::json_schema(&s);
    for d in &s.defs {
        assert!(py.contains(&d.name), "python lacks {}", d.name);
        assert!(ts.contains(&d.name), "typescript lacks {}", d.name);
        assert!(go.contains(&d.name), "go lacks {}", d.name);
        assert!(js.contains(&d.name), "json schema lacks {}", d.name);
    }
    assert!(ts.contains("/** A point in the plane. */"));
    assert!(js.contains("\"$schema\""));
    assert!(js.contains("\"$ref\":\"#/$defs/Doc\""));
    // The JSON Schema is itself valid JSON.
    wirefold::codec::syntax::parse_surface(Surface::Json, &js).unwrap();
}

// ---- cross-language ------------------------------------------------------------------------

fn tool(name: &str, arg: &str) -> bool {
    Command::new(name)
        .arg(arg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn have(name: &str, arg: &str) -> bool {
    if tool(name, arg) {
        return true;
    }
    assert!(
        std::env::var_os("WIREFOLD_REQUIRE_TOOLCHAINS").is_none(),
        "{name} is required (WIREFOLD_REQUIRE_TOOLCHAINS is set) but not found"
    );
    eprintln!("skipping: {name} not found");
    false
}

fn workdir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("facet_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed\n--- stdout\n{}\n--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn python_round_trips_the_wire_form() {
    if !have("python3", "--version") {
        return;
    }
    let dir = workdir("py");
    let mut schema = Schema::of::<Doc>();
    schema.add::<Outbound>();
    std::fs::write(dir.join("bindings.py"), emit::python(&schema)).unwrap();
    let d = sample();
    std::fs::write(dir.join("in.json"), to_surface(&d, Surface::Json).unwrap()).unwrap();
    let ob = to_surface(&outbound(), Surface::Json).unwrap();
    std::fs::write(dir.join("out_in.json"), &ob).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "import json, sys\nimport bindings as b\nsrc = open('in.json').read()\nv = b.Doc.from_json(src)\nassert isinstance(v.shapes[2], b.ShapeSeg), v.shapes[2]\nassert v.level == b.Level.mid, v.level\nopen('out.json', 'w').write(v.to_json())\no = b.Outbound.from_json(open('out_in.json').read())\nassert isinstance(o.one, b.WrappedWrap), o.one\nopen('out_out.json', 'w').write(o.to_json())\n",
    )
    .unwrap();
    run(Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg("import sys; sys.path.insert(0, '.'); exec(open('main.py').read())")
        .current_dir(&dir));
    let back = std::fs::read_to_string(dir.join("out.json")).unwrap();
    assert_eq!(from_surface::<Doc>(Surface::Json, &back).unwrap(), d);
    let back = std::fs::read_to_string(dir.join("out_out.json")).unwrap();
    assert_eq!(canonical(&back), canonical(&ob));
}

#[test]
fn go_round_trips_the_wire_form() {
    if !have("go", "version") {
        return;
    }
    let dir = workdir("go");
    let mut schema = Schema::of::<Doc>();
    schema.add::<Outbound>();
    std::fs::write(dir.join("go.mod"), "module wf\n\ngo 1.21\n").unwrap();
    std::fs::create_dir_all(dir.join("bindings")).unwrap();
    std::fs::write(
        dir.join("bindings/bindings.go"),
        emit::go(&schema, "bindings"),
    )
    .unwrap();
    let d = sample();
    std::fs::write(dir.join("in.json"), to_surface(&d, Surface::Json).unwrap()).unwrap();
    let ob = to_surface(&outbound(), Surface::Json).unwrap();
    std::fs::write(dir.join("out_in.json"), &ob).unwrap();
    std::fs::write(
        dir.join("main.go"),
        "package main\n\nimport (\n\t\"encoding/json\"\n\t\"os\"\n\n\t\"wf/bindings\"\n)\n\nfunc main() {\n\tb, err := os.ReadFile(\"in.json\")\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tvar d bindings.Doc\n\tif err := json.Unmarshal(b, &d); err != nil {\n\t\tpanic(err)\n\t}\n\tif d.Level != bindings.LevelMid {\n\t\tpanic(\"level\")\n\t}\n\tif d.Shapes[2].Seg == nil {\n\t\tpanic(\"seg\")\n\t}\n\tout, err := json.Marshal(d)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tif err := os.WriteFile(\"out.json\", out, 0o644); err != nil {\n\t\tpanic(err)\n\t}\n\tb2, err := os.ReadFile(\"out_in.json\")\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tvar o bindings.Outbound\n\tif err := json.Unmarshal(b2, &o); err != nil {\n\t\tpanic(err)\n\t}\n\tif o.One.Wrap == nil {\n\t\tpanic(\"wrap\")\n\t}\n\tout2, err := json.Marshal(o)\n\tif err != nil {\n\t\tpanic(err)\n\t}\n\tif err := os.WriteFile(\"out_out.json\", out2, 0o644); err != nil {\n\t\tpanic(err)\n\t}\n}\n",
    )
    .unwrap();
    run(Command::new("go")
        .args(["vet", "./..."])
        .current_dir(&dir)
        .env("GOFLAGS", "-mod=mod"));
    run(Command::new("go")
        .args(["run", "."])
        .current_dir(&dir)
        .env("GOFLAGS", "-mod=mod"));
    let back = std::fs::read_to_string(dir.join("out.json")).unwrap();
    assert_eq!(from_surface::<Doc>(Surface::Json, &back).unwrap(), d);
    let back = std::fs::read_to_string(dir.join("out_out.json")).unwrap();
    assert_eq!(canonical(&back), canonical(&ob));
}

#[test]
fn typescript_types_accept_the_wire_form() {
    if !have("tsc", "--version") {
        return;
    }
    let dir = workdir("ts");
    let mut schema = Schema::of::<Doc>();
    schema.add::<Outbound>();
    std::fs::write(dir.join("bindings.ts"), emit::typescript(&schema)).unwrap();
    let json = to_surface(&sample(), Surface::Json).unwrap();
    // The compiler checks the literal against the generated type.
    std::fs::write(
        dir.join("check.ts"),
        format!(
            "import type {{ Doc, Outbound }} from \"./bindings\";\nexport const sample: Doc = {json};\nexport const outbound: Outbound = {};\n",
            to_surface(&outbound(), Surface::Json).unwrap()
        ),
    )
    .unwrap();
    run(Command::new("tsc")
        .args([
            "--noEmit",
            "--strict",
            "--target",
            "es2020",
            "--module",
            "es2020",
            "--moduleResolution",
            "bundler",
            "check.ts",
        ])
        .current_dir(&dir));
}
