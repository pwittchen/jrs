//! Version variables in a Gradle build (SPEC §11.3).
//!
//! A build that keeps a version in a variable upgrades a family of artifacts
//! with one line. A manifest has no variables, so migration writes the value
//! into each declaration, and a review line names the variable it came from.
//!
//! Only a value jrs can know without running Gradle is used: a variable
//! assigned exactly once, to a string literal or to another variable that is,
//! outside a conditional, a loop or any block jrs does not run. The script's
//! `ext`, `def`, `val` and `extra` assignments are read, and so is the
//! `gradle.properties` beside it. Anything else is reported with the
//! variable's name, and the declaration is left out rather than written
//! without its version.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Report;

/// Why a value could not be read, naming the variable, for the report.
pub(super) type Unread = String;

/// The blocks whose assignments are extra properties: `ext { }` and its
/// spellings.
const EXT_BLOCKS: &[&str] = &[
    "ext",
    "project.ext",
    "rootProject.ext",
    "extra.apply",
    "project.extra.apply",
    "rootProject.extra.apply",
];

/// What a variable's name may be prefixed with: `project.ext.x` is `x`. In a
/// single-module build the root project is the project.
const OWNERS: &[&str] = &["rootProject.", "project.", ""];

/// What one assignment gives a variable.
#[derive(Debug, Clone)]
enum Value {
    Literal(String),
    /// `"$other"`, `"${other}"`, `other`: another variable's value.
    Alias(String),
    /// Anything else, as written: concatenation, a method call, `System.getenv`.
    Expression(String),
}

#[derive(Debug, Clone)]
struct Assignment {
    value: Value,
    /// Made inside a conditional, a loop, or a block jrs does not run.
    guarded: bool,
}

/// What a variable's value was written into.
#[derive(Debug, Default)]
struct Uses {
    dependencies: usize,
    plugins: Vec<String>,
}

/// The build's variables, and which of them migration took a version from.
#[derive(Debug, Default)]
pub(super) struct Variables {
    assignments: BTreeMap<String, Vec<Assignment>>,
    /// The Kotlin DSL, where `"$a.b"` is `a` followed by `.b`.
    kotlin: bool,
    /// Whether the script has an `ext { }` block, the names it assigns, in
    /// order, and whether it holds anything but assignments.
    ext_block: bool,
    ext_names: Vec<String>,
    ext_opaque: bool,
    /// By the name the declaration used.
    uses: BTreeMap<String, Uses>,
    /// Every variable a used one was followed through.
    used: BTreeSet<String>,
}

impl Variables {
    /// The variables `script` assigns, and the properties of the
    /// `gradle.properties` in `root`. `kotlin` is the Kotlin DSL.
    pub(super) fn read(script: &str, root: &Path, kotlin: bool) -> Variables {
        let mut vars = Variables {
            kotlin,
            ..Variables::default()
        };
        if let Ok(text) = std::fs::read_to_string(root.join("gradle.properties")) {
            vars.read_properties(&text);
        }
        vars.read_script(script);
        vars
    }

    fn assign(&mut self, name: &str, value: Value, guarded: bool) {
        self.assignments
            .entry(name.to_string())
            .or_default()
            .push(Assignment { value, guarded });
    }

