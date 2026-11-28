//! The proof gate under plain `cargo test` (the harness's blocking test step).
//!
//! The Lean proofs themselves are checked by `wirefold-proofs check` (Charon, Aeneas, `lake build`,
//! axiom audit), which needs the pinned toolchain and minutes of build time. This test is the part
//! that needs no toolchain: it recomputes the SHA-256 of every file the proof depends on (the Rust
//! core that is extracted, the extraction root, the generated and hand-written Lean, the pins and the
//! runner) and compares them with `proofs/RECEIPT`, which `wirefold-proofs check --write-receipt`
//! writes only after a green run. A change to the verified Rust, or to any proof, makes this test
//! fail until the full check has been re-run. It also re-checks, from the receipt, that every audited
//! theorem uses standard axioms only, and re-scans the hand-written Lean for forbidden tokens.
//!
//! It does NOT replay the Lean kernel check: that is `wirefold-proofs check`, and where that has not
//! been run the receipt is stale or missing and this test fails. A missing toolchain is never a pass.

#[path = "../proofs/runner/src/receipt.rs"]
mod receipt;
#[path = "../proofs/runner/src/sha256.rs"]
#[allow(dead_code)]
mod sha256;

use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn proof_receipt_binds_the_current_sources() {
    match receipt::verify_fresh(root()) {
        Ok(r) => {
            assert!(!r.theorems.is_empty(), "receipt lists no theorem");
            assert!(
                r.tools.iter().any(|t| t.0 == "aeneas-sha256"),
                "receipt records no tool identity"
            );
        }
        Err(e) => panic!("PROOFS NOT DONE: {e}"),
    }
}

#[test]
fn proof_theorems_are_the_required_set() {
    let have = receipt::audited_theorems(root()).expect("Audit.lean");
    for want in [
        "wirefold_extract.fmt_u64_spec",
        "wirefold_extract.fmt_u32_roundtrip",
        "wirefold_extract.fmt_i64_roundtrip",
        "wirefold_extract.pairs_digits",
        "wirefold_extract.divmod100",
    ] {
        assert!(
            have.iter().any(|h| h == want),
            "required theorem {want} is not audited; audited: {have:?}"
        );
    }
}
