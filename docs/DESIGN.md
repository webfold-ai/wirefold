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
3. **Coordinate tables** (`CoordMap`). Keys whose space is small enough to be mostly occupied get a direct
   index. The context-mixing coder's low orders use it; its sparse high orders use a table of its own (below),
   because apeiron's `FreqMap` has no slot handle and hashes per key, and the coder needs one cache line per
   nibble. The shape is chosen per order from the input size.
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

- **`#[derive(Collapse)]` lays a `Vec<T>` out as columns.** A column of `f64` is a contiguous run of bytes in the
  file. Opening the buffer validates the layout without walking rows, and reading one column is a slice, not a
  decode. Summing one field of 100,000 telemetry records reads only that field's bytes.
- **Text formats go through one universal `SyntaxTerm`.** JSON, YAML, TOON, POML, CSV and others parse into the
  same tree and render from it, so a conversion is parse then render, not a per-pair adapter.
- **Parsers are generic over a `Build` trait** with an owned builder (`SyntaxTerm`) and a borrowed one
  (`SyntaxTermRef`, which slices into the input). One parser body produces both.

The cost is that wirefold's formats are its own. If you need to emit a format wirefold does not have, or you rely
on the serde ecosystem, serde is the right choice.

## Why not bincode, postcard, rkyv

These are row-wise (bincode, postcard) or archive-in-place (rkyv). They are fast and small, and the benchmark
shows them winning some rows: bincode and rkyv decode a full vector of records as fast as or faster than
wirefold, and rkyv's validated column read beats wirefold's. What columnar layout buys is the case those formats
cannot serve cheaply, reading one field of many records without touching the rest, and a layout that a
byte-oriented coder compresses better because like values sit together (the compressed telemetry row).

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

## The context-mixing coder

The `cm` coder predicts each bit from many contexts and mixes them. It is the slowest part of the crate on
purpose and the benchmark says so: it is about 7 microseconds per byte on compress. It earns its place on size
(telemetry records at 7.4 bytes each against 10.0 for zstd -19), and it chooses among three faces (cm, lz,
hybrid) through `compress_shaped`.

Where the time was, and what changed (all measured, all lossless; sizes within 0.1% of before on a mixed set of
inputs, and every case round-trips):

- **One cache miss per bit became one per nibble.** Each of the 8 bits of a byte used to hash `(context, node)`
  into a table on its own, a random probe per bit for each of about ten context lanes, and then probed again to
  bump the count it had just read. The diffuse tables are now `NibbleTable`s: the context and the nibble's
  starting node pick one 64-byte bucket, found once per nibble per lane, holding the 15 counts for that nibble's
  nodes and a 32-bit tag of the owning context. The read and the later bump of a bit use the same cell, so there is
  no second probe, and the four bits of a nibble share one miss. A tag collision (2^-32 per probe) costs a hair of
  ratio, never correctness, because encode and decode build the table from the same coded prefix. Instruction
  count fell 24%; wall-clock fell more than 2x because the work was memory-bound.
- **Independent passes run as one static batch.** The stride search tries the raw stream and each proposed 2-D
  stride, and `compress_shaped` measures the cm, lz and hybrid faces on the input. Each is an independent whole
  coding pass, so they run on the pool and the winner is picked in the original order: the choice is identical.
  The faces run concurrently only below two chunks of input; above that `compress_at` already spreads the cm face
  over the pool and the faces run in turn.

The numbers are in [BENCHMARKS.md](BENCHMARKS.md). The coder is still the slowest part of the crate: a flat
profile of about 4,000 instructions per coded bit (twelve lanes of floating-point mixing, three APM stages, state
maps) with no single hot spot left, so another large gain would mean a different arithmetic (integer
lpaq-style mixing), which would change the format and is not attempted here. A single stream cannot be coded on
several cores without restarting the model, and the chunked path documents what that costs in ratio, so the
chunk size is left alone. The predictor's dense low orders still use `CoordMap`.

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
- **cm speed.** Compress is about 20 times slower than zstd -19 and decompress thousands of times slower, even
  after the changes above.
- **rkyv's validated column read** is faster in the measured case.
- **MessagePack** encode and decode are slower than `rmp-serde`.
- **Owned parsing** is sequential, as above.
- **One platform floor.** Parallel paths assume apeiron's pool; with one thread they run the same code
  sequentially (`HASHTRINITY_POOL_WIDTH=1`), so behaviour is the same and only speed differs.

If a result in BENCHMARKS.md contradicts something here, the benchmark is the authority.