    /// `key=value`, `key: value` or `key value`, as `java.util.Properties`
    /// reads them. A value continued on the next line is not read.
    fn read_properties(&mut self, text: &str) {
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with(['#', '!']) {
                continue;
            }
            let at = line.find(['=', ':', ' ', '\t']).unwrap_or(line.len());
            let key = line[..at].trim();
            let value = line[at..]
                .trim_start()
                .strip_prefix(['=', ':'])
                .unwrap_or(&line[at..])
                .trim();
            let value = if value.ends_with('\\') {
                Value::Expression(value.to_string())
            } else {
                Value::Literal(value.to_string())
            };
            if !key.is_empty() {
                self.assign(key, value, false);
            }
        }
    }

    fn read_script(&mut self, script: &str) {
        // The headers of the blocks the scan is inside, outermost first.
        let mut blocks: Vec<String> = Vec::new();
        for piece in pieces(script) {
            let guarded = !blocks
                .iter()
                .all(|b| compact(b) == "buildscript" || is_ext_block(b));
            let in_ext = blocks.last().is_some_and(|b| is_ext_block(b));
            match piece {
                Piece::Open(header) => {
                    if in_ext {
                        self.ext_opaque = true;
                    }
                    if is_ext_block(&header) {
                        self.ext_block = true;
                    }
                    // `val x = list.map { ... }`: an assignment of nothing jrs
                    // can read.
                    if let Some((name, _)) = assignment(&header, in_ext) {
                        let value = Value::Expression(format!("{header} {{ … }}"));
                        self.assign(&name, value, guarded);
                    }
                    blocks.push(header);
                }
                Piece::Close => {
                    blocks.pop();
                }
                Piece::Statement(statement) => {
                    let (statement, conditional) = unguard(&statement);
                    match assignment(statement, in_ext) {
                        Some((name, expression)) => {
                            if in_ext && !self.ext_names.contains(&name) {
                                self.ext_names.push(name.clone());
                            }
                            let value = self.value(&expression);
                            self.assign(&name, value, guarded || conditional);
                        }
                        None if in_ext => self.ext_opaque = true,
                        None => {}
                    }
                }
            }
        }
    }

    /// What the right-hand side of an assignment gives its variable.
    fn value(&self, expression: &str) -> Value {
        let expression = expression.trim().trim_end_matches(';').trim();
        if let Some((quote, text)) = whole_literal(expression) {
            if quote != '"' || !text.contains('$') {
                return Value::Literal(text);
            }
            let chars: Vec<char> = text.chars().collect();
            return match interpolation(&chars, 0, self.kotlin) {
                Some((inner, next)) if next == chars.len() => reference(&inner)
                    .map_or_else(|| Value::Expression(expression.to_string()), Value::Alias),
                _ => Value::Expression(expression.to_string()),
            };
        }
        reference(expression)
            .map_or_else(|| Value::Expression(expression.to_string()), Value::Alias)
    }

    /// A variable's value, and the variables it was followed through, from
    /// `name` to the one assigned the literal.
    fn resolve(&self, name: &str) -> Result<(String, Vec<String>), Unread> {
        let mut chain: Vec<String> = Vec::new();
        let mut current = name.to_string();
        let why = loop {
            if chain.contains(&current) {
                break format!("`{current}` refers back to itself");
            }
            chain.push(current.clone());
            match self.assignments.get(&current).map(Vec::as_slice) {
                None | Some([]) => {
                    break format!(
                        "`{current}` is set neither in the build script nor in gradle.properties"
                    );
                }
                Some([one]) if one.guarded => {
                    break format!(
                        "`{current}` is set inside a conditional, a loop or a block jrs does not run"
                    );
                }
                Some([one]) => match &one.value {
                    Value::Literal(v) if v.trim().is_empty() => {
                        break format!("`{current}` is empty");
                    }
                    Value::Literal(v) => return Ok((v.clone(), chain)),
                    Value::Alias(next) => current.clone_from(next),
                    Value::Expression(e) => {
                        break format!("`{current}` is `{e}`, not a string literal");
                    }
                },
                Some(_) => break format!("`{current}` is assigned more than once"),
            }
        };
        Err(match chain.last() {
            Some(last) if chain.len() > 1 => {
                format!("`{name}` takes its value from `{last}`, and {why}")
            }
            _ => why,
        })
    }

    /// A double-quoted literal's `text` with each `$name` and `${...}` in it
    /// replaced by the variable's value, and the variables it named.
    pub(super) fn interpolate(&self, text: &str) -> Result<(String, Vec<String>), Unread> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut names = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' if i + 1 < chars.len() => {
                    out.push(chars[i + 1]);
                    i += 2;
                }
                '$' => {
                    let Some((expression, next)) = interpolation(&chars, i, self.kotlin) else {
                        out.push('$');
                        i += 1;
                        continue;
                    };
                    let name = reference(&expression).ok_or_else(|| {
                        format!("`{expression}` is an expression, which jrs does not evaluate")
                    })?;
                    out.push_str(&self.resolve(&name)?.0);
                    names.push(name);
                    i = next;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        Ok((out, names))
    }

    /// A value as a declaration writes it: a string literal, interpolated
    /// when double-quoted, or a variable by name. The value, and the
    /// variables it named.
    pub(super) fn evaluate(&self, expression: &str) -> Result<(String, Vec<String>), Unread> {
        let expression = expression.trim();
        if let Some((quote, text)) = whole_literal(expression) {
            return if quote == '"' {
                self.interpolate(&text)
            } else {
                Ok((text, Vec::new()))
            };
        }
        match reference(expression) {
            Some(name) => Ok((self.resolve(&name)?.0, vec![name])),
            None => Err(format!(
                "`{expression}` is an expression, which jrs does not evaluate"
            )),
        }
    }

    /// Note that a dependency took a value from each of `names`.
    pub(super) fn used_by_dependency(&mut self, names: &[String]) {
        for name in names {
            self.uses.entry(name.clone()).or_default().dependencies += 1;
            self.follow(name);
        }
    }

    /// Note that plugin `id` took its version from `names`.
    pub(super) fn used_by_plugin(&mut self, names: &[String], id: &str) {
        for name in names {
            self.uses
                .entry(name.clone())
                .or_default()
                .plugins
                .push(id.to_string());
            self.follow(name);
        }
    }

    fn follow(&mut self, name: &str) {
        if let Ok((_, chain)) = self.resolve(name) {
            self.used.extend(chain);
        }
    }

    /// A review line for each variable a value was taken from, since the
    /// manifest no longer shows what moved together; and the `ext { }` block,
    /// unless everything in it was used.
    pub(super) fn report(&self, report: &mut Report) {
        for (name, uses) in &self.uses {
            let Ok((value, chain)) = self.resolve(name) else {
                continue;
            };
            let from = match chain.last() {
                Some(last) if chain.len() > 1 => format!(" (from `{last}`)"),
                _ => String::new(),
            };
            let mut into = Vec::new();
            match uses.dependencies {
                0 => {}
                1 => into.push("1 dependency".to_string()),
                n => into.push(format!("{n} dependencies")),
            }
            into.extend(uses.plugins.iter().map(|id| format!("plugin `{id}`")));
            let together = if uses.dependencies + uses.plugins.len() > 1 {
                " — the manifest writes the version into each, so they no longer move together"
            } else {
                ""
            };
            report.review(format!(
                "{name} = {value}{from} → {}{together}",
                into.join(" and ")
            ));
        }

        if !self.ext_block {
            return;
        }
        let unused: Vec<String> = self
            .ext_names
            .iter()
            .filter(|n| !self.used.contains(*n))
            .map(|n| format!("`{n}`"))
            .collect();
        match unused.as_slice() {
            [] if self.ext_opaque => report.skipped(
                "`ext { }` — it holds more than variables set to literals, which jrs cannot \
                 evaluate",
            ),
            [] => {}
            [one] => report.skipped(format!(
                "`ext {{ }}` — {one} is not used by anything jrs migrated"
            )),
            many => report.skipped(format!(
                "`ext {{ }}` — {} are not used by anything jrs migrated",
                many.join(", ")
            )),
        }
    }
}

