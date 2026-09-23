//! `[resources]`: `${name}` expansion in resource files (SPEC §7.3).
//!
//! The declarative answer to Gradle's `processResources { expand(...) }` and
//! Maven's resource `<filtering>`: the files `resources.expand` names have
//! every `${name}` replaced as they are copied into `target/classes`, and
//! nothing else about them changes. The syntax is the part both tools share,
//! plus Gradle's one escape:
//!
//! - `${name}` is replaced by the property `name`. A name jrs does not know is
//!   an error naming the file and line, as it is in Gradle — a resource that
//!   silently kept `${db.url}` would fail far from its cause.
//! - `\$` is a literal `$`, so `"\${ENV_VAR:}"` reaches the program as
//!   `"${ENV_VAR:}"`, for Spring to resolve at run time.
//! - Any other `$` is copied as it is.
//!
//! There is no scripting: Gradle's `$name` without braces and `<% %>` blocks
//! are Groovy templates, and jrs runs no build code.

use std::path::Path;

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Manifest;

/// What `[resources]` asks for, with every property's value known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    patterns: Vec<String>,
    properties: Vec<(String, String)>,
}

impl Expansion {
    /// The manifest's expansion, or `None` when `resources.expand` is empty.
    ///
    /// # Errors
    ///
    /// As for [`Manifest::resource_properties`].
    pub fn from_manifest(manifest: &Manifest) -> Result<Option<Expansion>> {
        if manifest.resources.expand.is_empty() {
            return Ok(None);
        }
        Ok(Some(Expansion::new(
            manifest.resources.expand.clone(),
            manifest.resource_properties()?,
        )))
    }

    #[must_use]
    pub fn new(patterns: Vec<String>, properties: Vec<(String, String)>) -> Expansion {
        Expansion {
            patterns,
            properties,
        }
    }

    /// Whether the resource at `relative` (`/`-separated) is expanded.
    #[must_use]
    pub fn applies_to(&self, relative: &str) -> bool {
        self.patterns.iter().any(|p| glob_matches(p, relative))
    }

    /// `text` with every `${name}` replaced and every `\$` unescaped.
    ///
    /// # Errors
    ///
    /// A message, with the 1-based line, for an unknown name or an unclosed
    /// `${`.
    pub fn expand(&self, text: &str) -> std::result::Result<String, String> {
        let mut out = String::with_capacity(text.len());
        let mut line = 1;
        let mut rest = text;
        while let Some(i) = rest.find(['\\', '$', '\n']) {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            if let Some(after) = tail.strip_prefix('\n') {
                line += 1;
                out.push('\n');
                rest = after;
            } else if let Some(after) = tail.strip_prefix("\\$") {
                out.push('$');
                rest = after;
            } else if let Some(after) = tail.strip_prefix("${") {
                let Some(end) = after.find('}').filter(|&e| !after[..e].contains('\n')) else {
                    return Err(format!(
                        "line {line}: `${{` is not closed on its line; write `\\${{` for a \
                         literal one"
                    ));
                };
                let name = after[..end].trim();
                let value = self
                    .properties
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v)
                    .ok_or_else(|| {
                        let known: Vec<String> = self
                            .properties
                            .iter()
                            .map(|(n, _)| format!("`{n}`"))
                            .collect();
                        format!(
                            "line {line}: unknown property `{name}` (known: {}); declare it in \
                             `resources.properties`, or write `\\${{{name}}}` to keep it as it is",
                            known.join(", ")
                        )
                    })?;
                out.push_str(value);
                rest = &after[end + 1..];
            } else {
                // A `\` not before `$`, or a `$` not before `{`: text.
                out.push_str(&tail[..1]);
                rest = &tail[1..];
            }
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Write `source`, expanded, to `destination`, unless it already holds
    /// exactly that. Returns whether it wrote.
    ///
    /// The content is compared rather than the mtime, since the result also
    /// depends on the manifest: a new `project.version` must reach
    /// `application.yml` even when the file itself has not changed.
    ///
    /// # Errors
    ///
    /// [`JrsError::Build`] for a file that is not UTF-8 or does not expand;
    /// [`JrsError::Io`] when a file cannot be read or written.
    pub fn copy(&self, source: &Path, destination: &Path) -> Result<bool> {
        let bytes = std::fs::read(source).path(source)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            JrsError::build(format!(
                "{}: `resources.expand` names it, but it is not UTF-8 text",
                source.display()
            ))
        })?;
        let expanded = self
            .expand(&text)
            .map_err(|e| JrsError::build(format!("{}: {e}", source.display())))?;
        if std::fs::read(destination).is_ok_and(|d| d == expanded.as_bytes()) {
            return Ok(false);
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        std::fs::write(destination, expanded).path(destination)?;
        Ok(true)
    }
}

