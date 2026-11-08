//! proc_masq — `proc_macro2` / `syn` / `quote` masquerade, zero dependencies.
//!
//! Self-contained on purpose: this file has no dependency on the rest of the `wirefold` crate
//! (token storage is a plain `Vec`, not `crate::Table` — a token stream is addressed by position,
//! and a dense position IS the coordinate already; no hash, no table needed on top of that).
//! That is what lets `wirefold_derive` share this exact file via `#[path]` instead of a
//! duplicate copy or a new crate: `wirefold` (this module, feature `proc-macro`) and
//! `wirefold_derive` (its own `proc_macro`-native bridge) both compile it independently — no
//! Cargo dependency edge between them, so no cycle (`wirefold` already optionally depends on
//! `wirefold_derive`; the reverse edge is a hard `cyclic package dependency` error, verified).
//!
//! ## Feature parity vs prior art
//!
//! | proc_macro2 / syn / quote    | wirefold `proc_masq`                | status |
//! |---|---|---|
//! | `TokenStream` (ordered vec)  | `TokenStream` (`Vec<TokenTree>`)        | ✓ |
//! | `TokenTree` enum             | `TokenTree` enum                       | ✓ |
//! | `Ident`, `Literal`, `Punct`  | `Ident`, `Literal`, `Punct`            | ✓ |
//! | `Group` (delimited subtree)  | `Group` (nested TokenStream)           | ✓ |
//! | `Span` (source location)     | `Span` (usize + callerData)            | ✓ |
//! | `Delimiter` / `Spacing`       | `Delimiter` / `Spacing`                | ✓ |
//! | `syn::parse::Parse` trait    | `FromTokens` trait                     | ✓ |
//! | `syn::DeriveInput`           | `DeriveInput` (data, ident, attrs, gens)| ✓ |
//! | `syn::Fields` / `Field`      | `Fields` / `Field` — fields ARE parsed  | ✓ |
//! | `syn::Attribute`             | `Attribute` (+ single-string-arg parse) | ✓ |
//! | `quote::ToTokens` trait      | `ToTokens` trait                       | ✓ |
//! | `quote::quote!` macro        | `quote_tokens!` macro — real `#var` interpolation | ✓ |
//! | `quote!`'s `#(...)* ` repetition | explicit join helpers (`concat_tokens`/`join_comma`/`each_comma`), called before interpolating | ✓ (different surface, same effect) |
//! | `syn::parse_macro_input!`    | `parse_input!` macro                   | ✓ |
//! | `syn` full AST (ItemFn, etc.)| DeriveInput only; remaining NB fold    | − |
//! | `proc_macro2::Ident::new`   | `Ident::new` (same API)                | ✓ |
//! | `proc_macro2::Literal::*`   | `Literal::string/u32/usize/...`        | ✓ |
//! | `proc_macro2::Punct::new`   | `Punct::new` (same API)                | ✓ |
//! | `proc_macro2::Group::new`   | `Group::new` (same API)                | ✓ |
//!
//! ## Why no `#(...)* ` repetition syntax
//!
//! Real `quote!` auto-detects, inside a `#(...)* ` block, which interpolated variables are
//! "repeatable" (via autoref specialization) and zips them in lockstep. That machinery earns its
//! keep when callers can't restructure their own code around it. Every caller here (all ported
//! by hand) can: build the per-item `TokenStream`s with an ordinary `.iter().map(...)`, then join
//! with [`concat_tokens`] (no separator), [`join_comma`] (comma-separated), or [`each_comma`]
//! (comma after every item including the last), and interpolate the single joined result. Same
//! expressive power, far less macro machinery — the "3 or 4 struct decomposition" this framework
//! keeps landing on: repetition wasn't a fourth primitive, it was `concat` composed with `map`.
//!
//! ## Spacing — a post-pass, not per-token lookahead
//!
//! `quote_tokens!` pushes every punctuation character as `Spacing::Alone` while building, then
//! [`fixup_spacing`] walks the finished stream once (recursing into groups) and promotes a punct
//! to `Joint` exactly when the next token is also a bare punct. That single rule reproduces every
//! compound operator Rust's own tokenizer forms (`::`, `->`, `=>`, `+=`, `..`, …) without a table
//! of two-character special cases — verified against a real proc-macro crate: `Spacing::Alone` on
//! both halves of `->` makes rustc's parser reject it (`expected one of` `->`, `where`, or `{`,
//! found `-`); marking the first Joint fixes it, and the same single rule handles every other
//! compound operator this crate's generated code emits.
//!
//! ## No conversion bridge — this REPLACES proc-macro2, it doesn't interoperate with it
//!
//! Feature `proc-macro` enables this module and nothing else — no optional dependency backs it,
//! and none should: the point of `proc_masq` is that nothing building against `wirefold` ever
//! needs the real `proc-macro2` crate, not that it needs it a little less. A crate that IS an
//! actual proc-macro (`proc-macro = true`) doesn't need proc-macro2 at all — it has direct sysroot
//! access to the real, compiler-native `proc_macro` crate, and writes the same handful of `From`
//! impls against THAT directly, the way `wirefold_derive`'s own `pm_bridge` module does
//! (`wirefold_derive/src/lib.rs`). A crate that ISN'T itself a proc-macro has no reason to touch
//! token trees at all. Either way, the real crate drops out.

// A few AST fields (`Generics::where_clause`, `WherePredicate::tokens`, `Field::vis`/`colon`,
// `Data::Union`'s payload, `Variant::discriminant`) round out parity with `syn`'s shape but
// aren't read by either compiling crate today — kept for the derives this enables next, not dead
// in the sense the lint means.
#![allow(dead_code)]

// ── Span ──

/// Source location — a coordinate, not a pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub index: usize,
    pub caller: &'static str,
}

impl Span {
    pub const fn call_site() -> Self {
        Span {
            index: 0,
            caller: "<call_site>",
        }
    }
    pub fn located_at(self, index: usize) -> Span {
        Span { index, ..self }
    }
}

// ── Delimiter and Spacing ──

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delimiter {
    Parenthesis,
    Brace,
    Bracket,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spacing {
    Alone,
    Joint,
}

// ── Sym: the text of an identifier or literal ──

/// A string that lives inline when short (≤ 22 bytes: almost every identifier and number) and on
/// the heap otherwise. Tokens are cloned and dropped constantly while parsing; keeping their text out
/// of the allocator is most of the cost saved. Behaves as a `str`.
#[derive(Clone)]
pub struct Sym(SymRepr);

#[derive(Clone)]
enum SymRepr {
    Inline { len: u8, buf: [u8; Sym::INLINE] },
    Heap(Box<str>),
}

impl Sym {
    const INLINE: usize = 22;

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            // SAFETY: `buf[..len]` was copied from a `&str`, so it is valid UTF-8.
            SymRepr::Inline { len, buf } => unsafe {
                std::str::from_utf8_unchecked(buf.get_unchecked(..*len as usize))
            },
            SymRepr::Heap(b) => b,
        }
    }

    /// Collect characters without an intermediate `String` when the result is short.
    pub fn from_chars(chars: &[char]) -> Sym {
        let mut buf = [0u8; Sym::INLINE];
        let mut len = 0usize;
        for &c in chars {
            let n = c.len_utf8();
            if len + n > Sym::INLINE {
                return Sym(SymRepr::Heap(
                    chars.iter().collect::<String>().into_boxed_str(),
                ));
            }
            c.encode_utf8(&mut buf[len..]);
            len += n;
        }
        Sym(SymRepr::Inline {
            len: len as u8,
            buf,
        })
    }
}

impl From<&str> for Sym {
    #[inline]
    fn from(s: &str) -> Sym {
        if s.len() <= Sym::INLINE {
            let mut buf = [0u8; Sym::INLINE];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Sym(SymRepr::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            Sym(SymRepr::Heap(s.into()))
        }
    }
}
impl From<String> for Sym {
    fn from(s: String) -> Sym {
        if s.len() <= Sym::INLINE {
            Sym::from(s.as_str())
        } else {
            Sym(SymRepr::Heap(s.into_boxed_str()))
        }
    }
}
impl From<Sym> for String {
    fn from(s: Sym) -> String {
        s.as_str().to_string()
    }
}
impl std::ops::Deref for Sym {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl AsRef<str> for Sym {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl std::borrow::Borrow<str> for Sym {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
impl Default for Sym {
    fn default() -> Sym {
        Sym::from("")
    }
}
impl std::fmt::Debug for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}
impl std::fmt::Display for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl PartialEq for Sym {
    #[inline]
    fn eq(&self, o: &Sym) -> bool {
        self.as_str() == o.as_str()
    }
}
impl Eq for Sym {}
impl PartialEq<str> for Sym {
    #[inline]
    fn eq(&self, o: &str) -> bool {
        self.as_str() == o
    }
}
impl PartialEq<&str> for Sym {
    #[inline]
    fn eq(&self, o: &&str) -> bool {
        self.as_str() == *o
    }
}
impl PartialEq<String> for Sym {
    fn eq(&self, o: &String) -> bool {
        self.as_str() == o.as_str()
    }
}
impl std::hash::Hash for Sym {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.as_str().hash(h)
    }
}

// ── Token types ──

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub sym: Sym,
    pub span: Span,
}

impl Ident {
    pub fn new(sym: &str, span: Span) -> Self {
        Ident {
            sym: Sym::from(sym),
            span,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Literal {
    pub text: Sym,
    pub span: Span,
}

impl Literal {
    pub fn string(s: &str, span: Span) -> Self {
        Literal {
            text: format!("\"{}\"", s.escape_default()).into(),
            span,
        }
    }

    pub fn u8(n: u8, span: Span) -> Self {
        Literal {
            text: format!("{n}_u8").into(),
            span,
        }
    }

    /// An untyped integer literal — no suffix, so it infers to whatever the comparison site
    /// needs (the `validator` masquerade's `#[validate(length/range(min = N, max = N))]` splices
    /// these against fields of varying integer types; a suffix would force one and break others).
    pub fn i64_unsuffixed(n: i64, span: Span) -> Self {
        Literal {
            text: format!("{n}").into(),
            span,
        }
    }

    pub fn u32(n: u32, span: Span) -> Self {
        Literal {
            text: format!("{n}_u32").into(),
            span,
        }
    }

    pub fn usize(n: usize, span: Span) -> Self {
        Literal {
            text: format!("{n}_usize").into(),
            span,
        }
    }

    pub fn byte_string(b: &[u8], span: Span) -> Self {
        let s: String = b
            .iter()
            .flat_map(|b| std::ascii::escape_default(*b))
            .map(char::from)
            .collect();
        Literal {
            text: format!("b\"{s}\"").into(),
            span,
        }
    }

    pub fn character(c: char, span: Span) -> Self {
        Literal {
            text: format!("'{}'", c.escape_default()).into(),
            span,
        }
    }

    pub fn bool(b: bool, span: Span) -> Self {
        Literal {
            text: if b { "true".into() } else { "false".into() },
            span,
        }
    }

    /// True iff this literal's raw text is a (possibly-escaped) string literal — starts and ends
    /// with an unescaped `"`. Distinguishes `#[error("...")]`'s argument from a numeric/char one.
    pub fn is_string(&self) -> bool {
        self.text.starts_with('"') && self.text.ends_with('"') && self.text.len() >= 2
    }

    /// The unescaped content of a string literal (`"a\nb"` -> `a` newline `b`). `None` if this
    /// literal isn't a string. Mirrors `syn::LitStr::value()` for the one escape set this crate's
    /// `#[error("...")]` messages actually use.
    pub fn string_value(&self) -> Option<String> {
        if !self.is_string() {
            return None;
        }
        let inner = &self.text[1..self.text.len() - 1];
        if !inner.contains('\\') {
            return Some(inner.to_string());
        }
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                // `\u{e9}` — any number of hex digits (and `_`) between the braces.
                Some('u') if chars.peek() == Some(&'{') => {
                    chars.next();
                    let mut code = 0u32;
                    for h in chars.by_ref() {
                        match h {
                            '}' => break,
                            '_' => {}
                            h => {
                                code = code
                                    .wrapping_mul(16)
                                    .wrapping_add(h.to_digit(16).unwrap_or(0))
                            }
                        }
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                // `\x41` — two hex digits (ASCII range in a `str`).
                Some('x') => {
                    let hi = chars.next().and_then(|d| d.to_digit(16)).unwrap_or(0);
                    let lo = chars.next().and_then(|d| d.to_digit(16)).unwrap_or(0);
                    out.push(char::from_u32(hi * 16 + lo).unwrap_or('\u{fffd}'));
                }
                // a backslash at the end of a line swallows the newline and the next line's indent
                Some('\n') | Some('\r') => {
                    while matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                        chars.next();
                    }
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        Some(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Punct {
    pub ch: char,
    pub spacing: Spacing,
    pub span: Span,
}

impl Punct {
    pub fn new(ch: char, spacing: Spacing) -> Self {
        Punct {
            ch,
            spacing,
            span: Span::call_site(),
        }
    }
}

// ── TokenTree ──

/// The universal Term at α=RustToken: one of four constructors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenTree {
    Ident(Ident),
    Literal(Literal),
    Punct(Punct),
    Group(Group),
}

/// A delimited group — nested token stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub delimiter: Delimiter,
    pub stream: TokenStream,
    pub span: Span,
}

impl Group {
    pub fn new(delimiter: Delimiter, stream: TokenStream) -> Self {
        Group {
            delimiter,
            stream,
            span: Span::call_site(),
        }
    }

    pub fn set_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }
}

// ── TokenStream ──

/// An ordered token stream. Position-indexed by a plain `Vec` — a sequence doesn't need a hash
/// table underneath it; the position already IS the coordinate (see the module doc).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenStream {
    tokens: Vec<TokenTree>,
}

impl TokenStream {
    pub fn new() -> Self {
        TokenStream { tokens: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        TokenStream {
            tokens: Vec::with_capacity(cap),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Append a token to the end.
    pub fn push(&mut self, tt: TokenTree) {
        self.tokens.push(tt);
    }

    /// Extend from an iterator of tokens.
    pub fn extend<I: IntoIterator<Item = TokenTree>>(&mut self, iter: I) {
        self.tokens.extend(iter);
    }

    /// Get token at position (O(1), direct index — the position is the address).
    pub fn get(&self, pos: usize) -> Option<&TokenTree> {
        self.tokens.get(pos)
    }

    /// Start a cursor at position 0.
    pub fn cursor(&self) -> Cursor<'_> {
        Cursor {
            stream: self,
            pos: 0,
        }
    }

    /// Iterate all tokens in order.
    pub fn iter(&self) -> impl Iterator<Item = &TokenTree> {
        self.tokens.iter()
    }

    /// Drain all tokens into a Vec<TokenTree>.
    pub fn into_tokens(self) -> Vec<TokenTree> {
        self.tokens
    }
}

impl FromIterator<TokenTree> for TokenStream {
    fn from_iter<I: IntoIterator<Item = TokenTree>>(iter: I) -> Self {
        TokenStream {
            tokens: iter.into_iter().collect(),
        }
    }
}

// ── Cursor — `fromTerm` at α=RustToken position ──

/// The cursor IS the fold: advance, peek, eat tokens at the current position.
/// `FromTokens` moves the cursor forward; `ToTokens` appends behind it.
pub struct Cursor<'a> {
    stream: &'a TokenStream,
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn at_end(&self) -> bool {
        self.pos >= self.stream.len()
    }

    /// Peek at the current token without advancing.
    pub fn peek(&self) -> Option<&'a TokenTree> {
        self.stream.get(self.pos)
    }

    /// Consume and advance one token.
    pub fn next(&mut self) -> Option<&'a TokenTree> {
        let tok = self.stream.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    /// Eat a token if it matches the predicate.
    pub fn eat_if(&mut self, f: impl FnOnce(&TokenTree) -> bool) -> Option<&'a TokenTree> {
        let tok = self.peek();
        if tok.is_some_and(f) {
            self.next()
        } else {
            None
        }
    }

    /// Eat a Group with the given delimiter, return its inner stream.
    pub fn group(&mut self, delim: Delimiter) -> Option<&'a TokenStream> {
        match self.peek() {
            Some(TokenTree::Group(g)) if g.delimiter == delim => {
                self.next();
                Some(&g.stream)
            }
            _ => None,
        }
    }

    /// Eat an Ident returning its symbol.
    pub fn ident(&mut self) -> Option<&'a str> {
        match self.peek() {
            Some(TokenTree::Ident(i)) => {
                self.next();
                Some(&i.sym)
            }
            _ => None,
        }
    }

    /// Eat a Punct with the given char.
    pub fn punct(&mut self, ch: char) -> bool {
        self.eat_if(|t| matches!(t, TokenTree::Punct(p) if p.ch == ch))
            .is_some()
    }

    /// Eat a Literal returning its text.
    pub fn literal(&mut self) -> Option<&'a str> {
        match self.peek() {
            Some(TokenTree::Literal(l)) => {
                self.next();
                Some(&l.text)
            }
            _ => None,
        }
    }
}

// ── ParseError ──

#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg: String,
    pub at_pos: usize,
    pub at_span: Span,
}

