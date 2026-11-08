//! The shared codegen logic behind every `wirefold_derive` proc-macro — field/variant
//! shape classification and the token-generating functions for `Collapse`/`Term`/`Error`/
//! `Validate`/`ToSchema`/`IntoParams`/`path`. Pure functions over `proc_masq` types only, no
//! `proc_macro::TokenStream` anywhere in this file — that keeps this module reusable from a
//! standalone (non-proc-macro) binary via the same `#[path]`-sharing `proc_masq.rs` itself uses,
//! not just from `lib.rs`'s thin `#[proc_macro_derive]` entry points.

use crate::proc_masq::{
    self, concat_tokens, each_comma, is_bare_ident, join_comma, option_inner, vec_inner, Attribute,
    Delimiter, Field, Fields, Ident, Literal, Span, TokenStream as PTokenStream, TokenTree,
    Variant,
};

pub(crate) fn fmt_ident(s: impl std::fmt::Display) -> Ident {
    Ident::new(&s.to_string(), proc_masq::Span::call_site())
}

/// Same as [`err`], for the helpers that build `proc_masq::TokenStream` (the `#[derive(Error)]`
/// enum/struct bodies, assembled before the final `.into()` at the top-level derive fn).
pub(crate) fn err2(msg: &str) -> proc_masq::TokenStream {
    let m = Literal::string(msg, proc_masq::Span::call_site());
    quote_tokens! { compile_error!(#m); }
}

pub(crate) fn is_numeric_ty(ty: &proc_masq::TokenStream) -> bool {
    const NUMS: &[&str] = &[
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64",
    ];
    NUMS.iter().any(|n| is_bare_ident(ty, n))
}

/// offset expr = sum of size_of of the prior field types (a const expression).
pub(crate) fn off_expr(prior: &[&proc_masq::TokenStream]) -> proc_masq::TokenStream {
    let sums = concat_tokens(
        prior
            .iter()
            .map(|t| quote_tokens! { + ::core::mem::size_of::<#t>() }),
    );
    quote_tokens! { (0usize #sums) }
}

pub(crate) fn derive_struct(name: &Ident, fields: &[Field]) -> proc_masq::TokenStream {
    #[allow(clippy::large_enum_variant)] // build-time only; a handful of these, size is irrelevant
    enum NonScalar {
        Str(Ident),
        Vec(Ident, proc_masq::TokenStream),
    }
    let mut scalars: Vec<(Ident, proc_masq::TokenStream, bool)> = Vec::new();
    let mut nonscalars: Vec<NonScalar> = Vec::new();
    for f in fields {
        let fname = f.ident.clone().unwrap();
        let ty = f.ty.clone();
        if is_bare_ident(&ty, "String") {
            nonscalars.push(NonScalar::Str(fname));
        } else if let Some(inner) = vec_inner(&ty) {
            nonscalars.push(NonScalar::Vec(fname, inner));
        } else if is_bare_ident(&ty, "bool") {
            scalars.push((fname, ty, true));
        } else {
            scalars.push((fname, ty, false));
        }
    }

    let all_sty: Vec<&proc_masq::TokenStream> = scalars.iter().map(|(_, t, _)| t).collect();
    let scalar_bytes = {
        let sums = concat_tokens(
            all_sty
                .iter()
                .map(|t| quote_tokens! { + ::core::mem::size_of::<#t>() }),
        );
        quote_tokens! { 0usize #sums }
    };
    let offsets: Vec<proc_masq::TokenStream> = (0..scalars.len())
        .map(|s| off_expr(&scalars[..s].iter().map(|(_, t, _)| t).collect::<Vec<_>>()))
        .collect();

    // One emit path for every shape (no has_vec/fast-path branch): the record table is the section
    // layout §0 = scalar Wire, §1.. = one column per variable field. `col_size_lets` measure each
    // column (pure arithmetic, no alloc). The whole table — and recursively the whole nested tree — is
    // ONE allocation (CollapseElem::collapse_pack), never a Seed per node.
    //
    // SERIALIZE is ONE pass over the source: `String` columns write their offset + bytes inline during
    // the single record loop (str_scatter), so the source `Vec<Record>` is streamed ONCE, not re-scanned
    // per column (the streaming law). `Vec<Collapse>` columns recurse and so emit after the loop
    // (vec_emits). No `__refs` Vec, no per-column re-traversal of the source.
    let block_writes: Vec<proc_masq::TokenStream> = scalars
        .iter()
        .zip(&offsets)
        .map(|((fname, ty, is_bool), off)| {
            if *is_bool {
                quote_tokens! { __b[#off] = it.#fname as u8; }
            } else {
                quote_tokens! { { let __o = #off; __b[__o..__o + ::core::mem::size_of::<#ty>()]
                .copy_from_slice(::wirefold::codec::web::pod_bytes(&it.#fname)); } }
            }
        })
        .collect();
    let scalar_acc: Vec<proc_masq::TokenStream> = scalars
        .iter()
        .zip(&offsets)
        .map(|((fname, ty, is_bool), off)| {
            if *is_bool {
                quote_tokens! { pub fn #fname(&self, i: usize) -> bool { self.__scalars.get(i)[#off] != 0 } }
            } else {
                quote_tokens! { pub fn #fname(&self, i: usize) -> #ty {
                    let b = self.__scalars.get(i); let __o = #off;
                    ::wirefold::codec::web::pod_read::<#ty>(&b[__o..__o + ::core::mem::size_of::<#ty>()]) } }
            }
        })
        .collect();

    let col_ids: Vec<Ident> = (0..nonscalars.len())
        .map(|k| fmt_ident(format_args!("__c{k}")))
        .collect();
    let sz_ids: Vec<Ident> = (0..nonscalars.len())
        .map(|k| fmt_ident(format_args!("__sz{k}")))
        .collect();
    let (mut col_size_lets, mut view_fields, mut view_opens, mut col_accs) =
        (vec![], vec![], vec![], vec![]);
    // `col_opens` open each column into a local, `col_checks` validate its contents once at open (the
    // accessors below then cannot panic), `view_inits` move the locals into the view.
    let (mut col_opens, mut col_checks, mut view_inits) = (vec![], vec![], vec![]);
    let (mut str_inits, mut str_scatters, mut str_finals, mut vec_emits) =
        (vec![], vec![], vec![], vec![]);
    for (k, ns) in nonscalars.iter().enumerate() {
        let cid = &col_ids[k];
        let szid = &sz_ids[k];
        let sec = k + 1;
        match ns {
            NonScalar::Str(fname) => {
                let csid = fmt_ident(format_args!("__cs{sec}"));
                let lbid = fmt_ident(format_args!("__lb{sec}"));
                let lcid = fmt_ident(format_args!("__lc{sec}"));
                // ragged_size(item byte-lengths), expanded so it needs no allocation to measure.
                col_size_lets.push(quote_tokens! {
                    let #szid: usize = 8usize + (__n + 1) * 8
                        + items.iter().map(|it| it.#fname.len()).sum::<usize>();
                });
                // init: count word + leaf cursor (the Ragged is [n][off₀..off_n][leaves]).
                str_inits.push(quote_tokens! {
                    let #csid = __starts[#sec];
                    __dst[#csid..#csid + 8].copy_from_slice(&(__n as u64).to_le_bytes());
                    let #lbid = #csid + 8 + (__n + 1) * 8;
                    let mut #lcid = #lbid;
                });
                // in-loop: write this record's offset, copy its bytes, advance the leaf cursor.
                str_scatters.push(quote_tokens! {{
                    let __fb = it.#fname.as_bytes();
                    let __oo = #csid + 8 + __i * 8;
                    __dst[__oo..__oo + 8].copy_from_slice(&((#lcid - #lbid) as u64).to_le_bytes());
                    __dst[#lcid..#lcid + __fb.len()].copy_from_slice(__fb);
                    #lcid += __fb.len();
                }});
                // after the loop: the closing offset off_n = total leaf length.
                str_finals.push(quote_tokens! {{
                    let __oo = #csid + 8 + __n * 8;
                    __dst[__oo..__oo + 8].copy_from_slice(&((#lcid - #lbid) as u64).to_le_bytes());
                }});
                view_fields.push(quote_tokens! { #cid: ::wirefold::codec::web::Ragged<'a>, });
                view_opens.push(quote_tokens! { #cid: ::wirefold::codec::web::Ragged::open(s.section(#sec))?, });
                col_opens.push(quote_tokens! { let #cid = ::wirefold::codec::web::Ragged::open(s.section(#sec))?; });
                // one entry per record, and every entry valid UTF-8 (so `get_ref`/`collapse_get` cannot panic)
                col_checks.push(quote_tokens! {
                    if #cid.len() != __n {
                        return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                    }
                    for __i in 0..__n {
                        if ::core::str::from_utf8(#cid.get(__i)).is_err() {
                            return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadUtf8);
                        }
                    }
                });
                view_inits.push(quote_tokens! { #cid, });
                col_accs.push(quote_tokens! { pub fn #fname(&self, i: usize) -> &'a [u8] { self.#cid.get(i) } });
            }
            NonScalar::Vec(fname, inner) => {
                // sections_size over per-record sub-buffer sizes, folded so measuring needs no alloc.
                col_size_lets.push(quote_tokens! {
                    let #szid: usize = {
                        let mut __p = 8usize * (1 + 2 * __n);
                        for it in items {
                            __p = (__p + 7) & !7;
                            __p += <#inner as ::wirefold::codec::web::CollapseElem>::collapse_size(it.#fname.as_slice());
                        }
                        __p
                    };
                });
                vec_emits.push(quote_tokens! {{
                    let __cs = __starts[#sec];
                    let __subsizes: ::std::vec::Vec<usize> = items.iter()
                        .map(|it| <#inner as ::wirefold::codec::web::CollapseElem>::collapse_size(it.#fname.as_slice()))
                        .collect();
                    let __sub = &mut __dst[__cs..__cs + #szid];
                    let __substarts = ::wirefold::codec::web::emit_sections_header(__sub, &__subsizes);
                    for (__j, it) in items.iter().enumerate() {
                        let __ss = __substarts[__j];
                        <#inner as ::wirefold::codec::web::CollapseElem>::collapse_emit(
                            it.#fname.as_slice(), &mut __sub[__ss..__ss + __subsizes[__j]]);
                    }
                }});
                view_fields.push(quote_tokens! { #cid: ::wirefold::codec::web::Sections<'a>, });
                view_opens.push(quote_tokens! { #cid: ::wirefold::codec::web::Sections::open(s.section(#sec))?, });
                col_opens.push(quote_tokens! { let #cid = ::wirefold::codec::web::Sections::open(s.section(#sec))?; });
                // one sub-table per record, each of which must itself open (recursively validated)
                col_checks.push(quote_tokens! {
                    if #cid.len() != __n {
                        return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                    }
                    for __i in 0..__n {
                        <#inner as ::wirefold::codec::web::CollapseElem>::collapse_open(#cid.section(__i))?;
                    }
                });
                view_inits.push(quote_tokens! { #cid, });
                col_accs.push(quote_tokens! {
                    pub fn #fname(&self, i: usize) -> <#inner as ::wirefold::codec::web::CollapseElem>::View<'a> {
                        <#inner as ::wirefold::codec::web::CollapseElem>::collapse_open(self.#cid.section(i)).unwrap()
                    }
                });
            }
        }
    }
    let view = fmt_ident(format_args!("{}View", name.sym));

    // Owned reconstruct (the serde-style deserialize): per field, scalars by value, String/Vec allocated.
    let mut owned_fields: Vec<proc_masq::TokenStream> = Vec::new();
    for (fname, _ty, _is_bool) in &scalars {
        owned_fields.push(quote_tokens! { #fname: self.#fname(i) });
    }
    for ns in &nonscalars {
        match ns {
            NonScalar::Str(fname) => owned_fields.push(quote_tokens! {
                #fname: ::std::string::String::from_utf8(self.#fname(i).to_vec())
                    .expect("cerealize: String field is not valid UTF-8")
            }),
            NonScalar::Vec(fname, _) => {
                owned_fields.push(quote_tokens! { #fname: self.#fname(i).to_vec() })
            }
        }
    }

    // Borrowed record (serde `#[serde(borrow)]` analog): String → &'a str into the seed, ZERO alloc;
    // the read-path win for the String case. Only emitted when a field actually borrows (else the 'a
    // would be unused) — pure-scalar records use the by-value owned `get(i)` instead.
    let refname = fmt_ident(format_args!("{}Ref", name.sym));
    let mut ref_fields: Vec<proc_masq::TokenStream> = Vec::new();
    let mut ref_inits: Vec<proc_masq::TokenStream> = Vec::new();
    for (fname, ty, _) in &scalars {
        ref_fields.push(quote_tokens! { pub #fname: #ty });
        ref_inits.push(quote_tokens! { #fname: self.#fname(i) });
    }
    for ns in &nonscalars {
        match ns {
            NonScalar::Str(fname) => {
                ref_fields.push(quote_tokens! { pub #fname: &'a str });
                ref_inits.push(quote_tokens! {
                    #fname: ::core::str::from_utf8(self.#fname(i))
                        .expect("cerealize: String field is not valid UTF-8")
                });
            }
            NonScalar::Vec(fname, inner) => {
                ref_fields.push(quote_tokens! { pub #fname: <#inner as ::wirefold::codec::web::CollapseElem>::View<'a> });
                ref_inits.push(quote_tokens! { #fname: self.#fname(i) });
            }
        }
    }
    let (ref_struct, get_ref_method) = if nonscalars.is_empty() {
        (proc_masq::TokenStream::new(), proc_masq::TokenStream::new())
    } else {
        let ref_fields_j = each_comma(ref_fields);
        let ref_inits_j = each_comma(ref_inits);
        (
            quote_tokens! {
                /// A borrowed record — `String` fields are `&'a str` into the seed (zero alloc), the
                /// serde `#[serde(borrow)]` analog. Read records as structs without materializing owned `T`.
                pub struct #refname<'a> { #ref_fields_j }
            },
            quote_tokens! {
                /// Borrowed record `i`: scalars by value, String/Vec as borrows into the seed — NO alloc.
                #[must_use] pub fn get_ref(&self, i: usize) -> #refname<'a> { #refname { #ref_inits_j } }
            },
        )
    };

    let view_fields_j = concat_tokens(view_fields);
    let _ = view_opens; // superseded by col_opens/col_checks/view_inits
    let col_opens_j = concat_tokens(col_opens);
    let col_checks_j = concat_tokens(col_checks);
    let view_inits_j = concat_tokens(view_inits);
    let nsections = nonscalars.len() + 1;
    let scalar_acc_j = concat_tokens(scalar_acc);
    let col_accs_j = concat_tokens(col_accs);
    let owned_fields_j = join_comma(owned_fields);
    let col_size_lets_j = concat_tokens(col_size_lets);
    let sz_ids_j = join_comma(&sz_ids);
    let str_inits_j = concat_tokens(str_inits);
    let block_writes_j = concat_tokens(block_writes);
    let str_scatters_j = concat_tokens(str_scatters);
    let str_finals_j = concat_tokens(str_finals);
    let vec_emits_j = concat_tokens(vec_emits);

    // A FIELD NAMED `open` IS NOT A BUG IN THE FIELD. The View gets one accessor per field plus
    // its own infrastructure (`open`/`len`/`is_empty`/`get`/`to_vec`), and both land in the SAME
    // inherent impl — so a record with a field called `open` failed to compile with "duplicate
    // definitions for `open`". That is every OHLC bar in existence, and the same trap is set for
    // `len`, `get`, `to_vec` and `is_empty`. The derive owns its namespace and the user owns
    // theirs, so the infrastructure moves to `collapse_*` (always emitted, always what generated
    // code calls) and the short names are emitted only where the record does not claim them —
    // existing callers of `View::open` keep working, and a field wins its own name.
    let field_names: Vec<String> = fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|i| i.sym.to_string()))
        .collect();
    let claims = |n: &str| field_names.iter().any(|f| f == n);
    let mut aliases: Vec<proc_masq::TokenStream> = Vec::new();
    if !claims("open") {
        aliases.push(quote_tokens! {
            /// Alias for [`Self::collapse_open`] — omitted when a field claims this name.
            pub fn open(bytes: &'a [u8]) -> ::core::result::Result<Self, ::wirefold::codec::web::WireErr> {
                Self::collapse_open(bytes)
            }
        });
    }
    if !claims("len") {
        aliases.push(quote_tokens! {
            #[must_use] pub fn len(&self) -> usize { self.collapse_len() }
        });
    }
    if !claims("is_empty") {
        aliases.push(quote_tokens! {
            #[must_use] pub fn is_empty(&self) -> bool { self.collapse_is_empty() }
        });
    }
    if !claims("get") {
        aliases.push(quote_tokens! {
            #[must_use] pub fn get(&self, i: usize) -> #name { self.collapse_get(i) }
        });
    }
    if !claims("to_vec") {
        aliases.push(quote_tokens! {
            #[must_use] pub fn to_vec(&self) -> ::std::vec::Vec<#name> { self.collapse_to_vec() }
        });
    }
    let aliases_j = concat_tokens(aliases);

    quote_tokens! {
        impl #name {
            pub const COLLAPSE_SCALAR_BYTES: usize = #scalar_bytes;
            #[must_use]
            pub fn collapse_encode(items: &[#name]) -> ::wirefold::codec::web::Seed {
                <#name as ::wirefold::codec::web::CollapseElem>::collapse_pack(items)
            }
            /// Open + reconstruct OWNED records — the serde `deserialize`-to-`Vec<T>` equivalent
            /// (allocates the Strings/Vecs). For zero-copy reads use the `View` instead. Returns an
            /// error — never panics — for a malformed buffer (bad structure, non-UTF-8 `String`).
            pub fn collapse_decode(bytes: &[u8]) -> ::core::result::Result<::std::vec::Vec<#name>, ::wirefold::codec::web::WireErr> {
                // Canonical names, never the aliases: a field may have claimed those.
                ::core::result::Result::Ok(#view::collapse_open(bytes)?.collapse_to_vec())
            }
        }
        pub struct #view<'a> {
            __scalars: ::wirefold::codec::web::Wire<'a, [u8; #name::COLLAPSE_SCALAR_BYTES]>,
            #view_fields_j
        }
        #ref_struct
        impl<'a> #view<'a> {
            /// Open a packed table. The canonical name — `open` is an alias, emitted only when no
            /// field of the record claims it.
            ///
            /// Validates the whole table once: section count, one entry per record in every column,
            /// UTF-8 in every `String`, every nested table. The accessors below cannot panic on a
            /// table that opened.
            pub fn collapse_open(bytes: &'a [u8]) -> ::core::result::Result<Self, ::wirefold::codec::web::WireErr> {
                let s = ::wirefold::codec::web::Sections::open(bytes)?;
                if s.len() != #nsections {
                    return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                }
                let __scalars = ::wirefold::codec::web::Wire::open(s.section(0))?;
                let __n = __scalars.len();
                #col_opens_j
                #col_checks_j
                let _ = __n;
                ::core::result::Result::Ok(#view { __scalars, #view_inits_j })
            }
            #[must_use] pub fn collapse_len(&self) -> usize { self.__scalars.len() }
            #[must_use] pub fn collapse_is_empty(&self) -> bool { self.collapse_len() == 0 }
            /// Reconstruct OWNED record `i` (serde-style): scalars by value, String/Vec allocated.
            #[must_use] pub fn collapse_get(&self, i: usize) -> #name { #name { #owned_fields_j } }
            /// Deserialize the whole table to owned records — the serde `Vec<T>` round-trip.
            #[must_use] pub fn collapse_to_vec(&self) -> ::std::vec::Vec<#name> {
                (0..self.collapse_len()).map(|i| self.collapse_get(i)).collect()
            }
            #aliases_j
            #scalar_acc_j
            #col_accs_j
            #get_ref_method
        }
        impl ::wirefold::codec::web::CollapseElem for #name {
            type View<'a> = #view<'a>;
            fn collapse_size(items: &[#name]) -> usize {
                let __n = items.len();
                let __scalar_sz: usize = 8usize + __n * #name::COLLAPSE_SCALAR_BYTES;
                #col_size_lets_j
                ::wirefold::codec::web::sections_size(&[__scalar_sz, #sz_ids_j])
            }
            fn collapse_emit(items: &[#name], __dst: &mut [u8]) {
                let __n = items.len();
                let __scalar_sz: usize = 8usize + __n * #name::COLLAPSE_SCALAR_BYTES;
                #col_size_lets_j
                let __sizes = [__scalar_sz, #sz_ids_j];
                let __starts = ::wirefold::codec::web::emit_sections_header(__dst, &__sizes);
                // §0 scalar count + each String column's count/leaf-cursor — then ONE pass over the
                // source writes every record's scalar block AND its String fields in place (no re-scan).
                let __s0 = __starts[0];
                __dst[__s0..__s0 + 8].copy_from_slice(&(__n as u64).to_le_bytes());
                #str_inits_j
                for (__i, it) in items.iter().enumerate() {
                    {
                        let __base = __s0 + 8 + __i * #name::COLLAPSE_SCALAR_BYTES;
                        let __b = &mut __dst[__base..__base + #name::COLLAPSE_SCALAR_BYTES];
                        #block_writes_j
                    }
                    #str_scatters_j
                }
                #str_finals_j
                // Vec<Collapse> columns recurse, so they emit after the single scalar/String pass.
                #vec_emits_j
            }
            fn collapse_open(bytes: &[u8]) -> ::core::result::Result<#view<'_>, ::wirefold::codec::web::WireErr> {
                #view::collapse_open(bytes)
            }
        }
    }
}

pub(crate) fn derive_enum(name: &Ident, variants: &[Variant]) -> proc_masq::TokenStream {
    let view = fmt_ident(format_args!("{}View", name.sym));
    let mut size_arms = Vec::new(); // &Self -> payload byte length (no alloc)
    let mut tag_arms = Vec::new(); // &Self -> tag byte
    let mut write_arms = Vec::new(); // &Self -> write fields into __pl (the leaf slice), in place
    let mut decode_arms = Vec::new(); // tag -> reconstruct from __p (the payload slice)
    let mut tag_size_arms = Vec::new(); // tag -> payload byte length (what `open` validates against)
    for (vidx, variant) in variants.iter().enumerate() {
        let tag = vidx as u8;
        let vname = &variant.ident;
        match &variant.fields {
            Fields::Named(named) => {
                let fnames: Vec<&Ident> = named.iter().map(|f| f.ident.as_ref().unwrap()).collect();
                let ftys: Vec<&proc_masq::TokenStream> = named.iter().map(|f| &f.ty).collect();
                let offs: Vec<_> = (0..ftys.len()).map(|j| off_expr(&ftys[..j])).collect();
                let fnames_j = join_comma(&fnames);
                let ftys_sum = concat_tokens(
                    ftys.iter()
                        .map(|t| quote_tokens! { + ::core::mem::size_of::<#t>() }),
                );
                tag_size_arms.push(quote_tokens! { #tag => 0usize #ftys_sum });
                size_arms.push(quote_tokens! { #name::#vname { .. } => 0usize #ftys_sum });
                tag_arms.push(quote_tokens! { #name::#vname { .. } => #tag });
                let writes = concat_tokens(fnames.iter().zip(&ftys).map(|(f, ty)| {
                    quote_tokens! {
                        __pl[__wo..__wo + ::core::mem::size_of::<#ty>()]
                            .copy_from_slice(::wirefold::codec::web::pod_bytes(#f));
                        __wo += ::core::mem::size_of::<#ty>();
                    }
                }));
                write_arms.push(quote_tokens! {
                    #name::#vname { #fnames_j } => {
                        let mut __wo = 0usize;
                        #writes
                        let _ = __wo;
                    }
                });
                let df = each_comma(fnames.iter().zip(&ftys).zip(&offs).map(|((f, ty), off)| {
                    quote_tokens! { #f: ::wirefold::codec::web::pod_read::<#ty>(&__p[#off..#off + ::core::mem::size_of::<#ty>()]) }
                }));
                decode_arms.push(quote_tokens! { #tag => #name::#vname { #df } });
            }
            Fields::Unnamed(unnamed) => {
                let ftys: Vec<&proc_masq::TokenStream> = unnamed.iter().map(|f| &f.ty).collect();
                let binds: Vec<Ident> = (0..ftys.len())
                    .map(|j| fmt_ident(format_args!("__f{j}")))
                    .collect();
                let offs: Vec<_> = (0..ftys.len()).map(|j| off_expr(&ftys[..j])).collect();
                let ftys_sum = concat_tokens(
                    ftys.iter()
                        .map(|t| quote_tokens! { + ::core::mem::size_of::<#t>() }),
                );
                tag_size_arms.push(quote_tokens! { #tag => 0usize #ftys_sum });
                size_arms.push(quote_tokens! { #name::#vname ( .. ) => 0usize #ftys_sum });
                tag_arms.push(quote_tokens! { #name::#vname ( .. ) => #tag });
                let binds_j = join_comma(&binds);
                let writes = concat_tokens(binds.iter().zip(&ftys).map(|(b, ty)| {
                    quote_tokens! {
                        __pl[__wo..__wo + ::core::mem::size_of::<#ty>()]
                            .copy_from_slice(::wirefold::codec::web::pod_bytes(#b));
                        __wo += ::core::mem::size_of::<#ty>();
                    }
                }));
                write_arms.push(quote_tokens! {
                    #name::#vname ( #binds_j ) => {
                        let mut __wo = 0usize;
                        #writes
                        let _ = __wo;
                    }
                });
                let df = join_comma(ftys.iter().zip(&offs).map(|(ty, off)| {
                    quote_tokens! { ::wirefold::codec::web::pod_read::<#ty>(&__p[#off..#off + ::core::mem::size_of::<#ty>()]) }
                }));
                decode_arms.push(quote_tokens! { #tag => #name::#vname( #df ) });
            }
            Fields::Unit => {
                tag_size_arms.push(quote_tokens! { #tag => 0usize });
                size_arms.push(quote_tokens! { #name::#vname => 0usize });
                tag_arms.push(quote_tokens! { #name::#vname => #tag });
                write_arms.push(quote_tokens! { #name::#vname => {} });
                decode_arms.push(quote_tokens! { #tag => #name::#vname });
            }
        }
    }

    let tag_size_arms_j = each_comma(tag_size_arms);
    let size_arms_j = each_comma(size_arms);
    let tag_arms_j = each_comma(tag_arms);
    let write_arms_j = each_comma(write_arms);
    let decode_arms_j = each_comma(decode_arms);

    quote_tokens! {
        impl #name {
            #[must_use]
            pub fn collapse_encode(items: &[#name]) -> ::wirefold::codec::web::Seed {
                <#name as ::wirefold::codec::web::CollapseElem>::collapse_pack(items)
            }
            /// Open + reconstruct OWNED records — the serde `deserialize`-to-`Vec<T>` equivalent.
            /// Returns an error — never panics — for a malformed buffer (bad structure, a tag no
            /// variant owns, a payload of the wrong size).
            pub fn collapse_decode(bytes: &[u8]) -> ::core::result::Result<::std::vec::Vec<#name>, ::wirefold::codec::web::WireErr> {
                ::core::result::Result::Ok(#view::open(bytes)?.to_vec())
            }
        }
        /// Zero-copy view of a `Collapse` enum: tag + payload columns; `get(i)` reconstructs the variant.
        pub struct #view<'a> {
            __tag: ::wirefold::codec::web::Wire<'a, u8>,
            __pay: ::wirefold::codec::web::Ragged<'a>,
        }
        impl<'a> #view<'a> {
            /// Validates the whole column once: both sections, one payload per tag, every tag owned by a
            /// variant, every payload exactly its variant's size. `get` cannot panic on a column that opened.
            pub fn open(bytes: &'a [u8]) -> ::core::result::Result<Self, ::wirefold::codec::web::WireErr> {
                let s = ::wirefold::codec::web::Sections::open(bytes)?;
                if s.len() != 2 {
                    return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                }
                let __tag = ::wirefold::codec::web::Wire::<u8>::open(s.section(0))?;
                let __pay = ::wirefold::codec::web::Ragged::open(s.section(1))?;
                if __pay.len() != __tag.len() {
                    return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                }
                for __i in 0..__tag.len() {
                    let __want: usize = match *__tag.get(__i) {
                        #tag_size_arms_j
                        _ => return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadTag),
                    };
                    if __pay.get(__i).len() != __want {
                        return ::core::result::Result::Err(::wirefold::codec::web::WireErr::BadLength);
                    }
                }
                ::core::result::Result::Ok(#view { __tag, __pay })
            }
            #[must_use] pub fn len(&self) -> usize { self.__tag.len() }
            #[must_use] pub fn is_empty(&self) -> bool { self.len() == 0 }
            /// Reconstruct record `i` — the tag and payload are read zero-copy; Pod variant fields by value.
            pub fn get(&self, i: usize) -> #name {
                let __p = self.__pay.get(i);
                match *self.__tag.get(i) {
                    #decode_arms_j
                    _ => ::core::unreachable!(),
                }
            }
            /// Deserialize the whole column to owned records — the serde `Vec<T>` round-trip.
            #[must_use] pub fn to_vec(&self) -> ::std::vec::Vec<#name> {
                (0..self.len()).map(|i| self.get(i)).collect()
            }
        }
        impl ::wirefold::codec::web::CollapseElem for #name {
            type View<'a> = #view<'a>;
            fn collapse_size(items: &[#name]) -> usize {
                let __n = items.len();
                let __tag_sz: usize = 8usize + __n;                       // Wire<u8>: count + n tags
                let __leaves: usize = items.iter().map(|it| match it { #size_arms_j }).sum();
                let __pay_sz: usize = 8usize + (__n + 1) * 8 + __leaves;  // Ragged: header + leaves
                ::wirefold::codec::web::sections_size(&[__tag_sz, __pay_sz])
            }
            fn collapse_emit(items: &[#name], __dst: &mut [u8]) {
                let __n = items.len();
                let __tag_sz: usize = 8usize + __n;
                // per-record payload sizes — one O(n) table for the ragged offsets (NOT a Seed per node).
                let __plens: ::std::vec::Vec<usize> =
                    items.iter().map(|it| match it { #size_arms_j }).collect();
                let __leaves: usize = __plens.iter().sum();
                let __pay_sz: usize = 8usize + (__n + 1) * 8 + __leaves;
                let __starts = ::wirefold::codec::web::emit_sections_header(__dst, &[__tag_sz, __pay_sz]);
                // §0 — tag Wire<u8>: count, then each record's tag byte.
                let __ts = __starts[0];
                __dst[__ts..__ts + 8].copy_from_slice(&(__n as u64).to_le_bytes());
                for (__i, it) in items.iter().enumerate() {
                    __dst[__ts + 8 + __i] = match it { #tag_arms_j };
                }
                // §1 — payload Ragged: offsets (prefix sums of __plens), then each variant's fields in place.
                let __ps = __starts[1];
                let __pay = &mut __dst[__ps..__ps + __pay_sz];
                __pay[0..8].copy_from_slice(&(__n as u64).to_le_bytes());
                let mut __off = 0u64;
                for __i in 0..__n {
                    __pay[8 + __i * 8..16 + __i * 8].copy_from_slice(&__off.to_le_bytes());
                    __off += __plens[__i] as u64;
                }
                __pay[8 + __n * 8..16 + __n * 8].copy_from_slice(&__off.to_le_bytes());
                let __leaf_base = 8 + (__n + 1) * 8;
                let mut __o2 = 0usize;
                for (__i, it) in items.iter().enumerate() {
                    let __pl = &mut __pay[__leaf_base + __o2..__leaf_base + __o2 + __plens[__i]];
                    match it { #write_arms_j }
                    __o2 += __plens[__i];
                }
            }
            fn collapse_open(bytes: &[u8]) -> ::core::result::Result<#view<'_>, ::wirefold::codec::web::WireErr> {
                #view::open(bytes)
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// `#[derive(Term)]` — the struct/enum ↔ `SyntaxTerm` bridge (`wirefold::codec::syntax`): the
// serde-`derive(Serialize, Deserialize)` analog for the UNIVERSAL TERM (JSON/BAML/TOON/POML are
// all one applicative over it — `codec::syntax`'s own module doc), not the binary columnar
// format `Collapse` targets above. `HttpCall::to_term`/`from_term` in `syntax.rs` is this
// pattern's hand-written instance; this derives it instead of requiring every struct to repeat
// it by hand.
//
// Field shapes: `String`, `bool`, any integer/float primitive, a nested type that ALSO derives
// `Term`, and `Option<_>`/`Vec<_>` wrapping ANY of those to ANY depth (`Vec<Option<T>>`,
// `Option<Vec<T>>`, `Vec<Vec<T>>`, ...) — `FieldShape` recurses. Enums: externally tagged
// (serde's default), `Bu([(variant_name, payload)])` — unit -> `Nb`, a single-field tuple ->
// that field's own term, a multi-field tuple -> `Fourth([...])`, a named-field (struct) variant
// -> a nested `Bu`. Not supported: tuple/unit STRUCTS (top-level, distinct from enum variants).
// ════════════════════════════════════════════════════════════════════════════

/// A field's (or an enum payload slot's) shape, fully recursive so `Option`/`Vec` compose to any
/// depth (`Vec<Option<T>>`, `Option<Vec<T>>`, ...) rather than being flattened one level deep.
pub(crate) enum FieldShape {
    Str,
    Bool,
    Num(proc_masq::TokenStream),
    Nested(proc_masq::TokenStream),
    OptionOf(Box<FieldShape>),
    ListOf(Box<FieldShape>),
}

pub(crate) fn classify_field(ty: &proc_masq::TokenStream) -> FieldShape {
    if let Some(inner) = option_inner(ty) {
        return FieldShape::OptionOf(Box::new(classify_field(&inner)));
    }
    if let Some(inner) = vec_inner(ty) {
        return FieldShape::ListOf(Box::new(classify_field(&inner)));
    }
    if is_bare_ident(ty, "String") {
        FieldShape::Str
    } else if is_bare_ident(ty, "bool") {
        FieldShape::Bool
    } else if is_numeric_ty(ty) {
        FieldShape::Num(ty.clone())
    } else {
        FieldShape::Nested(ty.clone())
    }
}

/// Build the `SyntaxTerm` for one already-OWNED value of this shape — used both at the top
/// level (a struct field / enum payload) and recursively inside an `Option`/`Vec` closure.
/// `value_ref` is always a REFERENCE (`&self.field`, or a match-ergonomics/`.iter()` binding
/// derived from one) — never an owned clone of the field. Only a shape that genuinely needs an
/// owned value to build its `Atom` (`Str`) clones, and only the leaf value itself (a `String`),
/// never the field's outer container. This is what lets a `Vec<Nested>`/`Option<Nested>` field
/// encode without requiring `Nested: Clone` (or `Vec<Nested>: Clone`) at all — `Nested`'s own
/// `to_term(&self)` only ever needs a reference, all the way down.
pub(crate) fn to_term_expr(
    shape: &FieldShape,
    value_ref: proc_masq::TokenStream,
) -> proc_masq::TokenStream {
    match shape {
        FieldShape::Str => {
            quote_tokens! { ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str((#value_ref).clone())) }
        }
        FieldShape::Bool => {
            quote_tokens! { ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Bool(*(#value_ref))) }
        }
        FieldShape::Num(_) => {
            quote_tokens! { ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Num(::std::string::ToString::to_string(#value_ref))) }
        }
        FieldShape::Nested(_) => {
            quote_tokens! { ::wirefold::codec::syntax::AsTerm::to_term(#value_ref) }
        }
        FieldShape::OptionOf(inner) => {
            let inner_expr = to_term_expr(inner, quote_tokens! { __v });
            quote_tokens! {
                match (#value_ref) {
                    ::core::option::Option::Some(__v) => #inner_expr,
                    ::core::option::Option::None => ::wirefold::codec::syntax::SyntaxTerm::Nb,
                }
            }
        }
        FieldShape::ListOf(inner) => {
            let inner_expr = to_term_expr(inner, quote_tokens! { __item });
            quote_tokens! {
                ::wirefold::codec::syntax::SyntaxTerm::Fourth(
                    (#value_ref).iter().map(|__item| #inner_expr).collect::<::std::vec::Vec<_>>()
                )
            }
        }
    }
}

/// The inverse of [`to_term_expr`] — `term_expr` is an expression of type `&SyntaxTerm`.
/// ALWAYS produces a `Result<Shape, SyntaxError>` VALUE (the caller applies `?`), never a bare
/// value with an internal `return` — this recurses inside `.map()` closures for nested
/// `Vec`/`Option`, where a `return` would exit the wrong (closure) scope instead of the
/// generated `from_term` function.
pub(crate) fn from_term_result_expr(
    shape: &FieldShape,
    term_expr: proc_masq::TokenStream,
) -> proc_masq::TokenStream {
    match shape {
        FieldShape::Str => {
            quote_tokens! { ::wirefold::codec::syntax::term_str(#term_expr).map(|s| s.to_string()) }
        }
        FieldShape::Bool => quote_tokens! { ::wirefold::codec::syntax::term_bool(#term_expr) },
        FieldShape::Num(ty) => quote_tokens! {
            ::wirefold::codec::syntax::term_num(#term_expr).and_then(|s| {
                s.parse::<#ty>().map_err(|_| ::wirefold::codec::syntax::SyntaxError::new("bad number"))
            })
        },
        FieldShape::Nested(ty) => {
            quote_tokens! { <#ty as ::wirefold::codec::syntax::AsTerm>::from_term(#term_expr) }
        }
        FieldShape::OptionOf(inner) => {
            let inner_expr = from_term_result_expr(inner, quote_tokens! { __t });
            quote_tokens! {
                match #term_expr {
                    ::wirefold::codec::syntax::SyntaxTerm::Nb => ::core::result::Result::Ok(::core::option::Option::None),
                    __t => (#inner_expr).map(::core::option::Option::Some),
                }
            }
        }
        FieldShape::ListOf(inner) => {
            let inner_expr = from_term_result_expr(inner, quote_tokens! { __t });
            quote_tokens! {
                match #term_expr {
                    ::wirefold::codec::syntax::SyntaxTerm::Fourth(__items) => __items
                        .iter()
                        .map(|__t| #inner_expr)
                        .collect::<::core::result::Result<::std::vec::Vec<_>, ::wirefold::codec::syntax::SyntaxError>>(),
                    _ => ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new("expected a list")),
                }
            }
        }
    }
}

pub(crate) fn to_term_field(fname: &Ident, shape: &FieldShape) -> proc_masq::TokenStream {
    let fname_str = fname.sym.to_string();
    let encode = to_term_expr(shape, quote_tokens! { &self.#fname });
    quote_tokens! { (#fname_str.to_string(), #encode) }
}

/// A missing field decodes as though it were `Nb` (rather than a hard "missing" error at THIS
/// layer): a required shape (`Str`/`Bool`/`Num`/`Nested`/`ListOf`) then naturally fails to match
/// `Nb` and reports a real decode error, while `OptionOf` naturally treats `Nb` as `None` — the
/// same "missing == None" behavior `field_opt` gave the one-level-only version, generalized to
/// any nesting depth without a separate missing-vs-present code path.
pub(crate) fn from_term_field(fname: &Ident, shape: &FieldShape) -> proc_masq::TokenStream {
    let fname_str = fname.sym.to_string();
    let decode = from_term_result_expr(shape, quote_tokens! { &__term });
    quote_tokens! {
        #fname: {
            let __term = ::wirefold::codec::syntax::field(__fields, #fname_str)
                .cloned()
                .unwrap_or(::wirefold::codec::syntax::SyntaxTerm::Nb);
            (#decode)?
        }
    }
}

pub(crate) fn derive_term_struct(name: &Ident, fields: &[Field]) -> proc_masq::TokenStream {
    let shapes: Vec<(Ident, FieldShape)> = fields
        .iter()
        .map(|f| (f.ident.clone().unwrap(), classify_field(&f.ty)))
        .collect();

    let to_term_entries = join_comma(
        shapes
            .iter()
            .map(|(fname, shape)| to_term_field(fname, shape)),
    );
    let from_term_entries = join_comma(
        shapes
            .iter()
            .map(|(fname, shape)| from_term_field(fname, shape)),
    );

    quote_tokens! {
        impl #name {
            /// Encode this value as a `SyntaxTerm` — feed to `wirefold::codec::syntax::render_surface`
            /// for JSON/BAML/TOON/POML text, or `cerealize_term` for the compressed binary form.
            #[must_use]
            pub fn to_term(&self) -> ::wirefold::codec::syntax::SyntaxTerm {
                ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![ #to_term_entries ])
            }
            /// Decode from a `SyntaxTerm` — the inverse of `to_term`, `Err` on a missing/wrong-shaped field.
            pub fn from_term(term: &::wirefold::codec::syntax::SyntaxTerm) -> ::core::result::Result<Self, ::wirefold::codec::syntax::SyntaxError> {
                let ::wirefold::codec::syntax::SyntaxTerm::Bu(__fields) = term else {
                    return ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new("expected a B/U object"));
                };
                ::core::result::Result::Ok(#name { #from_term_entries })
            }
        }
        // Also implement the `AsTerm` trait (delegating to the inherent methods above) so
        // generic code can bound on `T: AsTerm` for any `#[derive(Term)]` type, not just call
        // the inherent methods on a concrete, already-known type.
        impl ::wirefold::codec::syntax::AsTerm for #name {
            fn to_term(&self) -> ::wirefold::codec::syntax::SyntaxTerm {
                #name::to_term(self)
            }
            fn from_term(term: &::wirefold::codec::syntax::SyntaxTerm) -> ::core::result::Result<Self, ::wirefold::codec::syntax::SyntaxError> {
                #name::from_term(term)
            }
        }
    }
}

/// One enum variant's payload shape — unit (no data), a single unnamed field (its own term
/// directly, not wrapped in a 1-list), 2+ unnamed fields (a `Fourth` of their terms), or named
/// fields (a nested `Bu`, same shape a struct's own fields get).
pub(crate) enum VariantPayload {
    Unit,
    Single(FieldShape),
    Tuple(Vec<FieldShape>),
    Named(Vec<(Ident, FieldShape)>),
}

pub(crate) fn classify_variant(fields: &Fields) -> VariantPayload {
    match fields {
        Fields::Unit => VariantPayload::Unit,
        Fields::Unnamed(u) if u.len() == 1 => VariantPayload::Single(classify_field(&u[0].ty)),
        Fields::Unnamed(u) => {
            VariantPayload::Tuple(u.iter().map(|f| classify_field(&f.ty)).collect())
        }
        Fields::Named(n) => VariantPayload::Named(
            n.iter()
                .map(|f| (f.ident.clone().unwrap(), classify_field(&f.ty)))
                .collect(),
        ),
    }
}

/// Externally tagged enum encoding (serde's default): `Bu([(variant_name, payload_term)])`.
pub(crate) fn derive_term_enum(name: &Ident, variants: &[Variant]) -> proc_masq::TokenStream {
    let mut to_term_arms = Vec::new();
    let mut from_term_arms = Vec::new();

    for v in variants {
        let vname = &v.ident;
        let vname_str = vname.sym.to_string();
        let payload = classify_variant(&v.fields);

        match payload {
            VariantPayload::Unit => {
                to_term_arms.push(quote_tokens! {
                    #name::#vname => (#vname_str.to_string(), ::wirefold::codec::syntax::SyntaxTerm::Nb)
                });
                from_term_arms.push(quote_tokens! {
                    #vname_str => ::core::result::Result::Ok(#name::#vname)
                });
            }
            VariantPayload::Single(shape) => {
                // `match self { ... }` matches `&Self`, so match ergonomics already bind `__0` as
                // a reference here — no clone needed (and none of `Nested`/`ListOf`'s recursion
                // requires one either, see `to_term_expr`'s doc comment).
                let encode = to_term_expr(&shape, quote_tokens! { __0 });
                let decode = from_term_result_expr(&shape, quote_tokens! { __payload });
                to_term_arms.push(quote_tokens! {
                    #name::#vname(__0) => (#vname_str.to_string(), #encode)
                });
                from_term_arms.push(quote_tokens! {
                    #vname_str => (#decode).map(#name::#vname)
                });
            }
            VariantPayload::Tuple(shapes) => {
                let binds: Vec<Ident> = (0..shapes.len())
                    .map(|i| fmt_ident(format_args!("__{i}")))
                    .collect();
                let binds_j = join_comma(&binds);
                let encodes = join_comma(
                    shapes
                        .iter()
                        .zip(&binds)
                        .map(|(s, b)| to_term_expr(s, quote_tokens! { #b })),
                );
                let decodes = join_comma(shapes.iter().enumerate().map(|(i, s)| {
                    let idx = i;
                    let decode = from_term_result_expr(s, quote_tokens! { &__items[#idx] });
                    quote_tokens! { (#decode)? }
                }));
                let n = shapes.len();
                to_term_arms.push(quote_tokens! {
                    #name::#vname(#binds_j) => (
                        #vname_str.to_string(),
                        ::wirefold::codec::syntax::SyntaxTerm::Fourth(::std::vec![ #encodes ])
                    )
                });
                from_term_arms.push(quote_tokens! {
                    #vname_str => match __payload {
                        ::wirefold::codec::syntax::SyntaxTerm::Fourth(__items) if __items.len() == #n => {
                            ::core::result::Result::Ok(#name::#vname( #decodes ))
                        }
                        _ => ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new(
                            ::std::format!("variant {} expects {} fields", #vname_str, #n)
                        )),
                    }
                });
            }
            VariantPayload::Named(field_shapes) => {
                let fnames: Vec<&Ident> = field_shapes.iter().map(|(f, _)| f).collect();
                let fnames_j = join_comma(&fnames);
                let entries = join_comma(field_shapes.iter().map(|(fname, shape)| {
                    let fname_str = fname.sym.to_string();
                    let encode = to_term_expr(shape, quote_tokens! { #fname });
                    quote_tokens! { (#fname_str.to_string(), #encode) }
                }));
                let decodes = join_comma(field_shapes.iter().map(|(fname, shape)| {
                    let fname_str = fname.sym.to_string();
                    let decode = from_term_result_expr(
                        shape,
                        quote_tokens! { &::wirefold::codec::syntax::field(__vfields, #fname_str).cloned().unwrap_or(::wirefold::codec::syntax::SyntaxTerm::Nb) },
                    );
                    quote_tokens! { #fname: (#decode)? }
                }));
                to_term_arms.push(quote_tokens! {
                    #name::#vname { #fnames_j } => (
                        #vname_str.to_string(),
                        ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![ #entries ])
                    )
                });
                from_term_arms.push(quote_tokens! {
                    #vname_str => {
                        let ::wirefold::codec::syntax::SyntaxTerm::Bu(__vfields) = __payload else {
                            return ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new(
                                ::std::format!("variant {} expects a B/U object", #vname_str)
                            ));
                        };
                        ::core::result::Result::Ok(#name::#vname { #decodes })
                    }
                });
            }
        }
    }

    let to_term_arms_j = join_comma(to_term_arms);
    let from_term_arms_j = each_comma(from_term_arms);

    quote_tokens! {
        impl #name {
            #[must_use]
            pub fn to_term(&self) -> ::wirefold::codec::syntax::SyntaxTerm {
                let (__tag, __payload) = match self { #to_term_arms_j };
                ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![(__tag, __payload)])
            }
            pub fn from_term(term: &::wirefold::codec::syntax::SyntaxTerm) -> ::core::result::Result<Self, ::wirefold::codec::syntax::SyntaxError> {
                let ::wirefold::codec::syntax::SyntaxTerm::Bu(__fields) = term else {
                    return ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new("expected a B/U object (externally tagged enum)"));
                };
                let ::core::option::Option::Some((__tag, __payload)) = __fields.first() else {
                    return ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new("empty enum term -- no variant tag"));
                };
                match __tag.as_str() {
                    #from_term_arms_j
                    other => ::core::result::Result::Err(::wirefold::codec::syntax::SyntaxError::new(::std::format!("unknown variant: {other}"))),
                }
            }
        }
        impl ::wirefold::codec::syntax::AsTerm for #name {
            fn to_term(&self) -> ::wirefold::codec::syntax::SyntaxTerm {
                #name::to_term(self)
            }
            fn from_term(term: &::wirefold::codec::syntax::SyntaxTerm) -> ::core::result::Result<Self, ::wirefold::codec::syntax::SyntaxError> {
                #name::from_term(term)
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// #[derive(Error)] — thiserror-compatible subset, zero-dependency.
//
// `#[error("...")]` per variant (or on the struct), `#[from]` on a variant's sole field
// (generates `impl From<FieldType>`), `#[source]` marks the causal-chain field. Named-field
// variants get Rust's captured-identifier format args for free (`{field}` binds the
// destructured local of the same name); tuple variants get positional args (`{0}`, `{1}`, …)
// bound only up to the highest index the message actually references, so an unreferenced
// trailing field doesn't trip rustc's "argument never used" error.
// ═══════════════════════════════════════════════════════════════════

/// The single string literal inside `#[error("...")]`, if present.
pub(crate) fn error_lit(attrs: &[Attribute]) -> Option<Literal> {
    attrs.iter().find_map(|a| {
        if a.path == "error" {
            a.single_string_arg()
        } else {
            None
        }
    })
}

/// Is a bare `#[name]` attribute present (`#[from]`, `#[source]`)?
pub(crate) fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path == name)
}

/// The highest `{N}` positional index a format literal references, skipping `{{`/`}}` escapes.
/// `None` if it references no positional index (a static message, or named `{field}` captures).
pub(crate) fn max_positional_index(fmt: &str) -> Option<usize> {
    let mut max: Option<usize> = None;
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if fmt[i..].starts_with("{{") {
                i += 2;
                continue;
            }
            let rest = &fmt[i + 1..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                if let Ok(n) = digits.parse::<usize>() {
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
            }
        }
        i += 1;
    }
    max
}

/// Bind a variant/struct's fields for one match arm / destructure: pattern tokens, the bound
/// idents in declaration order, and (for tuple shapes) how many leading fields the format
/// string actually references — the rest are swallowed by `..` rather than bound and unused.
pub(crate) fn bind_error_fields(
    fields: &Fields,
    fmt: &str,
) -> (proc_masq::TokenStream, Vec<Ident>, bool) {
    match fields {
        Fields::Unit => (proc_masq::TokenStream::new(), Vec::new(), false),
        Fields::Named(n) => {
            let idents: Vec<Ident> = n.iter().map(|f| f.ident.clone().unwrap()).collect();
            let idents_j = join_comma(&idents);
            (quote_tokens! { { #idents_j } }, idents, false)
        }
        Fields::Unnamed(u) => {
            let total = u.len();
            let used = max_positional_index(fmt).map_or(0, |m| (m + 1).min(total));
            let idents: Vec<Ident> = (0..used).map(|i| fmt_ident(format_args!("_{i}"))).collect();
            let idents_j = join_comma(&idents);
            let pat = if used == total {
                quote_tokens! { ( #idents_j ) }
            } else {
                quote_tokens! { ( #idents_j .. ) }
            };
            (pat, idents, true)
        }
    }
}

/// The `source()` field for one variant/struct's fields — a `#[source]` field, or the sole
/// field of a single-field tuple/named shape marked `#[from]` (thiserror's own convention: a
/// `#[from]` field is automatically the source too).
pub(crate) fn error_source_ident(fields: &Fields) -> Option<Ident> {
    match fields {
        Fields::Named(n) => n
            .iter()
            .find(|f| has_attr(&f.attrs, "source") || has_attr(&f.attrs, "from"))
            .map(|f| f.ident.clone().unwrap()),
        Fields::Unnamed(u) if u.len() == 1 => {
            let f = &u[0];
            (has_attr(&f.attrs, "source") || has_attr(&f.attrs, "from")).then(|| fmt_ident("_0"))
        }
        _ => None,
    }
}

/// The `#[from]`d field's type + the constructor pattern to build `Self` from it, if this
/// variant/struct has exactly one field and it's marked `#[from]`.
pub(crate) fn error_from_ctor(
    fields: &Fields,
    build: impl FnOnce(proc_masq::TokenStream) -> proc_masq::TokenStream,
) -> Option<(proc_masq::TokenStream, proc_masq::TokenStream)> {
    match fields {
        Fields::Unnamed(u) if u.len() == 1 => {
            let f = &u[0];
            has_attr(&f.attrs, "from").then(|| (f.ty.clone(), build(quote_tokens! { (source) })))
        }
        Fields::Named(n) if n.len() == 1 => {
            let f = &n[0];
            let fname = f.ident.clone().unwrap();
            has_attr(&f.attrs, "from")
                .then(|| (f.ty.clone(), build(quote_tokens! { { #fname: source } })))
        }
        _ => None,
    }
}

pub(crate) fn derive_error_enum(name: &Ident, variants: &[Variant]) -> proc_masq::TokenStream {
    let mut display_arms = Vec::new();
    let mut source_arms = Vec::new();
    let mut from_impls = Vec::new();

    for v in variants {
        let vname = &v.ident;
        let Some(fmt) = error_lit(&v.attrs) else {
            return err2(&format!(
                "Error: variant `{}` needs #[error(\"...\")] (wirefold's derive, thiserror-compatible subset)",
                vname.sym
            ));
        };
        let fmt_value = fmt.string_value().unwrap_or_default();
        let (pat, idents, is_tuple) = bind_error_fields(&v.fields, &fmt_value);
        display_arms.push(if is_tuple && !idents.is_empty() {
            let idents_j = join_comma(&idents);
            quote_tokens! { Self::#vname #pat => ::core::write!(f, #fmt, #idents_j) }
        } else {
            quote_tokens! { Self::#vname #pat => ::core::write!(f, #fmt) }
        });

        let ignore_pat = match &v.fields {
            Fields::Unit => proc_masq::TokenStream::new(),
            Fields::Named(_) => quote_tokens! { { .. } },
            Fields::Unnamed(_) => quote_tokens! { (..) },
        };
        let source_ident = error_source_ident(&v.fields);
        source_arms.push(match &source_ident {
            Some(sf) if matches!(&v.fields, Fields::Unnamed(_)) => quote_tokens! {
                Self::#vname(#sf, ..) => ::core::option::Option::Some(#sf as &(dyn ::std::error::Error + 'static))
            },
            Some(sf) => quote_tokens! {
                Self::#vname { #sf, .. } => ::core::option::Option::Some(#sf as &(dyn ::std::error::Error + 'static))
            },
            None => quote_tokens! { Self::#vname #ignore_pat => ::core::option::Option::None },
        });

        if let Some((ty, ctor_fields)) = error_from_ctor(&v.fields, |f| f) {
            from_impls.push(quote_tokens! {
                impl ::core::convert::From<#ty> for #name {
                    fn from(source: #ty) -> Self { Self::#vname #ctor_fields }
                }
            });
        }
    }

    let display_arms_j = join_comma(display_arms);
    let source_arms_j = join_comma(source_arms);
    let from_impls_j = concat_tokens(from_impls);

    quote_tokens! {
        #[allow(unused_variables)]
        impl ::std::fmt::Display for #name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match self { #display_arms_j }
            }
        }
        impl ::std::error::Error for #name {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                match self { #source_arms_j }
            }
        }
        #from_impls_j
    }
}

pub(crate) fn derive_error_struct(
    name: &Ident,
    fields: &Fields,
    attrs: &[Attribute],
) -> proc_masq::TokenStream {
    let Some(fmt) = error_lit(attrs) else {
        return err2("Error: struct needs #[error(\"...\")] (wirefold's derive, thiserror-compatible subset)");
    };
    let fmt_value = fmt.string_value().unwrap_or_default();
    let (pat, idents, is_tuple) = bind_error_fields(fields, &fmt_value);
    let display_body = if is_tuple && !idents.is_empty() {
        let idents_j = join_comma(&idents);
        quote_tokens! { ::core::write!(f, #fmt, #idents_j) }
    } else {
        quote_tokens! { ::core::write!(f, #fmt) }
    };
    let source_ident = error_source_ident(fields);
    let source_body = match &source_ident {
        Some(sf) => {
            quote_tokens! { ::core::option::Option::Some(&self.#sf as &(dyn ::std::error::Error + 'static)) }
        }
        None => quote_tokens! { ::core::option::Option::None },
    };
    let from_impl = error_from_ctor(fields, |f| f)
        .map(|(ty, ctor_fields)| {
            quote_tokens! {
                impl ::core::convert::From<#ty> for #name {
                    fn from(source: #ty) -> Self { Self #ctor_fields }
                }
            }
        })
        .unwrap_or_default();

    quote_tokens! {
        #[allow(unused_variables)]
        impl ::std::fmt::Display for #name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                let Self #pat = self;
                #display_body
            }
        }
        impl ::std::error::Error for #name {
            fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> {
                #source_body
            }
        }
        #from_impl
    }
}

// ── #[derive(Validate)] — the `validator` masquerade ──
//
// Scope is the real crate's surveyed usage, not its full feature set: `email`, `length(min,
// max)`, `range(min, max)`, `regex(path = "*STATIC", code = "...")`, `custom(function = "...")`,
// `nested` are the only attribute shapes any caller uses. See `wirefold::validate`'s module
// doc for the runtime types this generates code against.

/// One `#[validate(...)]` field attribute, parsed.
pub(crate) enum ValidateSpec {
    Email,
    Nested,
    Length {
        min: Option<i64>,
        max: Option<i64>,
    },
    Range {
        min: Option<i64>,
        max: Option<i64>,
    },
    // `needs_get`: this codebase's regex statics are a genuine mix of `OnceLock<Regex>`
    // (value chosen at runtime, e.g. locale-dependent - `.get()` needed) and `LazyLock<Regex>`
    // (fixed at declaration, auto-derefs - no `.get()`). The attribute's `path` string carries
    // which one via a leading `*` (`"*RE_X"` = LazyLock, bare `"RE_X"` = OnceLock) - the same
    // convention the upstream `validator`-derived attributes already used.
    Regex {
        path_ident: Ident,
        code: Option<String>,
        needs_get: bool,
    },
    Custom {
        function: Ident,
    },
}

/// Split a token stream on TOP-LEVEL commas only (never inside a nested group) — e.g.
/// `length(min = 1), custom(function = "f")` -> two segments, while `min = 1, max = 2` (already
/// one segment's own args) needs the same split one level down.
pub(crate) fn split_top_level_commas(ts: &PTokenStream) -> Vec<PTokenStream> {
    let mut out = Vec::new();
    let mut current = PTokenStream::new();
    for tt in ts.iter() {
        if let TokenTree::Punct(p) = tt {
            if p.ch == ',' {
                if !current.is_empty() {
                    out.push(std::mem::replace(&mut current, PTokenStream::new()));
                }
                continue;
            }
        }
        current.push(tt.clone());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// `pair` is one already-split `key = value` segment; returns `value`'s token if `pair`'s key
/// matches `key`.
pub(crate) fn kv_value<'a>(pair: &'a PTokenStream, key: &str) -> Option<&'a TokenTree> {
    match pair.get(0) {
        Some(TokenTree::Ident(id)) if id.sym == key => {}
        _ => return None,
    }
    match pair.get(1) {
        Some(TokenTree::Punct(p)) if p.ch == '=' => {}
        _ => return None,
    }
    pair.get(2)
}

pub(crate) fn lit_i64(tt: &TokenTree) -> Option<i64> {
    let TokenTree::Literal(l) = tt else {
        return None;
    };
    let digits: String = l
        .text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

pub(crate) fn lit_string(tt: &TokenTree) -> Option<String> {
    let TokenTree::Literal(l) = tt else {
        return None;
    };
    l.string_value()
}

/// `length`/`range` share the same `(min = N, max = N)` shape (either bound optional).
pub(crate) fn parse_min_max(args: &PTokenStream) -> (Option<i64>, Option<i64>) {
    let mut min = None;
    let mut max = None;
    for pair in split_top_level_commas(args) {
        if let Some(v) = kv_value(&pair, "min") {
            min = lit_i64(v);
        }
        if let Some(v) = kv_value(&pair, "max") {
            max = lit_i64(v);
        }
    }
    (min, max)
}

/// One already-comma-split spec segment (e.g. `email`, `nested`, or `length ( min = 1 )`) into
/// its parsed shape. `None` for an attribute shape this masquerade doesn't cover (see the module
/// doc) - the caller silently skips it rather than erroring, since an unrecognized shape here
/// means a real feature nothing in this codebase uses yet, not a malformed attribute.
pub(crate) fn parse_validate_spec(spec: &PTokenStream) -> Option<ValidateSpec> {
    let TokenTree::Ident(kind) = spec.get(0)? else {
        return None;
    };
    match kind.sym.as_str() {
        "email" => Some(ValidateSpec::Email),
        "nested" => Some(ValidateSpec::Nested),
        "length" | "range" | "regex" | "custom" => {
            let TokenTree::Group(g) = spec.get(1)? else {
                return None;
            };
            if g.delimiter != Delimiter::Parenthesis {
                return None;
            }
            match kind.sym.as_str() {
                "length" => {
                    let (min, max) = parse_min_max(&g.stream);
                    Some(ValidateSpec::Length { min, max })
                }
                "range" => {
                    let (min, max) = parse_min_max(&g.stream);
                    Some(ValidateSpec::Range { min, max })
                }
                "regex" => {
                    let mut path_str = None;
                    let mut code = None;
                    for pair in split_top_level_commas(&g.stream) {
                        if let Some(v) = kv_value(&pair, "path") {
                            path_str = lit_string(v);
                        }
                        if let Some(v) = kv_value(&pair, "code") {
                            code = lit_string(v);
                        }
                    }
                    let ident_name = path_str?;
                    let needs_get = !ident_name.starts_with('*');
                    let ident_name = ident_name.trim_start_matches('*');
                    Some(ValidateSpec::Regex {
                        path_ident: Ident::new(ident_name, Span::call_site()),
                        code,
                        needs_get,
                    })
                }
                "custom" => {
                    let mut function = None;
                    for pair in split_top_level_commas(&g.stream) {
                        if let Some(v) = kv_value(&pair, "function") {
                            function = lit_string(v);
                        }
                    }
                    Some(ValidateSpec::Custom {
                        function: Ident::new(&function?, Span::call_site()),
                    })
                }
                _ => unreachable!(),
            }
        }
        _ => None,
    }
}

/// Every `#[validate(...)]` spec on one field, in declaration order (comma-separated specs
/// inside one attribute AND multiple stacked `#[validate(...)]` attributes both flatten here).
pub(crate) fn field_validate_specs(attrs: &[Attribute]) -> Vec<ValidateSpec> {
    attrs
        .iter()
        .filter(|a| a.path == "validate")
        .filter_map(|a| match a.tokens.get(0) {
            Some(TokenTree::Group(g)) if g.delimiter == Delimiter::Parenthesis => Some(&g.stream),
            _ => None,
        })
        .flat_map(split_top_level_commas)
        .filter_map(|spec| parse_validate_spec(&spec))
        .collect()
}

/// A value-level check (email/length/range/regex/custom) runs against `__v: &InnerType`, where
/// `InnerType` is the field's type with ONE `Option` layer already stripped if present -
/// `#[validate(...)]` on an `Option<T>` field means "check it if present", matching the real
/// crate. This wraps that one shared unwrap around whichever check body the caller supplies.
pub(crate) fn wrap_optional(
    fname: &Ident,
    is_option: bool,
    inner_check: proc_masq::TokenStream,
) -> proc_masq::TokenStream {
    if is_option {
        quote_tokens! {
            if let ::core::option::Option::Some(__v) = self.#fname.as_ref() {
                #inner_check
            }
        }
    } else {
        quote_tokens! {
            {
                let __v = &self.#fname;
                #inner_check
            }
        }
    }
}

pub(crate) fn derive_validate_struct(name: &Ident, fields: &[Field]) -> proc_masq::TokenStream {
    let mut checks: Vec<proc_masq::TokenStream> = Vec::new();

    for f in fields {
        let fname = f.ident.clone().unwrap();
        let fname_str = fname.sym.to_string();
        let is_option = option_inner(&f.ty).is_some();

        for spec in field_validate_specs(&f.attrs) {
            let check = match spec {
                ValidateSpec::Email => wrap_optional(
                    &fname,
                    is_option,
                    quote_tokens! {
                        if !::wirefold::validate::is_email(__v.as_str()) {
                            __errors.add(#fname_str, ::wirefold::validate::ValidationError::new("email"));
                        }
                    },
                ),
                ValidateSpec::Length { min, max } => {
                    let min_check = min.map(|m| {
                        let lit = Literal::i64_unsuffixed(m, Span::call_site());
                        quote_tokens! { || __len < #lit }
                    });
                    let max_check = max.map(|m| {
                        let lit = Literal::i64_unsuffixed(m, Span::call_site());
                        quote_tokens! { || __len > #lit }
                    });
                    let cond = concat_tokens([min_check, max_check].into_iter().flatten());
                    // `#[validate(length(...))]` on a `Vec<T>` (or `Option<Vec<T>>`) field means
                    // element count, matching the real `validator` crate's blanket length impl —
                    // `.chars().count()` unconditionally assumed a string field, a real bug this
                    // fixes: it failed to compile at all on a real caller's `Vec<AccessRights>`
                    // field (no `.chars()` method on `Vec`), not just a silently-wrong count.
                    let unwrapped_ty = option_inner(&f.ty).unwrap_or_else(|| f.ty.clone());
                    let len_expr = if vec_inner(&unwrapped_ty).is_some() {
                        quote_tokens! { __v.len() as i64 }
                    } else {
                        quote_tokens! { __v.chars().count() as i64 }
                    };
                    wrap_optional(
                        &fname,
                        is_option,
                        quote_tokens! {
                            let __len = #len_expr;
                            if false #cond {
                                __errors.add(#fname_str, ::wirefold::validate::ValidationError::new("length"));
                            }
                        },
                    )
                }
                ValidateSpec::Range { min, max } => {
                    let min_check = min.map(|m| {
                        let lit = Literal::i64_unsuffixed(m, Span::call_site());
                        quote_tokens! { || (*__v as i64) < #lit }
                    });
                    let max_check = max.map(|m| {
                        let lit = Literal::i64_unsuffixed(m, Span::call_site());
                        quote_tokens! { || (*__v as i64) > #lit }
                    });
                    let cond = concat_tokens([min_check, max_check].into_iter().flatten());
                    wrap_optional(
                        &fname,
                        is_option,
                        quote_tokens! {
                            if false #cond {
                                __errors.add(#fname_str, ::wirefold::validate::ValidationError::new("range"));
                            }
                        },
                    )
                }
                ValidateSpec::Regex {
                    path_ident,
                    code,
                    needs_get,
                } => {
                    let code_lit =
                        Literal::string(code.as_deref().unwrap_or("regex"), Span::call_site());
                    // `OnceLock<Regex>` (initialized elsewhere at startup, e.g. rauthy_config.rs's
                    // `RE_X.set(...)`) needs `.get()` first; `LazyLock<Regex>` auto-derefs, so the
                    // bare ident already has `.is_match()` — see `ValidateSpec::Regex`'s doc comment.
                    let regex_ref = if needs_get {
                        quote_tokens! { #path_ident.get().expect("regex static not initialized before validation") }
                    } else {
                        quote_tokens! { #path_ident }
                    };
                    wrap_optional(
                        &fname,
                        is_option,
                        quote_tokens! {
                            if !#regex_ref.is_match(__v.as_str()) {
                                __errors.add(#fname_str, ::wirefold::validate::ValidationError::new(#code_lit));
                            }
                        },
                    )
                }
                ValidateSpec::Custom { function } => wrap_optional(
                    &fname,
                    is_option,
                    quote_tokens! {
                        if let ::core::result::Result::Err(__e) = #function(__v) {
                            __errors.add(#fname_str, __e);
                        }
                    },
                ),
                ValidateSpec::Nested => {
                    if vec_inner(&f.ty).is_some() {
                        quote_tokens! {
                            for __item in self.#fname.iter() {
                                if let ::core::result::Result::Err(__e) = ::wirefold::validate::Validate::validate(__item) {
                                    __errors.merge_nested(#fname_str, __e);
                                }
                            }
                        }
                    } else {
                        wrap_optional(
                            &fname,
                            is_option,
                            quote_tokens! {
                                if let ::core::result::Result::Err(__e) = ::wirefold::validate::Validate::validate(__v) {
                                    __errors.merge_nested(#fname_str, __e);
                                }
                            },
                        )
                    }
                }
            };
            checks.push(check);
        }
    }

    let body = concat_tokens(checks);
    quote_tokens! {
        impl ::wirefold::validate::Validate for #name {
            // length/range emit `__len < min || __len > max`; a single `!(min..=max).contains(&x)`
            // isn't equivalent when only one bound is set, so allow the manual form in the generated body.
            #[allow(clippy::manual_range_contains)]
            fn validate(&self) -> ::core::result::Result<(), ::wirefold::validate::ValidationErrors> {
                let mut __errors = ::wirefold::validate::ValidationErrors::new();
                #body
                __errors.into_result()
            }
        }
    }
}

// ── #[derive(ToSchema)] / #[derive(IntoParams)] — the `utoipa` masquerade ──
//
// Both build directly on `wirefold::openapi`'s `SyntaxTerm`-based schema constructors (see
// that module's doc) and reuse this file's OWN `classify_field`/`FieldShape`/`classify_variant`
// (already built for `#[derive(Term)]`) rather than re-deriving field-shape detection.

/// `#[schema(value_type = X)]` field/variant-level override — the only `#[schema(...)]` shape
/// any caller uses (surveyed): force this field's schema to a specific primitive regardless of
/// its real Rust type (an `IpAddr`/lifetime-generic field documented as a plain string).
pub(crate) fn schema_value_type_override(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().filter(|a| a.path == "schema").find_map(|a| {
        let TokenTree::Group(g) = a.tokens.get(0)? else {
            return None;
        };
        for pair in split_top_level_commas(&g.stream) {
            if let Some(TokenTree::Ident(id)) = kv_value(&pair, "value_type") {
                return Some(id.sym.to_string());
            }
        }
        None
    })
}

pub(crate) fn primitive_schema_expr(name: &str) -> Option<proc_masq::TokenStream> {
    match name {
        "str" | "String" => Some(quote_tokens! { ::wirefold::openapi::schema_string() }),
        "bool" => Some(quote_tokens! { ::wirefold::openapi::schema_bool() }),
        "f32" | "f64" => Some(quote_tokens! { ::wirefold::openapi::schema_number() }),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => Some(quote_tokens! { ::wirefold::openapi::schema_integer() }),
        _ => None,
    }
}

/// One field's schema, as a generated expression building a `SyntaxTerm` at runtime.
pub(crate) fn field_shape_schema_expr(shape: &FieldShape) -> proc_masq::TokenStream {
    match shape {
        FieldShape::Str => quote_tokens! { ::wirefold::openapi::schema_string() },
        FieldShape::Bool => quote_tokens! { ::wirefold::openapi::schema_bool() },
        FieldShape::Num(ty) => {
            if is_bare_ident(ty, "f32") || is_bare_ident(ty, "f64") {
                quote_tokens! { ::wirefold::openapi::schema_number() }
            } else {
                quote_tokens! { ::wirefold::openapi::schema_integer() }
            }
        }
        FieldShape::Nested(ty) => quote_tokens! {
            ::wirefold::openapi::schema_ref(<#ty as ::wirefold::openapi::ToSchema>::schema_name())
        },
        FieldShape::OptionOf(inner) => field_shape_schema_expr(inner),
        FieldShape::ListOf(inner) => {
            let inner_expr = field_shape_schema_expr(inner);
            quote_tokens! { ::wirefold::openapi::schema_array(#inner_expr) }
        }
    }
}

pub(crate) fn derive_to_schema_struct(name: &Ident, fields: &[Field]) -> proc_masq::TokenStream {
    let mut properties = Vec::new();
    let mut required = Vec::new();

    for f in fields {
        let fname = f.ident.clone().unwrap();
        let fname_str = Literal::string(&fname.sym, Span::call_site());
        let shape = classify_field(&f.ty);
        let is_optional = matches!(shape, FieldShape::OptionOf(_));

        let schema_expr =
            match schema_value_type_override(&f.attrs).and_then(|t| primitive_schema_expr(&t)) {
                Some(expr) => expr,
                None => field_shape_schema_expr(&shape),
            };
        properties.push(quote_tokens! { (#fname_str.to_string(), #schema_expr) });
        if !is_optional {
            required.push(quote_tokens! { #fname_str.to_string() });
        }
    }

    let properties_j = join_comma(properties);
    let required_j = join_comma(required);
    let name_str = Literal::string(&name.sym, Span::call_site());
    quote_tokens! {
        impl ::wirefold::openapi::ToSchema for #name {
            fn schema_term() -> ::wirefold::codec::syntax::SyntaxTerm {
                ::wirefold::openapi::schema_object(::std::vec![ #properties_j ], ::std::vec![ #required_j ])
            }
            fn schema_name() -> &'static str {
                #name_str
            }
        }
    }
}

/// One data-carrying variant's schema override, honoring `#[schema(value_type = X)]` on the
/// variant itself (`Getent::HostIp`'s `IpAddr` payload documented as a plain string).
pub(crate) fn variant_payload_schema_expr(
    attrs: &[Attribute],
    shape: &FieldShape,
) -> proc_masq::TokenStream {
    match schema_value_type_override(attrs).and_then(|t| primitive_schema_expr(&t)) {
        Some(expr) => expr,
        None => field_shape_schema_expr(shape),
    }
}

pub(crate) fn derive_to_schema_enum(name: &Ident, variants: &[Variant]) -> proc_masq::TokenStream {
    let all_unit = variants.iter().all(|v| matches!(v.fields, Fields::Unit));

    let body = if all_unit {
        let names = join_comma(variants.iter().map(|v| {
            let s = Literal::string(&v.ident.sym, Span::call_site());
            quote_tokens! { #s.to_string() }
        }));
        quote_tokens! { ::wirefold::openapi::schema_string_enum(::std::vec![ #names ]) }
    } else {
        // Externally-tagged (serde's default, matching `#[derive(Term)]`'s own enum encoding):
        // one `oneOf` branch per variant, each `{"type":"object","properties":{Tag: payload},
        // "required":[Tag]}`.
        let branches = join_comma(variants.iter().map(|v| {
            let tag = Literal::string(&v.ident.sym, Span::call_site());
            let payload = match classify_variant(&v.fields) {
                VariantPayload::Unit => quote_tokens! {
                    ::wirefold::openapi::schema_object(::std::vec![], ::std::vec![])
                },
                VariantPayload::Single(shape) => variant_payload_schema_expr(&v.attrs, &shape),
                VariantPayload::Tuple(shapes) => {
                    // A fixed-arity tuple variant has no single-item schema; document it as an
                    // array of each position's own schema (matches the `Fourth([...])` wire
                    // shape `#[derive(Term)]` gives it - positional, not named).
                    let items = join_comma(shapes.iter().map(field_shape_schema_expr));
                    quote_tokens! {
                        ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![
                            ("type".to_string(), ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str("array".to_string()))),
                            ("items".to_string(), ::wirefold::codec::syntax::SyntaxTerm::Fourth(::std::vec![ #items ])),
                        ])
                    }
                }
                VariantPayload::Named(field_shapes) => {
                    let props = join_comma(field_shapes.iter().map(|(fname, shape)| {
                        let fname_str = Literal::string(&fname.sym, Span::call_site());
                        let expr = field_shape_schema_expr(shape);
                        quote_tokens! { (#fname_str.to_string(), #expr) }
                    }));
                    quote_tokens! { ::wirefold::openapi::schema_object(::std::vec![ #props ], ::std::vec![]) }
                }
            };
            quote_tokens! {
                ::wirefold::openapi::schema_object(
                    ::std::vec![(#tag.to_string(), #payload)],
                    ::std::vec![#tag.to_string()],
                )
            }
        }));
        quote_tokens! {
            {
                let _branches: ::std::vec::Vec<::wirefold::codec::syntax::SyntaxTerm> = ::std::vec![ #branches ];
                ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![(
                    "oneOf".to_string(),
                    ::wirefold::codec::syntax::SyntaxTerm::Fourth(_branches),
                )])
            }
        }
    };

    let name_str = Literal::string(&name.sym, Span::call_site());
    quote_tokens! {
        impl ::wirefold::openapi::ToSchema for #name {
            fn schema_term() -> ::wirefold::codec::syntax::SyntaxTerm {
                #body
            }
            fn schema_name() -> &'static str {
                #name_str
            }
        }
    }
}

pub(crate) fn derive_into_params_struct(name: &Ident, fields: &[Field]) -> proc_masq::TokenStream {
    let entries = join_comma(fields.iter().map(|f| {
        let fname = f.ident.clone().unwrap();
        let fname_str = Literal::string(&fname.sym, Span::call_site());
        let shape = classify_field(&f.ty);
        let is_optional = matches!(shape, FieldShape::OptionOf(_));
        let required_lit = if is_optional { quote_tokens! { false } } else { quote_tokens! { true } };
        let schema_expr = match schema_value_type_override(&f.attrs).and_then(|t| primitive_schema_expr(&t)) {
            Some(expr) => expr,
            None => field_shape_schema_expr(&shape),
        };
        quote_tokens! {
            ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![
                ("name".to_string(), ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str(#fname_str.to_string()))),
                ("in".to_string(), ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str("query".to_string()))),
                ("required".to_string(), ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Bool(#required_lit))),
                ("schema".to_string(), #schema_expr),
            ])
        }
    }));
    quote_tokens! {
        impl ::wirefold::openapi::IntoParams for #name {
            fn params_terms() -> ::std::vec::Vec<::wirefold::codec::syntax::SyntaxTerm> {
                ::std::vec![ #entries ]
            }
        }
    }
}

// ── #[path(...)] — the `#[utoipa::path(...)]` masquerade ──
//
// An ATTRIBUTE macro (not a derive): leaves the annotated handler function completely
// unchanged and generates one sibling function `__path_<handler>` matching
// `wirefold::openapi::PathEntry` (`fn() -> (&'static str, &'static str, SyntaxTerm)`), which
// `wirefold::openapi::build_document` collects by reference from an explicit list - the same
// structure `#[derive(OpenApi)] #[openapi(paths(...))]` already used, just assembled by hand
// instead of a second proc-macro scanning the first's output.
//
// Scope is the real DSL's surveyed usage: a bare method ident, `path = "..."`, `tag = "..."`
// (optional), `params(Type)` (optional, one IntoParams type), `request_body = Type` (optional),
// and `responses((status = N, description = "...", body = Type|[Type])*)`. `security(...)`,
// multiple `params(...)` types, and response `headers(...)` aren't implemented because nothing
// here uses them.

/// Read every token AFTER a `key = ` pair's `=` sign (unlike `kv_value`, which only returns the
/// first) — needed here because a type reference (`request_body = serde_json::Value`) or an
/// array body (`body = [ClientResponse]`) can be more than one token.
pub(crate) fn kv_rest(pair: &PTokenStream) -> Option<PTokenStream> {
    match pair.get(0) {
        Some(TokenTree::Ident(_)) => {}
        _ => return None,
    }
    match pair.get(1) {
        Some(TokenTree::Punct(p)) if p.ch == '=' => {}
        _ => return None,
    }
    let mut rest = PTokenStream::new();
    for tt in pair.iter().skip(2) {
        rest.push(tt.clone());
    }
    Some(rest)
}

/// `body = Type` or `body = [Type]` (array) -> the schema expression, referencing the type's
/// OWN registered schema (`schema_ref`) for composition rather than inlining it, matching how a
/// struct field's `Nested` shape works — UNLESS `Type` is a primitive/foreign type with no
/// `ToSchema` impl (`String`, `serde_json::Value`), same exception `classify_field` makes for
/// struct fields: `wirefold` has no dependency on `serde_json` (zero-dep by design) and
/// doesn't register a schema name for bare `String` either, so those two get the primitive
/// schema shapes (`schema_string`/`schema_any`) directly, not a `schema_ref` lookup that would
/// never resolve. A real gap this closed: `body = String`/`body = serde_json::Value` in real
/// `#[path(...)]` call sites failed with "the trait bound `String: ToSchema` is not satisfied"
/// (also "`serde_json::Value: ToSchema`") before this — not a hypothetical case.
pub(crate) fn body_type_schema_expr(type_tokens: &PTokenStream) -> proc_masq::TokenStream {
    if type_tokens.len() == 1 {
        if let Some(TokenTree::Group(g)) = type_tokens.get(0) {
            if g.delimiter == Delimiter::Bracket {
                let inner = body_type_schema_expr(&g.stream);
                return quote_tokens! { ::wirefold::openapi::schema_array(#inner) };
            }
        }
    }
    if is_bare_ident(type_tokens, "String") || is_bare_ident(type_tokens, "str") {
        return quote_tokens! { ::wirefold::openapi::schema_string() };
    }
    let path_str = type_tokens
        .iter()
        .map(|t| match t {
            TokenTree::Ident(i) => i.sym.to_string(),
            TokenTree::Punct(p) => p.ch.to_string(),
            _ => String::new(),
        })
        .collect::<String>();
    if path_str == "serde_json::Value" || path_str == "::serde_json::Value" {
        return quote_tokens! { ::wirefold::openapi::schema_any() };
    }
    let ty: proc_masq::TokenStream = type_tokens.iter().cloned().collect();
    quote_tokens! { ::wirefold::openapi::schema_ref(<#ty as ::wirefold::openapi::ToSchema>::schema_name()) }
}

pub(crate) struct ResponseEntry {
    status: String,
    description: String,
    body: Option<proc_masq::TokenStream>,
}

pub(crate) fn parse_response_entry(entry: &PTokenStream) -> Option<ResponseEntry> {
    let TokenTree::Group(g) = entry.get(0)? else {
        return None;
    };
    if g.delimiter != Delimiter::Parenthesis {
        return None;
    }
    let mut status = None;
    let mut description = None;
    let mut body = None;
    for pair in split_top_level_commas(&g.stream) {
        if let Some(v) = kv_value(&pair, "status") {
            status = lit_i64(v).map(|n| n.to_string());
        }
        if let Some(v) = kv_value(&pair, "description") {
            description = lit_string(v);
        }
        let is_body = matches!(pair.get(0), Some(TokenTree::Ident(id)) if id.sym == "body");
        if is_body {
            if let Some(rest) = kv_rest(&pair) {
                body = Some(body_type_schema_expr(&rest));
            }
        }
    }
    Some(ResponseEntry {
        status: status?,
        description: description.unwrap_or_default(),
        body,
    })
}

pub(crate) fn parse_path_attr(attr: &PTokenStream) -> proc_masq::TokenStream {
    let mut method = "get".to_string();
    let mut path = String::new();
    let mut tag: Option<String> = None;
    let mut params_ty: Option<PTokenStream> = None;
    let mut request_body_ty: Option<PTokenStream> = None;
    let mut responses: Vec<ResponseEntry> = Vec::new();

    for segment in split_top_level_commas(attr) {
        match segment.get(0) {
            Some(TokenTree::Ident(id)) if segment.len() == 1 => {
                if matches!(
                    id.sym.as_str(),
                    "get" | "post" | "put" | "delete" | "patch" | "options" | "head"
                ) {
                    method = id.sym.to_string();
                }
            }
            Some(TokenTree::Ident(id)) if id.sym == "path" => {
                if let Some(v) = kv_value(&segment, "path") {
                    path = lit_string(v).unwrap_or_default();
                }
            }
            Some(TokenTree::Ident(id)) if id.sym == "tag" => {
                if let Some(v) = kv_value(&segment, "tag") {
                    tag = lit_string(v);
                }
            }
            Some(TokenTree::Ident(id)) if id.sym == "params" => {
                if let Some(TokenTree::Group(g)) = segment.get(1) {
                    params_ty = Some(g.stream.clone());
                }
            }
            Some(TokenTree::Ident(id)) if id.sym == "request_body" => {
                if let Some(rest) = kv_rest(&segment) {
                    request_body_ty = Some(rest);
                }
            }
            Some(TokenTree::Ident(id)) if id.sym == "responses" => {
                if let Some(TokenTree::Group(g)) = segment.get(1) {
                    responses = split_top_level_commas(&g.stream)
                        .iter()
                        .filter_map(parse_response_entry)
                        .collect();
                }
            }
            _ => {}
        }
    }

    let responses_j = join_comma(responses.iter().map(|r| {
        let status_lit = Literal::string(&r.status, Span::call_site());
        let desc_lit = Literal::string(&r.description, Span::call_site());
        let mut fields = vec![quote_tokens! { ("description".to_string(), ::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str(#desc_lit.to_string()))) }];
        if let Some(body_expr) = &r.body {
            fields.push(quote_tokens! { ("content".to_string(), ::wirefold::openapi::object_value(::std::vec![("application/json", ::wirefold::openapi::object_value(::std::vec![("schema", #body_expr)]))])) });
        }
        let fields_j = join_comma(fields);
        quote_tokens! { (#status_lit.to_string(), ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![ #fields_j ])) }
    }));

    let mut op_fields = vec![quote_tokens! {
        ("responses".to_string(), ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![ #responses_j ]))
    }];
    if let Some(t) = &tag {
        let t_lit = Literal::string(t, Span::call_site());
        op_fields.push(quote_tokens! {
            ("tags".to_string(), ::wirefold::codec::syntax::SyntaxTerm::Fourth(::std::vec![::wirefold::codec::syntax::SyntaxTerm::B(::wirefold::codec::syntax::Atom::Str(#t_lit.to_string()))]))
        });
    }
    if let Some(pty) = &params_ty {
        let pty_j: proc_masq::TokenStream = pty.iter().cloned().collect();
        op_fields.push(quote_tokens! {
            ("parameters".to_string(), ::wirefold::codec::syntax::SyntaxTerm::Fourth(<#pty_j as ::wirefold::openapi::IntoParams>::params_terms()))
        });
    }
    if let Some(bty) = &request_body_ty {
        let body_expr = body_type_schema_expr(bty);
        op_fields.push(quote_tokens! {
            ("requestBody".to_string(), ::wirefold::openapi::object_value(::std::vec![("content", ::wirefold::openapi::object_value(::std::vec![("application/json", ::wirefold::openapi::object_value(::std::vec![("schema", #body_expr)]))]))]))
        });
    }
    let op_fields_j = join_comma(op_fields);

    let method_lit = Literal::string(&method, Span::call_site());
    let path_lit = Literal::string(&path, Span::call_site());
    quote_tokens! {
        (#method_lit, #path_lit, ::wirefold::codec::syntax::SyntaxTerm::Bu(::std::vec![ #op_fields_j ]))
    }
}

pub(crate) fn find_fn_name(item: &PTokenStream) -> Option<Ident> {
    let mut saw_fn = false;
    for tt in item.iter() {
        if let TokenTree::Ident(id) = tt {
            if saw_fn {
                return Some(id.clone());
            }
            if id.sym == "fn" {
                saw_fn = true;
            }
        }
    }
    None
}