/// A name `${...}` can hold: letters, digits, `.`, `-` and `_`.
#[must_use]
pub fn valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Whether `path` matches `pattern`, both `/`-separated. `*` and `?` stay
/// within one segment; a `**` segment matches any number of them, none
/// included.
#[must_use]
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments_match(&pattern, &path)
}

fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(seg, tail)| segment_matches(first, seg) && segments_match(rest, tail)),
    }
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let segment: Vec<char> = segment.chars().collect();
    // Classic two-pointer wildcard match, backtracking to the last `*`.
    let (mut p, mut s) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while s < segment.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == segment[s]) {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, s));
            p += 1;
        } else if let Some((sp, ss)) = star {
            p = sp + 1;
            s = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expansion() -> Expansion {
        Expansion::new(
            vec!["application.yml".into(), "config/**/*.properties".into()],
            vec![
                ("project.version".into(), "1.2.0".into()),
                ("version".into(), "1.2.0".into()),
                ("group".into(), "com.example".into()),
            ],
        )
    }

    #[test]
    fn names_are_replaced_and_escapes_unescaped() {
        let text = "app:\n  version: ${version}\n  maven: ${ project.version }\n  \
                    secret: \"\\${APP_SECRET:}\"\n  cost: $5 \\n {braces}\n";
        assert_eq!(
            expansion().expand(text).unwrap(),
            "app:\n  version: 1.2.0\n  maven: 1.2.0\n  \
             secret: \"${APP_SECRET:}\"\n  cost: $5 \\n {braces}\n"
        );
    }

    #[test]
    fn an_unknown_name_is_an_error_with_its_line() {
        let err = expansion().expand("a: 1\nb: ${db.url}\n").unwrap_err();
        assert!(
            err.starts_with("line 2: unknown property `db.url`"),
            "{err}"
        );
        assert!(err.contains("`group`"), "{err}");

        let err = expansion().expand("a: ${version\nb: }").unwrap_err();
        assert!(err.starts_with("line 1: `${` is not closed"), "{err}");
    }

    #[test]
    fn text_without_placeholders_is_unchanged() {
        let text = "plain $ text \\ with ünïcode\r\n";
        assert_eq!(expansion().expand(text).unwrap(), text);
    }

    #[test]
    fn globs_match_by_segment() {
        let e = expansion();
        assert!(e.applies_to("application.yml"));
        assert!(!e.applies_to("static/application.yml"));
        assert!(e.applies_to("config/a.properties"));
        assert!(e.applies_to("config/x/y/b.properties"));
        assert!(!e.applies_to("config/a.yml"));

        assert!(glob_matches("**", "a/b/c"));
        assert!(glob_matches("**/*.yml", "application.yml"));
        assert!(glob_matches("app*.y?l", "application-test.yml"));
        assert!(!glob_matches("*.yml", "a/b.yml"));
        assert!(glob_matches("a*b*c", "aXbYbZc"));
        assert!(!glob_matches("a*b*c", "aXbYbZ"));
    }

    #[test]
    fn copy_writes_only_what_changed() {
        let dir = std::env::temp_dir().join(format!("jrs-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("application.yml");
        let destination = dir.join("out/application.yml");
        std::fs::write(&source, "v: ${version}\n").unwrap();

        let e = expansion();
        assert!(e.copy(&source, &destination).unwrap());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "v: 1.2.0\n");
        assert!(!e.copy(&source, &destination).unwrap());

        let bumped = Expansion::new(e.patterns.clone(), vec![("version".into(), "2.0".into())]);
        assert!(bumped.copy(&source, &destination).unwrap());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "v: 2.0\n");

        std::fs::write(&source, [0xff, 0xfe]).unwrap();
        let err = e.copy(&source, &destination).unwrap_err().to_string();
        assert!(err.contains("not UTF-8"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
