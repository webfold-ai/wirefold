//! A [`serde`] bridge: any `Serialize`/`Deserialize` type to and from wirefold's universal
//! [`SyntaxTerm`], and from there to every surface wirefold renders (JSON, YAML, TOON, POML, RDF,
//! MessagePack and the rest).
//!
//! This is the adoption path for code that already derives serde: no wirefold derive, no changes to
//! the types, one call each way.
//!
//! ```
//! use serde::{Deserialize, Serialize};
//! use wirefold::codec::syntax::Surface;
//!
//! #[derive(Serialize, Deserialize, Debug, PartialEq)]
//! struct Job { id: u32, tags: Vec<String>, retry: Option<u8> }
//!
//! let job = Job { id: 7, tags: vec!["a".into()], retry: None };
//! let json = wirefold::serde_bridge::to_surface(&job, Surface::Json)?;
//! let back: Job = wirefold::serde_bridge::from_surface(Surface::Json, &json)?;
//! assert_eq!(job, back);
//! # Ok::<(), wirefold::serde_bridge::Error>(())
//! ```
//!
//! ## Data model
//!
//! The mapping is serde_json's, so a type that round-trips through `serde_json` round-trips here:
//!
//! | serde | `SyntaxTerm` |
//! |---|---|
//! | unit, `None` | `Nb` |
//! | `bool` | `B(Bool)` |
//! | integers, floats | `B(Num(text))` (floats keep a `.`/exponent so they stay floats) |
//! | `char`, `str` | `B(Str)` |
//! | bytes | `Fourth` of `Num` (one element per byte) |
//! | sequence, tuple, tuple struct | `Fourth` |
//! | map, struct | `Bu` (map keys must serialize as a string, char, bool or integer) |
//! | newtype struct, `Some(x)` | the inner value |
//! | unit variant | `B(Str(name))` |
//! | newtype / tuple / struct variant | `Bu` with one entry, `name: payload` |
//!
//! Non-finite floats are an error: no wirefold surface can write them faithfully. 64-bit integer and
//! float targets also accept decimal text, because wirefold's MessagePack encoder writes values it
//! cannot represent exactly in a native msgpack number (`u64` above `i64::MAX`, `1e300`) that way.

use crate::codec::syntax::{parse_surface, render_surface, Atom, Surface, SyntaxError, SyntaxTerm};
use serde::de::{self, DeserializeOwned, IntoDeserializer, Visitor};
use serde::ser::{self, Serialize};
use std::fmt;

/// Why a value could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    msg: String,
}

impl Error {
    fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::new(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::new(msg.to_string())
    }
}

impl From<SyntaxError> for Error {
    fn from(e: SyntaxError) -> Self {
        Self::new(e.msg)
    }
}

/// Convert any `Serialize` value to a [`SyntaxTerm`].
pub fn to_term<T: Serialize + ?Sized>(value: &T) -> Result<SyntaxTerm, Error> {
    value.serialize(TermSerializer)
}

/// Convert a [`SyntaxTerm`] to any `Deserialize` type. Strings may be borrowed from the term.
pub fn from_term<'a, T: de::Deserialize<'a>>(term: &'a SyntaxTerm) -> Result<T, Error> {
    T::deserialize(TermDeserializer(term))
}

/// Serialize a value and render it in `surface` (JSON, YAML, TOON, POML, ...).
pub fn to_surface<T: Serialize + ?Sized>(value: &T, surface: Surface) -> Result<String, Error> {
    Ok(render_surface(&to_term(value)?, surface))
}

/// Parse `input` as `surface` and deserialize it.
pub fn from_surface<T: DeserializeOwned>(surface: Surface, input: &str) -> Result<T, Error> {
    let term = parse_surface(surface, input)?;
    from_term(&term)
}

/// Serialize a value to MessagePack.
pub fn to_msgpack<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    Ok(crate::codec::msgpack::encode(&to_term(value)?))
}

/// Deserialize a value from MessagePack.
pub fn from_msgpack<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let term = crate::codec::msgpack::decode(bytes)?;
    from_term(&term)
}

// ---------------------------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------------------------

/// A serde `Serializer` whose output is a [`SyntaxTerm`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TermSerializer;

fn num(s: &impl ToString) -> SyntaxTerm {
    SyntaxTerm::B(Atom::Num(s.to_string()))
}

fn float(text: String, finite: bool) -> Result<SyntaxTerm, Error> {
    if finite {
        Ok(SyntaxTerm::B(Atom::Num(text)))
    } else {
        Err(Error::new(
            "non-finite float has no wirefold representation",
        ))
    }
}

