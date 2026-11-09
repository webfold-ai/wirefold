# Design

wirefold is built from apeiron's primitives, not from the crates a Rust project would normally reach for
(`serde`, `bincode`, `itoa`, `ryu`, `rayon`, `hashbrown`). This page says what that means in practice, why it
was chosen, where it pays and where it costs. The measurements it quotes are in [BENCHMARKS.md](BENCHMARKS.md).

## The basis

[apeiron](https://github.com/webfold-ai/apeiron) offers a small number of ideas, and every part of wirefold is
an application of one of them.

1. **A static, schedule-free partition.** `Pool::dispatch(tasks, f)` gives task `i` to a fixed worker. There is
   no work stealing and no queue, so the answer cannot depend on which core ran which piece. Parallelism in
   wirefold is only ever "cut the input by a pure function of its offset, run the pieces, join in piece order".
2. **The associative fold.** If an operation is associative, a sequence of it can be split anywhere and the parts
   recombined. Anything that looks serial but is really an associative accumulation is a fold, and a fold is
   parallel by construction. A parser's state is the clearest example (below).
3. **Accumulating tables** (`CoordMap`, `FreqMap`). A table is a probe and a merge; counting is the merge that
   is order-free, so a count table is built by splitting the stream, counting the parts and adding the tables.
   The entropy coder's model is such a table (below).
4. **An evidence standard.** A speed claim is a mean over at least three runs with its spread, and a change
   inside the spread seen on unchanged code is not a change. Instruction counts (callgrind) are used where
   wall-clock is too noisy to decide.

The practical effect is a short dependency list: wirefold depends on apeiron and on nothing else. That is the
point of the exercise, since fewer crates to fetch and compile is a direct build-time saving, but the design
reasons below stand independently of it.

## Why not serde

serde separates a data model (the `Serialize`/`Deserialize` traits and visitors) from formats. That separation is
what lets one derive serve many formats, and it is a good design. It also fixes the shape of the work: a value
is walked field by field, row by row, and each format receives a stream of calls.

wirefold keeps the data in the shape it will be used in:

- **`#[derive(Collapse)]` lays a `Vec<T>` out as a section table.** The scalar fields are one row-major array of
  packed records and each variable-length field is a column of its own. Opening the buffer validates the layout
  without walking rows, and reading a field is a read at a computed offset, not a decode. Summing the `f64` field
  of the telemetry records strides through the record array (0.3 ms for 20,000 records); the strings are never
  touched.
- **Text formats go through one universal `SyntaxTerm`.** JSON, YAML, TOON, POML, CSV and others parse into the
  same tree and render from it, so a conversion is parse then render, not a per-pair adapter.
- **Parsers are generic over a `Build` trait** with an owned builder (`SyntaxTerm`) and a borrowed one
  (`SyntaxTermRef`, which slices into the input). One parser body produces both.

The cost is that wirefold's formats are its own. If you need to emit a format wirefold does not have, or you rely
on the serde ecosystem, serde is the right choice.

## Why not bincode, postcard, rkyv

These are row-wise (bincode, postcard) or archive-in-place (rkyv). They are fast and small, and the benchmark
shows them winning some rows: bincode and rkyv decode a full vector of records as fast as or faster than
wirefold, and rkyv's validated column read beats wirefold's. What the layout buys is a zero-copy
open that validates without walking the rows. It does not buy smaller compressed output as stored: after zstd -19 the
telemetry records are 207,470 bytes as `Collapse` against 199,079 for bincode and 194,386 for postcard, because the
scalar fields are interleaved row after row. Split into one plane per field and compressed plane by plane, the same
records are 151,213 bytes at -19 and 166,713 at zstd -3, smaller than any of them (see BENCHMARKS.md, Columns and
zstd); that split is not in the library, and a layout with contiguous scalar columns would make it a slice.

## Parsing as a fold

A text parser looks inherently serial: you cannot know whether a `"` opens or closes a string without knowing
the state before it. The apeiron view is that "the state before it" is just one of a small number of possible
states, so a chunk can be summarised under every possible entry state and the summaries combined.

For JSON each chunk produces a `ChunkSummary`: for both entry states (outside or inside a string), the net
bracket depth, whether the string state flips by the end, and where the first top-level comma falls. Summaries
combine associatively. The combine over the chunk count is the only serial step; every pass over the document
itself is parallel. YAML cuts at margin dash lines, which are a pure function of the byte offset.

**Soundness.** The cut finder is allowed to be wrong, because the result is checked rather than trusted: each
piece is parsed on its own, and only if every piece parses is the joined result used. Anything else falls back to
the sequential parser. So the parallel result equals the sequential result whenever it is returned, and a cut
that lands in the wrong place costs time, never correctness. The tests include a decline-or-equal property
(parallel output is equal to sequential output or the parallel path declined) and an accuracy test that
distinguishes a correct cut from a merely harmless one, since a wrong cut alone would only cause declines and
would otherwise pass unnoticed. Each was mutation-checked.

**Where it does not apply.** The owned parse (`parse_surface`) stays sequential. Parsing is faster in parallel,
but freeing nodes allocated on other threads' arenas costs as much as the parse saved, so the end-to-end time did
not improve. The borrowed parse (`parse_surface_ref`) allocates no per-node strings, so it does parallelise.
Documents under 512 KiB take the unchanged path.

**Nested dispatch.** The pool's dispatch lock is not reentrant, so a task that itself dispatched would deadlock.
`par_tasks` is the one entry point and runs inline when called from inside a task. A test exercises it nested.

## The entropy coder

The entropy stage is a static model read out as a table, and it is fast because it does no work per bit that
could be done once per stream.

A context's counts are an order-free accumulation: they add, they split across any partition of the stream and
recombine to the same table, and the table is read back by its address (apeiron's tables are exactly that
accumulation, with the address regenerating the key). So the model is the stream's counts, built once, and
coding is a lookup against a fixed table: tANS (`codec::tans`) in order-0 and a class-shaped order-1 family,
four interleaved streams, plus a 4-stream Huffman face (`codec::huf`). Nothing is carried from one symbol's
prediction to the next, so there is no per-bit mixing, no adaptive state and no second pass to keep encode and
decode in step. `cm::compress` is this coder (an explicit `Order`, zero or one; a version byte starts the stream) preceded by the 2-D stride search (median-edge residual at the
row stride that codes smallest on a sample; the trials run as one static batch on the pool).
`compress_shaped` measures three faces on the input (order-1 tANS, lz, and the lz-plus-tANS hybrid) and takes
the fastest within a size tolerance of the smallest; they run as one static batch too.

Measured on the telemetry records: encode 127 ms and decode 2.0 ms for 713 KB, against 2.0 ms and 1.2 ms for
zstd -1 on the same bytes (237,083 against 243,202 bytes). So this coder does not compete with zstd at the
levels people use; it beats only level 19 on encode time (249 ms), at 17% more bytes. What it is for is a
compression path with no C dependency and nothing to build, not a better compressor, and the serialization
format does not depend on it: `Collapse` output is plain bytes that can be handed to zstd. An adaptive
context-mixing coder was used here before. It was 28% smaller than this one on the telemetry data (174,810
bytes, below zstd -19) and level on random data; it took 3.5 s to encode and 1.5 s to decode the same bytes
after being made 5 to 7 times faster by caching and chunking, and was removed for that cost.

## Numbers: why not ryu and itoa

`ryu` (floats) and `itoa` (integers) are very good: the benchmark measures them about 1.9x and 2.8x faster than the
standard library's `Display`. They are not used, and the reason is where numbers appear:

- In **JSON and YAML, numbers are never formatted.** `Atom::Num(String)` keeps the source text, so parse then
  render is a copy. This is also lossless (`1.0`, `1e3` and `0.10` survive), which a parse to `f64` and format
  back through `ryu` would not guarantee. There is no formatting step for a faster formatter to speed up.
- In **Collapse**, numbers are fixed-width binary. There is no text at all.
- In the **MessagePack and protobuf decoders**, a decoded number is rendered to text with the standard library.
  This is the one place a fast formatter would help, and the measured gap says so. It has not been done: adding
  `ryu` and `itoa` as dependencies conflicts with the dependency goal above, so the honest options are a small
  zero-dependency integer formatter (the digit-pair table approach is about a page) or leaving it. It is listed
  under limits.

The philosophy is that the cheapest number format is none: keep the representation the data already has, and
convert only at a boundary that needs text.

## What wirefold gives up

- **Ecosystem.** Nothing outside wirefold understands its formats, and it does not implement serde's traits.
- **Compression.** `compress()` is slower than zstd -1 (about 60 times to encode, 1.7 times to decode) and
  slightly larger on the structured telemetry data. Use zstd on `Collapse` bytes when compression matters.
- **rkyv's validated column read** is faster in the measured case.
- **MessagePack** encode and decode are slower than `rmp-serde`.
- **Owned parsing** is sequential, as above.
- **One platform floor.** Parallel paths assume apeiron's pool; with one thread they run the same code
  sequentially (`HASHTRINITY_POOL_WIDTH=1`), so behaviour is the same and only speed differs.

If a result in BENCHMARKS.md contradicts something here, the benchmark is the authority.
