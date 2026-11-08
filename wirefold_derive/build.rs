//! This crate `#[path]`-includes `hashtrinity/src/proc_masq.rs`, which `#[macro_export]`s a few `macro_rules!`
//! — illegal from a `proc-macro` crate type. The shared file guards those exports behind
//! `#[cfg_attr(not(procmasq_no_export), macro_export)]`; set that cfg here (and register it so it is
//! not an "unexpected cfg") so the proc-macro build skips the exports while `hashtrinity`'s own
//! (normal-crate) build keeps them.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(procmasq_no_export)");
    println!("cargo:rustc-cfg=procmasq_no_export");
}
