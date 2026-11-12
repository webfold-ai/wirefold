//! A language-neutral description of a type's JSON-shaped wire form, extracted from facet's `Shape`.
//!
//! [`Schema`] is a small registry of named definitions (structs, tuple structs, enums) plus the
//! root types you asked for. It is the input of every emitter in [`emit`](super::emit), and it is a
//! plain data structure on purpose: a build script can serialise it, a test can compare it, and
//! another language backend can be written against it without touching facet.

use ::facet::{
    Def as FacetDef, Facet, Field as FacetField, FieldFlags, ScalarType, Shape, StructKind, Type,
    UserType,
};
use std::any::TypeId;
use std::collections::{HashMap, HashSet};

/// A type as it appears on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    /// `()`: JSON `null`.
    Null,
    /// `bool`.
    Bool,
    /// An integer that fits in 64 bits. `usize`/`isize` count as 64.
    Int {
        /// Width in bits: 8, 16, 32 or 64.
        bits: u8,
        /// Whether the type is signed.
        signed: bool,
    },
    /// A 128-bit integer: decimal text on the wire, because JSON numbers cannot carry it.
    BigInt {
        /// Whether the type is signed.
        signed: bool,
    },
    /// `f32` or `f64`.
    Float {
        /// 32 or 64.
        bits: u8,
    },
    /// Text. Also used for scalars facet serialises as text (UUIDs, addresses, URLs, ...).
    String,
    /// `Option<T>`: `null` or a `T`.
    Option(Box<Ty>),
    /// `Vec<T>`, arrays, slices and sets: a JSON array.
    List(Box<Ty>),
    /// A map with string keys: a JSON object.
    Map(Box<Ty>),
    /// A tuple: a fixed-length JSON array whose items have their own types.
    Tuple(Vec<Ty>),
    /// A named definition in [`Schema::defs`].
    Ref(String),
    /// Anything the schema cannot describe; see [`Schema::warnings`].
    Any,
}

/// One field of a struct or struct variant.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The name on the wire (after `rename`).
    pub name: String,
    /// The field's type.
    pub ty: Ty,
    /// Doc comment lines.
    pub doc: Vec<String>,
}

impl Field {
    /// `Option<T>` fields may be `null` and may be absent.
    #[must_use]
    pub fn optional(&self) -> bool {
        matches!(self.ty, Ty::Option(_))
    }
}

/// How an enum names its variant on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnumRepr {
    /// The default: `"Unit"` or `{"Variant": payload}`.
    External,
    /// `#[facet(tag = "kind")]`: `{"kind": "Variant", ...fields}`. Variants carry fields or nothing.
    Internal {
        /// The tag field's name.
        tag: String,
    },
    /// `#[facet(tag = "t", content = "c")]`: `{"t": "Variant", "c": payload}`.
    Adjacent {
        /// The tag field's name.
        tag: String,
        /// The payload field's name.
        content: String,
    },
}

/// What an enum variant carries.
#[derive(Clone, Debug, PartialEq)]
pub enum VariantKind {
    /// No payload.
    Unit,
    /// One unnamed field.
    Newtype(Ty),
    /// Several unnamed fields: an array.
    Tuple(Vec<Ty>),
    /// Named fields: an object.
    Struct(Vec<Field>),
}

/// One variant of an enum.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    /// The name on the wire (after `rename`).
    pub name: String,
    /// What the variant carries.
    pub kind: VariantKind,
    /// Doc comment lines.
    pub doc: Vec<String>,
}

/// The body of a named definition.
#[derive(Clone, Debug, PartialEq)]
pub enum Def {
    /// A struct with named fields: a JSON object.
    Struct {
        /// The fields, in declaration order, after `skip` and `flatten`.
        fields: Vec<Field>,
    },
    /// A tuple struct (including a one-field newtype that is not `#[facet(transparent)]`): a JSON array.
    Tuple {
        /// The item types.
        items: Vec<Ty>,
    },
    /// An enum.
    Enum {
        /// How variants are named on the wire.
        repr: EnumRepr,
        /// The variants, in declaration order.
        variants: Vec<Variant>,
    },
}

impl Def {
    /// True for an enum whose variants all carry nothing: a plain string enumeration.
    #[must_use]
    pub fn is_unit_enum(&self) -> bool {
        matches!(self, Def::Enum { repr: EnumRepr::External, variants }
            if variants.iter().all(|v| v.kind == VariantKind::Unit))
    }
}

/// A named definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    /// A name unique within the schema, safe to use as an identifier in most languages.
    pub name: String,
    /// Doc comment lines.
    pub doc: Vec<String>,
    /// The definition.
    pub def: Def,
}

