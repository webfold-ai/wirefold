# wirefold against lz4, zstd and other compressors

A measurement report: how a column-oriented binary layout and per-column encodings compare with the common
general-purpose compressors (lz4, Snappy, deflate, Brotli, zstd) and with other Rust serializers (bincode, postcard,
MessagePack, rkyv, JSON), on three synthetic datasets of 20,000 records each. Everything here was measured on one
shared virtual machine; the tables are medians of three runs. The benchmark code, the data generators and the
commands to reproduce it are in the repository (`bench/`).

## Summary

Compressor trade-offs, on `Collapse` bytes of the telemetry dataset (713,212 bytes raw):

- **lz4 is the fast end, zstd the balanced middle, brotli and zstd -19 the dense end.** lz4 (fast mode) gives a ratio
  of 1.84 at 535 MB/s to encode and 1,289 MB/s to decode. zstd -1 gives 3.58 at 312 and 713 MB/s: its output is 48% smaller
  than lz4's, for 1.7 times the encode time and 1.8 times the decode time. zstd -3 is 3.76 (239 / 764 MB/s), zstd -19 is
  4.27 (3 / 687 MB/s) and Brotli 11 is 4.28 (1 / 176 MB/s, with a pure-Rust port that is slower than the C original).
  lz4 in its high-compression mode (level 9) reaches 2.20 at 22 MB/s: still only 61% of zstd -1's ratio, and 25 times slower
  to encode than lz4's fast mode. Snappy is level with lz4's fast mode (1.88, 519 / 1,018 MB/s).
- **The layout and the compressor interact.** Under the two compressors with no entropy-coding stage (lz4, Snappy)
  the column layout is worse than the row formats: `Collapse` is 387,056 bytes under lz4, against 281,100 for postcard,
  298,293 for bincode, 304,660 for MessagePack and 357,924 for rkyv. Under zstd and Brotli it is better: 199,403 under zstd -1
  (next best 227,528), 189,765 under zstd -3 (next best 207,314) and 167,124 under zstd -19 (next best 187,121). This
  is what a compressor that models each byte's distribution would be expected to gain from one field's values being
  together; the reason was not isolated here.
- **Encoding each column for its type needs no general-purpose compressor to win.** Delta coding, bit packing to the exact
  width, decimal floats stored as scaled integers and a dictionary for strings, with no compressor at all, take the
  telemetry buffer to 150,245 bytes: 79% smaller than raw, 10% smaller than zstd -19 and Brotli 11 on the whole buffer,
  in 1.44 ms to encode and 0.49 ms to decode.
- **With a compressor behind the encodings, the fast ones become competitive.** The encoded columns under lz4 are 126,067
  bytes at 1.70 ms to encode and 0.51 ms to decode: 37% smaller than zstd -1 on the whole buffer (199,403), and faster
  to both encode (2.29 ms) and decode (1.00 ms). Under zstd -1 they are 105,790 (2.89 ms / 0.80 ms); the densest
  combinations (zstd -19, Brotli 11) reach about 104,500 at 168 to 335 ms.
- **Random data is a control.** With uniformly random fields nothing compresses much (zstd and Brotli land at
  about 613,000 to 626,000 bytes of 918,792, deflate at about 640,000 to 645,000, lz4 at 832,448) and the encodings change nothing (918,799 bytes with none).
- **A text format can be the best row format.** On the sensor dataset JSON under zstd -19 is 164,149 bytes, smaller than
  every binary row format under any compressor shown (best 196,454, postcard under Brotli 5) and level with `Collapse` under zstd -19 (166,416).
- **Cost.** `Collapse` is not the fastest to decode: 1.27 ms to deserialize the telemetry records against 0.91 for bincode
  and 1.02 for postcard (1.84 / 1.13 / 1.20 ms including lz4 decompression). The column encodings are benchmark code,
  not part of the library, so a user of the library gets the plain-compressor sizes unless they write them.

## What is measured

**The serializers.** Each of six Rust serializers turns the same 20,000 records into bytes:

- wirefold `Collapse` (derive macro): a section table with one contiguous column per scalar field (booleans as bytes) and
  one for the strings (offsets then bytes). Every table begins with a header word, the section count and then the bytes
  `W`, `C`, a layout version and zero, so a reader refuses a buffer that is not a table or is of another version.
- bincode 1.3 and postcard 1: row-oriented, bincode fixed-width and postcard variable-length integers.
- rmp-serde (MessagePack), serde_json: the usual self-describing formats.
- rkyv 0.8: archive-in-place, validated on read.

