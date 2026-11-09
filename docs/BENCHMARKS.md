# Benchmarks

wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd on the same records. The harness
is [`bench/`](../bench/README.md). The serialization tables come from one run of it; the [parsing](#parsing) numbers
from runs described there.

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
- **Parsing text into a generic tree:** wirefold's owned JSON parser is level with `serde_json::Value`, and
  `parse_surface_ref`, which borrows strings from the input, is about 1.8 times faster than it (8 to 9 ms
  against 16 ms for 20,000 records). The other surfaces parse at 74 to 130 MB/s. See [Parsing](#parsing).
- **Rendering a tree to text:** YAML, TOON, CSV and POML render 3 to 7 times faster than they did, and JSON
  rendering was already tight. See [Rendering](#rendering).
- **Binary codecs:** MessagePack encoding is about 1.9 times faster and protobuf encoding about 1.7 times;
  decoding is unchanged for MessagePack and about 5% faster for protobuf. MessagePack is still about 1.4
  times slower to encode than `rmp-serde`. See [Binary codecs](#binary-codecs).

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

## Limits of these numbers

- One run on one shared virtual machine (the parsing table is the median of 15 runs on the same machine), so expect noise of several percent; differences under about 10%
  (bincode against Collapse, for example) should be read as ties.
- One record shape. Records with many strings, deep nesting or optional fields behave differently.
- The JSON parsing comparison is parser against parser, each building a generic tree. serde_json can also
  deserialise straight into a typed struct, which wirefold does not do, so that case is not measured.
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
