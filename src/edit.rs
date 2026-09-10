//! Format-preserving edits of `[dependencies]` and `[dev-dependencies]`, for
//! `jrs add` and `jrs remove`.
//!
//! Rewriting the file through `Manifest::render` would throw away every comment,
//! blank line and quoting choice the user made. A general format-preserving TOML
//! editor would keep them, but it is a new crate, and SPEC §13 (question 1)
//! keeps the crate list minimal. It is also more than the job needs: jrs's
//! dependency tables are flat, one `"group:artifact" = value` pair per line. So
//! this is a line editor for exactly that shape. It lexes just enough TOML to
//! tell table headers, keys, values and comments apart, and to know when a line
//! belongs to a string or array opened further up. Anything it cannot edit
//! without guessing (a sub-table declaration, a value spanning lines, a
//! duplicate key) is refused with an error that sends the user to `jrs.toml`,
//! rather than being mangled.
//!
//! Nothing here validates the result as a manifest; that is the caller's job.

use std::fmt::Write as _;
use std::ops::Range;

use crate::error::{JrsError, Result};
use crate::manifest::MANIFEST_FILE;

/// The outcome of [`upsert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edited {
    /// The key was not declared; a new entry was written.
    Added(String),
    /// The key was declared; only its value changed.
    Replaced {
        text: String,
        /// The old value as written, trimmed, without its trailing comment.
        previous: String,
    },
}

impl Edited {
    /// The edited manifest text.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Edited::Added(text) | Edited::Replaced { text, .. } => text,
        }
    }
}

/// Insert `key = value` into `[section]`, or replace the value of an existing
/// entry with that key.
///
/// `value` is already-rendered TOML (e.g. `"\"33.0.0-jre\""` or
/// `{ version = "1", compile-only = true }`). An existing entry keeps its key
/// quoting, its spacing around `=` and its trailing comment; only the value is
/// swapped. A missing section is created.
///
/// # Errors
///
/// [`JrsError::Manifest`] when the edit cannot be made safely by rewriting
/// one line: `value` spans several lines, the existing entry does, or the
/// key or section is declared in a form this editor does not rewrite.
pub fn upsert(text: &str, section: &str, key: &str, value: &str) -> Result<Edited> {
    let target = Target {
        section,
        key,
        removing: false,
    };
    // Everything below edits one line per entry; a multi-line value would
    // produce a file this module then refuses to touch again.
    if value.contains(['\n', '\r']) {
        return Err(target.refuse("would get a value spanning several lines"));
    }
    let doc = Doc::parse(text);
    doc.check_not_declared_elsewhere(&target)?;

    let Some(sec) = doc.section(&target)? else {
        let entry = format!("{} = {value}", render_key(key, false));
        return Ok(Edited::Added(doc.create_section(section, entry)));
    };

    if let Some(found) = doc.entry(&sec, &target)? {
        if found.multi_line {
            return Err(target.refuse("has a value spanning several lines"));
        }
        let line = doc.lines[found.line].text;
        let Range { start, end } = found.pair.value.clone();
        let previous = line[start..end].to_string();
        let edited = format!("{}{value}{}", &line[..start], &line[end..]);
        return Ok(Edited::Replaced {
            text: doc.replace(found.line, &edited),
            previous,
        });
    }

    let (single_quoted, indent) = doc.entry_style(&sec);
    let entry = format!("{indent}{} = {value}", render_key(key, single_quoted));
    let at = doc.content_end(&sec);
    Ok(Edited::Added(doc.splice(at, at, &[entry])))
}

/// Remove the entry for `key` from `[section]`. `Ok(None)` when there is no
/// such entry.
///
/// Only the entry's own line (or lines, for a value spanning several) goes;
/// comments elsewhere stay, and a section left empty keeps its header.
///
/// # Errors
///
/// [`JrsError::Manifest`] when the key or section is declared in a form this
/// editor does not rewrite.
pub fn remove(text: &str, section: &str, key: &str) -> Result<Option<String>> {
    let target = Target {
        section,
        key,
        removing: true,
    };
    let doc = Doc::parse(text);
    doc.check_not_declared_elsewhere(&target)?;
    let Some(sec) = doc.section(&target)? else {
        return Ok(None);
    };
    let Some(found) = doc.entry(&sec, &target)? else {
        return Ok(None);
    };
    // Continuation lines belong to the value, and the lexer has already
    // proven where it ends, so they go with it.
    let mut end = found.line + 1;
    while end < sec.end && matches!(doc.lines[end].kind, Kind::Continuation) {
        end += 1;
    }
    Ok(Some(doc.splice(found.line, end, &[])))
}

// ---- errors ----------------------------------------------------------------

/// The entry being edited, carried around so every refusal can name it.
struct Target<'a> {
    section: &'a str,
    key: &'a str,
    removing: bool,
}

impl Target<'_> {
    fn refuse(&self, problem: &str) -> JrsError {
        let hint = if self.removing {
            "remove it from"
        } else {
            "edit it in"
        };
        JrsError::manifest(format!(
            "`{}.\"{}\"` {problem}; {hint} {MANIFEST_FILE} by hand",
            self.section, self.key
        ))
    }
}

// ---- the line model --------------------------------------------------------

struct Doc<'a> {
    lines: Vec<Line<'a>>,
    /// The ending new lines get: the file's own, or `\n` for a file with none.
    eol: &'a str,
}

struct Line<'a> {
    /// The line without its ending.
    text: &'a str,
    /// `\r\n`, `\n`, or empty for a last line with no newline. Kept per line so
    /// a file with mixed endings comes back byte-identical outside the edit.
    ending: &'a str,
    kind: Kind,
}