/// A registry of definitions and the root types that were added to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    /// The types added with [`add`](Self::add), in order.
    pub roots: Vec<Ty>,
    /// Every named definition reachable from the roots, in discovery order (a type before the
    /// types it refers to, except through recursion).
    pub defs: Vec<Named>,
    /// Things that could not be described and were replaced by [`Ty::Any`].
    pub warnings: Vec<String>,
    ids: HashMap<TypeId, String>,
    names: HashSet<String>,
}

impl Schema {
    /// An empty schema.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The schema of one type.
    #[must_use]
    pub fn of<'f, T: Facet<'f>>() -> Self {
        let mut s = Self::new();
        s.add::<T>();
        s
    }

    /// Add a type and everything it refers to. Returns the type's wire description.
    pub fn add<'f, T: Facet<'f>>(&mut self) -> Ty {
        self.add_shape(T::SHAPE)
    }

    /// Add a type by its shape.
    pub fn add_shape(&mut self, shape: &'static Shape) -> Ty {
        let ty = self.ty(shape, 0);
        self.roots.push(ty.clone());
        ty
    }

    /// The definition called `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Named> {
        self.defs.iter().find(|d| d.name == name)
    }

    fn warn(&mut self, msg: String) -> Ty {
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
        Ty::Any
    }

    fn ty(&mut self, shape: &'static Shape, depth: usize) -> Ty {
        // A recursive type is cut at its name, so depth only guards against shapes that nest
        // without ever naming a type (a deeply nested generic container).
        if depth > 64 {
            return self.warn(format!("{}: nests too deeply", shape.type_identifier));
        }
        // Containers first: `Option<T>` and `Vec<T>` also carry an `inner` shape, and following
        // it would drop the container.
        match shape.def {
            FacetDef::Option(o) => return Ty::Option(Box::new(self.ty(o.t(), depth + 1))),
            FacetDef::List(l) => return Ty::List(Box::new(self.ty(l.t(), depth + 1))),
            FacetDef::Array(a) => return Ty::List(Box::new(self.ty(a.t(), depth + 1))),
            FacetDef::Slice(s) => return Ty::List(Box::new(self.ty(s.t(), depth + 1))),
            FacetDef::Set(s) => return Ty::List(Box::new(self.ty(s.t(), depth + 1))),
            FacetDef::Map(m) => return Ty::Map(Box::new(self.ty(m.v(), depth + 1))),
            FacetDef::Pointer(p) => {
                return match p.pointee() {
                    Some(t) => self.ty(t, depth + 1),
                    None => self.warn(format!(
                        "{}: pointer without a pointee",
                        shape.type_identifier
                    )),
                }
            }
            FacetDef::Result(_) => {
                return self.warn(format!(
                    "{}: Result has no schema form yet",
                    shape.type_identifier
                ))
            }
            FacetDef::DynamicValue(_) => return Ty::Any,
            FacetDef::NdArray(_) => {
                return self.warn(format!(
                    "{}: n-dimensional arrays have no schema form",
                    shape.type_identifier
                ))
            }
            _ => {}
        }
        if let Some(inner) = shape.inner {
            return self.ty(inner, depth + 1);
        }
        if let Some(s) = ScalarType::try_from_shape(shape) {
            return scalar(s);
        }
        match shape.ty {
            Type::User(UserType::Struct(st)) => match st.kind {
                StructKind::Unit => Ty::Null,
                StructKind::Tuple => {
                    let items = st
                        .fields
                        .iter()
                        .map(|f| self.ty(f.shape.get(), depth + 1))
                        .collect();
                    Ty::Tuple(items)
                }
                StructKind::TupleStruct | StructKind::Struct => self.named(shape, depth),
            },
            Type::User(UserType::Enum(_)) => self.named(shape, depth),
            _ if matches!(shape.def, FacetDef::Scalar) => Ty::String,
            _ => self.warn(format!("{}: opaque type", shape.type_identifier)),
        }
    }

    /// A struct, tuple struct or enum: reserve its name, then describe it.
    fn named(&mut self, shape: &'static Shape, depth: usize) -> Ty {
        let id = shape.id.get();
        if let Some(name) = self.ids.get(&id) {
            return Ty::Ref(name.clone());
        }
        let name = self.fresh_name(shape);
        self.ids.insert(id, name.clone());
        let index = self.defs.len();
        self.defs.push(Named {
            name: name.clone(),
            doc: docs(shape.doc),
            def: Def::Tuple { items: Vec::new() },
        });
        let def = match shape.ty {
            Type::User(UserType::Struct(st)) => {
                if st.kind == StructKind::Struct {
                    Def::Struct {
                        fields: self.fields(st.fields, depth),
                    }
                } else {
                    Def::Tuple {
                        items: st
                            .fields
                            .iter()
                            .map(|f| self.ty(f.shape.get(), depth + 1))
                            .collect(),
                    }
                }
            }
            Type::User(UserType::Enum(et)) => {
                if shape.is_untagged() {
                    self.warn(format!(
                        "{name}: #[facet(untagged)] enums have no schema form; emitted as Any"
                    ));
                    // Leave the placeholder out: callers see `Any`, not a dangling reference.
                    self.defs.remove(index);
                    self.ids.remove(&id);
                    self.names.remove(&name);
                    return Ty::Any;
                }
                let repr = match (shape.get_tag_attr(), shape.get_content_attr()) {
                    (Some(tag), Some(content)) => EnumRepr::Adjacent {
                        tag: tag.to_owned(),
                        content: content.to_owned(),
                    },
                    (Some(tag), None) => EnumRepr::Internal {
                        tag: tag.to_owned(),
                    },
                    _ => EnumRepr::External,
                };
                let variants = et
                    .variants
                    .iter()
                    .map(|v| Variant {
                        name: v.effective_name().to_owned(),
                        doc: docs(v.doc),
                        kind: match (v.data.kind, v.data.fields.len()) {
                            (StructKind::Unit, _) | (_, 0) => VariantKind::Unit,
                            (StructKind::Struct, _) => {
                                VariantKind::Struct(self.fields(v.data.fields, depth))
                            }
                            (_, 1) => VariantKind::Newtype(
                                self.ty(v.data.fields[0].shape.get(), depth + 1),
                            ),
                            _ => VariantKind::Tuple(
                                v.data
                                    .fields
                                    .iter()
                                    .map(|f| self.ty(f.shape.get(), depth + 1))
                                    .collect(),
                            ),
                        },
                    })
                    .collect();
                Def::Enum { repr, variants }
            }
            _ => Def::Tuple { items: Vec::new() },
        };
        self.defs[index].def = def;
        Ty::Ref(name)
    }

    fn fields(&mut self, fields: &'static [FacetField], depth: usize) -> Vec<Field> {
        let mut out = Vec::new();
        for f in fields {
            if f.flags.contains(FieldFlags::SKIP) || f.flags.contains(FieldFlags::SKIP_SERIALIZING)
            {
                continue;
            }
            let ty = self.ty(f.shape.get(), depth + 1);
            if f.flags.contains(FieldFlags::FLATTEN) {
                match self.flattened(&ty) {
                    Some(inner) => out.extend(inner),
                    None => {
                        self.warn(format!(
                            "field {}: only a flattened struct can be described; field dropped",
                            f.effective_name()
                        ));
                    }
                }
                continue;
            }
            out.push(Field {
                name: f.effective_name().to_owned(),
                ty,
                doc: docs(f.doc),
            });
        }
        out
    }

    /// The fields a flattened type contributes to its parent. A flattened `Option<Struct>` makes
    /// them all optional, as facet does.
    fn flattened(&self, ty: &Ty) -> Option<Vec<Field>> {
        match ty {
            Ty::Ref(name) => match &self.get(name)?.def {
                Def::Struct { fields } => Some(fields.clone()),
                _ => None,
            },
            Ty::Option(inner) => self.flattened(inner).map(|fs| {
                fs.into_iter()
                    .map(|mut f| {
                        if !f.optional() {
                            f.ty = Ty::Option(Box::new(f.ty));
                        }
                        f
                    })
                    .collect()
            }),
            _ => None,
        }
    }

    /// `Page<Foo>` becomes `PageFoo`; two different types that share a name get a numeric suffix.
    fn fresh_name(&mut self, shape: &'static Shape) -> String {
        let mut base = ident(shape.effective_name());
        for p in shape.type_params {
            base.push_str(&ident(p.shape.effective_name()));
        }
        if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
            base.insert(0, 'T');
        }
        let mut name = base.clone();
        let mut n = 2;
        while self.names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.names.insert(name.clone());
        name
    }
}

