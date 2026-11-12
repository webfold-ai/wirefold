//! Emitters: turn a [`Schema`](super::Schema) into source for another language.
//!
//! Each emitter writes one self-contained file that reads and writes the JSON-shaped wire form
//! with that language's standard JSON support and nothing else:
//!
//! | Emitter | Output | Runtime needs |
//! |---|---|---|
//! | [`python`] | dataclasses, `Enum`s, `decode_*`/`encode_*` functions | Python 3.8+, standard library |
//! | [`typescript`] | `interface`s and union types | none (types only; `JSON.parse` is the codec) |
//! | [`go`] | structs with `json` tags and `MarshalJSON`/`UnmarshalJSON` for enums | Go standard library |
//! | [`json_schema`] | JSON Schema 2020-12 | none; feed it to quicktype, datamodel-code-generator, ... for the rest |
//!
//! `json_schema` is the escape hatch for every language without a dedicated emitter: Kotlin, Swift,
//! Java, C#, Dart and the rest all have mature generators that read it.
//!
//! The wire form is what [`to_term`](super::to_term) produces rendered as JSON, so generated code
//! and Rust agree. `tests/facet_bridge.rs` checks that against Python, TypeScript and Go when those
//! toolchains are present.

mod go;
mod json_schema;
mod python;
mod typescript;

pub use go::go;
pub use json_schema::json_schema;
pub use python::python;
pub use typescript::typescript;

use super::schema::{Field, Ty};

/// `snake_case`, `kebab-case` and `camelCase` to `PascalCase`; interior capitals are kept.
pub(crate) fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(if up { c.to_ascii_uppercase() } else { c });
            up = false;
        } else {
            up = true;
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'F');
    }
    out
}

/// Make `s` a valid identifier in a language whose reserved words are `reserved`.
pub(crate) fn safe_ident(s: &str, reserved: &[&str]) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if reserved.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// Give each name in `names` a distinct value by appending `_2`, `_3`, ... to repeats.
pub(crate) fn dedupe(names: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    names
        .into_iter()
        .map(|n| {
            let mut candidate = n.clone();
            let mut i = 2;
            while !seen.insert(candidate.clone()) {
                candidate = format!("{n}_{i}");
                i += 1;
            }
            candidate
        })
        .collect()
}

/// The field names of `fields` after `f`, deduplicated.
pub(crate) fn field_names(fields: &[Field], f: impl Fn(&str) -> String) -> Vec<String> {
    dedupe(fields.iter().map(|x| f(&x.name)).collect())
}

/// True if `ty` is a struct reference, the only payload an internally tagged newtype can carry.
pub(crate) fn is_ref(ty: &Ty) -> bool {
    matches!(ty, Ty::Ref(_))
}