enum Kind {
    Blank,
    Comment,
    /// `[table]` or `[[array]]`; `path` is `None` when the header is malformed,
    /// which still ends the section before it.
    Header {
        path: Option<Vec<String>>,
        array: bool,
    },
    /// The first line of a `key = value` pair. `pair` is `None` when the key
    /// could not be read, which makes the line unmatchable but still an entry.
    Entry {
        pair: Option<Pair>,
        /// The value opens a string, array or inline table closed on a later line.
        multi_line: bool,
    },
    /// Inside a string, array or inline table opened on an earlier line.
    Continuation,
}

struct Pair {
    /// `"g:a".version` is `["g:a", "version"]`.
    path: Vec<String>,
    /// The quote character of the first key segment, `None` for a bare key.
    quote: Option<u8>,
    /// Byte range of the value, trimmed, without the trailing comment.
    value: Range<usize>,
}

/// A `[section]` header and the lines up to the next header.
struct Section {
    header: usize,
    /// Index of the next header, or the line count.
    end: usize,
}

struct Found<'d> {
    line: usize,
    pair: &'d Pair,
    multi_line: bool,
}

impl<'a> Doc<'a> {
    fn parse(text: &'a str) -> Doc<'a> {
        let mut lexer = Lexer::default();
        let mut lines = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            let (raw, tail) = match rest.find('\n') {
                Some(n) => rest.split_at(n + 1),
                None => (rest, ""),
            };
            let body = raw
                .strip_suffix("\r\n")
                .or_else(|| raw.strip_suffix('\n'))
                .unwrap_or(raw);
            let (text, ending) = raw.split_at(body.len());
            lines.push(Line {
                text,
                ending,
                kind: lexer.classify(text),
            });
            rest = tail;
        }
        let eol = lines
            .iter()
            .map(|l| l.ending)
            .find(|e| !e.is_empty())
            .unwrap_or("\n");
        Doc { lines, eol }
    }

    /// The plain `[name]` header, refusing forms of the table this module
    /// cannot extend safely.
    fn section(&self, target: &Target) -> Result<Option<Section>> {
        let mut found = None;
        for (i, line) in self.lines.iter().enumerate() {
            let Kind::Header {
                path: Some(path),
                array,
            } = &line.kind
            else {
                continue;
            };
            if path.len() != 1 || path[0] != target.section {
                continue;
            }
            if *array {
                return Err(target.refuse(&format!(
                    "cannot be edited: `[[{}]]` is an array of tables",
                    target.section
                )));
            }
            if found.is_some() {
                return Err(target.refuse(&format!(
                    "cannot be edited: `[{}]` is declared more than once",
                    target.section
                )));
            }
            found = Some(i);
        }
        Ok(found.map(|header| Section {
            header,
            end: self.next_header(header + 1),
        }))
    }