**The record.** `{ ts: u64, user: u32, kind: u8, value: f64, ok: bool, name: String }`, 20,000 of them.

**The three datasets** (all generated in `bench/src/main.rs` from a fixed seed):

- *Telemetry-shaped*: `ts` increases by 1 to 50 per record (uniform); `user` is uniform over 0 to 4,999; `kind` is uniform
  over 6 values; `value` is uniform over 0 to 999.99 in steps of 0.01; `ok` is true nine times in ten; `name` is one of 6
  strings, uniformly.
- *Random*: `ts`, `user` and `kind` are full-range random integers, `value` is a random finite `f64`, `ok` is a coin flip,
  `name` is the hex of a random 64-bit number. A control with nothing to find.
- *Sensor-shaped*: `ts` advances 10 per record with a 1 in 10 chance of a jump of 1 to 1,000; `user` is a skewed device id
  (`u*u/1000` for `u` uniform over 0 to 999); `kind` changes with probability 1/20 to one of 8 values; `value` is a random
  walk in steps of up to 1.000, with three decimals; `ok` is false one time in a hundred; `name` is `sensor-` and the id
  modulo 200 (200 distinct names).

**The compressors.** lz4 (fast mode, and high-compression level 9), Snappy, deflate (zlib, levels 1, 6, 9), Brotli
(qualities 1, 5, 9, 11) and zstd (levels 1, 3, 9, 19), each applied to a byte slice and checked to round-trip.

**The column encodings** (the `encoded columns` rows). Each column of the `Collapse` buffer is encoded for its type and then
compressed as its own frame:

- integers: either the values minus their minimum, at the narrowest whole-byte width, or packed to the exact bit width,
  or either of those applied to the differences between neighbours, whichever is smallest after the compressor (judged
  with the compressor itself when it is fast, zstd -3 otherwise, so the search does not dominate the slow settings);
- `ts`: the differences between neighbours; `value`: the floats as integers scaled by the fewest decimal places that
  reproduce every value exactly, then as an integer column; `name`: a dictionary and one-byte indices when there are at
  most 255 distinct strings; `kind` and `ok`: as stored.

These encoders live in the benchmark, not in the wirefold library. The `columns` rows apply the compressor to each column
as its own frame with no encoding, which separates the effect of framing from the effect of the encodings.

**Timing.** Each cell is the median of three separate process runs. Within a run it is the median of five repetitions after
a first run, or that first run alone when it takes over 0.3 s. Encode and decode rates (MB/s) use the uncompressed size
as the numerator. End-to-end times are serialize then compress for encode, and decompress then deserialize for decode.
Times of a millisecond or two vary by a few tenths between runs on this shared machine, so small differences between
rows are not established.

**Environment.**

- Machine: Intel(R) Xeon(R) Processor @ 2.10GHz, 4 vCPUs (a shared virtual machine); rustc 1.97.0 (2d8144b78 2026-07-07); release build, `lto = true`, `codegen-units = 1`.
- Compressors: lz4 1.28.1 (the C library liblz4, block format with the original length prepended), zstd 0.13.4 (C library), snap 1.1.2 (Snappy, pure Rust), flate2 1.1.11 (zlib/deflate through the pure-Rust miniz_oxide), brotli 7.0.0 (pure Rust). The pure-Rust ports are slower than their C originals, so the deflate and brotli times understate what the C libraries do.
- Serializers: serde_json 1.0.154, bincode 1.3.3, postcard 1.1.3, rmp-serde 1.3.1, rkyv 0.8.18, wirefold 0.0.0.

## Results

### Telemetry-shaped records (20000 records)

#### Compressors on the `Collapse` bytes (713212 bytes)

