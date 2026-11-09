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
  and `Collapse` (207,470). The columnar layout does not produce smaller zstd output on this data. Decoding is
  1.6 to 3.2 ms for the binary formats and 5 to 7 ms for JSON, of which zstd decompression is 0.6 to 1.6 ms.
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
apeiron, a columnar layout whose open step validates without walking the rows, one `Term` that reads and
writes JSON, YAML, TOON, POML, MessagePack and others, and an experimental dependency-free coder.

## Setup

- 20,000 records of one shape (`u64`, `u32`, `u8`, `f64`, `bool`, `String`), in two datasets: telemetry-shaped
  (increasing timestamps, small domains, six repeated names) and random (full-range integers and floats,
  random-looking names), which is the worst case for a columnar model.
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

20,000 records. The rows are the pipelines described under Setup; `raw` is the serializer alone.

#### Telemetry-shaped records (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 713116 | 35.7 | 0.19 (0.18 to 0.20) | 0.19 | 0.00 | 1.03 (1.02 to 1.05) | 0.00 | 1.03 | 3 (1.4) | 20001 (1.0) |
| wirefold Collapse + zstd -1 | 237083 | 11.9 | 2.26 (2.14 to 2.32) | 0.19 | 2.06 | 1.94 (1.89 to 2.00) | 0.83 | 1.11 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -3 | 234791 | 11.7 | 2.73 (2.65 to 2.84) | 0.19 | 2.54 | 2.07 (2.00 to 2.12) | 0.91 | 1.16 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -6 | 218018 | 10.9 | 8.88 (7.62 to 10.91) | 0.21 | 8.67 | 1.99 (1.83 to 2.19) | 0.84 | 1.15 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + zstd -19 | 207470 | 10.4 | 260.24 (244.62 to 282.79) | 0.32 | 259.92 | 2.07 (1.95 to 2.22) | 0.89 | 1.17 | 8 (1.9) | 20010 (3.1) |
| wirefold Collapse + wirefold compress() | 243203 | 12.2 | 124.66 (119.63 to 129.74) | 0.28 | 124.38 | 3.62 (3.41 to 3.89) | 2.08 | 1.54 | 1595 (33.9) | 20052 (3.3) |
| bincode 1.3 raw | 713060 | 35.7 | 0.17 (0.16 to 0.19) | 0.17 | 0.00 | 0.83 (0.80 to 0.87) | 0.00 | 0.83 | 1 (0.7) | 20001 (1.0) |
| bincode 1.3 + zstd -1 | 234749 | 11.7 | 2.73 (2.39 to 3.36) | 0.21 | 2.52 | 1.81 (1.72 to 1.97) | 0.87 | 0.94 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -3 | 219402 | 11.0 | 3.03 (2.58 to 3.87) | 0.19 | 2.84 | 1.57 (1.52 to 1.65) | 0.75 | 0.82 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -6 | 203248 | 10.2 | 7.79 (7.58 to 8.07) | 0.19 | 7.60 | 1.86 (1.57 to 2.25) | 0.84 | 1.02 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + zstd -19 | 199079 | 10.0 | 272.69 (246.95 to 286.76) | 0.30 | 272.40 | 2.08 (1.76 to 2.62) | 1.01 | 1.07 | 6 (1.2) | 20010 (3.1) |
| bincode 1.3 + wirefold compress() | 238005 | 11.9 | 49.37 (48.06 to 50.58) | 0.25 | 49.13 | 3.77 (3.14 to 4.11) | 2.24 | 1.53 | 895 (20.0) | 20052 (3.2) |
| postcard raw | 492525 | 24.6 | 0.49 (0.43 to 0.52) | 0.49 | 0.00 | 1.09 (1.03 to 1.14) | 0.00 | 1.09 | 17 (1.0) | 20001 (1.0) |
| postcard + zstd -1 | 263942 | 13.2 | 2.09 (2.00 to 2.15) | 0.43 | 1.66 | 1.70 (1.65 to 1.73) | 0.70 | 1.00 | 23 (2.0) | 20009 (2.1) |
| postcard + zstd -3 | 207314 | 10.4 | 2.91 (2.89 to 2.92) | 0.44 | 2.47 | 1.73 (1.63 to 1.78) | 0.71 | 1.02 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -6 | 195023 | 9.8 | 8.99 (7.19 to 10.82) | 0.50 | 8.50 | 1.56 (1.50 to 1.63) | 0.59 | 0.97 | 22 (1.5) | 20009 (2.1) |
| postcard + zstd -19 | 194386 | 9.7 | 180.59 (166.15 to 192.49) | 0.53 | 180.06 | 2.03 (1.68 to 2.60) | 0.87 | 1.16 | 22 (1.5) | 20009 (2.1) |
| postcard + wirefold compress() | 215656 | 10.8 | 49.13 (46.13 to 54.06) | 0.51 | 48.62 | 3.44 (2.63 to 4.64) | 1.90 | 1.54 | 953 (21.7) | 20031 (2.5) |
| rmp-serde (MessagePack) raw | 611506 | 30.6 | 0.53 (0.51 to 0.55) | 0.53 | 0.00 | 1.66 (1.54 to 1.76) | 0.00 | 1.66 | 18 (2.0) | 20001 (1.0) |
| rmp-serde (MessagePack) + zstd -1 | 227528 | 11.4 | 2.02 (1.95 to 2.14) | 0.53 | 1.49 | 2.13 (2.11 to 2.15) | 0.58 | 1.55 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -3 | 220582 | 11.0 | 3.07 (2.69 to 3.82) | 0.60 | 2.47 | 2.25 (2.11 to 2.41) | 0.60 | 1.65 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -6 | 215972 | 10.8 | 8.73 (6.67 to 10.61) | 0.65 | 8.09 | 2.20 (2.18 to 2.24) | 0.59 | 1.61 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + zstd -19 | 202583 | 10.1 | 170.70 (165.93 to 174.92) | 0.58 | 170.12 | 2.56 (2.13 to 2.81) | 0.77 | 1.80 | 23 (2.5) | 20010 (3.1) |
| rmp-serde (MessagePack) + wirefold compress() | 220043 | 11.0 | 52.91 (49.74 to 58.97) | 0.57 | 52.34 | 3.31 (2.67 to 4.46) | 1.03 | 2.28 | 911 (21.6) | 20031 (2.2) |
| rkyv 0.8 (validated) raw | 800008 | 40.0 | 0.26 (0.26 to 0.27) | 0.26 | 0.00 | 1.10 (1.06 to 1.17) | 0.00 | 1.10 | 16 (2.8) | 20002 (2.0) |
| rkyv 0.8 (validated) + zstd -1 | 273202 | 13.7 | 3.08 (2.69 to 3.84) | 0.28 | 2.80 | 3.15 (2.63 to 3.81) | 1.64 | 1.51 | 22 (3.8) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -3 | 225765 | 11.3 | 3.00 (2.96 to 3.07) | 0.27 | 2.72 | 1.92 (1.77 to 2.00) | 0.90 | 1.01 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -6 | 220282 | 11.0 | 10.35 (9.05 to 12.81) | 0.27 | 10.07 | 2.09 (1.72 to 2.63) | 0.93 | 1.16 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + zstd -19 | 187121 | 9.4 | 297.13 (288.75 to 308.25) | 0.41 | 296.72 | 2.13 (1.92 to 2.43) | 0.96 | 1.17 | 21 (3.3) | 20011 (4.1) |
| rkyv 0.8 (validated) + wirefold compress() | 218413 | 10.9 | 126.12 (122.87 to 129.14) | 0.34 | 125.77 | 3.11 (2.65 to 3.86) | 1.52 | 1.59 | 1428 (30.0) | 20032 (3.4) |
| serde_json raw | 1646252 | 82.3 | 2.07 (2.04 to 2.15) | 2.07 | 0.00 | 4.77 (4.71 to 4.79) | 0.00 | 4.77 | 15 (4.0) | 20014 (3.1) |
| serde_json + zstd -1 | 260706 | 13.0 | 7.35 (5.65 to 8.26) | 2.62 | 4.72 | 7.23 (6.40 to 8.20) | 2.03 | 5.20 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -3 | 265783 | 13.3 | 6.40 (6.21 to 6.66) | 1.96 | 4.44 | 5.75 (5.70 to 5.84) | 1.20 | 4.56 | 21 (5.0) | 20024 (7.2) |
| serde_json + zstd -6 | 240484 | 12.0 | 17.64 (17.34 to 17.90) | 2.00 | 15.64 | 6.29 (5.09 to 7.88) | 1.15 | 5.14 | 20 (4.5) | 20024 (7.2) |
| serde_json + zstd -19 | 201563 | 10.1 | 883.70 (856.50 to 913.03) | 2.05 | 881.65 | 5.45 (5.07 to 5.89) | 0.97 | 4.48 | 20 (4.5) | 20024 (7.2) |
| serde_json + wirefold compress() | 244651 | 12.2 | 54.95 (51.46 to 58.96) | 2.13 | 52.82 | 6.17 (5.89 to 6.34) | 1.31 | 4.85 | 893 (26.5) | 20044 (5.3) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.30 (0.30 to 0.31) |
| rkyv 0.8 (validate + read) | 0.24 (0.20 to 0.30) |
| bincode (decode all, then sum) | 1.37 (1.33 to 1.40) |
| postcard (decode all, then sum) | 1.47 (1.05 to 1.71) |

