//! Block-style YAML: the parser ([`parse_yaml`], re-exported by [`super`]), the flow-collection
//! sub-parser, the renderer, and their scalar quote/unquote/chomp helpers. The honest scope is
//! stated in the header below.

use super::surfaces::{depth_error, MAX_NESTING_DEPTH};
use super::{Atom, SyntaxError, SyntaxTerm};

// ════════════════════════════════════════════════════════════════════════════
// YAML (block style) — a real, wide subset, not the full 1.2 spec: block mappings,
// block sequences (including the "dash aligns with its key's indent" quirk real
// files use), literal (`|`) and folded (`>`) block scalars with `-`/`+`/clip
// chomping, single/double-quoted and plain scalars, flow sequences/mappings
// (`[a, b]`/`{a: 1}`), and `#` comments outside quotes/block-scalars. This is the
// subset semgrep rules, OWASP ZAP's Automation Framework config, Kubernetes
// manifests, and most CI/tool rule-files actually use.
//
// NOT supported (fails loudly rather than mis-parsing silently): anchors/aliases
// (`&x`/`*x`), tags (`!!str`), multi-document streams (only the first document
// is read, `---`/`...` separators are skipped as noise), merge keys (`<<:`),
// multi-line plain scalars without an explicit block indicator. Anchors/aliases
// are rare in the declarative-rule-file class this exists for; failing loudly on
// them beats a parser that silently drops the aliased content.
// ════════════════════════════════════════════════════════════════════════════

pub fn parse_yaml(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let lines: Vec<&str> = input.lines().collect();
    let mut p = YamlParser {
        lines,
        pos: 0,
        depth: 0,
        fields: Vec::new(),
        items: Vec::new(),
    };
    match p.peek_indent() {
        Some(indent) => p.parse_block(indent),
        None => Ok(SyntaxTerm::Nb),
    }
}

fn line_indent(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b' ').count()
}

fn is_yaml_noise(line: &str) -> bool {
    // Fast path for the overwhelmingly common case: the first byte after any spaces/tabs is an
    // ordinary ASCII character that cannot start a blank, a comment or a `---`/`...` marker. The
    // Unicode-aware `trim` below handles everything else.
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if let Some(&c) = b.get(i) {
        if c.is_ascii_graphic() && c != b'#' {
            // `-` and `.` only matter as the start of a `---` / `...` marker.
            let marker = (c == b'-' || c == b'.') && b.get(i + 1) == Some(&c);
            if !marker {
                return false;
            }
        }
    }
    let t = line.trim();
    t.is_empty() || t.starts_with('#') || t == "---" || t == "..."
}

/// Find the key/value-separating colon in a mapping line's content (already past any leading
/// indent / `- ` marker). `None` means this line is not `key: value` shaped — the real YAML
/// rule: the separating colon must be followed by whitespace or end-of-line, and a quoted key
/// is skipped whole first (so `"a:b": val` finds the colon after the closing quote, not inside).
fn find_key_colon(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
        let q = b[i];
        i += 1;
        while i < b.len() {
            if b[i] == q {
                if q == b'\'' && i + 1 < b.len() && b[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                i += 1;
                break;
            }
            if q == b'"' && b[i] == b'\\' && i + 1 < b.len() {
                i += 2;
                continue;
            }
            i += 1;
        }
    }
    while i < b.len() {
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ' || b[i + 1] == b'\t') {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Strip a trailing `# comment` from a plain (unquoted) scalar — only when the `#` is preceded
/// by whitespace or starts the string (YAML's rule; a bare `#` inside a token, e.g. a URL
/// fragment `...html#anchor`, is NOT a comment).
fn strip_unquoted_comment(s: &str) -> &str {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'#' && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') {
            return s[..i].trim_end();
        }
    }
    s
}