impl ParseError {
    pub fn new(msg: impl Into<String>, cursor: &Cursor) -> Self {
        let span = cursor
            .peek()
            .map(|t| match t {
                TokenTree::Ident(i) => i.span,
                TokenTree::Literal(l) => l.span,
                TokenTree::Punct(p) => p.span,
                TokenTree::Group(g) => g.span,
            })
            .unwrap_or_default();
        ParseError {
            msg: msg.into(),
            at_pos: cursor.pos(),
            at_span: span,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at position {} (span {:?})",
            self.msg, self.at_pos, self.at_span
        )
    }
}

impl std::error::Error for ParseError {}

// ── FromTokens — the `syn::parse::Parse` masquerade ──

/// Parse from a token stream cursor. This IS `fromTerm` at α=RustToken:
/// the cursor advances positionally, each `FromTokens` impl extracting
/// one node from the stream.
pub trait FromTokens: Sized {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError>;
}

// ── ToTokens — the `quote::ToTokens` masquerade ──

/// Generate tokens into a stream. The inverse of `fromTerm`.
pub trait ToTokens {
    fn to_tokens(&self, stream: &mut TokenStream);
}

impl ToTokens for TokenStream {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.extend(self.tokens.iter().cloned());
    }
}

impl ToTokens for Ident {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Ident(self.clone()));
    }
}

impl ToTokens for Literal {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Literal(self.clone()));
    }
}

impl ToTokens for Punct {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Punct(self.clone()));
    }
}

impl ToTokens for TokenTree {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(self.clone());
    }
}

impl ToTokens for Group {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Group(self.clone()));
    }
}

impl<T: ToTokens + ?Sized> ToTokens for &T {
    fn to_tokens(&self, stream: &mut TokenStream) {
        (*self).to_tokens(stream);
    }
}

/// `&str`/`String` interpolate as a Rust STRING LITERAL token (`#name` where `name: &str` should
/// splice `"the value"`, not the bare identifier) — the same choice `quote!` makes.
impl ToTokens for str {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Literal(Literal::string(self, Span::call_site())));
    }
}
impl ToTokens for Sym {
    fn to_tokens(&self, stream: &mut TokenStream) {
        self.as_str().to_tokens(stream);
    }
}
impl ToTokens for String {
    fn to_tokens(&self, stream: &mut TokenStream) {
        self.as_str().to_tokens(stream);
    }
}

macro_rules! impl_to_tokens_numeric {
    ($($ty:ident => $ctor:ident),* $(,)?) => {
        $(
            impl ToTokens for $ty {
                fn to_tokens(&self, stream: &mut TokenStream) {
                    stream.push(TokenTree::Literal(Literal::$ctor(*self, Span::call_site())));
                }
            }
        )*
    };
}
impl_to_tokens_numeric!(u8 => u8, u32 => u32, usize => usize);

impl ToTokens for bool {
    fn to_tokens(&self, stream: &mut TokenStream) {
        stream.push(TokenTree::Literal(Literal::bool(*self, Span::call_site())));
    }
}

// ── join helpers — replace `quote!`'s `#(...)* ` repetition (see module doc) ──

/// Concatenate each item's tokens back to back, no separator.
pub fn concat_tokens<T: ToTokens>(items: impl IntoIterator<Item = T>) -> TokenStream {
    let mut out = TokenStream::new();
    for item in items {
        item.to_tokens(&mut out);
    }
    out
}

/// Concatenate with a `,` BETWEEN items (none before the first, none after the last).
pub fn join_comma<T: ToTokens>(items: impl IntoIterator<Item = T>) -> TokenStream {
    let mut out = TokenStream::new();
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
        }
        item.to_tokens(&mut out);
    }
    out
}

/// Concatenate with a `,` AFTER every item, including the last.
pub fn each_comma<T: ToTokens>(items: impl IntoIterator<Item = T>) -> TokenStream {
    let mut out = TokenStream::new();
    for item in items {
        item.to_tokens(&mut out);
        out.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
    }
    out
}

// ── runtime token classification — see `quote_tokens_impl!`'s last rule for why this isn't done
// via macro_rules fragment specifiers ──

/// Classify one already-lexed token's source text (from `stringify!`) and push the right
/// `TokenTree` — a string/char/byte-string/numeric literal, an identifier (keywords included:
/// `true`/`false`/`struct`/… are ordinary idents at the token level), or a punctuation run
/// (pushed one character at a time; [`fixup_spacing`] reassembles compound operators after).
pub fn push_raw_token(stream: &mut TokenStream, text: &str) {
    let span = Span::call_site();
    let Some(first) = text.chars().next() else {
        return;
    };
    // A `"` ANYWHERE in the text means this is a string/byte-string literal, raw or not (`"..."`,
    // `r"..."`, `r#"..."#`, `b"..."`, `br#"..."#`, ...) — an ordinary identifier can never contain
    // one, so this check must come BEFORE the alphabetic-prefix check below. Missing this exact
    // case is what made a doc comment's `#[doc = r"..."]` attribute panic: `stringify!` renders
    // the raw string as `r"..."`, whose first character `r` is alphabetic, so without this check
    // it was misclassified as an identifier named after the entire doc comment's text.
    if text.contains('"') || first == '\'' || first.is_ascii_digit() {
        stream.push(TokenTree::Literal(Literal {
            text: text.to_string().into(),
            span,
        }));
    } else if first == '_' || first.is_alphabetic() {
        stream.push(TokenTree::Ident(Ident::new(text, span)));
    } else {
        for c in text.chars() {
            stream.push(TokenTree::Punct(Punct::new(c, Spacing::Alone)));
        }
    }
}

fn punct_char_at(ts: &TokenStream, idx: usize) -> Option<char> {
    match ts.tokens.get(idx) {
        Some(TokenTree::Punct(p)) => Some(p.ch),
        _ => None,
    }
}

/// Fix up punct spacing so compound operators (`::`, `->`, `=>`, `+=`, `..`, a lifetime's `'`, …)
/// parse correctly, recursing into groups. Verified against a real proc-macro crate: with every
/// punct `Spacing::Alone`, rustc's parser rejects `->` outright ("expected `->`, found `-`").
///
/// A run of the SAME punct character needs its OWN grouping, not pairwise adjacency: a struct
/// field's separator colon directly followed by an absolute path (`#cid: ::wirefold::…`,
/// nothing between them — there's no such thing as inter-token whitespace in a token model) is
/// THREE `:` in a row. Naive left-to-right pairwise Joint glues the first two into a spurious
/// `::` and stranded the third — rustc read that as "expected `:`, found `::`" exactly at the
/// field separator. The fix: a same-char run of length N pairs up `::`-style from the END; an odd
/// leftover (there's always at most one, and it's always the first token of the run, since a lone
/// separator colon precedes a path, never the reverse) stays `Alone`. Different-character
/// adjacent puncts (`-` then `>`, `+` then `=`) have only one possible pairing and keep the
/// simpler adjacent-Joint rule.
pub fn fixup_spacing(ts: &mut TokenStream) {
    let n = ts.tokens.len();
    let mut i = 0;
    while i < n {
        match punct_char_at(ts, i) {
            None => {
                if let TokenTree::Group(g) = &mut ts.tokens[i] {
                    fixup_spacing(&mut g.stream);
                }
                i += 1;
            }
            Some('\'') => {
                if let TokenTree::Punct(p) = &mut ts.tokens[i] {
                    p.spacing = Spacing::Joint;
                }
                i += 1;
            }
            Some(ch) => {
                let mut j = i;
                while punct_char_at(ts, j) == Some(ch) {
                    j += 1;
                }
                let run_len = j - i;
                if run_len >= 2 {
                    let lone = run_len % 2 == 1;
                    let mut k = i;
                    if lone {
                        if let TokenTree::Punct(p) = &mut ts.tokens[k] {
                            p.spacing = Spacing::Alone;
                        }
                        k += 1;
                    }
                    while k + 1 < j {
                        if let TokenTree::Punct(p) = &mut ts.tokens[k] {
                            p.spacing = Spacing::Joint;
                        }
                        if let TokenTree::Punct(p) = &mut ts.tokens[k + 1] {
                            p.spacing = Spacing::Alone;
                        }
                        k += 2;
                    }
                } else {
                    let next_is_different_punct =
                        punct_char_at(ts, i + 1).is_some_and(|c2| c2 != ch);
                    if let TokenTree::Punct(p) = &mut ts.tokens[i] {
                        p.spacing = if next_is_different_punct {
                            Spacing::Joint
                        } else {
                            Spacing::Alone
                        };
                    }
                }
                i = j;
            }
        }
    }
}

// ── quote_tokens! — a real quasi-quoter (see module doc for what it deliberately drops) ──

