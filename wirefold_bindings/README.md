# wirefold_bindings

Python and TypeScript (WebAssembly) bindings for [wirefold](../README.md), through one small API.

| Function | What it does |
|---|---|
| `translate(input, from, to)` | convert a document between `json`, `yaml`, `toon`, `poml`, `csv`, `baml`, `turtle`, `rdfxml` |
| `json_to_msgpack(json)` / `jsonToMsgpack` | JSON text to MessagePack bytes |
| `msgpack_to_json(bytes)` / `msgpackToJson` | MessagePack bytes to JSON text |
| `formats()` | the surface names |
| `version()` | the version |

Every function takes and returns strings and bytes; a failure raises `ValueError` (Python) or throws
`Error` (JavaScript) with a message that names what was rejected. Hostile input is an error, not a
crash (`tests/python/test_wirefold.py` and `tests/wasm/test.mjs` feed both bad MessagePack).

Not exposed: the columnar `Collapse` codec (generated per record type, so there is no single
signature to bind) and the experimental entropy coder.

## Python

```python
import wirefold
wirefold.translate('{"a": [1, 2]}', "json", "yaml")
wirefold.msgpack_to_json(wirefold.json_to_msgpack('{"a": 1}'))
```

```bash
scripts/test-python.sh        # builds the extension, runs the tests (no maturin needed)
maturin build --release       # a wheel (abi3: one wheel for Python 3.9+)
```

## TypeScript / JavaScript

```bash
scripts/build-wasm.sh         # needs the wasm32-unknown-unknown target and a matching wasm-bindgen CLI
node tests/wasm/test.mjs
```

`build-wasm.sh` writes `dist/` (a Node.js module and `wirefold_bindings.d.ts`).

## Not done

Go, Java/Kotlin, Swift and C bindings. A C ABI (`cbindgen`) would serve Go and C; UniFFI would serve
Kotlin and Swift but is MPL-2.0, which this repository's `deny.toml` does not allow.