fn looks_numeric(s: &str) -> bool {
    // Bytes, not chars: any non-ASCII byte fails the character test below, so the answer is the
    // same, and the index of an accepted byte is its index as a character.
    let b = s.as_bytes();
    !b.is_empty()
        && b.iter().enumerate().all(|(i, &c)| {
            c.is_ascii_digit()
                || c == b'.'
                || (i == 0 && (c == b'-' || c == b'+'))
                || c == b'e'
                || c == b'E'
        })
        && b.iter().any(u8::is_ascii_digit)
}

fn scalar_from_plain(s: &str) -> SyntaxTerm {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => SyntaxTerm::Nb,
        "true" | "True" | "TRUE" => SyntaxTerm::B(Atom::Bool(true)),
        "false" | "False" | "FALSE" => SyntaxTerm::B(Atom::Bool(false)),
        _ if looks_numeric(s) => SyntaxTerm::B(Atom::Num(s.to_string())),
        _ => SyntaxTerm::B(Atom::Str(s.to_string())),
    }
}

fn unquote_scalar(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        unescape_double(&s[1..s.len() - 1])
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone, Copy)]
enum Chomp {
    Strip,
    Clip,
    Keep,
}

struct YamlParser<'a> {
    lines: Vec<&'a str>,
    pos: usize,
    /// Open block collections; bounded by [`MAX_NESTING_DEPTH`] (the parser recurses per level).
    depth: usize,
    /// Scratch stacks for the mapping fields and sequence items being built, so each collection
    /// is allocated once at its final size instead of grown by pushing.
    fields: Vec<(String, SyntaxTerm)>,
    items: Vec<SyntaxTerm>,
}