#[cfg_attr(not(procmasq_no_export), macro_export)]
macro_rules! quote_tokens {
    ($($tokens:tt)*) => {{
        let mut __stream = $crate::proc_masq::TokenStream::new();
        $crate::proc_masq::quote_tokens_impl!(__stream $($tokens)*);
        $crate::proc_masq::fixup_spacing(&mut __stream);
        __stream
    }};
}
// `macro_rules!` items can't take `pub` directly (only `#[macro_export]`, which a
// `proc-macro = true` crate is forbidden from using at all — see the module doc). This
// `pub(crate) use` is what makes `$crate::proc_masq::quote_tokens_impl!` resolve as a path from
// inside `quote_tokens!`'s own expansion when `wirefold_derive` compiles this file — verified:
// without it, the crate-root path fails with "could not find `quote_tokens_impl` in `proc_masq`".
// Two build modes, one path. `quote_tokens!` expands to `$crate::proc_masq::quote_tokens_impl!`,
// so that path must resolve to the helper in BOTH:
//  * `wirefold_derive` (`procmasq_no_export`, no `#[macro_export]` allowed): a crate-private
//    `use` of the local macro_rules item makes the module path resolve.
//  * `wirefold` (macro IS exported): the module path must be PUBLIC, or any other crate calling
//    `wirefold::quote_tokens!` fails with "macro `quote_tokens_impl` is private" (the old
//    unconditional `pub(crate) use` shadowed the exported macro inside the module path).
#[cfg(not(procmasq_no_export))]
#[allow(unused_imports)]
pub use crate::quote_tokens;
#[cfg(procmasq_no_export)]
#[allow(unused_imports)]
pub(crate) use quote_tokens;

#[cfg_attr(not(procmasq_no_export), macro_export)]
macro_rules! quote_tokens_impl {
    // Done.
    ($stream:ident) => {};

    // Interpolate a bound value: #var — var must be `impl ToTokens` (Ident/Literal/TokenStream/
    // String/numeric/bool/anything built from them). This is the ONLY special meaning `#` has.
    ($stream:ident # $var:ident $($rest:tt)*) => {
        $crate::proc_masq::ToTokens::to_tokens(&$var, &mut $stream);
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };

    // Recurse into a delimited group — its contents get their own sub-stream.
    ($stream:ident ( $($inner:tt)* ) $($rest:tt)*) => {
        {
            let mut __g = $crate::proc_masq::TokenStream::new();
            $crate::proc_masq::quote_tokens_impl!(__g $($inner)*);
            $stream.push($crate::proc_masq::TokenTree::Group(
                $crate::proc_masq::Group::new($crate::proc_masq::Delimiter::Parenthesis, __g)));
        }
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };
    ($stream:ident { $($inner:tt)* } $($rest:tt)*) => {
        {
            let mut __g = $crate::proc_masq::TokenStream::new();
            $crate::proc_masq::quote_tokens_impl!(__g $($inner)*);
            $stream.push($crate::proc_masq::TokenTree::Group(
                $crate::proc_masq::Group::new($crate::proc_masq::Delimiter::Brace, __g)));
        }
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };
    ($stream:ident [ $($inner:tt)* ] $($rest:tt)*) => {
        {
            let mut __g = $crate::proc_masq::TokenStream::new();
            $crate::proc_masq::quote_tokens_impl!(__g $($inner)*);
            $stream.push($crate::proc_masq::TokenTree::Group(
                $crate::proc_masq::Group::new($crate::proc_masq::Delimiter::Bracket, __g)));
        }
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };

    // A lifetime (`'a`) — two tokens (a `'` punct + the name), same as proc_macro2 represents it.
    ($stream:ident $lt:lifetime $($rest:tt)*) => {
        {
            let __s = stringify!($lt);
            $stream.push($crate::proc_masq::TokenTree::Punct(
                $crate::proc_masq::Punct::new('\'', $crate::proc_masq::Spacing::Alone)));
            $stream.push($crate::proc_masq::TokenTree::Ident(
                $crate::proc_masq::Ident::new(&__s[1..], $crate::proc_masq::Span::call_site())));
        }
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };

    // Everything else — an ident, a literal, or a (possibly multi-char-glued) punct run —
    // classified at RUNTIME from its source text via `push_raw_token`, deliberately never through
    // macro_rules' `:literal`/`:ident` fragment specifiers. `:literal` greedily commits to an
    // "optional leading `-`" the moment it sees a minus sign, so ordinary subtraction between two
    // interpolations (`#a - #b`) hard-errors ("unexpected token", not a graceful fall-through to
    // the next rule) the instant `-` is followed by anything but a number — verified empirically.
    // A bare `tt` has no such commit-and-fail behavior, so classification is pushed into ordinary
    // Rust code instead, where a leading `-` is just... the first character of the text.
    ($stream:ident $tok:tt $($rest:tt)*) => {
        $crate::proc_masq::push_raw_token(&mut $stream, stringify!($tok));
        $crate::proc_masq::quote_tokens_impl!($stream $($rest)*);
    };
}
#[cfg(not(procmasq_no_export))]
#[allow(unused_imports)]
pub use crate::quote_tokens_impl;
#[cfg(procmasq_no_export)]
#[allow(unused_imports)]
pub(crate) use quote_tokens_impl;

// ── parse_input! — from proc_macro::TokenStream into our types ──