    /// Refuse the key when it lives somewhere other than a line of its own
    /// section: a `[section."key"]` table anywhere in the file, or the whole
    /// section spelled as a dotted key or inline table before the first header.
    /// Editing `[section]` in either case would leave a second declaration
    /// behind, or produce a table defined twice.
    fn check_not_declared_elsewhere(&self, target: &Target) -> Result<()> {
        let mut in_root = true;
        for line in &self.lines {
            match &line.kind {
                Kind::Header {
                    path: Some(path), ..
                } => {
                    in_root = false;
                    if path.len() >= 2 && path[0] == target.section && path[1] == target.key {
                        return Err(target.refuse("is declared as a table"));
                    }
                }
                Kind::Header { path: None, .. } => in_root = false,
                Kind::Entry { pair: Some(p), .. }
                    if in_root && p.path.first().is_some_and(|k| k == target.section) =>
                {
                    return Err(target.refuse(&format!(
                        "cannot be edited: `{}` is written as a dotted key or inline table \
                         at the top of the file",
                        target.section
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The line declaring `key` in `sec`, if any.
    fn entry(&self, sec: &Section, target: &Target) -> Result<Option<Found<'_>>> {
        let mut found = None;
        for i in sec.header + 1..sec.end {
            let Kind::Entry {
                pair: Some(pair),
                multi_line,
            } = &self.lines[i].kind
            else {
                continue;
            };
            if pair.path[0] != target.key {
                continue;
            }
            // `"g:a".version = "1"` is the sub-table form written as a dotted key.
            if pair.path.len() > 1 {
                return Err(target.refuse("is declared as a table"));
            }
            if found.is_some() {
                return Err(target.refuse("is declared more than once"));
            }
            found = Some(Found {
                line: i,
                pair,
                multi_line: *multi_line,
            });
        }
        Ok(found)
    }

    fn next_header(&self, from: usize) -> usize {
        (from..self.lines.len())
            .find(|&i| matches!(self.lines[i].kind, Kind::Header { .. }))
            .unwrap_or(self.lines.len())
    }

    /// Where a new entry goes: just past the section's last non-blank line, so
    /// the blank lines separating it from the next section stay where they are.
    fn content_end(&self, sec: &Section) -> usize {
        let body = sec.header + 1;
        let mut end = sec.end;
        if end < self.lines.len() {
            end = self.attached_comment_start(end);
        }
        while end > body && matches!(self.lines[end - 1].kind, Kind::Blank) {
            end -= 1;
        }
        end
    }

    /// The first line of the comment block sitting directly on top of the
    /// header at `header`, or `header` itself when there is none. A block only
    /// counts when a blank line (or the start of the file) sets it apart from
    /// what precedes it; `# Test-only` above `[dev-dependencies]` documents
    /// that table, and a new dependency must not land between the two.
    fn attached_comment_start(&self, header: usize) -> usize {
        let mut start = header;
        while start > 0 && matches!(self.lines[start - 1].kind, Kind::Comment) {
            start -= 1;
        }
        let set_apart = start == 0 || matches!(self.lines[start - 1].kind, Kind::Blank);
        if start < header && set_apart {
            start
        } else {
            header
        }
    }

    /// Quoting and indentation for a new entry, copied from the ones already
    /// in the section so the addition does not stand out.
    fn entry_style(&self, sec: &Section) -> (bool, &'a str) {
        let mut any = false;
        let mut all_single = true;
        let mut indent = "";
        for line in &self.lines[sec.header + 1..sec.end] {
            if let Kind::Entry { pair, .. } = &line.kind {
                any = true;
                all_single &= pair.as_ref().is_some_and(|p| p.quote == Some(b'\''));
                indent = &line.text[..line.text.len() - line.text.trim_start().len()];
            }
        }
        (any && all_single, indent)
    }

    /// Put `[section]` with its first entry somewhere sensible.
    ///
    /// `[dependencies]` and `[dev-dependencies]` read best side by side, so a
    /// new one goes next to the other when it exists; otherwise at the end of
    /// the file, since TOML does not care about table order.
    fn create_section(&self, section: &str, entry: String) -> String {
        let header = format!("[{section}]");
        let blank = String::new;
        match section {
            "dev-dependencies" => {
                if let Some(h) = self.plain_header("dependencies") {
                    let at = self.content_end(&Section {
                        header: h,
                        end: self.next_header(h + 1),
                    });
                    let mut insert = vec![blank(), header, entry];
                    if self
                        .lines
                        .get(at)
                        .is_some_and(|l| !matches!(l.kind, Kind::Blank))
                    {
                        insert.push(blank());
                    }
                    return self.splice(at, at, &insert);
                }
            }
            "dependencies" => {
                if let Some(h) = self.plain_header("dev-dependencies") {
                    let at = self.attached_comment_start(h);
                    let mut insert = Vec::new();
                    if at > 0 && !matches!(self.lines[at - 1].kind, Kind::Blank) {
                        insert.push(blank());
                    }
                    insert.extend([header, entry, blank()]);
                    return self.splice(at, at, &insert);
                }
            }
            _ => {}
        }
        // Trailing blank lines are dropped so exactly one separates the new
        // section from the content above it.
        let mut at = self.lines.len();
        while at > 0 && matches!(self.lines[at - 1].kind, Kind::Blank) {
            at -= 1;
        }
        let mut insert = if at > 0 { vec![blank()] } else { Vec::new() };
        insert.extend([header, entry]);
        self.splice(at, self.lines.len(), &insert)
    }

    fn plain_header(&self, name: &str) -> Option<usize> {
        self.lines.iter().position(|l| {
            matches!(&l.kind, Kind::Header { path: Some(p), array: false } if p.len() == 1 && p[0] == name)
        })
    }

    /// Replace lines `start..end` with `insert`, each ending in the file's
    /// line ending. Inserting after a last line that had no newline gives it
    /// one, so the file always ends in a newline after an insertion at EOF.
    fn splice(&self, start: usize, end: usize, insert: &[String]) -> String {
        let mut out = String::new();
        for line in &self.lines[..start] {
            out.push_str(line.text);
            out.push_str(line.ending);
        }
        if !insert.is_empty() && start > 0 && self.lines[start - 1].ending.is_empty() {
            out.push_str(self.eol);
        }
        for line in insert {
            out.push_str(line);
            out.push_str(self.eol);
        }
        for line in &self.lines[end..] {
            out.push_str(line.text);
            out.push_str(line.ending);
        }
        out
    }

    /// Swap the text of line `index`, keeping its original ending.
    fn replace(&self, index: usize, text: &str) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            out.push_str(if i == index { text } else { line.text });
            out.push_str(line.ending);
        }
        out
    }
}

// ---- lexing ----------------------------------------------------------------

/// State carried from one line to the next: how many arrays and inline tables
/// are open, and whether a multi-line string is. While either is true, a line
/// is part of an earlier value, whatever it looks like — `"g:a" = "1"` inside a
/// `"""` string, or `[1, 2],` inside a nested array, is not an entry or a header.
#[derive(Default)]
struct Lexer {
    depth: usize,
    string: Option<Multi>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Multi {
    Basic,
    Literal,
}

impl Lexer {
    fn inside_value(&self) -> bool {
        self.depth > 0 || self.string.is_some()
    }

    fn classify(&mut self, line: &str) -> Kind {
        if self.inside_value() {
            self.scan(line, 0);
            return Kind::Continuation;
        }
        let code = line.trim_start();
        let indent = line.len() - code.len();
        if code.is_empty() {
            return Kind::Blank;
        }
        if code.starts_with('#') {
            return Kind::Comment;
        }
        if code.starts_with('[') {
            return match parse_header(code) {
                Some((path, array)) => Kind::Header {
                    path: Some(path),
                    array,
                },
                None => Kind::Header {
                    path: None,
                    array: false,
                },
            };
        }
        let head = parse_pair_head(line, indent);
        let from = head
            .as_ref()
            .map_or(indent, |(_, _, value_start)| *value_start);
        let code_end = self.scan(line, from);
        let pair = head.map(|(path, quote, start)| Pair {
            path,
            quote,
            value: start..line[..code_end].trim_end().len().max(start),
        });
        Kind::Entry {
            pair,
            multi_line: self.inside_value(),
        }
    }

    /// Walk `line[from..]`, tracking strings and brackets, and return where
    /// the code ends: the byte offset of a comment's `#`, or the line length.
    /// Every delimiter is ASCII, so scanning bytes never splits a character.
    fn scan(&mut self, line: &str, from: usize) -> usize {
        let b = line.as_bytes();
        let mut i = from;
        while i < b.len() {
            if let Some(multi) = self.string {
                let delim = if multi == Multi::Basic { b'"' } else { b'\'' };
                if multi == Multi::Basic && b[i] == b'\\' {
                    i += 2;
                } else if b[i..].starts_with(&[delim; 3]) {
                    // Up to two quotes may sit right before the closing
                    // delimiter; the whole run ends the string.
                    while b.get(i) == Some(&delim) {
                        i += 1;
                    }
                    self.string = None;
                } else {
                    i += 1;
                }
                continue;
            }
            match b[i] {
                b'#' => return i,
                b'"' if b[i..].starts_with(b"\"\"\"") => {
                    self.string = Some(Multi::Basic);
                    i += 3;
                }
                b'\'' if b[i..].starts_with(b"'''") => {
                    self.string = Some(Multi::Literal);
                    i += 3;
                }
                b'"' => i = skip_basic(b, i),
                b'\'' => {
                    i = b[i + 1..]
                        .iter()
                        .position(|&c| c == b'\'')
                        .map_or(b.len(), |n| i + n + 2);
                }
                b'[' | b'{' => {
                    self.depth += 1;
                    i += 1;
                }
                b']' | b'}' => {
                    self.depth = self.depth.saturating_sub(1);
                    i += 1;
                }
                _ => i += 1,
            }
        }
        b.len()
    }
}

/// Past the single-line basic string opening at `at`; the line end if it
/// never closes.
fn skip_basic(b: &[u8], at: usize) -> usize {
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// `[a."b"]` or `[[a]]`, with nothing but a comment after it.
fn parse_header(code: &str) -> Option<(Vec<String>, bool)> {
    let (inner, array) = match code.strip_prefix("[[") {
        Some(inner) => (inner, true),
        None => (code.strip_prefix('[')?, false),
    };
    let (path, _, end) = parse_key_path(inner, 0)?;
    let rest = inner[end..].trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix(if array { "]]" } else { "]" })?;
    let rest = rest.trim_start_matches([' ', '\t']);
    (rest.is_empty() || rest.starts_with('#')).then_some((path, array))
}

/// The key path and `=` of a pair: the path, its first segment's quote, and
/// where the value starts.
fn parse_pair_head(line: &str, at: usize) -> Option<(Vec<String>, Option<u8>, usize)> {
    let (path, quote, end) = parse_key_path(line, at)?;
    let rest = line[end..]
        .trim_start_matches([' ', '\t'])
        .strip_prefix('=')?;
    let value_start = line.len() - rest.trim_start_matches([' ', '\t']).len();
    Some((path, quote, value_start))
}

/// A dotted key starting at `at`: its decoded segments, the quote character
/// of the first one, and the offset just past the last. Double- and single-
/// quoted spellings of a key decode to the same segment, so `"g:a"` and
/// `'g:a'` compare equal.
fn parse_key_path(s: &str, at: usize) -> Option<(Vec<String>, Option<u8>, usize)> {
    let b = s.as_bytes();
    let mut path = Vec::new();
    let mut quote = None;
    let mut i = at;
    loop {
        while matches!(b.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        let first = *b.get(i)?;
        let (segment, next) = match first {
            b'"' => parse_basic(s, i)?,
            b'\'' => {
                let close = s[i + 1..].find('\'')?;
                (s[i + 1..i + 1 + close].to_string(), i + close + 2)
            }
            _ => {
                let len = b[i..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'-')
                    .count();
                if len == 0 {
                    return None;
                }
                (s[i..i + len].to_string(), i + len)
            }
        };
        if path.is_empty() && matches!(first, b'"' | b'\'') {
            quote = Some(first);
        }
        path.push(segment);
        i = next;
        let mut j = i;
        while matches!(b.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        if b.get(j) == Some(&b'.') {
            i = j + 1;
        } else {
            return Some((path, quote, i));
        }
    }
}

/// Decode the basic string opening at `at`; returns it and the offset past
/// its closing quote.
fn parse_basic(s: &str, at: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = s[at + 1..].char_indices();
    while let Some((n, c)) = chars.next() {
        match c {
            '"' => return Some((out, at + 1 + n + 1)),
            '\\' => {
                let (_, escape) = chars.next()?;
                out.push(match escape {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'e' => '\u{1b}',
                    'u' | 'U' => {
                        let digits = if escape == 'u' { 4 } else { 8 };
                        let hex = (0..digits)
                            .map(|_| chars.next().map(|(_, h)| h))
                            .collect::<Option<String>>()?;
                        char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
                    }
                    _ => return None,
                });
            }
            _ => out.push(c),
        }
    }
    None
}

/// A key as TOML: single-quoted when the section's existing entries all are
/// (and the key allows it), double-quoted otherwise.
fn render_key(key: &str, single_quoted: bool) -> String {
    if single_quoted && !key.contains(|c: char| c == '\'' || c.is_control()) {
        return format!("'{key}'");
    }
    let mut s = String::from('"');
    for c in key.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(s, "\\u{:04X}", u32::from(c));
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::path::Path;

    const PROJECT: &str = "[project]\nname = \"app\"\nversion = \"1.0.0\"\n";

    /// Every edit must leave a file TOML can read.
    fn toml(text: &str) -> toml::Table {
        toml::from_str(text).unwrap_or_else(|e| panic!("not TOML: {e}\n---\n{text}"))
    }

    /// `(dependencies, dev-dependencies)` as `g:a:v`, through the real manifest parser.
    fn declared(text: &str) -> (Vec<String>, Vec<String>) {
        toml(text);
        let m = Manifest::parse(text, Path::new("/p/jrs.toml"), Path::new("/p"))
            .unwrap_or_else(|e| panic!("{e}\n---\n{text}"));
        let show = |deps: &[crate::manifest::Dependency]| -> Vec<String> {
            deps.iter().map(ToString::to_string).collect()
        };
        (show(&m.dependencies), show(&m.dev_dependencies))
    }

    fn added(edited: Edited) -> String {
        match edited {
            Edited::Added(text) => text,
            other => panic!("expected an insertion, got {other:?}"),
        }
    }

    fn replaced(edited: Edited) -> (String, String) {
        match edited {
            Edited::Replaced { text, previous } => (text, previous),
            other => panic!("expected a replacement, got {other:?}"),
        }
    }

    fn refused<T: std::fmt::Debug>(result: Result<T>) -> String {
        let msg = result.expect_err("the edit should be refused").to_string();
        assert!(msg.ends_with("jrs.toml by hand"), "{msg}");
        msg
    }

    // ---- insertion ---------------------------------------------------------

    #[test]
    fn an_insertion_keeps_every_comment_and_blank_line() {
        let before = r#"# my app
[project]
name = "app"
version = "1.0.0"

[dependencies]   # runtime
# logging
"org.slf4j:slf4j-api" = "2.0.13"  # pinned, see #42

"com.google.guava:guava" = "33.0.0-jre"


[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
"#;
        let after =
            added(upsert(before, "dependencies", "info.picocli:picocli", "\"4.7.6\"").unwrap());
        assert_eq!(
            after,
            r#"# my app
[project]
name = "app"
version = "1.0.0"

[dependencies]   # runtime
# logging
"org.slf4j:slf4j-api" = "2.0.13"  # pinned, see #42

"com.google.guava:guava" = "33.0.0-jre"
"info.picocli:picocli" = "4.7.6"


[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
"#
        );
        let (main, dev) = declared(&after);
        assert_eq!(
            main,
            [
                "org.slf4j:slf4j-api:2.0.13",
                "com.google.guava:guava:33.0.0-jre",
                "info.picocli:picocli:4.7.6"
            ]
        );
        assert_eq!(dev, ["org.junit.jupiter:junit-jupiter:5.10.2"]);
    }

    #[test]
    fn a_comment_above_the_next_header_stays_with_that_header() {
        let before = format!(
            "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\n# Test-only\n[dev-dependencies]\n"
        );
        let after = added(upsert(&before, "dependencies", "x.y:z", "\"2\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\"x.y:z\" = \"2\"\n\n\
                 # Test-only\n[dev-dependencies]\n"
            )
        );
        assert_eq!(declared(&after).0, ["a.b:c:1", "x.y:z:2"]);
    }

    #[test]
    fn an_empty_section_gets_the_entry_right_after_its_header() {
        let before = format!("{PROJECT}\n[dependencies]\n\n[dev-dependencies]\n");
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!("{PROJECT}\n[dependencies]\n\"g:a\" = \"1\"\n\n[dev-dependencies]\n")
        );
        assert_eq!(declared(&after).0, ["g:a:1"]);

        // A comment directly under the header describes the section, so the
        // entry goes below it.
        let before = format!("{PROJECT}\n[dependencies]\n# none yet\n");
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!("{PROJECT}\n[dependencies]\n# none yet\n\"g:a\" = \"1\"\n")
        );
    }

    #[test]
    fn indentation_of_existing_entries_is_copied() {
        let before = format!("{PROJECT}[dependencies]\n    \"a.b:c\" = \"1\"\n");
        let after = added(upsert(&before, "dependencies", "x.y:z", "\"2\"").unwrap());
        assert_eq!(
            after,
            format!("{PROJECT}[dependencies]\n    \"a.b:c\" = \"1\"\n    \"x.y:z\" = \"2\"\n")
        );
        assert_eq!(declared(&after).0, ["a.b:c:1", "x.y:z:2"]);
    }

    #[test]
    fn a_single_quoted_section_gets_a_single_quoted_key() {
        let before = format!("{PROJECT}[dependencies]\n'a.b:c' = '1'\n'd.e:f' = \"2\"\n");
        let after = added(upsert(&before, "dependencies", "x.y:z", "\"3\"").unwrap());
        assert!(
            after.ends_with("'d.e:f' = \"2\"\n'x.y:z' = \"3\"\n"),
            "{after}"
        );
        assert_eq!(declared(&after).0, ["a.b:c:1", "d.e:f:2", "x.y:z:3"]);

        // One double-quoted entry is enough to fall back to double quotes.
        let before = format!("{PROJECT}[dependencies]\n'a.b:c' = '1'\n\"d.e:f\" = \"2\"\n");
        let after = added(upsert(&before, "dependencies", "x.y:z", "\"3\"").unwrap());
        assert!(after.ends_with("\"x.y:z\" = \"3\"\n"), "{after}");
    }

    #[test]
    fn keys_needing_escapes_are_double_quoted_and_escaped() {
        let before = format!("{PROJECT}[dependencies]\n'a.b:c' = '1'\n");
        let after = added(upsert(&before, "dependencies", "we\"ird\\'key", "\"1\"").unwrap());
        assert!(
            after.ends_with("\"we\\\"ird\\\\'key\" = \"1\"\n"),
            "{after}"
        );
        let table = toml(&after);
        let deps = table["dependencies"].as_table().unwrap();
        assert_eq!(deps["we\"ird\\'key"].as_str(), Some("1"));
    }

    #[test]
    fn an_inline_table_value_is_written_as_given() {
        let before = format!("{PROJECT}[dependencies]\n\"a.b:c\" = \"1\"\n");
        let value = "{ version = \"1\", compile-only = true }";
        let after = added(upsert(&before, "dependencies", "x.y:z", value).unwrap());
        assert!(
            after.ends_with(&format!("\"x.y:z\" = {value}\n")),
            "{after}"
        );
        let table = toml(&after);
        let entry = table["dependencies"]["x.y:z"].as_table().unwrap();
        assert_eq!(entry["compile-only"].as_bool(), Some(true));
    }

    // ---- creating a section ------------------------------------------------

    #[test]
    fn a_missing_section_is_appended_after_one_blank_line() {
        let after = added(upsert(PROJECT, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!("{PROJECT}\n[dependencies]\n\"g:a\" = \"1\"\n")
        );
        assert_eq!(declared(&after).0, ["g:a:1"]);

        // Extra trailing blank lines collapse into the one separator.
        let before = format!("{PROJECT}\n\n\n");
        let again = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(again, after);

        // So does a missing final newline.
        let before = PROJECT.trim_end();
        let again = added(upsert(before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(again, after);
    }

    #[test]
    fn an_empty_file_gets_just_the_section() {
        let expected = "[dev-dependencies]\n\"g:a\" = \"1\"\n";
        for empty in ["", "\n", "  \n\n"] {
            let after = added(upsert(empty, "dev-dependencies", "g:a", "\"1\"").unwrap());
            assert_eq!(after, expected, "from {empty:?}");
        }
    }

    #[test]
    fn a_new_dev_dependencies_section_goes_right_after_dependencies() {
        let before = format!(
            "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\n\
             [repositories]\ninternal = \"https://repo.example.com/maven\"\n"
        );
        let after = added(upsert(&before, "dev-dependencies", "t.t:junit", "\"5\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\n\
                 [dev-dependencies]\n\"t.t:junit\" = \"5\"\n\n\
                 [repositories]\ninternal = \"https://repo.example.com/maven\"\n"
            )
        );
        assert_eq!(
            declared(&after),
            (vec!["a.b:c:1".into()], vec!["t.t:junit:5".into()])
        );

        // With the next header directly below, a blank line is added after too.
        let before = format!("{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n[repositories]\n");
        let after = added(upsert(&before, "dev-dependencies", "t.t:junit", "\"5\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\n\
                 [dev-dependencies]\n\"t.t:junit\" = \"5\"\n\n[repositories]\n"
            )
        );

        // And at the end of a file with no final newline.
        let before = format!("{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"");
        let after = added(upsert(&before, "dev-dependencies", "t.t:junit", "\"5\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\"\n\n\
                 [dev-dependencies]\n\"t.t:junit\" = \"5\"\n"
            )
        );
    }

    #[test]
    fn a_new_dependencies_section_goes_right_before_dev_dependencies() {
        let before = format!(
            "{PROJECT}\n# Test-only\n[dev-dependencies]\n\"t.t:junit\" = \"5\"\n\n\
             [repositories]\ninternal = \"https://repo.example.com/maven\"\n"
        );
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"g:a\" = \"1\"\n\n\
                 # Test-only\n[dev-dependencies]\n\"t.t:junit\" = \"5\"\n\n\
                 [repositories]\ninternal = \"https://repo.example.com/maven\"\n"
            )
        );
        assert_eq!(
            declared(&after),
            (vec!["g:a:1".into()], vec!["t.t:junit:5".into()])
        );

        let before = format!("{PROJECT}[dev-dependencies]\n\"t.t:junit\" = \"5\"\n");
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!(
                "{PROJECT}\n[dependencies]\n\"g:a\" = \"1\"\n\n\
                 [dev-dependencies]\n\"t.t:junit\" = \"5\"\n"
            )
        );
    }

    // ---- replacement -------------------------------------------------------

    #[test]
    fn replacing_keeps_the_key_quoting_spacing_and_comment() {
        let before =
            format!("{PROJECT}[dependencies]\n'g:a'   =  '1.0'   # pinned\n\"x.y:z\" = \"2\"\n");
        let (after, previous) =
            replaced(upsert(&before, "dependencies", "g:a", "\"1.1\"").unwrap());
        assert_eq!(previous, "'1.0'");
        assert_eq!(
            after,
            format!("{PROJECT}[dependencies]\n'g:a'   =  \"1.1\"   # pinned\n\"x.y:z\" = \"2\"\n")
        );
        assert_eq!(declared(&after).0, ["g:a:1.1", "x.y:z:2"]);
    }

    #[test]
    fn replacing_an_inline_table_reports_it_as_the_previous_value() {
        let before = format!("{PROJECT}[dependencies]\n\"g:a\" = {{ version = \"1\" }}\n");
        let edited = upsert(&before, "dependencies", "g:a", "\"2\"").unwrap();
        assert_eq!(
            edited.text(),
            format!("{PROJECT}[dependencies]\n\"g:a\" = \"2\"\n")
        );
        let (_, previous) = replaced(edited);
        assert_eq!(previous, "{ version = \"1\" }");
    }

    #[test]
    fn a_key_matches_however_it_is_quoted_or_escaped() {
        for spelling in ["\"g:a\"", "'g:a'", "\"g:\\u0061\"", "\"g\\u003Aa\""] {
            let before = format!("{PROJECT}[dependencies]\n{spelling} = \"1\"\n");
            let (after, previous) =
                replaced(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
            assert_eq!(previous, "\"1\"", "{spelling}");
            assert_eq!(
                after,
                format!("{PROJECT}[dependencies]\n{spelling} = \"2\"\n")
            );
            assert_eq!(declared(&after).0, ["g:a:2"]);
        }
        // Keys are compared exactly.
        let before = format!("{PROJECT}[dependencies]\n\"G:A\" = \"1\"\n");
        added(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
    }

    #[test]
    fn strings_can_hold_hashes_and_brackets() {
        let before = format!("{PROJECT}[dependencies]\n\"g:a\" = \"1#[{{\" # real comment\n");
        let (after, previous) = replaced(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(previous, "\"1#[{\"");
        assert_eq!(
            after,
            format!("{PROJECT}[dependencies]\n\"g:a\" = \"2\" # real comment\n")
        );
        // The brackets in the string opened nothing, so the next entry is
        // still an entry.
        let before = format!("{PROJECT}[dependencies]\n\"g:a\" = '1[' # [\n\"x.y:z\" = \"1\"\n");
        let (after, _) = replaced(upsert(&before, "dependencies", "x.y:z", "\"2\"").unwrap());
        assert_eq!(declared(&after).0, ["g:a:1[", "x.y:z:2"]);
    }

    #[test]
    fn a_header_with_a_trailing_comment_is_found() {
        let before = format!("{PROJECT}\n[dependencies]   # runtime deps\n\"g:a\" = \"1\"\n");
        let (after, _) = replaced(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(
            after,
            format!("{PROJECT}\n[dependencies]   # runtime deps\n\"g:a\" = \"2\"\n")
        );
        // Spacing and quoting inside the brackets do not matter either.
        let before = format!("{PROJECT}\n[ \"dependencies\" ]\n\"g:a\" = \"1\"\n");
        let (after, _) = replaced(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(declared(&after).0, ["g:a:2"]);
    }

    // ---- line endings ------------------------------------------------------

    #[test]
    fn crlf_line_endings_are_preserved() {
        let crlf = |s: &str| s.replace('\n', "\r\n");
        let base = format!("{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\" # c\n\"d.e:f\" = \"2\"\n");

        let after = added(upsert(&crlf(&base), "dependencies", "x.y:z", "\"3\"").unwrap());
        assert_eq!(
            after,
            crlf(&format!(
                "{PROJECT}\n[dependencies]\n\"a.b:c\" = \"1\" # c\n\"d.e:f\" = \"2\"\n\"x.y:z\" = \"3\"\n"
            ))
        );
        assert_eq!(declared(&after).0, ["a.b:c:1", "d.e:f:2", "x.y:z:3"]);

        let (after, previous) =
            replaced(upsert(&crlf(&base), "dependencies", "a.b:c", "\"9\"").unwrap());
        assert_eq!(previous, "\"1\"");
        assert_eq!(after, crlf(&base.replace("\"1\" # c", "\"9\" # c")));

        let after = remove(&crlf(&base), "dependencies", "a.b:c")
            .unwrap()
            .unwrap();
        assert_eq!(after, crlf(&base.replace("\"a.b:c\" = \"1\" # c\n", "")));

        let after = added(upsert(&crlf(&base), "dev-dependencies", "t.t:j", "\"5\"").unwrap());
        assert_eq!(
            after,
            crlf(&format!("{base}\n[dev-dependencies]\n\"t.t:j\" = \"5\"\n"))
        );
    }

    #[test]
    fn a_file_without_a_final_newline() {
        let before = format!("{PROJECT}[dependencies]\n\"a.b:c\" = \"1\"");

        // An insertion at the end of the file terminates both lines.
        let after = added(upsert(&before, "dependencies", "x.y:z", "\"2\"").unwrap());
        assert_eq!(after, format!("{before}\n\"x.y:z\" = \"2\"\n"));

        // A replacement leaves the missing newline missing.
        let (after, _) = replaced(upsert(&before, "dependencies", "a.b:c", "\"3\"").unwrap());
        assert_eq!(after, format!("{PROJECT}[dependencies]\n\"a.b:c\" = \"3\""));

        let after = remove(&before, "dependencies", "a.b:c").unwrap().unwrap();
        assert_eq!(after, format!("{PROJECT}[dependencies]\n"));
    }

    // ---- removal -----------------------------------------------------------

    const THREE: &str = "[dependencies] # all of them\n\
                         \"a.b:c\" = \"1\"\n\
                         # the middle one\n\
                         \"d.e:f\" = \"2\" # why\n\
                         \"g.h:i\" = \"3\"\n\
                         \n\
                         [dev-dependencies]\n\
                         \"d.e:f\" = \"2\"\n";

    #[test]
    fn removing_the_middle_entry_leaves_its_neighbours_and_comments() {
        let before = format!("{PROJECT}{THREE}");
        let after = remove(&before, "dependencies", "d.e:f").unwrap().unwrap();
        assert_eq!(
            after,
            format!(
                "{PROJECT}[dependencies] # all of them\n\"a.b:c\" = \"1\"\n# the middle one\n\
                 \"g.h:i\" = \"3\"\n\n[dev-dependencies]\n\"d.e:f\" = \"2\"\n"
            )
        );
        assert_eq!(
            declared(&after),
            (
                vec!["a.b:c:1".into(), "g.h:i:3".into()],
                vec!["d.e:f:2".into()]
            )
        );
    }

    #[test]
    fn removing_the_last_entry() {
        let before = format!("{PROJECT}{THREE}");
        let after = remove(&before, "dependencies", "g.h:i").unwrap().unwrap();
        assert_eq!(after, before.replace("\"g.h:i\" = \"3\"\n", ""));
        assert_eq!(declared(&after).0, ["a.b:c:1", "d.e:f:2"]);

        // The dev-dependency of the same name is a different entry.
        let after = remove(&before, "dev-dependencies", "d.e:f")
            .unwrap()
            .unwrap();
        assert_eq!(
            after,
            format!("{PROJECT}{}", THREE.trim_end_matches("\"d.e:f\" = \"2\"\n"))
        );
        assert_eq!(declared(&after).1, Vec::<String>::new());
    }

    #[test]
    fn removing_the_only_entry_keeps_the_header() {
        let before = format!("{PROJECT}\n[dependencies]\n\"g:a\" = \"1\"\n\n[java]\nsource = 21\n");
        let after = remove(&before, "dependencies", "g:a").unwrap().unwrap();
        assert_eq!(
            after,
            format!("{PROJECT}\n[dependencies]\n\n[java]\nsource = 21\n")
        );
        assert_eq!(declared(&after).0, Vec::<String>::new());
    }

    #[test]
    fn removing_an_absent_key_is_none() {
        let before = format!("{PROJECT}{THREE}");
        assert_eq!(remove(&before, "dependencies", "x.y:z").unwrap(), None);
        assert_eq!(remove(&before, "dependencies", "D.E:F").unwrap(), None);
        assert_eq!(remove(PROJECT, "dependencies", "g:a").unwrap(), None);
        assert_eq!(remove("", "dev-dependencies", "g:a").unwrap(), None);
    }

    // ---- what is not the section or not an entry ---------------------------

    #[test]
    fn a_dotted_sub_table_header_is_not_the_section() {
        let before = format!("{PROJECT}\n[dependencies.extra]\nnote = \"x\"\n");
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!("{before}\n[dependencies]\n\"g:a\" = \"1\"\n")
        );
        toml(&after);
        assert_eq!(remove(&before, "dependencies", "note").unwrap(), None);
    }

    #[test]
    fn dev_dependencies_is_not_dependencies() {
        let before = format!("{PROJECT}\n[dev-dependencies]\n\"g:a\" = \"1\"\n");
        assert_eq!(remove(&before, "dependencies", "g:a").unwrap(), None);
        let after = added(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(
            declared(&after),
            (vec!["g:a:2".into()], vec!["g:a:1".into()])
        );
    }

    #[test]
    fn nothing_inside_a_multi_line_string_is_a_header_or_an_entry() {
        let before =
            format!("{PROJECT}description = \"\"\"\n[dependencies]\n\"g:a\" = \"0.1\"\n\"\"\"\n");
        assert_eq!(remove(&before, "dependencies", "g:a").unwrap(), None);
        let after = added(upsert(&before, "dependencies", "g:a", "\"1\"").unwrap());
        assert_eq!(
            after,
            format!("{before}\n[dependencies]\n\"g:a\" = \"1\"\n")
        );
        assert_eq!(declared(&after).0, ["g:a:1"]);

        // A multi-line literal string as a dependency's own value: its
        // content looks like an entry and must not be taken for one.
        let before = format!("{PROJECT}[dependencies]\n\"x.y:z\" = '''\n\"g:a\" = \"1\"\n'''\n");
        let after = added(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(after, format!("{before}\"g:a\" = \"2\"\n"));
        let table = toml(&after);
        assert_eq!(table["dependencies"]["g:a"].as_str(), Some("2"));
    }

    #[test]
    fn lines_of_a_multi_line_array_belong_to_their_entry() {
        let before = format!(
            "{PROJECT}[dependencies]\n\
             \"x.y:z\" = {{ version = \"1\", exclusions = [\n    \"g:a\", # [\n] }}\n\
             \"a.b:c\" = \"1\"\n"
        );
        toml(&before);

        // `"g:a",` is an array element, not an entry.
        let after = added(upsert(&before, "dependencies", "g:a", "\"2\"").unwrap());
        assert_eq!(after, format!("{before}\"g:a\" = \"2\"\n"));
        toml(&after);

        // The entry after the array is found normally.
        let (after, _) = replaced(upsert(&before, "dependencies", "a.b:c", "\"2\"").unwrap());
        toml(&after);

        // The multi-line entry itself cannot be replaced...
        let msg = refused(upsert(&before, "dependencies", "x.y:z", "\"2\""));
        assert!(msg.contains("`dependencies.\"x.y:z\"`"), "{msg}");
        assert!(msg.contains("spanning several lines"), "{msg}");

        // ...but can be removed, all of its lines together.
        let after = remove(&before, "dependencies", "x.y:z").unwrap().unwrap();
        assert_eq!(
            after,
            format!("{PROJECT}[dependencies]\n\"a.b:c\" = \"1\"\n")
        );
        assert_eq!(declared(&after).0, ["a.b:c:1"]);
    }

    // ---- refusals ----------------------------------------------------------

    #[test]
    fn a_dependency_declared_as_a_sub_table_is_refused() {
        for header in ["[dependencies.\"g:a\"]", "[dependencies . 'g:a']  # pinned"] {
            let before = format!(
                "{PROJECT}\n[dependencies]\n\"x.y:z\" = \"1\"\n\n{header}\nversion = \"1\"\n"
            );
            toml(&before);

            let msg = refused(remove(&before, "dependencies", "g:a"));
            assert_eq!(
                msg,
                "`dependencies.\"g:a\"` is declared as a table; remove it from jrs.toml by hand"
            );
            let msg = refused(upsert(&before, "dependencies", "g:a", "\"2\""));
            assert_eq!(
                msg,
                "`dependencies.\"g:a\"` is declared as a table; edit it in jrs.toml by hand"
            );

            // Other entries of the section are still editable.
            let after = remove(&before, "dependencies", "x.y:z").unwrap().unwrap();
            toml(&after);
        }
    }

    #[test]
    fn a_dependency_declared_as_a_dotted_key_is_refused() {
        let before = format!("{PROJECT}[dependencies]\n\"g:a\".version = \"1\"\n");
        toml(&before);
        let msg = refused(upsert(&before, "dependencies", "g:a", "\"2\""));
        assert!(msg.contains("declared as a table"), "{msg}");
        let msg = refused(remove(&before, "dependencies", "g:a"));
        assert!(msg.contains("declared as a table"), "{msg}");
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let before = format!("{PROJECT}[dependencies]\n\"g:a\" = \"1\"\n'g:a' = \"2\"\n");
        let msg = refused(upsert(&before, "dependencies", "g:a", "\"3\""));
        assert!(
            msg.contains("`dependencies.\"g:a\"` is declared more than once"),
            "{msg}"
        );
        let msg = refused(remove(&before, "dependencies", "g:a"));
        assert!(msg.contains("more than once"), "{msg}");
    }

    #[test]
    fn a_section_declared_twice_is_refused() {
        let before = format!("{PROJECT}[dependencies]\n\"a:b\" = \"1\"\n[dependencies]\n");
        let msg = refused(upsert(&before, "dependencies", "g:a", "\"3\""));
        assert!(
            msg.contains("`[dependencies]` is declared more than once"),
            "{msg}"
        );
    }

    #[test]
    fn sections_this_editor_cannot_extend_are_refused() {
        // Appending `[dependencies]` to either would define the table twice.
        let inline = format!("dependencies = {{ \"x.y:z\" = \"1\" }}\n{PROJECT}");
        let dotted = format!("dependencies.\"x.y:z\" = \"1\"\n{PROJECT}");
        let array = format!("{PROJECT}[[dependencies]]\nname = \"x\"\n");
        for before in [inline, dotted, array] {
            toml(&before);
            let msg = refused(upsert(&before, "dependencies", "g:a", "\"1\""));
            assert!(msg.starts_with("`dependencies.\"g:a\"`"), "{msg}");
            refused(remove(&before, "dependencies", "g:a"));
        }
    }

    #[test]
    fn a_value_spanning_lines_is_refused_before_it_is_written() {
        let before = format!("{PROJECT}[dependencies]\n");
        let msg = refused(upsert(&before, "dependencies", "g:a", "[\n\"1\"]"));
        assert!(msg.contains("spanning several lines"), "{msg}");
    }
}