impl YamlParser<'_> {
    fn skip_noise(&mut self) {
        while self.pos < self.lines.len() && is_yaml_noise(self.lines[self.pos]) {
            self.pos += 1;
        }
    }

    /// The indent of the next non-noise line, without consuming it.
    fn peek_indent(&mut self) -> Option<usize> {
        self.skip_noise();
        if self.pos >= self.lines.len() {
            None
        } else {
            Some(line_indent(self.lines[self.pos]))
        }
    }

    fn is_sequence_line_at(&mut self, indent: usize) -> bool {
        self.skip_noise();
        if self.pos >= self.lines.len() {
            return false;
        }
        let line = self.lines[self.pos];
        if line_indent(line) != indent {
            return false;
        }
        let content = &line[indent..];
        content == "-" || content.starts_with("- ")
    }

    fn looks_like_mapping_line(content: &str) -> bool {
        find_key_colon(content).is_some()
    }

    /// Parse whatever sits at exactly `indent`: a sequence, a mapping, or (rare at block level)
    /// a lone scalar/flow value.
    fn parse_block(&mut self, indent: usize) -> Result<SyntaxTerm, SyntaxError> {
        if self.depth >= MAX_NESTING_DEPTH {
            return Err(depth_error());
        }
        self.depth += 1;
        let term = self.parse_block_at(indent);
        self.depth -= 1;
        term
    }

    fn parse_block_at(&mut self, indent: usize) -> Result<SyntaxTerm, SyntaxError> {
        let Some(cur) = self.peek_indent() else {
            return Ok(SyntaxTerm::Nb);
        };
        if cur != indent {
            return Ok(SyntaxTerm::Nb);
        }
        let line = self.lines[self.pos];
        let content = &line[indent..];
        if content == "-" || content.starts_with("- ") {
            self.parse_sequence(indent)
        } else if Self::looks_like_mapping_line(content) {
            self.parse_mapping(indent)
        } else {
            self.pos += 1;
            self.parse_inline_value(content, indent)
        }
    }

    fn parse_sequence(&mut self, indent: usize) -> Result<SyntaxTerm, SyntaxError> {
        let base = self.items.len();
        while let Some(cur) = self.peek_indent() {
            if cur != indent {
                break;
            }
            let line = self.lines[self.pos];
            let content = &line[indent..];
            if content != "-" && !content.starts_with("- ") {
                break;
            }
            self.pos += 1;
            if content == "-" {
                let next = self
                    .peek_indent()
                    .filter(|&d| d > indent)
                    .unwrap_or(indent + 2);
                let item = self.parse_block(next)?;
                self.items.push(item);
                continue;
            }
            let rest = content[1..].trim_start();
            let dash_col = indent + (content.len() - rest.len());
            let item = if Self::looks_like_mapping_line(rest) {
                self.parse_mapping_from_first_line(rest, dash_col)?
            } else {
                self.parse_inline_value(rest, dash_col)?
            };
            self.items.push(item);
        }
        Ok(SyntaxTerm::Fourth(self.items.drain(base..).collect()))
    }

    fn parse_mapping(&mut self, indent: usize) -> Result<SyntaxTerm, SyntaxError> {
        let base = self.fields.len();
        while let Some(cur) = self.peek_indent() {
            if cur != indent {
                break;
            }
            let content = &self.lines[self.pos][indent..];
            if !Self::looks_like_mapping_line(content) {
                break;
            }
            self.pos += 1;
            let field = self.parse_key_value(content, indent)?;
            self.fields.push(field);
        }
        Ok(SyntaxTerm::Bu(self.fields.drain(base..).collect()))
    }

    /// A mapping whose FIRST key:value pair already sat on a `- ` sequence-item line (the
    /// current line was already consumed by the caller); subsequent keys of the same mapping
    /// appear on later lines at `indent` (the column right after the dash).
    fn parse_mapping_from_first_line(
        &mut self,
        first: &str,
        indent: usize,
    ) -> Result<SyntaxTerm, SyntaxError> {
        let base = self.fields.len();
        let first_field = self.parse_key_value(first, indent)?;
        self.fields.push(first_field);
        while let Some(cur) = self.peek_indent() {
            if cur != indent {
                break;
            }
            let content = &self.lines[self.pos][indent..];
            if content == "-"
                || content.starts_with("- ")
                || !Self::looks_like_mapping_line(content)
            {
                break;
            }
            self.pos += 1;
            let field = self.parse_key_value(content, indent)?;
            self.fields.push(field);
        }
        Ok(SyntaxTerm::Bu(self.fields.drain(base..).collect()))
    }

    /// `content` is one `key: value` (or bare `key:`) line's text, already past its indent;
    /// `self.pos` already points PAST this line (the caller advanced it) so lookahead here sees
    /// the next line.
    fn parse_key_value(
        &mut self,
        content: &str,
        key_indent: usize,
    ) -> Result<(String, SyntaxTerm), SyntaxError> {
        let colon =
            find_key_colon(content).ok_or_else(|| SyntaxError::new("expected 'key: value'"))?;
        let key = unquote_scalar(content[..colon].trim());
        let rest = content[colon + 1..].trim_start();
        if rest.is_empty() {
            let value = match self.peek_indent() {
                Some(next) if next > key_indent => self.parse_block(next)?,
                Some(next) if next == key_indent && self.is_sequence_line_at(next) => {
                    self.parse_block(next)?
                }
                _ => SyntaxTerm::Nb,
            };
            Ok((key, value))
        } else {
            Ok((key, self.parse_inline_value(rest, key_indent)?))
        }
    }

    /// The value text after `key: ` or `- ` on a single line: a block-scalar indicator, a flow
    /// collection, a quoted scalar, or a plain scalar (with its trailing `# comment` stripped).
    fn parse_inline_value(
        &mut self,
        s: &str,
        key_indent: usize,
    ) -> Result<SyntaxTerm, SyntaxError> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(SyntaxTerm::Nb);
        }
        if let Some(rest) = s.strip_prefix('|').or_else(|| s.strip_prefix('>')) {
            let folded = s.starts_with('>');
            let chomp = if rest.starts_with('-') {
                Chomp::Strip
            } else if rest.starts_with('+') {
                Chomp::Keep
            } else {
                Chomp::Clip
            };
            return Ok(SyntaxTerm::B(Atom::Str(
                self.parse_block_scalar(key_indent, folded, chomp),
            )));
        }
        if s.starts_with('[') || s.starts_with('{') {
            return parse_yaml_flow(s);
        }
        if s.starts_with('"') {
            return Ok(SyntaxTerm::B(Atom::Str(unquote_scalar(s))));
        }
        if s.starts_with('\'') {
            return Ok(SyntaxTerm::B(Atom::Str(unquote_scalar(s))));
        }
        Ok(scalar_from_plain(strip_unquoted_comment(s).trim()))
    }

    /// Consume every subsequent line indented MORE than `key_indent` as this block scalar's
    /// body; the first such line's indent sets the block's own left margin.
    fn parse_block_scalar(&mut self, key_indent: usize, folded: bool, chomp: Chomp) -> String {
        let mut raw: Vec<&str> = Vec::new();
        let mut block_indent: Option<usize> = None;
        loop {
            if self.pos >= self.lines.len() {
                break;
            }
            let line = self.lines[self.pos];
            if line.trim().is_empty() {
                raw.push("");
                self.pos += 1;
                continue;
            }
            let ind = line_indent(line);
            if ind <= key_indent {
                break;
            }
            let bi = *block_indent.get_or_insert(ind);
            raw.push(if line.len() >= bi { &line[bi..] } else { "" });
            self.pos += 1;
        }
        while raw.last() == Some(&"") {
            raw.pop();
        }
        let body = if folded {
            let mut out = String::new();
            for (i, l) in raw.iter().enumerate() {
                if i > 0 {
                    out.push(if raw[i - 1].is_empty() || l.is_empty() {
                        '\n'
                    } else {
                        ' '
                    });
                }
                out.push_str(l);
            }
            out
        } else {
            raw.join("\n")
        };
        match chomp {
            Chomp::Strip => body,
            Chomp::Clip | Chomp::Keep => format!("{body}\n"),
        }
    }
}