#[cfg_attr(not(procmasq_no_export), macro_export)]
macro_rules! parse_input {
    ($input:expr, $T:ty) => {{
        let ts: $crate::proc_masq::TokenStream = $input.into();
        let mut cursor = ts.cursor();
        match <$T as $crate::proc_masq::FromTokens>::from_tokens(&mut cursor) {
            Ok(v) => v,
            Err(e) => {
                // A proper `compile_error!(...)`, not the parse error's own text re-tokenized as
                // if it were Rust source (which produced garbage output, never caught because
                // this path had no real caller before `wirefold_derive` started using it).
                let __msg = $crate::proc_masq::Literal::string(
                    &e.to_string(),
                    $crate::proc_masq::Span::call_site(),
                );
                return $crate::proc_masq::quote_tokens! { compile_error!(#__msg); }.into();
            }
        }
    }};
}
#[allow(unused_imports)]
// path form kept for symmetry with quote_tokens/quote_tokens_impl; both
// call sites here use the textual (#[macro_use]) form instead
pub(crate) use parse_input;

// ── syn-style AST types (DeriveInput only; remaining syn surface is NB fold) ──

#[derive(Clone, Debug)]
pub struct Attribute {
    pub path: String,
    pub tokens: TokenStream,
}

impl Attribute {
    /// The single string-literal argument of a `#[name("...")]` attribute, if that's the shape.
    /// Backs `#[error("...")]` (`wirefold_derive`'s `thiserror` masquerade).
    pub fn single_string_arg(&self) -> Option<Literal> {
        match self.tokens.get(0) {
            Some(TokenTree::Group(g)) if g.delimiter == Delimiter::Parenthesis => {
                match g.stream.get(0) {
                    Some(TokenTree::Literal(l)) if l.is_string() => Some(l.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Visibility {
    pub tok: TokenStream, // pub / pub(crate) / empty
}

#[derive(Clone, Debug)]
pub struct Generics {
    pub params: Vec<GenericParam>,
    pub where_clause: Vec<WherePredicate>,
}

/// What a generic parameter declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenericKind {
    /// `T`, `T: Bound`, `T = Default`
    Type,
    /// `'a`, `'a: 'b` (the `ident` is the name WITHOUT the tick)
    Lifetime,
    /// `const N: usize`, `const N: usize = 3`
    Const,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    /// Outer attributes on the parameter (`#[allow(..)] T`, or a doc comment, which lexes to `#[doc]`).
    pub attrs: Vec<Attribute>,
    /// The parameter's name; for a lifetime, without the leading `'`.
    pub ident: Ident,
    pub colon: bool, // true if : bound follows
    pub kind: GenericKind,
    /// Tokens after the `:` (the bounds), or a const parameter's type. Empty when `colon` is false.
    pub bounds: TokenStream,
    /// Tokens after `=` (a type or const default), if any.
    pub default: Option<TokenStream>,
}

#[derive(Clone, Debug)]
pub struct WherePredicate {
    pub tokens: TokenStream,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub attrs: Vec<Attribute>,
    pub vis: Visibility,
    pub ident: Option<Ident>,
    pub colon: bool,
    pub ty: TokenStream,
}

#[derive(Clone, Debug)]
pub enum Fields {
    Named(Vec<Field>),
    Unnamed(Vec<Field>),
    Unit,
}

#[derive(Clone, Debug)]
pub enum Data {
    Struct(Fields),
    Enum(Vec<Variant>),
    Union(Fields),
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub attrs: Vec<Attribute>,
    pub ident: Ident,
    pub fields: Fields,
    pub discriminant: Option<Literal>,
}

#[derive(Clone, Debug)]
pub struct DeriveInput {
    pub attrs: Vec<Attribute>,
    pub vis: Visibility,
    pub ident: Ident,
    pub generics: Generics,
    pub data: Data,
}

// ── type-shape helpers on a raw `Field.ty`/`TokenStream` — the `syn::Type` inspection this
// crate's derives actually need (single-generic-argument unwrap; everything else is a plain
// equality check on a one-token ident, both far simpler than a full type grammar) ──

/// `true` iff this type is exactly the bare identifier `name` (e.g. `"String"`, `"bool"`, `"i64"`)
/// — a one-token type, no path segments, no generics.
pub fn is_bare_ident(ty: &TokenStream, name: &str) -> bool {
    ty.len() == 1 && matches!(ty.get(0), Some(TokenTree::Ident(i)) if i.sym == name)
}

/// The inner type of `Outer<T>` where `Outer` is a single leading identifier (`Vec`, `Option`,
/// …) — depth-aware, so `Vec<Vec<T>>`'s outer `Vec` unwraps to the inner `Vec<T>` unchanged, not
/// truncated at the first `>`.
fn generic_inner(ty: &TokenStream, outer_name: &str) -> Option<TokenStream> {
    let first_is_outer = matches!(ty.get(0), Some(TokenTree::Ident(i)) if i.sym == outer_name);
    if !first_is_outer {
        return None;
    }
    if !matches!(ty.get(1), Some(TokenTree::Punct(p)) if p.ch == '<') {
        return None;
    }
    let mut depth = 0i32;
    let mut close = None;
    for i in 2..ty.len() {
        match ty.get(i) {
            Some(TokenTree::Punct(p)) if p.ch == '<' => depth += 1,
            Some(TokenTree::Punct(p)) if p.ch == '>' => {
                if depth == 0 {
                    close = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let close = close?;
    if close != ty.len() - 1 {
        // trailing tokens after the closing `>` — not a bare `Outer<...>` type.
        return None;
    }
    Some(TokenStream::from_iter(
        (2..close).filter_map(|i| ty.get(i).cloned()),
    ))
}

pub fn vec_inner(ty: &TokenStream) -> Option<TokenStream> {
    generic_inner(ty, "Vec")
}

pub fn option_inner(ty: &TokenStream) -> Option<TokenStream> {
    generic_inner(ty, "Option")
}

// ── FromTokens impls for the AST types ──

impl FromTokens for Ident {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        match cursor.next() {
            Some(TokenTree::Ident(i)) => Ok(i.clone()),
            other => Err(ParseError::new(
                format!(
                    "expected ident, got {:?}",
                    other.map(std::mem::discriminant)
                ),
                cursor,
            )),
        }
    }
}

impl FromTokens for Attribute {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        if !cursor.punct('#') {
            return Err(ParseError::new("expected # for attribute", cursor));
        }
        let group = cursor
            .group(Delimiter::Bracket)
            .ok_or_else(|| ParseError::new("expected [...] for attribute", cursor))?;
        let mut inner = group.cursor();
        let path = inner
            .ident()
            .ok_or_else(|| ParseError::new("expected attribute path", &inner))?;
        let tokens = TokenStream::from_iter(
            (inner.pos()..group.len()).filter_map(|i| group.get(i).cloned()),
        );
        Ok(Attribute {
            path: path.to_string(),
            tokens,
        })
    }
}

impl FromTokens for Visibility {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        let is_pub = cursor.peek().and_then(|t| match t {
            TokenTree::Ident(i) => Some(i.sym.as_str()),
            _ => None,
        }) == Some("pub");
        if !is_pub {
            return Ok(Visibility {
                tok: TokenStream::new(),
            });
        }
        cursor.next(); // consume "pub"
        let start = cursor.pos() - 1;
        // `pub(crate)` / `pub(super)` / `pub(in path)`: the restriction is ONE parenthesised group
        // token, not a `(` punct — the old `cursor.punct('(')` test could never match, so
        // `pub(crate) struct S;` failed with "expected struct/enum/union".
        if matches!(cursor.peek(), Some(TokenTree::Group(g)) if g.delimiter == Delimiter::Parenthesis)
        {
            cursor.next();
        }
        let tok = TokenStream::from_iter(
            (start..cursor.pos()).filter_map(|i| cursor.stream.get(i).cloned()),
        );
        Ok(Visibility { tok })
    }
}

/// Scan `ts` from `from` to the first token at angle-bracket depth 0 that is one of `stops`
/// (puncts), or — when `body_stop` — a `;` or a `{}` group (the start of an item body). Returns the
/// index of the stopper and which one it was; `None` means the end of the stream.
///
/// Angle brackets are not delimiters in the token tree, so depth is tracked by hand: `<` opens,
/// `>` closes, and a `>` that is the tail of `->` (or `=>`) is not a close. Every other delimiter is
/// already a single `Group` token and is skipped whole.
///
/// Terminates by construction: `i` strictly increases and is bounded by `ts.len()` (the verified
/// Slang lexer's rule: every step consumes, nothing is re-read).
pub(crate) fn scan_angle(
    ts: &TokenStream,
    from: usize,
    stops: &[char],
    body_stop: bool,
) -> (usize, Option<char>) {
    let mut depth = 0usize;
    let mut i = from;
    while i < ts.len() {
        match ts.get(i) {
            Some(TokenTree::Punct(p)) => match p.ch {
                '<' => depth += 1,
                '>' => {
                    let arrow_tail = i > 0
                        && matches!(ts.get(i - 1), Some(TokenTree::Punct(q))
                            if (q.ch == '-' || q.ch == '=') && q.spacing == Spacing::Joint);
                    if !arrow_tail {
                        if depth == 0 {
                            if stops.contains(&'>') {
                                return (i, Some('>'));
                            }
                        } else {
                            depth -= 1;
                        }
                    }
                }
                ';' if depth == 0 && body_stop => return (i, Some(';')),
                c if depth == 0 && stops.contains(&c) => return (i, Some(c)),
                _ => {}
            },
            Some(TokenTree::Group(g))
                if depth == 0 && body_stop && g.delimiter == Delimiter::Brace =>
            {
                // `ty!{}` is a type macro INSIDE a where predicate, not the item body.
                let after_bang =
                    i > 0 && matches!(ts.get(i - 1), Some(TokenTree::Punct(q)) if q.ch == '!');
                if !after_bang {
                    return (i, Some('{'));
                }
            }
            _ => {}
        }
        i += 1;
    }
    (ts.len(), None)
}

pub(crate) fn slice_tokens(ts: &TokenStream, from: usize, to: usize) -> TokenStream {
    TokenStream::from_iter((from..to).filter_map(|i| ts.get(i).cloned()))
}

fn parse_generic_param(item: &TokenStream, at: &Cursor) -> Result<GenericParam, ParseError> {
    let mut c = item.cursor();
    let mut attrs = Vec::new();
    while matches!(c.peek(), Some(TokenTree::Punct(p)) if p.ch == '#') {
        attrs.push(Attribute::from_tokens(&mut c)?);
    }
    let kind;
    if c.punct('\'') {
        kind = GenericKind::Lifetime;
    } else if matches!(c.peek(), Some(TokenTree::Ident(i)) if i.sym == "const") {
        c.next();
        kind = GenericKind::Const;
    } else {
        kind = GenericKind::Type;
    }
    let name = c
        .ident()
        .ok_or_else(|| ParseError::new("expected a generic parameter name", at))?;
    let ident = Ident::new(name, Span::call_site());
    let colon = c.punct(':');
    // bounds run to a top-level `=` (a default) or the end of the item.
    let from = c.pos();
    let (end, stop) = scan_angle(item, from, &['='], false);
    let bounds = if colon {
        slice_tokens(item, from, end)
    } else {
        TokenStream::new()
    };
    let default = if stop == Some('=') {
        Some(slice_tokens(item, end + 1, item.len()))
    } else if !colon && end > from {
        // `T Foo` — tokens after the name that are neither `:` nor `=`.
        return Err(ParseError::new(
            "unexpected tokens in a generic parameter",
            at,
        ));
    } else {
        None
    };
    Ok(GenericParam {
        attrs,
        ident,
        colon,
        kind,
        bounds,
        default,
    })
}

impl FromTokens for Generics {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        if !cursor.punct('<') {
            return Ok(Generics {
                params: Vec::new(),
                where_clause: Vec::new(),
            });
        }
        let mut params = Vec::new();
        loop {
            // One parameter per iteration, ending at a depth-0 `,` or the closing `>`. Each pass
            // moves `pos` past at least its stopper, so the loop cannot spin.
            let from = cursor.pos;
            let (end, stop) = scan_angle(cursor.stream, from, &[',', '>'], false);
            if stop.is_none() {
                return Err(ParseError::new(
                    "unterminated generic parameter list (missing `>`)",
                    cursor,
                ));
            }
            let item = slice_tokens(cursor.stream, from, end);
            if !item.is_empty() {
                params.push(parse_generic_param(&item, cursor)?);
            }
            cursor.pos = end + 1;
            if stop == Some('>') {
                break;
            }
        }
        Ok(Generics {
            params,
            where_clause: Vec::new(),
        })
    }
}

/// `where P1, P2, ...` up to (not including) the item body: a `{}` group or `;`, or the end of the
/// stream. Returns no predicates when the next token is not `where`.
fn parse_where(cursor: &mut Cursor) -> Result<Vec<WherePredicate>, ParseError> {
    if !matches!(cursor.peek(), Some(TokenTree::Ident(i)) if i.sym == "where") {
        return Ok(Vec::new());
    }
    cursor.next();
    let mut preds = Vec::new();
    loop {
        let from = cursor.pos;
        let (end, stop) = scan_angle(cursor.stream, from, &[','], true);
        if end > from {
            preds.push(WherePredicate {
                tokens: slice_tokens(cursor.stream, from, end),
            });
        }
        if stop == Some(',') {
            cursor.pos = end + 1; // past the comma: strictly forward
        } else {
            cursor.pos = end; // leave the `{`/`;` for the caller
            break;
        }
    }
    Ok(preds)
}

impl FromTokens for Field {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        let mut attrs = Vec::new();
        while cursor
            .peek()
            .map(|t| matches!(t, TokenTree::Punct(p) if p.ch == '#'))
            .unwrap_or(false)
        {
            attrs.push(Attribute::from_tokens(cursor)?);
        }
        let vis = Visibility::from_tokens(cursor)?;

        // A leading identifier is only a field NAME if a SINGLE `:` genuinely follows — an
        // unnamed (tuple) field's leading identifier is the START of its type, so put it back
        // otherwise. Critically, "is `:` next" alone isn't enough: an unnamed field whose type
        // is itself a path (`io::Error`) ALSO has an ident directly followed by a `:` — the
        // second colon of `::` is what tells them apart (`io::Error` fails here on the SECOND
        // colon and rewinds all the way; `name: io::Error`'s single separator colon is followed
        // by an ident, not another colon, so it commits). Missing this distinction turned
        // `Io(#[from] io::Error)` into a field named `io` with type `:Error` — a real,
        // previously-uncaught bug: this whole shape (unnamed field, path type, `#[from]`) had no
        // caller before `wirefold_derive` started using it.
        let mut ident = None;
        let mut colon = false;
        if matches!(cursor.peek(), Some(TokenTree::Ident(_))) {
            let save = cursor.pos();
            let name = cursor.ident().unwrap().to_string();
            let consumed_colon = cursor.punct(':');
            let is_path_sep =
                consumed_colon && matches!(cursor.peek(), Some(TokenTree::Punct(p)) if p.ch == ':');
            if is_path_sep || !consumed_colon {
                cursor.pos = save; // "Name::..." (a path) or no colon at all — put it all back
            } else {
                ident = Some(Ident::new(&name, Span::call_site()));
                colon = true;
            }
        }

        // `ty` spans from here to the next top-level comma (peeked, not consumed — the caller
        // owns comma-eating between fields, matching `Fields`'s own group-boundary consumption).
        let ty_start = cursor.pos();
        while !cursor.at_end() && !matches!(cursor.peek(), Some(TokenTree::Punct(p)) if p.ch == ',')
        {
            cursor.next();
        }
        let ty = TokenStream::from_iter(
            (ty_start..cursor.pos()).filter_map(|i| cursor.stream.get(i).cloned()),
        );
        Ok(Field {
            attrs,
            vis,
            ident,
            colon,
            ty,
        })
    }
}

/// Parse a comma-separated `Vec<Field>` from a delimited group's inner stream (used for both
/// named `{ .. }` and unnamed `( .. )` field lists — `Field::from_tokens` handles either shape).
fn parse_field_list(inner_stream: &TokenStream) -> Result<Vec<Field>, ParseError> {
    let mut inner = inner_stream.cursor();
    let mut fields = Vec::new();
    while !inner.at_end() {
        fields.push(Field::from_tokens(&mut inner)?);
        inner.punct(','); // consume the separator Field::from_tokens left in place
    }
    Ok(fields)
}

impl FromTokens for Fields {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        if let Some(inner_stream) = cursor.group(Delimiter::Brace) {
            return Ok(Fields::Named(parse_field_list(inner_stream)?));
        }
        if let Some(inner_stream) = cursor.group(Delimiter::Parenthesis) {
            return Ok(Fields::Unnamed(parse_field_list(inner_stream)?));
        }
        if cursor.punct(';') {
            return Ok(Fields::Unit);
        }
        Ok(Fields::Unit)
    }
}

impl FromTokens for Data {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        let kw = cursor
            .ident()
            .ok_or_else(|| ParseError::new("expected struct/enum/union", cursor))?;
        match kw {
            "struct" => Ok(Data::Struct(Fields::from_tokens(cursor)?)),
            "enum" => {
                let group = cursor
                    .group(Delimiter::Brace)
                    .ok_or_else(|| ParseError::new("expected { variants }", cursor))?;
                let mut inner = group.cursor();
                let mut variants = Vec::new();
                while !inner.at_end() {
                    let before = inner.pos();
                    let mut attrs = Vec::new();
                    while inner
                        .peek()
                        .map(|t| matches!(t, TokenTree::Punct(p) if p.ch == '#'))
                        .unwrap_or(false)
                    {
                        attrs.push(Attribute::from_tokens(&mut inner)?);
                    }
                    if let Some(name) = inner.ident() {
                        let fields = Fields::from_tokens(&mut inner)?;
                        // an explicit discriminant (`Open = 0`) — legal only on a fieldless
                        // variant, but parsed unconditionally here (a discriminant on a
                        // data-carrying variant is a rustc-level error elsewhere, not this
                        // parser's job to reject).
                        let discriminant = if inner.punct('=') {
                            inner.literal().map(|t| Literal {
                                text: t.to_string().into(),
                                span: Span::call_site(),
                            })
                        } else {
                            None
                        };
                        let v = Variant {
                            attrs,
                            ident: Ident::new(name, Span::call_site()),
                            fields,
                            discriminant,
                        };
                        variants.push(v);
                    }
                    inner.punct(',');
                    if inner.pos() == before {
                        return Err(ParseError::new(
                            "stuck parsing an enum variant (unrecognized token)",
                            &inner,
                        ));
                    }
                }
                Ok(Data::Enum(variants))
            }
            "union" => Ok(Data::Union(Fields::from_tokens(cursor)?)),
            _ => Err(ParseError::new(
                format!("expected struct/enum/union, got {kw}"),
                cursor,
            )),
        }
    }
}

impl FromTokens for DeriveInput {
    fn from_tokens(cursor: &mut Cursor) -> Result<Self, ParseError> {
        let mut attrs = Vec::new();
        while cursor
            .peek()
            .map(|t| matches!(t, TokenTree::Punct(p) if p.ch == '#'))
            .unwrap_or(false)
        {
            attrs.push(Attribute::from_tokens(cursor)?);
        }
        let vis = Visibility::from_tokens(cursor)?;
        let kw = cursor
            .ident()
            .ok_or_else(|| ParseError::new("expected struct/enum/union", cursor))?;
        let ident = Ident::from_tokens(cursor)?;
        let mut generics = Generics::from_tokens(cursor)?;
        // `struct S<T> where T: Bound { .. }` / `enum E<T> where .. { .. }`: the where clause sits
        // between the generics and the body. (Tuple structs put it AFTER the fields — handled below.)
        generics.where_clause = parse_where(cursor)?;
        if matches!(kw, "struct" | "union") {
            let ok = match cursor.peek() {
                None => true,
                Some(TokenTree::Punct(p)) => p.ch == ';',
                Some(TokenTree::Group(g)) => {
                    matches!(g.delimiter, Delimiter::Brace | Delimiter::Parenthesis)
                }
                Some(_) => false,
            };
            if !ok {
                return Err(ParseError::new(
                    "expected `{ fields }`, `( fields )` or `;` after the type header",
                    cursor,
                ));
            }
        }
        let data = match kw {
            "struct" => Data::Struct(Fields::from_tokens(cursor)?),
            "enum" => {
                let group = cursor
                    .group(Delimiter::Brace)
                    .ok_or_else(|| ParseError::new("expected { variants }", cursor))?;
                let mut inner = group.cursor();
                let mut variants = Vec::new();
                while !inner.at_end() {
                    let before = inner.pos();
                    let mut v_attrs = Vec::new();
                    while inner
                        .peek()
                        .map(|t| matches!(t, TokenTree::Punct(p) if p.ch == '#'))
                        .unwrap_or(false)
                    {
                        v_attrs.push(Attribute::from_tokens(&mut inner)?);
                    }
                    if let Some(name) = inner.ident() {
                        let fields = Fields::from_tokens(&mut inner)?;
                        let discriminant = if inner.punct('=') {
                            inner.literal().map(|t| Literal {
                                text: t.to_string().into(),
                                span: Span::call_site(),
                            })
                        } else {
                            None
                        };
                        let v = Variant {
                            attrs: v_attrs,
                            ident: Ident::new(name, Span::call_site()),
                            fields,
                            discriminant,
                        };
                        variants.push(v);
                    }
                    inner.punct(',');
                    if inner.pos() == before {
                        return Err(ParseError::new(
                            "stuck parsing an enum variant (unrecognized token)",
                            &inner,
                        ));
                    }
                }
                Data::Enum(variants)
            }
            "union" => Data::Union(Fields::from_tokens(cursor)?),
            _ => {
                return Err(ParseError::new(
                    format!("expected struct/enum/union, got {kw}"),
                    cursor,
                ))
            }
        };
        // `struct S<T>(T) where T: Bound;` — a tuple struct's where clause follows its fields.
        if generics.where_clause.is_empty() {
            generics.where_clause = parse_where(cursor)?;
        }
        Ok(DeriveInput {
            attrs,
            vis,
            ident,
            generics,
            data,
        })
    }
}

// ── ToTokens impls for AST types ──

impl ToTokens for DeriveInput {
    fn to_tokens(&self, stream: &mut TokenStream) {
        for attr in &self.attrs {
            attr.to_tokens(stream);
        }
        self.vis.to_tokens(stream);
        self.ident.to_tokens(stream);
        self.generics.to_tokens(stream);
        // data fields omitted — NB fold (the inverse is structural, not token-exact)
    }
}

impl ToTokens for Attribute {
    fn to_tokens(&self, stream: &mut TokenStream) {
        // `path` is the attribute's leading ident (`doc`, `allow`, `error`); `tokens` is everything
        // after it. Emitting only `tokens` (as this once did) dropped the path: `#[doc = ".."]`
        // came out as `#[= ".."]`.
        let mut body = TokenStream::new();
        body.push(TokenTree::Ident(Ident::new(&self.path, Span::call_site())));
        body.extend(self.tokens.iter().cloned());
        stream.push(TokenTree::Punct(Punct::new('#', Spacing::Alone)));
        stream.push(TokenTree::Group(Group::new(Delimiter::Bracket, body)));
    }
}

impl ToTokens for Visibility {
    fn to_tokens(&self, stream: &mut TokenStream) {
        self.tok.to_tokens(stream);
    }
}

impl ToTokens for Generics {
    /// The DECLARATION form, `<'a, T: Bound = D, const N: usize>`, for `impl<..>` heads.
    fn to_tokens(&self, stream: &mut TokenStream) {
        if self.params.is_empty() {
            return;
        }
        stream.push(TokenTree::Punct(Punct::new('<', Spacing::Alone)));
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                stream.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
            }
            for a in &p.attrs {
                a.to_tokens(stream);
            }
            match p.kind {
                GenericKind::Lifetime => {
                    stream.push(TokenTree::Punct(Punct::new('\'', Spacing::Joint)));
                }
                GenericKind::Const => {
                    stream.push(TokenTree::Ident(Ident::new("const", Span::call_site())));
                }
                GenericKind::Type => {}
            }
            p.ident.to_tokens(stream);
            if p.colon {
                stream.push(TokenTree::Punct(Punct::new(':', Spacing::Alone)));
                p.bounds.to_tokens(stream);
            }
            if let Some(d) = &p.default {
                stream.push(TokenTree::Punct(Punct::new('=', Spacing::Alone)));
                d.to_tokens(stream);
            }
        }
        stream.push(TokenTree::Punct(Punct::new('>', Spacing::Alone)));
    }
}

impl Generics {
    /// The USAGE form, `<'a, T, N>` — names only, for `Type<..>` in `impl<..> Trait for Type<..>`.
    pub fn type_args(&self) -> TokenStream {
        let mut out = TokenStream::new();
        if self.params.is_empty() {
            return out;
        }
        out.push(TokenTree::Punct(Punct::new('<', Spacing::Alone)));
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                out.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
            }
            if p.kind == GenericKind::Lifetime {
                out.push(TokenTree::Punct(Punct::new('\'', Spacing::Joint)));
            }
            p.ident.to_tokens(&mut out);
        }
        out.push(TokenTree::Punct(Punct::new('>', Spacing::Alone)));
        out
    }

    /// `where P1, P2` (empty when there are no predicates).
    pub fn where_tokens(&self) -> TokenStream {
        let mut out = TokenStream::new();
        if self.where_clause.is_empty() {
            return out;
        }
        out.push(TokenTree::Ident(Ident::new("where", Span::call_site())));
        for (i, w) in self.where_clause.iter().enumerate() {
            if i > 0 {
                out.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
            }
            w.tokens.to_tokens(&mut out);
        }
        out
    }
}