fn scalar(s: ScalarType) -> Ty {
    use ScalarType as S;
    match s {
        S::Unit => Ty::Null,
        S::Bool => Ty::Bool,
        S::F32 => Ty::Float { bits: 32 },
        S::F64 => Ty::Float { bits: 64 },
        S::U8 => Ty::Int {
            bits: 8,
            signed: false,
        },
        S::U16 => Ty::Int {
            bits: 16,
            signed: false,
        },
        S::U32 => Ty::Int {
            bits: 32,
            signed: false,
        },
        S::U64 | S::USize => Ty::Int {
            bits: 64,
            signed: false,
        },
        S::I8 => Ty::Int {
            bits: 8,
            signed: true,
        },
        S::I16 => Ty::Int {
            bits: 16,
            signed: true,
        },
        S::I32 => Ty::Int {
            bits: 32,
            signed: true,
        },
        S::I64 | S::ISize => Ty::Int {
            bits: 64,
            signed: true,
        },
        S::U128 => Ty::BigInt { signed: false },
        S::I128 => Ty::BigInt { signed: true },
        _ => Ty::String,
    }
}

fn docs(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end().to_owned())
        .collect()
}

/// Keep ASCII letters, digits and underscores; drop everything else (`<`, `>`, `::`, spaces).
fn ident(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}
