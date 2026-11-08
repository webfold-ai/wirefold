//! wirefold: a zero-copy wire format, a universal `Term`, an integer column codec, a
//! context-mixing coder and structured-text surfaces, built on [apeiron](https://github.com/webfold-ai/apeiron).
//!
//! ## Feature flags
//!
//! | Feature | What it adds |
//! |---------|-------------|
//! | `codec` (default) | Zero-copy wire format ([`codec::web`]), universal Term, integer column codec |
//! | `compress` | Context-mixing entropy coder ([`codec::cm`]) |
//! | `syntax` | Collapsed syntax AST ([`codec::syntax`]): JSON, YAML, TOON, POML, RDF and other surfaces |
//! | `derive` | `#[derive(Collapse)]` and `#[derive(Term)]` for columnar zero-copy serialization |
//! | `error` | `Error`, `Result` and `Context`, plus `#[derive(Error)]` |
//! | `validate` | `Validate` trait plus `#[derive(Validate)]` |

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
// Style lints inherited from the code this crate was split from (mostly the RDF surface and the
// codec's reference tests). Allowed by name so `clippy -D warnings` can gate everything else;
// each one is a candidate for a cleanup PR, not a decision that the pattern is right.
#![allow(
    clippy::format_push_string,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::module_inception,
    clippy::empty_line_after_doc_comments,
    clippy::mixed_attributes_style,
    clippy::manual_let_else,
    clippy::used_underscore_items,
    clippy::assigning_clones
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::must_use_candidate,
    clippy::doc_markdown,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::similar_names,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::inline_always,
    clippy::wildcard_imports,
    clippy::many_single_char_names,
    clippy::cast_ptr_alignment
)]

#[cfg(feature = "codec")]
#[allow(missing_docs)]
pub mod codec;

#[cfg(feature = "derive")]
pub use wirefold_derive::Collapse;

#[cfg(feature = "derive")]
pub use wirefold_derive::Term;

#[cfg(feature = "error")]
pub mod error;
#[cfg(feature = "error")]
pub use error::{Context, Error, Result};

#[cfg(feature = "error")]
pub use wirefold_derive::Error;

#[cfg(feature = "validate")]
pub mod validate;

#[cfg(feature = "validate")]
pub use validate::{Validate, ValidationError, ValidationErrors};
#[cfg(feature = "validate")]
pub use wirefold_derive::Validate;

// The table, pool and counters the codec is built on live in apeiron; re-exported at the crate
// root so the codec's `crate::{CoordMap, FreqMap}` and `crate::par` paths resolve.
pub use apeiron::map::hashtrinity::*;
