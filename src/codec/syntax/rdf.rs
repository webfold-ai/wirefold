//! The two RDF ontology surfaces — Turtle and RDF/XML — parsers and renderers. Entry points
//! ([`parse_turtle`], [`parse_rdfxml`], [`render_turtle`], [`render_rdfxml`]) are re-exported by
//! [`super`]. The honest scope of each is stated in the headers below.

use super::surfaces::{depth_error, MAX_NESTING_DEPTH};
use super::{Atom, SyntaxError, SyntaxTerm};

// ═══════════════════════════════════════════════════════════════════════════
// Turtle (RDF 1.1 Turtle) and RDF/XML — OWL ontology ingestion surfaces.
// ═══════════════════════════════════════════════════════════════════════════
//
// Turtle and RDF/XML are the two wire formats SEPA, SAREF, OEO, Brick, OWL
// ontologies, and every published smart-grid/cim/rdf export actually ship in.
// A single `SyntaxTerm` tree holds either, walked the same way by the
// ontology-store OWL lifter. Scope (honest): supports the subset that every
// real ontology actually uses — `@prefix` declarations, IRIs (`<...>` and
// prefixed), blank nodes (`_:b`), string literals with `^^xsd:` and `@lang`
// tags, the `a` shorthand for `rdf:type`, `;` for repeated predicates on the
// same subject, `,` for multiple objects on the same predicate, `.` for
// statement terminators. Anything more exotic (nested blank-node property
// lists `[ ... ]`, RDF collections `( ... )`, reified statements) fails
// loudly — better than a parser that silently mis-lifts an ontology.

/// Turtle parser. The lifted shape:
/// `Bu { subject_iri → Bu { predicate_iri → Fourth([object_term, ...]) } }`
/// where each `object_term` is either `B(Str(iri))` for a resource reference
/// or `B(Str(literal))` for a plain string literal (typed literals carry the
/// `^^xsd:...` suffix in the string — the ontology walker can split it).
struct TurtleParser<'a> {
    s: &'a [u8],
    pos: usize,
    /// `prefix → full_iri` — accumulated `@prefix` declarations.
    prefixes: Vec<(String, String)>,
    /// `@base <http://...>` declaration, if any.
    base: String,
}