/// One piece of a script: a block's header as it opens, its closing brace,
/// or a statement.
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Open(String),
    Close,
    Statement(String),
}

/// `script` cut into statements and the blocks around them, at braces, `;`
/// and line ends outside string literals.
fn pieces(script: &str) -> Vec<Piece> {
    fn flush(current: &mut String, out: &mut Vec<Piece>) {
        let statement = current.trim();
        if !statement.is_empty() {
            out.push(Piece::Statement(statement.to_string()));
        }
        current.clear();
    }
    let chars: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' | '"' => {
                let next = string_span(&chars, i).next;
                current.extend(&chars[i..next]);
                i = next;
                continue;
            }
            '{' => {
                out.push(Piece::Open(current.trim().to_string()));
                current.clear();
            }
            '}' => {
                flush(&mut current, &mut out);
                out.push(Piece::Close);
            }
            ';' | '\n' => flush(&mut current, &mut out),
            c => current.push(c),
        }
        i += 1;
    }
    flush(&mut current, &mut out);
    out
}

/// The statements and block headers in `text`, split at braces and `;`
/// outside string literals: `imports { mavenBom "g:a:${v}" }` is `imports`
/// and `mavenBom "g:a:${v}"`.
pub(super) fn statements(text: &str) -> Vec<String> {
    pieces(text)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Open(s) | Piece::Statement(s) if !s.is_empty() => Some(s),
            _ => None,
        })
        .collect()
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn is_ext_block(header: &str) -> bool {
    EXT_BLOCKS.contains(&compact(header).as_str())
}