macro_rules! ser_num {
    ($($f:ident($t:ty)),* $(,)?) => {$(
        fn $f(self, v: $t) -> Result<SyntaxTerm, Error> { Ok(num(&v)) }
    )*};
}

impl ser::Serializer for TermSerializer {
    type Ok = SyntaxTerm;
    type Error = Error;
    type SerializeSeq = SeqSer;
    type SerializeTuple = SeqSer;
    type SerializeTupleStruct = SeqSer;
    type SerializeTupleVariant = VariantSeqSer;
    type SerializeMap = MapSer;
    type SerializeStruct = StructSer;
    type SerializeStructVariant = VariantStructSer;

    ser_num!(
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
    );

    fn serialize_bool(self, v: bool) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::B(Atom::Bool(v)))
    }
    fn serialize_f32(self, v: f32) -> Result<SyntaxTerm, Error> {
        float(format!("{v:?}"), v.is_finite())
    }
    fn serialize_f64(self, v: f64) -> Result<SyntaxTerm, Error> {
        float(format!("{v:?}"), v.is_finite())
    }
    fn serialize_char(self, v: char) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::B(Atom::Str(v.to_string())))
    }
    fn serialize_str(self, v: &str) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::B(Atom::Str(v.to_owned())))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Fourth(v.iter().map(num).collect()))
    }
    fn serialize_none(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Nb)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<SyntaxTerm, Error> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Nb)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Nb)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::B(Atom::Str(variant.to_owned())))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<SyntaxTerm, Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Bu(vec![(
            variant.to_owned(),
            v.serialize(self)?,
        )]))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSer, Error> {
        Ok(SeqSer(Vec::with_capacity(len.unwrap_or(0).min(4096))))
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqSer, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<SeqSer, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantSeqSer, Error> {
        Ok(VariantSeqSer {
            variant,
            items: Vec::with_capacity(len.min(4096)),
        })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<MapSer, Error> {
        Ok(MapSer {
            entries: Vec::with_capacity(len.unwrap_or(0).min(4096)),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<StructSer, Error> {
        Ok(StructSer(Vec::with_capacity(len)))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantStructSer, Error> {
        Ok(VariantStructSer {
            variant,
            fields: Vec::with_capacity(len),
        })
    }
}

/// Sequence, tuple and tuple-struct builder.
pub struct SeqSer(Vec<SyntaxTerm>);

impl ser::SerializeSeq for SeqSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.0.push(to_term(v)?);
        Ok(())
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Fourth(self.0))
    }
}

impl ser::SerializeTuple for SeqSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SeqSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, v)
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        ser::SerializeSeq::end(self)
    }
}

/// Tuple-variant builder.
pub struct VariantSeqSer {
    variant: &'static str,
    items: Vec<SyntaxTerm>,
}

impl ser::SerializeTupleVariant for VariantSeqSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        self.items.push(to_term(v)?);
        Ok(())
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Bu(vec![(
            self.variant.to_owned(),
            SyntaxTerm::Fourth(self.items),
        )]))
    }
}

/// Map builder.
pub struct MapSer {
    entries: Vec<(String, SyntaxTerm)>,
    key: Option<String>,
}

impl ser::SerializeMap for MapSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error::new("map value without a key"))?;
        self.entries.push((key, to_term(v)?));
        Ok(())
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Bu(self.entries))
    }
}

/// Struct builder.
pub struct StructSer(Vec<(String, SyntaxTerm)>);

impl ser::SerializeStruct for StructSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.0.push((key.to_owned(), to_term(v)?));
        Ok(())
    }
    fn skip_field(&mut self, _: &'static str) -> Result<(), Error> {
        Ok(())
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Bu(self.0))
    }
}

/// Struct-variant builder.
pub struct VariantStructSer {
    variant: &'static str,
    fields: Vec<(String, SyntaxTerm)>,
}

impl ser::SerializeStructVariant for VariantStructSer {
    type Ok = SyntaxTerm;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.fields.push((key.to_owned(), to_term(v)?));
        Ok(())
    }
    fn end(self) -> Result<SyntaxTerm, Error> {
        Ok(SyntaxTerm::Bu(vec![(
            self.variant.to_owned(),
            SyntaxTerm::Bu(self.fields),
        )]))
    }
}

/// Serializes a map key to the string a `Bu` entry needs.
struct KeySerializer;

fn key_err() -> Error {
    Error::new("map keys must serialize as a string, char, bool or integer")
}

