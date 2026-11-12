//! Shape-guided rewrite of a term before `facet_value::from_value` reads it.
//!
//! facet-value writes a tuple struct (`struct Meters(f64)`) as an array but reads one only from an
//! object keyed by field position (`{"0": 1.5}`), so a value does not survive its own round trip.
//! This pass walks the term alongside the target type's `Shape` and rewrites exactly those arrays;
//! everything else passes through untouched. A type that cannot be matched is left as it is, so the
//! worst case is the error facet-value would have given anyway.

use crate::Error;
use ::facet::{Def, Field, FieldFlags, Shape, StructKind, StructType, Type, UserType};
use wirefold::codec::syntax::{Atom, SyntaxTerm};

const MAX_DEPTH: usize = 256;

pub(super) fn adapt(t: &SyntaxTerm, shape: &'static Shape) -> Result<SyntaxTerm, Error> {
    go(t, shape, 0)
}

fn deep() -> Error {
    Error::new("term nests too deeply")
}

fn go(t: &SyntaxTerm, shape: &'static Shape, depth: usize) -> Result<SyntaxTerm, Error> {
    if depth > MAX_DEPTH {
        return Err(deep());
    }
    // Same order as the schema: containers, then a transparent wrapper's inner shape.
    match shape.def {
        Def::Option(o) => {
            return match t {
                SyntaxTerm::Nb => Ok(SyntaxTerm::Nb),
                _ => go(t, o.t(), depth + 1),
            }
        }
        Def::List(l) => return items(t, l.t(), depth),
        Def::Array(a) => return items(t, a.t(), depth),
        Def::Slice(s) => return items(t, s.t(), depth),
        Def::Set(s) => return items(t, s.t(), depth),
        Def::Map(m) => {
            return match t {
                SyntaxTerm::Bu(fields) => fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), go(v, m.v(), depth + 1)?)))
                    .collect::<Result<Vec<_>, Error>>()
                    .map(SyntaxTerm::Bu),
                _ => Ok(t.clone()),
            }
        }
        Def::Pointer(p) => {
            return match p.pointee() {
                Some(inner) => go(t, inner, depth + 1),
                None => Ok(t.clone()),
            }
        }
        _ => {}
    }
    if let Some(inner) = shape.inner {
        return go(t, inner, depth + 1);
    }
    match shape.ty {
        Type::User(UserType::Struct(st)) => strukt(t, st, depth),
        Type::User(UserType::Enum(et)) => {
            if shape.is_untagged() {
                return Ok(t.clone());
            }
            enum_(t, et.variants, shape, depth)
        }
        _ => Ok(t.clone()),
    }
}

fn items(t: &SyntaxTerm, elem: &'static Shape, depth: usize) -> Result<SyntaxTerm, Error> {
    match t {
        SyntaxTerm::Fourth(xs) => xs
            .iter()
            .map(|x| go(x, elem, depth + 1))
            .collect::<Result<Vec<_>, Error>>()
            .map(SyntaxTerm::Fourth),
        _ => Ok(t.clone()),
    }
}

/// The shape of the field a key addresses, looking through flattened structs.
fn shape_for_key(st: &StructType, key: &str, depth: usize) -> Option<&'static Shape> {
    if depth > 8 {
        return None;
    }
    for f in st.fields {
        if f.flags.contains(FieldFlags::FLATTEN) {
            let mut inner = f.shape.get();
            if let Def::Option(o) = inner.def {
                inner = o.t();
            }
            if let Type::User(UserType::Struct(s)) = inner.ty {
                if let Some(found) = shape_for_key(&s, key, depth + 1) {
                    return Some(found);
                }
            }
        } else if f.effective_name() == key || f.alias == Some(key) {
            return Some(f.shape.get());
        }
    }
    None
}

fn named_fields(
    fields: &[(String, SyntaxTerm)],
    st: &StructType,
    skip: Option<&str>,
    depth: usize,
) -> Result<Vec<(String, SyntaxTerm)>, Error> {
    fields
        .iter()
        .map(|(k, v)| match shape_for_key(st, k, 0) {
            Some(s) if Some(k.as_str()) != skip => Ok((k.clone(), go(v, s, depth + 1)?)),
            _ => Ok((k.clone(), v.clone())),
        })
        .collect()
}