/// A statement behind an `if (...)`, `else`, `for (...)` or `while (...)` on
/// its own line, and whether it had one.
fn unguard(statement: &str) -> (&str, bool) {
    let s = statement.trim();
    for keyword in ["if", "for", "while"] {
        let Some(rest) = s.strip_prefix(keyword) else {
            continue;
        };
        let rest = rest.trim_start();
        if !rest.starts_with('(') {
            continue;
        }
        let mut depth = 0usize;
        for (at, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return (unguard(&rest[at + 1..]).0, true);
                    }
                }
                _ => {}
            }
        }
        return ("", true);
    }
    match s.strip_prefix("else") {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
            (unguard(rest).0, true)
        }
        _ => (s, false),
    }
}

/// The variable `statement` assigns, and the expression it assigns to it:
/// `def x = ...`, `val x: String = ...`, `val x by extra(...)`, `ext.x = ...`,
/// `extra["x"] = ...`, `set('x', ...)` in an `ext { }` block (`in_ext`), or
/// a bare `x = ...`. `x += ...` assigns an expression.
fn assignment(statement: &str, in_ext: bool) -> Option<(String, String)> {
    let s = statement.trim().trim_end_matches(';').trim();

    // Declarations: Groovy's `def` or a type, Kotlin's `val` and `var`.
    for keyword in ["def ", "val ", "var ", "String "] {
        let Some(rest) = s.strip_prefix(keyword) else {
            continue;
        };
        let (name, rest) = identifier(rest.trim_start())?;
        let mut rest = rest.trim_start();
        if let Some(typed) = rest.strip_prefix(':') {
            let typed = typed.trim_start();
            let end = typed
                .find(|c: char| c.is_whitespace() || c == '=')
                .unwrap_or(typed.len());
            rest = typed[end..].trim_start();
        }
        if let Some(delegate) = rest.strip_prefix("by ") {
            // `by extra("1.2")` assigns; `by project` and `by extra` read a
            // value set elsewhere under the same name.
            let argument = delegate
                .trim()
                .strip_prefix("extra(")
                .and_then(|d| d.strip_suffix(')'))?;
            return Some((name, argument.to_string()));
        }
        return Some((name, right_hand(rest)?.to_string()));
    }

    // Extra properties: `ext.x = ...`, `ext["x"] = ...`, `extra["x"] = ...`,
    // `ext.set("x", ...)`, with or without `project.` before them.
    for owner in OWNERS {
        let Some(rest) = s.strip_prefix(owner) else {
            continue;
        };
        if let Some(args) = rest
            .strip_prefix("ext.set(")
            .or_else(|| rest.strip_prefix("extra.set("))
        {
            return set_call(args);
        }
        if let Some(rest) = rest.strip_prefix("ext.") {
            let (name, rest) = identifier(rest)?;
            return Some((name, right_hand(rest)?.to_string()));
        }
        for index in ["ext[", "extra["] {
            if let Some(rest) = rest.strip_prefix(index) {
                let (key, rest) = key_literal(rest.trim_start())?;
                let rest = rest.trim_start().strip_prefix(']')?;
                return Some((key, right_hand(rest)?.to_string()));
            }
        }
    }
    if in_ext && let Some(args) = s.strip_prefix("set(") {
        return set_call(args);
    }

    // A bare `x = ...`: a property in `ext { }`, a script variable at the top.
    let (name, rest) = identifier(s)?;
    if let Some(value) = right_hand(rest) {
        return Some((name, value.to_string()));
    }
    let rest = rest.trim_start();
    ["+=", "-=", "*=", "/=", "?="]
        .iter()
        .any(|op| rest.starts_with(op))
        .then(|| (name, s.to_string()))
}

/// `'x', '1.2')`, the rest of a `set(` call.
fn set_call(args: &str) -> Option<(String, String)> {
    let (key, rest) = key_literal(args.trim_start())?;
    let value = rest.trim_start().strip_prefix(',')?.trim();
    Some((key, value.strip_suffix(')')?.trim().to_string()))
}

/// What follows the `=` of an assignment; `None` for `==` or no `=`.
fn right_hand(rest: &str) -> Option<&str> {
    let value = rest.trim_start().strip_prefix('=')?;
    (!value.starts_with('=')).then(|| value.trim())
}

/// A name at the start of `text`, and what follows it.
fn identifier(text: &str) -> Option<(String, &str)> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    let name = &text[..end];
    let first = name.chars().next()?;
    (first.is_alphabetic() || first == '_').then(|| (name.to_string(), &text[end..]))
}

