# Proofs of the number formatters

Charon extracts the safe Rust core to LLBC, Aeneas translates LLBC to Lean, Lean checks the theorems.
The pipeline is one Rust program, `wirefold-proofs` (`proofs/runner`), so there is no shell or Python
glue to trust.

| Piece | Where | Role |
|---|---|---|
| extraction root | `proofs/extract/lib.rs` | `#[path]`-includes the real `src/num/itoa_core.rs` (and `float_core.rs`, `float_tables.rs`); `w_*` wrappers are the Charon roots |
| generated Lean | `proofs/lean/Wirefold/Extracted.lean` | Aeneas output, committed; `regen --check` fails on drift |
| theorems | `proofs/lean/Wirefold/{Itoa,Float}.lean` | hand-written, no `sorry`, no extra axioms |
| axiom audit | `proofs/lean/Wirefold/Audit.lean` | `#print axioms` for every theorem the gate requires |
| pins | `proofs/PINS` | Aeneas release archive URL and sha256, Charon commit, Rust and Lean toolchains, Mathlib revision |
| receipt | `proofs/RECEIPT` | sha256 of every bound file plus the audited theorems and their axioms; written only by a green `check --write-receipt` |

## Commands

```sh
cargo run --manifest-path proofs/runner/Cargo.toml --release -- setup          # install/verify the pinned toolchain under .box/aeneas
cargo run --manifest-path proofs/runner/Cargo.toml --release -- regen --check  # Charon + Aeneas, fail on drift
cargo run --manifest-path proofs/runner/Cargo.toml --release -- lean           # lake build + axiom audit
cargo run --manifest-path proofs/runner/Cargo.toml --release -- check --write-receipt   # all of it
```

Every step fails (non-zero exit) when a tool is missing, a pin differs, `lake build` fails, a `sorry`
or a forbidden token (`native_decide`, `bv_decide`, `ofReduceBool`, `implemented_by`, an `axiom`
declaration) appears in the hand-written Lean, or a theorem depends on an axiom other than
`propext`, `Classical.choice`, `Quot.sound`. Nothing is skipped silently.

## What `cargo test` checks without the toolchain

`tests/proofs_gate.rs` recomputes the sha256 of every bound file and compares it with `proofs/RECEIPT`.
Editing the verified Rust (`src/num/itoa_core.rs`, `float_core.rs`, `float_tables.rs`), the extraction
root, the generated Lean, a proof or the pins makes `cargo test` fail until `check --write-receipt` has
been re-run on a machine with the toolchain. That makes the Lean kernel check a precondition of a green
`cargo test`, without putting a Lean toolchain on every developer machine. The receipt is evidence of a
run, not a proof; the proof is the Lean check the receipt records.

## Toolchain

The Aeneas release `nightly-2026.09.09-505b6ca` is used as published (archive sha256 pinned in
`PINS`), not built from source: its Charon is commit `b104e24` and `rustc nightly-2026-08-18` is what it
asks for. Apeiron's `proof/bootstrap.sh` pins the same release and Charon commit and additionally builds
patched tools for kernels this code does not use (array repeat with an optional trait, `Vec` capacity);
no patch is applied here. Lean is `v4.31.0` and Mathlib `fabf563` (the revision the Aeneas backend
requires). The Aeneas Lean backend and a built Mathlib are large (about 8 GB): build and check on the
build box, not on a nearly full laptop.

Two things the extraction taught, kept as constraints on the Rust:

- A literal `[[u8; 2]; 100]` becomes a Lean term whose elaboration exhausts memory (more than 20 GB,
  killed); the same table as a flat `[u8; 200]` elaborates in seconds. All tables are flat.
- `unsigned_abs` and similar core methods are extracted as `axiom`s. The core uses plain arithmetic
  instead, so the generated file declares no axiom at all.

## Statement of what is and is not proved

See the table at the end of this file once the theorems are in; it is regenerated with the receipt.