macro_rules! key_num {
    ($($f:ident($t:ty)),* $(,)?) => {$(
        fn $f(self, v: $t) -> Result<String, Error> { Ok(v.to_string()) }
    )*};
}

impl ser::Serializer for KeySerializer {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = ser::Impossible<String, Error>;
    type SerializeTuple = ser::Impossible<String, Error>;
    type SerializeTupleStruct = ser::Impossible<String, Error>;
    type SerializeTupleVariant = ser::Impossible<String, Error>;
    type SerializeMap = ser::Impossible<String, Error>;
    type SerializeStruct = ser::Impossible<String, Error>;
    type SerializeStructVariant = ser::Impossible<String, Error>;

    key_num!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_char(char),
    );

    fn serialize_f32(self, _: f32) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_f64(self, _: f64) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_str(self, v: &str) -> Result<String, Error> {
        Ok(v.to_owned())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_none(self) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<String, Error> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<String, Error> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<String, Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<String, Error> {
        Err(key_err())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(key_err())
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Error> {
        Err(key_err())
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(key_err())
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(key_err())
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(key_err())
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Error> {
        Err(key_err())
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(key_err())
    }
}

// ---------------------------------------------------------------------------------------------
// Deserializer
// ---------------------------------------------------------------------------------------------

/// A serde `Deserializer` that reads a borrowed [`SyntaxTerm`].
#[derive(Clone, Copy, Debug)]
pub struct TermDeserializer<'a>(pub &'a SyntaxTerm);

fn visit_number<'de, V: Visitor<'de>>(text: &str, visitor: V) -> Result<V::Value, Error> {
    if let Ok(v) = text.parse::<u64>() {
        return visitor.visit_u64(v);
    }
    if let Ok(v) = text.parse::<i64>() {
        return visitor.visit_i64(v);
    }
    if let Ok(v) = text.parse::<f64>() {
        if v.is_finite() {
            return visitor.visit_f64(v);
        }
    }
    if let Ok(v) = text.parse::<u128>() {
        return visitor.visit_u128(v);
    }
    if let Ok(v) = text.parse::<i128>() {
        return visitor.visit_i128(v);
    }
    Err(Error::new(format!("invalid number {text:?}")))
}

impl<'de> de::Deserializer<'de> for TermDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::Nb | SyntaxTerm::Unknown => visitor.visit_unit(),
            SyntaxTerm::B(Atom::Bool(b)) => visitor.visit_bool(*b),
            SyntaxTerm::B(Atom::Num(n)) => visit_number(n, visitor),
            SyntaxTerm::B(Atom::Str(s)) => visitor.visit_borrowed_str(s),
            SyntaxTerm::Fourth(items) => visitor.visit_seq(SeqDe(items.iter())),
            SyntaxTerm::Bu(fields) => visitor.visit_map(MapDe {
                it: fields.iter(),
                value: None,
            }),
        }
    }

    // 128-bit integers go through their own visitor calls so values past 64 bits survive.
    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            // MessagePack has no 128-bit integer, so that surface carries one as decimal text.
            SyntaxTerm::B(Atom::Num(n) | Atom::Str(n)) => n
                .parse::<i128>()
                .map_err(|e| Error::new(format!("invalid i128 {n:?}: {e}")))
                .and_then(|v| visitor.visit_i128(v)),
            _ => self.deserialize_any(visitor),
        }
    }
    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            // MessagePack has no 128-bit integer, so that surface carries one as decimal text.
            SyntaxTerm::B(Atom::Num(n) | Atom::Str(n)) => n
                .parse::<u128>()
                .map_err(|e| Error::new(format!("invalid u128 {n:?}: {e}")))
                .and_then(|v| visitor.visit_u128(v)),
            _ => self.deserialize_any(visitor),
        }
    }

    // wirefold's MessagePack encoder writes an integer outside `i64` as decimal text, so a 64-bit
    // target accepts that text too.
    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::B(Atom::Str(n)) => n
                .parse::<u64>()
                .map_err(|_| Error::new(format!("invalid u64 {n:?}")))
                .and_then(|v| visitor.visit_u64(v)),
            _ => self.deserialize_any(visitor),
        }
    }
    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::B(Atom::Str(n)) => n
                .parse::<i64>()
                .map_err(|_| Error::new(format!("invalid i64 {n:?}")))
                .and_then(|v| visitor.visit_i64(v)),
            _ => self.deserialize_any(visitor),
        }
    }

    // Likewise a float the encoder cannot prove is its own shortest form (`1e300`) goes out as text.
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::B(Atom::Str(n)) => n
                .parse::<f64>()
                .map_err(|_| Error::new(format!("invalid f64 {n:?}")))
                .and_then(|v| visitor.visit_f64(v)),
            _ => self.deserialize_any(visitor),
        }
    }
    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::B(Atom::Str(n)) => n
                .parse::<f32>()
                .map_err(|_| Error::new(format!("invalid f32 {n:?}")))
                .and_then(|v| visitor.visit_f32(v)),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::Nb | SyntaxTerm::Unknown => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.0 {
            SyntaxTerm::B(Atom::Str(s)) => visitor.visit_enum(EnumDe {
                variant: s,
                payload: None,
            }),
            SyntaxTerm::Bu(fields) => match fields.as_slice() {
                [(variant, payload)] => visitor.visit_enum(EnumDe {
                    variant,
                    payload: Some(payload),
                }),
                _ => Err(Error::new(format!(
                    "enum {name} must be a string or an object with exactly one entry"
                ))),
            },
            _ => Err(Error::new(format!(
                "enum {name} must be a string or an object with exactly one entry"
            ))),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 u8 u16 u32 char str string bytes byte_buf
        unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

struct SeqDe<'a>(std::slice::Iter<'a, SyntaxTerm>);