// ── tokenize — raw source text → TokenStream, offline (no proc-macro context needed) ──
//
// Every other way this file's types get populated is either FROM the compiler (during an actual
// proc-macro invocation, converted via a `proc_macro`-native bridge) or built by hand (via
// `quote_tokens!`, for output). Neither reads raw `.rs` source text. This closes that gap: a
// offline codegen tool (one used to live in this repo as `wirefold_codegen`; removed, nothing ran it)
// could share this file via `#[path]`, same as `wirefold_derive` does, and tokenize a real source file and drive the exact same derive logic offline, with no
// `proc-macro2` dependency — Joint/Alone spacing is read directly off source adjacency (no
// intervening whitespace/comment), which is simpler and more exact than reconstructing it after
// the fact the way `quote_tokens!`-built code has to (see `fixup_spacing`'s doc comment).

/// An error while tokenizing raw source text — the byte offset plus what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizeError {
    pub message: String,
    pub pos: usize,
}

impl std::fmt::Display for TokenizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tokenize error at char {}: {}", self.pos, self.message)
    }
}
impl std::error::Error for TokenizeError {}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A non-doc comment (`// ..` or `/* .. */`) found while tokenizing, recorded by
/// [`tokenize_with_comments`]. Doc comments (`///`, `//!`, `/** */`, `/*! */`) are tokens, never
/// `Comment`s.
///
/// Position is given in token-tree terms so a consumer can re-attach the comment to the tree
/// without byte offsets: tokens are counted in depth-first pre-order — a [`Group`] counts as ONE
/// token at its open delimiter and nothing at its close, and the three-or-more tokens a doc comment
/// expands to count like any others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Exact source text, delimiters included (`// x`, `/* x */`), without a trailing newline.
    pub text: String,
    /// `true` for `/* .. */`, `false` for `// ..`.
    pub block: bool,
    /// Tokens lexed before this comment (pre-order count): the pre-order index of the next token.
    pub before_token: usize,
    /// Only whitespace precedes the comment on its line.
    pub own_line: bool,
    /// Lexed right after a source group's closing delimiter, before any further token at that
    /// nesting level (so the comment trails that group rather than the token before it).
    pub after_close_of_group: bool,
    /// Group nesting depth the comment sits at (0 = top level). Together with `before_token` this
    /// tells a comment at the end of a group (`f(a /* c */)`) from one right after it
    /// (`f(a) /* c */`): both have the same `before_token`, but different `depth`.
    pub depth: usize,
}

/// Comment-recording state threaded through the lexer. With `C = false` (plain [`tokenize`]) every
/// use is behind `if C` and compiles away; the struct is a few zeroed words and never allocates.
struct Rec {
    comments: Vec<Comment>,
    /// `(start, end)` char indices of every identifier and literal token, in source order (a doc
    /// comment contributes the two it expands to, both spanning the comment).
    words: Vec<(usize, usize)>,
    /// Tokens emitted so far (pre-order).
    count: usize,
    depth: usize,
    after_close: bool,
    /// Delimiter groups open right now, counted whether or not comments are recorded. The lexer
    /// recurses once per group, and so do `Clone`, `Drop` and every consumer of the resulting tree.
    nest: usize,
}

/// How many delimiter groups (`(`, `[`, `{`) may be open at once. Deeper input is a
/// [`TokenizeError`], not a stack overflow: the tokenizer, the derived `Drop`/`Clone` of
/// [`TokenStream`] and the formatters all recurse per level, and the input is only linear in the
/// depth. 128 is the nesting `syn` and `proc_macro2` accept.
pub const MAX_GROUP_DEPTH: usize = 128;

/// Tokenize raw Rust source text into a `TokenStream`.
///
/// The result is identical to `proc_macro2::TokenStream::from_str(src)` (the fallback lexer) for
/// source that `proc_macro2` accepts: same tree shape, identifiers (raw idents keep their `r#`),
/// literal text (suffixes included), punct characters and — the part that is easy to get wrong —
/// `Spacing`. Non-doc comments are dropped; see [`tokenize_with_comments`] to keep them.
pub fn tokenize(src: &str) -> Result<TokenStream, TokenizeError> {
    tokenize_impl::<false>(src).map(|(ts, _, _)| ts)
}

/// Like [`tokenize`], additionally returning every non-doc comment, in source order, positioned
/// by token count (see [`Comment`]). The token stream is identical to [`tokenize`]'s.
pub fn tokenize_with_comments(src: &str) -> Result<(TokenStream, Vec<Comment>), TokenizeError> {
    tokenize_impl::<true>(src).map(|(ts, c, _)| (ts, c))
}

type Lexed = (TokenStream, Vec<Comment>, Vec<(usize, usize)>);

fn tokenize_impl<const C: bool>(src: &str) -> Result<Lexed, TokenizeError> {
    // proc_macro2 strips a leading byte-order mark; it is not whitespace to `char::is_whitespace`.
    let stripped = src.strip_prefix('\u{feff}').unwrap_or(src);
    let bom = src.len() - stripped.len();
    let chars: Vec<char> = stripped.chars().collect();
    let mut pos = 0;
    let mut rec = Rec {
        comments: Vec::new(),
        words: Vec::new(),
        count: 0,
        depth: 0,
        after_close: false,
        nest: 0,
    };
    let ts = tokenize_until::<C>(&chars, &mut pos, None, &mut rec)?;
    let mut words = Vec::new();
    if C && !rec.words.is_empty() {
        // char index -> byte offset in `src`
        let mut byte_of = Vec::with_capacity(chars.len() + 1);
        let mut b = bom;
        for &c in &chars {
            byte_of.push(b);
            b += c.len_utf8();
        }
        byte_of.push(b);
        words = rec
            .words
            .iter()
            .map(|&(a, z)| (byte_of[a], byte_of[z]))
            .collect();
    }
    Ok((ts, rec.comments, words))
}

/// Byte spans `(start, end)` of every identifier and literal token of `src`, in source order. A doc
/// comment counts as the two tokens (`doc` and its string) it lexes to, both spanning the comment.
/// The sequence of these tokens is invariant under reformatting, which is what lets comments be
/// re-attached to formatted output.
pub fn word_spans(src: &str) -> Result<Vec<(usize, usize)>, TokenizeError> {
    tokenize_impl::<true>(src).map(|(_, _, w)| w)
}

/// Push the tokens `proc_macro2` produces for a doc comment: `#[doc = "text"]` (outer, `///` or
/// `/** */`) or `#![doc = "text"]` (inner, `//!` or `/*! */`).
fn push_doc_attr(out: &mut Vec<TokenTree>, inner: bool, text: &[char]) {
    let alone = |ch| TokenTree::Punct(Punct::new(ch, Spacing::Alone));
    out.push(alone('#'));
    if inner {
        out.push(alone('!'));
    }
    // Printable Unicode stays as written (`escape_debug`, minus the `'` escape): this is how
    // proc_macro2 spells a doc string, and a parser comparing literals by their source text
    // (`syn::LitStr`) sees `—`, not `\u{2014}`. A NUL is `\0`, or `\x00` when an octal digit
    // follows (so the text cannot be misread as an octal escape).
    let mut t = String::with_capacity(text.len() + 2);
    t.push('"');
    for (i, &c) in text.iter().enumerate() {
        match c {
            // printable ASCII other than `"` and `\`: exactly what `escape_debug` leaves alone
            ' '..='~' if c != '"' && c != '\\' => t.push(c),
            '\'' => t.push(c),
            '\0' => t.push_str(if matches!(text.get(i + 1), Some('0'..='7')) {
                "\\x00"
            } else {
                "\\0"
            }),
            _ => t.extend(c.escape_debug()),
        }
    }
    t.push('"');
    let body = vec![
        TokenTree::Ident(Ident {
            sym: "doc".to_string().into(),
            span: Span::call_site(),
        }),
        alone('='),
        TokenTree::Literal(Literal {
            text: t.into(),
            span: Span::call_site(),
        }),
    ];
    out.push(TokenTree::Group(Group::new(
        Delimiter::Bracket,
        TokenStream { tokens: body },
    )));
}

/// Is `c` whitespace to rustc / proc_macro2 (Unicode `White_Space`, plus the LRM and RLM marks)?
#[inline]
fn is_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{200e}' || c == '\u{200f}'
}

