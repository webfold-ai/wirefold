# Benchmarks

wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd on the same records. The harness
is [`bench/`](../bench/README.md); every number below comes from one run of it.

## Summary

- **Plain encode and decode:** wirefold `Collapse` is the same as bincode (3,566,664 against 3,566,608
  bytes, about 1 ms to encode and 5 ms to decode 100,000 records). It is not faster than bincode, rkyv or
  postcard at decoding everything. It is about 2.3 times smaller than serde_json, about 10 times faster to
  encode and about 4 times faster to decode. postcard is smaller (2.46 MB against 3.57 MB) and takes about
  twice as long to encode.
- **Reading one column without decoding the rest:** wirefold's validated open plus a column read is about
  3 to 4 times faster than bincode or postcard (decode everything, then read) and slower than rkyv: 1.5 ms
  against 1.2 ms on the telemetry data and 1.6 ms against 0.5 ms on the random data.
- **Size with an entropy coder:** wirefold's context-mixing coder produced the smallest output, 21% below
  `postcard + zstd -19` and 26% below `Collapse + zstd -19` on the telemetry data, and 5% below
  `Collapse + zstd -19` on random data. It costs 44 to 72 seconds to encode 100,000 records (zstd -19:
  about 1 second) and 16 to 24 seconds to decode (zstd: about 4 to 8 ms). Use it when ratio matters more than
  time, for example archival.
- **JSON text into a generic tree:** wirefold's parser is slower than serde_json: 143 ms against 88 ms,
  57 MB/s against 94 MB/s.

So wirefold is not a faster serde. What it offers is a different combination: no dependencies beyond
apeiron, a columnar layout whose open step validates without walking the rows, one `Term` that reads and
writes JSON, YAML, TOON, POML, MessagePack and others, and an optional coder that trades time for ratio.

## Setup

- 100,000 records of one shape (`u64`, `u32`, `u8`, `f64`, `bool`, `String`), in two datasets: telemetry-shaped
  (increasing timestamps, small domains, six repeated names) and random (full-range integers and floats,
  random-looking names), which is the worst case for a columnar model.
- Machine: 4 vCPU Intel Xeon @ 2.10 GHz virtual machine, rustc 1.97.0, release build with `lto = true` and
  `codegen-units = 1`, run on an otherwise idle machine.
- Versions: serde_json 1 (with `float_roundtrip` so floats are exact), bincode 1.3, postcard 1,
  rmp-serde 1, rkyv 0.8 (validated), zstd 0.13 at level 19.
- Each encoder's output is decoded and compared with the input before it is timed. Timings are the median
  of 9 runs, 3 for zstd encoding and 1 for the cm coder.

## Results

#### Telemetry-shaped records (100000 records)

| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |
|---|---:|---:|---:|---:|
| wirefold Collapse | 3566664 | 35.7 | 0.96 | 5.67 |
| wirefold Collapse + cm coder | 743641 | 7.4 | 44099.82 | 16064.95 |
| wirefold Collapse + zstd -19 | 1002091 | 10.0 | 1307.79 | 4.36 |
| bincode 1.3 | 3566608 | 35.7 | 0.89 | 4.83 |
| postcard | 2464061 | 24.6 | 2.11 | 5.79 |
| postcard + zstd -19 | 936513 | 9.4 | 882.29 | 3.59 |
| rmp-serde (MessagePack) | 3058959 | 30.6 | 2.78 | 9.67 |
| rkyv 0.8 (validated) | 4000008 | 40.0 | 1.02 | 5.20 |
| serde_json | 8233460 | 82.3 | 9.82 | 21.18 |

Sum one `f64` column of 100000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 1.493 |
| rkyv 0.8 (validate + read) | 1.205 |
| bincode (decode all, then sum) | 5.078 |
| postcard (decode all, then sum) | 6.167 |

#### Random records (worst case for a columnar model) (100000 records)

| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |
|---|---:|---:|---:|---:|
| wirefold Collapse | 4593280 | 45.9 | 0.80 | 6.29 |
| wirefold Collapse + cm coder | 2995987 | 30.0 | 72411.98 | 24192.49 |
| wirefold Collapse + zstd -19 | 3154168 | 31.5 | 966.48 | 6.65 |
| bincode 1.3 | 4593224 | 45.9 | 0.93 | 4.60 |
| postcard | 4136643 | 41.4 | 2.63 | 6.30 |
| postcard + zstd -19 | 3403678 | 34.0 | 1041.07 | 7.82 |
| rmp-serde (MessagePack) | 4343065 | 43.4 | 3.36 | 9.56 |
| rkyv 0.8 (validated) | 5593224 | 55.9 | 1.53 | 4.71 |
| serde_json | 12309176 | 123.1 | 12.69 | 50.80 |

Sum one `f64` column of 100000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 1.621 |
| rkyv 0.8 (validate + read) | 0.514 |
| bincode (decode all, then sum) | 5.194 |
| postcard (decode all, then sum) | 6.569 |

#### JSON text into a generic tree (100000 records, 8233460 bytes)

| parser | time (ms) | MB/s |
|---|---:|---:|
| wirefold `parse_surface(Json)` | 143.47 | 57 |
| serde_json `Value` | 88.00 | 94 |

[exited with code 0]

## Limits of these numbers

- One run on one shared virtual machine, so expect noise of several percent; differences under about 10%
  (bincode against Collapse, for example) should be read as ties.
- One record shape. Records with many strings, deep nesting or optional fields behave differently.
- The JSON row compares parsers building a generic tree. serde_json can also deserialise straight into a
  typed struct, which wirefold does not do, so that case is not measured.
- The wirefold and rkyv column reads include the open step. A loop that opens once and reads many columns
  amortises it.
- zstd was tried at level 19 only. Lower levels are much faster and compress less.

## Reproduce

```bash
cd bench
cargo run --release -- 100000
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
