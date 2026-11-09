# Changelog

All notable changes to this crate are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `docs/DESIGN.md` (the apeiron basis against serde, bincode, ryu and itoa) and fresh serialization and
  number-formatting benchmarks.
- The cm coder's stride search runs its trials in parallel with byte-identical output; nested pool dispatch
  from inside a task now runs inline instead of deadlocking.

- Initial release of wirefold: the serialization core split out of `hashtrinity` and renamed. It contains
  `codec` (zero-copy wire format, universal `Term`, integer column codec, context-mixing coder and the
  structured-text surfaces), the `Collapse`, `Term`, `Validate` and `Error` derives, and the `error` and
  `validate` modules the derives target.
- cargo-fuzz targets for the decoders and parsers, a supply-chain workflow (audit, deny, SBOM) and a signed
  release workflow with SLSA provenance.

### Added

- `codec::syntax::SyntaxTermRef`, `AtomRef` and `parse_surface_ref`: parse JSON and YAML into a term whose keys
  and scalars borrow from the input where the input already holds the exact text. About twice as fast as
  `parse_surface` and faster than `serde_json::Value`; `into_owned` converts back. Other surfaces work with the
  same call but save nothing. For JSON and YAML documents of 512 KiB or more whose top level is an array or a
  margin-level block sequence, `parse_surface_ref` parses the elements in parallel on apeiron's worker pool, with
  an identical result (-49% JSON, -64% YAML on 100,000 records with 4 cores). Finding the cuts is itself a
  fold: per-chunk summaries for JSON, a pure function of the offset for YAML.

### Fixed

- The YAML parser panicked on a block scalar (`|` or `>`) when a later line, indented less than the block's
  margin, had a multi-byte character where the margin falls (slicing inside the character). Untrusted YAML could
  crash the process. It now returns a result.

### Changed

- Rendering is much faster for YAML (-90% instructions), TOON (-85%), POML (-79%), CSV (-77%) and BAML (-36%);
  JSON was already tight. MessagePack encode is 1.9 times faster and protobuf encode 1.7 times; decoders are
  unchanged to within noise. Output is byte-identical.
- Faster parsers for every text surface, with identical results: JSON −18%, YAML −16%, TOON −16%, POML −44%,
  CSV −24%, BAML −5% wall-clock on 20,000 records (instruction counts down 38%, 13%, 28%, 47% and 39% for JSON,
  YAML, TOON, POML and CSV). JSON parsing is now level with `serde_json::Value`. See `docs/BENCHMARKS.md`.

- Minimum supported Rust version is 1.89 (the derive crate already required it).
- `clippy -D warnings` is clean for the whole crate; a short list of inherited style lints is allowed by
  name in `src/lib.rs`.
