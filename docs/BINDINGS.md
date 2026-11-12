# Bindings: using wirefold from other types and other languages

There are three separate problems, and they have separate answers.

1. **Rust types you already have** (derived with serde or facet) should reach wirefold's surfaces and
   MessagePack without rewriting them. That is the [serde bridge](#serde-bridge) and
   [`wirefold_facet`](#facet-wirefold_facet).
2. **Other languages** should read and write the same data. That is
   [generated types for the JSON wire form](#other-languages).
3. **Other languages should call wirefold itself**: translate between surfaces, convert JSON to and
   from MessagePack. That is [`wirefold_bindings`](#calling-wirefold-from-python-and-typescript), for
   Python and for TypeScript through WebAssembly.

## Serde bridge

Enable the `serde` feature and convert any `Serialize`/`Deserialize` type. No wirefold derive, no changes to
the type.

```toml
wirefold = { git = "https://github.com/webfold-ai/wirefold", features = ["serde"] }
```

```rust
use wirefold::codec::syntax::Surface;
use wirefold::serde_bridge::{from_msgpack, from_surface, to_msgpack, to_surface, to_term};

let json = to_surface(&job, Surface::Json)?;
let back: Job = from_surface(Surface::Json, &json)?;
let bytes = to_msgpack(&job)?;
let term = to_term(&job)?;            // wirefold's universal SyntaxTerm
```

The data model is serde_json's (see the table in the `serde_bridge` module docs), so a type that round-trips
through `serde_json` round-trips here. `serde` is an optional dependency; without the feature the crate still
depends on apeiron alone.

Things to know:

- Non-finite floats are an error.
- A 64-bit integer or float target also accepts decimal text. wirefold's MessagePack encoder writes a `u64` above
  `i64::MAX` and a float like `1e300` as text, and this is what lets them come back.
- Map keys must serialize as a string, char, bool or integer.
- Some surfaces have their own limits; see [Known gaps](#known-gaps).

## Facet: `wirefold_facet`

For types that derive `Facet` instead. It is a separate workspace crate rather than a feature, because facet
pulls in `smallvec` 2 (beta), whose `From` impls make rustc reject wirefold's own `From<E: Error> for Error`
whenever both are compiled in one crate. A separate crate keeps wirefold's dependency list and its `error`
feature intact.

```toml
wirefold = { git = "https://github.com/webfold-ai/wirefold" }
wirefold_facet = { git = "https://github.com/webfold-ai/wirefold" }
```

```rust
use facet::Facet;
use wirefold_facet::{emit, from_surface, to_surface, Schema};

#[derive(Facet)]
struct Job { id: u32, tags: Vec<String> }

let json = to_surface(&job, wirefold::codec::syntax::Surface::Json)?;
let schema = Schema::of::<Job>();
std::fs::write("job.py", emit::python(&schema))?;
```

Conversion reuses facet's own serialization (renames, `skip`, `flatten`, enum tagging). Facet writes a tuple
struct as an array but reads one only from an object, so `from_term` rewrites those arrays first (see
`wirefold_facet/src/adapt.rs`); without that, `struct Meters(f64)` would not survive its own round trip.

## Other languages

`Schema::of::<T>()` reads the type's `Shape` and describes its JSON-shaped wire form: structs, tuple structs,
enums (externally, internally and adjacently tagged), options, lists, maps, tuples, recursion and generics.
Emitters turn that into source for:

| Emitter | Output | Runtime |
|---|---|---|
| `emit::python` | dataclasses, `Enum`s, `decode_*`/`encode_*` | Python 3.8+, standard library |
| `emit::typescript` | `interface`s and union types | none (types only) |
| `emit::go` | structs with `json` tags, custom (un)marshalling for enums and tuples | Go standard library |
| `emit::json_schema` | JSON Schema 2020-12 | none |

`json_schema` is the exit for everything else. Kotlin, Swift, Java, C#, Dart and the rest each have a mature
generator that reads JSON Schema (quicktype, openapi-generator, ...), so one emitter covers them without us
maintaining a backend per language. Add a dedicated emitter when a language needs a codec that a schema
generator cannot give it.

The generated code does **not** reimplement wirefold's binary formats. It reads and writes JSON with each
language's standard library, which is the same data that `to_term` produces.

What is checked: `wirefold_facet/tests/facet_bridge.rs` runs Rust → JSON → Python / Go → JSON → Rust over a
document that uses every shape above and requires the result to equal the original, and compiles a TypeScript
literal of the same JSON against the generated types. Those tests skip with a notice when a toolchain is
missing; `WIREFOLD_REQUIRE_TOOLCHAINS=1` makes that a failure, and CI sets it.

What a schema cannot describe is reported in `Schema::warnings` and emitted as `Any` / `unknown`, not guessed:
`#[facet(untagged)]` enums, `Result`, opaque types, n-dimensional arrays, trait objects.

Wire choices a reader of the generated code will notice: a 128-bit integer is a decimal string; `Vec<u8>` is an
array of numbers (`[]int` in Go, because `[]byte` marshals as base64); `Option<T>` fields are `null` when `None`
and may be absent; an inline tuple is `[]any` in Go.

## Calling wirefold from Python and TypeScript

`wirefold_bindings` is one small, explicit API (`translate`, `json_to_msgpack`, `msgpack_to_json`,
`formats`, `version`) written once in plain Rust, with two thin front ends: a PyO3 Python module and a
wasm-bindgen WebAssembly module with TypeScript declarations. It takes and returns strings and bytes,
never wirefold's internal types. See [`wirefold_bindings/README.md`](../wirefold_bindings/README.md).

Both are tested against the built artifact, not just the Rust: `scripts/test-python.sh` builds the
extension and runs `unittest` against it, and `scripts/build-wasm.sh` plus `tests/wasm/test.mjs` load
the compiled `.wasm` in Node. Both feed hostile MessagePack and require an error, not a crash. CI runs
them (`language-bindings` job).

PyO3 and wasm-bindgen are MIT/Apache-2.0, so they fit this repository's `deny.toml`. UniFFI would add
Kotlin and Swift but is MPL-2.0, which `deny.toml` does not allow; that is a licensing decision for the
owner, not one to make in a dependency bump.

The WebAssembly build needed a fix in apeiron (`MAX_K` overflowed a 32-bit `usize`); wirefold pins the
commit that carries it (webfold-ai/apeiron#6).

## Not done yet

- **Go, Java/Kotlin, Swift and C bindings for the codecs.** A C ABI (`cbindgen`) would serve Go and C;
  UniFFI would serve Kotlin and Swift (see the licence note above). The columnar `Collapse` codec is not
  bound at all: it is generated per record type, so there is no single signature to expose.
- **Independent binary codecs per language** (no Rust library loaded). `serde-reflection` + `serde-generate` is
  the closest existing architecture: a language-neutral registry plus per-language backends and runtime
  libraries, tested against the Rust side for interoperability. `Schema` is the registry half of that; the
  runtimes are not written.
- Kotlin, Swift, Java and C# emitters, if the JSON Schema route proves insufficient.
- Python/Go/TypeScript emitters for a type that facet or serde cannot describe (`untagged`, `Result`).

## Known gaps

These were found while building the above. The first two are upstream; the rest are in wirefold and untouched.

- facet-value can write an internally tagged *newtype* variant (`#[facet(tag = "kind")] enum E { A(Inner) }`)
  but cannot read it back ("internally tagged tuple variants are not supported"). The generated Python, Go and
  TypeScript handle it; the Rust side does not. The tests check it in one direction.
- facet widens `f32` to `f64` when writing (`0.1f32` becomes `0.10000000149011612`).
- The YAML renderer does not quote strings that contain quotes or newlines and writes an empty list as nothing
  (`a: []` comes out as `a:`), so such values do not parse back. JSON and MessagePack are unaffected.
- wirefold's MessagePack encoder writes a `u64` above `i64::MAX` and floats it cannot prove shortest-form (`1e300`)
  as strings rather than as native msgpack numbers. Other msgpack readers will see strings there.
