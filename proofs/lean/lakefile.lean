import Lake
open Lake DSL

package wirefoldProofs where
  leanOptions := #[
    { name := `autoImplicit, value := false },
    { name := `linter.style.nameCheck, value := false }
  ]

-- Aeneas's Lean backend is not vendored. `cargo run --manifest-path proofs/runner/Cargo.toml -- setup`
-- installs the pinned release archive (sha256 in proofs/PINS) under `.box/aeneas`, which is the
-- directory every other command here reads. Mathlib is resolved by Lake from the manifest below.
require aeneas from "../../.box/aeneas/backends/lean"

@[default_target]
lean_lib Wirefold where
  roots := #[`Wirefold]
