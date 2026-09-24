//! A small JSON writer, for `jrs metadata`, and a small reader, for the
//! Gradle module metadata a Kotlin Multiplatform library is published with.
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

    /// Read a JSON document (RFC 8259). A number that is not an integer is
    /// kept as its text, in a `Str`: what jrs reads never computes with one.
    ///
    /// # Errors
    ///
    /// A message with the byte offset where the text stops being JSON.
    pub fn parse(text: &str) -> std::result::Result<Json, String> {
        let mut reader = Reader {
            bytes: text.as_bytes(),
            at: 0,
            depth: 0,
        };
        let value = reader.value()?;
        reader.space();
        if reader.at != reader.bytes.len() {
            return Err(reader.fail("text after the document"));
        }
        Ok(value)
    }

    /// The value under `key`, when this is an object that has one.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The items, when this is an array; nothing otherwise.
    #[must_use]
    pub fn items(&self) -> &[Json] {
        match self {
            Json::Array(items) => items,
            _ => &[],
        }
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

/// How deep arrays and objects may nest before the reader gives up, so a
/// hostile document cannot overflow the stack.
const MAX_DEPTH: usize = 128;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl Reader<'_> {
    fn fail(&self, what: &str) -> String {
        format!("{what} at byte {}", self.at)
    }

    fn space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> std::result::Result<(), String> {
        self.space();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.fail(&format!("expected `{}`", char::from(byte))))
        }
    }

    fn value(&mut self) -> std::result::Result<Json, String> {
        self.space();
        match self.bytes.get(self.at) {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'n') => self.word("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => Ok(self.number()),
            Some(_) => Err(self.fail("unexpected character")),
            None => Err(self.fail("unexpected end")),
        }
    }

    fn nested(
        &mut self,
        read: impl FnOnce(&mut Self) -> std::result::Result<Json, String>,
    ) -> std::result::Result<Json, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail("nested too deeply"));
        }
        let value = read(self);
        self.depth -= 1;
        value
    }

    fn object(&mut self) -> std::result::Result<Json, String> {
        self.eat(b'{')?;
        let mut pairs = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Json::Object(pairs));
        }
        loop {
            self.space();
            if self.bytes.get(self.at) != Some(&b'"') {
                return Err(self.fail("expected a key"));
            }
            let key = self.string()?;
            self.eat(b':')?;
            pairs.push((key, self.value()?));
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(pairs));
                }
                _ => return Err(self.fail("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self) -> std::result::Result<Json, String> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.fail("expected `,` or `]`")),
            }
        }
    }

    fn word(&mut self, word: &str, value: Json) -> std::result::Result<Json, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.fail("unexpected character"))
        }
    }

    fn number(&mut self) -> Json {
        let start = self.at;
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            self.at += 1;
        }
        let text = String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned();
        text.parse().map_or(Json::Str(text), Json::Int)
    }

    fn string(&mut self) -> std::result::Result<String, String> {
        self.at += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&byte) = self.bytes.get(self.at) else {
                return Err(self.fail("unterminated string"));
            };
            self.at += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let Some(&escaped) = self.bytes.get(self.at) else {
                        return Err(self.fail("unterminated string"));
                    };
                    self.at += 1;
                    let c = match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode()?,
                        _ => return Err(self.fail("unknown escape")),
                    };
                    let mut buffer = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                }
                _ => out.push(byte),
            }
        }
        String::from_utf8(out).map_err(|_| self.fail("a string that is not UTF-8"))
    }

    /// The character after `\u`, a surrogate pair's second half included.
    fn unicode(&mut self) -> std::result::Result<char, String> {
        let high = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&high) && self.bytes[self.at..].starts_with(b"\\u")
        {
            self.at += 2;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.fail("a lone surrogate"));
            }
            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
        } else {
            high
        };
        char::from_u32(code).ok_or_else(|| self.fail("a lone surrogate"))
    }

    fn hex4(&mut self) -> std::result::Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u32::from_str_radix(d, 16).ok())
            .ok_or_else(|| self.fail("a bad `\\u` escape"))?;
        self.at += 4;
        Ok(digits)
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
    fn documents_read_back_as_they_were_written() {
        let doc = Json::object([
            ("name", Json::string("a \"b\" \\ zażółć 😀\n")),
            ("n", Json::Int(-12)),
            ("list", Json::Array(vec![Json::Null, Json::Bool(true)])),
            ("empty", Json::Object(Vec::new())),
        ]);
        assert_eq!(Json::parse(&doc.render()).unwrap(), doc);
    }

    #[test]
    fn the_reader_takes_escapes_and_keeps_fractions_as_text() {
        let doc = Json::parse(r#" {"a": "\u0041\ud83d\ude00\/", "f": 1.5e3, "e": []} "#).unwrap();
        assert_eq!(doc.get("a").and_then(Json::as_str), Some("A😀/"));
        assert_eq!(doc.get("f"), Some(&Json::string("1.5e3")));
        assert!(doc.get("e").unwrap().items().is_empty());
        assert_eq!(doc.get("missing"), None);
    }

    #[test]
    fn the_reader_refuses_what_is_not_json() {
        for bad in [
            "",
            "{",
            "{\"a\" 1}",
            "[1,]",
            "\"open",
            "tru",
            "{} x",
            "\"\\ud800\"",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?} should not parse");
        }
        let deep = "[".repeat(MAX_DEPTH + 1);
        assert!(
            Json::parse(&deep)
                .unwrap_err()
                .contains("nested too deeply")
        );
    }

    #[test]
    fn keys_are_escaped_like_values() {
        let doc = Json::object([("a\"b", Json::or_null(None::<i64>, Json::Int))]);
        assert_eq!(doc.render(), "{\n  \"a\\\"b\": null\n}\n");
    }
}
