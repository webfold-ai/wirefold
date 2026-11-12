//! Python and WebAssembly bindings for wirefold.
//!
//! One small API, written once in [`api`] in plain Rust, with two thin front ends: a
//! Python module (PyO3, feature `python`) and a WebAssembly module (wasm-bindgen, feature `wasm`,
//! which gives TypeScript declarations). The surface is deliberately narrow and explicit: it takes and returns strings and
//! bytes, never wirefold's internal types.
//!
//! | Function | What it does |
//! |---|---|
//! | `translate(input, from, to)` | convert a document between text surfaces (`json`, `yaml`, `toon`, `poml`, `csv`, `baml`, `turtle`, `rdfxml`) |
//! | `json_to_msgpack(json)` | JSON text to MessagePack bytes |
//! | `msgpack_to_json(bytes)` | MessagePack bytes to JSON text |
//! | `formats()` | the surface names |
//! | `version()` | the crate version |
//!
//! Not exposed: the columnar `Collapse` codec, which is generated per record type and so has no
//! single signature to bind, and the experimental entropy coder.

#![warn(missing_docs)]

pub mod api;

#[cfg(feature = "python")]
mod python;

#[cfg(feature = "wasm")]
mod wasm;