fn positional(xs: &[SyntaxTerm], fields: &[Field], depth: usize) -> Result<Vec<SyntaxTerm>, Error> {
    xs.iter()
        .zip(fields)
        .map(|(x, f)| go(x, f.shape.get(), depth + 1))
        .collect()
}

fn strukt(t: &SyntaxTerm, st: StructType, depth: usize) -> Result<SyntaxTerm, Error> {
    match (st.kind, t) {
        (StructKind::Struct, SyntaxTerm::Bu(fields)) => {
            named_fields(fields, &st, None, depth).map(SyntaxTerm::Bu)
        }
        // The rewrite this module exists for.
        (StructKind::TupleStruct, SyntaxTerm::Fourth(xs)) if xs.len() == st.fields.len() => {
            let xs = positional(xs, st.fields, depth)?;
            Ok(SyntaxTerm::Bu(
                st.fields
                    .iter()
                    .zip(xs)
                    .map(|(f, x)| (f.effective_name().to_owned(), x))
                    .collect(),
            ))
        }
        (StructKind::Tuple, SyntaxTerm::Fourth(xs)) if xs.len() == st.fields.len() => {
            positional(xs, st.fields, depth).map(SyntaxTerm::Fourth)
        }
        _ => Ok(t.clone()),
    }
}

/// The payload of one variant, in whatever form the enum's tagging puts it.
fn payload(t: &SyntaxTerm, variant: &::facet::Variant, depth: usize) -> Result<SyntaxTerm, Error> {
    let data = variant.data;
    match (data.kind, data.fields.len(), t) {
        (StructKind::Struct, _, SyntaxTerm::Bu(fields)) => {
            named_fields(fields, &data, None, depth).map(SyntaxTerm::Bu)
        }
        (StructKind::Unit, _, _) | (_, 0, _) => Ok(t.clone()),
        (_, 1, _) => go(t, data.fields[0].shape.get(), depth + 1),
        (_, n, SyntaxTerm::Fourth(xs)) if xs.len() == n => {
            positional(xs, data.fields, depth).map(SyntaxTerm::Fourth)
        }
        _ => Ok(t.clone()),
    }
}

fn enum_(
    t: &SyntaxTerm,
    variants: &'static [::facet::Variant],
    shape: &'static Shape,
    depth: usize,
) -> Result<SyntaxTerm, Error> {
    let find = |name: &str| variants.iter().find(|v| v.effective_name() == name);
    let SyntaxTerm::Bu(fields) = t else {
        return Ok(t.clone());
    };
    match (shape.get_tag_attr(), shape.get_content_attr()) {
        (Some(tag), content) => {
            let name = fields.iter().find_map(|(k, v)| match v {
                SyntaxTerm::B(Atom::Str(s)) if k == tag => Some(s.as_str()),
                _ => None,
            });
            let Some(variant) = name.and_then(find) else {
                return Ok(t.clone());
            };
            match content {
                // Adjacently tagged: only the content is typed by the variant.
                Some(content) => Ok(SyntaxTerm::Bu(
                    fields
                        .iter()
                        .map(|(k, v)| {
                            if k == content {
                                Ok((k.clone(), payload(v, variant, depth)?))
                            } else {
                                Ok((k.clone(), v.clone()))
                            }
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                )),
                // Internally tagged: the variant's fields sit beside the tag.
                None => match (variant.data.kind, variant.data.fields.len()) {
                    (StructKind::Struct, _) => {
                        named_fields(fields, &variant.data, Some(tag), depth).map(SyntaxTerm::Bu)
                    }
                    (_, 1) => go(t, variant.data.fields[0].shape.get(), depth + 1),
                    _ => Ok(t.clone()),
                },
            }
        }
        (None, _) => match fields.as_slice() {
            [(k, v)] => match find(k) {
                Some(variant) => Ok(SyntaxTerm::Bu(vec![(
                    k.clone(),
                    payload(v, variant, depth)?,
                )])),
                None => Ok(t.clone()),
            },
            _ => Ok(t.clone()),
        },
    }
}