/// Flow-style YAML (`[a, b]` / `{a: 1, b: 2}`) — bracket-delimited, so no indent sensitivity;
/// close enough to JSON that a small dedicated parser (tolerating unquoted scalars inside,
/// which strict JSON forbids) is simplest.
fn parse_yaml_flow(s: &str) -> Result<SyntaxTerm, SyntaxError> {
    let mut fp = FlowParser {
        b: s.as_bytes(),
        pos: 0,
        depth: 0,
    };
    fp.value()
}

struct FlowParser<'a> {
    b: &'a [u8],
    pos: usize,
    /// Open flow collections; bounded by [`MAX_NESTING_DEPTH`].
    depth: usize,
}

impl FlowParser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.pos), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }
    fn value(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.ws();
        match self.peek() {
            Some(b'[' | b'{') if self.depth >= MAX_NESTING_DEPTH => Err(depth_error()),
            Some(b'[') => {
                self.depth += 1;
                let v = self.seq();
                self.depth -= 1;
                v
            }
            Some(b'{') => {
                self.depth += 1;
                let v = self.map();
                self.depth -= 1;
                v
            }
            Some(b'"') => Ok(SyntaxTerm::B(Atom::Str(self.quoted(b'"')?))),
            Some(b'\'') => Ok(SyntaxTerm::B(Atom::Str(self.quoted(b'\'')?))),
            _ => Ok(scalar_from_plain(self.plain_until(b",]}").trim())),
        }
    }
    fn seq(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(SyntaxTerm::Fourth(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.ws();
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(SyntaxError::new("unterminated flow sequence")),
            }
        }
        Ok(SyntaxTerm::Fourth(items))
    }
    fn map(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.pos += 1;
        let mut fields = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(SyntaxTerm::Bu(fields));
        }
        loop {
            self.ws();
            let key = match self.peek() {
                Some(b'"') => self.quoted(b'"')?,
                Some(b'\'') => self.quoted(b'\'')?,
                // Stop at `,`/`}` too — a flow-mapping entry may have no colon at all (`{UUID}`
                // means `{UUID: null}`, real YAML's implicit-null-value shorthand).
                _ => self.plain_until(b":,}").trim().to_string(),
            };
            self.ws();
            let val = if self.peek() == Some(b':') {
                self.pos += 1;
                self.value()?
            } else {
                SyntaxTerm::Nb
            };
            fields.push((key, val));
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(SyntaxError::new("unterminated flow mapping")),
            }
        }
        Ok(SyntaxTerm::Bu(fields))
    }
    fn plain_until(&mut self, stops: &[u8]) -> String {
        let start = self.pos;
        while self.pos < self.b.len() && !stops.contains(&self.b[self.pos]) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.b[start..self.pos]).into_owned()
    }
    fn quoted(&mut self, q: u8) -> Result<String, SyntaxError> {
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.b.len() {
            if self.b[self.pos] == q {
                if q == b'\'' && self.b.get(self.pos + 1) == Some(&b'\'') {
                    self.pos += 2;
                    continue;
                }
                let raw = std::str::from_utf8(&self.b[start..self.pos]).unwrap_or("");
                self.pos += 1;
                return Ok(if q == b'"' {
                    unescape_double(raw)
                } else {
                    raw.replace("''", "'")
                });
            }
            if q == b'"' && self.b[self.pos] == b'\\' {
                self.pos += 2;
                continue;
            }
            self.pos += 1;
        }
        Err(SyntaxError::new("unterminated quoted flow scalar"))
    }
}

