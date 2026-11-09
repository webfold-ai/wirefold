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
- **Encoding each column for its type roughly halves it again, and that is a property of columns, not of
  `Collapse`.** Delta coding, packing integers to their exact bit width, storing decimal floats as scaled integers
  and dictionary-coding the strings, each column then compressed with zstd on its own, gives 105,790 bytes at zstd -1
  on the telemetry records (5.3 bytes per record, 2.3 ms to encode, 2.0 ms to decode) and 79,795 on the sensor records
  (the encoder tries several candidate forms per column, which is why its encode takes 2 to 8 ms). The other
  serializers get no such treatment here, because their layouts are rows: they would have to be transposed into
  columns first, and then the same encoders would apply to them as well. So this is a comparison of a column layout
  with encodings against row layouts with plain zstd, which is where the size comes from, and not a claim that
  `Collapse`'s wire format beats theirs. The encodings are in the benchmark, not the library; an application gets
  the plain-zstd sizes above unless it writes them. See [Layout, columns and blocks](#layout-columns-and-blocks),
  which has the per-column, per-level and per-block-size breakdowns. On the random data there is little to find: the
  column pipelines land at about 590,000 bytes, 6% below whole-buffer zstd -3 (624,300) and 14% below
  bincode + zstd -3 (683,707).
- **A text format can be the smallest row format.** On the sensor records `serde_json` + zstd -19 is 164,149 bytes,
  smaller than every binary row format (bincode 198,768, postcard 198,944, rkyv 217,627) and slightly smaller than
  `Collapse` + zstd -19 (166,445), because the JSON text repeats long runs that zstd matches; it takes 885 ms to encode
  and 5.8 ms to decode. The encoded columns are 79,795 bytes at zstd -1, 51% smaller than that.
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

- 20,000 records of one shape (`u64`, `u32`, `u8`, `f64`, `bool`, `String`), in three synthetic datasets, all
  generated in `bench/src/main.rs`: telemetry-shaped (increasing timestamps, uniform small domains, six repeated
  names), random (full-range integers and floats, random-looking names), which has no structure for a column store
  to exploit, and sensor-shaped (regular ticks with jitter, skewed device ids, a category that changes in bursts, a
  random-walk float with three decimals, rare faults, 200 distinct names).
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

20,000 records in each of three datasets. The rows are the pipelines described under Setup; `raw` is the serializer
alone. The `columns` rows split the buffer into its columns and are described under
[Layout, columns and blocks](#layout-columns-and-blocks).

#### Telemetry-shaped records (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 713212 | 35.7 | 0.25 (0.20 to 0.28) | 0.25 | 0.00 | 1.38 (1.25 to 1.51) | 0.00 | 1.38 | 3 (1.4) | 20001 (1.0) |
| wirefold Collapse + zstd -1 | 199392 | 10.0 | 2.71 (2.32 to 3.40) | 0.25 | 2.46 | 2.53 (2.20 to 3.19) | 1.08 | 1.45 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -3 | 189754 | 9.5 | 3.86 (3.16 to 4.98) | 0.25 | 3.61 | 2.55 (2.10 to 3.36) | 1.07 | 1.48 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -6 | 180132 | 9.0 | 10.97 (8.70 to 13.70) | 0.27 | 10.69 | 2.60 (2.11 to 3.53) | 1.10 | 1.50 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -19 | 167132 | 8.4 | 257.74 (241.84 to 269.87) | 0.35 | 257.40 | 2.51 (2.34 to 2.83) | 1.18 | 1.33 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + wirefold compress() | 222540 | 11.1 | 93.60 (91.57 to 95.04) | 0.31 | 93.28 | 4.81 (4.16 to 5.82) | 2.90 | 1.91 | 1412 (34.1) | 20052 (3.3) |
| wirefold Collapse + columns, zstd -1 | 178207 | 8.9 | 3.75 (3.17 to 4.23) | 0.64 | 3.11 | 2.47 (2.38 to 2.58) | 1.16 | 1.30 | 59 (4.1) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -3 | 167109 | 8.4 | 3.94 (3.52 to 4.38) | 0.26 | 3.68 | 2.55 (2.34 to 2.80) | 1.19 | 1.36 | 59 (4.1) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -6 | 163356 | 8.2 | 10.15 (8.61 to 11.99) | 0.25 | 9.90 | 2.43 (2.16 to 2.86) | 1.10 | 1.33 | 58 (4.0) | 20055 (5.3) |
| wirefold Collapse + columns, zstd -19 | 151261 | 7.6 | 664.58 (653.26 to 671.29) | 0.33 | 664.25 | 2.35 (2.25 to 2.51) | 1.05 | 1.30 | 58 (4.0) | 20055 (5.3) |
| wirefold Collapse + columns, encoded planes, zstd -1 | 105790 | 5.3 | 3.73 (3.61 to 3.80) | 0.22 | 3.52 | 2.08 (2.05 to 2.10) | 0.81 | 1.28 | 240 (8.3) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -3 | 107638 | 5.4 | 4.16 (3.81 to 4.44) | 0.22 | 3.94 | 2.08 (2.03 to 2.11) | 0.82 | 1.26 | 240 (8.3) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -6 | 106939 | 5.3 | 5.04 (4.92 to 5.17) | 0.22 | 4.82 | 2.28 (2.04 to 2.73) | 0.89 | 1.39 | 240 (8.3) | 20091 (4.9) |
| wirefold Collapse + columns, encoded planes, zstd -19 | 104729 | 5.2 | 326.20 (315.87 to 331.64) | 0.31 | 325.88 | 2.10 (1.88 to 2.34) | 0.78 | 1.32 | 240 (8.3) | 20091 (4.9) |
| wirefold Collapse + columns, byte shuffle, zstd -3 | 201596 | 10.1 | 2.01 (1.97 to 2.05) | 0.22 | 1.80 | 2.45 (2.37 to 2.52) | 1.20 | 1.26 | 64 (4.5) | 20055 (5.3) |
| bincode 1.3 raw | 713060 | 35.7 | 0.19 (0.18 to 0.21) | 0.19 | 0.00 | 0.96 (0.92 to 1.02) | 0.00 | 0.96 | 1 (0.7) | 20001 (1.0) |
| bincode 1.3 + zstd -1 | 234749 | 11.7 | 2.51 (2.49 to 2.56) | 0.20 | 2.31 | 1.83 (1.77 to 1.87) | 0.90 | 0.93 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -3 | 219402 | 11.0 | 3.48 (2.90 to 4.51) | 0.21 | 3.27 | 1.71 (1.68 to 1.73) | 0.81 | 0.90 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -6 | 203248 | 10.2 | 8.25 (8.05 to 8.53) | 0.20 | 8.05 | 1.59 (1.57 to 1.62) | 0.69 | 0.89 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -19 | 199079 | 10.0 | 262.59 (247.60 to 286.70) | 0.31 | 262.28 | 1.86 (1.82 to 1.93) | 0.91 | 0.95 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + wirefold compress() | 238005 | 11.9 | 54.62 (50.71 to 61.96) | 0.26 | 54.36 | 3.44 (3.07 to 4.04) | 2.04 | 1.40 | 895 (20.0) | 20052 (3.2) |
| postcard raw | 492525 | 24.6 | 0.46 (0.45 to 0.47) | 0.46 | 0.00 | 1.14 (1.11 to 1.15) | 0.00 | 1.14 | 17 (1.0) | 20001 (1.0) |
| postcard + zstd -1 | 263942 | 13.2 | 2.24 (2.18 to 2.28) | 0.45 | 1.79 | 1.86 (1.83 to 1.91) | 0.74 | 1.12 | 23 (2.0) | 20009 (2.1) |
| postcard + zstd -3 | 207314 | 10.4 | 2.99 (2.87 to 3.09) | 0.43 | 2.55 | 1.80 (1.72 to 1.90) | 0.73 | 1.07 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -6 | 195023 | 9.8 | 7.68 (7.29 to 8.04) | 0.46 | 7.22 | 2.02 (1.69 to 2.58) | 0.73 | 1.29 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -19 | 194386 | 9.7 | 184.86 (174.09 to 190.29) | 0.54 | 184.32 | 1.92 (1.72 to 2.04) | 0.84 | 1.09 | 22 (1.5) | 20009 (2.1) |
| postcard + wirefold compress() | 215656 | 10.8 | 53.71 (53.14 to 54.52) | 0.55 | 53.16 | 3.08 (2.92 to 3.18) | 1.64 | 1.44 | 953 (21.7) | 20031 (2.5) |
| rmp-serde (MessagePack) raw | 611506 | 30.6 | 0.72 (0.56 to 0.99) | 0.72 | 0.00 | 2.03 (1.72 to 2.63) | 0.00 | 2.03 | 18 (2.0) | 20001 (1.0) |
| rmp-serde (MessagePack) + zstd -1 | 227528 | 11.4 | 2.60 (2.12 to 3.51) | 0.71 | 1.89 | 2.31 (2.20 to 2.41) | 0.64 | 1.67 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -3 | 220582 | 11.0 | 2.78 (2.74 to 2.84) | 0.57 | 2.21 | 2.30 (2.25 to 2.37) | 0.61 | 1.69 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -6 | 215972 | 10.8 | 7.55 (7.24 to 8.11) | 0.58 | 6.97 | 2.43 (2.39 to 2.49) | 0.66 | 1.77 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -19 | 202583 | 10.1 | 189.38 (181.40 to 200.96) | 0.67 | 188.70 | 2.42 (2.27 to 2.59) | 0.64 | 1.78 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + wirefold compress() | 220043 | 11.0 | 52.97 (50.81 to 54.23) | 0.60 | 52.37 | 3.08 (2.90 to 3.23) | 0.90 | 2.18 | 911 (21.6) | 20031 (2.2) |
| rkyv 0.8 (validated) raw | 800008 | 40.0 | 0.28 (0.25 to 0.34) | 0.28 | 0.00 | 1.21 (0.97 to 1.61) | 0.00 | 1.21 | 16 (2.8) | 20002 (2.0) |
| rkyv 0.8 (validated) + zstd -1 | 273202 | 13.7 | 3.19 (2.59 to 4.19) | 0.29 | 2.91 | 3.18 (2.77 to 3.99) | 1.70 | 1.48 | 22 (3.8) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -3 | 225765 | 11.3 | 2.99 (2.86 to 3.12) | 0.27 | 2.72 | 1.88 (1.77 to 2.02) | 0.89 | 1.00 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -6 | 220282 | 11.0 | 11.05 (9.31 to 14.15) | 0.28 | 10.77 | 2.08 (1.79 to 2.60) | 0.92 | 1.16 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -19 | 187121 | 9.4 | 293.25 (291.76 to 294.14) | 0.41 | 292.84 | 2.09 (2.08 to 2.10) | 0.95 | 1.14 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + wirefold compress() | 218413 | 10.9 | 135.44 (131.47 to 139.71) | 0.37 | 135.07 | 2.93 (2.91 to 2.94) | 1.39 | 1.54 | 1428 (30.0) | 20032 (3.4) |
| serde_json raw | 1646252 | 82.3 | 2.65 (2.26 to 3.26) | 2.65 | 0.00 | 6.05 (4.75 to 8.45) | 0.00 | 6.05 | 15 (4.0) | 20014 (3.1) |
| serde_json + zstd -1 | 260706 | 13.0 | 7.35 (6.18 to 8.85) | 2.59 | 4.76 | 9.21 (7.53 to 12.44) | 2.52 | 6.70 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -3 | 265783 | 13.3 | 7.71 (6.59 to 9.80) | 2.39 | 5.32 | 6.18 (5.98 to 6.37) | 1.30 | 4.89 | 21 (5.0) | 20024 (7.2) |
| serde_json + zstd -6 | 240484 | 12.0 | 18.04 (17.57 to 18.29) | 2.08 | 15.96 | 5.88 (5.74 to 5.97) | 1.09 | 4.80 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -19 | 201563 | 10.1 | 901.83 (882.24 to 932.41) | 2.19 | 899.64 | 5.70 (5.40 to 5.96) | 0.96 | 4.74 | 20 (4.5) | 20024 (7.2) |
| serde_json + wirefold compress() | 244651 | 12.2 | 61.12 (59.02 to 65.11) | 2.34 | 58.79 | 6.60 (6.41 to 6.75) | 1.42 | 5.18 | 893 (26.5) | 20044 (5.3) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold open only (validates the table) | 0.35 (0.34 to 0.35) |
| wirefold, view already open: per-element accessor | 0.01 (0.01 to 0.01) |
| wirefold, view already open: column slice | 0.01 (0.01 to 0.01) |
| wirefold view (open + per-element accessor) | 0.35 (0.33 to 0.37) |
| wirefold view (open + column slice) | 0.35 (0.34 to 0.35) |
| rkyv 0.8 (validate + read) | 0.23 (0.21 to 0.25) |
| bincode (decode all, then sum) | 0.88 (0.85 to 0.91) |
| postcard (decode all, then sum) | 1.08 (1.03 to 1.14) |
| whole buffer zstd -3: decompress all, then read | 1.21 (1.21 to 1.22) |
| columns, zstd -3: decompress one column | 0.20 (0.20 to 0.20) |
| columns, encoded planes, zstd -3: decompress one column | 0.15 (0.15 to 0.15) |

#### Random records (no structure to exploit) (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 918792 | 45.9 | 0.70 (0.69 to 0.72) | 0.66 | 0.04 | 1.08 (1.06 to 1.09) | 0.00 | 1.08 | 3 (1.8) | 20001 (1.2) |
| wirefold Collapse + zstd -1 | 613402 | 30.7 | 2.09 (2.06 to 2.13) | 0.43 | 1.65 | 1.72 (1.70 to 1.74) | 0.59 | 1.13 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -3 | 624300 | 31.2 | 3.12 (3.08 to 3.16) | 0.22 | 2.90 | 1.84 (1.81 to 1.88) | 0.72 | 1.12 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -6 | 617234 | 30.9 | 6.67 (6.41 to 7.14) | 0.23 | 6.44 | 1.84 (1.82 to 1.87) | 0.70 | 1.14 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -19 | 624034 | 31.2 | 182.47 (176.96 to 192.03) | 0.35 | 182.12 | 2.27 (2.13 to 2.35) | 1.13 | 1.13 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + wirefold compress() | 664841 | 33.2 | 151.11 (147.82 to 153.18) | 0.31 | 150.80 | 3.04 (2.94 to 3.10) | 1.49 | 1.55 | 1303 (50.1) | 20045 (4.8) |
| wirefold Collapse + columns, zstd -1 | 591178 | 29.6 | 2.72 (2.21 to 3.21) | 0.76 | 1.96 | 2.16 (1.89 to 2.67) | 0.84 | 1.33 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -3 | 590820 | 29.5 | 2.79 (2.69 to 2.89) | 0.23 | 2.56 | 1.89 (1.83 to 1.94) | 0.76 | 1.14 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -6 | 593085 | 29.7 | 7.01 (6.67 to 7.27) | 0.22 | 6.78 | 2.09 (1.82 to 2.62) | 0.81 | 1.28 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, zstd -19 | 585556 | 29.3 | 463.26 (454.07 to 481.10) | 0.34 | 462.92 | 1.78 (1.72 to 1.84) | 0.61 | 1.17 | 65 (6.8) | 20057 (6.7) |
| wirefold Collapse + columns, encoded planes, zstd -1 | 591198 | 29.6 | 5.92 (5.03 to 7.61) | 0.24 | 5.68 | 2.05 (1.96 to 2.13) | 0.94 | 1.11 | 229 (16.1) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -3 | 590840 | 29.5 | 6.77 (6.64 to 6.91) | 0.23 | 6.54 | 2.09 (2.00 to 2.16) | 0.95 | 1.13 | 229 (16.1) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -6 | 593105 | 29.7 | 10.97 (10.39 to 12.01) | 0.24 | 10.73 | 2.13 (2.07 to 2.22) | 0.97 | 1.16 | 229 (16.1) | 20074 (7.3) |
| wirefold Collapse + columns, encoded planes, zstd -19 | 585576 | 29.3 | 471.48 (458.42 to 487.71) | 0.35 | 471.13 | 2.05 (1.97 to 2.11) | 0.88 | 1.17 | 229 (16.1) | 20074 (7.3) |
| wirefold Collapse + columns, byte shuffle, zstd -3 | 589031 | 29.5 | 4.18 (3.67 to 5.16) | 0.25 | 3.93 | 2.47 (2.37 to 2.54) | 1.28 | 1.19 | 70 (7.2) | 20057 (6.7) |
| bincode 1.3 raw | 918640 | 45.9 | 0.19 (0.18 to 0.21) | 0.19 | 0.00 | 0.81 (0.80 to 0.84) | 0.00 | 0.81 | 1 (0.9) | 20001 (1.2) |
| bincode 1.3 + zstd -1 | 678272 | 33.9 | 5.07 (3.62 to 6.17) | 0.26 | 4.81 | 2.75 (2.47 to 2.99) | 1.48 | 1.27 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -3 | 683707 | 34.2 | 7.19 (5.40 to 9.42) | 0.26 | 6.93 | 2.51 (2.19 to 3.03) | 1.47 | 1.04 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -6 | 686931 | 34.3 | 15.94 (12.94 to 18.55) | 0.25 | 15.69 | 2.12 (2.06 to 2.22) | 1.31 | 0.81 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -19 | 673404 | 33.7 | 231.34 (219.11 to 243.63) | 0.32 | 231.02 | 2.47 (2.15 to 2.96) | 1.49 | 0.98 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + wirefold compress() | 660097 | 33.0 | 76.57 (70.72 to 84.46) | 0.29 | 76.29 | 4.88 (4.40 to 5.53) | 3.49 | 1.39 | 1015 (36.4) | 20031 (4.1) |
| postcard raw | 827235 | 41.4 | 0.53 (0.52 to 0.54) | 0.53 | 0.00 | 1.15 (1.12 to 1.17) | 0.00 | 1.15 | 18 (2.0) | 20001 (1.2) |
| postcard + zstd -1 | 740567 | 37.0 | 2.32 (2.15 to 2.44) | 0.73 | 1.59 | 2.32 (2.24 to 2.36) | 1.10 | 1.22 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -3 | 740578 | 37.0 | 4.10 (2.85 to 5.01) | 0.68 | 3.42 | 2.62 (2.23 to 3.23) | 1.16 | 1.46 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -6 | 729758 | 36.5 | 10.50 (9.46 to 12.37) | 0.55 | 9.95 | 2.55 (2.48 to 2.69) | 1.28 | 1.27 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -19 | 692567 | 34.6 | 167.68 (162.37 to 171.52) | 0.58 | 167.10 | 2.82 (2.76 to 2.91) | 1.60 | 1.22 | 25 (4.0) | 20010 (3.3) |
| postcard + wirefold compress() | 670280 | 33.5 | 75.05 (70.04 to 81.80) | 0.62 | 74.43 | 5.25 (4.21 to 7.08) | 3.46 | 1.79 | 1055 (45.6) | 20008 (3.1) |
| rmp-serde (MessagePack) raw | 868558 | 43.4 | 0.94 (0.79 to 1.18) | 0.94 | 0.00 | 2.25 (1.89 to 2.84) | 0.00 | 2.25 | 18 (2.0) | 20001 (1.2) |
| rmp-serde (MessagePack) + zstd -1 | 764693 | 38.2 | 2.89 (2.81 to 3.02) | 1.04 | 1.85 | 3.65 (3.16 to 4.19) | 1.24 | 2.40 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -3 | 760886 | 38.0 | 5.48 (5.41 to 5.57) | 0.77 | 4.71 | 3.17 (3.16 to 3.19) | 1.29 | 1.88 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -6 | 744722 | 37.2 | 14.87 (11.71 to 20.24) | 0.92 | 13.95 | 3.42 (3.11 to 3.95) | 1.47 | 1.95 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -19 | 710596 | 35.5 | 230.00 (217.54 to 253.08) | 0.90 | 229.11 | 4.06 (3.51 to 5.13) | 1.93 | 2.13 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + wirefold compress() | 676350 | 33.8 | 77.47 (71.45 to 86.72) | 1.01 | 76.47 | 5.54 (5.39 to 5.78) | 3.24 | 2.29 | 914 (39.1) | 20008 (3.2) |
| rkyv 0.8 (validated) raw | 1118640 | 55.9 | 0.46 (0.45 to 0.49) | 0.46 | 0.00 | 1.32 (1.23 to 1.40) | 0.00 | 1.32 | 19 (5.1) | 20002 (3.2) |
| rkyv 0.8 (validated) + zstd -1 | 700458 | 35.0 | 5.75 (4.88 to 6.57) | 1.08 | 4.67 | 3.32 (3.01 to 3.81) | 1.84 | 1.48 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -3 | 702764 | 35.1 | 6.86 (6.50 to 7.15) | 0.35 | 6.51 | 2.58 (2.53 to 2.67) | 1.61 | 0.97 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -6 | 705922 | 35.3 | 17.63 (16.83 to 19.20) | 0.40 | 17.23 | 2.42 (2.21 to 2.82) | 1.45 | 0.96 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -19 | 647127 | 32.4 | 276.71 (256.15 to 311.55) | 0.56 | 276.15 | 2.04 (1.96 to 2.18) | 1.09 | 0.94 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + wirefold compress() | 715062 | 35.8 | 133.56 (125.80 to 137.60) | 0.51 | 133.05 | 6.08 (5.38 to 7.34) | 4.55 | 1.54 | 1533 (56.5) | 20032 (6.5) |
| serde_json raw | 2461362 | 123.1 | 2.42 (2.40 to 2.44) | 2.42 | 0.00 | 6.52 (6.28 to 6.81) | 0.00 | 6.52 | 16 (8.0) | 20149 (3.3) |
| serde_json + zstd -1 | 831395 | 41.6 | 12.04 (10.55 to 14.52) | 2.74 | 9.30 | 11.28 (9.36 to 15.10) | 2.92 | 8.36 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -3 | 917434 | 45.9 | 20.29 (19.77 to 20.97) | 3.18 | 17.12 | 13.09 (10.78 to 17.55) | 4.75 | 8.35 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -6 | 872907 | 43.6 | 43.70 (42.98 to 44.10) | 3.07 | 40.63 | 11.16 (8.96 to 15.47) | 3.24 | 7.92 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -19 | 749105 | 37.5 | 1115.10 (1061.47 to 1167.37) | 2.59 | 1112.51 | 8.55 (8.42 to 8.62) | 2.24 | 6.30 | 23 (10.0) | 20160 (11.4) |
| serde_json + wirefold compress() | 817820 | 40.9 | 137.69 (130.80 to 149.39) | 2.52 | 135.17 | 11.14 (10.96 to 11.29) | 4.41 | 6.73 | 1251 (60.8) | 20220 (9.1) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold open only (validates the table) | 0.37 (0.33 to 0.39) |
| wirefold, view already open: per-element accessor | 0.01 (0.01 to 0.01) |
| wirefold, view already open: column slice | 0.01 (0.01 to 0.01) |
| wirefold view (open + per-element accessor) | 0.35 (0.31 to 0.39) |
| wirefold view (open + column slice) | 0.32 (0.32 to 0.32) |
| rkyv 0.8 (validate + read) | 0.09 (0.09 to 0.09) |
| bincode (decode all, then sum) | 0.78 (0.76 to 0.79) |
| postcard (decode all, then sum) | 1.51 (1.11 to 1.76) |
| whole buffer zstd -3: decompress all, then read | 1.28 (1.03 to 1.41) |
| columns, zstd -3: decompress one column | 0.04 (0.04 to 0.05) |
| columns, encoded planes, zstd -3: decompress one column | 0.05 (0.05 to 0.06) |

#### Sensor-shaped records, smooth (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 785445 | 39.3 | 0.19 (0.19 to 0.20) | 0.19 | 0.00 | 1.20 (1.15 to 1.23) | 0.00 | 1.20 | 3 (1.5) | 20001 (1.1) |
| wirefold Collapse + zstd -1 | 206250 | 10.3 | 2.79 (2.71 to 2.91) | 0.20 | 2.59 | 2.58 (2.45 to 2.82) | 1.23 | 1.36 | 8 (2.0) | 20010 (3.2) |
| wirefold Collapse + zstd -3 | 188083 | 9.4 | 4.52 (3.37 to 6.79) | 0.27 | 4.25 | 2.53 (2.34 to 2.84) | 1.13 | 1.41 | 8 (2.0) | 20010 (3.2) |
| wirefold Collapse + zstd -6 | 179924 | 9.0 | 10.86 (9.37 to 13.82) | 0.24 | 10.62 | 2.39 (2.23 to 2.59) | 1.04 | 1.35 | 8 (2.0) | 20010 (3.2) |
| wirefold Collapse + zstd -19 | 166445 | 8.3 | 254.82 (250.32 to 261.71) | 0.33 | 254.49 | 2.42 (2.40 to 2.45) | 1.13 | 1.29 | 8 (2.0) | 20010 (3.2) |
| wirefold Collapse + wirefold compress() | 206860 | 10.3 | 187.01 (184.83 to 188.85) | 0.34 | 186.67 | 4.54 (4.41 to 4.68) | 2.86 | 1.68 | 1667 (34.3) | 20053 (3.4) |
| wirefold Collapse + columns, zstd -1 | 186456 | 9.3 | 4.03 (3.59 to 4.86) | 0.65 | 3.39 | 3.37 (2.52 to 3.82) | 1.66 | 1.72 | 60 (4.4) | 20056 (5.8) |
| wirefold Collapse + columns, zstd -3 | 168063 | 8.4 | 3.73 (3.17 to 4.68) | 0.23 | 3.50 | 2.80 (2.37 to 3.54) | 1.32 | 1.48 | 60 (4.4) | 20056 (5.8) |
| wirefold Collapse + columns, zstd -6 | 163624 | 8.2 | 11.71 (9.54 to 14.23) | 0.23 | 11.47 | 2.91 (2.39 to 3.52) | 1.30 | 1.61 | 58 (4.2) | 20056 (5.8) |
| wirefold Collapse + columns, zstd -19 | 150503 | 7.5 | 697.22 (682.09 to 707.02) | 0.31 | 696.91 | 2.35 (2.23 to 2.46) | 1.05 | 1.30 | 58 (4.2) | 20056 (5.8) |
| wirefold Collapse + columns, encoded planes, zstd -1 | 79795 | 4.0 | 7.52 (7.38 to 7.73) | 0.20 | 7.32 | 2.00 (1.96 to 2.04) | 0.78 | 1.22 | 245 (7.9) | 20112 (5.9) |
| wirefold Collapse + columns, encoded planes, zstd -3 | 80337 | 4.0 | 7.89 (7.84 to 7.95) | 0.21 | 7.69 | 2.08 (2.05 to 2.13) | 0.82 | 1.27 | 245 (7.9) | 20112 (5.9) |
| wirefold Collapse + columns, encoded planes, zstd -6 | 79491 | 4.0 | 10.49 (9.37 to 12.52) | 0.25 | 10.24 | 2.65 (2.24 to 3.17) | 0.98 | 1.67 | 245 (7.9) | 20112 (5.9) |
| wirefold Collapse + columns, encoded planes, zstd -19 | 78824 | 3.9 | 347.57 (345.20 to 351.44) | 0.33 | 347.24 | 2.21 (2.02 to 2.35) | 0.86 | 1.35 | 245 (7.9) | 20112 (5.9) |
| wirefold Collapse + columns, byte shuffle, zstd -3 | 203804 | 10.2 | 2.83 (2.64 to 3.09) | 0.25 | 2.59 | 3.04 (2.81 to 3.33) | 1.45 | 1.58 | 65 (4.9) | 20056 (5.8) |
| bincode 1.3 raw | 785293 | 39.3 | 0.21 (0.17 to 0.24) | 0.21 | 0.00 | 1.20 (0.99 to 1.62) | 0.00 | 1.20 | 1 (0.7) | 20001 (1.1) |
| bincode 1.3 + zstd -1 | 238662 | 11.9 | 2.88 (2.60 to 3.41) | 0.21 | 2.66 | 2.11 (1.71 to 2.75) | 1.00 | 1.11 | 6 (1.2) | 20010 (3.2) |
| bincode 1.3 + zstd -3 | 230676 | 11.5 | 3.88 (3.36 to 4.76) | 0.22 | 3.66 | 2.29 (1.94 to 2.91) | 1.17 | 1.12 | 6 (1.2) | 20010 (3.2) |
| bincode 1.3 + zstd -6 | 216260 | 10.8 | 9.70 (9.53 to 9.91) | 0.22 | 9.48 | 2.06 (1.69 to 2.68) | 1.00 | 1.07 | 6 (1.2) | 20010 (3.2) |
| bincode 1.3 + zstd -19 | 198768 | 9.9 | 277.98 (268.10 to 294.63) | 0.31 | 277.67 | 1.88 (1.66 to 2.01) | 1.00 | 0.88 | 6 (1.2) | 20010 (3.2) |
| bincode 1.3 + wirefold compress() | 258074 | 12.9 | 76.36 (74.06 to 79.51) | 0.25 | 76.11 | 2.66 (2.57 to 2.77) | 1.30 | 1.36 | 1066 (23.0) | 20031 (2.5) |
| postcard raw | 558258 | 27.9 | 0.56 (0.50 to 0.59) | 0.56 | 0.00 | 1.34 (1.13 to 1.45) | 0.00 | 1.34 | 18 (2.0) | 20001 (1.1) |
| postcard + zstd -1 | 280982 | 14.0 | 2.46 (2.20 to 2.88) | 0.49 | 1.97 | 2.12 (1.95 to 2.44) | 0.83 | 1.29 | 24 (3.0) | 20010 (3.2) |
| postcard + zstd -3 | 220752 | 11.0 | 3.16 (3.09 to 3.26) | 0.44 | 2.72 | 1.98 (1.97 to 2.00) | 0.82 | 1.17 | 23 (2.5) | 20010 (3.2) |
| postcard + zstd -6 | 208745 | 10.4 | 8.20 (7.81 to 8.56) | 0.43 | 7.76 | 1.96 (1.96 to 1.98) | 0.81 | 1.16 | 23 (2.5) | 20010 (3.2) |
| postcard + zstd -19 | 198944 | 9.9 | 195.89 (185.46 to 201.86) | 0.52 | 195.37 | 2.19 (2.03 to 2.46) | 0.96 | 1.23 | 23 (2.5) | 20010 (3.2) |
| postcard + wirefold compress() | 226519 | 11.3 | 62.84 (60.23 to 65.78) | 0.49 | 62.35 | 3.45 (3.09 to 3.97) | 1.92 | 1.54 | 885 (23.6) | 20031 (2.7) |
| rmp-serde (MessagePack) raw | 668232 | 33.4 | 0.87 (0.65 to 0.99) | 0.87 | 0.00 | 2.41 (1.89 to 2.72) | 0.00 | 2.41 | 18 (2.0) | 20001 (1.1) |
| rmp-serde (MessagePack) + zstd -1 | 257368 | 12.9 | 3.28 (2.81 to 4.08) | 0.79 | 2.50 | 3.03 (2.48 to 3.90) | 0.89 | 2.14 | 23 (2.5) | 20010 (3.2) |
| rmp-serde (MessagePack) + zstd -3 | 236076 | 11.8 | 3.47 (3.44 to 3.53) | 0.67 | 2.80 | 2.81 (2.60 to 3.03) | 0.87 | 1.94 | 23 (2.5) | 20010 (3.2) |
| rmp-serde (MessagePack) + zstd -6 | 227806 | 11.4 | 10.63 (8.51 to 14.33) | 0.81 | 9.82 | 3.03 (2.62 to 3.78) | 0.98 | 2.05 | 23 (2.5) | 20010 (3.2) |
| rmp-serde (MessagePack) + zstd -19 | 198008 | 9.9 | 281.10 (265.46 to 303.07) | 0.70 | 280.40 | 2.92 (2.77 to 3.10) | 1.07 | 1.85 | 23 (2.5) | 20010 (3.2) |
| rmp-serde (MessagePack) + wirefold compress() | 246722 | 12.3 | 51.66 (50.88 to 52.77) | 0.69 | 50.97 | 3.92 (3.54 to 4.35) | 1.46 | 2.46 | 914 (21.8) | 20031 (2.4) |
| rkyv 0.8 (validated) raw | 964200 | 48.2 | 0.46 (0.42 to 0.48) | 0.45 | 0.00 | 1.35 (1.30 to 1.38) | 0.00 | 1.35 | 18 (2.9) | 20002 (2.1) |
| rkyv 0.8 (validated) + zstd -1 | 315183 | 15.8 | 4.11 (3.60 to 4.80) | 0.36 | 3.75 | 3.33 (3.06 to 3.66) | 1.93 | 1.40 | 24 (3.9) | 20011 (4.2) |
| rkyv 0.8 (validated) + zstd -3 | 283612 | 14.2 | 4.48 (4.34 to 4.57) | 0.33 | 4.15 | 2.47 (2.38 to 2.59) | 1.43 | 1.04 | 24 (3.9) | 20011 (4.2) |
| rkyv 0.8 (validated) + zstd -6 | 267467 | 13.4 | 16.70 (15.04 to 19.62) | 0.41 | 16.29 | 2.78 (2.35 to 3.57) | 1.59 | 1.20 | 24 (3.9) | 20011 (4.2) |
| rkyv 0.8 (validated) + zstd -19 | 217627 | 10.9 | 366.81 (347.49 to 396.81) | 0.52 | 366.29 | 2.64 (2.33 to 3.15) | 1.46 | 1.17 | 23 (3.4) | 20011 (4.2) |
| rkyv 0.8 (validated) + wirefold compress() | 287467 | 14.4 | 148.51 (132.37 to 156.68) | 0.47 | 148.04 | 3.97 (3.63 to 4.63) | 2.36 | 1.61 | 1119 (25.9) | 20053 (4.2) |
| serde_json raw | 1713398 | 85.7 | 2.04 (1.98 to 2.13) | 2.04 | 0.00 | 4.81 (4.68 to 4.92) | 0.00 | 4.81 | 15 (4.0) | 20014 (3.2) |
| serde_json + zstd -1 | 252511 | 12.6 | 6.64 (6.31 to 6.99) | 2.36 | 4.28 | 7.40 (6.87 to 8.19) | 2.08 | 5.32 | 20 (4.5) | 20024 (7.3) |
| serde_json + zstd -3 | 258395 | 12.9 | 7.32 (6.16 to 9.24) | 2.34 | 4.98 | 5.95 (5.85 to 6.05) | 1.29 | 4.66 | 20 (4.5) | 20024 (7.3) |
| serde_json + zstd -6 | 230084 | 11.5 | 19.73 (16.48 to 24.66) | 2.40 | 17.32 | 6.11 (5.84 to 6.42) | 1.24 | 4.87 | 20 (4.5) | 20024 (7.3) |
| serde_json + zstd -19 | 164149 | 8.2 | 884.60 (844.75 to 926.55) | 2.08 | 882.52 | 5.81 (5.69 to 5.93) | 1.23 | 4.58 | 20 (4.5) | 20024 (7.3) |
| serde_json + wirefold compress() | 237146 | 11.9 | 62.61 (62.52 to 62.67) | 2.46 | 60.15 | 6.80 (6.67 to 7.00) | 1.83 | 4.97 | 917 (26.8) | 20065 (5.9) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold open only (validates the table) | 0.35 (0.34 to 0.36) |
| wirefold, view already open: per-element accessor | 0.01 (0.01 to 0.01) |
| wirefold, view already open: column slice | 0.01 (0.01 to 0.01) |
| wirefold view (open + per-element accessor) | 0.39 (0.37 to 0.44) |
| wirefold view (open + column slice) | 0.36 (0.35 to 0.37) |
| rkyv 0.8 (validate + read) | 0.18 (0.18 to 0.18) |
| bincode (decode all, then sum) | 1.13 (0.86 to 1.54) |
| postcard (decode all, then sum) | 1.42 (1.17 to 1.84) |
| whole buffer zstd -3: decompress all, then read | 1.36 (1.31 to 1.41) |
| columns, zstd -3: decompress one column | 0.22 (0.21 to 0.22) |
| columns, encoded planes, zstd -3: decompress one column | 0.14 (0.14 to 0.14) |

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

What the encodings reach, against what each column holds (the bounds follow from the generators in `bench/src/main.rs`: the draws are uniform). `ts` is increasing by 1 to 50, so its differences carry
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

#### Per column, sensor records (20000 records, 785445 bytes)

| plane | raw (B) | zstd -1 | zstd -3 | zstd -19 | encoded (B) | encoded, zstd -1 | zstd -3 | zstd -19 | encoding |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| ts (u64) | 160000 | 25212 | 25212 | 25250 | 40017 | 6038 | 6218 | 5546 | delta, integer |
| user (u32) | 80000 | 33069 | 33215 | 32523 | 25011 | 24492 | 24492 | 24521 | integer |
| kind (u8) | 20000 | 1839 | 2033 | 1368 | 20001 | 1840 | 2032 | 1356 | as stored |
| value (f64) | 160000 | 82465 | 66449 | 58888 | 27520 | 27529 | 27529 | 27529 | decimal scaled, integer |
| ok (bool) | 20000 | 357 | 515 | 389 | 20001 | 357 | 515 | 383 | as stored |
| name lengths | 160000 | 8879 | 5238 | 5948 | 20001 | 18857 | 18857 | 18872 | dictionary |
| name bytes | 185285 | 35183 | 34888 | 26127 | 2692 | 542 | 554 | 477 | dictionary |
| **total** | 785285 | 187004 | 167550 | 150493 | 155243 | 79655 | 80197 | 78684 | |

On the sensor records the random-walk `value` column is stored as the differences between neighbours of the scaled
integers (27.5 KB, against 45.0 KB for the absolute values and 66.4 KB for zstd on the floats), the timestamps as
differences (6.2 KB against 25.2 KB), and the 200 names as indices and a 2.7 KB dictionary. The `kind` and `ok` columns are
left as stored because zstd already reaches their entropy. The name-length column is the one place where the
dictionary is worse than not using one on its own (18.9 KB of indices against 5.2 KB of lengths) and better together
with the text (19.4 KB against 40.1 KB), so a dictionary is judged on both planes jointly.

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
- One record shape, in three synthetic datasets whose distributions come from the generators in `bench/src/main.rs`.
  Real data, records with many strings, deep nesting or optional fields behave differently.
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
- Every `Collapse` table now starts with a magic and a version (`TABLE_VERSION`, 1), written in the same 8-byte word
  as the section count, so the buffers in these tables are the same size as before and opening one costs the same
  (0.30 ms against 0.31 to 0.33). Buffers written before the header existed, and before the column layout, have no magic
  and are refused with `WireErr::BadMagic`, including the one-scalar-field records that the section count could
  not tell apart.
- The CI jobs, including the sanitizer jobs, have not run on GitHub (a billing limit rejects them before a runner is
  assigned); the same commands were run locally.
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
