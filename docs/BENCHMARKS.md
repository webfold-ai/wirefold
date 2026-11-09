# Benchmarks

wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd on the same records. The harness
is [`bench/`](../bench/README.md). The serialization tables come from the run described under Results; the [parsing](#parsing) numbers
from runs described there.

## Summary

Serialization and compression are separate stages, and the tables below time them separately: a pipeline is a
serializer, optionally followed by a compressor, and encode is serialize then compress while decode is
decompress then deserialize.

- **Serialization alone (the `raw` pipelines):** wirefold `Collapse` is the same size as bincode (713,116 against
  713,060 bytes for 20,000 telemetry records) and encodes in the same time (0.19 against 0.17 ms). It decodes
  slower than bincode (1.03 against 0.83 ms) and about level with postcard and rkyv (1.09 and 1.10 ms). Against
  serde_json it is about 11 times faster to encode and 4.6 times faster to decode, and 2.3 times smaller;
  MessagePack through rmp-serde is slower to encode (0.53 ms) and decode (1.66 ms) than `Collapse`. postcard is
  smaller (24.6 against 35.7 bytes per record) and rkyv larger (40.0). On the random data the picture is the
  same for sizes and rkyv decodes fastest (0.80 ms).
- **With a compressor, the compressor sets the time and the serializer sets the size.** Serializing takes
  0.2 to 0.6 ms (2 ms for JSON) whatever follows, so with zstd -3 the encode time is 2.7 to 3.1 ms for every binary
  serializer and almost all of it is zstd. What differs is the output: on the telemetry data after zstd -19, rkyv
  is smallest (187,121 bytes), then postcard (194,386), bincode (199,079), JSON (201,563), rmp-serde (202,583)
  and `Collapse` (207,470). `Collapse` as stored does not compress smaller than the row formats, because its scalar
  fields are one row-major array of packed records; compressed per field it does (next bullet). Decoding is
  1.6 to 3.2 ms for the binary formats and 5 to 7 ms for JSON, of which zstd decompression is 0.6 to 1.6 ms.
- **wirefold plus zstd, compressed per field:** splitting the scalar records into one plane per field and
  compressing each plane with zstd gives 166,713 bytes at zstd -3 (8.3 bytes per record) on the telemetry data, against
  234,791 for zstd on the whole buffer, 219,402 for bincode, 207,314 for postcard and 225,765 for rkyv at the same
  level, and below all of their zstd -19 sizes (151,213 bytes at -19). It costs 3.5 ms to encode and 2.1 ms to
  decode, the same as the others at -3, and reading one field decompresses only its plane: 0.18 ms for the `f64`
  field against 1.12 ms to decompress everything and then read. On the random data the saving is small (589,955
  against 628,869 bytes) because the values have no structure to find. The split is in the benchmark, not the
  library. See [Columns and zstd](#columns-and-zstd).
- **Reading one column without decoding the rest:** wirefold's validated open plus a column read is about
  4.5 times faster than bincode or postcard (decode everything, then read) and slower than rkyv: 0.30 against
  0.24 ms on the telemetry data and 0.35 against 0.10 ms on the random data.
- **wirefold's own coder (`compress()`, experimental):** after `Collapse` it gives 243,203 bytes and costs
  124.7 ms to encode and 3.6 ms to decode, against 237,083 bytes, 2.3 ms and 1.9 ms for zstd -1 (and 207,470
  bytes, 260 ms and 2.1 ms for zstd -19). It does not beat zstd at any level. Its cost depends on the input
  as well as the serializer: on the postcard, bincode and MessagePack bytes it encodes in about 49 ms.
  `Collapse` does not depend on it; if you want compression, compress the `Collapse` bytes with zstd.
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
  `compress()`. Each pipeline is round-trip checked before it is timed and runs in its own process. A run
  is the median of 9 repetitions (5 for `compress()`, 3 for zstd -19); the encode and decode totals are the mean
  of 3 runs with the range shown, and the stage columns are the mean of the same runs.
- Every serializer is measured as bytes out of the records and records back out of bytes. wirefold and rkyv go
  through a `Vec<u8>` (a copy each way) so that every pipeline has the same shape; rkyv needs aligned input, so its
  decode copies the bytes into an aligned buffer, counted in its deserialize stage.
- Allocations are the calls and MiB requested from the Rust allocator during one encode and one decode, counted
  by a wrapper around the system allocator. zstd's internal C allocations are not seen.

## Results

20,000 records. The rows are the pipelines described under Setup; `raw` is the serializer alone. The
`columns` rows are described under [Columns and zstd](#columns-and-zstd).

#### Telemetry-shaped records (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 713116 | 35.7 | 0.18 (0.18 to 0.19) | 0.18 | 0.00 | 1.03 (1.02 to 1.04) | 0.00 | 1.03 | 3 (1.4) | 20001 (1.0) |
| wirefold Collapse + zstd -1 | 237083 | 11.9 | 2.30 (2.25 to 2.33) | 0.19 | 2.11 | 2.10 (2.03 to 2.19) | 0.88 | 1.21 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -3 | 234791 | 11.7 | 3.06 (2.90 to 3.33) | 0.20 | 2.86 | 2.13 (2.00 to 2.33) | 0.94 | 1.19 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -6 | 218018 | 10.9 | 7.93 (7.49 to 8.59) | 0.20 | 7.73 | 1.97 (1.92 to 2.00) | 0.81 | 1.15 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -19 | 207470 | 10.4 | 238.40 (228.93 to 256.75) | 0.31 | 238.09 | 2.15 (2.10 to 2.22) | 0.93 | 1.22 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + wirefold compress() | 243203 | 12.2 | 129.34 (123.73 to 135.61) | 0.30 | 129.04 | 3.98 (3.80 to 4.08) | 2.36 | 1.62 | 1595 (33.9) | 20052 (3.3) |
| wirefold Collapse + columns, zstd -1 | 178831 | 8.9 | 3.76 (3.39 to 4.46) | 0.58 | 3.18 | 2.31 (2.08 to 2.42) | 1.10 | 1.21 | 108 (4.3) | 20044 (4.6) |
| wirefold Collapse + columns, zstd -3 | 166713 | 8.3 | 3.54 (3.38 to 3.81) | 0.20 | 3.35 | 2.11 (2.05 to 2.23) | 1.01 | 1.10 | 108 (4.2) | 20044 (4.6) |
| wirefold Collapse + columns, zstd -6 | 163277 | 8.2 | 10.17 (8.84 to 12.82) | 0.22 | 9.95 | 2.24 (2.16 to 2.33) | 1.04 | 1.21 | 107 (4.1) | 20044 (4.6) |
| wirefold Collapse + columns, zstd -19 | 151213 | 7.6 | 644.43 (629.58 to 663.00) | 0.30 | 644.12 | 2.56 (2.11 to 3.31) | 1.14 | 1.42 | 107 (4.1) | 20044 (4.6) |
| wirefold Collapse + columns, shuffle, zstd -1 | 200571 | 10.0 | 2.29 (2.21 to 2.38) | 0.50 | 1.79 | 2.11 (2.04 to 2.24) | 1.00 | 1.11 | 114 (4.8) | 20050 (5.2) |
| wirefold Collapse + columns, shuffle, zstd -3 | 201532 | 10.1 | 2.67 (2.18 to 3.22) | 0.22 | 2.44 | 2.86 (2.28 to 3.44) | 1.37 | 1.48 | 114 (4.8) | 20050 (5.2) |
| wirefold Collapse + columns, shuffle, zstd -6 | 197755 | 9.9 | 4.77 (4.59 to 5.06) | 0.20 | 4.57 | 2.27 (2.12 to 2.41) | 1.08 | 1.20 | 114 (4.8) | 20050 (5.2) |
| wirefold Collapse + columns, shuffle, zstd -19 | 178525 | 8.9 | 453.75 (445.23 to 463.80) | 0.29 | 453.46 | 2.52 (2.07 to 3.25) | 1.17 | 1.34 | 113 (4.7) | 20050 (5.2) |
| bincode 1.3 raw | 713060 | 35.7 | 0.16 (0.15 to 0.18) | 0.16 | 0.00 | 0.87 (0.83 to 0.90) | 0.00 | 0.87 | 1 (0.7) | 20001 (1.0) |
| bincode 1.3 + zstd -1 | 234749 | 11.7 | 2.72 (2.27 to 3.49) | 0.22 | 2.50 | 1.94 (1.63 to 2.52) | 0.94 | 0.99 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -3 | 219402 | 11.0 | 2.99 (2.92 to 3.04) | 0.20 | 2.80 | 1.75 (1.69 to 1.88) | 0.82 | 0.94 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -6 | 203248 | 10.2 | 8.54 (8.02 to 9.41) | 0.20 | 8.34 | 1.84 (1.61 to 2.26) | 0.81 | 1.02 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -19 | 199079 | 10.0 | 268.28 (254.12 to 278.80) | 0.32 | 267.96 | 1.70 (1.60 to 1.80) | 0.83 | 0.87 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + wirefold compress() | 238005 | 11.9 | 48.59 (46.09 to 51.58) | 0.25 | 48.35 | 3.18 (2.79 to 3.79) | 1.90 | 1.28 | 895 (20.0) | 20052 (3.2) |
| postcard raw | 492525 | 24.6 | 0.45 (0.39 to 0.51) | 0.45 | 0.00 | 1.02 (0.93 to 1.19) | 0.00 | 1.02 | 17 (1.0) | 20001 (1.0) |
| postcard + zstd -1 | 263942 | 13.2 | 2.06 (1.97 to 2.12) | 0.41 | 1.65 | 1.64 (1.58 to 1.72) | 0.67 | 0.97 | 23 (2.0) | 20009 (2.1) |
| postcard + zstd -3 | 207314 | 10.4 | 2.65 (2.63 to 2.68) | 0.40 | 2.25 | 1.61 (1.60 to 1.62) | 0.67 | 0.94 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -6 | 195023 | 9.8 | 7.06 (6.44 to 8.08) | 0.41 | 6.65 | 1.54 (1.49 to 1.62) | 0.57 | 0.96 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -19 | 194386 | 9.7 | 163.85 (162.08 to 164.98) | 0.48 | 163.37 | 2.38 (1.72 to 3.58) | 0.81 | 1.57 | 22 (1.5) | 20009 (2.1) |
| postcard + wirefold compress() | 215656 | 10.8 | 53.22 (50.76 to 57.48) | 0.47 | 52.74 | 3.38 (2.88 to 4.30) | 1.85 | 1.53 | 953 (21.7) | 20031 (2.5) |
| rmp-serde (MessagePack) raw | 611506 | 30.6 | 0.68 (0.58 to 0.80) | 0.68 | 0.00 | 1.91 (1.71 to 2.29) | 0.00 | 1.91 | 18 (2.0) | 20001 (1.0) |
| rmp-serde (MessagePack) + zstd -1 | 227528 | 11.4 | 2.20 (2.16 to 2.25) | 0.59 | 1.61 | 2.42 (2.34 to 2.58) | 0.66 | 1.76 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -3 | 220582 | 11.0 | 3.73 (2.58 to 4.36) | 0.79 | 2.94 | 2.69 (2.28 to 3.42) | 0.70 | 1.99 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -6 | 215972 | 10.8 | 7.89 (7.41 to 8.18) | 0.59 | 7.30 | 2.75 (2.60 to 2.84) | 0.75 | 2.00 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -19 | 202583 | 10.1 | 177.84 (170.81 to 188.42) | 0.68 | 177.17 | 2.27 (2.07 to 2.39) | 0.59 | 1.69 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + wirefold compress() | 220043 | 11.0 | 53.16 (51.62 to 55.29) | 0.63 | 52.53 | 3.14 (2.95 to 3.32) | 0.95 | 2.19 | 911 (21.6) | 20031 (2.2) |
| rkyv 0.8 (validated) raw | 800008 | 40.0 | 0.23 (0.23 to 0.23) | 0.23 | 0.00 | 1.02 (0.95 to 1.09) | 0.00 | 1.02 | 16 (2.8) | 20002 (2.0) |
| rkyv 0.8 (validated) + zstd -1 | 273202 | 13.7 | 2.84 (2.41 to 3.64) | 0.26 | 2.59 | 2.82 (2.49 to 3.22) | 1.53 | 1.29 | 22 (3.8) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -3 | 225765 | 11.3 | 2.77 (2.64 to 2.99) | 0.24 | 2.53 | 1.95 (1.80 to 2.08) | 0.91 | 1.03 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -6 | 220282 | 11.0 | 11.18 (9.27 to 13.70) | 0.28 | 10.90 | 2.16 (1.84 to 2.73) | 0.96 | 1.20 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -19 | 187121 | 9.4 | 290.31 (287.27 to 294.62) | 0.40 | 289.90 | 2.01 (1.88 to 2.12) | 0.91 | 1.10 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + wirefold compress() | 218413 | 10.9 | 133.25 (124.44 to 145.80) | 0.33 | 132.92 | 2.77 (2.76 to 2.77) | 1.33 | 1.43 | 1428 (30.0) | 20032 (3.4) |
| serde_json raw | 1646252 | 82.3 | 2.17 (2.00 to 2.34) | 2.17 | 0.00 | 4.83 (4.57 to 5.02) | 0.00 | 4.83 | 15 (4.0) | 20014 (3.1) |
| serde_json + zstd -1 | 260706 | 13.0 | 6.01 (5.91 to 6.07) | 2.10 | 3.90 | 7.11 (6.91 to 7.30) | 2.01 | 5.10 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -3 | 265783 | 13.3 | 7.84 (6.89 to 9.35) | 2.45 | 5.38 | 6.55 (5.96 to 7.54) | 1.32 | 5.23 | 21 (5.0) | 20024 (7.2) |
| serde_json + zstd -6 | 240484 | 12.0 | 17.38 (16.93 to 17.61) | 2.05 | 15.33 | 5.32 (5.24 to 5.42) | 0.98 | 4.34 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -19 | 201563 | 10.1 | 877.27 (850.42 to 929.84) | 2.20 | 875.07 | 7.14 (5.80 to 9.70) | 1.17 | 5.97 | 20 (4.5) | 20024 (7.2) |
| serde_json + wirefold compress() | 244651 | 12.2 | 54.34 (52.78 to 57.28) | 1.95 | 52.40 | 5.78 (5.68 to 5.94) | 1.25 | 4.53 | 893 (26.5) | 20044 (5.3) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.28 (0.28 to 0.30) |
| rkyv 0.8 (validate + read) | 0.23 (0.20 to 0.27) |
| bincode (decode all, then sum) | 0.80 (0.77 to 0.84) |
| postcard (decode all, then sum) | 0.98 (0.91 to 1.08) |
| whole buffer zstd -3: decompress all, then read | 1.12 (1.09 to 1.15) |
| columns, zstd -3: decompress one column | 0.18 (0.18 to 0.19) |
| columns, shuffle, zstd -3: decompress one column | 0.31 (0.28 to 0.33) |

#### Random records (no structure to exploit) (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 918696 | 45.9 | 0.73 (0.62 to 0.81) | 0.68 | 0.05 | 1.09 (0.93 to 1.39) | 0.00 | 1.09 | 3 (1.8) | 20001 (1.2) |
| wirefold Collapse + zstd -1 | 627123 | 31.4 | 2.43 (2.11 to 3.05) | 0.40 | 2.03 | 1.71 (1.46 to 2.18) | 0.55 | 1.15 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -3 | 628869 | 31.4 | 3.09 (3.02 to 3.15) | 0.18 | 2.91 | 1.58 (1.55 to 1.60) | 0.59 | 0.99 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -6 | 630628 | 31.5 | 6.29 (6.01 to 6.78) | 0.18 | 6.11 | 1.62 (1.58 to 1.68) | 0.62 | 1.00 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -19 | 634681 | 31.7 | 176.49 (161.49 to 206.11) | 0.35 | 176.14 | 2.41 (2.30 to 2.63) | 1.38 | 1.03 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + wirefold compress() | 635363 | 31.8 | 149.30 (144.86 to 154.95) | 0.28 | 149.02 | 4.88 (4.27 to 5.78) | 3.39 | 1.49 | 1363 (47.7) | 20032 (4.8) |
| wirefold Collapse + columns, zstd -1 | 591107 | 29.6 | 2.46 (2.35 to 2.53) | 0.47 | 2.00 | 2.43 (2.38 to 2.49) | 1.22 | 1.21 | 114 (6.7) | 20046 (5.8) |
| wirefold Collapse + columns, zstd -3 | 589955 | 29.5 | 3.05 (2.91 to 3.18) | 0.18 | 2.87 | 1.98 (1.88 to 2.11) | 0.78 | 1.20 | 114 (6.7) | 20046 (5.8) |
| wirefold Collapse + columns, zstd -6 | 592205 | 29.6 | 7.86 (6.45 to 9.56) | 0.20 | 7.66 | 2.04 (1.82 to 2.42) | 0.83 | 1.22 | 114 (6.7) | 20046 (5.8) |
| wirefold Collapse + columns, zstd -19 | 584535 | 29.2 | 471.89 (459.50 to 493.05) | 0.31 | 471.58 | 1.66 (1.59 to 1.79) | 0.58 | 1.09 | 114 (6.7) | 20046 (5.8) |
| wirefold Collapse + columns, shuffle, zstd -1 | 583550 | 29.2 | 2.52 (2.47 to 2.59) | 0.60 | 1.91 | 1.99 (1.82 to 2.15) | 0.90 | 1.09 | 120 (7.3) | 20052 (6.4) |
| wirefold Collapse + columns, shuffle, zstd -3 | 588165 | 29.4 | 4.34 (3.75 to 5.49) | 0.21 | 4.13 | 2.46 (2.10 to 3.14) | 1.21 | 1.25 | 120 (7.3) | 20052 (6.4) |
| wirefold Collapse + columns, shuffle, zstd -6 | 589409 | 29.5 | 8.18 (6.62 to 9.90) | 0.23 | 7.95 | 2.47 (1.90 to 3.36) | 1.23 | 1.24 | 120 (7.2) | 20052 (6.4) |
| wirefold Collapse + columns, shuffle, zstd -19 | 582007 | 29.1 | 446.42 (427.80 to 478.48) | 0.32 | 446.10 | 2.08 (1.86 to 2.34) | 0.98 | 1.09 | 120 (7.2) | 20052 (6.4) |
| bincode 1.3 raw | 918640 | 45.9 | 0.20 (0.17 to 0.22) | 0.20 | 0.00 | 0.89 (0.74 to 1.15) | 0.00 | 0.89 | 1 (0.9) | 20001 (1.2) |
| bincode 1.3 + zstd -1 | 678272 | 33.9 | 3.31 (3.29 to 3.32) | 0.18 | 3.13 | 1.91 (1.84 to 1.99) | 1.15 | 0.76 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -3 | 683707 | 34.2 | 5.85 (5.02 to 6.97) | 0.22 | 5.63 | 1.98 (1.93 to 2.02) | 1.22 | 0.76 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -6 | 686931 | 34.3 | 12.48 (12.40 to 12.55) | 0.21 | 12.28 | 2.01 (1.92 to 2.06) | 1.23 | 0.78 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -19 | 673404 | 33.7 | 230.88 (216.71 to 248.78) | 0.30 | 230.58 | 1.98 (1.89 to 2.11) | 1.23 | 0.75 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + wirefold compress() | 660097 | 33.0 | 64.21 (61.63 to 68.44) | 0.27 | 63.94 | 4.25 (3.92 to 4.57) | 3.06 | 1.19 | 1015 (36.4) | 20031 (4.1) |
| postcard raw | 827235 | 41.4 | 0.48 (0.46 to 0.51) | 0.48 | 0.00 | 1.14 (0.96 to 1.43) | 0.00 | 1.14 | 18 (2.0) | 20001 (1.2) |
| postcard + zstd -1 | 740567 | 37.0 | 2.41 (2.26 to 2.54) | 0.71 | 1.69 | 2.18 (2.10 to 2.24) | 1.05 | 1.14 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -3 | 740578 | 37.0 | 3.19 (2.87 to 3.72) | 0.53 | 2.66 | 2.26 (2.22 to 2.29) | 1.11 | 1.16 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -6 | 729758 | 36.5 | 9.71 (9.06 to 10.68) | 0.51 | 9.19 | 2.47 (2.13 to 2.88) | 1.28 | 1.19 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -19 | 692567 | 34.6 | 168.20 (164.26 to 172.15) | 0.55 | 167.65 | 2.88 (2.51 to 3.29) | 1.73 | 1.15 | 25 (4.0) | 20010 (3.3) |
| postcard + wirefold compress() | 670280 | 33.5 | 67.67 (64.58 to 70.31) | 0.61 | 67.05 | 4.86 (4.12 to 6.25) | 3.23 | 1.63 | 1055 (45.6) | 20008 (3.1) |
| rmp-serde (MessagePack) raw | 868558 | 43.4 | 0.95 (0.80 to 1.07) | 0.95 | 0.00 | 2.70 (2.62 to 2.77) | 0.00 | 2.70 | 18 (2.0) | 20001 (1.2) |
| rmp-serde (MessagePack) + zstd -1 | 764693 | 38.2 | 3.11 (2.65 to 3.97) | 1.16 | 1.95 | 3.25 (2.83 to 4.01) | 1.15 | 2.10 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -3 | 760886 | 38.0 | 5.53 (5.16 to 6.27) | 0.77 | 4.76 | 3.12 (3.10 to 3.16) | 1.23 | 1.89 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -6 | 744722 | 37.2 | 14.45 (10.86 to 21.39) | 0.88 | 13.57 | 3.11 (2.88 to 3.45) | 1.31 | 1.80 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -19 | 710596 | 35.5 | 185.67 (177.79 to 192.49) | 0.79 | 184.88 | 3.29 (3.07 to 3.52) | 1.63 | 1.66 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + wirefold compress() | 676350 | 33.8 | 66.26 (62.45 to 68.60) | 0.85 | 65.41 | 5.22 (4.66 to 5.64) | 2.99 | 2.23 | 914 (39.1) | 20008 (3.2) |
| rkyv 0.8 (validated) raw | 1118640 | 55.9 | 0.36 (0.35 to 0.36) | 0.36 | 0.00 | 0.83 (0.81 to 0.87) | 0.00 | 0.83 | 19 (5.1) | 20002 (3.2) |
| rkyv 0.8 (validated) + zstd -1 | 700458 | 35.0 | 4.88 (3.97 to 6.16) | 0.96 | 3.92 | 2.99 (2.55 to 3.82) | 1.68 | 1.31 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -3 | 702764 | 35.1 | 6.27 (6.05 to 6.51) | 0.32 | 5.95 | 2.37 (2.23 to 2.46) | 1.50 | 0.87 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -6 | 705922 | 35.3 | 14.81 (14.19 to 15.73) | 0.32 | 14.49 | 2.22 (1.96 to 2.73) | 1.33 | 0.89 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -19 | 647127 | 32.4 | 244.54 (231.47 to 270.58) | 0.51 | 244.02 | 1.89 (1.82 to 1.97) | 0.96 | 0.93 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + wirefold compress() | 715062 | 35.8 | 120.68 (117.12 to 127.34) | 0.49 | 120.20 | 5.00 (4.65 to 5.51) | 3.76 | 1.25 | 1533 (56.5) | 20032 (6.5) |
| serde_json raw | 2461362 | 123.1 | 2.45 (2.29 to 2.75) | 2.45 | 0.00 | 5.84 (5.71 to 5.96) | 0.00 | 5.84 | 16 (8.0) | 20149 (3.3) |
| serde_json + zstd -1 | 831395 | 41.6 | 9.18 (9.02 to 9.27) | 2.20 | 6.98 | 8.98 (8.11 to 10.11) | 2.46 | 6.52 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -3 | 917434 | 45.9 | 17.93 (17.43 to 18.26) | 2.85 | 15.08 | 10.38 (10.16 to 10.72) | 3.80 | 6.58 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -6 | 872907 | 43.6 | 42.41 (39.38 to 47.70) | 2.99 | 39.42 | 8.83 (8.39 to 9.28) | 2.72 | 6.11 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -19 | 749105 | 37.5 | 1101.99 (1071.62 to 1134.06) | 2.64 | 1099.34 | 8.85 (7.62 to 10.23) | 2.47 | 6.39 | 23 (10.0) | 20160 (11.4) |
| serde_json + wirefold compress() | 817820 | 40.9 | 130.73 (124.66 to 135.80) | 2.91 | 127.82 | 12.78 (9.77 to 18.63) | 4.63 | 8.15 | 1251 (60.8) | 20220 (9.1) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.29 (0.28 to 0.30) |
| rkyv 0.8 (validate + read) | 0.09 (0.09 to 0.10) |
| bincode (decode all, then sum) | 0.77 (0.76 to 0.77) |
| postcard (decode all, then sum) | 1.09 (1.06 to 1.14) |
| whole buffer zstd -3: decompress all, then read | 0.86 (0.83 to 0.89) |
| columns, zstd -3: decompress one column | 0.04 (0.04 to 0.05) |
| columns, shuffle, zstd -3: decompress one column | 0.19 (0.16 to 0.25) |

## Columns and zstd

What `Collapse` stores is a section table: the scalar fields of the records as one array of packed records (for
this `Event`, 22 bytes each: `ts` 8, `user` 4, `kind` 1, `value` 8, `ok` 1, row after row), then each
variable-length field as its own column (the strings, as offsets and bytes). So the fixed-width fields are row-major, and
reading one of them strides through the record array.

The `columns` rows take that buffer and split the record array into one plane per field (5 planes), turn the string
offsets into lengths (so the strings are a lengths plane and a bytes plane), compress each of the 7 planes as its own zstd
frame, and rebuild the exact buffer on decode (checked against the original before timing). The `shuffle` rows also
byte-shuffle each multi-byte plane first. The split and rebuild are in `bench/src/main.rs` and know this record's
field offsets; zstd stays out of the library.

| telemetry, 20,000 records | zstd -1 | zstd -3 | zstd -19 |
|---|---:|---:|---:|
| `Collapse` as stored | 237,083 | 234,791 | 207,470 |
| per-field planes | 178,831 | 166,713 | 151,213 |
| per-field planes, byte-shuffled | 200,571 | 201,532 | 178,525 |

Sizes in bytes. Per-field planes beat the whole buffer by 25% to 29% on this data, and also beat bincode, postcard, rkyv
and MessagePack through zstd at every level, because each plane holds one field's values (increasing timestamps, small
domains, repeated flags) and zstd finds structure that the interleaved rows hide. Byte-shuffling the planes made them
larger here, so it is not used for the headline numbers. The cost is the split and rebuild, which is inside the 3.5 ms
encode and 2.1 ms decode. The column read in the Results table (decompress one plane, then sum) is what a column store
gives that zstd on rows cannot: the other planes stay compressed.

A layout that stores the scalar fields as contiguous columns in the first place would make each plane a slice of the
buffer, so no split or rebuild step would be needed; that is a format change and is not part of these numbers.

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
- Every serializer here round-trips through a `Vec<u8>`; a caller that keeps rkyv or `Collapse` bytes in place
  avoids the copies counted in those pipelines.

## Reproduce

```bash
cd bench
cargo run --release -- 20000        # serialization tables: [records] [runs per cell, default 3]
cargo run --release -- numbers       # ryu / itoa against std and against keeping the text
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