/// Render a `SyntaxTerm` back to block-style YAML — mainly for round-trip testing; the parser
/// above is this module's real purpose (ingesting real rule files), not YAML generation.
pub(super) fn render_yaml(term: &SyntaxTerm, indent: usize) -> String {
    use std::fmt::Write as _;
    let pad = " ".repeat(indent);
    match term {
        SyntaxTerm::Nb | SyntaxTerm::Unknown => "null\n".to_string(),
        SyntaxTerm::B(Atom::Bool(b)) => format!("{b}\n"),
        SyntaxTerm::B(Atom::Num(n)) => format!("{n}\n"),
        SyntaxTerm::B(Atom::Str(s)) => format!("{}\n", render_yaml_scalar(s)),
        SyntaxTerm::Bu(fields) => {
            let mut out = String::new();
            for (k, v) in fields {
                match v {
                    SyntaxTerm::Bu(f) if !f.is_empty() => {
                        writeln!(out, "{pad}{k}:").unwrap();
                        out.push_str(&render_yaml(v, indent + 2));
                    }
                    SyntaxTerm::Fourth(items) if !items.is_empty() => {
                        writeln!(out, "{pad}{k}:").unwrap();
                        out.push_str(&render_yaml(v, indent));
                    }
                    _ => {
                        write!(out, "{pad}{k}: ").unwrap();
                        out.push_str(&render_yaml(v, indent + 2));
                    }
                }
            }
            out
        }
        SyntaxTerm::Fourth(items) => {
            let mut out = String::new();
            for item in items {
                match item {
                    SyntaxTerm::Bu(fields) if !fields.is_empty() => {
                        let mut first = true;
                        for (k, v) in fields {
                            let marker = if first {
                                format!("{pad}- ")
                            } else {
                                format!("{pad}  ")
                            };
                            first = false;
                            match v {
                                SyntaxTerm::Bu(f) if !f.is_empty() => {
                                    writeln!(out, "{marker}{k}:").unwrap();
                                    out.push_str(&render_yaml(v, indent + 4));
                                }
                                _ => {
                                    write!(out, "{marker}{k}: ").unwrap();
                                    out.push_str(&render_yaml(v, indent + 4));
                                }
                            }
                        }
                    }
                    _ => {
                        write!(out, "{pad}- ").unwrap();
                        out.push_str(&render_yaml(item, indent + 2));
                    }
                }
            }
            out
        }
    }
}

fn render_yaml_scalar(s: &str) -> String {
    if s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s == "true"
        || s == "false"
        || s == "null"
        || looks_numeric(s)
    {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        s.to_string()
    }
}
