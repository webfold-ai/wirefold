# wirefold-bench

wirefold against serde_json, bincode, postcard, rmp-serde and rkyv on the same records, plus zstd -19 as
the usual "serialize, then compress" baseline. It lives outside the library workspace (it has its own
`[workspace]`) so the library stays dependency-free.

```bash
cd bench
cargo run --release -- 20000       # records [and runs per cell]; default 20000 records, 3 runs
```

It prints markdown tables. Every encoder is round-trip checked before it is timed, and each cell runs in a fresh process and
the table gives the mean of 3 runs with the range. Run it on an otherwise idle machine: other CPU work changes
the numbers. A full run takes about a minute.

Parser speed for every text surface, with serde_json as the JSON reference:

```bash
cargo run --release -- parse            # all surfaces, each in its own process, 20000 records
cargo run --release -- parse yaml       # just one surface
cargo run --release -- parse json-ref   # the borrowing parser (also yaml-ref); parallel over 512 KiB
HASHTRINITY_POOL_WIDTH=1 cargo run --release -- parse json-ref 100000   # the same on one thread
cargo run --release -- parse serde 100000   # the serde_json reference alone, any record count
```

Rendering and the binary codecs, each row in its own process:

```bash
cargo run --release -- render           # every surface rendered from the same tree
cargo run --release -- numbers          # ryu and itoa against std formatting and against keeping the text
cargo run --release -- binary           # MessagePack (against rmp-serde) and protobuf, encode and decode
```

A row varies by about 10% between runs on a shared machine, so compare differences bigger than that.

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
