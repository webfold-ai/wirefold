//! Facet integration for [wirefold](https://github.com/webfold-ai/wirefold): reflect any
//! [`Facet`] type to and from wirefold's universal
//! [`SyntaxTerm`], and extract a language-neutral [`Schema`] from its `Shape` so the same type can be
//! used from Python, TypeScript, Go and anything that reads JSON Schema.
//!
//! This is the counterpart of `wirefold::serde_bridge` for code that derives
//! `#[derive(Facet)]` instead of serde. Facet is reflection, not code generation: a type carries a
//! static `Shape` describing its fields, variants and attributes, and this module reads it.
//!
//! ```
//! use facet::Facet;
//! use wirefold::codec::syntax::Surface;
//!
//! #[derive(Facet, Debug, PartialEq)]
//! struct Job { id: u32, tags: Vec<String> }
//!
//! let job = Job { id: 7, tags: vec!["a".into()] };
//! let json = wirefold_facet::to_surface(&job, Surface::Json)?;
//! let back: Job = wirefold_facet::from_surface(Surface::Json, &json)?;
//! assert_eq!(job, back);
//!
//! // The same type, described for other languages.
//! let schema = wirefold_facet::Schema::of::<Job>();
//! let python = wirefold_facet::emit::python(&schema);
//! assert!(python.contains("class Job"));
//! # Ok::<(), wirefold_facet::Error>(())
//! ```
//!
//! ## Two deliverables, kept apart
//!
//! * **Conversion** ([`to_term`], [`from_term`], ...) runs in Rust and reuses facet's own
//!   serialization rules, so renames, `skip`, enum tagging and the rest behave as they do for
//!   facet's other formats.
//! * **Schema and emitters** ([`Schema`], [`emit`]) describe the *JSON-shaped wire form* of the
//!   type. The generated Python/TypeScript/Go code reads and writes that form with each language's
//!   native JSON support; it does not reimplement wirefold's binary codecs.
//!
//! What a schema cannot describe (opaque types, `#[facet(untagged)]` enums, trait objects) becomes
//! `Any` and is listed in [`Schema::warnings`] instead of being guessed at.

mod adapt;
pub mod emit;
pub mod schema;

pub use schema::{Def, EnumRepr, Field, Schema, Ty, Variant, VariantKind};

use ::facet::Facet;
use facet_value::{VArray, VNumber, VObject, VString, Value};
use std::fmt;
use wirefold::codec::syntax::{
    parse_surface, render_surface, Atom, Surface, SyntaxError, SyntaxTerm,
};

/// Why a value could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    msg: String,
}

impl Error {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<SyntaxError> for Error {
    fn from(e: SyntaxError) -> Self {
        Self::new(e.msg)
    }
}

/// Convert any `Facet` value to a [`SyntaxTerm`].
///
/// The mapping follows facet's own serialization: structs and maps become `Bu`, lists and tuples
/// `Fourth`, `None` and `()` `Nb`, enums are externally tagged unless the type says otherwise.
/// 128-bit integers become decimal text, as in facet's other formats.
pub fn to_term<'f, T: Facet<'f>>(value: &T) -> Result<SyntaxTerm, Error> {
    let v = facet_value::to_value(value).map_err(|e| Error::new(format!("{e:?}")))?;
    value_to_term(&v)
}

/// Convert a [`SyntaxTerm`] to any owned `Facet` type.
pub fn from_term<T: Facet<'static>>(term: &SyntaxTerm) -> Result<T, Error> {
    let v = term_to_value(&adapt::adapt(term, T::SHAPE)?)?;
    facet_value::from_value(v).map_err(|e| Error::new(e.to_string()))
}

/// Serialize a value and render it in `surface` (JSON, YAML, TOON, POML, ...).
pub fn to_surface<'f, T: Facet<'f>>(value: &T, surface: Surface) -> Result<String, Error> {
    Ok(render_surface(&to_term(value)?, surface))
}

/// Parse `input` as `surface` and deserialize it.
pub fn from_surface<T: Facet<'static>>(surface: Surface, input: &str) -> Result<T, Error> {
    from_term(&parse_surface(surface, input)?)
}

