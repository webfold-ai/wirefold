# Using wirefold

Every snippet here is in [`examples/usage.rs`](../examples/usage.rs), which is compiled and run by
`cargo run --example usage --all-features`.

## Install

```toml
[dependencies]
wirefold = { git = "https://github.com/webfold-ai/wirefold", features = ["derive", "syntax"] }
```

Pick only the features you need (see the table in the [README](../README.md)). The library depends on
apeiron and nothing else; the derive crate depends on nothing.

## Columnar zero-copy records: `#[derive(Collapse)]`

`Collapse` stores a slice of records column by column. Opening the bytes checks the layout once; it
does not walk the rows, so reading one field of one row costs the read, not a decode.

```rust
use wirefold::codec::web::CollapseElem;
use wirefold::Collapse;

#[derive(Collapse, Clone, Debug, PartialEq)]
struct Reading {
    ts: u64,
    sensor: u32,
    celsius: f64,
    label: String,
}

let packed = Reading::collapse_pack(&readings);      // Vec<Reading> -> Seed
let bytes = packed.as_bytes();                       // store or send these
let view = ReadingView::collapse_open(bytes)?;       // validates the layout, no row walk
let one = view.celsius(7);                           // one field of row 7, in place
let all = view.collapse_to_vec();                    // or materialise every record
```

Supported field types: the primitive integers and floats, `bool`, nested `Collapse` records, `String`,
`Vec<_>` of those, and enums with plain-data variants. A field named `open`, `len`, `get`, `is_empty` or
`to_vec` is fine: your field wins its own name and the derive's methods are always available under the
`collapse_*` prefix.

Opening a truncated or corrupt buffer returns an error. It does not panic.

## Structs to and from text: `#[derive(Term)]`

`Term` converts a struct to a format-neutral tree and back. The same tree renders to JSON, YAML, TOON,
POML, CSV, BAML and RDF.

```rust
use wirefold::codec::syntax::{parse_surface, render_surface, Surface};
use wirefold::Term;

#[derive(Term, Clone, Debug, PartialEq)]
struct Person { name: String, age: i64, tags: Vec<String> }

let json = render_surface(&person.to_term(), Surface::Json);
let back = Person::from_term(&parse_surface(Surface::Json, &json)?)?;
```

Convert between surfaces without a struct:

```rust
use wirefold::codec::syntax::{translate, Surface};

let yaml = translate(&json, Surface::Json, Surface::Yaml)?;
```

Scope to know about: the YAML surface is a wide subset of YAML 1.2, not the whole specification, and the
JSON path produces a generic tree rather than deserialising straight into your type. See the doc comments
on `parse_yaml` and `parse_turtle` for the exact scope of each surface.

## Compression

```rust
let z = wirefold::codec::compress(&bytes);
let same = wirefold::codec::decompress(&z);
```

`compress` is a context-mixing coder. It gives much smaller output than the usual compressors on
structured data and is much slower; the numbers are in [BENCHMARKS.md](BENCHMARKS.md). For bytes you did
not produce yourself, use `codec::try_decompress_framed`, which returns an error on malformed input.

## MessagePack

```rust
let mp = wirefold::codec::msgpack::encode(&person.to_term());
let back = Person::from_term(&wirefold::codec::msgpack::decode(&mp)?)?;
```

Object keys come back in sorted order, so compare through your struct rather than the raw term.

## Errors and validation

With the `error` feature, `wirefold::{Error, Result, Context}` and `#[derive(Error)]` give an
`anyhow`/`thiserror`-style error type. With `validate`, the `Validate` trait and `#[derive(Validate)]`
give field validation. Both exist because the derives emit paths into them; use them or ignore them.

## Untrusted input

The decoders and parsers return errors on malformed input, and `tests/codec_hostile_input.rs`,
`tests/parser_dos.rs` and the `fuzz/` targets exist to keep it that way. A panic, hang or input-sized
allocation on hostile bytes is a bug: please report it as described in [SECURITY.md](../SECURITY.md).