/// A property's key, a string literal without interpolation at the start of
/// `text`, and what follows its closing quote.
fn key_literal(text: &str) -> Option<(String, &str)> {
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let body = &text[1..];
    let end = body.find(quote)?;
    let key = &body[..end];
    (!key.is_empty() && !key.contains('$')).then(|| (key.to_string(), &body[end + 1..]))
}

/// The variable an expression names: `x`, `project.x`, `ext.x`,
/// `rootProject.ext.x`, `property("x")`, `extra["x"]`, with `as String` or
/// without.
fn reference(expression: &str) -> Option<String> {
    let e = expression.trim();
    let e = e.strip_suffix(" as String").unwrap_or(e).trim();
    for owner in OWNERS {
        let Some(rest) = e.strip_prefix(owner) else {
            continue;
        };
        for (open, close) in [
            ("property(", ")"),
            ("ext[", "]"),
            ("extra[", "]"),
            ("ext.get(", ")"),
            ("extra.get(", ")"),
        ] {
            if let Some(inner) = rest.strip_prefix(open).and_then(|r| r.strip_suffix(close)) {
                let (key, tail) = key_literal(inner.trim())?;
                return tail.trim().is_empty().then_some(key);
            }
        }
        let rest = rest.strip_prefix("ext.").unwrap_or(rest);
        if let Some((name, tail)) = identifier(rest)
            && tail.is_empty()
            && !matches!(name.as_str(), "true" | "false" | "null" | "it" | "this")
        {
            return Some(name);
        }
    }
    None
}

/// The expression a `$` at `at` interpolates, and the index past it:
/// `${expression}`, or `$name` — which in Groovy runs on over `.name`.
fn interpolation(chars: &[char], at: usize, kotlin: bool) -> Option<(String, usize)> {
    if chars.get(at) != Some(&'$') {
        return None;
    }
    if chars.get(at + 1) == Some(&'{') {
        let end = braced_end(chars, at + 1);
        if chars.get(end.checked_sub(1)?) != Some(&'}') {
            return None;
        }
        return Some((chars[at + 2..end - 1].iter().collect(), end));
    }
    let starts_name = |c: &char| c.is_alphabetic() || *c == '_';
    if !chars.get(at + 1).is_some_and(starts_name) {
        return None;
    }
    let mut end = at + 1;
    while chars
        .get(end)
        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
        || (!kotlin && chars.get(end) == Some(&'.') && chars.get(end + 1).is_some_and(starts_name))
    {
        end += 1;
    }
    Some((chars[at + 1..end].iter().collect(), end))
}

/// Where a string literal's contents start and end, the index past its
/// closing quote, and whether it had one.
struct Span {
    start: usize,
    end: usize,
    next: usize,
}

/// The string literal whose quote is at `open`: `'...'`, `"..."` with any
/// `${...}` in it, and the triple-quoted forms. One left open ends at the end
/// of its line.
fn string_span(chars: &[char], open: usize) -> Span {
    let quote = chars[open];
    let triple = chars.get(open + 1) == Some(&quote) && chars.get(open + 2) == Some(&quote);
    let width = if triple { 3 } else { 1 };
    let start = open + width;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '$' if quote == '"' && chars.get(i + 1) == Some(&'{') => i = braced_end(chars, i + 1),
            '\n' if !triple => break,
            c if c == quote
                && (!triple
                    || (chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote))) =>
            {
                return Span {
                    start,
                    end: i,
                    next: i + width,
                };
            }
            _ => i += 1,
        }
    }
    let end = i.min(chars.len());
    Span {
        start: start.min(end),
        end,
        next: end.max(open + 1),
    }
}

/// The index past the `}` closing the `{` at `open`, string literals inside
/// it skipped; the line's end when there is none.
fn braced_end(chars: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '\'' | '"' => {
                i = string_span(chars, i).next;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            '\n' => return i,
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

/// Every string literal in `text`, in order, with the quote that opened it.
/// Unlike [`super::gradle::quoted`], a `"` inside `${...}` stays inside:
/// `"g:a:${property("v")}"` is one literal.
pub(super) fn literals(text: &str) -> Vec<(char, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\'' | '"') {
            let span = string_span(&chars, i);
            out.push((chars[i], chars[span.start..span.end].iter().collect()));
            i = span.next;
        } else {
            i += 1;
        }
    }
    out
}

