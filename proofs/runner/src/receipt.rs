//! The proof receipt: digests of every file the proof depends on, the theorems that were audited
//! and the axioms each one uses. `check` writes it after a full green run; `tests/proofs_gate.rs`
//! recomputes the digests under plain `cargo test` and fails when the receipt is stale, so a change
//! to the verified Rust (or to a proof) cannot pass `cargo test` until `check` has been re-run.
//!
//! The receipt is evidence of a run, not a proof by itself. The proof is the Lean kernel check that
//! `check` performed; the receipt binds that run to these exact bytes.

use crate::sha256::sha256_hex;
use std::fs;
use std::path::Path;

/// Axioms every Lean development may use.
pub const ALLOWED_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

/// Tokens that must not appear in hand-written proof sources.
pub const FORBIDDEN: [&str; 8] = [
    "sorry",
    "native_decide",
    "bv_decide",
    "ofReduceBool",
    "implemented_by",
    "@[extern",
    "unsafe",
    "axiom ",
];

const FIXED: [&str; 12] = [
    "src/num/itoa_core.rs",
    "proofs/extract/lib.rs",
    "proofs/extract/Cargo.toml",
    "proofs/lean/Wirefold.lean",
    "proofs/runner/Cargo.toml",
    "proofs/lean/lakefile.lean",
    "proofs/lean/lake-manifest.json",
    "proofs/lean/lean-toolchain",
    "proofs/PINS",
    "proofs/runner/src/main.rs",
    "proofs/runner/src/receipt.rs",
    "proofs/runner/src/sha256.rs",
];

/// Every file the receipt binds, as repo-relative paths, sorted.
pub fn bound_files(root: &Path) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = FIXED.iter().map(|s| s.to_string()).collect();
    let dir = root.join("proofs/lean/Wirefold");
    let rd = fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for ent in rd {
        let ent = ent.map_err(|e| e.to_string())?;
        let name = ent.file_name().to_string_lossy().to_string();
        if name.ends_with(".lean") {
            v.push(format!("proofs/lean/Wirefold/{name}"));
        }
    }
    v.sort();
    Ok(v)
}

pub fn digests(root: &Path) -> Result<Vec<(String, String)>, String> {
    bound_files(root)?
        .into_iter()
        .map(|rel| {
            let bytes = fs::read(root.join(&rel)).map_err(|e| format!("read {rel}: {e}"))?;
            Ok((rel, sha256_hex(&bytes)))
        })
        .collect()
}

/// `#print axioms NAME` lines of `proofs/lean/Wirefold/Audit.lean`, in file order.
pub fn audited_theorems(root: &Path) -> Result<Vec<String>, String> {
    let p = root.join("proofs/lean/Wirefold/Audit.lean");
    let text = fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    Ok(text
        .lines()
        .filter_map(|l| l.strip_prefix("#print axioms "))
        .map(|s| s.trim().to_string())
        .collect())
}

/// Forbidden tokens in the hand-written Lean (everything but `Extracted.lean`, which Aeneas writes).
pub fn scan_forbidden(root: &Path) -> Result<Vec<String>, String> {
    let mut hits = Vec::new();
    for rel in bound_files(root)? {
        if !rel.ends_with(".lean")
            || rel.ends_with("/Extracted.lean")
            || rel.ends_with("lakefile.lean")
        {
            continue;
        }
        let text = fs::read_to_string(root.join(&rel)).map_err(|e| format!("read {rel}: {e}"))?;
        for (i, line) in text.lines().enumerate() {
            let code = line.split("--").next().unwrap_or("");
            for tok in FORBIDDEN {
                if code.contains(tok) {
                    hits.push(format!("{rel}:{}: forbidden token `{}`", i + 1, tok.trim()));
                }
            }
        }
    }
    Ok(hits)
}

/// `axiom` declarations in the generated file (the verified core must have none).
pub fn extracted_axioms(root: &Path) -> Result<Vec<String>, String> {
    let p = root.join("proofs/lean/Wirefold/Extracted.lean");
    let text = fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    Ok(text
        .lines()
        .filter(|l| l.starts_with("axiom "))
        .map(|l| l.to_string())
        .collect())
}

pub struct Receipt {
    pub digests: Vec<(String, String)>,
    /// (theorem, axioms it depends on)
    pub theorems: Vec<(String, Vec<String>)>,
    pub tools: Vec<(String, String)>,
}

pub fn render(r: &Receipt) -> String {
    let mut s = String::from("# wirefold proof receipt: written by `wirefold-proofs check --write-receipt`. Do not edit.\n");
    for (t, v) in &r.tools {
        s.push_str(&format!("tool {t} {v}\n"));
    }
    for (p, d) in &r.digests {
        s.push_str(&format!("digest {p} {d}\n"));
    }
    for (t, axs) in &r.theorems {
        let a = if axs.is_empty() {
            "-".to_string()
        } else {
            axs.join(",")
        };
        s.push_str(&format!("theorem {t} {a}\n"));
    }
    s
}

pub fn parse(text: &str) -> Receipt {
    let mut r = Receipt {
        digests: vec![],
        theorems: vec![],
        tools: vec![],
    };
    for line in text.lines() {
        let mut it = line.splitn(3, ' ');
        match (it.next(), it.next(), it.next()) {
            (Some("digest"), Some(p), Some(d)) => r.digests.push((p.to_string(), d.to_string())),
            (Some("theorem"), Some(t), Some(a)) => {
                let axs = if a == "-" {
                    vec![]
                } else {
                    a.split(',').map(str::to_string).collect()
                };
                r.theorems.push((t.to_string(), axs));
            }
            (Some("tool"), Some(t), Some(v)) => r.tools.push((t.to_string(), v.to_string())),
            _ => {}
        }
    }
    r
}

/// The hermetic freshness check: no Lean toolchain needed. Returns the receipt when it binds the
/// current bytes, every audited theorem is listed with allowed axioms only, and the hand-written
/// Lean has no forbidden token.
pub fn verify_fresh(root: &Path) -> Result<Receipt, String> {
    let text = fs::read_to_string(root.join("proofs/RECEIPT")).map_err(|e| {
        format!("proofs/RECEIPT missing ({e}); run `wirefold-proofs check --write-receipt`")
    })?;
    let r = parse(&text);
    let now = digests(root)?;
    if r.digests != now {
        let stale: Vec<&str> = now
            .iter()
            .filter(|n| !r.digests.contains(n))
            .map(|n| n.0.as_str())
            .collect();
        return Err(format!("proofs/RECEIPT is stale for: {stale:?}; re-run `wirefold-proofs check --write-receipt`"));
    }
    let want = audited_theorems(root)?;
    let have: Vec<&String> = r.theorems.iter().map(|t| &t.0).collect();
    if want.iter().collect::<Vec<_>>() != have {
        return Err(format!(
            "receipt theorems {have:?} differ from Audit.lean {want:?}"
        ));
    }
    for (t, axs) in &r.theorems {
        for a in axs {
            if !ALLOWED_AXIOMS.contains(&a.as_str()) {
                return Err(format!("theorem {t} depends on non-standard axiom {a}"));
            }
        }
    }
    let hits = scan_forbidden(root)?;
    if !hits.is_empty() {
        return Err(hits.join("\n"));
    }
    let ax = extracted_axioms(root)?;
    if !ax.is_empty() {
        return Err(format!("generated Lean declares axioms: {ax:?}"));
    }
    Ok(r)
}
