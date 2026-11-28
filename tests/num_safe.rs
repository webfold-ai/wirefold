//! The number formatters are safe Rust, enforced twice: the compiler (`#![forbid(unsafe_code)]` on
//! `num`) and this scan, which is a verify step run by `cargo test`. It fails on the escape-hatch
//! tokens anywhere under `src/num`, comments included, so a variant "behind a feature" cannot land
//! either. The only allowed occurrence of the word is the forbid attribute itself.

use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("read src/num") {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn num_sources_contain_no_escape_hatches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/num");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(
        files.len() >= 6,
        "expected the num sources, found {files:?}"
    );
    let banned = [
        "unsafe",
        "get_unchecked",
        "from_utf8_unchecked",
        "write_unaligned",
        "read_unaligned",
        "transmute",
        "MaybeUninit",
        "assume_init",
        "*const ",
        "*mut ",
        "extern \"",
    ];
    let mut hits = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        let text = text.replace("forbid(unsafe_code)", "");
        for (i, line) in text.lines().enumerate() {
            for b in banned {
                if line.contains(b) {
                    hits.push(format!("{}:{}: `{b}`", f.display(), i + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "escape hatches in src/num:\n{}",
        hits.join("\n")
    );
}

#[test]
fn num_module_forbids_unsafe_code() {
    let m =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/num/mod.rs")).unwrap();
    assert!(
        m.contains("#![forbid(unsafe_code)]"),
        "src/num/mod.rs must carry #![forbid(unsafe_code)]"
    );
}

#[test]
fn num_has_no_feature_gated_variant() {
    let cargo =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    assert!(
        !cargo.contains("num-unchecked"),
        "no unchecked variant may be offered behind a feature"
    );
}
