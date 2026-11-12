//! JSON Schema (draft 2020-12) emitter.
//!
//! The universal exit: every language without a dedicated emitter has a generator that reads JSON
//! Schema (quicktype, datamodel-code-generator, openapi-generator, ...).

use crate::schema::{Def, EnumRepr, Field, Named, Schema, Ty, Variant, VariantKind};
use wirefold::codec::syntax::{render_surface, Atom, Surface, SyntaxTerm};

type Obj = Vec<(String, SyntaxTerm)>;

fn s(v: &str) -> SyntaxTerm {
    SyntaxTerm::B(Atom::Str(v.to_owned()))
}
fn n(v: impl ToString) -> SyntaxTerm {
    SyntaxTerm::B(Atom::Num(v.to_string()))
}
fn b(v: bool) -> SyntaxTerm {
    SyntaxTerm::B(Atom::Bool(v))
}
fn o(pairs: Vec<(&str, SyntaxTerm)>) -> SyntaxTerm {
    SyntaxTerm::Bu(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
fn arr(items: Vec<SyntaxTerm>) -> SyntaxTerm {
    SyntaxTerm::Fourth(items)
}

/// Emit a JSON Schema document. The first root added to `schema` is the document's own schema;
/// every definition is under `$defs`.
#[must_use]
pub fn json_schema(schema: &Schema) -> String {
    let mut top: Obj = vec![(
        "$schema".into(),
        s("https://json-schema.org/draft/2020-12/schema"),
    )];
    if let Some(root) = schema.roots.first() {
        if let SyntaxTerm::Bu(fields) = ty(root) {
            top.extend(fields);
        }
    }
    let defs: Obj = schema
        .defs
        .iter()
        .map(|d| (d.name.clone(), named(d)))
        .collect();
    top.push(("$defs".into(), SyntaxTerm::Bu(defs)));
    render_surface(&SyntaxTerm::Bu(top), Surface::Json)
}

fn int_range(bits: u8, signed: bool) -> (String, String) {
    if signed {
        let max = (1u128 << (bits - 1)) - 1;
        (format!("-{}", max + 1), max.to_string())
    } else {
        ("0".into(), ((1u128 << bits) - 1).to_string())
    }
}

fn ty(t: &Ty) -> SyntaxTerm {
    match t {
        Ty::Null => o(vec![("type", s("null"))]),
        Ty::Bool => o(vec![("type", s("boolean"))]),
        Ty::Int { bits, signed } => {
            let (lo, hi) = int_range(*bits, *signed);
            o(vec![
                ("type", s("integer")),
                ("minimum", n(lo)),
                ("maximum", n(hi)),
            ])
        }
        Ty::BigInt { signed } => o(vec![
            ("type", s("string")),
            (
                "pattern",
                s(if *signed { "^-?[0-9]+$" } else { "^[0-9]+$" }),
            ),
        ]),
        Ty::Float { .. } => o(vec![("type", s("number"))]),
        Ty::String => o(vec![("type", s("string"))]),
        Ty::Option(i) => o(vec![(
            "anyOf",
            arr(vec![ty(i), o(vec![("type", s("null"))])]),
        )]),
        Ty::List(i) => o(vec![("type", s("array")), ("items", ty(i))]),
        Ty::Map(i) => o(vec![("type", s("object")), ("additionalProperties", ty(i))]),
        Ty::Tuple(items) => tuple(items),
        Ty::Ref(name) => o(vec![("$ref", s(&format!("#/$defs/{name}")))]),
        Ty::Any => SyntaxTerm::Bu(vec![]),
    }
}

fn tuple(items: &[Ty]) -> SyntaxTerm {
    o(vec![
        ("type", s("array")),
        ("prefixItems", arr(items.iter().map(ty).collect())),
        ("minItems", n(items.len())),
        ("maxItems", n(items.len())),
    ])
}

/// `properties` and `required` for a list of fields, plus any extra leading properties.
fn object(fields: &[Field], lead: Vec<(String, SyntaxTerm)>, lead_required: &[&str]) -> Obj {
    let mut props: Obj = lead;
    let mut required: Vec<SyntaxTerm> = lead_required.iter().map(|r| s(r)).collect();
    for f in fields {
        let mut schema = ty(&f.ty);
        if !f.doc.is_empty() {
            if let SyntaxTerm::Bu(items) = &mut schema {
                items.push(("description".into(), s(&f.doc.join(" "))));
            }
        }
        props.push((f.name.clone(), schema));
        if !f.optional() {
            required.push(s(&f.name));
        }
    }
    let mut out: Obj = vec![
        ("type".into(), s("object")),
        ("properties".into(), SyntaxTerm::Bu(props)),
    ];
    if !required.is_empty() {
        out.push(("required".into(), arr(required)));
    }
    out
}

fn variant(repr: &EnumRepr, v: &Variant) -> SyntaxTerm {
    let one = |name: &str, payload: SyntaxTerm| {
        o(vec![
            ("type", s("object")),
            (
                "properties",
                SyntaxTerm::Bu(vec![(name.to_owned(), payload)]),
            ),
            ("required", arr(vec![s(name)])),
            ("additionalProperties", b(false)),
        ])
    };
    let konst = |x: &str| o(vec![("const", s(x))]);
    let payload = |k: &VariantKind| match k {
        VariantKind::Unit => o(vec![("type", s("null"))]),
        VariantKind::Newtype(t) => ty(t),
        VariantKind::Tuple(items) => tuple(items),
        VariantKind::Struct(fields) => SyntaxTerm::Bu(object(fields, vec![], &[])),
    };
    let mut out = match (repr, &v.kind) {
        (EnumRepr::External, VariantKind::Unit) => konst(&v.name),
        (EnumRepr::External, k) => one(&v.name, payload(k)),
        (EnumRepr::Internal { tag }, VariantKind::Struct(fields)) => {
            SyntaxTerm::Bu(object(fields, vec![(tag.clone(), konst(&v.name))], &[tag]))
        }
        (EnumRepr::Internal { tag }, VariantKind::Newtype(t)) => o(vec![(
            "allOf",
            arr(vec![
                ty(t),
                SyntaxTerm::Bu(object(&[], vec![(tag.clone(), konst(&v.name))], &[tag])),
            ]),
        )]),
        (EnumRepr::Internal { tag }, _) => {
            SyntaxTerm::Bu(object(&[], vec![(tag.clone(), konst(&v.name))], &[tag]))
        }
        (EnumRepr::Adjacent { tag, content }, k) => {
            let mut props = vec![(tag.clone(), konst(&v.name))];
            let mut required = vec![s(tag)];
            if *k != VariantKind::Unit {
                props.push((content.clone(), payload(k)));
                required.push(s(content));
            }
            o(vec![
                ("type", s("object")),
                ("properties", SyntaxTerm::Bu(props)),
                ("required", arr(required)),
            ])
        }
    };
    if !v.doc.is_empty() {
        if let SyntaxTerm::Bu(items) = &mut out {
            items.push(("description".into(), s(&v.doc.join(" "))));
        }
    }
    out
}

fn named(d: &Named) -> SyntaxTerm {
    let mut out: Obj = match &d.def {
        Def::Struct { fields } => object(fields, vec![], &[]),
        Def::Tuple { items } => match tuple(items) {
            SyntaxTerm::Bu(x) => x,
            _ => vec![],
        },
        Def::Enum { repr, variants } => {
            if d.def.is_unit_enum() {
                vec![
                    ("type".into(), s("string")),
                    (
                        "enum".into(),
                        arr(variants.iter().map(|v| s(&v.name)).collect()),
                    ),
                ]
            } else {
                vec![(
                    "oneOf".into(),
                    arr(variants.iter().map(|v| variant(repr, v)).collect()),
                )]
            }
        }
    };
    if !d.doc.is_empty() {
        out.push(("description".into(), s(&d.doc.join(" "))));
    }
    SyntaxTerm::Bu(out)
}