/// Record the comment `chars[start..end]` (`C` mode only).
#[cold]
fn record_comment(chars: &[char], start: usize, end: usize, block: bool, rec: &mut Rec) {
    let mut own_line = true;
    let mut i = start;
    while i > 0 {
        let c = chars[i - 1];
        if c == '\n' {
            break;
        }
        if !is_ws(c) {
            own_line = false;
            break;
        }
        i -= 1;
    }
    rec.comments.push(Comment {
        text: chars[start..end].iter().collect(),
        block,
        before_token: rec.count,
        own_line,
        after_close_of_group: rec.after_close,
        depth: rec.depth,
    });
}

/// Skip whitespace and comments. Plain comments are dropped (or, in `C` mode, recorded in `rec`);
/// DOC comments (`///`, `//!`, `/** */`, `/*! */`) become `#[doc = ".."]` attribute tokens pushed
/// to `out`, exactly as `proc_macro2` / rustc represent them — dropping them silently changed what
/// a derive sees.
///
/// Every iteration of the loop advances `*pos` (verified-lexer rule: each step consumes), so it
/// terminates; an unterminated block comment consumes to end of input, as before.
fn skip_ws_and_comments<const C: bool>(
    chars: &[char],
    pos: &mut usize,
    out: &mut Vec<TokenTree>,
    rec: &mut Rec,
) {
    loop {
        while *pos < chars.len() && is_ws(chars[*pos]) {
            *pos += 1;
        }
        if *pos + 1 < chars.len() && chars[*pos] == '/' && chars[*pos + 1] == '/' {
            // `///x` is an outer doc unless a 4th `/` follows (`////` is a plain comment);
            // `//!x` is an inner doc.
            let start = *pos;
            let third = chars.get(*pos + 2).copied();
            let fourth = chars.get(*pos + 3).copied();
            let doc = match third {
                Some('/') if fourth != Some('/') => Some(false),
                Some('!') => Some(true),
                _ => None,
            };
            let body_start = *pos + 3;
            while *pos < chars.len() && chars[*pos] != '\n' {
                *pos += 1;
            }
            let mut end = *pos;
            if end > start && chars[end - 1] == '\r' {
                end -= 1;
            }
            if let Some(inner) = doc {
                push_doc_attr(out, inner, &chars[body_start.min(end)..end]);
                if C {
                    rec.words.push((start, end));
                    rec.words.push((start, end));
                    rec.count += if inner { 6 } else { 5 };
                    rec.after_close = false;
                }
            } else if C {
                record_comment(chars, start, end, false, rec);
            }
            continue;
        }
        if *pos + 1 < chars.len() && chars[*pos] == '/' && chars[*pos + 1] == '*' {
            let start = *pos;
            // `/** x */` outer doc (not `/**/` or `/***`); `/*! x */` inner doc.
            let third = chars.get(start + 2).copied();
            let fourth = chars.get(start + 3).copied();
            let doc = match third {
                Some('*') if fourth != Some('*') && fourth != Some('/') => Some(false),
                Some('!') => Some(true),
                _ => None,
            };
            *pos += 2;
            let mut depth = 1;
            while *pos < chars.len() && depth > 0 {
                if *pos + 1 < chars.len() && chars[*pos] == '/' && chars[*pos + 1] == '*' {
                    depth += 1;
                    *pos += 2;
                } else if *pos + 1 < chars.len() && chars[*pos] == '*' && chars[*pos + 1] == '/' {
                    depth -= 1;
                    *pos += 2;
                } else {
                    *pos += 1;
                }
            }
            match (doc, depth) {
                (Some(inner), 0) => {
                    push_doc_attr(out, inner, &chars[start + 3..*pos - 2]);
                    if C {
                        rec.words.push((start, *pos));
                        rec.words.push((start, *pos));
                        rec.count += if inner { 6 } else { 5 };
                        rec.after_close = false;
                    }
                }
                _ => {
                    if C {
                        record_comment(chars, start, *pos, true, rec);
                    }
                }
            }
            continue;
        }
        break;
    }
}

fn read_ident(chars: &[char], pos: &mut usize) -> Result<Ident, TokenizeError> {
    let start = *pos;
    // raw identifier: r#ident
    if chars.get(*pos) == Some(&'r')
        && chars.get(*pos + 1) == Some(&'#')
        && chars.get(*pos + 2).is_some_and(|c| is_ident_start(*c))
    {
        *pos += 2;
    }
    while *pos < chars.len() && is_ident_continue(chars[*pos]) {
        *pos += 1;
    }
    if *pos == start {
        return Err(TokenizeError {
            message: "expected an identifier".into(),
            pos: start,
        });
    }
    let sym = Sym::from_chars(&chars[start..*pos]);
    // build the Ident directly: `Ident::new(&sym, ..)` would copy the String just collected
    Ok(Ident {
        sym,
        span: Span::call_site(),
    })
}

/// True at a `r"..."`/`r#"..."#`/`r##"..."##` raw string's opening `r`.
fn is_raw_string_start(chars: &[char], pos: usize) -> bool {
    let mut i = pos + 1;
    while chars.get(i) == Some(&'#') {
        i += 1;
    }
    chars.get(i) == Some(&'"')
}

fn read_raw_string_literal(chars: &[char], pos: &mut usize) -> Result<Literal, TokenizeError> {
    let start = *pos;
    *pos += 1; // 'r'
    let mut hashes = 0;
    while chars.get(*pos) == Some(&'#') {
        hashes += 1;
        *pos += 1;
    }
    if chars.get(*pos) != Some(&'"') {
        return Err(TokenizeError {
            message: "malformed raw string".into(),
            pos: start,
        });
    }
    *pos += 1;
    loop {
        if *pos >= chars.len() {
            return Err(TokenizeError {
                message: "unterminated raw string".into(),
                pos: start,
            });
        }
        if chars[*pos] == '"' {
            let close_start = *pos;
            let mut i = *pos + 1;
            let mut n = 0;
            while chars.get(i) == Some(&'#') && n < hashes {
                i += 1;
                n += 1;
            }
            if n == hashes {
                *pos = i;
                let text: String = chars[start..*pos].iter().collect();
                return Ok(Literal {
                    text: text.into(),
                    span: Span::call_site(),
                });
            }
            *pos = close_start + 1;
            continue;
        }
        *pos += 1;
    }
}

fn read_string_literal(chars: &[char], pos: &mut usize) -> Result<Literal, TokenizeError> {
    let start = *pos;
    *pos += 1; // opening quote
    while *pos < chars.len() && chars[*pos] != '"' {
        if chars[*pos] == '\\' {
            *pos += 1;
        }
        *pos += 1;
    }
    if *pos >= chars.len() {
        return Err(TokenizeError {
            message: "unterminated string literal".into(),
            pos: start,
        });
    }
    *pos += 1; // closing quote
    let text: String = chars[start..*pos].iter().collect();
    Ok(Literal {
        text: text.into(),
        span: Span::call_site(),
    })
}

/// `br"..."` / `br#"..."#` / `cr#"..."#`: a raw string with a one-character prefix. Before this,
/// `br#"x"#` lexed as the ident `br`, a `#`, a string and another `#`.
/// A string literal may carry a lexical suffix (`"x"suffix`): lexically one token, rejected later by
/// the compiler. proc_macro2 keeps it in the literal; so do we.
fn with_suffix(chars: &[char], pos: &mut usize, mut lit: Literal) -> Literal {
    if *pos < chars.len() && is_ident_start(chars[*pos]) {
        let end = *pos;
        while *pos < chars.len() && is_ident_continue(chars[*pos]) {
            *pos += 1;
        }
        let mut text = String::from(lit.text.as_str());
        text.extend(&chars[end..*pos]);
        lit.text = text.into();
    }
    lit
}

fn read_prefixed_raw_string_literal(
    chars: &[char],
    pos: &mut usize,
) -> Result<Literal, TokenizeError> {
    let start = *pos;
    *pos += 1; // the `b` / `c` prefix
    read_raw_string_literal(chars, pos)?;
    Ok(with_suffix(
        chars,
        pos,
        Literal {
            text: Sym::from_chars(&chars[start..*pos]),
            span: Span::call_site(),
        },
    ))
}

fn read_byte_string_literal(chars: &[char], pos: &mut usize) -> Result<Literal, TokenizeError> {
    let start = *pos;
    *pos += 1; // 'b' / 'c'
    read_string_literal(chars, pos)?;
    Ok(with_suffix(
        chars,
        pos,
        Literal {
            text: Sym::from_chars(&chars[start..*pos]),
            span: Span::call_site(),
        },
    ))
}

/// End index (past the closing quote) of the char literal (`'a'`, `'\n'`, `'\''`) starting at
/// `start`, or `None` if this `'` is actually a lifetime punct instead (no closing `'` where a
/// char literal's would be).
fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if chars.get(i) == Some(&'\\') {
        i += 1; // the backslash
        match chars.get(i) {
            // `'\u{08}'` — braces hold 1..=6 hex digits (underscores allowed)
            Some('u') if chars.get(i + 1) == Some(&'{') => {
                let mut j = i + 2;
                while j < i + 2 + 12
                    && chars
                        .get(j)
                        .is_some_and(|c| c.is_ascii_hexdigit() || *c == '_')
                {
                    j += 1;
                }
                if chars.get(j) != Some(&'}') {
                    return None;
                }
                i = j + 1;
            }
            // `'\x41'` — `x` and two hex digits
            Some('x') => i += 3,
            Some(_) => i += 1, // `'\n'`, `'\''`, `'\\'`: the one escaped char
            None => return None,
        }
        return (chars.get(i) == Some(&'\'')).then_some(i + 1);
    }
    // one char (any, even `'`), then the closing quote
    (chars.get(i).is_some() && chars.get(i + 1) == Some(&'\'')).then_some(i + 2)
}

fn read_byte_char_literal(chars: &[char], pos: &mut usize) -> Result<Literal, TokenizeError> {
    let start = *pos;
    match char_literal_end(chars, start + 1) {
        Some(end) => {
            *pos = end;
            Ok(with_suffix(
                chars,
                pos,
                Literal {
                    text: Sym::from_chars(&chars[start..end]),
                    span: Span::call_site(),
                },
            ))
        }
        None => Err(TokenizeError {
            message: "malformed byte char literal".into(),
            pos: start,
        }),
    }
}

/// End of the identifier-shaped run starting at `i` (a suffix such as `f32`, `usize`).
fn ident_run_end(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && is_ident_continue(chars[i]) {
        i += 1;
    }
    i
}

/// The end of a number token whose digits end at `e`: an optional identifier-shaped suffix, and the
/// token must then not run straight into more identifier characters (proc_macro2's `word_break`).
fn number_tail(chars: &[char], mut e: usize) -> Option<usize> {
    if chars.get(e).is_some_and(|&c| is_ident_start(c)) {
        e = ident_run_end(chars, e);
    }
    if chars.get(e).is_some_and(|&c| is_ident_continue(c)) {
        return None;
    }
    Some(e)
}

/// proc_macro2's `float_digits`: a decimal with a `.` and/or an exponent. `None` if the text at
/// `start` (which begins with a digit) is not a float — `1`, `1..2`, `1.foo()`, `0x1e5`.
fn float_digits(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    let (mut has_dot, mut has_exp) = (false, false);
    while let Some(&ch) = chars.get(i) {
        match ch {
            '0'..='9' | '_' => i += 1,
            '.' => {
                if has_dot {
                    break;
                }
                i += 1;
                // `1..2` is a range and `1.foo` a method call / field: not a float
                if chars.get(i).is_some_and(|&c| c == '.' || is_ident_start(c)) {
                    return None;
                }
                has_dot = true;
            }
            'e' | 'E' => {
                i += 1;
                has_exp = true;
                break;
            }
            _ => break,
        }
    }
    if !(has_dot || has_exp) {
        return None;
    }
    if has_exp {
        // without a usable exponent value, a float with a fraction ends just before the `e`
        let before_exp = has_dot.then_some(i - 1);
        let (mut has_sign, mut has_value) = (false, false);
        while let Some(&ch) = chars.get(i) {
            match ch {
                '+' | '-' => {
                    if has_value {
                        break;
                    }
                    if has_sign {
                        return before_exp;
                    }
                    i += 1;
                    has_sign = true;
                }
                '0'..='9' => {
                    i += 1;
                    has_value = true;
                }
                '_' => i += 1,
                _ => break,
            }
        }
        if !has_value {
            return before_exp;
        }
    }
    Some(i)
}

/// proc_macro2's `digits`: an integer in base 2/8/10/16 (`0b`/`0o`/`0x` prefix), `_` separators
/// allowed. Returns the end of the digits; `None` for no digits or a digit out of range for the base.
fn int_digits(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    let mut base = 10u32;
    if chars[i] == '0' {
        match chars.get(i + 1) {
            Some('x') => base = 16,
            Some('o') => base = 8,
            Some('b') => base = 2,
            _ => {}
        }
        if base != 10 {
            i += 2;
        }
    }
    let mut empty = true;
    while let Some(&ch) = chars.get(i) {
        match ch {
            '0'..='9' => {
                if ch as u32 - '0' as u32 >= base {
                    return None;
                }
                empty = false;
            }
            'a'..='f' | 'A'..='F' => {
                if 10 + (ch.to_ascii_lowercase() as u32 - 'a' as u32) >= base {
                    break;
                }
                empty = false;
            }
            '_' => {
                if empty && base == 10 {
                    return None;
                }
            }
            _ => break,
        }
        i += 1;
    }
    if empty {
        None
    } else {
        Some(i)
    }
}