#### Random records (worst case for a columnar model) (20000 records)

Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.

| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| wirefold Collapse raw | 918696 | 45.9 | 0.84 (0.69 to 0.94) | 0.78 | 0.06 | 1.43 (1.03 to 1.71) | 0.00 | 1.43 | 3 (1.8) | 20001 (1.2) |
| wirefold Collapse + zstd -1 | 627123 | 31.4 | 2.81 (2.47 to 3.30) | 0.47 | 2.35 | 1.97 (1.77 to 2.26) | 0.65 | 1.32 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -3 | 628869 | 31.4 | 3.81 (3.57 to 4.00) | 0.20 | 3.60 | 1.82 (1.80 to 1.83) | 0.68 | 1.14 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -6 | 630628 | 31.5 | 7.30 (6.60 to 8.22) | 0.22 | 7.09 | 1.84 (1.80 to 1.86) | 0.70 | 1.14 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + zstd -19 | 634681 | 31.7 | 186.26 (172.19 to 197.43) | 0.35 | 185.90 | 3.03 (2.61 to 3.78) | 1.75 | 1.28 | 10 (3.8) | 20010 (3.3) |
| wirefold Collapse + wirefold compress() | 635363 | 31.8 | 148.80 (145.02 to 153.57) | 0.28 | 148.52 | 5.24 (4.60 to 6.45) | 3.64 | 1.60 | 1363 (47.7) | 20032 (4.8) |
| bincode 1.3 raw | 918640 | 45.9 | 0.26 (0.19 to 0.33) | 0.26 | 0.00 | 1.08 (0.83 to 1.26) | 0.00 | 1.08 | 1 (0.9) | 20001 (1.2) |
| bincode 1.3 + zstd -1 | 678272 | 33.9 | 3.48 (3.22 to 3.69) | 0.20 | 3.28 | 2.01 (1.98 to 2.06) | 1.20 | 0.81 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -3 | 683707 | 34.2 | 5.99 (5.01 to 7.12) | 0.22 | 5.77 | 2.10 (2.04 to 2.14) | 1.29 | 0.81 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -6 | 686931 | 34.3 | 12.99 (12.04 to 13.57) | 0.22 | 12.77 | 2.06 (1.90 to 2.18) | 1.26 | 0.80 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + zstd -19 | 673404 | 33.7 | 244.10 (219.95 to 266.25) | 0.33 | 243.76 | 2.13 (1.90 to 2.37) | 1.33 | 0.79 | 8 (2.9) | 20010 (3.3) |
| bincode 1.3 + wirefold compress() | 660097 | 33.0 | 70.51 (63.88 to 77.55) | 0.30 | 70.21 | 5.12 (4.38 to 6.42) | 3.69 | 1.44 | 1015 (36.4) | 20031 (4.1) |
| postcard raw | 827235 | 41.4 | 0.56 (0.49 to 0.61) | 0.56 | 0.00 | 1.12 (1.10 to 1.14) | 0.00 | 1.12 | 18 (2.0) | 20001 (1.2) |
| postcard + zstd -1 | 740567 | 37.0 | 2.39 (2.23 to 2.57) | 0.74 | 1.66 | 2.11 (2.00 to 2.17) | 1.03 | 1.08 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -3 | 740578 | 37.0 | 2.90 (2.52 to 3.25) | 0.50 | 2.40 | 2.13 (2.06 to 2.19) | 1.05 | 1.08 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -6 | 729758 | 36.5 | 9.76 (9.52 to 10.14) | 0.53 | 9.23 | 2.55 (2.28 to 3.04) | 1.28 | 1.27 | 25 (4.0) | 20010 (3.3) |
| postcard + zstd -19 | 692567 | 34.6 | 163.30 (160.82 to 164.85) | 0.55 | 162.75 | 2.53 (2.37 to 2.77) | 1.48 | 1.05 | 25 (4.0) | 20010 (3.3) |
| postcard + wirefold compress() | 670280 | 33.5 | 71.27 (65.67 to 81.76) | 0.61 | 70.66 | 3.96 (3.81 to 4.07) | 2.63 | 1.32 | 1055 (45.6) | 20008 (3.1) |
| rmp-serde (MessagePack) raw | 868558 | 43.4 | 0.68 (0.66 to 0.70) | 0.68 | 0.00 | 1.58 (1.55 to 1.63) | 0.00 | 1.58 | 18 (2.0) | 20001 (1.2) |
| rmp-serde (MessagePack) + zstd -1 | 764693 | 38.2 | 2.70 (2.32 to 3.42) | 0.98 | 1.72 | 2.75 (2.62 to 2.82) | 1.05 | 1.69 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -3 | 760886 | 38.0 | 4.88 (4.73 to 5.05) | 0.69 | 4.19 | 2.96 (2.81 to 3.07) | 1.19 | 1.77 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -6 | 744722 | 37.2 | 11.89 (11.58 to 12.18) | 0.75 | 11.14 | 2.98 (2.91 to 3.04) | 1.28 | 1.70 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + zstd -19 | 710596 | 35.5 | 190.56 (174.59 to 208.54) | 0.78 | 189.79 | 3.45 (3.40 to 3.50) | 1.66 | 1.79 | 25 (4.0) | 20010 (3.3) |
| rmp-serde (MessagePack) + wirefold compress() | 676350 | 33.8 | 74.28 (64.77 to 88.08) | 0.81 | 73.47 | 4.96 (4.69 to 5.24) | 2.87 | 2.09 | 914 (39.1) | 20008 (3.2) |
| rkyv 0.8 (validated) raw | 1118640 | 55.9 | 0.35 (0.34 to 0.37) | 0.35 | 0.00 | 0.80 (0.80 to 0.81) | 0.00 | 0.80 | 19 (5.1) | 20002 (3.2) |
| rkyv 0.8 (validated) + zstd -1 | 700458 | 35.0 | 4.52 (4.09 to 5.30) | 0.90 | 3.63 | 2.66 (2.63 to 2.70) | 1.51 | 1.16 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -3 | 702764 | 35.1 | 6.17 (6.10 to 6.22) | 0.33 | 5.85 | 2.46 (2.42 to 2.54) | 1.56 | 0.90 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -6 | 705922 | 35.3 | 16.08 (15.55 to 16.66) | 0.36 | 15.72 | 2.24 (2.16 to 2.35) | 1.33 | 0.91 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + zstd -19 | 647127 | 32.4 | 276.23 (253.01 to 309.33) | 0.53 | 275.71 | 1.93 (1.86 to 2.02) | 1.01 | 0.92 | 26 (7.1) | 20012 (7.3) |
| rkyv 0.8 (validated) + wirefold compress() | 715062 | 35.8 | 120.03 (118.88 to 120.97) | 0.50 | 119.54 | 4.88 (4.73 to 5.01) | 3.60 | 1.27 | 1533 (56.5) | 20032 (6.5) |
| serde_json raw | 2461362 | 123.1 | 2.77 (2.15 to 3.55) | 2.77 | 0.00 | 6.63 (6.35 to 7.11) | 0.00 | 6.63 | 16 (8.0) | 20149 (3.3) |
| serde_json + zstd -1 | 831395 | 41.6 | 12.29 (10.29 to 13.37) | 3.02 | 9.27 | 10.80 (9.22 to 13.93) | 2.76 | 8.04 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -3 | 917434 | 45.9 | 19.53 (18.37 to 20.15) | 3.05 | 16.48 | 11.18 (10.76 to 12.01) | 4.28 | 6.90 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -6 | 872907 | 43.6 | 42.20 (40.37 to 44.80) | 2.89 | 39.31 | 9.04 (8.54 to 9.75) | 2.68 | 6.35 | 23 (10.0) | 20160 (11.4) |
| serde_json + zstd -19 | 749105 | 37.5 | 1119.69 (1062.29 to 1197.68) | 2.58 | 1117.11 | 12.06 (8.03 to 15.34) | 2.81 | 9.25 | 23 (10.0) | 20160 (11.4) |
| serde_json + wirefold compress() | 817820 | 40.9 | 134.08 (130.64 to 138.90) | 2.60 | 131.48 | 14.72 (11.05 to 17.68) | 5.56 | 9.15 | 1251 (60.8) | 20220 (9.1) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.35 (0.29 to 0.39) |
| rkyv 0.8 (validate + read) | 0.10 (0.10 to 0.10) |
| bincode (decode all, then sum) | 1.10 (0.87 to 1.31) |
| postcard (decode all, then sum) | 1.35 (1.15 to 1.73) |

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
