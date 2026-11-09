# wirefold

A zero-copy wire format, a universal `Term`, an integer column codec, a static table-driven entropy coder and
structured-text surfaces (JSON, YAML, TOON, POML, RDF and others), built on
[apeiron](https://github.com/webfold-ai/apeiron). The only dependency is apeiron; the derive crate has none.

```toml
[dependencies]
wirefold = { git = "https://github.com/webfold-ai/wirefold", features = ["derive"] }
```

| Feature | What it adds |
|---------|-------------|
| `codec` (default) | Zero-copy wire format (`codec::web`), universal Term, integer column codec |
| `compress` | Static table-driven entropy coder (`codec::cm`, tANS) |
| `syntax` | Collapsed syntax AST (`codec::syntax`) for JSON, YAML, TOON, POML, RDF and other surfaces |
| `derive` | `#[derive(Collapse)]` and `#[derive(Term)]` |
| `error` | `Error`, `Result`, `Context` and `#[derive(Error)]` |
| `validate` | `Validate` and `#[derive(Validate)]` |

## Documentation

- [Usage guide](docs/USAGE.md): `Collapse`, `Term`, text surfaces, compression, MessagePack. Every snippet
  is in `examples/usage.rs`, which CI runs (`cargo run --example usage --all-features`).
- [Design](docs/DESIGN.md): why it is built on apeiron rather than serde, bincode, ryu and itoa, how parsing
  becomes a fold, and what it gives up.
- [Benchmarks](docs/BENCHMARKS.md): wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd,
  with the harness in `bench/` and a plain statement of where it wins and where it does not.
- [Contributing](CONTRIBUTING.md), [security policy](SECURITY.md), [code of conduct](CODE_OF_CONDUCT.md),
  [changelog](CHANGELOG.md).

## Origin

This is the serialization core of [`Ptyktos/hashtrinity`](https://github.com/Ptyktos/hashtrinity),
renamed. The rest of that repository (the Rust-source surface, `proc_masq`, `itertools`, the reactor
and runtime, the rustls provider and small utilities) stays there.

## Build

```bash
cargo test --all-features
```

Minimum supported Rust version: 1.89, checked in CI. `cargo clippy --all-features -- -D warnings` is clean.

## Licence

Apache-2.0. See `LICENSE`.
