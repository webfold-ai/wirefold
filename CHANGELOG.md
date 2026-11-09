# Changelog

All notable changes to this crate are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `docs/DESIGN.md` (the apeiron basis against serde, bincode, ryu and itoa) and fresh serialization and
  number-formatting benchmarks.
- `codec::cm` is now a static, table-driven coder (order-0 or order-1 tANS over four interleaved streams, behind the
  2-D stride search) instead of an adaptive context-mixing coder, and is documented as experimental. `compress()`
  encodes the telemetry benchmark in 127 ms (was 3.5 s) and decodes it in 2.0 ms (was 1.5 s), at 39% larger
  output on that data. zstd -1 does the same in 2.0 ms and 1.2 ms with 237,083 bytes, so compress with zstd when
  compression matters. `Collapse` serialization is unchanged and independent of it.
- Fixed: `ValidationErrors::merge_nested` leaked a `String` for every nested failing field on every call. Field
  names are now `Cow<'static, str>`, so nested names are freed with the error set. Found by AddressSanitizer's
  leak check on the library tests.
- CI gains AddressSanitizer and ThreadSanitizer jobs. Locally, the library and integration tests pass under both,
  Miri finds no undefined behaviour in the tANS, Huffman and `web` module tests or the derive tests, and each fuzz
  target ran 4 minutes without a crash.
- `docs/REPORT.md`: a standalone report comparing `Collapse`, the column encodings and five other serializers against
  lz4, Snappy, deflate, Brotli and zstd on three datasets (medians of three runs; `bench/scripts/merge_reports.py`). The
  benchmark gains those compressors behind one `Backend` type (`bench/src/backends.rs`) and a `report` mode.
- Breaking, section tables are versioned: the first word of every table (the root and each nested record table) is
  now the section count in its low 32 bits and the bytes `W`, `C`, `TABLE_VERSION` (1) and `0` in its high 32 bits,
  so no table grew. `Sections::open` and every derive-generated `collapse_open` refuse a table without the magic
  (`WireErr::BadMagic`, which includes every buffer written before this change, whatever its layout) and one of
  another version (`WireErr::UnsupportedVersion(v)`). `WireErr` gains those two variants and `codec::web` exports
  `TABLE_VERSION` and `table_word`. A plain `Wire<T>` array is not a table and has no header. Tests cover the magic,
  the version, an old-format buffer (including the one-scalar-field case the section count could not tell apart) and
  nested tables; removing either check fails the test.
- Breaking, `Collapse` layout: each scalar field of a record is now its own contiguous column (a `Wire<T>` section,
  `bool` as a byte) instead of one row-major block of packed records, followed by the variable-length columns as before.
  `#[derive(Collapse)]` views gain a zero-copy `<field>_column()` slice accessor per scalar field (omitted when a field
  has that name), `collapse_to_vec` reads each column once, and a scalar field's alignment must divide 8 (checked at
  compile time). Buffers written by earlier versions are refused at open, usually by the section count, and not
  misread (and now, by the table header above, always). Measured on the telemetry benchmark: whole-buffer zstd -3
  189,754 bytes (was 234,791), decode of every record 1.25 ms (was 1.03). The record count of a record with no scalar
  field is kept in one empty count section.
- The benchmark harness reports per column, per zstd level and per block size, with encodings for each column (delta,
  exact-bit-width packing, decimal scaling, dictionary) that take the telemetry records to 105,790 bytes at zstd -1,
  and measures a block store and synthetic bf16 weights. `docs/DESIGN.md` gains a section on a copy-on-write storage
  design (ZFS) from the apeiron basis and `seed-format`. The encodings and the block layer are benchmark code, not library.
- The sanitizer CI job now runs every test with AddressSanitizer's leak check on: `tests/parser_dos.rs` drops its
  200,000-deep terms on a large-stack thread instead of leaking them. The benchmark gains a third, sensor-shaped
  dataset and a delta form for integer columns (a random-walk float column goes from 45.0 to 27.5 KB), and its
  summary no longer presents the column-encoding result as a comparison of wire formats; `docs/BENCHMARKS.md` now
  notes that `Collapse` has no version header and that the CI jobs have not run on GitHub.
- Documentation correction: `Collapse` stores the scalar fields of a record slice as one row-major array of packed
  records and only variable-length fields as columns; earlier text called the whole layout columnar. The
  benchmarks now include compressing the scalar fields plane by plane (a bench-only split): 166,713 bytes against
  234,791 for zstd on the whole buffer at zstd -3 on the telemetry records, with a one-field read that decompresses
  only that field.
- The benchmark harness now measures pipelines: each serializer raw and followed by zstd (levels 1, 3, 6, 19) and
  by `compress()`, with serialize/compress and decompress/deserialize timed separately and Rust-side allocations
  counted, replacing rows that timed a compressor on bytes serialized outside the timer. The docs no longer claim
  the columnar layout compresses smaller: after zstd -19 `Collapse` is larger than bincode and postcard.
- Breaking, `compress` feature: the stream begins with a version byte (`cm::VERSION`, now 1) and a reader refuses any
  other version with `DecodeError::UnsupportedVersion`. Streams from the previous coder, which had no version byte,
  are intentionally not supported. The `k: usize` argument is replaced by `cm::Order::{Zero, One}` (`compress_at`
  takes the enum too) and a header order value outside the enum is `DecodeError::Malformed`; the `DEFAULT_ORDER`
  constant is removed (`Order::default()` is `One`). `compress_at` and `decompress_framed` are thin wrappers.
- The stride search and the face measurement run as parallel batches with the same choice; nested pool dispatch
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