/// Serialize a value to MessagePack.
pub fn to_msgpack<'f, T: Facet<'f>>(value: &T) -> Result<Vec<u8>, Error> {
    Ok(wirefold::codec::msgpack::encode(&to_term(value)?))
}

/// Deserialize a value from MessagePack.
pub fn from_msgpack<T: Facet<'static>>(bytes: &[u8]) -> Result<T, Error> {
    from_term(&wirefold::codec::msgpack::decode(bytes)?)
}

/// Nesting limit for the conversions below, so hostile input cannot overflow the stack.
const MAX_DEPTH: usize = 256;

fn number_text(n: &VNumber) -> String {
    if let Some(i) = n.to_i64() {
        i.to_string()
    } else if let Some(u) = n.to_u64() {
        u.to_string()
    } else {
        // `{:?}` keeps a `.0` so a float stays a float through the text surfaces.
        format!("{:?}", n.to_f64_lossy())
    }
}

fn value_to_term(v: &Value) -> Result<SyntaxTerm, Error> {
    fn go(v: &Value, depth: usize) -> Result<SyntaxTerm, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::new("value nests too deeply"));
        }
        use facet_value::ValueType as T;
        Ok(match v.value_type() {
            T::Null => SyntaxTerm::Nb,
            T::Bool => SyntaxTerm::B(Atom::Bool(v.is_true())),
            T::Number => match v.as_number() {
                Some(n) if n.is_float() && !n.to_f64_lossy().is_finite() => {
                    return Err(Error::new(
                        "non-finite float has no wirefold representation",
                    ))
                }
                Some(n) => SyntaxTerm::B(Atom::Num(number_text(n))),
                None => return Err(Error::new("malformed number")),
            },
            T::String => SyntaxTerm::B(Atom::Str(
                v.as_string()
                    .map(|s| s.as_str().to_owned())
                    .unwrap_or_default(),
            )),
            T::Bytes => SyntaxTerm::Fourth(
                v.as_bytes()
                    .map(|b| {
                        b.as_slice()
                            .iter()
                            .map(|x| SyntaxTerm::B(Atom::Num(x.to_string())))
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            T::Array => {
                let mut out = Vec::new();
                if let Some(a) = v.as_array() {
                    for item in a.as_slice() {
                        out.push(go(item, depth + 1)?);
                    }
                }
                SyntaxTerm::Fourth(out)
            }
            T::Object => {
                let mut out = Vec::new();
                if let Some(o) = v.as_object() {
                    for (k, item) in o.iter() {
                        out.push((k.as_str().to_owned(), go(item, depth + 1)?));
                    }
                }
                SyntaxTerm::Bu(out)
            }
            other => {
                return Err(Error::new(format!(
                    "{other:?} values have no SyntaxTerm form"
                )))
            }
        })
    }
    go(v, 0)
}

fn term_to_value(t: &SyntaxTerm) -> Result<Value, Error> {
    fn go(t: &SyntaxTerm, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::new("term nests too deeply"));
        }
        Ok(match t {
            SyntaxTerm::Nb => Value::NULL,
            SyntaxTerm::Unknown => return Err(Error::new("an unknown value cannot be typed")),
            SyntaxTerm::B(Atom::Bool(b)) => Value::from(*b),
            SyntaxTerm::B(Atom::Num(s)) => {
                if let Ok(i) = s.parse::<i64>() {
                    VNumber::from_i64(i).into_value()
                } else if let Ok(u) = s.parse::<u64>() {
                    VNumber::from_u64(u).into_value()
                } else if let Some(f) = s.parse::<f64>().ok().filter(|f| f.is_finite()) {
                    VNumber::from_f64(f).into_value()
                } else {
                    return Err(Error::new(format!("invalid number {s:?}")));
                }
            }
            SyntaxTerm::B(Atom::Str(s)) => VString::new(s).into(),
            SyntaxTerm::Fourth(items) => {
                let mut a = VArray::with_capacity(items.len());
                for item in items {
                    a.push(go(item, depth + 1)?);
                }
                a.into_value()
            }
            SyntaxTerm::Bu(fields) => {
                let mut o = VObject::with_capacity(fields.len());
                for (k, item) in fields {
                    o.insert(VString::new(k), go(item, depth + 1)?);
                }
                o.into_value()
            }
        })
    }
    go(t, 0)
}
