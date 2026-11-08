//! `#[derive(Collapse)]` — the columnar zero-copy bridge.
//!
//! **Structs** → a columnar record table + zero-copy `TView`: scalar fields (primitives AND nested
//! `Pod` structs) packed into one `Wire<[u8; N]>`; `String` → a `Ragged`; `Vec<U>` → a
//! `Sections`-of-sub-buffers, dispatched through `CollapseElem` (so `U` may be `Pod` OR itself a
//! `Collapse` record — nesting recurses to any depth).
//!
//! **Enums** (Pod-field variants) → a tag `Wire<u8>` + a payload `Ragged`; `TView::get(i)`
//! reconstructs the variant. So a `Vec<Enum>` field of a struct works too (via `CollapseElem`).
//!
//! Every type also gets `impl CollapseElem`, so it can be a `Vec<_>` element of another derive.
//! Build-time only; the runtime crate stays zero-dependency.
//!
//! ## Zero dependencies — the `syn`/`quote`/`proc-macro2` masquerade, self-hosted
//!
//! This crate depends on nothing (`Cargo.toml` has an empty `[dependencies]`). The token/AST
//! model it parses and generates through is `src/proc_masq.rs` in this crate: a table-backed
//! stand-in for `proc_macro`/`syn`/`quote`. The part that needs the real, compiler-native
//! `proc_macro` crate (only a `proc-macro = true` crate gets sysroot access to it) lives in
//! [`pm_bridge`] below.

// `quote_tokens!` recurses once per remaining token (see its module doc); the biggest single
// template here (`derive_struct`'s final `impl`/`struct`/`impl CollapseElem` block) has more
// literal tokens than the default 128-deep limit allows even after the join-helper pre-joining
// collapses every repeated section to one interpolation.
#![recursion_limit = "512"]

use proc_macro::TokenStream;

// `#[macro_use]` (not `#[macro_export]`, forbidden in a proc-macro crate — see the Cargo.toml
// comment): brings `quote_tokens!`/`quote_tokens_impl!`/`parse_input!` into textual scope for the
// rest of this file, since they aren't at the crate root here.
#[macro_use]
#[path = "proc_masq.rs"]
mod proc_masq;

use proc_masq::{Data, DeriveInput, Fields, TokenStream as PTokenStream};

// The actual codegen logic (`derive_struct`, `derive_term_struct`, `parse_path_attr`, ...) — pure
// functions over `proc_masq` types, reusable outside a proc-macro context (an offline codegen
// binary could share this same file via `#[path]`, matching how `proc_masq.rs` itself is shared).
// `use codegen::*;` brings every `pub(crate)` item into scope unqualified, so the thin entry
// points below call `derive_struct(...)` etc. exactly as they did when those functions lived here.
mod codegen;
use codegen::*;

/// The one place this crate touches the real compiler — `proc_macro::TokenStream` (and its
/// `TokenTree` variants) converted to/from `proc_masq`'s own types. `wirefold`'s copy of this
/// file bridges to `proc_macro2` instead (see its module doc); a proc-macro crate has no need for
/// that indirection, since it already has direct sysroot access to the real `proc_macro`.
mod pm_bridge {
    use super::proc_masq::{
        Delimiter as PDelimiter, Group as PGroup, Ident as PIdent, Literal as PLiteral,
        Punct as PPunct, Spacing as PSpacing, Span as PSpan, TokenStream as PTokenStream,
        TokenTree as PTokenTree,
    };

    impl From<proc_macro::TokenTree> for PTokenTree {
        fn from(tt: proc_macro::TokenTree) -> Self {
            match tt {
                proc_macro::TokenTree::Ident(i) => PTokenTree::Ident(PIdent {
                    sym: i.to_string().into(),
                    span: PSpan::call_site(),
                }),
                proc_macro::TokenTree::Literal(l) => PTokenTree::Literal(PLiteral {
                    text: l.to_string().into(),
                    span: PSpan::call_site(),
                }),
                proc_macro::TokenTree::Punct(p) => PTokenTree::Punct(PPunct {
                    ch: p.as_char(),
                    spacing: match p.spacing() {
                        proc_macro::Spacing::Alone => PSpacing::Alone,
                        proc_macro::Spacing::Joint => PSpacing::Joint,
                    },
                    span: PSpan::call_site(),
                }),
                proc_macro::TokenTree::Group(g) => PTokenTree::Group(PGroup {
                    delimiter: match g.delimiter() {
                        proc_macro::Delimiter::Parenthesis => PDelimiter::Parenthesis,
                        proc_macro::Delimiter::Brace => PDelimiter::Brace,
                        proc_macro::Delimiter::Bracket => PDelimiter::Bracket,
                        proc_macro::Delimiter::None => PDelimiter::None,
                    },
                    stream: g.stream().into(),
                    span: PSpan::call_site(),
                }),
            }
        }
    }

    impl From<proc_macro::TokenStream> for PTokenStream {
        fn from(ts: proc_macro::TokenStream) -> Self {
            ts.into_iter().map(PTokenTree::from).collect()
        }
    }

