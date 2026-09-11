//! A small JSON writer, for `jrs metadata`.
//!
//! The document jrs writes is small and its shape is fixed, so a value tree
//! and a pretty-printer cover it; `serde_json` would be a new crate for what
//! is a hundred lines here (SPEC §13). Objects keep their keys in insertion
//! order, so the same project always prints the same bytes.

use std::fmt::Write as _;
use std::path::Path;

/// A JSON value. Objects are ordered lists of pairs, not maps: key order is
/// part of the output, and the caller decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn string(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    /// A path, as the platform displays it.
    #[must_use]
    pub fn path(p: &Path) -> Json {
        Json::Str(p.display().to_string())
    }

    /// `null` for `None`, otherwise whatever `f` makes of the value.
    pub fn or_null<T>(value: Option<T>, f: impl FnOnce(T) -> Json) -> Json {
        value.map_or(Json::Null, f)
    }

    /// An object from `(key, value)` pairs, in the order given.
    pub fn object<'a>(pairs: impl IntoIterator<Item = (&'a str, Json)>) -> Json {
        Json::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// The document, pretty-printed with two-space indentation and a trailing
    /// newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Str(s) => escape_into(s, out),
            Json::Array(items) if items.is_empty() => out.push_str("[]"),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    item.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push(']');
            }
            Json::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
            Json::Object(pairs) => {
                out.push('{');
                for (i, (key, value)) in pairs.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    escape_into(key, out);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// `s` as a quoted JSON string (RFC 8259 §7): quotes, backslashes and every
/// control character escaped; other characters, non-ASCII included, written
/// as UTF-8. U+2028 and U+2029 are escaped too, since JavaScript before
/// ES2019 reads them as line ends inside a string.
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    escape_into(s, &mut out);
    out
}

fn escape_into(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 || c == '\u{7f}' || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_escape_quotes_backslashes_and_control_characters() {
        assert_eq!(escape("plain"), r#""plain""#);
        assert_eq!(escape(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(escape(r"C:\Users\me"), r#""C:\\Users\\me""#);
        assert_eq!(escape("a\nb\r\tc"), r#""a\nb\r\tc""#);
        assert_eq!(escape("\u{8}\u{c}"), r#""\b\f""#);
        assert_eq!(escape("\u{0}\u{1f}\u{7f}"), r#""\u0000\u001f\u007f""#);
    }

    #[test]
    fn non_ascii_is_written_as_utf8_except_the_javascript_line_ends() {
        assert_eq!(escape("zażółć 日本 😀"), "\"zażółć 日本 😀\"");
        assert_eq!(escape("a\u{2028}b\u{2029}"), r#""a\u2028b\u2029""#);
    }

    #[test]
    fn documents_pretty_print_in_insertion_order() {
        let doc = Json::object([
            ("version", Json::Int(1)),
            ("name", Json::string("app")),
            ("none", Json::Null),
            (
                "flags",
                Json::Array(vec![Json::Bool(true), Json::Bool(false)]),
            ),
            ("empty", Json::Array(Vec::new())),
            (
                "nested",
                Json::object([("z", Json::Int(-2)), ("a", Json::Object(Vec::new()))]),
            ),
        ]);
        assert_eq!(
            doc.render(),
            concat!(
                "{\n",
                "  \"version\": 1,\n",
                "  \"name\": \"app\",\n",
                "  \"none\": null,\n",
                "  \"flags\": [\n",
                "    true,\n",
                "    false\n",
                "  ],\n",
                "  \"empty\": [],\n",
                "  \"nested\": {\n",
                "    \"z\": -2,\n",
                "    \"a\": {}\n",
                "  }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn keys_are_escaped_like_values() {
        let doc = Json::object([("a\"b", Json::or_null(None::<i64>, Json::Int))]);
        assert_eq!(doc.render(), "{\n  \"a\\\"b\": null\n}\n");
    }
}