| compressor | size (B) | ratio | size / raw | encode (ms) | encode (MB/s) | decode (ms) | decode (MB/s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 713212 | 1.00 | 100.0% | 0.02 | - | 0.02 | - |
| lz4 (fast) | 387056 | 1.84 | 54.3% | 1.33 | 535 | 0.55 | 1289 |
| lz4 hc 9 | 324146 | 2.20 | 45.4% | 33.10 | 22 | 0.55 | 1297 |
| snappy | 379074 | 1.88 | 53.2% | 1.37 | 519 | 0.70 | 1018 |
| deflate 1 | 234851 | 3.04 | 32.9% | 3.78 | 189 | 2.19 | 325 |
| deflate 6 | 203089 | 3.51 | 28.5% | 50.24 | 14 | 1.79 | 398 |
| deflate 9 | 199675 | 3.57 | 28.0% | 158.92 | 4 | 1.74 | 410 |
| brotli 1 | 217867 | 3.27 | 30.5% | 5.06 | 141 | 4.24 | 168 |
| brotli 5 | 176705 | 4.04 | 24.8% | 21.29 | 33 | 2.77 | 257 |
| brotli 9 | 174169 | 4.09 | 24.4% | 81.04 | 9 | 3.91 | 183 |
| brotli 11 | 166478 | 4.28 | 23.3% | 1263.61 | 1 | 4.06 | 176 |
| zstd 1 | 199403 | 3.58 | 28.0% | 2.29 | 312 | 1.00 | 713 |
| zstd 3 | 189765 | 3.76 | 26.6% | 2.99 | 239 | 0.93 | 764 |
| zstd 9 | 178452 | 4.00 | 25.0% | 12.76 | 56 | 0.89 | 803 |
| zstd 19 | 167124 | 4.27 | 23.4% | 247.12 | 3 | 1.04 | 687 |

#### Size of every serializer's bytes under each compressor

| serializer | none | lz4 (fast) | snappy | zstd 1 | zstd 3 | zstd 19 | brotli 5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 713212 | 387056 | 379074 | 199403 | 189765 | 167124 | 176705 |
| bincode 1.3 | 713060 | 298293 | 332040 | 234749 | 219402 | 199079 | 196054 |
| postcard | 492525 | 281100 | 301390 | 263942 | 207314 | 194386 | 187488 |
| rmp-serde (MessagePack) | 611506 | 304660 | 305533 | 227528 | 220582 | 202583 | 206315 |
| rkyv 0.8 (validated) | 800008 | 357924 | 360647 | 273202 | 225765 | 187121 | 194927 |
| serde_json | 1646252 | 448857 | 467381 | 260706 | 265783 | 201563 | 239955 |

#### End-to-end time with lz4 and with zstd -3

Encode is serialize then compress, decode is decompress then deserialize; milliseconds, median of 5.

| serializer | serialize | + lz4: compress | encode total | decompress | deserialize | decode total | + zstd -3: compress | encode total | decompress | decode total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 0.21 | 1.29 | 1.52 | 0.52 | 1.27 | 1.84 | 3.14 | 3.35 | 0.95 | 2.25 |
| bincode 1.3 | 0.17 | 1.31 | 1.48 | 0.22 | 0.91 | 1.13 | 2.67 | 2.84 | 0.82 | 1.73 |
| postcard | 0.45 | 1.14 | 1.56 | 0.18 | 1.02 | 1.20 | 2.38 | 2.79 | 0.66 | 1.68 |
| rmp-serde (MessagePack) | 0.58 | 0.80 | 1.38 | 0.15 | 1.75 | 1.94 | 2.08 | 2.66 | 0.62 | 2.44 |
| rkyv 0.8 (validated) | 0.27 | 1.47 | 1.73 | 0.31 | 1.07 | 1.36 | 2.81 | 3.07 | 1.00 | 2.10 |
| serde_json | 2.07 | 2.51 | 4.53 | 0.55 | 4.54 | 5.09 | 4.34 | 6.41 | 1.27 | 5.75 |

#### `Collapse` columns with encodings, behind each compressor

The same compressor is applied to the whole buffer, to each column as its own frame, and to each column after the encodings (delta, exact-bit-width packing, decimal scaling, dictionary). Times are for the encoded columns.

| compressor | whole buffer (B) | columns (B) | encoded columns (B) | saving vs whole | encode (ms) | decode (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 713212 | 713199 | 150245 | 79% | 1.44 | 0.49 |
| lz4 (fast) | 387056 | 352574 | 126067 | 67% | 1.70 | 0.51 |
| lz4 hc 9 | 324146 | 259146 | 122845 | 62% | 9.79 | 0.60 |
| snappy | 379074 | 330806 | 120764 | 68% | 1.70 | 0.58 |
| deflate 6 | 203089 | 173065 | 107616 | 47% | 8.87 | 0.99 |
| zstd 1 | 199403 | 178207 | 105790 | 47% | 2.89 | 0.80 |
| zstd 3 | 189765 | 167109 | 107638 | 43% | 4.12 | 0.81 |
| zstd 19 | 167124 | 151261 | 104729 | 37% | 334.89 | 0.75 |
| brotli 5 | 176705 | 161366 | 107264 | 39% | 6.64 | 1.23 |
| brotli 11 | 166478 | 134114 | 104446 | 37% | 168.13 | 1.32 |

### Random records (20000 records)

#### Compressors on the `Collapse` bytes (918792 bytes)

| compressor | size (B) | ratio | size / raw | encode (ms) | encode (MB/s) | decode (ms) | decode (MB/s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 918792 | 1.00 | 100.0% | 0.03 | - | 0.03 | - |
| lz4 (fast) | 832448 | 1.10 | 90.6% | 0.51 | 1786 | 0.24 | 3843 |
| lz4 hc 9 | 753520 | 1.22 | 82.0% | 25.56 | 36 | 0.43 | 2140 |
| snappy | 832118 | 1.10 | 90.6% | 0.31 | 2999 | 0.22 | 4172 |
| deflate 1 | 639731 | 1.44 | 69.6% | 8.13 | 113 | 3.62 | 254 |
| deflate 6 | 644778 | 1.42 | 70.2% | 41.40 | 22 | 2.36 | 389 |
| deflate 9 | 644446 | 1.43 | 70.1% | 91.77 | 10 | 2.32 | 396 |
| brotli 1 | 622518 | 1.48 | 67.8% | 2.83 | 324 | 2.95 | 312 |
| brotli 5 | 624854 | 1.47 | 68.0% | 35.04 | 26 | 5.16 | 178 |
| brotli 9 | 625664 | 1.47 | 68.1% | 86.47 | 11 | 5.10 | 180 |
| brotli 11 | 623803 | 1.47 | 67.9% | 894.58 | 1 | 8.09 | 114 |
| zstd 1 | 613402 | 1.50 | 66.8% | 1.12 | 822 | 0.64 | 1435 |
| zstd 3 | 624300 | 1.47 | 67.9% | 2.81 | 327 | 0.76 | 1212 |
| zstd 9 | 617330 | 1.49 | 67.2% | 8.37 | 110 | 0.74 | 1241 |
| zstd 19 | 624045 | 1.47 | 67.9% | 184.25 | 5 | 1.18 | 776 |

#### Size of every serializer's bytes under each compressor

| serializer | none | lz4 (fast) | snappy | zstd 1 | zstd 3 | zstd 19 | brotli 5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 918792 | 832448 | 832118 | 613402 | 624300 | 624045 | 624854 |
| bincode 1.3 | 918640 | 812040 | 794548 | 678272 | 683707 | 673404 | 686199 |
| postcard | 827235 | 830485 | 827282 | 740567 | 740578 | 692567 | 725108 |
| rmp-serde (MessagePack) | 868558 | 871913 | 868605 | 764693 | 760886 | 710596 | 740112 |
| rkyv 0.8 (validated) | 1118640 | 957556 | 944554 | 700458 | 702764 | 647127 | 675880 |
| serde_json | 2461362 | 1590999 | 1534363 | 831395 | 917434 | 749105 | 894583 |

#### End-to-end time with lz4 and with zstd -3

Encode is serialize then compress, decode is decompress then deserialize; milliseconds, median of 5.

| serializer | serialize | + lz4: compress | encode total | decompress | deserialize | decode total | + zstd -3: compress | encode total | decompress | decode total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 0.23 | 0.49 | 0.74 | 0.24 | 1.15 | 1.40 | 2.81 | 3.09 | 0.79 | 2.03 |
| bincode 1.3 | 0.18 | 1.27 | 1.45 | 0.11 | 0.89 | 0.99 | 4.85 | 5.03 | 1.32 | 2.13 |
| postcard | 0.58 | 0.07 | 0.65 | 0.04 | 1.17 | 1.22 | 2.31 | 3.00 | 1.11 | 2.31 |
| rmp-serde (MessagePack) | 0.77 | 0.30 | 1.07 | 0.05 | 1.89 | 1.93 | 4.41 | 5.24 | 1.24 | 3.22 |
| rkyv 0.8 (validated) | 0.39 | 1.41 | 1.87 | 0.19 | 0.96 | 1.14 | 6.54 | 7.02 | 1.82 | 2.93 |
| serde_json | 2.59 | 4.65 | 7.23 | 0.88 | 6.32 | 7.20 | 14.70 | 17.25 | 2.63 | 8.95 |

#### `Collapse` columns with encodings, behind each compressor

The same compressor is applied to the whole buffer, to each column as its own frame, and to each column after the encodings (delta, exact-bit-width packing, decimal scaling, dictionary). Times are for the encoded columns.

| compressor | whole buffer (B) | columns (B) | encoded columns (B) | saving vs whole | encode (ms) | decode (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 918792 | 918779 | 918799 | -0% | 2.31 | 0.44 |
| lz4 (fast) | 832448 | 761555 | 761575 | 9% | 3.09 | 0.46 |
| lz4 hc 9 | 753520 | 676098 | 676113 | 10% | 32.16 | 0.60 |
| snappy | 832118 | 756440 | 756459 | 9% | 3.27 | 0.62 |
| deflate 6 | 644778 | 608628 | 608648 | 6% | 30.00 | 2.36 |
| zstd 1 | 613402 | 591178 | 591198 | 4% | 4.56 | 1.00 |
| zstd 3 | 624300 | 590820 | 590840 | 5% | 6.56 | 1.00 |
| zstd 19 | 624045 | 585556 | 585576 | 6% | 462.52 | 0.85 |
| brotli 5 | 624854 | 602561 | 602581 | 4% | 30.94 | 3.70 |
| brotli 11 | 623803 | 583849 | 583869 | 6% | 960.74 | 4.25 |

### Sensor-shaped records (20000 records)

#### Compressors on the `Collapse` bytes (785445 bytes)

| compressor | size (B) | ratio | size / raw | encode (ms) | encode (MB/s) | decode (ms) | decode (MB/s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 785445 | 1.00 | 100.0% | 0.02 | - | 0.02 | - |
| lz4 (fast) | 366126 | 2.15 | 46.6% | 1.48 | 530 | 0.56 | 1411 |
| lz4 hc 9 | 323185 | 2.43 | 41.1% | 33.28 | 24 | 0.56 | 1393 |
| snappy | 360188 | 2.18 | 45.9% | 1.38 | 571 | 0.70 | 1129 |
| deflate 1 | 232514 | 3.38 | 29.6% | 3.95 | 199 | 2.21 | 356 |
| deflate 6 | 204512 | 3.84 | 26.0% | 52.34 | 15 | 2.02 | 389 |
| deflate 9 | 200017 | 3.93 | 25.5% | 155.39 | 5 | 2.05 | 383 |
| brotli 1 | 222208 | 3.53 | 28.3% | 5.99 | 131 | 5.19 | 151 |
| brotli 5 | 176925 | 4.44 | 22.5% | 23.10 | 34 | 2.76 | 284 |
| brotli 9 | 173309 | 4.53 | 22.1% | 85.69 | 9 | 2.58 | 304 |
| brotli 11 | 166813 | 4.71 | 21.2% | 1409.43 | 1 | 3.88 | 202 |
| zstd 1 | 206261 | 3.81 | 26.3% | 2.50 | 314 | 1.02 | 767 |
| zstd 3 | 188094 | 4.18 | 23.9% | 3.00 | 262 | 1.05 | 748 |
| zstd 9 | 178812 | 4.39 | 22.8% | 13.40 | 59 | 1.02 | 770 |
| zstd 19 | 166416 | 4.72 | 21.2% | 265.11 | 3 | 1.18 | 665 |

#### Size of every serializer's bytes under each compressor

| serializer | none | lz4 (fast) | snappy | zstd 1 | zstd 3 | zstd 19 | brotli 5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 785445 | 366126 | 360188 | 206261 | 188094 | 166416 | 176925 |
| bincode 1.3 | 785293 | 341410 | 351912 | 238662 | 230676 | 198768 | 202768 |
| postcard | 558258 | 312729 | 322494 | 280982 | 220752 | 198944 | 196454 |
| rmp-serde (MessagePack) | 668232 | 333615 | 318212 | 257368 | 236076 | 198008 | 212445 |
| rkyv 0.8 (validated) | 964200 | 475088 | 442959 | 315183 | 283612 | 217627 | 229878 |
| serde_json | 1713398 | 447407 | 451312 | 252511 | 258395 | 164149 | 225454 |

#### End-to-end time with lz4 and with zstd -3

Encode is serialize then compress, decode is decompress then deserialize; milliseconds, median of 5.

| serializer | serialize | + lz4: compress | encode total | decompress | deserialize | decode total | + zstd -3: compress | encode total | decompress | decode total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wirefold Collapse | 0.20 | 1.45 | 1.65 | 0.54 | 1.33 | 1.84 | 3.20 | 3.40 | 1.07 | 2.37 |
| bincode 1.3 | 0.17 | 1.35 | 1.51 | 0.27 | 0.91 | 1.20 | 2.94 | 3.09 | 0.97 | 1.89 |
| postcard | 0.46 | 1.21 | 1.70 | 0.17 | 1.11 | 1.30 | 2.76 | 3.21 | 0.85 | 1.96 |
| rmp-serde (MessagePack) | 0.66 | 1.27 | 1.93 | 0.18 | 1.77 | 1.95 | 2.80 | 3.46 | 0.82 | 2.63 |
| rkyv 0.8 (validated) | 0.36 | 2.03 | 2.39 | 0.38 | 1.01 | 1.39 | 4.22 | 4.57 | 1.48 | 2.43 |
| serde_json | 2.13 | 3.29 | 5.47 | 0.57 | 4.93 | 5.50 | 4.40 | 6.58 | 1.48 | 6.40 |

#### `Collapse` columns with encodings, behind each compressor

The same compressor is applied to the whole buffer, to each column as its own frame, and to each column after the encodings (delta, exact-bit-width packing, decimal scaling, dictionary). Times are for the encoded columns.

| compressor | whole buffer (B) | columns (B) | encoded columns (B) | saving vs whole | encode (ms) | decode (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| none | 785445 | 785432 | 140384 | 82% | 5.13 | 0.45 |
| lz4 (fast) | 366126 | 323668 | 88710 | 76% | 5.67 | 0.59 |
| lz4 hc 9 | 323185 | 254507 | 83778 | 74% | 12.34 | 0.56 |
| snappy | 360188 | 308047 | 88058 | 76% | 5.70 | 0.50 |
| deflate 6 | 204512 | 172809 | 79564 | 61% | 10.33 | 0.86 |
| zstd 1 | 206261 | 186456 | 79795 | 61% | 6.93 | 0.80 |
| zstd 3 | 188094 | 168063 | 80337 | 57% | 7.64 | 0.79 |
| zstd 19 | 166416 | 150503 | 78824 | 53% | 350.73 | 0.75 |
| brotli 5 | 176925 | 159267 | 79231 | 55% | 9.82 | 1.17 |
| brotli 11 | 166813 | 135420 | 77675 | 53% | 276.73 | 1.20 |



## What these numbers do not show

- **All three datasets are synthetic.** Their distributions are simple (mostly uniform draws), so the encodings meet
  close to their entropy bounds; real data with correlated fields, long strings, nulls or nested records will behave
  differently. The random dataset is the control and shows no gain.
- **One record shape, one machine.** A 4-vCPU shared virtual machine, a single thread for every compressor. Absolute rates
  will differ elsewhere; the order of the compressors should not.
- **Pure-Rust ports.** Snappy, deflate (miniz_oxide) and Brotli are Rust implementations, slower than zlib, zlib-ng and the C
  Brotli, so their times understate those libraries. lz4 and zstd are the C libraries through bindings. lz4 is used in
  its block format with the length prepended, not the frame format.
- **The column encodings are not in the library.** They are benchmark code written for this record's field types; making
  them a library feature is separate work. The row formats here receive no equivalent transformation, because they would have to
  be transposed into columns first; the comparison is of a column layout with encodings against row layouts with a
  plain compressor, not of the wire formats alone.
- **Deserialization speed is not the strong point.** `Collapse` decodes every record more slowly than bincode and postcard
  here, as the end-to-end tables show; its strength in these runs is size under entropy-coding compressors and reading one
  column without decoding the rest, which these tables do not measure (see `docs/BENCHMARKS.md`).
- **No zstd dictionaries, no multi-threading, no streaming.** Each buffer is compressed whole, in one call.
- **The explanation of the lz4 result is a hypothesis.** The tables show the interaction; they do not isolate its cause.
- **Continuous integration has not run on this repository** (a billing limit rejects the jobs before a runner is assigned);
  the tests, sanitizer runs, Miri and fuzzing cited in the repository were run locally.

## Reproduce

```bash
cd bench
cargo build --release
for i in 1 2 3; do ./target/release/wirefold-bench report 20000 > run$i.md; done
python3 scripts/merge_reports.py run1.md run2.md run3.md > merged.md   # the median of the three, as in this report
```

`wirefold-bench report [records]` prints the tables above for one run. The data generators, the serializers, the
compressor wrappers (`bench/src/backends.rs`) and the column encoders are in `bench/src/`.