    impl From<PTokenTree> for proc_macro::TokenTree {
        fn from(tt: PTokenTree) -> Self {
            match tt {
                PTokenTree::Ident(i) => proc_macro::TokenTree::Ident(proc_macro::Ident::new(
                    &i.sym,
                    proc_macro::Span::call_site(),
                )),
                PTokenTree::Literal(l) => {
                    // `proc_macro::Literal` has no generic "from raw source text" constructor, but
                    // `TokenStream::from_str` parses exactly this one token back out losslessly.
                    let parsed: proc_macro::TokenStream = l.text.parse().unwrap_or_else(|_| {
                        panic!("wirefold_derive: malformed literal token `{}`", l.text)
                    });
                    let mut it = parsed.into_iter();
                    match (it.next(), it.next()) {
                        (Some(proc_macro::TokenTree::Literal(real)), None) => {
                            proc_macro::TokenTree::Literal(real)
                        }
                        _ => panic!(
                            "wirefold_derive: `{}` is not a single literal token",
                            l.text
                        ),
                    }
                }
                PTokenTree::Punct(p) => proc_macro::TokenTree::Punct(proc_macro::Punct::new(
                    p.ch,
                    match p.spacing {
                        PSpacing::Alone => proc_macro::Spacing::Alone,
                        PSpacing::Joint => proc_macro::Spacing::Joint,
                    },
                )),
                PTokenTree::Group(g) => proc_macro::TokenTree::Group(proc_macro::Group::new(
                    match g.delimiter {
                        PDelimiter::Parenthesis => proc_macro::Delimiter::Parenthesis,
                        PDelimiter::Brace => proc_macro::Delimiter::Brace,
                        PDelimiter::Bracket => proc_macro::Delimiter::Bracket,
                        PDelimiter::None => proc_macro::Delimiter::None,
                    },
                    g.stream.into(),
                )),
            }
        }
    }

    impl From<PTokenStream> for proc_macro::TokenStream {
        fn from(ts: PTokenStream) -> Self {
            ts.into_tokens()
                .into_iter()
                .map(proc_macro::TokenTree::from)
                .collect()
        }
    }
}

fn err(msg: &str) -> TokenStream {
    err2(msg).into()
}

#[proc_macro_derive(Collapse)]
pub fn derive_collapse(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Struct(Fields::Named(fields)) => derive_struct(&name, fields).into(),
        Data::Struct(_) => err("Collapse: structs must have named fields"),
        Data::Enum(variants) => derive_enum(&name, variants).into(),
        _ => err("Collapse supports structs and enums"),
    }
}

#[proc_macro_derive(Term)]
pub fn derive_term(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Struct(Fields::Named(fields)) => derive_term_struct(&name, fields).into(),
        Data::Struct(_) => {
            err("Term: structs must have named fields (tuple/unit structs aren't supported)")
        }
        Data::Enum(variants) => derive_term_enum(&name, variants).into(),
        Data::Union(_) => err("Term does not support unions"),
    }
}

#[proc_macro_derive(Error, attributes(error, from, source))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Enum(variants) => derive_error_enum(&name, variants).into(),
        Data::Struct(fields) => derive_error_struct(&name, fields, &di.attrs).into(),
        Data::Union(_) => err("Error: unions are not supported"),
    }
}

#[proc_macro_derive(Validate, attributes(validate))]
pub fn derive_validate(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Struct(Fields::Named(fields)) => derive_validate_struct(&name, fields).into(),
        Data::Struct(_) => {
            err("Validate: structs must have named fields (tuple/unit structs aren't supported)")
        }
        Data::Enum(_) => {
            err("Validate: enums are not supported - nothing calling this derives it on an enum")
        }
        Data::Union(_) => err("Validate: unions are not supported"),
    }
}

#[proc_macro_derive(ToSchema, attributes(schema))]
pub fn derive_to_schema(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Struct(Fields::Named(fields)) => derive_to_schema_struct(&name, fields).into(),
        Data::Struct(_) => {
            err("ToSchema: structs must have named fields (tuple/unit structs aren't supported)")
        }
        Data::Enum(variants) => derive_to_schema_enum(&name, variants).into(),
        Data::Union(_) => err("ToSchema: unions are not supported"),
    }
}

#[proc_macro_derive(IntoParams)]
pub fn derive_into_params(input: TokenStream) -> TokenStream {
    let di: DeriveInput = parse_input!(input, DeriveInput);
    let name = di.ident.clone();
    match &di.data {
        Data::Struct(Fields::Named(fields)) => derive_into_params_struct(&name, fields).into(),
        Data::Struct(_) => {
            err("IntoParams: structs must have named fields (tuple/unit structs aren't supported)")
        }
        Data::Enum(_) => {
            err("IntoParams: enums are not supported - nothing calling this derives it on an enum")
        }
        Data::Union(_) => err("IntoParams: unions are not supported"),
    }
}

#[proc_macro_attribute]
pub fn path(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_p: PTokenStream = attr.into();
    let item_p: PTokenStream = item.clone().into();

    let Some(fn_name) = find_fn_name(&item_p) else {
        return err("path: could not find the annotated item's function name");
    };
    let path_fn_name = fmt_ident(format_args!("__path_{}", fn_name.sym));
    let body = parse_path_attr(&attr_p);

    let generated: proc_masq::TokenStream = quote_tokens! {
        #[allow(non_snake_case)]
        pub fn #path_fn_name() -> (&'static str, &'static str, ::wirefold::codec::syntax::SyntaxTerm) {
            #body
        }
    };

    let mut out: TokenStream = item;
    out.extend(TokenStream::from(generated));
    out
}
