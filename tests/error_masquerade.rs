//! Proves the thiserror + anyhow masquerade end to end: `#[derive(Error)]` (macro namespace),
//! `wirefold::Error`/`Result`/`Context` (type namespace) — both resolve at the same crate
//! root with zero conflict, and both behave the way the real crates' callers already expect.

#![cfg(feature = "error")]

use std::fmt;
use wirefold::{Context, Error, Result};

// A minimal std error to wrap/convert from, standing in for `std::io::Error` et al.
#[derive(Debug)]
struct BoringError(&'static str);
impl fmt::Display for BoringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "boring: {}", self.0)
    }
}
impl std::error::Error for BoringError {}

// Exercises: unit variant, named-field variant with `{field}` capture, tuple variant with
// `{0}` capture, tuple variant whose field is unreferenced in the message (the
// "argument never used" trap the positional-index scan exists to dodge), and `#[from]`.
#[derive(Debug, wirefold::Error)]
enum DeployError {
    #[error("missing required input `{name}`")]
    MissingInput { name: String },
    #[error("bad recipe id: {0}")]
    BadRecipe(String),
    #[error("queue unreachable")]
    QueueDown(#[source] BoringError),
    #[error("wrapped: {0}")]
    Wrapped(#[from] BoringError),
    #[error("no queue configured")]
    NoQueue,
}

#[test]
fn display_renders_named_field_capture() {
    let e = DeployError::MissingInput {
        name: "tenant_id".into(),
    };
    assert_eq!(e.to_string(), "missing required input `tenant_id`");
}

#[test]
fn display_renders_tuple_positional_capture() {
    let e = DeployError::BadRecipe("wordpress".into());
    assert_eq!(e.to_string(), "bad recipe id: wordpress");
}

#[test]
fn display_ignores_unreferenced_tuple_field_without_error() {
    // QueueDown's message doesn't reference {0} at all — this only compiles if the derive
    // doesn't blindly pass every tuple field as a positional write! argument.
    let e = DeployError::QueueDown(BoringError("timeout"));
    assert_eq!(e.to_string(), "queue unreachable");
}

#[test]
fn source_reaches_the_sourced_field() {
    let e = DeployError::QueueDown(BoringError("timeout"));
    let src = std::error::Error::source(&e).expect("source() should reach the #[source] field");
    assert_eq!(src.to_string(), "boring: timeout");
}

#[test]
fn from_impl_generated_and_source_wired() {
    let e: DeployError = BoringError("conn refused").into();
    assert_eq!(e.to_string(), "wrapped: boring: conn refused");
    let src = std::error::Error::source(&e).expect("#[from] field is implicitly the source");
    assert_eq!(src.to_string(), "boring: conn refused");
}

#[test]
fn unit_variant_needs_no_fields() {
    assert_eq!(DeployError::NoQueue.to_string(), "no queue configured");
}

// The anyhow masquerade: Result<T> defaults to wirefold::Error, and ? auto-converts any
// std::error::Error (including a #[derive(Error)] enum) through the blanket From impl.
fn parse_port(s: &str) -> Result<u16> {
    // Turbofish needed here specifically: `.parse()`'s own return type is generic, and the
    // compiler can't pick the `E: std::error::Error` vs. `E = Error` Context impl before it
    // knows what E is — a chicken/egg only bare-inferred parses hit, not typed error values.
    let n = s.parse::<u16>().context("parsing the port")?;
    Ok(n)
}

fn deploy(recipe: &str) -> Result<()> {
    if recipe.is_empty() {
        Err(DeployError::BadRecipe(recipe.to_string()))?;
    }
    Ok(())
}

// Proves `Error` (the type) resolves at the same `use wirefold::Error` that also brings the
// derive macro into scope for `#[derive(Error)]` above.
fn explicit_error_type(msg: &'static str) -> Error {
    Error::new(BoringError(msg))
}

#[test]
fn context_wraps_a_real_std_error_with_a_message() {
    let err = parse_port("not-a-port").unwrap_err();
    let rendered = err.to_string();
    assert!(rendered.contains("parsing the port"), "{rendered}");
    assert!(rendered.contains("invalid digit"), "{rendered}");
}

#[test]
fn question_mark_auto_converts_a_derive_error_enum_into_error() {
    let err = deploy("").unwrap_err();
    assert!(err.to_string().contains("bad recipe id:"), "{err}");
    // root_cause reaches the original DeployError even after the blanket From<E> boxed it.
    assert!(err.root_cause().to_string().contains("bad recipe id:"));
}

#[test]
fn error_resolves_as_a_type_from_the_same_import_as_the_derive_macro() {
    let e = explicit_error_type("standalone");
    assert_eq!(e.to_string(), "boring: standalone");
}

#[test]
fn chained_context_prepends_without_rewrapping() {
    let result: Result<u16> = parse_port("nope").context("loading server config");
    let rendered = result.unwrap_err().to_string();
    // Both lines present, outermost context first.
    let loading_pos = rendered.find("loading server config").unwrap();
    let parsing_pos = rendered.find("parsing the port").unwrap();
    assert!(loading_pos < parsing_pos, "{rendered}");
}
