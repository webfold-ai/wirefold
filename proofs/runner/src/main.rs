//! `wirefold-proofs`: the proof pipeline as one Rust program.
//!
//!   setup                 install the pinned Aeneas release archive under `.box/aeneas` (or verify it)
//!   regen [--check]       Charon (Rust -> LLBC) then Aeneas (LLBC -> Lean); with --check, fail on drift
//!   lean                  `lake build` over the generated Lean and the hand-written theorems, then the axiom audit
//!   check [--write-receipt]  setup + regen --check + lean; with the flag, write proofs/RECEIPT when all green
//!
//! Every failure to find a tool, a pin mismatch, a `sorry`, a forbidden token or a non-standard axiom
//! is an error and a non-zero exit. Nothing is skipped silently.

mod receipt;
mod sha256;

use receipt::{Receipt, ALLOWED_AXIOMS};
use sha256::sha256_hex;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type R<T> = Result<T, String>;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn home(root: &Path) -> PathBuf {
    match std::env::var("WIREFOLD_AENEAS_HOME") {
        Ok(p) => PathBuf::from(p),
        Err(_) => root.join(".box/aeneas"),
    }
}

fn pins(root: &Path) -> R<BTreeMap<String, String>> {
    let text = fs::read_to_string(root.join("proofs/PINS")).map_err(|e| format!("proofs/PINS: {e}"))?;
    Ok(text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect())
}

fn pin<'a>(p: &'a BTreeMap<String, String>, k: &str) -> R<&'a str> {
    p.get(k).map(String::as_str).ok_or(format!("proofs/PINS has no {k}"))
}

/// Run a command, echo its output, return (success, combined output).
fn run(prog: &Path, args: &[&str], cwd: &Path, envs: &[(&str, String)]) -> R<(bool, String)> {
    println!("$ (cd {} && {} {})", cwd.display(), prog.display(), args.join(" "));
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("cannot run {}: {e}", prog.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    print!("{text}");
    Ok((out.status.success(), text))
}

fn sh(prog: &str) -> PathBuf {
    PathBuf::from(prog)
}

fn tool_ids(home: &Path) -> R<Vec<(String, String)>> {
    let mut v = Vec::new();
    for t in ["aeneas", "charon", "charon-driver"] {
        let b = fs::read(home.join(t)).map_err(|e| format!("tool {}: {e}", home.join(t).display()))?;
        v.push((format!("{t}-sha256"), sha256_hex(&b)));
    }
    Ok(v)
}

fn setup(root: &Path) -> R<()> {
    let p = pins(root)?;
    let home = home(root);
    if !home.join("aeneas").exists() {
        let dl = home.with_extension("tar.gz");
        fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        let (ok, _) = run(&sh("curl"), &["-fsSL", "-o", &dl.to_string_lossy(), pin(&p, "AENEAS_ARCHIVE_URL")?], root, &[])?;
        if !ok {
            return Err("download of the pinned Aeneas archive failed".into());
        }
        let got = sha256_hex(&fs::read(&dl).map_err(|e| e.to_string())?);
        if got != pin(&p, "AENEAS_ARCHIVE_SHA256")? {
            return Err(format!("archive sha256 {got} != pinned {}", pin(&p, "AENEAS_ARCHIVE_SHA256")?));
        }
        let (ok, _) = run(&sh("tar"), &["xzf", &dl.to_string_lossy(), "-C", &home.to_string_lossy()], root, &[])?;
        if !ok {
            return Err("unpacking the Aeneas archive failed".into());
        }
    }
    // identity: Charon must be the pinned commit and the pinned rustc must be installed
    let (ok, ver) = run(&home.join("charon"), &["version"], root, &[])?;
    if !ok || !ver.contains(&pin(&p, "CHARON_COMMIT")?[..40]) {
        return Err(format!("charon is not the pinned commit {}: {ver}", pin(&p, "CHARON_COMMIT")?));
    }
    let (ok, tc) = run(&home.join("charon"), &["toolchain-version"], root, &[])?;
    if !ok || tc.trim() != pin(&p, "RUST_TOOLCHAIN")? {
        return Err(format!("charon wants {} but PINS says {}", tc.trim(), pin(&p, "RUST_TOOLCHAIN")?));
    }
    println!("setup: OK ({})", home.display());
    Ok(())
}

fn roots(root: &Path) -> R<Vec<String>> {
    let text = fs::read_to_string(root.join("proofs/extract/lib.rs")).map_err(|e| e.to_string())?;
    let v: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("pub fn "))
        .filter_map(|l| l.split('(').next())
        .filter(|n| n.starts_with("w_"))
        .map(|n| n.to_string())
        .collect();
    if v.is_empty() {
        return Err("proofs/extract/lib.rs has no w_* extraction roots".into());
    }
    Ok(v)
}