/// An integer or float literal, with an optional trailing type suffix (`1_000_i64`, `1.5f32`,
/// `0x1F`, `0b101`, `0o17`, `1e10`) — lexed exactly as `proc_macro2` does, quirks included:
/// `x.0.1` is the single float literal `0.1`, `1.` is a float, `1..2` and `1.foo()` are not.
fn read_number_literal(chars: &[char], pos: &mut usize) -> Result<Literal, TokenizeError> {
    let start = *pos;
    // Only `.` / `e` / `E` straight after the leading digit run can make a float; everything else
    // (the common case) goes directly to the integer path.
    let mut j = start + 1;
    while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '_') {
        j += 1;
    }
    let float_end = if matches!(chars.get(j), Some('.' | 'e' | 'E')) {
        float_digits(chars, start).and_then(|e| number_tail(chars, e))
    } else {
        None
    };
    let end = float_end
        .or_else(|| int_digits(chars, start).and_then(|e| number_tail(chars, e)))
        .ok_or_else(|| TokenizeError {
            message: "malformed number literal".into(),
            pos: start,
        })?;
    *pos = end;
    Ok(Literal {
        text: Sym::from_chars(&chars[start..end]),
        span: Span::call_site(),
    })
}

/// Punctuation characters Rust's tokenizer recognizes as standalone `Punct` tokens (never part
/// of an identifier/literal/delimiter). `'` is included: a lifetime tick is a `Punct` too, and a
/// punct directly before one is `Joint` (`&'a`).
fn is_punct_char(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '^'
            | '!'
            | '&'
            | '|'
            | '<'
            | '>'
            | '='
            | '@'
            | '.'
            | ','
            | ';'
            | ':'
            | '#'
            | '$'
            | '?'
            | '~'
            | '\''
    )
}

/// `Spacing` of a punct whose next source character is at `at`: `Joint` iff that character is
/// itself punctuation — `proc_macro2`'s rule, applied to whatever follows with no whitespace
/// between, so it also fires across token kinds (`&'a`, `,'x'`, `=$x`) — except that a `/` that
/// starts a comment (`//`, `/*`) is not punctuation.
#[inline]
fn spacing_before(chars: &[char], at: usize) -> Spacing {
    match chars.get(at) {
        Some('/') => {
            if matches!(chars.get(at + 1), Some('/' | '*')) {
                Spacing::Alone
            } else {
                Spacing::Joint
            }
        }
        Some(&n) if is_punct_char(n) => Spacing::Joint,
        _ => Spacing::Alone,
    }
}

/// proc_macro2's stand-in for a macro-expansion error, lexed as a single literal.
const ERROR_LITERAL: &str = "(/*ERROR*/)";

fn starts_with_at(chars: &[char], at: usize, s: &str) -> bool {
    s.chars()
        .enumerate()
        .all(|(k, c)| chars.get(at + k) == Some(&c))
}

fn tokenize_until<const C: bool>(
    chars: &[char],
    pos: &mut usize,
    closer: Option<char>,
    rec: &mut Rec,
) -> Result<TokenStream, TokenizeError> {
    let mut out: Vec<TokenTree> = Vec::new();
    if C {
        rec.after_close = false;
    }
    loop {
        skip_ws_and_comments::<C>(chars, pos, &mut out, rec);
        if *pos >= chars.len() {
            if closer.is_some() {
                return Err(TokenizeError {
                    message: "unexpected end of input inside a delimited group".into(),
                    pos: *pos,
                });
            }
            break;
        }
        let c = chars[*pos];
        if let Some(close) = closer {
            if c == close {
                *pos += 1;
                break;
            }
        }
        if C {
            rec.count += 1;
            rec.after_close = false;
        }
        let (tok_start, tok_first) = (*pos, out.len());
        match c {
            '(' if starts_with_at(chars, *pos, ERROR_LITERAL) => {
                *pos += ERROR_LITERAL.len();
                out.push(TokenTree::Literal(Literal {
                    text: ERROR_LITERAL.to_string().into(),
                    span: Span::call_site(),
                }));
            }
            '(' | '{' | '[' => {
                let (close, delim) = match c {
                    '(' => (')', Delimiter::Parenthesis),
                    '{' => ('}', Delimiter::Brace),
                    '[' => (']', Delimiter::Bracket),
                    _ => unreachable!(),
                };
                if rec.nest >= MAX_GROUP_DEPTH {
                    return Err(TokenizeError {
                        message: format!(
                            "delimiters nested deeper than the limit of {MAX_GROUP_DEPTH}"
                        ),
                        pos: *pos,
                    });
                }
                *pos += 1;
                rec.nest += 1;
                if C {
                    rec.depth += 1;
                }
                let inner = tokenize_until::<C>(chars, pos, Some(close), rec)?;
                rec.nest -= 1;
                if C {
                    rec.depth -= 1;
                    rec.after_close = true;
                }
                out.push(TokenTree::Group(Group::new(delim, inner)));
            }
            ')' | '}' | ']' => {
                return Err(TokenizeError {
                    message: format!("unexpected closing delimiter `{c}`"),
                    pos: *pos,
                });
            }
            '"' => {
                let lit = read_string_literal(chars, pos)?;
                out.push(TokenTree::Literal(with_suffix(chars, pos, lit)));
            }
            'r' if is_raw_string_start(chars, *pos) => {
                let lit = read_raw_string_literal(chars, pos)?;
                out.push(TokenTree::Literal(with_suffix(chars, pos, lit)));
            }
            'b' | 'c'
                if chars.get(*pos + 1) == Some(&'r') && is_raw_string_start(chars, *pos + 1) =>
            {
                out.push(TokenTree::Literal(read_prefixed_raw_string_literal(
                    chars, pos,
                )?));
            }
            // `c"..."` (C string literal): same shape as `b"..."` with a different prefix
            'c' if chars.get(*pos + 1) == Some(&'"') => {
                out.push(TokenTree::Literal(read_byte_string_literal(chars, pos)?));
            }
            'b' if chars.get(*pos + 1) == Some(&'"') => {
                out.push(TokenTree::Literal(read_byte_string_literal(chars, pos)?));
            }
            'b' if chars.get(*pos + 1) == Some(&'\'') => {
                out.push(TokenTree::Literal(read_byte_char_literal(chars, pos)?));
            }
            '\'' => {
                if let Some(end) = char_literal_end(chars, *pos) {
                    let lit = Literal {
                        text: Sym::from_chars(&chars[*pos..end]),
                        span: Span::call_site(),
                    };
                    *pos = end;
                    out.push(TokenTree::Literal(with_suffix(chars, pos, lit)));
                } else {
                    // lifetime: emit the `'` as a Joint punct, then the name as an Ident —
                    // matching how proc_macro2/rustc represent `'a`.
                    *pos += 1;
                    out.push(TokenTree::Punct(Punct {
                        ch: '\'',
                        spacing: Spacing::Joint,
                        span: Span::call_site(),
                    }));
                    out.push(TokenTree::Ident(read_ident(chars, pos)?));
                    if C {
                        rec.count += 1;
                    }
                }
            }
            c if is_ident_start(c) => out.push(TokenTree::Ident(read_ident(chars, pos)?)),
            c if c.is_ascii_digit() => {
                out.push(TokenTree::Literal(read_number_literal(chars, pos)?))
            }
            c if is_punct_char(c) => {
                *pos += 1;
                out.push(TokenTree::Punct(Punct {
                    ch: c,
                    spacing: spacing_before(chars, *pos),
                    span: Span::call_site(),
                }));
            }
            _ => {
                return Err(TokenizeError {
                    message: format!("unrecognized character `{c}`"),
                    pos: *pos,
                })
            }
        }
        if C {
            // a lifetime's word is its name; the `'` is a separate punct
            let start = if c == '\'' && matches!(out.get(tok_first), Some(TokenTree::Punct(_))) {
                tok_start + 1
            } else {
                tok_start
            };
            for t in &out[tok_first..] {
                if matches!(t, TokenTree::Ident(_) | TokenTree::Literal(_)) {
                    rec.words.push((start, *pos));
                }
            }
        }
    }
    Ok(TokenStream { tokens: out })
}

// ── render — TokenStream → readable Rust source text, the inverse of `tokenize` ──
//
// Correctness only needs one rule: no space after a `Joint` punct (so `::`/`->`/`'a` stay
// adjacent), a space everywhere else. Extra whitespace never changes Rust's meaning, so this
// stays deliberately generous rather than trying to reproduce idiomatic formatting — a standalone
// codegen tool runs the result through `rustfmt` afterward for that.

/// Render a `TokenStream` back to Rust source text — what a standalone codegen tool writes to
/// disk after calling the same derive logic `wirefold_derive`'s proc-macros use.
pub fn render(ts: &TokenStream) -> String {
    let mut out = String::new();
    render_into(ts, &mut out);
    out
}

