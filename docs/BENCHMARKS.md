# Benchmarks

wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd on the same records. The harness
is [`bench/`](../bench/README.md). The serialization tables come from the run described under Results; the [parsing](#parsing) numbers
from runs described there.

## Summary

Serialization and compression are separate stages, and the tables below time them separately: a pipeline is a
serializer, optionally followed by a compressor, and encode is serialize then compress while decode is
decompress then deserialize.

- **Serialization alone (the `raw` pipelines):** wirefold `Collapse` is the same size as bincode (713,212 against
  713,060 bytes for 20,000 telemetry records) and encodes in about the same time (0.20 against 0.16 ms). It
  decodes slower than bincode (1.25 against 0.92 ms) and rkyv (1.16 ms), and faster than MessagePack through
  rmp-serde (1.65 ms). Against serde_json it is about 11 times faster to encode and 3.6 times faster to decode,
  and 2.3 times smaller. postcard is smaller (24.6 against 35.7 bytes per record) and rkyv larger (40.0).
- **With a compressor, the layout decides the size.** `Collapse` stores each scalar field as its own contiguous
  column, so the compressor sees one field's values at a time. With plain zstd on the whole buffer the telemetry
  records are 189,754 bytes at -3 and 167,132 at -19, against 219,402 and 199,079 for bincode, 207,314 and 194,386 for
  postcard and 225,765 and 187,121 for rkyv. Before that layout (the scalar fields interleaved row after row) it was
  234,791 and 207,470, the largest of the binary formats. Encode and decode times with zstd are the same as the
  other serializers'.
- **Encoding each column for its type roughly halves it again.** Delta coding the timestamps, packing integers to
  their exact bit width, storing decimal floats as scaled integers and dictionary-coding the strings, each column then
  compressed with zstd on its own, gives 105,790 bytes at zstd -1 (5.3 bytes per record): 2.3 ms to encode, 2.0 ms to
  decode, 46% smaller than the best other pipeline at zstd -19 (postcard, 194,386 bytes, 164 ms to encode). Going up to
  zstd -19 only reaches 104,729. Reading one field decompresses only its column: 0.17 ms for the `f64` field against
  1.64 ms to decompress the whole buffer and then read. These encodings are in the benchmark, not the library; see
  [Layout, columns and blocks](#layout-columns-and-blocks), which also has the per-column, per-level and per-block-size
  breakdowns. On the random data there is little to find: the column pipelines land at about 590,000 bytes, 6% below
  whole-buffer zstd -3 (624,300) and 14% below bincode + zstd -3 (683,707).
- **Reading one column without decoding the rest:** opening a `Collapse` buffer validates it (all 20,000 strings
  as UTF-8) in 0.33 ms; reading the 20,000 `f64` values from an open view takes 0.013 ms. rkyv's validate-and-read takes
  0.26 ms on the telemetry data and 0.08 ms on the random data, where wirefold's open takes 0.28 ms.
- **wirefold's own coder (`compress()`, experimental):** after `Collapse` it gives 222,540 bytes and costs
  89 ms to encode and 3.8 ms to decode, against 199,392 bytes, 3.1 ms and 3.6 ms for zstd -1. It does not beat zstd
  at any level. `Collapse` does not depend on it; if you want compression, compress the `Collapse` bytes with zstd.
- **Allocations:** every deserializer allocates once per record for the name string (about 20,000 calls for
  20,000 records), so decode allocation counts are within 14 of each other for the plain pipelines. Encode is
  where they differ: `Collapse` makes 3 calls (1.4 MiB, the packed buffer and a copy), bincode 1, postcard 17,
  rmp-serde 18, rkyv 16 and serde_json 15. wirefold's coder makes 900 to 1,600 calls and 20 to 61 MiB per encode.
- **Parsing text into a generic tree:** wirefold's owned JSON parser is level with `serde_json::Value`, and
  `parse_surface_ref`, which borrows strings from the input, is about 1.8 times faster than it on one thread,
  and about 5 times faster on a pool of 4 for documents over 512 KiB (23 ms against about 110 ms for 100,000
  records). The other surfaces parse at 74 to 130 MB/s. See [Parsing](#parsing).
- **Rendering a tree to text:** YAML, TOON, CSV and POML render 3 to 7 times faster than they did, and JSON
  rendering was already tight. See [Rendering](#rendering).
- **Binary codecs:** MessagePack encoding is about 1.9 times faster and protobuf encoding about 1.7 times;
  decoding is unchanged for MessagePack and about 5% faster for protobuf. MessagePack is still about 1.4
  times slower to encode than `rmp-serde`. See [Binary codecs](#binary-codecs).

So wirefold is not a faster serde. What it offers is a different combination: no dependencies beyond
apeiron, a layout whose open step validates without walking the rows, one `Term` that reads and
writes JSON, YAML, TOON, POML, MessagePack and others, and an experimental dependency-free coder. Its scalar
fields are stored as one row-major array of records and its variable-length fields as columns.

## Setup

- 20,000 records of one shape (`u64`, `u32`, `u8`, `f64`, `bool`, `String`), in two datasets: telemetry-shaped
  (increasing timestamps, small domains, six repeated names) and random (full-range integers and floats,
  random-looking names), which has no structure for a column store to exploit.
- Machine: 4 vCPU Intel Xeon @ 2.10 GHz virtual machine, rustc 1.97.0, release build with `lto = true` and
  `codegen-units = 1`, run on an otherwise idle machine.
- Versions: serde_json 1 (with `float_roundtrip` so floats are exact), bincode 1.3, postcard 1,
  rmp-serde 1, rkyv 0.8 (validated), zstd 0.13 (levels 1, 3, 6 and 19).
- Pipelines: each of the six serializers is run raw and followed by zstd -1, -3, -6, -19 and wirefold's
  `compress()`; `Collapse` is also run with its columns compressed one by one (plain, encoded for their types, and
  byte-shuffled). Each pipeline is round-trip checked before it is timed and runs in its own process. A run
  is the median of 9 repetitions (5 for `compress()`, 3 for zstd -19); the encode and decode totals are the mean
  of 3 runs with the range shown, and the stage columns are the mean of the same runs.
- Every serializer is measured as bytes out of the records and records back out of bytes. wirefold and rkyv go
  through a `Vec<u8>` (a copy each way) so that every pipeline has the same shape; rkyv needs aligned input, so its
  decode copies the bytes into an aligned buffer, counted in its deserialize stage.
- Allocations are the calls and MiB requested from the Rust allocator during one encode and one decode, counted
  by a wrapper around the system allocator. zstd's internal C allocations are not seen.

## Results

20,000 records. The rows are the pipelines described under Setup; `raw` is the serializer alone. The `columns`
rows split the buffer into its columns and are described under [Layout, columns and blocks](#layout-columns-and-blocks).

#### Telemetry-shaped records (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 713212 | 35.7 | 0.20 (0.20 to 0.21) | 0.20 | 0.00 | 1.25 (1.21 to 1.30) | 0.00 | 1.25 | 3 (1.4) | 20001 (1.0) |
| wirefold Collapse + zstd -1 | 199392 | 10.0 | 3.12 (2.50 to 3.46) | 0.28 | 2.84 | 3.55 (3.30 to 3.85) | 1.54 | 2.02 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -3 | 189754 | 9.5 | 3.13 (3.05 to 3.24) | 0.21 | 2.92 | 2.16 (2.07 to 2.26) | 0.91 | 1.25 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -6 | 180132 | 9.0 | 10.29 (8.05 to 14.64) | 0.26 | 10.03 | 2.69 (2.13 to 3.77) | 1.20 | 1.49 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -19 | 167132 | 8.4 | 240.08 (229.06 to 248.36) | 0.32 | 239.76 | 2.33 (2.17 to 2.49) | 1.10 | 1.23 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + wirefold compress() | 222540 | 11.1 | 89.08 (83.61 to 96.98) | 0.28 | 88.80 | 3.82 (3.51 to 4.17) | 2.24 | 1.58 | 1412 (34.1) | 20052 (3.3) |
| wirefold Collapse + columns, zstd -1 | 178207 | 8.9 | 2.93 (2.78 to 3.21) | 0.52 | 2.41 | 2.25 (2.08 to 2.34) | 1.04 | 1.21 | 59 (4.1) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -3 | 167109 | 8.4 | 3.11 (3.04 to 3.20) | 0.20 | 2.90 | 2.22 (2.13 to 2.31) | 1.03 | 1.19 | 59 (4.1) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -6 | 163356 | 8.2 | 8.01 (7.77 to 8.27) | 0.20 | 7.81 | 2.17 (2.12 to 2.22) | 0.99 | 1.18 | 58 (4.0) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -19 | 151261 | 7.6 | 661.89 (659.19 to 666.95) | 0.33 | 661.56 | 2.25 (2.21 to 2.31) | 0.96 | 1.29 | 58 (4.0) | 20055 (5.3) |
| wirefold Collapse + columns, encoded planes, zstd -1 | 105790 | 5.3 | 2.28 (2.20 to 2.39) | 0.21 | 2.07 | 1.99 (1.96 to 2.01) | 0.75 | 1.24 | 133 (5.5) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -3 | 107638 | 5.4 | 2.46 (2.33 to 2.62) | 0.20 | 2.25 | 2.04 (2.00 to 2.12) | 0.80 | 1.24 | 133 (5.5) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -6 | 106939 | 5.3 | 3.64 (3.53 to 3.73) | 0.21 | 3.43 | 1.93 (1.85 to 2.00) | 0.75 | 1.18 | 133 (5.5) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -19 | 104729 | 5.2 | 318.76 (310.37 to 327.26) | 0.32 | 318.44 | 2.38 (2.04 to 3.00) | 0.93 | 1.45 | 133 (5.5) | 20091 (4.9) |
| wirefold Collapse + columns, byte shuffle, zstd -3 | 201596 | 10.1 | 1.86 (1.73 to 1.96) | 0.20 | 1.66 | 2.43 (2.37 to 2.50) | 1.17 | 1.26 | 64 (4.5) | 20055 (5.3) |
| bincode 1.3 raw | 713060 | 35.7 | 0.16 (0.15 to 0.19) | 0.16 | 0.00 | 0.92 (0.88 to 0.98) | 0.00 | 0.92 | 1 (0.7) | 20001 (1.0) |
| bincode 1.3 + zstd -1 | 234749 | 11.7 | 2.34 (2.28 to 2.40) | 0.18 | 2.16 | 1.66 (1.62 to 1.74) | 0.81 | 0.86 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -3 | 219402 | 11.0 | 2.90 (2.78 to 3.08) | 0.18 | 2.72 | 1.76 (1.74 to 1.79) | 0.85 | 0.91 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -6 | 203248 | 10.2 | 8.18 (7.92 to 8.52) | 0.20 | 7.99 | 1.62 (1.56 to 1.70) | 0.72 | 0.90 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -19 | 199079 | 10.0 | 261.79 (255.09 to 271.59) | 0.29 | 261.50 | 1.82 (1.77 to 1.86) | 0.90 | 0.91 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + wirefold compress() | 238005 | 11.9 | 50.26 (47.43 to 52.67) | 0.23 | 50.03 | 3.07 (2.87 to 3.19) | 1.77 | 1.30 | 895 (20.0) | 20052 (3.2) |
| postcard raw | 492525 | 24.6 | 0.42 (0.40 to 0.43) | 0.42 | 0.00 | 1.02 (0.98 to 1.03) | 0.00 | 1.02 | 17 (1.0) | 20001 (1.0) |
| postcard + zstd -1 | 263942 | 13.2 | 2.13 (1.98 to 2.25) | 0.43 | 1.70 | 1.84 (1.83 to 1.86) | 0.75 | 1.10 | 23 (2.0) | 20009 (2.1) |
| postcard + zstd -3 | 207314 | 10.4 | 2.97 (2.91 to 3.05) | 0.44 | 2.52 | 1.84 (1.80 to 1.87) | 0.76 | 1.08 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -6 | 195023 | 9.8 | 7.15 (7.05 to 7.25) | 0.42 | 6.73 | 1.68 (1.62 to 1.75) | 0.62 | 1.06 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -19 | 194386 | 9.7 | 164.23 (160.24 to 169.63) | 0.46 | 163.77 | 1.88 (1.70 to 2.02) | 0.80 | 1.08 | 22 (1.5) | 20009 (2.1) |
| postcard + wirefold compress() | 215656 | 10.8 | 48.03 (45.64 to 51.12) | 0.46 | 47.57 | 3.28 (2.87 to 4.02) | 1.79 | 1.50 | 953 (21.7) | 20031 (2.5) |
| rmp-serde (MessagePack) raw | 611506 | 30.6 | 0.57 (0.56 to 0.57) | 0.57 | 0.00 | 1.65 (1.62 to 1.69) | 0.00 | 1.65 | 18 (2.0) | 20001 (1.0) |
| rmp-serde (MessagePack) + zstd -1 | 227528 | 11.4 | 2.26 (2.23 to 2.29) | 0.59 | 1.67 | 2.40 (2.22 to 2.50) | 0.66 | 1.74 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -3 | 220582 | 11.0 | 2.53 (2.43 to 2.68) | 0.52 | 2.01 | 2.22 (2.09 to 2.43) | 0.59 | 1.62 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -6 | 215972 | 10.8 | 7.18 (7.12 to 7.28) | 0.56 | 6.62 | 2.30 (2.11 to 2.45) | 0.63 | 1.67 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -19 | 202583 | 10.1 | 188.08 (175.84 to 197.97) | 0.61 | 187.47 | 2.81 (2.46 to 3.47) | 0.72 | 2.09 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + wirefold compress() | 220043 | 11.0 | 47.95 (47.37 to 48.74) | 0.53 | 47.41 | 3.00 (2.71 to 3.15) | 0.91 | 2.09 | 911 (21.6) | 20031 (2.2) |
| rkyv 0.8 (validated) raw | 800008 | 40.0 | 0.29 (0.27 to 0.32) | 0.29 | 0.00 | 1.16 (1.09 to 1.29) | 0.00 | 1.16 | 16 (2.8) | 20002 (2.0) |
| rkyv 0.8 (validated) + zstd -1 | 273202 | 13.7 | 3.11 (2.67 to 3.67) | 0.28 | 2.82 | 2.78 (2.70 to 2.84) | 1.49 | 1.30 | 22 (3.8) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -3 | 225765 | 11.3 | 2.90 (2.65 to 3.07) | 0.26 | 2.64 | 1.85 (1.76 to 1.99) | 0.86 | 0.99 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -6 | 220282 | 11.0 | 10.04 (9.58 to 10.87) | 0.28 | 9.76 | 1.95 (1.83 to 2.07) | 0.87 | 1.08 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -19 | 187121 | 9.4 | 299.75 (290.40 to 307.43) | 0.43 | 299.32 | 2.66 (2.17 to 3.12) | 1.21 | 1.45 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + wirefold compress() | 218413 | 10.9 | 131.87 (124.38 to 137.54) | 0.35 | 131.53 | 2.79 (2.60 to 2.92) | 1.32 | 1.47 | 1428 (30.0) | 20032 (3.4) |
| serde_json raw | 1646252 | 82.3 | 2.29 (1.92 to 2.98) | 2.29 | 0.00 | 4.54 (4.40 to 4.66) | 0.00 | 4.54 | 15 (4.0) | 20014 (3.1) |
| serde_json + zstd -1 | 260706 | 13.0 | 5.81 (5.71 to 5.91) | 2.02 | 3.80 | 6.69 (6.62 to 6.75) | 1.94 | 4.75 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -3 | 265783 | 13.3 | 6.48 (6.00 to 6.75) | 2.01 | 4.48 | 5.98 (5.70 to 6.29) | 1.27 | 4.71 | 21 (5.0) | 20024 (7.2) |
| serde_json + zstd -6 | 240484 | 12.0 | 17.51 (17.22 to 17.95) | 2.07 | 15.44 | 5.85 (5.78 to 5.97) | 1.11 | 4.74 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -19 | 201563 | 10.1 | 927.06 (880.32 to 973.25) | 2.20 | 924.86 | 6.41 (5.44 to 8.26) | 1.13 | 5.28 | 20 (4.5) | 20024 (7.2) |
| serde_json + wirefold compress() | 244651 | 12.2 | 56.57 (53.96 to 58.90) | 2.45 | 54.12 | 6.51 (6.43 to 6.56) | 1.43 | 5.09 | 893 (26.5) | 20044 (5.3) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold open only (validates the table) | 0.33 (0.32 to 0.33) |
| wirefold, view already open: per-element accessor | 0.01 (0.01 to 0.01) |
| wirefold, view already open: column slice | 0.01 (0.01 to 0.01) |
| wirefold view (open + per-element accessor) | 0.34 (0.34 to 0.35) |
| wirefold view (open + column slice) | 0.35 (0.35 to 0.35) |
| rkyv 0.8 (validate + read) | 0.26 (0.25 to 0.28) |
| bincode (decode all, then sum) | 0.92 (0.89 to 0.95) |
| postcard (decode all, then sum) | 1.11 (1.08 to 1.16) |
| whole buffer zstd -3: decompress all, then read | 1.64 (1.28 to 1.84) |
| columns, zstd -3: decompress one column | 0.24 (0.20 to 0.32) |
| columns, encoded planes, zstd -3: decompress one column | 0.17 (0.15 to 0.22) |

#### Random records (no structure to exploit) (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 918792 | 45.9 | 0.82 (0.73 to 1.00) | 0.77 | 0.06 | 1.31 (1.10 to 1.74) | 0.00 | 1.31 | 3 (1.8) | 20001 (1.2) |
| wirefold Collapse + zstd -1 | 613402 | 30.7 | 2.52 (2.20 to 3.15) | 0.51 | 2.01 | 1.76 (1.67 to 1.83) | 0.61 | 1.15 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -3 | 624300 | 31.2 | 3.20 (3.14 to 3.29) | 0.23 | 2.97 | 1.90 (1.85 to 1.96) | 0.76 | 1.14 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -6 | 617234 | 30.9 | 6.90 (6.79 to 7.01) | 0.23 | 6.68 | 1.84 (1.81 to 1.86) | 0.71 | 1.13 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -19 | 624034 | 31.2 | 178.83 (169.89 to 192.47) | 0.37 | 178.46 | 2.26 (2.14 to 2.35) | 1.15 | 1.10 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + wirefold compress() | 664841 | 33.2 | 151.72 (145.51 to 155.16) | 0.30 | 151.42 | 3.10 (2.74 to 3.55) | 1.52 | 1.57 | 1303 (50.1) | 20045 (4.8) |
| wirefold Collapse + columns, zstd -1 | 591178 | 29.6 | 2.06 (2.05 to 2.07) | 0.61 | 1.45 | 1.85 (1.82 to 1.90) | 0.76 | 1.10 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -3 | 590820 | 29.5 | 2.77 (2.58 to 2.89) | 0.22 | 2.55 | 1.85 (1.78 to 1.91) | 0.74 | 1.10 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -6 | 593085 | 29.7 | 6.62 (6.41 to 6.81) | 0.23 | 6.39 | 1.84 (1.79 to 1.87) | 0.74 | 1.10 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -19 | 585556 | 29.3 | 455.53 (437.59 to 473.69) | 0.32 | 455.21 | 1.75 (1.73 to 1.77) | 0.61 | 1.14 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, encoded planes, zstd -1 | 591198 | 29.6 | 3.45 (3.31 to 3.52) | 0.24 | 3.21 | 2.38 (2.34 to 2.42) | 1.26 | 1.12 | 134 (10.4) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -3 | 590840 | 29.5 | 5.00 (4.44 to 5.96) | 0.24 | 4.76 | 2.34 (2.27 to 2.47) | 1.21 | 1.13 | 134 (10.4) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -6 | 593105 | 29.7 | 8.91 (8.46 to 9.29) | 0.25 | 8.67 | 2.45 (2.41 to 2.50) | 1.29 | 1.17 | 134 (10.4) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -19 | 585576 | 29.3 | 481.47 (476.36 to 485.81) | 0.34 | 481.14 | 2.51 (2.17 to 2.90) | 1.20 | 1.31 | 134 (10.4) | 20074 (7.3) |
| wirefold Collapse + columns, byte shuffle, zstd -3 | 589031 | 29.5 | 4.87 (4.09 to 5.30) | 0.28 | 4.59 | 3.10 (2.30 to 3.56) | 1.59 | 1.51 | 70 (7.2) | 20057 (6.7) |
| bincode 1.3 raw | 918640 | 45.9 | 0.22 (0.18 to 0.28) | 0.22 | 0.00 | 0.99 (0.82 to 1.32) | 0.00 | 0.99 | 1 (0.9) | 20001 (1.2) |
| bincode 1.3 + zstd -1 | 678272 | 33.9 | 4.51 (3.65 to 5.94) | 0.22 | 4.28 | 2.54 (2.11 to 2.83) | 1.41 | 1.13 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -3 | 683707 | 34.2 | 6.53 (5.27 to 8.88) | 0.25 | 6.28 | 2.46 (2.10 to 2.81) | 1.46 | 1.00 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -6 | 686931 | 34.3 | 15.83 (12.73 to 21.97) | 0.24 | 15.60 | 2.17 (2.15 to 2.19) | 1.33 | 0.83 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -19 | 673404 | 33.7 | 260.83 (237.72 to 297.62) | 0.32 | 260.51 | 2.32 (2.29 to 2.34) | 1.43 | 0.89 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + wirefold compress() | 660097 | 33.0 | 69.27 (65.54 to 74.23) | 0.31 | 68.96 | 4.56 (4.48 to 4.63) | 3.30 | 1.26 | 1015 (36.4) | 20031 (4.1) |
| postcard raw | 827235 | 41.4 | 0.52 (0.49 to 0.55) | 0.52 | 0.00 | 1.17 (1.12 to 1.20) | 0.00 | 1.17 | 18 (2.0) | 20001 (1.2) |
| postcard + zstd -1 | 740567 | 37.0 | 2.34 (2.26 to 2.44) | 0.74 | 1.60 | 2.26 (2.22 to 2.32) | 1.07 | 1.19 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -3 | 740578 | 37.0 | 2.79 (2.67 to 2.88) | 0.50 | 2.30 | 2.22 (2.19 to 2.25) | 1.07 | 1.16 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -6 | 729758 | 36.5 | 9.88 (9.56 to 10.14) | 0.53 | 9.34 | 2.44 (2.38 to 2.51) | 1.24 | 1.20 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -19 | 692567 | 34.6 | 177.55 (169.44 to 181.69) | 0.60 | 176.95 | 2.84 (2.68 to 2.96) | 1.65 | 1.19 | 25 (4.0) | 20010 (3.3) |
| postcard + wirefold compress() | 670280 | 33.5 | 70.14 (64.45 to 75.14) | 0.59 | 69.56 | 4.34 (4.15 to 4.51) | 2.88 | 1.46 | 1055 (45.6) | 20008 (3.1) |
| rmp-serde (MessagePack) raw | 868558 | 43.4 | 0.82 (0.79 to 0.84) | 0.82 | 0.00 | 1.81 (1.79 to 1.84) | 0.00 | 1.81 | 18 (2.0) | 20001 (1.2) |
| rmp-serde (MessagePack) + zstd -1 | 764693 | 38.2 | 2.81 (2.69 to 2.92) | 1.04 | 1.77 | 3.03 (2.96 to 3.11) | 1.16 | 1.86 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -3 | 760886 | 38.0 | 5.53 (5.22 to 6.02) | 0.81 | 4.72 | 3.13 (3.07 to 3.19) | 1.27 | 1.86 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -6 | 744722 | 37.2 | 11.68 (11.42 to 11.91) | 0.80 | 10.88 | 3.18 (3.11 to 3.23) | 1.34 | 1.84 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -19 | 710596 | 35.5 | 203.95 (182.50 to 229.81) | 0.90 | 203.04 | 3.59 (3.43 to 3.77) | 1.73 | 1.86 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + wirefold compress() | 676350 | 33.8 | 72.73 (68.51 to 78.24) | 0.86 | 71.87 | 5.57 (5.27 to 5.85) | 3.30 | 2.27 | 914 (39.1) | 20008 (3.2) |
| rkyv 0.8 (validated) raw | 1118640 | 55.9 | 0.38 (0.35 to 0.41) | 0.38 | 0.00 | 0.98 (0.92 to 1.10) | 0.00 | 0.98 | 19 (5.1) | 20002 (3.2) |
| rkyv 0.8 (validated) + zstd -1 | 700458 | 35.0 | 4.73 (4.54 to 4.93) | 0.96 | 3.76 | 2.97 (2.89 to 3.01) | 1.70 | 1.27 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -3 | 702764 | 35.1 | 6.22 (5.80 to 6.44) | 0.33 | 5.89 | 2.38 (2.26 to 2.51) | 1.51 | 0.87 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -6 | 705922 | 35.3 | 15.28 (14.48 to 15.76) | 0.33 | 14.95 | 2.17 (2.10 to 2.24) | 1.29 | 0.88 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -19 | 647127 | 32.4 | 257.15 (254.54 to 259.45) | 0.54 | 256.60 | 2.23 (1.76 to 2.86) | 1.13 | 1.09 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + wirefold compress() | 715062 | 35.8 | 122.79 (118.57 to 126.34) | 0.49 | 122.30 | 5.12 (5.02 to 5.20) | 3.81 | 1.31 | 1533 (56.5) | 20032 (6.5) |
| serde_json raw | 2461362 | 123.1 | 2.68 (2.28 to 3.42) | 2.68 | 0.00 | 6.26 (5.95 to 6.70) | 0.00 | 6.26 | 16 (8.0) | 20149 (3.3) |
| serde_json + zstd -1 | 831395 | 41.6 | 10.47 (9.76 to 11.87) | 2.31 | 8.16 | 9.03 (8.57 to 9.59) | 2.53 | 6.50 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -3 | 917434 | 45.9 | 18.28 (17.93 to 18.70) | 2.83 | 15.45 | 10.01 (9.48 to 10.52) | 3.76 | 6.25 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -6 | 872907 | 43.6 | 42.45 (41.38 to 44.28) | 3.01 | 39.44 | 8.88 (8.81 to 8.97) | 2.70 | 6.18 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -19 | 749105 | 37.5 | 1076.64 (1036.20 to 1129.87) | 2.46 | 1074.18 | 8.23 (7.69 to 8.84) | 2.24 | 5.99 | 23 (10.0) | 20160 (11.4) |
| serde_json + wirefold compress() | 817820 | 40.9 | 132.55 (128.94 to 135.74) | 2.41 | 130.14 | 11.22 (10.88 to 11.46) | 4.41 | 6.81 | 1251 (60.8) | 20220 (9.1) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold open only (validates the table) | 0.28 (0.27 to 0.29) |
| wirefold, view already open: per-element accessor | 0.01 (0.01 to 0.01) |
| wirefold, view already open: column slice | 0.01 (0.01 to 0.01) |
| wirefold view (open + per-element accessor) | 0.28 (0.28 to 0.29) |
| wirefold view (open + column slice) | 0.28 (0.27 to 0.28) |
| rkyv 0.8 (validate + read) | 0.08 (0.07 to 0.08) |
| bincode (decode all, then sum) | 0.75 (0.70 to 0.79) |
| postcard (decode all, then sum) | 1.12 (1.08 to 1.21) |
| whole buffer zstd -3: decompress all, then read | 0.99 (0.98 to 1.01) |
| columns, zstd -3: decompress one column | 0.05 (0.04 to 0.05) |
| columns, encoded planes, zstd -3: decompress one column | 0.05 (0.04 to 0.05) |

## Layout, columns and blocks

### The layout

`Collapse` is a section table: one section per scalar field of the record, each a contiguous column
(`[count][values]`, 8-aligned, `bool` as a byte), then one section per variable-length field (the strings, as offsets
and bytes). It used to pack all the scalar fields of a record into one row-major block, which put one field's values
22 bytes apart; the change is what moves the compressed sizes below. What it did to the other numbers, on the telemetry
records:

| | before (scalars row-major) | after (a column per field) |
|---|---:|---:|
| raw size | 713,116 B | 713,212 B (the longer section table) |
| raw encode | 0.18 ms | 0.20 ms |
| raw decode, all records | 1.03 ms | 1.25 ms (+21%) |
| `open` (validate the table) | not measured separately | 0.33 ms, almost all of it the 20,000 UTF-8 checks |
| read the `f64` field, view open | not measured separately | 0.013 ms, by element or as `value_column()` |
| zstd -1 on the whole buffer | 237,083 B | 199,392 B |
| zstd -3 on the whole buffer | 234,791 B | 189,754 B |
| zstd -19 on the whole buffer | 207,470 B | 167,132 B |

Decoding every record got slower because it now reads five columns and the strings instead of one block and the strings.
Each column is available as a zero-copy slice (`value_column()`), so a column can be summed, searched or handed to a
compressor without touching the others.

### Per column

Where the bytes are, for each column: as stored and under zstd, then encoded for its type (delta coding for the
increasing timestamps, exact-bit-width packing of integers, decimal floats stored as scaled integers, a dictionary for the
strings) and under zstd. Each integer column is stored as the smaller of two forms, minimum-subtracted bytes at the
narrowest byte width (which zstd then compresses) or bit-packed at the exact width (which it cannot).

#### Per column, telemetry records (20000 records, 713212 bytes)

| plane | raw (B) | zstd -1 | zstd -3 | zstd -19 | encoded (B) | encoded, zstd -1 | zstd -3 | zstd -19 | encoding |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| ts (u64) | 160000 | 24448 | 24448 | 24461 | 20018 | 14386 | 14386 | 14431 | delta, integer |
| user (u32) | 80000 | 43956 | 41039 | 40228 | 32511 | 32520 | 32520 | 32520 | integer |
| kind (u8) | 20000 | 6686 | 7736 | 6731 | 20001 | 6818 | 7736 | 6731 | as stored |
| value (f64) | 160000 | 73452 | 67364 | 60197 | 42512 | 42521 | 42521 | 42521 | decimal scaled, integer |
| ok (bool) | 20000 | 2553 | 2522 | 1599 | 20001 | 2553 | 2523 | 1597 | as stored |
| name lengths | 160000 | 10058 | 6237 | 7599 | 20001 | 6783 | 7748 | 6726 | dictionary |
| name bytes | 113052 | 17598 | 17287 | 10318 | 60 | 69 | 64 | 63 | dictionary |
| **total** | 713052 | 178751 | 166633 | 151133 | 155104 | 105650 | 107498 | 104589 | |

What the encodings reach, against what each column holds. `ts` is increasing by 1 to 50, so its differences carry
about 5.6 bits each: 14.1 KB at the entropy bound, 14.4 KB achieved. `user` is uniform over 5,000 values, 12.3 bits each, a
30.7 KB bound, and 32.5 KB is achieved (zstd on the bytes alone gets 40.2 KB). `value` is uniform over 100,000 cent values,
16.6 bits each, a 41.5 KB bound, and 42.5 KB is achieved (zstd on the bytes alone: 60.2 KB; on the scaled integers as
bytes: 53.3 KB, which is what bit packing beats). `kind` has 6 values (2.6 bits, 6.4 KB bound, 6.7 KB achieved) and `ok`
is true nine times in ten (0.47 bits, 1.2 KB bound, 1.6 KB achieved). The six distinct names become one byte each plus a
60-byte dictionary. The columns' bounds sum to about 100.4 KB and the achieved total is 104.6 KB at zstd -19, so
about 4% is left on this data.

Byte-shuffling the columns instead (grouping the first bytes of every value, then the second, and so on) made the
whole buffer larger than not shuffling at every level here (201,596 against 167,109 bytes at -3), so it is not used.
For 8-byte columns like these the encodings above are what works. Shuffling is what helps for floats whose
fields do not align with bytes: see the weights below.

#### Per column, random records (20000 records, 918792 bytes)

| plane | raw (B) | zstd -1 | zstd -3 | zstd -19 | encoded (B) | encoded, zstd -1 | zstd -3 | zstd -19 | encoding |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| ts (u64) | 160000 | 160012 | 160012 | 160012 | 160011 | 160023 | 160023 | 160023 | delta, integer |
| user (u32) | 80000 | 80009 | 80009 | 80009 | 80011 | 80020 | 80020 | 80020 | integer |
| kind (u8) | 20000 | 20009 | 20009 | 20009 | 20001 | 20010 | 20010 | 20010 | as stored |
| value (f64) | 160000 | 160012 | 160012 | 160012 | 160001 | 160013 | 160013 | 160013 | as stored |
| ok (bool) | 20000 | 3865 | 3800 | 2578 | 20001 | 3865 | 3800 | 2578 | as stored |
| name lengths | 160000 | 7729 | 1990 | 2073 | 160001 | 6763 | 1991 | 2062 | as stored |
| name bytes | 318632 | 159391 | 164043 | 159762 | 318633 | 160364 | 164843 | 160730 | as stored |
| **total** | 918632 | 591027 | 589875 | 584455 | 918659 | 591058 | 590700 | 585436 | |

On the random data nothing helps: every column is incompressible except the `ok` column, the string lengths and the hex
names, which zstd already halves.

### Every level

Size and time at every zstd level that matters, for the whole buffer, for each column as its own frame, for each
encoded column as its own frame, and the encoded columns spread over 4 threads (column `i` on thread `i mod 4`).
In-process medians of 5 runs (3 from level 15).

#### Every level, telemetry records (20000 records, 713212 bytes)

| level | whole: size | enc ms | dec ms | planes: size | enc ms | dec ms | encoded planes: size | enc ms | dec ms | enc ms, 4 threads |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 199392 | 2.02 | 0.81 | 178207 | 2.40 | 1.09 | 105790 | 2.06 | 0.79 | 2.56 |
| 2 | 188471 | 2.76 | 1.01 | 166974 | 2.83 | 1.18 | 106589 | 2.11 | 0.86 | 2.36 |
| 3 | 189754 | 4.58 | 1.46 | 167109 | 4.88 | 1.52 | 107638 | 2.42 | 0.86 | 3.26 |
| 4 | 188665 | 4.95 | 1.36 | 165006 | 5.13 | 1.59 | 107633 | 3.45 | 1.10 | 4.23 |
| 5 | 182619 | 5.45 | 1.02 | 159550 | 5.46 | 1.14 | 107161 | 3.29 | 0.86 | 4.49 |
| 6 | 180132 | 7.94 | 0.98 | 163356 | 8.09 | 1.08 | 106939 | 4.01 | 1.15 | 4.51 |
| 7 | 179090 | 8.39 | 0.91 | 161902 | 10.04 | 0.97 | 106853 | 4.17 | 0.81 | 5.82 |
| 8 | 178386 | 11.42 | 0.93 | 161347 | 12.37 | 1.08 | 106798 | 4.56 | 0.82 | 7.03 |
| 9 | 178438 | 11.78 | 0.91 | 161393 | 14.01 | 0.97 | 106798 | 5.96 | 0.82 | 8.63 |
| 10 | 177437 | 16.33 | 0.95 | 160044 | 20.89 | 0.95 | 106755 | 8.85 | 0.80 | 5.88 |
| 12 | 176574 | 53.11 | 0.90 | 158923 | 161.99 | 0.99 | 106710 | 138.18 | 0.90 | 46.49 |
| 15 | 176744 | 97.24 | 0.90 | 158791 | 300.44 | 0.96 | 106607 | 235.68 | 1.21 | 86.16 |
| 19 | 167132 | 239.67 | 1.09 | 151261 | 685.33 | 1.40 | 104729 | 313.60 | 0.77 | 113.13 |
| 22 | 166783 | 623.89 | 1.37 | 151264 | 3082.75 | 1.82 | 104723 | 2648.73 | 0.92 | 995.20 |

Reading the table: the encoded columns at zstd -1 are 47% smaller than the whole buffer at the same level, at the
same encode time (2.06 against 2.02 ms) and the same decode time. The size barely moves from level 1 (105,790 B) to level 19 (104,729
B) because the encodings have already removed what the higher levels find, so level 19 buys 1% for 150 times the encode
time on this data. Splitting the columns over 4 threads helps only where the compression itself is slow: at level 19 it
takes the encode from 314 to 113 ms (2.8 times) and at level 22 from 2,649 to 995 ms, and at levels 1 to 9 it
is no faster than one thread.

#### Every level, random records (20000 records, 918792 bytes)

| level | whole: size | enc ms | dec ms | planes: size | enc ms | dec ms | encoded planes: size | enc ms | dec ms | enc ms, 4 threads |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 613402 | 1.14 | 0.61 | 591178 | 1.28 | 0.78 | 591198 | 3.04 | 1.25 | 2.86 |
| 2 | 622553 | 1.22 | 0.69 | 591239 | 1.24 | 0.79 | 591259 | 3.02 | 1.26 | 2.77 |
| 3 | 624300 | 5.36 | 1.15 | 590820 | 4.14 | 0.89 | 590840 | 4.85 | 1.58 | 5.12 |
| 4 | 625868 | 5.59 | 1.07 | 592382 | 5.20 | 0.76 | 592402 | 6.92 | 1.78 | 7.74 |
| 5 | 617414 | 5.41 | 0.77 | 592445 | 5.16 | 0.77 | 592465 | 7.22 | 1.31 | 6.89 |
| 6 | 617234 | 6.70 | 0.80 | 593085 | 6.61 | 0.76 | 593105 | 10.72 | 1.26 | 11.18 |
| 7 | 617191 | 7.45 | 0.72 | 592979 | 8.02 | 1.05 | 592999 | 13.47 | 1.70 | 10.77 |
| 8 | 617330 | 7.69 | 0.74 | 592985 | 8.22 | 0.77 | 593005 | 9.65 | 1.21 | 12.32 |
| 9 | 617330 | 8.59 | 0.77 | 592984 | 9.98 | 0.71 | 593004 | 11.43 | 1.16 | 13.41 |
| 10 | 617157 | 10.63 | 0.77 | 592837 | 13.24 | 0.72 | 592857 | 14.88 | 2.01 | 17.31 |
| 12 | 616948 | 36.34 | 0.76 | 592657 | 159.34 | 1.08 | 592677 | 166.80 | 1.21 | 58.36 |
| 15 | 616572 | 87.02 | 0.89 | 592674 | 251.17 | 0.78 | 592694 | 268.44 | 1.34 | 99.76 |
| 19 | 624034 | 188.39 | 1.15 | 585556 | 448.10 | 0.66 | 585576 | 467.32 | 1.09 | 197.72 |
| 22 | 624034 | 564.04 | 1.69 | 585291 | 3266.85 | 1.03 | 585311 | 3199.52 | 1.55 | 1303.69 |

### Blocks

The same columns cut into fixed-size blocks, each compressed on its own, stored with a 16-byte pointer (checksum,
logical and stored length), and identical blocks stored once. A block store of this kind is what a copy-on-write
filesystem such as ZFS keeps, and the section table is already its table of pointers. For each block size: the stored size,
encode and decode time, the time to read 100 rows of the `f64` column by decompressing only the blocks that cover them
against decompressing the whole column, the bytes that sit in duplicate blocks, and the cost of an FNV-1a checksum
over every block (a fast 64-bit non-cryptographic hash; it only times the cost of one).

#### Blocks, telemetry records (20000 records)

| block size | blocks | stored (B) | with pointers (B) | encode ms | decode ms | read 100 rows (ms) | whole plane read (ms) | duplicate bytes | checksum ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 KiB | 40 | 107426 | 108066 | 1.82 | 0.70 | 0.014 | 0.019 | 0 | 0.21 |
| 16 KiB | 14 | 107429 | 107653 | 1.21 | 0.45 | 0.021 | 0.025 | 0 | 0.21 |
| 64 KiB | 7 | 107498 | 107610 | 0.87 | 0.32 | 0.027 | 0.026 | 0 | 0.21 |
| 256 KiB | 7 | 107498 | 107610 | 0.85 | 0.31 | 0.026 | 0.026 | 0 | 0.21 |
| whole plane | 7 | 107498 | 107610 | 0.85 | 0.32 | 0.026 | 0.026 | 0 | 0.21 |

#### Blocks, telemetry records, scaled up (1000000 records)

| block size | blocks | stored (B) | with pointers (B) | encode ms | decode ms | read 100 rows (ms) | whole plane read (ms) | duplicate bytes | checksum ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1897 | 5364424 | 5394776 | 84.63 | 33.95 | 0.014 | 0.411 | 0 | 10.41 |
| 16 KiB | 479 | 5365389 | 5373053 | 35.90 | 13.05 | 0.016 | 0.408 | 0 | 10.49 |
| 64 KiB | 123 | 5374027 | 5375995 | 21.26 | 7.40 | 0.022 | 0.524 | 0 | 10.34 |
| 256 KiB | 33 | 5375481 | 5376009 | 18.25 | 5.56 | 0.034 | 0.452 | 0 | 10.28 |
| whole plane | 7 | 5382006 | 5382118 | 18.30 | 6.16 | 0.530 | 0.515 | 0 | 10.25 |

#### Blocks, random records (20000 records)

| block size | blocks | stored (B) | with pointers (B) | encode ms | decode ms | read 100 rows (ms) | whole plane read (ms) | duplicate bytes | checksum ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 KiB | 228 | 592335 | 595983 | 11.55 | 5.07 | 0.018 | 0.039 | 0 | 1.29 |
| 16 KiB | 59 | 587016 | 587960 | 3.94 | 1.21 | 0.015 | 0.031 | 0 | 1.32 |
| 64 KiB | 18 | 586839 | 587127 | 1.94 | 0.73 | 0.022 | 0.031 | 0 | 1.30 |
| 256 KiB | 8 | 589628 | 589756 | 2.84 | 0.64 | 0.034 | 0.034 | 0 | 1.30 |
| whole plane | 7 | 590700 | 590812 | 2.52 | 0.61 | 0.032 | 0.032 | 0 | 1.29 |

Finding: block size costs almost nothing in size (at 1,000,000 records the 4 KiB store is 0.3% smaller than whole columns,
because each block adapts to its own data) and cuts the cost of reading a few rows from 0.53 ms to 0.014 to 0.034 ms
(15 to 38 times) at that size, where at 20,000 records the columns are small enough that it makes no difference. The
price is the per-block overhead in encode and decode: 4 KiB blocks take 4.6 times as long to encode and 5.5 times as long to
decode as whole columns, 64 KiB blocks 16% and 20% longer. So 16 to 64 KiB is the useful range. No duplicate block
turned up in either dataset: deduplication does nothing for a single set of records with this much variety, and would
pay for repeated or versioned data, which these runs do not include.

### Model weights

`seed-format` stores model checkpoints (tensors as raw bytes with a dtype). The cheapest test of whether
column-wise compression helps weights is bf16 values drawn from a normal distribution with standard deviation 0.02, as in an
initialised or trained linear layer. This is synthetic: it shows the mechanism, not any real checkpoint.

#### Model weights (bf16), synthetic normal (sd 0.02), 4000000 values, 8000000 bytes

| representation | zstd -1 | zstd -3 | zstd -19 | share of raw at -3 |
|---|---:|---:|---:|---:|
| as stored (interleaved bytes) | 6238029 | 6238961 | 6240658 | 78.0% |
| two byte planes (high, low), compressed separately | 5474415 | 5693916 | 5359504 | 71.2% |
| high byte plane alone (sign and 7 exponent bits) | 1474316 | 1693817 | 1359405 | 21.2% |
| low byte plane alone (1 exponent bit, 7 mantissa bits) | 4000099 | 4000099 | 4000099 | 50.0% |

Splitting each 2-byte value into a high plane (sign and 7 exponent bits) and a low plane (1 exponent bit and 7 mantissa
bits) takes zstd -3 from 78.0% to 71.2% of the raw size (67.0% at -19): the high plane compresses to 42% of itself and
the mantissa plane does not compress at all. Here byte-shuffling is right, because the fields of a bf16 value really are
split across the two bytes.

### What is in the library and what is not

The layout is in the library: `#[derive(Collapse)]` writes a column per scalar field and each view has
`<field>_column()`. Everything else on this page that is not zstd is in `bench/src/main.rs` and knows the `Event`
record: the encodings (delta, exact-bit-width packing, decimal scaling, dictionary), the per-column frames, the thread
split and the block store. zstd stays out of the library. Making the encodings and the block layer library features is
a separate piece of work.

## Parsing

Speed of each text surface turning a document into a generic tree, 20,000 records (1.6 to 6.5 MB depending on
the surface), each in its own process, from `cd bench && cargo run --release -- parse`. "Before" is the parsers
as first split from hashtrinity (commit `7a9c900`, which you can check out and run the same command against);
"owned" is `parse_surface` now; "borrowed" is `parse_surface_ref`, which JSON and YAML parse into directly.

| surface | size (bytes) | before (ms) | owned now (ms) | borrowed (ms) |
|---|---:|---:|---:|---:|
| JSON | 1,646,252 | 20.2 | 17.0 | 8.8 |
| YAML | 1,686,251 | 25.5 | 22.4 | 13.0 |
| TOON | 2,619,591 | 34.9 | 29.3 | - |
| POML | 6,466,278 | 97.3 | 54.1 | - |
| CSV | 2,948,507 | 48.5 | 37.0 | - |
| BAML | 1,946,257 | 17.0 | 16.3 | - |

`serde_json::Value` takes 15.6 ms on the same JSON, so the owned parser is level with it and the borrowed one is
about 1.8 times faster. At 100,000 records (8.2 MB) the owned parser takes 101 to 105 ms against 110 to 113 ms.
Instruction counts for the same inputs fell by 38% (JSON), 13% (YAML), 28% (TOON), 47% (POML) and 39% (CSV) for
the owned parsers, and by a further 56% (JSON) and 41% (YAML) for the borrowed ones.

What was slow, found by profiling with callgrind: `malloc` and `free` were 40% of the JSON parse, POML built a
substring searcher for every tag it looked for, and TOON and CSV ran several character searches and an
allocation per line just to read a path. The fixes are bulk-copying strings that need no unescaping, building
each object, list and mapping at its exact size from a reusable stack, searching for tags and path separators
byte by byte, and not allocating for the CSV columns and TOON path segments.

The owned term still owns a `String` for every key and scalar, which is most of what is left. The borrowed term
(`SyntaxTermRef`) removes it: a string with no escapes, a number, a plain YAML scalar or an unquoted key is a
slice of the input. Use it when the tree is read and dropped, or kept no longer than the input.

### Parallel parsing of the borrowed term

An array is the concatenation of its elements, and list concatenation is associative, so a document whose top
level is an array (JSON) or a block sequence at the left margin (YAML) is a fold over its elements.
`parse_surface_ref` cuts such a document at element boundaries, parses the pieces on apeiron's `Pool` (a static,
schedule-free partition: the answer cannot depend on which core ran which piece), and joins the results in piece
order. It applies from 512 KiB up; below that the path is unchanged. Finding the cuts is also a fold, so no
pass over the document is serial:

- JSON: whether a `"` is a real delimiter depends only on the backslashes immediately before it, so each chunk
  classifies its own quotes. A chunk summarises itself under both possible entry states (outside or inside a
  string) in one pass: the net bracket depth, whether the string state flips, and the first comma at each depth.
  Those summaries combine associatively, and the combine over the chunk count is the only serial step.
- YAML: each cut is a pure function of its byte offset, the first margin dash line at or after it, found by looking
  forward only as far as that line. Each piece's start and end are computed from its index alone.

100,000 records (8.2 MB JSON, 8.4 MB YAML), `cargo run --release -- parse json-ref 100000`, mean of 6 runs with
the observed range, on 4 cores. One thread is `HASHTRINITY_POOL_WIDTH=1`.

| document | one thread (ms) | pool of 4 (ms) | change |
|---|---:|---:|---:|
| JSON, borrowed | 44.7 (41.5 to 48.7) | 22.9 (22.2 to 24.5) | -49% |
| YAML, borrowed | 78.2 (76.0 to 82.3) | 28.5 (25.8 to 34.5) | -64% |

An earlier version found the cuts with one serial scan over the document. Replacing it with the folds above
took JSON from 35.0 (sd 2.5) to 29.6 (sd 1.8) ms on the 8 MB document in 15 paired runs, every pair favouring the
new version, and YAML from 40.7 to 33.5 ms in 6 runs with non-overlapping ranges.

For reference the owned parsers take 90.8 ms (JSON) and 139.9 ms (YAML) on the same documents, and they are not
parallel: the owned term allocates a `String` per key and scalar on the worker threads and then frees them from
the thread that drops the tree, and dropping got slower by about as much as parsing got faster (parse 57 to 69
ms down to 38 to 42 ms, drop 32 to 36 ms up to 38 to 45 ms, net 79.9 ms to 76.9 ms, inside the spread). A change
inside the spread is not a change, so `parse_surface` stays sequential.

Why the result is the same as the sequential parse:

- JSON: the document is `[` body `]`. If every piece is a valid comma-separated run of values, the body is one,
  and the sequential parser reads it as exactly those values in that order, wherever the cuts fell. A cut in the
  wrong place (inside a string or a nested container) makes a piece fail, never a different result. Any failing
  piece makes the call decline, and the sequential parser then produces the result or the error, so messages are
  unchanged.
- YAML: the cuts are at margin dash lines, and a piece must parse as a sequence and leave no line unread. An item
  the sequential parser would stop inside (after which it ignores everything) leaves lines unread, so such a
  document is declined. What an item looks ahead to at its end is the next margin dash line, which for an item and
  for the end of a piece alike means "no more of this item".
- Checked by `parallel_parse` in `src/codec/syntax/tests.rs`: tens of thousands of generated and mutated
  documents with a one-byte chunk floor, requiring that the parallel parse either declines or equals the
  sequential one, and never returns a value where the sequential parser reports an error; and a separate test
  that on valid documents full of commas, brackets, quotes and backslash runs the cuts land on top-level commas,
  so the parallel path is always taken (a wrong cut only makes a piece fail, which the first test cannot see).
  Mutating the implementation (accepting trailing garbage in a piece, ignoring unread lines, cutting mid-line,
  losing the string state or the depth across chunks, taking every quote for a delimiter) turns a test red, and
  restoring it turns it green. On the 8 MB documents the result equals the owned parse at pool widths 1,
  2 and 4.

Not covered: the combine over the chunk summaries (a handful of entries) and the final join of the pieces' item
lists (a memmove of the parsed items) are serial, and the gain depends on the machine having idle cores. The pool's
workers spin briefly between dispatches, as documented in apeiron.

How to read these numbers:

- Compare surfaces in separate processes. The heap state left by one parse changes the speed of the next by
  10 to 20%, in both directions, so `cargo run --release -- parse yaml` and so on is the fair way to measure one
  surface; the all-surfaces run starts each surface in its own process for the same reason.
- This is a shared virtual machine: the same row varies by about 10% between runs, so a difference smaller than
  that is not a result. Instruction counts (callgrind) do not vary, and are quoted where a wall-clock figure
  would be too close to call.
- Behaviour is unchanged. The old and new parsers were compared on tens of millions of generated and mutated
  inputs across the surfaces with no difference in result or error message, the borrowed parser against the
  owned one on millions more, and `tests/parse_behaviour.rs` pins the edge cases.

## Rendering

Speed of turning a generic tree into text, 20,000 records, each surface in its own process, from
`cargo run --release -- render`. "Before" is commit `7a9c900`.

| surface | size (bytes) | before (ms) | now (ms) | instructions |
|---|---:|---:|---:|---:|
| JSON | 1,646,252 | 4.1 | 4.1 | unchanged |
| YAML | 1,686,251 | 15.5 | 2.2 | -90% |
| TOON | 2,619,591 | 24.9 | 2.9 | -85% |
| POML | 6,466,278 | 28.1 | 9.5 | -79% |
| CSV | 2,948,507 | 26.7 | 6.0 | -77% |
| BAML | 1,946,257 | 7.4 | 4.5 | -36% |

The renderers other than JSON built a new `String` for almost every line or node (a path string and a
`format!` per TOON and CSV line, a returned `String` per YAML node, a `replace` chain per POML escape) and then
joined and copied them. They now write into one output buffer, with one path buffer extended and truncated as the
walk goes. Output is byte-identical: old and new renderers were compared on hundreds of thousands of terms in
every surface, including the odd path cases (a key named `$`, empty containers).

## Binary codecs

MessagePack and protobuf on 20,000 records as a generic tree, from `cargo run --release -- binary`.
`rmp-serde` (through `serde_json::Value`, also a generic tree) is the MessagePack reference. The protobuf
schema keeps the integer, bool and string fields, since this codec models no floats. The protobuf decode row
was measured on a schema that also carries an `int64` field, which is why its size is larger than the encode row's.

| operation | size (bytes) | before (ms) | now (ms) | instructions | `rmp-serde` (ms) |
|---|---:|---:|---:|---:|---:|
| MessagePack encode | 1,151,410 | 5.1 | 2.7 | -38% | 1.9 |
| MessagePack decode | 1,151,410 | 16.1 | 16.7 | identical | 14.4 |
| protobuf encode | 472,522 | 3.4 | 2.0 | -44% | - |
| protobuf decode | 549,178 | 15.1 | 14.1 | -5% | - |

What changed: the MessagePack encoder collected and sorted every object's keys into a new `Vec` (it now sorts
small objects on the stack) and built a `String` just to check that a float text round-trips (it now decides
that from the text for plain short decimals and uses a stack buffer otherwise). The protobuf encoder encoded
each nested message into a temporary `Vec` and copied it into its parent, and rebuilt and sorted its field list on
every call; it now encodes in place and patches the length prefix. The decoders were already close to their
floor: the remaining cost is one allocation per key and scalar, which the owned term requires. The MessagePack
decoder is unchanged (the same instruction count), and the protobuf decode figures are 5% fewer instructions and 7% less time in a paired run, a gain close to the
run-to-run noise.

Encoded bytes are identical to before: compared on over a million random terms and corrupted inputs, including
nested messages past 128 bytes and 16 KB where the length prefix grows to two and three bytes, and millions of
decimal-number texts.

## Number formatting

Why wirefold does not use `ryu` or `itoa` ([DESIGN.md](DESIGN.md#numbers-why-not-ryu-and-itoa)): JSON and YAML keep a
number's text verbatim, so there is no formatting step. Cost of the alternatives, 1,000,000 values, mean of 3
runs (`cargo run --release -- numbers`):

| operation | ns/value (range) |
|---|---:|
| f64 to text, std `Display` | 82.8 (81.2 to 84.9) |
| f64 to text, `ryu` | 44.3 (37.7 to 56.1) |
| u64 to text, std `Display` | 20.2 (19.1 to 21.2) |
| u64 to text, `itoa` | 7.2 (5.9 to 9.1) |
| text to f64, std `parse` | 10.1 (9.3 to 11.0) |
| text kept as it arrived (`Atom::Num`: one copy) | 11.1 (10.9 to 11.3) |

`ryu` is about 1.9 times and `itoa` about 2.8 times faster than std. The one place wirefold formats numbers is
the MessagePack and protobuf decoders, which still use std; that is the measured cost it leaves on the table.
Keeping the text also costs the same as parsing it (11.1 against 10.1 ns), so it saves nothing there; what it buys
is exactness, since `1.0`, `1e3` and `0.10` survive unchanged.

## Limits of these numbers

- One run on one shared virtual machine (the parsing table is the median of 15 runs on the same machine), so expect noise of several percent; differences under about 10%
  (bincode against Collapse, for example) should be read as ties.
- One record shape. Records with many strings, deep nesting or optional fields behave differently.
- The JSON parsing comparison is parser against parser, each building a generic tree. serde_json can also
  deserialise straight into a typed struct, which wirefold does not do, so that case is not measured.
- The wirefold and rkyv column reads include the open step. A loop that opens once and reads many columns
  amortises it.
- zstd is measured at levels 1, 3, 6 and 19; other levels and zstd dictionaries are not.
- Stage times under about 1 ms vary between processes (allocator and page-fault effects: wirefold's serialize
  stage on the random data was 0.2 to 0.8 ms across rows doing the same work), so differences of a few tenths
  of a millisecond between serializers are not established.
- The encodings, the per-column frames, the thread split and the block store are benchmark code (see
  [Layout, columns and blocks](#layout-columns-and-blocks)); the bf16 weights are synthetic.
- Every serializer here round-trips through a `Vec<u8>`; a caller that keeps rkyv or `Collapse` bytes in place
  avoids the copies counted in those pipelines.

## Reproduce

```bash
cd bench
cargo run --release -- 20000        # serialization tables: [records] [runs per cell, default 3]
cargo run --release -- numbers       # ryu / itoa against std and against keeping the text
cargo run --release -- planes ordered 20000    # the per-column table (also: random)
cargo run --release -- levels ordered 20000    # every zstd level, whole buffer against columns
cargo run --release -- blocks ordered 20000    # the block store (try 1000000 records)
cargo run --release -- bf16 4000000            # synthetic bf16 weights
```

## Earlier benchmarks

Before the split, the codec's benchmarks lived in the `hashtrinity` examples
(`codec_shootout` against pcodec, `fsst_strings` against FSST, `random_access`, `shaped_bench`) and in
`hashtrinity-bench` (`scripts/bench.sh` against lz4, zstd, brotli and xz, `scripts/bench_compress.sh` for the
ratio and throughput frontier). They are not reproduced here and no figures from them are claimed. The
comparison against serde and the other serialization libraries in this file was written fresh: when it was
written, no serde, bincode or rkyv harness turned up in `hashtrinity-bench` or the other repositories
searched, and the codec's comments about a gap to rkyv's `to_bytes_in` refer to measurements that were not
found. If such a harness exists elsewhere, its results belong here.