fn regen(root: &Path, check: bool) -> R<()> {
    let home = home(root);
    let out = root.join(".box");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let llbc = out.join("Extracted.llbc");
    let _ = fs::remove_file(&llbc);
    let mut args: Vec<String> = vec!["cargo".into(), "--preset".into(), "aeneas".into(), "--dest-file".into(), llbc.to_string_lossy().into()];
    for r in roots(root)? {
        args.push("--start-from".into());
        args.push(format!("crate::{r}"));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // Charon records `Source:` paths relative to its working directory; pin it.
    let (ok, _) = run(&home.join("charon"), &a, &root.join("proofs/extract"), &[])?;
    if !ok || !llbc.exists() {
        return Err("charon failed (see output above)".into());
    }
    let dest = out.join("lean-out");
    let _ = fs::remove_dir_all(&dest);
    let (ok, log) = run(
        &home.join("aeneas"),
        &["-backend", "lean", "-dest", &dest.to_string_lossy(), "-no-progress-bar", "-max-recdepth", "20000", "-max-heartbeats", "4000000", &llbc.to_string_lossy()],
        root,
        &[],
    )?;
    if !ok || log.contains("Error") && !log.contains("Generated") {
        return Err("aeneas failed (see output above)".into());
    }
    let generated = fs::read_to_string(dest.join("Extracted.lean")).map_err(|e| format!("aeneas wrote no Extracted.lean: {e}"))?;
    let target = root.join("proofs/lean/Wirefold/Extracted.lean");
    if check {
        let committed = fs::read_to_string(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        if committed != generated {
            let n = committed.lines().zip(generated.lines()).position(|(x, y)| x != y).unwrap_or(0) + 1;
            return Err(format!("DRIFT: committed Extracted.lean differs from a fresh extraction (first difference at line {n}); run `wirefold-proofs regen`"));
        }
        println!("regen --check: OK (extraction is byte-identical to the committed Lean)");
    } else {
        fs::write(&target, generated).map_err(|e| e.to_string())?;
        println!("regen: wrote {}", target.display());
    }
    Ok(())
}

fn lean(root: &Path) -> R<Vec<(String, Vec<String>)>> {
    // hand-written sources first: cheap and decisive
    let hits = receipt::scan_forbidden(root)?;
    if !hits.is_empty() {
        return Err(hits.join("\n"));
    }
    let ax = receipt::extracted_axioms(root)?;
    if !ax.is_empty() {
        return Err(format!("generated Lean declares axioms: {ax:?}"));
    }
    let wanted = receipt::audited_theorems(root)?;
    if wanted.is_empty() {
        return Err("proofs/lean/Wirefold/Audit.lean audits no theorem".into());
    }
    let dir = root.join("proofs/lean");
    let path = format!("{}/.elan/bin:{}", std::env::var("HOME").unwrap_or_default(), std::env::var("PATH").unwrap_or_default());
    let envs = [("PATH", path)];
    let (ok, log) = run(&sh("lake"), &["build"], &dir, &envs)?;
    if !ok {
        return Err("lake build failed (see output above)".into());
    }
    if log.contains("declaration uses 'sorry'") || log.contains("sorry") {
        return Err("lake build output mentions `sorry`".into());
    }
    // `lake build` replays cached module output; the audit module prints on every full build, but a
    // cached module prints nothing, so re-elaborate the audit file directly for the axiom lists.
    let (ok, audit) = run(&sh("lake"), &["env", "lean", "Wirefold/Audit.lean"], &dir, &envs)?;
    if !ok {
        return Err("axiom audit did not elaborate".into());
    }
    let mut out = Vec::new();
    for t in &wanted {
        let key = format!("'{t}' ");
        let line = audit
            .lines()
            .find(|l| l.contains(&key))
            .ok_or(format!("audit printed nothing for theorem {t}"))?;
        let axs: Vec<String> = match line.split_once("depends on axioms: [") {
            Some((_, rest)) => rest.trim_end_matches(']').split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            None if line.contains("does not depend on any axioms") => vec![],
            None => return Err(format!("unparseable audit line for {t}: {line}")),
        };
        for a in &axs {
            if !ALLOWED_AXIOMS.contains(&a.as_str()) {
                return Err(format!("theorem {t} depends on non-standard axiom {a}"));
            }
        }
        println!("audit: {t}: {}", if axs.is_empty() { "no axioms".into() } else { axs.join(", ") });
        out.push((t.clone(), axs));
    }
    Ok(out)
}

fn check(root: &Path, write: bool) -> R<()> {
    setup(root)?;
    regen(root, true)?;
    let theorems = lean(root)?;
    println!("check: GREEN ({} theorems audited, standard axioms only, no sorry)", theorems.len());
    if write {
        let r = Receipt { digests: receipt::digests(root)?, theorems, tools: tool_ids(&home(root))? };
        fs::write(root.join("proofs/RECEIPT"), receipt::render(&r)).map_err(|e| e.to_string())?;
        println!("check: wrote proofs/RECEIPT");
    }
    Ok(())
}

fn main() {
    let root = root();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let res = match args.first().map(String::as_str) {
        Some("setup") => setup(&root),
        Some("regen") => setup(&root).and_then(|_| regen(&root, flag("--check"))),
        Some("lean") => lean(&root).map(|_| ()),
        Some("check") => check(&root, flag("--write-receipt")),
        _ => Err("usage: wirefold-proofs <setup | regen [--check] | lean | check [--write-receipt]>".into()),
    };
    if let Err(e) = res {
        eprintln!("wirefold-proofs: FAIL: {e}");
        std::process::exit(1);
    }
}