/// `text` when it is one string literal, whole: its quote and contents.
fn whole_literal(text: &str) -> Option<(char, String)> {
    let chars: Vec<char> = text.chars().collect();
    let quote = *chars.first().filter(|c| matches!(c, '\'' | '"'))?;
    let span = string_span(&chars, 0);
    let closed = span.next > span.end;
    (closed && span.next == chars.len())
        .then(|| (quote, chars[span.start..span.end].iter().collect()))
}

/// The value an expression starts with: a string literal, quotes and all, or
/// a name — with `property("x")`'s brackets — up to a space, a comma or a
/// closing bracket.
pub(super) fn leading_value(text: &str) -> String {
    let chars: Vec<char> = text.trim_start().chars().collect();
    let end = if matches!(chars.first(), Some('\'' | '"')) {
        string_span(&chars, 0).next
    } else {
        let (mut depth, mut i) = (0usize, 0);
        while i < chars.len() {
            match chars[i] {
                '\'' | '"' => {
                    i = string_span(&chars, i).next;
                    continue;
                }
                '(' | '[' => depth += 1,
                ')' | ']' | ',' if depth == 0 => break,
                ')' | ']' => depth -= 1,
                c if c.is_whitespace() && depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        i
    };
    chars[..end.min(chars.len())].iter().collect()
}

/// The value of `key: value` or `key = value` in an argument list — map
/// notation, or the Kotlin DSL's named arguments — as written, up to the next
/// comma or closing bracket outside string literals.
pub(super) fn map_field(line: &str, key: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let key: Vec<char> = key.chars().collect();
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\'' | '"') {
            i = string_span(&chars, i).next;
            continue;
        }
        if !chars[i..].starts_with(&key) || (i > 0 && is_name(chars[i - 1])) {
            i += 1;
            continue;
        }
        let mut j = i + key.len();
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        let separated = match chars.get(j) {
            Some(':') => true,
            Some('=') => chars.get(j + 1) != Some(&'='),
            _ => false,
        };
        if !separated {
            i += 1;
            continue;
        }
        let start = j + 1;
        let (mut depth, mut k) = (0usize, start);
        while k < chars.len() {
            match chars[k] {
                '\'' | '"' => {
                    k = string_span(&chars, k).next;
                    continue;
                }
                '(' | '[' => depth += 1,
                ')' | ']' | ',' if depth == 0 => break,
                ')' | ']' => depth -= 1,
                _ => {}
            }
            k += 1;
        }
        return Some(
            chars[start..k.min(chars.len())]
                .iter()
                .collect::<String>()
                .trim()
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groovy(script: &str) -> Variables {
        let mut vars = Variables::default();
        vars.read_script(script);
        vars
    }

    fn kotlin(script: &str) -> Variables {
        let mut vars = Variables {
            kotlin: true,
            ..Variables::default()
        };
        vars.read_script(script);
        vars
    }

    fn value(vars: &Variables, name: &str) -> Result<String, Unread> {
        vars.resolve(name).map(|(v, _)| v)
    }

    #[test]
    fn every_groovy_spelling_of_a_variable_is_read() {
        let vars = groovy(
            "buildscript {\n  ext {\n    bootVersion = '3.5.4'\n  }\n  ext.kotlinVersion = '2.2.0'\n}\n\
             ext {\n  guava = '33.4.8-jre'; slf4j = \"2.0.17\"\n  set('springCloud', '2025.0.0')\n}\n\
             ext.netty = '4.2.4.Final'\nproject.ext.jackson = '2.19.2'\next['junit'] = '5.13.4'\n\
             def lombok = '1.18.38'\nString h2 = '2.3.232'\n",
        );
        for (name, expected) in [
            ("bootVersion", "3.5.4"),
            ("kotlinVersion", "2.2.0"),
            ("guava", "33.4.8-jre"),
            ("slf4j", "2.0.17"),
            ("springCloud", "2025.0.0"),
            ("netty", "4.2.4.Final"),
            ("jackson", "2.19.2"),
            ("junit", "5.13.4"),
            ("lombok", "1.18.38"),
            ("h2", "2.3.232"),
        ] {
            assert_eq!(value(&vars, name).as_deref(), Ok(expected), "{name}");
        }
        assert_eq!(
            vars.ext_names,
            ["bootVersion", "guava", "slf4j", "springCloud"]
        );
        assert!(!vars.ext_opaque);
    }

    #[test]
    fn every_kotlin_spelling_of_a_variable_is_read() {
        let vars = kotlin(
            "val guava = \"33.4.8-jre\"\nval slf4j: String = \"2.0.17\"\n\
             extra[\"netty\"] = \"4.2.4.Final\"\nval springCloud by extra(\"2025.0.0\")\n\
             val jackson: String by project\nvar h2 = \"2.3.232\"\n",
        );
        for (name, expected) in [
            ("guava", "33.4.8-jre"),
            ("slf4j", "2.0.17"),
            ("netty", "4.2.4.Final"),
            ("springCloud", "2025.0.0"),
            ("h2", "2.3.232"),
        ] {
            assert_eq!(value(&vars, name).as_deref(), Ok(expected), "{name}");
        }
        assert!(
            !vars.assignments.contains_key("jackson"),
            "`by project` reads gradle.properties, it assigns nothing"
        );
    }

    #[test]
    fn gradle_properties_are_variables() {
        let mut vars = Variables::default();
        vars.read_properties(
            "# a comment\norg.gradle.jvmargs=-Xmx2g\njackson = 2.19.2\nsnakeyaml: 2.4\n\
             multi=a\\\n  b\nempty=\n",
        );
        assert_eq!(value(&vars, "jackson").as_deref(), Ok("2.19.2"));
        assert_eq!(value(&vars, "snakeyaml").as_deref(), Ok("2.4"));
        assert_eq!(value(&vars, "org.gradle.jvmargs").as_deref(), Ok("-Xmx2g"));
        assert!(value(&vars, "multi").is_err());
        assert!(value(&vars, "empty").unwrap_err().contains("is empty"));
    }

    #[test]
    fn a_variable_is_followed_through_another() {
        let vars = groovy(
            "ext {\n  bootVersion = '3.5.4'\n  springVersion = \"$bootVersion\"\n  \
             braced = \"${project.bootVersion}\"\n  bare = springVersion\n}\n",
        );
        assert_eq!(
            vars.resolve("bare"),
            Ok((
                "3.5.4".to_string(),
                vec![
                    "bare".to_string(),
                    "springVersion".to_string(),
                    "bootVersion".to_string()
                ]
            ))
        );
        assert_eq!(value(&vars, "braced").as_deref(), Ok("3.5.4"));
    }

    #[test]
    fn only_a_literal_assigned_once_outside_a_conditional_is_used() {
        let vars = groovy(
            "def twice = '1.0'\ntwice = '2.0'\n\
             def summed = '1.0'\nsummed += '.1'\n\
             def env = System.getenv('V') ?: '1.0'\n\
             def joined = '1.' + '0'\n\
             def templated = \"1.${minor}\"\n\
             if (ci) {\n  def guarded = '1.0'\n}\n\
             if (ci) unbraced = '1.0'\n\
             tasks.register('x') {\n  def inTask = '1.0'\n}\n\
             def mapped = ['a'].collect { it }\n\
             def loop = 'a'\ndef alias = loop\ndef loopBack = \"$loopBack\"\n\
             def number = 1.2\n",
        );
        for (name, why) in [
            ("twice", "assigned more than once"),
            ("summed", "assigned more than once"),
            ("env", "not a string literal"),
            ("joined", "not a string literal"),
            ("templated", "not a string literal"),
            ("guarded", "inside a conditional"),
            ("unbraced", "inside a conditional"),
            ("inTask", "inside a conditional"),
            ("mapped", "not a string literal"),
            ("loopBack", "refers back to itself"),
            ("number", "not a string literal"),
            (
                "missing",
                "set neither in the build script nor in gradle.properties",
            ),
        ] {
            let error = value(&vars, name).unwrap_err();
            assert!(error.contains(&format!("`{name}`")), "{name}: {error}");
            assert!(error.contains(why), "{name}: {error}");
        }
        assert_eq!(value(&vars, "alias").as_deref(), Ok("a"));
    }

    #[test]
    fn a_broken_link_names_both_variables() {
        let vars = groovy("def base = System.getenv('V')\ndef derived = \"$base\"\n");
        let error = value(&vars, "derived").unwrap_err();
        assert!(
            error.starts_with("`derived` takes its value from `base`, and `base` is"),
            "{error}"
        );
    }

    #[test]
    fn every_way_of_naming_a_variable_is_interpolated() {
        let vars = groovy("ext {\n  v = '1.2'\n}\n");
        for text in [
            "g:a:$v",
            "g:a:${v}",
            "g:a:${project.v}",
            "g:a:${rootProject.ext.v}",
            "g:a:${project.ext.v}",
            "g:a:${property('v')}",
            "g:a:${project.property(\"v\")}",
            "g:a:${extra[\"v\"]}",
            "g:a:${rootProject.extra[\"v\"] as String}",
        ] {
            assert_eq!(
                vars.interpolate(text),
                Ok(("g:a:1.2".to_string(), vec!["v".to_string()])),
                "{text}"
            );
        }
        assert!(vars.interpolate("g:a:${v.trim()}").is_err());
        assert!(
            vars.interpolate("g:a:$v.RELEASE")
                .unwrap_err()
                .contains("`v.RELEASE`"),
            "Groovy reads a property of `v` there"
        );
        let kts = kotlin("val v = \"1.2\"\n");
        assert_eq!(
            kts.interpolate("g:a:$v.RELEASE").unwrap().0,
            "g:a:1.2.RELEASE",
            "Kotlin does not"
        );
    }

    #[test]
    fn a_value_is_a_literal_or_a_variable() {
        let vars = kotlin("val v = \"1.2\"\n");
        assert_eq!(vars.evaluate("'1.0'").unwrap().0, "1.0");
        assert_eq!(vars.evaluate("\"$v\"").unwrap().0, "1.2");
        assert_eq!(
            vars.evaluate("v").unwrap(),
            ("1.2".to_string(), vec!["v".to_string()])
        );
        assert_eq!(vars.evaluate("property(\"v\")").unwrap().0, "1.2");
        assert!(
            vars.evaluate("v + '.1'")
                .unwrap_err()
                .contains("an expression")
        );
    }

    #[test]
    fn a_quote_inside_an_interpolation_does_not_end_the_literal() {
        assert_eq!(
            literals("mavenBom(\"g:a:${property(\"v\")}\") // 'x'"),
            [
                ('"', "g:a:${property(\"v\")}".to_string()),
                ('\'', "x".to_string())
            ]
        );
        assert_eq!(
            statements("imports { mavenBom \"g:a:${v}\" }; dependency 'x:y:1'"),
            ["imports", "mavenBom \"g:a:${v}\"", "dependency 'x:y:1'"]
        );
    }

    #[test]
    fn map_fields_are_read_in_either_dsl() {
        let groovy = "group: 'g', name: 'a', version: lombokVersion, classifier: 'c'";
        assert_eq!(
            map_field(groovy, "version").as_deref(),
            Some("lombokVersion")
        );
        assert_eq!(map_field(groovy, "name").as_deref(), Some("'a'"));
        let kts = "(group = \"g\", name = \"a\", version = \"${property(\"v\")}\")";
        assert_eq!(
            map_field(kts, "version").as_deref(),
            Some("\"${property(\"v\")}\"")
        );
        assert_eq!(
            map_field("group: 'my.name: x'", "name"),
            None,
            "inside a literal"
        );
        assert_eq!(map_field("names: 'x'", "name"), None);
    }

    #[test]
    fn leading_values_stop_where_the_value_does() {
        assert_eq!(leading_value("\"3.5.4\" apply false"), "\"3.5.4\"");
        assert_eq!(leading_value("bootVersion apply false"), "bootVersion");
        assert_eq!(leading_value("property(\"v\"))"), "property(\"v\")");
    }

    #[test]
    fn the_ext_line_goes_once_every_variable_in_it_is_used() {
        let mut vars = groovy("ext {\n  a = '1'\n  b = '2'\n}\n");
        vars.used_by_dependency(&["a".to_string()]);
        let mut report = Report::default();
        vars.report(&mut report);
        assert_eq!(report.needs_review, ["a = 1 → 1 dependency"]);
        assert_eq!(
            report.not_migrated,
            ["`ext { }` — `b` is not used by anything jrs migrated"]
        );

        vars.used_by_dependency(&["b".to_string()]);
        vars.used_by_dependency(&["b".to_string()]);
        let mut report = Report::default();
        vars.report(&mut report);
        assert!(report.not_migrated.is_empty(), "{:?}", report.not_migrated);
        assert!(
            report.needs_review[1].starts_with("b = 2 → 2 dependencies — the manifest"),
            "{:?}",
            report.needs_review
        );

        let mut opaque = groovy("ext {\n  a = '1'\n  configure()\n}\n");
        opaque.used_by_dependency(&["a".to_string()]);
        let mut report = Report::default();
        opaque.report(&mut report);
        assert!(report.not_migrated[0].contains("more than variables"));
    }
}