fn render_into(ts: &TokenStream, out: &mut String) {
    let mut prev_joint = false;
    for (i, tt) in ts.iter().enumerate() {
        if i > 0 && !prev_joint {
            out.push(' ');
        }
        prev_joint = false;
        match tt {
            TokenTree::Ident(id) => out.push_str(&id.sym),
            TokenTree::Literal(lit) => out.push_str(&lit.text),
            TokenTree::Punct(p) => {
                out.push(p.ch);
                prev_joint = p.spacing == Spacing::Joint;
            }
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter {
                    Delimiter::Parenthesis => ('(', ')'),
                    Delimiter::Brace => ('{', '}'),
                    Delimiter::Bracket => ('[', ']'),
                    Delimiter::None => ('\0', '\0'),
                };
                if open != '\0' {
                    out.push(open);
                }
                render_into(&g.stream, out);
                if close != '\0' {
                    out.push(close);
                }
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    // ── tokenize ──

    fn tt_texts(ts: &TokenStream) -> Vec<String> {
        ts.iter()
            .map(|t| match t {
                TokenTree::Ident(i) => i.sym.to_string(),
                TokenTree::Literal(l) => l.text.to_string(),
                TokenTree::Punct(p) => p.ch.to_string(),
                TokenTree::Group(g) => {
                    format!("<group {:?} of {} tokens>", g.delimiter, g.stream.len())
                }
            })
            .collect()
    }

    #[test]
    fn tokenize_plain_idents_and_puncts() {
        let ts = tokenize("foo bar_baz + 1").unwrap();
        assert_eq!(tt_texts(&ts), vec!["foo", "bar_baz", "+", "1"]);
    }

    #[test]
    fn tokenize_compound_punct_gets_joint_spacing() {
        let ts = tokenize("a :: b -> c").unwrap();
        // `::` -> two Joint(':') tokens; `->` -> Joint('-') then Alone('>')
        let puncts: Vec<(char, Spacing)> = ts
            .iter()
            .filter_map(|t| match t {
                TokenTree::Punct(p) => Some((p.ch, p.spacing)),
                _ => None,
            })
            .collect();
        assert_eq!(
            puncts,
            vec![
                (':', Spacing::Joint),
                (':', Spacing::Alone),
                ('-', Spacing::Joint),
                ('>', Spacing::Alone)
            ]
        );
    }

    #[test]
    fn tokenize_string_and_number_literals() {
        let ts = tokenize(r#""hello world" 42 3.5 0x1F i64"#).unwrap();
        let texts = tt_texts(&ts);
        assert_eq!(texts, vec!["\"hello world\"", "42", "3.5", "0x1F", "i64"]);
    }

    #[test]
    fn tokenize_raw_string_literal() {
        let ts = tokenize(r##"r#"a "quoted" thing"#"##).unwrap();
        assert_eq!(ts.len(), 1);
        let TokenTree::Literal(lit) = ts.get(0).unwrap() else {
            panic!("expected a literal")
        };
        assert!(lit.text.starts_with("r#\""));
    }

    #[test]
    fn tokenize_char_and_lifetime_disambiguation() {
        let ts = tokenize("'a' 'lifetime").unwrap();
        // 'a' is a char literal (one token); 'lifetime is a Joint `'` punct + an Ident
        assert_eq!(ts.len(), 3);
        assert!(matches!(ts.get(0), Some(TokenTree::Literal(l)) if l.text == "'a'"));
        assert!(
            matches!(ts.get(1), Some(TokenTree::Punct(p)) if p.ch == '\'' && p.spacing == Spacing::Joint)
        );
        assert!(matches!(ts.get(2), Some(TokenTree::Ident(i)) if i.sym == "lifetime"));
    }

    #[test]
    fn tokenize_skips_line_and_block_comments() {
        let ts = tokenize("a // a line comment\n b /* a block comment */ c").unwrap();
        assert_eq!(tt_texts(&ts), vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenize_angle_brackets_are_plain_puncts_not_a_delimiter() {
        let ts = tokenize("Vec<Option<String>>").unwrap();
        assert_eq!(
            tt_texts(&ts),
            vec!["Vec", "<", "Option", "<", "String", ">", ">"]
        );
    }

    #[test]
    fn tokenize_braces_produce_a_group() {
        let ts = tokenize("{ a: String, b: i64 }").unwrap();
        assert_eq!(ts.len(), 1);
        let TokenTree::Group(g) = ts.get(0).unwrap() else {
            panic!("expected a group")
        };
        assert_eq!(g.delimiter, Delimiter::Brace);
        assert_eq!(
            tt_texts(&g.stream),
            vec!["a", ":", "String", ",", "b", ":", "i64"]
        );
    }

    #[test]
    fn tokenize_then_parse_a_real_struct_definition() {
        let src = r#"
            #[derive(Debug, Clone)]
            pub struct Widget {
                pub name: String,
                pub count: i64,
                pub tag: Option<String>,
            }
        "#;
        let ts = tokenize(src).unwrap();
        let mut cursor = ts.cursor();
        let di = DeriveInput::from_tokens(&mut cursor).expect("parses as a DeriveInput");
        assert_eq!(di.ident.sym, "Widget");
        let Data::Struct(Fields::Named(fields)) = &di.data else {
            panic!("expected named struct fields")
        };
        let names: Vec<&str> = fields
            .iter()
            .map(|f| f.ident.as_ref().unwrap().sym.as_str())
            .collect();
        assert_eq!(names, vec!["name", "count", "tag"]);
    }

    #[test]
    fn tokenize_then_parse_a_real_enum_definition() {
        let src = r#"
            pub enum Shape {
                Unit,
                Single(String),
                Named { x: i64, y: i64 },
            }
        "#;
        let ts = tokenize(src).unwrap();
        let mut cursor = ts.cursor();
        let di = DeriveInput::from_tokens(&mut cursor).expect("parses as a DeriveInput");
        assert_eq!(di.ident.sym, "Shape");
        let Data::Enum(variants) = &di.data else {
            panic!("expected an enum")
        };
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].ident.sym, "Unit");
        assert_eq!(variants[1].ident.sym, "Single");
        assert_eq!(variants[2].ident.sym, "Named");
    }

    #[test]
    fn enum_with_explicit_discriminants_does_not_hang() {
        // Regression: a fieldless variant's `= N` discriminant used to leave the cursor stuck
        // (Fields::from_tokens consumes nothing for a unit variant, and `punct(',')` doesn't
        // match `=` either, so nothing advanced and the parse loop spun forever).
        let src = r#"
            pub enum Status {
                Open = 0,
                Finished,
                Canceled,
            }
        "#;
        let ts = tokenize(src).unwrap();
        let mut cursor = ts.cursor();
        let di = DeriveInput::from_tokens(&mut cursor).expect("parses as a DeriveInput");
        let Data::Enum(variants) = &di.data else {
            panic!("expected an enum")
        };
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].ident.sym, "Open");
        assert_eq!(
            variants[0].discriminant.as_ref().map(|l| l.text.as_str()),
            Some("0")
        );
        assert_eq!(variants[1].ident.sym, "Finished");
        assert!(variants[1].discriminant.is_none());
        assert_eq!(variants[2].ident.sym, "Canceled");
    }

    // ── render ──

    #[test]
    fn render_simple_struct_is_readable_and_reparses_identically() {
        let src = "pub struct Widget { pub name : String , pub count : i64 }";
        let ts = tokenize(src).unwrap();
        let rendered = render(&ts);
        let reparsed = tokenize(&rendered).unwrap();
        assert_eq!(ts, reparsed, "tokenize(render(ts)) must equal ts");
    }

    #[test]
    fn render_preserves_compound_operators() {
        let ts = tokenize("a :: b -> c").unwrap();
        let rendered = render(&ts);
        assert!(rendered.contains("::"), "rendered: {rendered}");
        assert!(rendered.contains("->"), "rendered: {rendered}");
        // round trip through tokenize again to confirm it's still the SAME compound ops, not
        // just the substring accidentally present
        assert_eq!(tokenize(&rendered).unwrap(), ts);
    }

    #[test]
    fn render_preserves_lifetimes_and_groups() {
        let src = "impl<'a> Foo<'a> { fn bar(x: Vec<i64>) -> Option<&'a str> { None } }";
        let ts = tokenize(src).unwrap();
        let rendered = render(&ts);
        let reparsed = tokenize(&rendered).unwrap();
        assert_eq!(
            ts, reparsed,
            "tokenize(render(ts)) must equal ts for lifetimes/generics/groups"
        );
    }

    #[test]
    fn render_string_and_number_literals_verbatim() {
        let ts = tokenize(r#""hello" 42 3.5"#).unwrap();
        let rendered = render(&ts);
        assert_eq!(rendered.trim(), "\"hello\" 42 3.5");
    }

    #[test]
    fn token_stream_push_and_get() {
        let mut ts = TokenStream::new();
        ts.push(TokenTree::Ident(Ident::new("foo", Span::call_site())));
        ts.push(TokenTree::Punct(Punct::new(';', Spacing::Alone)));
        assert_eq!(ts.len(), 2);
        assert!(matches!(ts.get(0), Some(TokenTree::Ident(_))));
        assert!(matches!(ts.get(1), Some(TokenTree::Punct(_))));
    }

    #[test]
    fn cursor_advances() {
        let mut ts = TokenStream::new();
        ts.push(TokenTree::Ident(Ident::new("a", Span::call_site())));
        ts.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
        ts.push(TokenTree::Ident(Ident::new("b", Span::call_site())));
        let mut c = ts.cursor();
        assert_eq!(c.ident(), Some("a"));
        assert!(c.punct(','));
        assert_eq!(c.ident(), Some("b"));
        assert!(c.at_end());
    }

    #[test]
    fn from_tokens_ident() {
        let mut ts = TokenStream::new();
        ts.push(TokenTree::Ident(Ident::new(
            "test_struct",
            Span::call_site(),
        )));
        let mut c = ts.cursor();
        let id: Ident = FromTokens::from_tokens(&mut c).unwrap();
        assert_eq!(id.sym, "test_struct");
    }

    #[test]
    fn to_tokens_round_trip() {
        let ident = Ident::new("foo", Span::call_site());
        let mut ts = TokenStream::new();
        ident.to_tokens(&mut ts);
        assert_eq!(ts.len(), 1);
        let mut c = ts.cursor();
        let back: Ident = FromTokens::from_tokens(&mut c).unwrap();
        assert_eq!(back.sym, "foo");
    }

    #[test]
    fn derive_input_parse_struct_with_fields() {
        // struct Foo { a: String, b: Vec<i64> }
        let mut ts = TokenStream::new();
        ts.push(TokenTree::Ident(Ident::new("struct", Span::call_site())));
        ts.push(TokenTree::Ident(Ident::new("Foo", Span::call_site())));
        let mut body = TokenStream::new();
        body.push(TokenTree::Ident(Ident::new("a", Span::call_site())));
        body.push(TokenTree::Punct(Punct::new(':', Spacing::Alone)));
        body.push(TokenTree::Ident(Ident::new("String", Span::call_site())));
        body.push(TokenTree::Punct(Punct::new(',', Spacing::Alone)));
        body.push(TokenTree::Ident(Ident::new("b", Span::call_site())));
        body.push(TokenTree::Punct(Punct::new(':', Spacing::Alone)));
        body.push(TokenTree::Ident(Ident::new("Vec", Span::call_site())));
        body.push(TokenTree::Punct(Punct::new('<', Spacing::Alone)));
        body.push(TokenTree::Ident(Ident::new("i64", Span::call_site())));
        body.push(TokenTree::Punct(Punct::new('>', Spacing::Alone)));
        ts.push(TokenTree::Group(Group::new(Delimiter::Brace, body)));

        let mut c = ts.cursor();
        let di: DeriveInput = FromTokens::from_tokens(&mut c).unwrap();
        assert_eq!(di.ident.sym, "Foo");
        let Data::Struct(Fields::Named(fields)) = &di.data else {
            panic!("expected named struct fields");
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].ident.as_ref().unwrap().sym, "a");
        assert!(is_bare_ident(&fields[0].ty, "String"));
        assert_eq!(fields[1].ident.as_ref().unwrap().sym, "b");
        let inner = vec_inner(&fields[1].ty).expect("Vec<..> unwrap");
        assert!(is_bare_ident(&inner, "i64"));
    }

    #[test]
    fn generic_inner_handles_nesting() {
        // Vec < Vec < i64 > >
        let mut ty = TokenStream::new();
        ty.push(TokenTree::Ident(Ident::new("Vec", Span::call_site())));
        ty.push(TokenTree::Punct(Punct::new('<', Spacing::Alone)));
        ty.push(TokenTree::Ident(Ident::new("Vec", Span::call_site())));
        ty.push(TokenTree::Punct(Punct::new('<', Spacing::Alone)));
        ty.push(TokenTree::Ident(Ident::new("i64", Span::call_site())));
        ty.push(TokenTree::Punct(Punct::new('>', Spacing::Alone)));
        ty.push(TokenTree::Punct(Punct::new('>', Spacing::Alone)));

        let outer = vec_inner(&ty).expect("outer Vec unwrap");
        let inner = vec_inner(&outer).expect("inner Vec unwrap");
        assert!(is_bare_ident(&inner, "i64"));
    }

    #[test]
    fn quote_tokens_plain_passthrough() {
        // The exact shape that used to hit the macro recursion limit.
        let ts = quote_tokens! { struct Foo ; };
        assert_eq!(ts.len(), 3);
    }

    #[test]
    fn quote_tokens_interpolation_and_groups() {
        let name = Ident::new("Bar", Span::call_site());
        let n: usize = 3;
        let ts = quote_tokens! {
            pub fn #name ( ) -> usize { #n }
        };
        // pub fn Bar ( ) -> usize { 3 }
        let toks: Vec<_> = ts.iter().collect();
        assert!(matches!(&toks[2], TokenTree::Ident(i) if i.sym == "Bar"));
        let TokenTree::Group(body) = toks.last().unwrap() else {
            panic!("expected trailing brace group");
        };
        assert!(matches!(body.stream.get(0), Some(TokenTree::Literal(l)) if l.text == "3_usize"));
    }

    #[test]
    fn quote_tokens_join_helpers_and_compound_ops() {
        let idents = [
            Ident::new("a", Span::call_site()),
            Ident::new("b", Span::call_site()),
        ];
        let joined = join_comma(idents.iter());
        let ts = quote_tokens! { fn f ( #joined ) -> usize { 0 } };
        // Compound operator `->` must have survived the fixup pass.
        let toks: Vec<_> = ts.iter().collect();
        let arrow_pos = toks
            .iter()
            .position(|t| matches!(t, TokenTree::Punct(p) if p.ch == '-'))
            .unwrap();
        let TokenTree::Punct(dash) = toks[arrow_pos] else {
            unreachable!()
        };
        assert_eq!(dash.spacing, Spacing::Joint);
    }

    #[test]
    fn triple_colon_separator_then_absolute_path() {
        // A struct field's separator `:` immediately followed by an absolute path's `::` —
        // exactly the `#cid: ::wirefold::codec::web::Ragged<'a>` shape that broke real
        // multi-field `#[derive(Collapse)]` structs before this had a dedicated test.
        let cid = Ident::new("__c0", Span::call_site());
        let ts = quote_tokens! { #cid : ::wirefold::codec::web::Ragged };
        let puncts: Vec<&Punct> = ts
            .iter()
            .filter_map(|t| match t {
                TokenTree::Punct(p) => Some(p),
                _ => None,
            })
            .collect();
        // : :: :: :: :: -> lone separator (Alone), then FOUR `::` pairs (wirefold/codec/web/Ragged).
        assert_eq!(
            puncts.iter().map(|p| p.spacing).collect::<Vec<_>>(),
            vec![
                Spacing::Alone,
                Spacing::Joint,
                Spacing::Alone,
                Spacing::Joint,
                Spacing::Alone,
                Spacing::Joint,
                Spacing::Alone,
                Spacing::Joint,
                Spacing::Alone,
            ]
        );
    }

    #[test]
    fn error_attribute_single_string_arg() {
        // #[error("bad thing: {0}")]
        let mut ts = TokenStream::new();
        ts.push(TokenTree::Punct(Punct::new('#', Spacing::Alone)));
        let mut inner = TokenStream::new();
        inner.push(TokenTree::Ident(Ident::new("error", Span::call_site())));
        let mut args = TokenStream::new();
        args.push(TokenTree::Literal(Literal::string(
            "bad thing: {0}",
            Span::call_site(),
        )));
        inner.push(TokenTree::Group(Group::new(Delimiter::Parenthesis, args)));
        ts.push(TokenTree::Group(Group::new(Delimiter::Bracket, inner)));

        let mut c = ts.cursor();
        let attr = Attribute::from_tokens(&mut c).unwrap();
        assert_eq!(attr.path, "error");
        let lit = attr.single_string_arg().expect("string arg");
        assert_eq!(lit.string_value().unwrap(), "bad thing: {0}");
    }
}