impl<'de> de::SeqAccess<'de> for SeqDe<'de> {
    type Error = Error;
    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        self.0
            .next()
            .map(|t| seed.deserialize(TermDeserializer(t)))
            .transpose()
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

struct MapDe<'a> {
    it: std::slice::Iter<'a, (String, SyntaxTerm)>,
    value: Option<&'a SyntaxTerm>,
}

impl<'de> de::MapAccess<'de> for MapDe<'de> {
    type Error = Error;
    fn next_key_seed<K: de::DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.it.next() {
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(KeyDe(k)).map(Some)
            }
            None => Ok(None),
        }
    }
    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let v = self
            .value
            .take()
            .ok_or_else(|| Error::new("map value without a key"))?;
        seed.deserialize(TermDeserializer(v))
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.it.len())
    }
}

/// Deserializes a `Bu` key; integer, bool and char key types parse the key text.
struct KeyDe<'a>(&'a str);

macro_rules! key_parse {
    ($($f:ident => $visit:ident($t:ty)),* $(,)?) => {$(
        fn $f<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            let v: $t = self
                .0
                .parse()
                .map_err(|_| Error::new(format!("map key {:?} is not a valid {}", self.0, stringify!($t))))?;
            visitor.$visit(v)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for KeyDe<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_borrowed_str(self.0)
    }

    key_parse!(
        deserialize_bool => visit_bool(bool),
        deserialize_i8 => visit_i8(i8),
        deserialize_i16 => visit_i16(i16),
        deserialize_i32 => visit_i32(i32),
        deserialize_i64 => visit_i64(i64),
        deserialize_i128 => visit_i128(i128),
        deserialize_u8 => visit_u8(u8),
        deserialize_u16 => visit_u16(u16),
        deserialize_u32 => visit_u32(u32),
        deserialize_u64 => visit_u64(u64),
        deserialize_u128 => visit_u128(u128),
    );

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_enum(self.0.into_deserializer())
    }

    serde::forward_to_deserialize_any! {
        f32 f64 char str string bytes byte_buf unit unit_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

struct EnumDe<'a> {
    variant: &'a str,
    payload: Option<&'a SyntaxTerm>,
}

impl<'de> de::EnumAccess<'de> for EnumDe<'de> {
    type Error = Error;
    type Variant = VariantDe<'de>;
    fn variant_seed<V: de::DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, VariantDe<'de>), Error> {
        let v = seed.deserialize(KeyDe(self.variant))?;
        Ok((v, VariantDe(self.payload)))
    }
}

struct VariantDe<'a>(Option<&'a SyntaxTerm>);

impl<'de> de::VariantAccess<'de> for VariantDe<'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        match self.0 {
            None | Some(SyntaxTerm::Nb) => Ok(()),
            Some(_) => Err(Error::new("unit variant carries a payload")),
        }
    }
    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        match self.0 {
            Some(t) => seed.deserialize(TermDeserializer(t)),
            None => Err(Error::new("newtype variant is missing its payload")),
        }
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Some(SyntaxTerm::Fourth(items)) => visitor.visit_seq(SeqDe(items.iter())),
            _ => Err(Error::new("tuple variant payload must be an array")),
        }
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.0 {
            Some(SyntaxTerm::Bu(fields)) => visitor.visit_map(MapDe {
                it: fields.iter(),
                value: None,
            }),
            _ => Err(Error::new("struct variant payload must be an object")),
        }
    }
}
