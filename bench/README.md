# wirefold-bench

wirefold against serde_json, bincode, postcard, rmp-serde and rkyv on the same records, plus zstd -19 as
the usual "serialize, then compress" baseline. It lives outside the library workspace (it has its own
`[workspace]`) so the library stays dependency-free.

```bash
cd bench
cargo run --release -- 100000      # number of records; default 100000
```

It prints markdown tables. Every encoder is round-trip checked before it is timed, and timings are the
median of 9 runs (fewer for the slow coders). Run it on an otherwise idle machine: other CPU work changes
the numbers. The cm coder takes tens of seconds per dataset, so a full run takes about five minutes.

Parser speed for every text surface, with serde_json as the JSON reference:

```bash
cargo run --release -- parse            # all surfaces, 20000 records
cargo run --release -- parse yaml       # one surface per process (the fair way to compare surfaces)
cargo run --release -- parse serde 100000   # the serde_json reference alone, any record count
```

Results and how to read them: [`../docs/BENCHMARKS.md`](../docs/BENCHMARKS.md).

## What it measures

- **Two datasets.** Telemetry-shaped records (increasing timestamps, small domains, repeated names) and
  random records, the worst case for a columnar model.
- **Encode, decode all, size**, for each format.
- **Reading one column** of a packed buffer without decoding the rest (wirefold and rkyv read in place;
  the serde formats decode everything first).
- **JSON text into a generic tree**: wirefold's `parse_surface(Json)` against `serde_json::Value`.

## What it does not measure

Only one record shape, one machine, one run. serde_json is built with `float_roundtrip` so floats are
exact. rkyv is measured with validation on. There is no typed JSON deserialisation on the wirefold side
(it builds a generic tree), so the JSON row is parser against parser, not a full serde replacement.
