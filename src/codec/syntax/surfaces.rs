//! The compact surfaces over the one term: JSON (writer + parser), BAML, TOON, CSV, POML, the
//! `Segment`/`BuildNode` path tree the CSV/TOON parsers rebuild through, and the escape/attr
//! helpers those surfaces share. Entry points are re-exported by [`super`].

use super::{Atom, SyntaxError, SyntaxTerm};

pub(super) fn render_json(term: &SyntaxTerm) -> String {
    let mut out = String::new();
    write_json(term, &mut out);
    out
}

/// The renderers below walk the term with an explicit work stack rather than recursing: a term
/// built in memory (or decoded from bytes) can be arbitrarily deep, and a recursive walk would
/// overflow the thread's stack on it. Work items are pushed in reverse so they pop in output order.
enum JsonWork<'a> {
    Term(&'a SyntaxTerm),
    Lit(&'static str),
    Key(&'a str),
}

fn write_json(term: &SyntaxTerm, out: &mut String) {
    let mut stack = vec![JsonWork::Term(term)];
    while let Some(work) = stack.pop() {
        match work {
            JsonWork::Lit(s) => out.push_str(s),
            JsonWork::Key(k) => {
                out.push('"');
                push_escaped_json(out, k);
                out.push_str("\":");
            }
            JsonWork::Term(term) => match term {
                SyntaxTerm::Nb | SyntaxTerm::Unknown => out.push_str("null"),
                SyntaxTerm::B(Atom::Bool(v)) => out.push_str(if *v { "true" } else { "false" }),
                SyntaxTerm::B(Atom::Num(v)) => out.push_str(v),
                SyntaxTerm::B(Atom::Str(v)) => {
                    out.push('"');
                    push_escaped_json(out, v);
                    out.push('"');
                }
                SyntaxTerm::Bu(fields) => {
                    out.push('{');
                    stack.push(JsonWork::Lit("}"));
                    for (i, (k, v)) in fields.iter().enumerate().rev() {
                        stack.push(JsonWork::Term(v));
                        stack.push(JsonWork::Key(k));
                        if i > 0 {
                            stack.push(JsonWork::Lit(","));
                        }
                    }
                }
                SyntaxTerm::Fourth(items) => {
                    out.push('[');
                    stack.push(JsonWork::Lit("]"));
                    for (i, v) in items.iter().enumerate().rev() {
                        stack.push(JsonWork::Term(v));
                        if i > 0 {
                            stack.push(JsonWork::Lit(","));
                        }
                    }
                }
            },
        }
    }
}

enum BamlWork<'a> {
    Term(&'a SyntaxTerm, usize),
    Lit(&'static str),
    /// `"\n" + "  " * depth + "}"` — a `Bu`'s closing brace.
    CloseObject(usize),
    /// `"\n" + "  " * depth + key + " = "` — one `Bu` field's lead-in.
    Field(&'a str, usize),
}

pub(super) fn render_baml_term(term: &SyntaxTerm, depth: usize) -> String {
    let mut out = String::new();
    let mut stack = vec![BamlWork::Term(term, depth)];
    while let Some(work) = stack.pop() {
        match work {
            BamlWork::Lit(s) => out.push_str(s),
            BamlWork::CloseObject(depth) => {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push('}');
            }
            BamlWork::Field(k, depth) => {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push_str(k);
                out.push_str(" = ");
            }
            BamlWork::Term(term, depth) => match term {
                SyntaxTerm::Nb | SyntaxTerm::Unknown => out.push_str("nb"),
                SyntaxTerm::B(Atom::Bool(v)) => out.push_str(if *v { "true" } else { "false" }),
                SyntaxTerm::B(Atom::Num(v)) => out.push_str(v),
                SyntaxTerm::B(Atom::Str(v)) => {
                    out.push('"');
                    out.push_str(&escape_json(v));
                    out.push('"');
                }
                SyntaxTerm::Bu(fields) => {
                    out.push('{');
                    stack.push(BamlWork::CloseObject(depth));
                    for (k, v) in fields.iter().rev() {
                        stack.push(BamlWork::Term(v, depth + 1));
                        stack.push(BamlWork::Field(k, depth + 1));
                    }
                }
                SyntaxTerm::Fourth(items) => {
                    out.push('[');
                    stack.push(BamlWork::Lit("]"));
                    for (i, v) in items.iter().enumerate().rev() {
                        stack.push(BamlWork::Term(v, depth));
                        if i > 0 {
                            stack.push(BamlWork::Lit(", "));
                        }
                    }
                }
            },
        }
    }
    out
}

pub(super) fn render_toon(term: &SyntaxTerm) -> String {
    let mut lines = Vec::new();
    flatten_toon("$", term, &mut lines);
    lines.join("\n") + "\n"
}

fn flatten_toon(path: &str, term: &SyntaxTerm, out: &mut Vec<String>) {
    let mut stack = vec![(path.to_string(), term)];
    while let Some((path, term)) = stack.pop() {
        match term {
            SyntaxTerm::Bu(fields) => {
                for (k, v) in fields.iter().rev() {
                    let next = if path == "$" {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    stack.push((next, v));
                }
            }
            SyntaxTerm::Fourth(items) => {
                for (idx, v) in items.iter().enumerate().rev() {
                    stack.push((format!("{path}[{idx}]"), v));
                }
            }
            _ => out.push(format!("{path} = {}", scalar_lit(term))),
        }
    }
}

pub(super) fn render_csv(term: &SyntaxTerm) -> String {
    let mut rows = Vec::new();
    rows.push(csv_row(["path", "kind", "value"]));
    flatten_csv("$", term, &mut rows);
    rows.join("\n") + "\n"
}

fn flatten_csv(path: &str, term: &SyntaxTerm, out: &mut Vec<String>) {
    let mut stack = vec![(path.to_string(), term)];
    while let Some((path, term)) = stack.pop() {
        let path = path.as_str();
        match term {
            SyntaxTerm::Nb => out.push(csv_row([path, "nb", ""])),
            SyntaxTerm::Unknown => out.push(csv_row([path, "unknown", ""])),
            SyntaxTerm::B(Atom::Bool(v)) => {
                out.push(csv_row([path, "bool", if *v { "true" } else { "false" }]));
            }
            SyntaxTerm::B(Atom::Num(v)) => out.push(csv_row([path, "num", v.as_str()])),
            SyntaxTerm::B(Atom::Str(v)) => out.push(csv_row([path, "str", v.as_str()])),
            SyntaxTerm::Bu(fields) => {
                out.push(csv_row([path, "bu", ""]));
                for (k, v) in fields.iter().rev() {
                    let next = if path == "$" {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    stack.push((next, v));
                }
            }
            SyntaxTerm::Fourth(items) => {
                out.push(csv_row([path, "fourth", ""]));
                for (idx, v) in items.iter().enumerate().rev() {
                    let next = if path == "$" {
                        format!("[{idx}]")
                    } else {
                        format!("{path}[{idx}]")
                    };
                    stack.push((next, v));
                }
            }
        }
    }
}

fn csv_row<I, S>(cols: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    cols.into_iter()
        .map(|s| csv_escape(s.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub(super) fn parse_csv(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let mut rows = input.lines().filter(|line| !line.trim().is_empty());
    let header = rows
        .next()
        .ok_or_else(|| SyntaxError::new("csv requires a header"))?;
    let expected = ["path", "kind", "value"];
    let parsed_header = parse_csv_row(header)?;
    if parsed_header != expected {
        return Err(SyntaxError::new("csv header must be path,kind,value"));
    }
    let mut root = BuildNode::Empty;
    let mut fillers = filler_budget(input);
    let mut cols: Vec<std::borrow::Cow<'_, str>> = Vec::with_capacity(3);
    for row in rows {
        cols.clear();
        csv_columns(row, &mut cols)?;
        if cols.len() != 3 {
            return Err(SyntaxError::new("csv row must have exactly 3 columns"));
        }
        let path = cols[0].trim();
        let kind = cols[1].trim();
        let value = cols[2].trim();
        let term = match kind {
            "nb" => SyntaxTerm::Nb,
            "unknown" => SyntaxTerm::Unknown,
            "bool" => SyntaxTerm::B(Atom::Bool(match value {
                "true" => true,
                "false" => false,
                _ => return Err(SyntaxError::new("csv bool must be true or false")),
            })),
            "num" => SyntaxTerm::B(Atom::Num(value.to_string())),
            "str" => SyntaxTerm::B(Atom::Str(value.to_string())),
            "bu" => SyntaxTerm::Bu(Vec::new()),
            "fourth" => SyntaxTerm::Fourth(Vec::new()),
            other => return Err(SyntaxError::new(format!("unknown csv kind: {other}"))),
        };
        insert_csv_node(&mut root, path, term, &mut fillers)?;
    }
    Ok(root.finish())
}

/// The columns of one row, borrowed from it when it has no quote character (no unescaping is
/// possible then, so each column is just a slice between commas) and owned otherwise.
fn csv_columns<'r>(
    row: &'r str,
    out: &mut Vec<std::borrow::Cow<'r, str>>,
) -> Result<(), SyntaxError> {
    if row.contains('"') {
        out.extend(parse_csv_row(row)?.into_iter().map(std::borrow::Cow::Owned));
    } else {
        out.extend(row.split(',').map(std::borrow::Cow::Borrowed));
    }
    Ok(())
}

fn parse_csv_row(row: &str) -> Result<Vec<String>, SyntaxError> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = row.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted => {
                if matches!(chars.peek(), Some('"')) {
                    chars.next();
                    cur.push('"');
                } else {
                    quoted = false;
                }
            }
            '"' if cur.is_empty() => quoted = true,
            ',' if !quoted => {
                fields.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if quoted {
        return Err(SyntaxError::new("unterminated csv quote"));
    }
    fields.push(cur);
    Ok(fields)
}

fn insert_csv_node(
    root: &mut BuildNode,
    path: &str,
    term: SyntaxTerm,
    fillers: &mut usize,
) -> Result<(), SyntaxError> {
    if path == "$" {
        *root = match term {
            SyntaxTerm::Bu(_) => BuildNode::Obj(Vec::new()),
            SyntaxTerm::Fourth(_) => BuildNode::List(Vec::new()),
            other => BuildNode::Scalar(other),
        };
        return Ok(());
    }
    let segs = parse_path(path)?;
    root.insert(&segs, term, fillers)
}

fn scalar_lit(term: &SyntaxTerm) -> String {
    match term {
        SyntaxTerm::Nb | SyntaxTerm::Unknown => "nb".into(),
        SyntaxTerm::B(Atom::Bool(v)) => v.to_string(),
        SyntaxTerm::B(Atom::Num(v)) => v.clone(),
        SyntaxTerm::B(Atom::Str(v)) => format!("\"{}\"", escape_json(v)),
        SyntaxTerm::Bu(_) | SyntaxTerm::Fourth(_) => render_json(term),
    }
}

pub(super) fn render_poml(term: &SyntaxTerm) -> String {
    format!("<poml>{}</poml>\n", render_poml_node(term))
}

enum PomlWork<'a> {
    Term(&'a SyntaxTerm),
    Lit(&'static str),
    /// `<field name="…">` — one `Bu` field's opening tag.
    FieldOpen(&'a str),
}

fn render_poml_node(term: &SyntaxTerm) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut stack = vec![PomlWork::Term(term)];
    while let Some(work) = stack.pop() {
        match work {
            PomlWork::Lit(s) => out.push_str(s),
            PomlWork::FieldOpen(k) => {
                out.push_str("<field name=\"");
                out.push_str(&escape_xml(k));
                out.push_str("\">");
            }
            PomlWork::Term(term) => match term {
                SyntaxTerm::Nb | SyntaxTerm::Unknown => out.push_str("<nb/>"),
                SyntaxTerm::B(Atom::Bool(v)) => {
                    let _ = write!(out, "<b kind=\"bool\">{v}</b>");
                }
                SyntaxTerm::B(Atom::Num(v)) => {
                    let _ = write!(out, "<b kind=\"num\">{}</b>", escape_xml(v));
                }
                SyntaxTerm::B(Atom::Str(v)) => {
                    let _ = write!(out, "<b kind=\"str\">{}</b>", escape_xml(v));
                }
                SyntaxTerm::Bu(fields) => {
                    out.push_str("<bu>");
                    stack.push(PomlWork::Lit("</bu>"));
                    for (k, v) in fields.iter().rev() {
                        stack.push(PomlWork::Lit("</field>"));
                        stack.push(PomlWork::Term(v));
                        stack.push(PomlWork::FieldOpen(k));
                    }
                }
                SyntaxTerm::Fourth(items) => {
                    out.push_str("<list>");
                    stack.push(PomlWork::Lit("</list>"));
                    for item in items.iter().rev() {
                        stack.push(PomlWork::Lit("</item>"));
                        stack.push(PomlWork::Term(item));
                        stack.push(PomlWork::Lit("<item>"));
                    }
                }
            },
        }
    }
    out
}

pub(super) fn parse_baml(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let trimmed = input.trim();
    let body = trimmed.strip_prefix("baml").unwrap_or(trimmed).trim();
    JsonParser::new(body, true).parse()
}

pub(super) fn parse_toon(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let mut root = BuildNode::Obj(Vec::new());
    let mut fillers = filler_budget(input);
    let mut segs: Vec<Segment<'_>> = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (path, value) = line
            .split_once('=')
            .ok_or_else(|| SyntaxError::new(format!("bad TOON line: {line}")))?;
        parse_path_into(path.trim(), &mut segs)?;
        let term = JsonParser::new(value.trim(), true).parse()?;
        root.insert(&segs, term, &mut fillers)?;
    }
    Ok(root.finish())
}

pub(super) fn parse_poml(input: &str) -> Result<SyntaxTerm, SyntaxError> {
    let body = input
        .trim()
        .strip_prefix("<poml>")
        .and_then(|s| s.strip_suffix("</poml>"))
        .ok_or_else(|| SyntaxError::new("POML must be wrapped in <poml>"))?;
    parse_poml_node(body.trim(), 0, &mut PomlScratch::default())
}

/// Reusable stacks for the fields and items being built, so each object or list is allocated once
/// at its final size instead of grown by pushing.
#[derive(Default)]
struct PomlScratch {
    fields: Vec<(String, SyntaxTerm)>,
    items: Vec<SyntaxTerm>,
}

fn parse_poml_node(
    input: &str,
    depth: usize,
    scratch: &mut PomlScratch,
) -> Result<SyntaxTerm, SyntaxError> {
    let input = input.trim();
    if input == "<nb/>" {
        return Ok(SyntaxTerm::Nb);
    }
    if let Some(inner) = input
        .strip_prefix("<bu>")
        .and_then(|s| s.strip_suffix("</bu>"))
    {
        if depth >= MAX_NESTING_DEPTH {
            return Err(depth_error());
        }
        let base = scratch.fields.len();
        let mut rest = inner.trim();
        while !rest.is_empty() {
            let tag_end = rest
                .find('>')
                .ok_or_else(|| SyntaxError::new("bad POML field"))?;
            let tag = &rest[..=tag_end];
            let name = attr(tag, "name").ok_or_else(|| SyntaxError::new("field without name"))?;
            let close = "</field>";
            let end = find_balanced_close(&rest[tag_end + 1..], "<field ", close)
                .ok_or_else(|| SyntaxError::new("unclosed field"))?
                + tag_end
                + 1;
            let field = (
                unescape_xml(name),
                parse_poml_node(&rest[tag_end + 1..end], depth + 1, scratch)?,
            );
            scratch.fields.push(field);
            rest = rest[end + close.len()..].trim();
        }
        return Ok(SyntaxTerm::Bu(scratch.fields.drain(base..).collect()));
    }
    if let Some(inner) = input
        .strip_prefix("<list>")
        .and_then(|s| s.strip_suffix("</list>"))
    {
        if depth >= MAX_NESTING_DEPTH {
            return Err(depth_error());
        }
        let base = scratch.items.len();
        let mut rest = inner.trim();
        while !rest.is_empty() {
            let tag = "<item>";
            let close = "</item>";
            let body = rest
                .strip_prefix(tag)
                .ok_or_else(|| SyntaxError::new("bad list item"))?;
            let end = find_balanced_close(body, tag, close)
                .ok_or_else(|| SyntaxError::new("unclosed item"))?;
            let item = parse_poml_node(&body[..end], depth + 1, scratch)?;
            scratch.items.push(item);
            rest = body[end + close.len()..].trim();
        }
        return Ok(SyntaxTerm::Fourth(scratch.items.drain(base..).collect()));
    }
    if input.starts_with("<b ") {
        let tag_end = input
            .find('>')
            .ok_or_else(|| SyntaxError::new("bad scalar tag"))?;
        let tag = &input[..=tag_end];
        let kind = attr(tag, "kind").ok_or_else(|| SyntaxError::new("scalar without kind"))?;
        let value = input[tag_end + 1..]
            .strip_suffix("</b>")
            .ok_or_else(|| SyntaxError::new("unclosed scalar"))?;
        let value = unescape_xml(value);
        return Ok(SyntaxTerm::B(match kind {
            "bool" => Atom::Bool(value == "true"),
            "num" => Atom::Num(value),
            _ => Atom::Str(value),
        }));
    }
    Err(SyntaxError::new("unknown POML node"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment<'a> {
    /// Borrowed from the path text: the key is copied only if inserting it creates a new field.
    Key(&'a str),
    Index(usize),
}

#[derive(Clone, Debug)]
enum BuildNode {
    Empty,
    Scalar(SyntaxTerm),
    Obj(Vec<(String, BuildNode)>),
    List(Vec<BuildNode>),
}

impl BuildNode {
    /// Place `value` at the path `segs`. `fillers` is what is left of the budget for the empty slots
    /// a sparse index (`a[9] = 1` makes nine before the one it sets) creates, so one short line
    /// cannot ask for a list of a billion slots.
    fn insert(
        &mut self,
        segs: &[Segment<'_>],
        value: SyntaxTerm,
        fillers: &mut usize,
    ) -> Result<(), SyntaxError> {
        if segs.is_empty() {
            *self = match value {
                SyntaxTerm::Bu(_) => BuildNode::Obj(Vec::new()),
                SyntaxTerm::Fourth(_) => BuildNode::List(Vec::new()),
                other => BuildNode::Scalar(other),
            };
            return Ok(());
        }
        match &segs[0] {
            Segment::Key(key) => {
                if matches!(self, BuildNode::Empty) {
                    *self = BuildNode::Obj(Vec::new());
                }
                let BuildNode::Obj(fields) = self else {
                    return Err(SyntaxError::new("path crosses non-object"));
                };
                let pos = fields.iter().position(|(k, _)| k.as_str() == *key);
                let idx = if let Some(i) = pos {
                    i
                } else {
                    fields.push(((*key).to_owned(), BuildNode::Empty));
                    fields.len() - 1
                };
                fields[idx].1.insert(&segs[1..], value, fillers)
            }
            Segment::Index(i) => {
                if matches!(self, BuildNode::Empty) {
                    *self = BuildNode::List(Vec::new());
                }
                let BuildNode::List(items) = self else {
                    return Err(SyntaxError::new("path crosses non-list"));
                };
                while items.len() <= *i {
                    *fillers = fillers
                        .checked_sub(1)
                        .ok_or_else(|| SyntaxError::new("sparse list index is too large"))?;
                    items.push(BuildNode::Empty);
                }
                items[*i].insert(&segs[1..], value, fillers)
            }
        }
    }

    fn finish(self) -> SyntaxTerm {
        match self {
            BuildNode::Empty => SyntaxTerm::Nb,
            BuildNode::Scalar(term) => term,
            BuildNode::Obj(fields) => {
                SyntaxTerm::Bu(fields.into_iter().map(|(k, v)| (k, v.finish())).collect())
            }
            BuildNode::List(items) => {
                SyntaxTerm::Fourth(items.into_iter().map(BuildNode::finish).collect())
            }
        }
    }
}

/// Empty list slots a TOON/CSV document may create beyond the values it states: proportional to its
/// line count, so memory stays linear in the input however the indexes are written.
fn filler_budget(input: &str) -> usize {
    (input.lines().count() + 1).saturating_mul(MAX_NESTING_DEPTH)
}

fn parse_path(path: &str) -> Result<Vec<Segment<'_>>, SyntaxError> {
    let mut segs = Vec::new();
    parse_path_into(path, &mut segs)?;
    Ok(segs)
}

/// First index of `b` in `s`. Paths are a few bytes long, where a plain loop beats the general
/// character searcher's setup cost.
#[inline]
fn find_byte(s: &[u8], b: u8) -> Option<usize> {
    s.iter().position(|&x| x == b)
}

/// [`parse_path`] into a caller-owned buffer (cleared first), so a parser that reads one path per
/// line can reuse a single allocation. The buffer keeps the segments borrowed from `path`.
fn parse_path_into<'p>(path: &'p str, segs: &mut Vec<Segment<'p>>) -> Result<(), SyntaxError> {
    segs.clear();
    if path == "$" {
        return Ok(());
    }
    let bytes = path.as_bytes();
    let mut start = 0;
    loop {
        // One `.`-separated part (empty parts are skipped, as `split('.')` yielded them).
        let end = find_byte(&bytes[start..], b'.').map_or(bytes.len(), |i| start + i);
        let part = &path[start..end];
        if segs.len() > MAX_NESTING_DEPTH {
            return Err(depth_error());
        }
        let mut rem = part;
        let mut keyed_only = false;
        if let Some(key_end) = find_byte(rem.as_bytes(), b'[') {
            if key_end > 0 {
                segs.push(Segment::Key(&rem[..key_end]));
            }
            rem = &rem[key_end..];
        } else if !rem.is_empty() {
            segs.push(Segment::Key(rem));
            keyed_only = true;
        }
        if !keyed_only {
            while let Some(body) = rem.strip_prefix('[') {
                let close = find_byte(body.as_bytes(), b']')
                    .ok_or_else(|| SyntaxError::new("bad index path"))?;
                segs.push(Segment::Index(
                    body[..close]
                        .parse()
                        .map_err(|_| SyntaxError::new("bad index"))?,
                ));
                rem = &body[close + 1..];
                // each segment is a level of the tree built (then finished and dropped) from this path
                if segs.len() > MAX_NESTING_DEPTH {
                    return Err(depth_error());
                }
            }
            if !rem.is_empty() {
                return Err(SyntaxError::new("bad path tail"));
            }
        }
        if end == bytes.len() {
            break;
        }
        start = end + 1;
    }
    if segs.len() > MAX_NESTING_DEPTH {
        return Err(depth_error());
    }
    Ok(())
}

/// How many arrays/objects may be open at once while parsing (`serde_json`'s default). The parsers
/// here recurse once per level, so without a bound a few KB of `[` aborts the process with a stack
/// overflow, which `catch_unwind` cannot stop. Shared by every recursive surface parser.
pub(super) const MAX_NESTING_DEPTH: usize = 128;

/// The error every surface parser returns when its nesting limit is exceeded.
pub(super) fn depth_error() -> SyntaxError {
    SyntaxError::new(format!(
        "nesting exceeds the depth limit of {MAX_NESTING_DEPTH}"
    ))
}

/// `loose == false` is the strict RFC 8259 grammar (what [`Surface::Json`](super::Surface::Json)
/// uses): no control characters in strings, only the escapes the RFC defines, no leading zeros,
/// no trailing commas, and a repeated object key is an error (the RFC leaves duplicates to the
/// receiver, and parsers disagree on which one wins; refusing them keeps every reader of the same
/// bytes on the same document). `loose == true` is the lenient dialect BAML and TOON values use.
pub(super) struct JsonParser<'a> {
    /// The input as text: any slice that starts and ends on an ASCII delimiter is valid UTF-8, so
    /// strings and numbers are copied out without being validated again.
    text: &'a str,
    s: &'a [u8],
    pos: usize,
    loose: bool,
    depth: usize,
    max_depth: usize,
    /// Scratch stacks for the object fields and array items being built. Children are pushed here
    /// and moved into a `Vec` of exactly the right size when the container closes, so building a
    /// container is one allocation instead of a push-and-grow sequence of them.
    fields: Vec<(String, SyntaxTerm)>,
    items: Vec<SyntaxTerm>,
}

impl<'a> JsonParser<'a> {
    pub(super) fn new(input: &'a str, loose: bool) -> Self {
        Self {
            text: input,
            s: input.as_bytes(),
            pos: 0,
            loose,
            depth: 0,
            max_depth: MAX_NESTING_DEPTH,
            fields: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Override the nesting limit (default [`MAX_NESTING_DEPTH`]).
    #[allow(dead_code)]
    pub(super) fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    fn enter(&mut self) -> Result<(), SyntaxError> {
        if self.depth >= self.max_depth {
            return Err(depth_error());
        }
        self.depth += 1;
        Ok(())
    }

    pub(super) fn parse(mut self) -> Result<SyntaxTerm, SyntaxError> {
        let v = self.value()?;
        self.ws();
        if self.pos != self.s.len() {
            return Err(SyntaxError::new("trailing input"));
        }
        Ok(v)
    }

    fn value(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.ws();
        match self.peek() {
            Some(b'{') => {
                self.enter()?;
                let v = self.object();
                self.depth -= 1;
                v
            }
            Some(b'[') => {
                self.enter()?;
                let v = self.array();
                self.depth -= 1;
                v
            }
            Some(b'"') => self.string().map(|s| SyntaxTerm::B(Atom::Str(s))),
            Some(b'-' | b'0'..=b'9') => Ok(SyntaxTerm::B(Atom::Num(self.number()?))),
            Some(_) if self.loose => {
                let word = self.word();
                Ok(match word.as_str() {
                    "nb" | "null" => SyntaxTerm::Nb,
                    "true" => SyntaxTerm::B(Atom::Bool(true)),
                    "false" => SyntaxTerm::B(Atom::Bool(false)),
                    _ => SyntaxTerm::B(Atom::Str(word)),
                })
            }
            _ if self.take_lit("null") => Ok(SyntaxTerm::Nb),
            _ if self.take_lit("true") => Ok(SyntaxTerm::B(Atom::Bool(true))),
            _ if self.take_lit("false") => Ok(SyntaxTerm::B(Atom::Bool(false))),
            _ => Err(SyntaxError::new("expected value")),
        }
    }

    fn object(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.expect(b'{')?;
        let base = self.fields.len();
        // Duplicate detection: a linear scan of the keys so far is fastest for the small objects
        // that dominate real documents; an object that grows past `LINEAR_KEY_LIMIT` keys switches
        // to a hash set so a hostile wide object stays linear overall.
        const LINEAR_KEY_LIMIT: usize = 16;
        let mut seen: Option<std::collections::HashSet<String>> = None;
        let mut after_comma = false;
        loop {
            self.ws();
            if self.take(b'}') {
                if after_comma && !self.loose {
                    return Err(SyntaxError::new("trailing comma in object"));
                }
                break;
            }
            let key = if self.peek() == Some(b'"') {
                self.string()?
            } else if self.loose {
                self.word()
            } else {
                return Err(SyntaxError::new("expected quoted object key"));
            };
            self.ws();
            if !(self.take(b':') || (self.loose && self.take(b'='))) {
                return Err(SyntaxError::new("expected object separator"));
            }
            if !self.loose {
                let duplicate = if let Some(set) = seen.as_mut() {
                    !set.insert(key.clone())
                } else {
                    let mine = &self.fields[base..];
                    let dup = mine.iter().any(|(k, _)| *k == key);
                    if !dup && mine.len() + 1 >= LINEAR_KEY_LIMIT {
                        let mut set: std::collections::HashSet<String> =
                            mine.iter().map(|(k, _)| k.clone()).collect();
                        set.insert(key.clone());
                        seen = Some(set);
                    }
                    dup
                };
                if duplicate {
                    return Err(SyntaxError::new(format!("duplicate object key {key:?}")));
                }
            }
            let val = self.value()?;
            self.fields.push((key, val));
            self.ws();
            if self.take(b'}') {
                break;
            }
            after_comma = self.take(b',') || (self.loose && self.take(b';'));
            if !after_comma {
                self.ws();
                if !self.loose {
                    return Err(SyntaxError::new("expected comma"));
                }
            }
        }
        Ok(SyntaxTerm::Bu(self.fields.drain(base..).collect()))
    }

    fn array(&mut self) -> Result<SyntaxTerm, SyntaxError> {
        self.expect(b'[')?;
        let base = self.items.len();
        let mut after_comma = false;
        loop {
            self.ws();
            if self.take(b']') {
                if after_comma && !self.loose {
                    return Err(SyntaxError::new("trailing comma in array"));
                }
                break;
            }
            let item = self.value()?;
            self.items.push(item);
            self.ws();
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
            after_comma = true;
        }
        Ok(SyntaxTerm::Fourth(self.items.drain(base..).collect()))
    }

    /// A JSON string. Bytes are collected raw (not widened one-at-a-time to `char`, which mangled
    /// every multi-byte UTF-8 sequence — the earlier form treated a Chinese character's 3 continuation
    /// bytes as 3 separate Latin-1 codepoints) and decoded once at the end; `\uXXXX` is handled,
    /// including surrogate pairs (Python's `json.dump` default `ensure_ascii=True` encodes every
    /// non-ASCII character this way, so any CJK-bearing corpus hits this path on nearly every string).
    ///
    /// **There is deliberately no bytes-as-chars variant of this**, and a byte-level tokenizer
    /// loader is not a reason to add one. GPT-2 style vocabularies do spell each byte as its own
    /// printable char, but that alphabet (`bytes_to_unicode`) is 256 real Unicode *scalars* — `Ġ` is
    /// U+0120, not the byte pair C4 A0 — so decoding the file correctly is already exactly what such
    /// a loader wants. A parser that widened bytes only forced the loader to undo the widening, and
    /// that repair is not idempotent: run it over correct scalars and `Ã©` (the spelling of `é`'s two
    /// UTF-8 bytes) collapses back to one char. One decode, here, is the whole contract.
    fn string(&mut self) -> Result<String, SyntaxError> {
        self.expect(b'"')?;
        // Fast path: scan to the closing quote. With no escape and no rejected control byte, the
        // string is one slice of the input, copied once and already known to be valid UTF-8.
        let start = self.pos;
        let mut i = start;
        while let Some(&b) = self.s.get(i) {
            if b == b'"' {
                self.pos = i + 1;
                return Ok(self.text[start..i].to_owned());
            }
            if b == b'\\' || (b < 0x20 && !self.loose) {
                break;
            }
            i += 1;
        }
        // Slow path (an escape, a control byte, or no closing quote): carry on from where the scan
        // stopped, with everything scanned so far already in the buffer.
        let mut out: Vec<u8> = Vec::with_capacity(i - start + 16);
        out.extend_from_slice(&self.s[start..i]);
        self.pos = i;
        while let Some(b) = self.next() {
            match b {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| SyntaxError::new("bad utf8 in string"))
                }
                b'\\' => {
                    let esc = self.next().ok_or_else(|| SyntaxError::new("bad escape"))?;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'u' => {
                            let cp = self.hex4()?;
                            let ch = if (0xD800..=0xDBFF).contains(&cp) {
                                if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                                    return Err(SyntaxError::new("unpaired surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return Err(SyntaxError::new("unpaired surrogate"));
                                }
                                let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                char::from_u32(c)
                                    .ok_or_else(|| SyntaxError::new("bad codepoint"))?
                            } else {
                                char::from_u32(cp)
                                    .ok_or_else(|| SyntaxError::new("bad codepoint"))?
                            };
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ if !self.loose => return Err(SyntaxError::new("invalid escape")),
                        other => out.push(other),
                    }
                }
                0x00..=0x1F if !self.loose => {
                    return Err(SyntaxError::new("control character in string"))
                }
                other => out.push(other),
            }
        }
        Err(SyntaxError::new("unterminated string"))
    }

    /// Four hex digits after `\u` — the codepoint (or surrogate half) they encode.
    fn hex4(&mut self) -> Result<u32, SyntaxError> {
        let mut v = 0u32;
        for _ in 0..4 {
            let b = self
                .next()
                .ok_or_else(|| SyntaxError::new("bad \\u escape"))?;
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(SyntaxError::new("bad \\u escape")),
            };
            v = v * 16 + d as u32;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<String, SyntaxError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digit_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.pos == digit_start {
            return Err(SyntaxError::new("bad number"));
        }
        if !self.loose && self.pos - digit_start > 1 && self.s[digit_start] == b'0' {
            return Err(SyntaxError::new("leading zero in number"));
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(SyntaxError::new("bad number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(SyntaxError::new("bad number"));
            }
        }
        Ok(self.text[start..self.pos].to_owned())
    }

    fn word(&mut self) -> String {
        self.ws();
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'/' | b'.')
        ) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.pos]).to_string()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn take_lit(&mut self, lit: &str) -> bool {
        if self.s[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), SyntaxError> {
        if self.take(b) {
            Ok(())
        } else {
            Err(SyntaxError::new("unexpected byte"))
        }
    }

    fn take(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }
}

/// The value of `name="..."` inside a tag: the first place `name="` occurs, up to the next quote.
fn attr<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let hay = tag.as_bytes();
    let name = name.as_bytes();
    let need = name.len() + 2;
    let mut i = 0;
    while i + need <= hay.len() {
        if hay[i..].starts_with(name)
            && hay[i + name.len()] == b'='
            && hay[i + name.len() + 1] == b'"'
        {
            let start = i + need;
            let end = tag[start..].find('"')?;
            return Some(&tag[start..start + end]);
        }
        i += 1;
    }
    None
}

/// First index at or after `from` where `pat` occurs, for a `pat` that starts with `<`. Finds the
/// `<` with the fast single-character search and checks the rest in place, instead of building a
/// substring searcher for every call.
fn find_lt(s: &str, pat: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(i) = s[at..].find('<') {
        let p = at + i;
        if s[p..].starts_with(pat) {
            return Some(p);
        }
        at = p + 1;
    }
    None
}

fn find_balanced_close(s: &str, open: &str, close: &str) -> Option<usize> {
    let mut cursor = 0usize;
    let mut depth = 0usize;
    // The next `open` / `close` at or after `cursor`, searched again only once `cursor` has passed
    // it, so the whole scan reads `s` once. (Searching both from `cursor` on every step re-read the
    // far-away match each time: quadratic on `<item>` x N followed by `</item>` x N.) A search that
    // found nothing stays "nothing": `cursor` only moves forward.
    let mut next_open = find_lt(s, open, 0);
    let mut next_close = find_lt(s, close, 0);
    loop {
        if next_open.is_some_and(|o| o < cursor) {
            next_open = find_lt(s, open, cursor);
        }
        if next_close.is_some_and(|c| c < cursor) {
            next_close = find_lt(s, close, cursor);
        }
        match (next_open, next_close) {
            (_, Some(c)) if next_open.is_none_or(|o| c < o) => {
                if depth == 0 {
                    return Some(c);
                }
                depth -= 1;
                cursor = c + close.len();
            }
            (Some(o), _) => {
                depth += 1;
                cursor = o + open.len();
            }
            _ => return None,
        }
    }
}

fn push_escaped_json(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c => out.push(c),
        }
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_escaped_json(&mut out, s);
    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape_xml(s: &str) -> String {
    // No `&` means no entity: one copy instead of four replace passes (the common case).
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
