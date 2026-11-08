# wirefold

A zero-copy wire format, a universal `Term`, an integer column codec, a context-mixing coder and
structured-text surfaces (JSON, YAML, TOON, POML, RDF and others), built on
[apeiron](https://github.com/webfold-ai/apeiron). The only dependency is apeiron; the derive crate has none.

```toml
[dependencies]
wirefold = { git = "https://github.com/webfold-ai/wirefold", features = ["derive"] }
```

| Feature | What it adds |
|---------|-------------|
| `codec` (default) | Zero-copy wire format (`codec::web`), universal Term, integer column codec |
| `compress` | Context-mixing entropy coder (`codec::cm`) |
| `syntax` | Collapsed syntax AST (`codec::syntax`) for JSON, YAML, TOON, POML, RDF and other surfaces |
| `derive` | `#[derive(Collapse)]` and `#[derive(Term)]` |
| `error` | `Error`, `Result`, `Context` and `#[derive(Error)]` |
| `validate` | `Validate` and `#[derive(Validate)]` |

## Origin

This is the serialization core of [`Ptyktos/hashtrinity`](https://github.com/Ptyktos/hashtrinity),
renamed. The rest of that repository (the Rust-source surface, `proc_masq`, `itertools`, the reactor
and runtime, the rustls provider and small utilities) stays there.

## Build

```bash
cargo test --all-features
```

Minimum supported Rust version: 1.89 (the derive crate's `rust-version`).

## Licence

Apache-2.0. See `LICENSE`.
