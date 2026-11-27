# Composition — one buffer, three faces

wirefold ships three things that are easy to read as alternatives and are not:

| face | what it gives | where |
|---|---|---|
| **speed** | `web`: `[count: u64 LE][T; count]` over `Pod` nodes — `pack` is a memcpy, a read is a coordinate, validation is length + alignment (`Web.encircle_iff`) | `codec::web` (`web::Seed`, `pack`, `pod_bytes`, `collapse_open`) |
| **size** | `cm`: order-k context-mixing + tANS, r5 ≈ 1.93 b/B on Lean source | `codec::cm::{compress, decompress}` |
| **surfaces** | `syntax`: one `SyntaxTerm` over JSON / YAML / TOON / POML / RDF, plus proto schema encode/decode | `codec::syntax::{encode, decode, render_surface, parse_surface}` |

`codec::web`'s own module doc names the constraint that makes this a design question rather than a
preference: *"this is the speed face, **mutually exclusive** with the entropy-coded size face."* One
buffer cannot be both packed and compressed. So the question is not which face to choose. It is
**where each face attaches**.

The answer, in five rules, is that the speed face owns the middle of the system and the other two
attach at its edges.

## R1 — the canonical form is one `web` buffer, and it *is* the address

A stored object is a packed `web` seed, or — when a field is variable-length — a section table:
`Pod` scalars in one array, strings in one heap section, offset and length as columns.

```rust
// one array, memcpy, no per-element work
#[repr(C)]
struct Entry { blob: [u8; 32], mode: u32, path_off: u32, path_len: u32 }
// one section, always canonical
heap: b"src/main.rs" b"README.md" ...
```

Consequences, all of which are properties of the layout rather than of any code that reads it:

* **validation is O(1)** — length identity and alignment, never a structural walk;
* **a field read is O(1)** — offset arithmetic, not a decode, because the stored bytes are the
  runtime layout (`storage = execution`);
* **node `i` is independent of node `j`** (`Web.node_independent`), so a node can be `pread` at its
  own coordinate instead of loading the whole blob;
* **the content address is the hash of these bytes** — one identity, whatever the caller does next.

## R2 — the size face is a transport transform on that same buffer, never a second layout

`cm::compress` at the write boundary; `try_decompress` on load. Because the address hashes the
**canonical, pre-compression** bytes, compression cannot perturb identity: two peers that store the
same object under different compression settings still agree on its address, and a recompression
never mints a new object.

Decompression is O(bytes), paid once per load and never per access.

Measured, not assumed: re-running the serialization bench on a second machine reproduced every
serialized size to the byte for 44 of 45 rows, while the entropy-coded form of an identical input
shifted by 3 bytes (222540 -> 222543). A compressed byte stream is not a stable address; the
canonical bytes are (`docs/BENCHMARKS.md`, re-run 2026-11-27). So "mutually exclusive" becomes
a per-buffer decision made at the edge — cold bulk gets `cm`, hot and randomly-read planes stay
packed — and the two never meet inside a hot path.

## R3 — the surfaces face lives only at the boundary

`SyntaxTerm` is never the stored form. Text enters through `parse_surface` once (ingest) and leaves
through `render_surface` once (egress). Nothing in the middle converts.

The failure this rule forbids is subtle and common: storing a `SyntaxTerm`, or storing a
hand-rolled text form (`path\tblob\tmode`), and therefore paying an O(n) parse on **every** read.
A text form is a surface. Surfaces are where data crosses to a human or a foreign system.

## R4 — typed and ontology access is a plan, resolved once

A proto schema binds column offsets and section positions once per `(schema, version)`. After that
every field is O(1) by offset and every entity lookup is O(1) through a `Table` index. The plan is
cached under the schema's own content address, so it needs no invalidation and no TTL — the same
argument `R2` makes for compression.

## R5 — never compose the faces in the middle

`encode → SyntaxTerm → encode`, or `decompress → render → parse` per row, is exactly how an O(1)
access becomes O(n). Each face attaches at an edge and is applied once per batch, never once per
field.

## What it costs

| operation | shape |
|---|---|
| read one field at one coordinate | **O(1)** — offset arithmetic, no decode |
| validate a buffer | **O(1)** — length + alignment, not a walk |
| scan n rows of one column | O(n) sequential, `pread`-able per node |
| lookup by key or entity id | **O(1)** — `Table` / plan index |
| load a compressed chunk | O(bytes), once per load, never per access |
| render one row to a surface, or ingest one | O(that row's bytes) — inherent, boundary only |
| **add the second and third face** | **no extra pass in any hot path** — only edge transforms proportional to the bytes crossing the edge |

The middle of a system built this way is only ever the speed face. All three faces, unchanged order.

## Per plane

| plane | canonical | size face | surfaces | typed access |
|---|---|---|---|---|
| **rows / columns** | packed `web` rows per chunk; `Pod` columns include the integer columns (mode, flags, ts) | `cm` on cold partitions only | JSON / TOON at the API edge | column offsets from the schema plan |
| **agent sessions / events** | one row per record; `source_id`, `session`, `turn` are fixed columns | `cm` for archived transcripts | JSONL in, JSON / TOON out | "what happened before this commit" is a key → row, O(1) |
| **ontology** | `(kind, id) → object` index; properties in the packed buffer | `cm` for imported catalogues | OWL / Turtle / JSON-LD only through the RDF surface | adjacency in a `Table<(from, to), Relationship>` |
| **typed API** | the same buffer | — | rendered from the buffer, no intermediate AST | field by index |

## Worked example — a repository's tree manifest

A tree manifest is the case where the wrong face is tempting: it reads like text, so people store
text. Stored as text, every commit and every attribution step pays an O(n) parse, and a one-file
change rewrites the whole manifest.

Per R1 it is a section table: `Entry` above, one array, one path heap. Then `mode` is an integer
column, `blob` a fixed 32-byte column (32 bytes because the store's own content address is 32 bytes
— the manifest names the object, it does not copy it), `tree.get(path)` is an index hit O(1), a
listing is a `render_surface` of the same buffer rather than a parse, and the commit identity is
the hash of those bytes.

Measured on the prototype that motivated this note (20 commits × 200 files):
**279 objects for the whole history — 219 blobs, 40 trees, 20 commits** — because unchanged content
resolves to the same address across commits instead of being re-emitted per commit.

## What this page does not claim

* It does not claim `cm` beats gzip on your data. The 1.93 b/B figure is this repository's own
  measured result on Lean source; treat it as that.
* It does not claim the three faces are free to add. They are free in *asymptotics*; each edge
  transform still costs bytes in proportion to what crosses it.
* It does not claim a section table is always right. For a fixed-size record `web::pack` alone is
  the whole story; the section table exists for variable-length fields, and a heap section is the
  price of them.
