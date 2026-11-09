# Benchmarks

wirefold against serde_json, bincode, postcard, rmp-serde, rkyv and zstd on the same records. The harness
is [`bench/`](../bench/README.md). The serialization tables come from the run described under Results; the [parsing](#parsing) numbers
from runs described there.

## Summary

- **Plain encode and decode:** wirefold `Collapse` and bincode are the same size (713,116 against 713,060
  bytes for 20,000 telemetry records) and the same speed to encode (0.15 against 0.17 ms). Decoding everything,
  wirefold is slower than bincode and rkyv on this data (1.3 against 0.7 and 1.0 ms), roughly a tie on the random
  data. It is about 2.3 times smaller than serde_json, about 14 times faster to encode and about 3 times faster to
  decode. postcard is smaller (24.6 against 35.7 bytes per record) and takes about three times as long to encode.
  An earlier version of this page said wirefold tied bincode on decode; in fresh processes it does not on the
  telemetry data.
- **Reading one column without decoding the rest:** wirefold's validated open plus a column read is about
  3 times faster than bincode or postcard (decode everything, then read) and slower than rkyv: 0.34 against
  0.22 ms on the telemetry data and 0.26 against 0.11 ms on the random data.
- **Size with an entropy coder:** wirefold's context-mixing coder produced the smallest output, 14% below
  `postcard + zstd -19` and 19% below `Collapse + zstd -19` on the telemetry data, and 1% below
  `Collapse + zstd -19` on random data. It costs about 4.9 s to encode 20,000 telemetry records (zstd -19: about
  0.25 s) and 3.4 s to decode (zstd: about 1 ms), so about 7 and 5 microseconds per byte. That is 3.6 times faster to
  encode and 2.4 times faster to decode than the version measured earlier on this page (17.7 s and 8.4 s), at
  0.08% larger output (168,287 against 168,148 bytes) on the telemetry data. On the random data it is 5.2 times
  faster to encode (33.4 s to 6.4 s) and 2.9 times faster to decode (10.5 s to 3.6 s). It is still about 20 times
  slower than zstd -19 to encode and thousands of times slower to decode. Use it when ratio matters more than time,
  for example archival. What changed is in [DESIGN.md](DESIGN.md#the-context-mixing-coder).
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
writes JSON, YAML, TOON, POML, MessagePack and others, and an optional coder that trades time for ratio.

## Setup

- 20,000 records of one shape (`u64`, `u32`, `u8`, `f64`, `bool`, `String`), in two datasets: telemetry-shaped
  (increasing timestamps, small domains, six repeated names) and random (full-range integers and floats,
  random-looking names), which is the worst case for a columnar model.
- Machine: 4 vCPU Intel Xeon @ 2.10 GHz virtual machine, rustc 1.97.0, release build with `lto = true` and
  `codegen-units = 1`, run on an otherwise idle machine.
- Versions: serde_json 1 (with `float_roundtrip` so floats are exact), bincode 1.3, postcard 1,
  rmp-serde 1, rkyv 0.8 (validated), zstd 0.13 at level 19.
- Each encoder's output is decoded and compared with the input before it is timed. Timings are the mean of 3 runs
  per cell, each cell in a fresh process, with the range shown.

## Results

20,000 records, each row in a fresh process, mean of 3 runs with the range in brackets. The cm coder row is
instead the mean and range over three whole processes, one run each, because a run takes tens of seconds.

#### Telemetry-shaped records (20000 records)

| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |
|---|---:|---:|---:|---:|
| wirefold Collapse | 713116 | 35.7 | 0.15 (0.14 to 0.17) | 1.34 (1.14 to 1.67) |
| wirefold Collapse + cm coder | 168287 | 8.4 | 4,925 (4,618 to 5,162) | 3,440 (3,397 to 3,492) |
| wirefold Collapse + zstd -19 | 207470 | 10.4 | 258.61 (246.48 to 273.54) | 0.84 (0.75 to 0.94) |
| bincode 1.3 | 713060 | 35.7 | 0.17 (0.15 to 0.18) | 0.74 (0.72 to 0.78) |
| postcard | 492525 | 24.6 | 0.45 (0.44 to 0.46) | 0.91 (0.89 to 0.94) |
| postcard + zstd -19 | 194386 | 9.7 | 177.88 (157.20 to 209.26) | 0.72 (0.69 to 0.78) |
| rmp-serde (MessagePack) | 611506 | 30.6 | 0.65 (0.60 to 0.68) | 1.74 (1.52 to 2.19) |
| rkyv 0.8 (validated) | 800008 | 40.0 | 0.22 (0.20 to 0.25) | 0.98 (0.90 to 1.10) |
| serde_json | 1646252 | 82.3 | 2.09 (1.95 to 2.27) | 4.19 (3.97 to 4.40) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.34 (0.31 to 0.41) |
| rkyv 0.8 (validate + read) | 0.22 (0.22 to 0.23) |
| bincode (decode all, then sum) | 1.02 (0.80 to 1.34) |
| postcard (decode all, then sum) | 1.28 (0.99 to 1.65) |

#### Random records (worst case for a columnar model) (20000 records)

| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |
|---|---:|---:|---:|---:|
| wirefold Collapse | 918696 | 45.9 | 0.16 (0.13 to 0.17) | 1.23 (1.01 to 1.55) |
| wirefold Collapse + cm coder | 626004 | 31.3 | 6,416 (6,221 to 6,572) | 3,641 (3,381 to 3,996) |
| wirefold Collapse + zstd -19 | 634681 | 31.7 | 185.42 (163.17 to 223.57) | 1.74 (1.26 to 2.14) |
| bincode 1.3 | 918640 | 45.9 | 0.17 (0.16 to 0.18) | 0.82 (0.64 to 1.15) |
| postcard | 827235 | 41.4 | 0.67 (0.54 to 0.79) | 1.06 (1.00 to 1.16) |
| postcard + zstd -19 | 692567 | 34.6 | 168.40 (155.34 to 191.49) | 1.60 (1.34 to 1.97) |
| rmp-serde (MessagePack) | 868558 | 43.4 | 0.72 (0.71 to 0.72) | 1.53 (1.50 to 1.56) |
| rkyv 0.8 (validated) | 1118640 | 55.9 | 0.28 (0.26 to 0.29) | 0.76 (0.59 to 0.92) |
| serde_json | 2461362 | 123.1 | 3.02 (2.30 to 3.73) | 6.94 (5.47 to 9.09) |

Sum one `f64` column of 20000 records (open + read, no full decode):

| format | time (ms) |
|---|---:|
| wirefold view (open + column read) | 0.26 (0.22 to 0.34) |
| rkyv 0.8 (validate + read) | 0.11 (0.07 to 0.13) |
| bincode (decode all, then sum) | 0.90 (0.83 to 0.94) |
| postcard (decode all, then sum) | 1.29 (0.96 to 1.55) |

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
- zstd was tried at level 19 only. Lower levels are much faster and compress less.

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