impl<'a> TurtleParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            s: input.as_bytes(),
            pos: 0,
            prefixes: Vec::new(),
            base: String::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                self.pos += 1;
            } else if b == b'#' {
                // comment to end of line
                while let Some(c) = self.peek() {
                    if c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn skip_one_ws(&mut self) {
        // skip exactly one whitespace character (Turtle allows single
        // whitespace between tokens but not always comments — handled in ws()).
        if let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                self.pos += 1;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), SyntaxError> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(SyntaxError::new(format!(
                "expected `{}`, found `{}`",
                c as char,
                self.peek().map_or('?', |b| b as char)
            )))
        }
    }

    fn take(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn at_end(&mut self) -> bool {
        self.ws();
        self.peek().is_none()
    }

    fn _at_dot(&mut self) -> bool {
        self.ws();
        self.peek() == Some(b'.')
    }

    /// Read a string literal `"..."` with `\"`, `\\`, `\n`, `\t`, `\r`, `\uXXXX`, `\UXXXXXXXX` escapes.
    fn read_string(&mut self) -> Result<String, SyntaxError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = self
                .bump()
                .ok_or_else(|| SyntaxError::new("unterminated string literal"))?;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self
                        .bump()
                        .ok_or_else(|| SyntaxError::new("dangling escape"))?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'u' => out.push(self.read_hex(4)?),
                        b'U' => out.push(self.read_hex(8)?),
                        other => {
                            return Err(SyntaxError::new(format!(
                                "unknown escape `\\{}`",
                                other as char
                            )))
                        }
                    }
                }
                other => out.push(other as char),
            }
        }
    }

    fn read_hex(&mut self, n: usize) -> Result<char, SyntaxError> {
        let mut val = 0u32;
        for _ in 0..n {
            let b = self
                .bump()
                .ok_or_else(|| SyntaxError::new("short hex escape"))?;
            val = val * 16
                + match b {
                    b'0'..=b'9' => (b - b'0') as u32,
                    b'a'..=b'f' => (b - b'a' + 10) as u32,
                    b'A'..=b'F' => (b - b'A' + 10) as u32,
                    _ => return Err(SyntaxError::new("non-hex char in \\uXXXX")),
                };
        }
        char::from_u32(val).ok_or_else(|| SyntaxError::new("invalid unicode escape"))
    }

    /// Read an IRI in `<...>` form. Returns the inner text (raw — caller
    /// applies URI-relative resolution against `@base` if needed).
    fn read_iri(&mut self) -> Result<String, SyntaxError> {
        self.expect(b'<')?;
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'>' {
                let s = std::str::from_utf8(&self.s[start..self.pos])
                    .map_err(|_| SyntaxError::new("non-UTF8 in IRI"))?
                    .to_string();
                self.pos += 1;
                return Ok(s);
            }
            if b == b'<' {
                return Err(SyntaxError::new("`<` inside IRI"));
            }
            self.pos += 1;
        }
        Err(SyntaxError::new("unterminated IRI"))
    }

    /// Read a prefixed name (`prefix:local`) or the `a` shorthand. Returns
    /// the full IRI by expanding against the `prefixes` table.
    fn read_prefixed(&mut self) -> Result<String, SyntaxError> {
        let start = self.pos;
        // A prefixed name has the shape `prefix:localname`. We read up to
        // the FIRST non-name character (a name character is alphanumeric,
        // `_`, `-`, `.`, or the separating `:`). After this, the local
        // part is whatever followed the first `:`.
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b':' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let raw = std::str::from_utf8(&self.s[start..self.pos])
            .map_err(|_| SyntaxError::new("non-UTF8 in prefixed name"))?
            .to_string();
        if raw == "a" {
            return Ok("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string());
        }
        let (prefix, local) = match raw.split_once(':') {
            Some((p, l)) => (p, l),
            None => {
                return Err(SyntaxError::new(format!(
                    "bare token `{raw}` is not a valid prefixed name"
                )))
            }
        };
        for (p, iri) in &self.prefixes {
            if p == prefix {
                return Ok(format!("{iri}{local}"));
            }
        }
        Err(SyntaxError::new(format!(
            "unknown prefix `{prefix}` in `{raw}`"
        )))
    }

    /// Read a subject/object — IRI, blank node, prefixed name, or literal.
    fn read_term(&mut self) -> Result<(String, bool), SyntaxError> {
        self.ws();
        match self.peek() {
            Some(b'<') => Ok((self.read_iri()?, false)),
            Some(b'"') => {
                // string literal: `"..."` optionally followed by `^^<type>` or `@lang`
                let s = self.read_string()?;
                let mut full = format!("\"{s}\"");
                self.skip_one_ws();
                if self.take(b'^') {
                    self.skip_one_ws();
                    if self.take(b'^') {
                        self.ws();
                        let dtype = if self.peek() == Some(b'<') {
                            self.read_iri()?
                        } else {
                            self.read_prefixed()?
                        };
                        full.push_str("^^");
                        full.push_str(&dtype);
                    } else {
                        return Err(SyntaxError::new("expected `^^` for typed literal"));
                    }
                } else if self.peek() == Some(b'@') {
                    self.pos += 1;
                    let lang_start = self.pos;
                    while let Some(b) = self.peek() {
                        if b.is_ascii_alphanumeric() || b == b'-' {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let lang = std::str::from_utf8(&self.s[lang_start..self.pos])
                        .unwrap_or("")
                        .to_string();
                    full.push('@');
                    full.push_str(&lang);
                }
                Ok((full, true))
            }
            Some(b'_') => {
                // blank node label — `_:` followed by alphanumeric / `-` / `_`
                self.expect(b'_')?;
                self.expect(b':')?;
                let id_start = self.pos;
                while let Some(b) = self.peek() {
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let id = std::str::from_utf8(&self.s[id_start..self.pos])
                    .map_err(|_| SyntaxError::new("non-UTF8 in blank node label"))?
                    .to_string();
                Ok((format!("_:{id}"), false))
            }
            Some(b'a') => {
                // check it's not part of a longer token
                let next = self.s.get(self.pos + 1).copied();
                if next.is_none_or(|c| {
                    !c.is_ascii_alphanumeric() && c != b'_' && c != b'-' && c != b'.'
                }) {
                    self.pos += 1;
                    Ok((
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string(),
                        false,
                    ))
                } else {
                    Ok((self.read_prefixed()?, false))
                }
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => Ok((self.read_prefixed()?, false)),
            Some(b) if b.is_ascii_digit() || b == b'+' || b == b'-' => {
                // bare numeric literal: `42`, `-3.14`, `+1e10`
                let start = self.pos;
                if b == b'+' || b == b'-' {
                    self.pos += 1;
                }
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit()
                        || c == b'.'
                        || c == b'e'
                        || c == b'E'
                        || c == b'+'
                        || c == b'-'
                    {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let n = std::str::from_utf8(&self.s[start..self.pos])
                    .unwrap_or("")
                    .to_string();
                Ok((n, true))
            }
            Some(b) => Err(SyntaxError::new(format!(
                "unexpected `{}` in term",
                b as char
            ))),
            None => Err(SyntaxError::new("expected term, found EOF")),
        }
    }

    /// Read one triple-statement: subject, then zero or more (predicate, object-list),
    /// terminated by `.`. Multi-predicates via `;`, multi-objects via `,`.
    fn parse_statement(
        &mut self,
        out: &mut Vec<(String, Vec<(String, String)>)>,
    ) -> Result<(), SyntaxError> {
        self.ws();
        let (subject, _is_lit) = self.read_term()?;
        let mut triples: Vec<(String, String)> = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(b'.') {
                self.pos += 1;
                break;
            }
            if self.at_end() {
                break;
            }
            // predicate
            let predicate = match self.peek() {
                Some(b'<') => self.read_iri()?,
                Some(b'a') => {
                    let next = self.s.get(self.pos + 1).copied();
                    if next.is_none_or(|c| {
                        !c.is_ascii_alphanumeric() && c != b'_' && c != b'-' && c != b'.'
                    }) {
                        self.pos += 1;
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string()
                    } else {
                        self.read_prefixed()?
                    }
                }
                Some(b) if b.is_ascii_alphabetic() || b == b'_' => self.read_prefixed()?,
                Some(b) => {
                    return Err(SyntaxError::new(format!(
                        "unexpected `{}` after subject",
                        b as char
                    )))
                }
                None => return Err(SyntaxError::new("expected predicate, found EOF")),
            };
            // one or more objects
            loop {
                let (obj, _is_lit) = self.read_term()?;
                triples.push((predicate.clone(), obj));
                self.ws();
                if self.take(b',') {
                    continue;
                }
                break;
            }
            self.ws();
            if self.take(b';') {
                // more predicates for the same subject
                continue;
            }
            if self.take(b'.') {
                break;
            }
            // neither `;` nor `.` — expect another predicate by inference
            if self.peek().is_none() {
                break;
            }
        }
        out.push((subject, triples));
        Ok(())
    }

    fn parse_directive(&mut self) -> Result<(), SyntaxError> {
        self.ws();
        if self.take(b'@') {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_alphabetic() || b == b'-' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let kw = std::str::from_utf8(&self.s[start..self.pos])
                .unwrap_or("")
                .to_string();
            self.ws();
            match kw.as_str() {
                "prefix" => {
                    // `@prefix label: <iri> .`
                    let label_start = self.pos;
                    while let Some(b) = self.peek() {
                        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let label = std::str::from_utf8(&self.s[label_start..self.pos])
                        .unwrap_or("")
                        .to_string();
                    self.ws();
                    self.expect(b':')?;
                    self.ws();
                    let iri = self.read_iri()?;
                    self.ws();
                    self.expect(b'.')?;
                    self.prefixes.push((label, iri));
                    Ok(())
                }
                "base" => {
                    self.base = self.read_iri()?;
                    self.ws();
                    self.expect(b'.')?;
                    Ok(())
                }
                _ => Err(SyntaxError::new(format!("unknown directive `@{kw}`"))),
            }
        } else {
            Err(SyntaxError::new("expected directive or statement"))
        }
    }

    /// Parse the whole document into `Bu { subject → Bu { predicate → Fourth(objects) } }`.
    fn parse_document(mut self) -> Result<SyntaxTerm, SyntaxError> {
        let mut flat: Vec<(String, Vec<(String, String)>)> = Vec::new();
        loop {
            self.ws();
            if self.peek().is_none() {
                break;
            }
            if self.peek() == Some(b'@') {
                self.parse_directive()?;
                continue;
            }
            self.parse_statement(&mut flat)?;
        }
        // merge by subject → by predicate
        let mut subjects: Vec<(String, Vec<(String, Vec<String>)>)> = Vec::new();
        for (subj, triples) in flat {
            // find or insert subject
            let subj_idx = if let Some(i) = subjects.iter().position(|(s, _)| s == &subj) {
                i
            } else {
                subjects.push((subj.clone(), Vec::new()));
                subjects.len() - 1
            };
            let preds = &mut subjects[subj_idx].1;
            for (pred, obj) in triples {
                let pred_idx = if let Some(i) = preds.iter().position(|(p, _)| p == &pred) {
                    i
                } else {
                    preds.push((pred.clone(), Vec::new()));
                    preds.len() - 1
                };
                preds[pred_idx].1.push(obj);
            }
        }
        let mut out = Vec::new();
        for (subj, preds) in subjects {
            let pred_fields: Vec<(String, SyntaxTerm)> = preds
                .into_iter()
                .map(|(p, objs)| {
                    let items: Vec<SyntaxTerm> = objs
                        .into_iter()
                        .map(|o| SyntaxTerm::B(Atom::Str(o)))
                        .collect();
                    (p, SyntaxTerm::Fourth(items))
                })
                .collect();
            out.push((subj, SyntaxTerm::Bu(pred_fields)));
        }
        Ok(SyntaxTerm::Bu(out))
    }
}

/// Parse Turtle (RDF 1.1 Turtle) into the same `SyntaxTerm` tree. The lifted shape:
/// `Bu { subject_iri → Bu { predicate_iri → Fourth([object_term, ...]) } }`
pub fn parse_turtle(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let p = TurtleParser::new(input);
    p.parse_document()
}

// ═══════════════════════════════════════════════════════════════════════════
// RDF/XML — the other wire format SEPA / SAREF / OEO ontologies ship in.
// ═══════════════════════════════════════════════════════════════════════════
//
// Scoped to the `rdf:RDF` wrapper of `rdf:Description` blocks: the shape
// practically every published OWL/RDF ontology uses. Each `rdf:Description`
// with an `rdf:about="..."` becomes one subject; each child element becomes
/// a predicate; `rdf:resource` attributes name object IRIs; child text becomes
/// a literal; `rdf:parseType="Resource"` lets a child element carry nested
/// predicate/object triples (the `rdf:nodeID` blank-node reference).

struct RdfXmlParser<'a> {
    s: &'a [u8],
    pos: usize,
    /// Open `rdf:parseType="Resource"` bodies; bounded by [`MAX_NESTING_DEPTH`].
    depth: usize,
}

impl<'a> RdfXmlParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            s: input.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }

    /// Skip whitespace.
    fn ws(&mut self) {
        while let Some(b) = self.s.get(self.pos).copied() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn take(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.pos) == Some(&c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_str(&mut self, s: &str) -> Result<(), SyntaxError> {
        self.ws();
        let bytes = s.as_bytes();
        if self.s.len() >= self.pos + bytes.len()
            && &self.s[self.pos..self.pos + bytes.len()] == bytes
        {
            self.pos += bytes.len();
            Ok(())
        } else {
            Err(SyntaxError::new(format!("expected `{s}`")))
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.pos).copied()
    }

    /// Read a tag name (letters, digits, `:`, `-`, `_`, `.`).
    fn read_name(&mut self) -> Result<String, SyntaxError> {
        self.ws();
        let start = self.pos;
        while let Some(b) = self.s.get(self.pos).copied() {
            if b.is_ascii_alphanumeric() || b == b':' || b == b'-' || b == b'_' || b == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.s[start..self.pos])
            .map(String::from)
            .map_err(|_| SyntaxError::new("non-UTF8 tag"))
    }

    /// Read a quoted XML attribute value (`"..."` or `'...'`).
    fn read_attr_value(&mut self) -> Result<String, SyntaxError> {
        self.ws();
        let quote = self
            .s
            .get(self.pos)
            .copied()
            .ok_or_else(|| SyntaxError::new("expected attribute value"))?;
        if quote != b'"' && quote != b'\'' {
            return Err(SyntaxError::new("attribute value must be quoted"));
        }
        self.pos += 1;
        let start = self.pos;
        while let Some(b) = self.s.get(self.pos).copied() {
            if b == quote {
                let v = std::str::from_utf8(&self.s[start..self.pos])
                    .unwrap_or("")
                    .to_string();
                self.pos += 1;
                return Ok(v);
            }
            self.pos += 1;
        }
        Err(SyntaxError::new("unterminated attribute value"))
    }

    /// Read `<name attr="..." attr2="...">` or `<name ... />`. Returns the
    /// tag name, its attributes, and a flag indicating whether the tag was
    /// self-closing. The self-closing flag lets callers distinguish
    /// `<foo/>` (one predicate, no body) from `<foo>...</foo>` (predicate
    /// with a body) without re-scanning the buffer.
    fn read_open_tag(&mut self) -> Result<(String, Vec<(String, String)>, bool), SyntaxError> {
        self.expect_str("<")?;
        let name = self.read_name()?;
        let mut attrs = Vec::new();
        let mut self_closing = false;
        loop {
            self.ws();
            match self.peek() {
                Some(b'/') => {
                    self.pos += 1;
                    self.expect_str(">")?;
                    self_closing = true;
                    return Ok((name, attrs, self_closing));
                }
                Some(b'>') => {
                    self.pos += 1;
                    return Ok((name, attrs, self_closing));
                }
                Some(_) => {
                    let attr_name = self.read_name()?;
                    self.ws();
                    if !self.take(b'=') {
                        let ctx: String = self.s
                            [self.pos..self.pos.saturating_add(20).min(self.s.len())]
                            .iter()
                            .map(|&b| {
                                if b.is_ascii_graphic() || b == b' ' {
                                    b as char
                                } else {
                                    '.'
                                }
                            })
                            .collect();
                        return Err(SyntaxError::new(format!(
                            "expected `=` after attribute `{attr_name}` at context `{ctx}`"
                        )));
                    }
                    let val = self.read_attr_value()?;
                    attrs.push((attr_name, val));
                }
                None => return Err(SyntaxError::new("unterminated tag")),
            }
        }
    }

    /// Skip until `</tag>` is found for the given tag name. Currently unused
    /// (kept for the `rdf:parseType="Resource"` nested-blank-node case which
    /// our recursive `read_description` handles directly) — rename without
    /// the underscore if a future path needs it.
    fn _skip_to_close(&mut self, tag: &str) -> Result<(), SyntaxError> {
        loop {
            if self.peek().is_none() {
                return Err(SyntaxError::new(format!("no `</{tag}>` found")));
            }
            if self.take(b'<') {
                if self.take(b'/') {
                    let got = self.read_name()?;
                    self.ws();
                    self.expect_str(">")?;
                    if got == tag {
                        return Ok(());
                    }
                    return Err(SyntaxError::new(format!(
                        "expected `</{tag}>`, found `</{got}>`"
                    )));
                }
                // skip nested opening tag — recurse one level
                let inner = self.read_name()?;
                self.ws();
                self.expect_str(">")?;
                self._skip_to_close(&inner)?;
            } else {
                self.pos += 1;
            }
        }
    }

    /// Read element body — either text content (literal) or nested
    /// `<predicate> ... </predicate>` triples. Kept for future use with
    /// blank-node nesting variants — the main `read_description` reads text
    /// bodies inline so this path isn't currently invoked.
    fn _read_body(
        &mut self,
        subject: &str,
        out: &mut Vec<(String, Vec<(String, String)>)>,
    ) -> Result<(), SyntaxError> {
        // skip whitespace
        self.ws();
        if self.take(b'<') {
            // could be nested triples or end tag
            if self.take(b'/') {
                // closing tag of the caller; rewind so the caller sees it
                self.pos -= 2;
                return Ok(());
            }
            // nested element — treat as predicate → subject/predicate-pair
            self.pos -= 1;
            self.read_description(subject, true, out)
        } else {
            // text body — literal value of the just-consumed predicate
            // (the caller reads the predicate first; we signal via a sentinel)
            Err(SyntaxError::new("read_description handles the body inline"))
        }
    }

    /// Read an `<rdf:Description>` block (or any element when `inline` is true).
    /// - When `inline == false`: reads `<rdf:Description rdf:about="..."> ... </rdf:Description>`.
    ///   The subject is taken from `rdf:about` (resource) or `rdf:nodeID` (blank).
    /// - When `inline == true`: reads a nested element where the tag name IS the
    ///   predicate, the subject is the parent's subject, the body either carries
    ///   `rdf:resource` / `rdf:nodeID` (object = resource/blank) or text (object = literal).
    fn read_description(
        &mut self,
        parent_subject: &str,
        inline: bool,
        out: &mut Vec<(String, Vec<(String, String)>)>,
    ) -> Result<(), SyntaxError> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(depth_error());
        }
        self.depth += 1;
        let result = self.read_description_at(parent_subject, inline, out);
        self.depth -= 1;
        result
    }

    fn read_description_at(
        &mut self,
        parent_subject: &str,
        inline: bool,
        out: &mut Vec<(String, Vec<(String, String)>)>,
    ) -> Result<(), SyntaxError> {
        let (tag, attrs, _self_closing) = self.read_open_tag()?;
        // determine the subject for this description
        let subject = if inline {
            parent_subject.to_string()
        } else {
            // extract rdf:about or rdf:nodeID from attrs
            let mut subj = String::new();
            for (k, v) in &attrs {
                if k == "rdf:about" {
                    subj = v.clone();
                } else if k == "rdf:nodeID" {
                    subj = format!("_:{v}");
                }
            }
            subj
        };
        // collect triples for this description
        let mut triples: Vec<(String, String)> = Vec::new();
        // OWL/RDFS shorthand: an outer tag of `owl:Class` / `rdfs:Class` /
        // `owl:ObjectProperty` / etc. is implicit `rdf:type` for the subject —
        // `<owl:Class rdf:about="ex:Foo">` is exactly `ex:Foo a owl:Class` in Turtle.
        // Emit the implicit type triple so downstream walkers see the same shape
        // they would from a Turtle source.
        let tag_local = tag.rsplit(':').next().unwrap_or(&tag);
        let implicit_type = match tag_local {
            "Class"
                if tag.starts_with("http://www.w3.org/2002/07/owl#")
                    || tag == "owl:Class"
                    || tag.ends_with(":Class") && tag.contains("owl") =>
            {
                Some("http://www.w3.org/2002/07/owl#Class")
            }
            "Class"
                if tag.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                    || tag == "rdfs:Class"
                    || tag.ends_with(":Class") && tag.contains("rdfs") =>
            {
                Some("http://www.w3.org/2000/01/rdf-schema#Class")
            }
            "ObjectProperty" => Some("http://www.w3.org/2002/07/owl#ObjectProperty"),
            "DatatypeProperty" => Some("http://www.w3.org/2002/07/owl#DatatypeProperty"),
            "AnnotationProperty" => Some("http://www.w3.org/2002/07/owl#AnnotationProperty"),
            "NamedIndividual" => Some("http://www.w3.org/2002/07/owl#NamedIndividual"),
            "Ontology" if tag.starts_with("owl") || tag == "owl:Ontology" => {
                Some("http://www.w3.org/2002/07/owl#Ontology")
            }
            _ => None,
        };
        if let Some(itype) = implicit_type {
            if !subject.is_empty() {
                triples.push((
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string(),
                    itype.to_string(),
                ));
            }
        }
        loop {
            self.ws();
            // check for end tag
            if self.peek() == Some(b'<') {
                // peek further
                let save = self.pos;
                self.pos += 1;
                if self.take(b'/') {
                    let got = self.read_name()?;
                    self.ws();
                    self.expect_str(">")?;
                    if got == tag {
                        // done; flush triples
                        if !triples.is_empty() {
                            out.push((subject.clone(), triples));
                        } else if !inline && !subject.is_empty() {
                            // empty description — still record the subject with no triples
                            out.push((subject.clone(), Vec::new()));
                        }
                        return Ok(());
                    }
                    return Err(SyntaxError::new(format!(
                        "expected `</{tag}>`, got `</{got}>`"
                    )));
                }
                self.pos = save;
            }
            // parse a child predicate element
            let (pred, pred_attrs, pred_self_closing) = self.read_open_tag()?;
            let mut obj = String::new();
            let mut is_resource = false;
            let mut parse_type_resource = false;
            for (k, v) in &pred_attrs {
                match k.as_str() {
                    "rdf:resource" => {
                        obj = v.clone();
                        is_resource = true;
                    }
                    "rdf:nodeID" => {
                        obj = format!("_:{v}");
                        is_resource = true;
                    }
                    "rdf:parseType" if v == "Resource" => {
                        parse_type_resource = true;
                    }
                    _ => {}
                }
            }
            if parse_type_resource {
                // body is another description — same subject, predicate inherited
                self.read_description(&subject, true, out)?;
                triples.push((pred.clone(), format!("__nested__{}", triples.len())));
            } else if pred_self_closing {
                // The `<pred .../>` form — `read_open_tag` already consumed the `/>`,
                // so pos is past the close. Just push the (predicate, object) pair.
                if !obj.is_empty() || is_resource {
                    triples.push((pred.clone(), obj));
                }
            } else {
                // body: text → literal (read until `</pred>`)
                let text_start = self.pos;
                while let Some(b) = self.s.get(self.pos).copied() {
                    if b == b'<' {
                        break;
                    }
                    self.pos += 1;
                }
                obj = String::from_utf8_lossy(&self.s[text_start..self.pos])
                    .trim()
                    .to_string();
                self.expect_str("</")?;
                let got = self.read_name()?;
                self.ws();
                self.expect_str(">")?;
                if got != pred {
                    return Err(SyntaxError::new(format!(
                        "expected `</{pred}>`, got `</{got}>`"
                    )));
                }
                if !obj.is_empty() || is_resource {
                    triples.push((pred.clone(), obj));
                }
            }
        }
    }
}

/// Parse RDF/XML (`<rdf:RDF>...<rdf:Description>...</rdf:Description>...</rdf:RDF>`) into the
/// same `SyntaxTerm` shape as Turtle.
pub fn parse_rdfxml(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let mut p = RdfXmlParser::new(input);
    let mut flat: Vec<(String, Vec<(String, String)>)> = Vec::new();
    p.ws();
    // Skip the `<?xml ...?>` prolog and any XML comments / DOCTYPE declarations.
    // We just scan for the literal `<rdf:RDF` (with optional `:` prefix variant).
    loop {
        p.ws();
        if p.peek() != Some(b'<') {
            return Err(SyntaxError::new("no `<rdf:RDF>` in input"));
        }
        // peek ahead — skip XML prolog `<?...?>`, comments `<!--...-->`, and DOCTYPE
        let save = p.pos;
        p.pos += 1;
        if p.peek() == Some(b'?') || p.peek() == Some(b'!') {
            // skip until `?>` or `>` (for DOCTYPE/prolog)
            while let Some(b) = p.peek() {
                p.pos += 1;
                if b == b'>' {
                    break;
                }
            }
            continue;
        }
        // back up — the next char is the tag's first letter
        p.pos = save;
        break;
    }
    let (outer_tag, _attrs, _outer_self_closing) = p.read_open_tag()?;
    if outer_tag != "rdf:RDF" && outer_tag != "rdf" {
        return Err(SyntaxError::new(format!(
            "expected `<rdf:RDF>`, got `<{outer_tag}>`"
        )));
    }
    loop {
        p.ws();
        match p.peek() {
            None => return Err(SyntaxError::new("unterminated <rdf:RDF>")),
            Some(b'<') => {
                let save = p.pos;
                p.pos += 1;
                if p.take(b'/') {
                    let got = p.read_name()?;
                    p.ws();
                    p.expect_str(">")?;
                    if got != outer_tag {
                        return Err(SyntaxError::new(format!(
                            "expected `</{outer_tag}>`, got `</{got}>`"
                        )));
                    }
                    break;
                }
                p.pos = save;
                p.read_description("", false, &mut flat)?;
            }
            Some(_) => p.pos += 1,
        }
    }
    // merge by subject → by predicate, same as Turtle
    let mut subjects: Vec<(String, Vec<(String, Vec<String>)>)> = Vec::new();
    for (subj, triples) in flat {
        let subj_idx = if let Some(i) = subjects.iter().position(|(s, _)| s == &subj) {
            i
        } else {
            subjects.push((subj.clone(), Vec::new()));
            subjects.len() - 1
        };
        let preds = &mut subjects[subj_idx].1;
        for (pred, obj) in triples {
            let pred_idx = if let Some(i) = preds.iter().position(|(p, _)| p == &pred) {
                i
            } else {
                preds.push((pred.clone(), Vec::new()));
                preds.len() - 1
            };
            preds[pred_idx].1.push(obj);
        }
    }
    let mut out = Vec::new();
    for (subj, preds) in subjects {
        let pred_fields: Vec<(String, SyntaxTerm)> = preds
            .into_iter()
            .map(|(p, objs)| {
                let items: Vec<SyntaxTerm> = objs
                    .into_iter()
                    .map(|o| SyntaxTerm::B(Atom::Str(o)))
                    .collect();
                (p, SyntaxTerm::Fourth(items))
            })
            .collect();
        out.push((subj, SyntaxTerm::Bu(pred_fields)));
    }
    Ok(SyntaxTerm::Bu(out))
}

/// Render a `SyntaxTerm` back to Turtle. Sufficient for round-trip tests —
/// production rendering isn't required by the ontology ingestion path.
pub fn render_turtle(term: &SyntaxTerm) -> String {
    let mut out = String::new();
    out.push_str("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n");
    if let SyntaxTerm::Bu(subjects) = term {
        for (subj, preds) in subjects {
            let SyntaxTerm::Bu(preds) = preds else {
                continue;
            };
            out.push_str(&format!("<{}>\n", turtle_escape_iri(subj)));
            let mut first = true;
            for (pred, objs) in preds {
                if first {
                    out.push_str("    ");
                    first = false;
                } else {
                    out.push_str(" ;\n    ");
                }
                out.push_str(&format!("<{}>", turtle_escape_iri(pred)));
                if let SyntaxTerm::Fourth(items) = objs {
                    for obj in items {
                        if let SyntaxTerm::B(Atom::Str(s)) = obj {
                            out.push_str(&format!(" {}", turtle_format_object(s)));
                        }
                    }
                }
            }
            out.push_str(" .\n");
        }
    }
    out
}

/// Render a `SyntaxTerm` back to RDF/XML. Round-trip test scaffold.
pub fn render_rdfxml(term: &SyntaxTerm) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rdf:RDF\n");
    out.push_str("    xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    if let SyntaxTerm::Bu(subjects) = term {
        for (subj, preds) in subjects {
            out.push_str(&format!(
                "  <rdf:Description rdf:about=\"{}\">\n",
                escape_xml_attr(subj)
            ));
            if let SyntaxTerm::Bu(preds) = preds {
                for (pred, objs) in preds {
                    let tag = pred.rsplit(['/', '#']).next().unwrap_or(pred);
                    if let SyntaxTerm::Fourth(items) = objs {
                        for obj in items {
                            if let SyntaxTerm::B(Atom::Str(s)) = obj {
                                if s.starts_with("_:")
                                    || s.starts_with("http")
                                    || s.starts_with("urn")
                                {
                                    out.push_str(&format!(
                                        "    <{} rdf:resource=\"{}\" />\n",
                                        tag,
                                        escape_xml_attr(s)
                                    ));
                                } else {
                                    out.push_str(&format!(
                                        "    <{}>{}</{}>\n",
                                        tag,
                                        escape_xml_text(s),
                                        tag
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            out.push_str("  </rdf:Description>\n");
        }
    }
    out.push_str("</rdf:RDF>\n");
    out
}

fn turtle_escape_iri(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('>', "\\>")
        .replace('<', "\\<")
}

fn turtle_format_object(s: &str) -> String {
    if s.starts_with("_:") || s.starts_with("http") || s.starts_with("urn") || s.contains(':') {
        format!("<{}>", turtle_escape_iri(s))
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
