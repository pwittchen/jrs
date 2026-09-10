//! Maven coordinates, scopes, and version ordering.
//!
//! Version comparison follows Maven's `ComparableVersion` closely enough for the
//! shapes that occur in practice: `1.2.3`, `3.14.0`, `33.0.0-jre`, `2.0-beta-7`,
//! `1.0-SNAPSHOT`. jrs never *picks* a version by ordering — mediation is
//! nearest-wins (SPEC §8.2) — but it needs the order to apply
//! `<dependencyManagement>` sanely and to report conflicts in a stable way.

use std::cmp::Ordering;
use std::fmt;

use crate::error::{JrsError, Result};

/// A `group:artifact` pair — the identity a dependency graph dedupes on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ga {
    pub group: String,
    pub artifact: String,
}

impl Ga {
    pub fn new(group: impl Into<String>, artifact: impl Into<String>) -> Ga {
        Ga {
            group: group.into(),
            artifact: artifact.into(),
        }
    }
}

impl fmt::Display for Ga {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group, self.artifact)
    }
}

/// A full `group:artifact:version` triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Coord {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Coord {
    pub fn new(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Coord {
        Coord {
            group: group.into(),
            artifact: artifact.into(),
            version: version.into(),
        }
    }

    pub fn parse(s: &str) -> Result<Coord> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(JrsError::resolve(format!(
                "`{s}` is not a `group:artifact:version` coordinate"
            )));
        }
        Ok(Coord::new(parts[0], parts[1], parts[2]))
    }

    pub fn ga(&self) -> Ga {
        Ga::new(&self.group, &self.artifact)
    }

    /// `guava-33.0.0-jre.jar`
    pub fn file_name(&self, ext: &str) -> String {
        format!("{}-{}.{ext}", self.artifact, self.version)
    }

    /// `com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar` (SPEC §8.1).
    pub fn repo_path(&self, ext: &str) -> String {
        format!(
            "{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.file_name(ext)
        )
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)
    }
}

impl Ord for Coord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.group
            .cmp(&other.group)
            .then_with(|| self.artifact.cmp(&other.artifact))
            .then_with(|| compare_versions(&self.version, &other.version))
            // `1.0` and `1.0.0` compare equal as versions but are distinct
            // coordinates; break the tie so `Ord` and `Eq` stay consistent.
            .then_with(|| self.version.cmp(&other.version))
    }
}

impl PartialOrd for Coord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Maven dependency scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Compile,
    Runtime,
    Provided,
    System,
    Test,
    Import,
}

impl Scope {
    pub fn parse(s: &str) -> Scope {
        match s.trim().to_ascii_lowercase().as_str() {
            "runtime" => Scope::Runtime,
            "provided" => Scope::Provided,
            "system" => Scope::System,
            "test" => Scope::Test,
            "import" => Scope::Import,
            _ => Scope::Compile,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Compile => "compile",
            Scope::Runtime => "runtime",
            Scope::Provided => "provided",
            Scope::System => "system",
            Scope::Test => "test",
            Scope::Import => "import",
        }
    }

    /// Whether a dependency in this scope is walked transitively and put on the
    /// classpath (SPEC §8.2 step 4).
    pub fn is_transitive(self) -> bool {
        matches!(self, Scope::Compile | Scope::Runtime)
    }
}

/// True for Maven version ranges like `[1.0,2.0)` or `[1.5,]`.
///
/// jrs rejects these rather than silently mishandling them (SPEC §8.2).
pub fn is_range(version: &str) -> bool {
    let v = version.trim();
    v.starts_with('[') || v.starts_with('(')
}

// ---- version ordering ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Num(u64),
    Qualifier(String),
}

/// Known qualifiers, weakest first. A release (the empty qualifier) sits between
/// `snapshot` and `sp`, which is what makes `1.0` newer than `1.0-rc1` but older
/// than `1.0-sp1`.
const QUALIFIERS: &[&str] = &["alpha", "beta", "milestone", "rc", "snapshot", "", "sp"];

fn canonical_qualifier(s: &str) -> String {
    match s {
        "a" => "alpha".into(),
        "b" => "beta".into(),
        "m" => "milestone".into(),
        "cr" => "rc".into(),
        "ga" | "final" | "release" => String::new(),
        other => other.to_string(),
    }
}

fn qualifier_rank(s: &str) -> Option<usize> {
    QUALIFIERS.iter().position(|q| *q == s)
}

/// Split a version into numeric and qualifier items, breaking on `.`, `-`, `_`
/// and on every digit/letter transition.
fn tokenize(version: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut buf = String::new();
    let mut buf_is_digit = false;

    fn flush(buf: &mut String, is_digit: bool, items: &mut Vec<Item>) {
        if buf.is_empty() {
            return;
        }
        if is_digit {
            // A version segment longer than u64 is pathological; clamp rather
            // than fail, since ordering is advisory here.
            items.push(Item::Num(buf.parse().unwrap_or(u64::MAX)));
        } else {
            items.push(Item::Qualifier(canonical_qualifier(buf)));
        }
        buf.clear();
    }

    for ch in version.trim().to_ascii_lowercase().chars() {
        if ch == '.' || ch == '-' || ch == '_' || ch == '+' {
            flush(&mut buf, buf_is_digit, &mut items);
            continue;
        }
        let is_digit = ch.is_ascii_digit();
        if !buf.is_empty() && is_digit != buf_is_digit {
            flush(&mut buf, buf_is_digit, &mut items);
        }
        buf_is_digit = is_digit;
        buf.push(ch);
    }
    flush(&mut buf, buf_is_digit, &mut items);
    items
}

fn compare_items(a: Option<&Item>, b: Option<&Item>) -> Ordering {
    // A missing item is "null": zero for numbers, the release qualifier for
    // strings. `1.0` and `1` are therefore equal, and `1.0` beats `1.0-rc1`.
    match (a, b) {
        (None, None) => Ordering::Equal,
        (Some(Item::Num(n)), None) => n.cmp(&0),
        (None, Some(Item::Num(n))) => 0.cmp(n),
        (Some(Item::Qualifier(q)), None) => compare_qualifiers(q, ""),
        (None, Some(Item::Qualifier(q))) => compare_qualifiers("", q),
        (Some(Item::Num(x)), Some(Item::Num(y))) => x.cmp(y),
        (Some(Item::Qualifier(x)), Some(Item::Qualifier(y))) => compare_qualifiers(x, y),
        // A number always outranks a qualifier: `1.1` > `1.1-beta`, `1.1` > `1.ga`.
        (Some(Item::Num(_)), Some(Item::Qualifier(_))) => Ordering::Greater,
        (Some(Item::Qualifier(_)), Some(Item::Num(_))) => Ordering::Less,
    }
}

fn compare_qualifiers(a: &str, b: &str) -> Ordering {
    match (qualifier_rank(a), qualifier_rank(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        // Unknown qualifiers sort after every known one, and among themselves
        // alphabetically — Maven's rule, and the only stable choice available.
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Compare two Maven versions.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (x, y) = (tokenize(a), tokenize(b));
    for i in 0..x.len().max(y.len()) {
        let ord = compare_items(x.get(i), y.get(i));
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Identical after normalisation: `1.0`, `1.0.0` and `1.0-ga` all name the
    // same release, so they compare equal even though the strings differ.
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn coordinates_map_onto_repository_paths() {
        let c = Coord::new("com.google.guava", "guava", "33.0.0-jre");
        assert_eq!(
            c.repo_path("jar"),
            "com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar"
        );
        assert_eq!(
            c.repo_path("pom"),
            "com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.pom"
        );
        assert_eq!(c.file_name("jar"), "guava-33.0.0-jre.jar");
    }

    #[test]
    fn coordinate_parsing_rejects_the_wrong_shape() {
        assert_eq!(
            Coord::parse("g:a:1.0").unwrap(),
            Coord::new("g", "a", "1.0")
        );
        assert!(Coord::parse("g:a").is_err());
        assert!(Coord::parse("g:a:1.0:jar").is_err());
        assert!(Coord::parse("g::1.0").is_err());
    }

    #[test]
    fn numeric_segments_compare_numerically() {
        assert_eq!(compare_versions("1.2", "1.10"), Less);
        assert_eq!(compare_versions("1.10", "1.2"), Greater);
        assert_eq!(compare_versions("3.14.0", "3.14.0"), Equal);
        assert_eq!(compare_versions("33.0.0-jre", "32.1.3-jre"), Greater);
    }

    #[test]
    fn trailing_zeroes_do_not_change_a_version() {
        assert_eq!(compare_versions("1.0", "1"), Equal);
        assert_eq!(compare_versions("1.0.0", "1.0"), Equal);
    }

    #[test]
    fn prereleases_sort_below_their_release() {
        assert_eq!(compare_versions("1.0-alpha1", "1.0"), Less);
        assert_eq!(compare_versions("1.0-beta2", "1.0-rc1"), Less);
        assert_eq!(compare_versions("1.0-SNAPSHOT", "1.0"), Less);
        assert_eq!(compare_versions("1.0-rc1", "1.0-SNAPSHOT"), Less);
        assert_eq!(compare_versions("1.0", "1.0-sp1"), Less);
        assert_eq!(compare_versions("2.0-beta-7", "2.0"), Less);
    }

    #[test]
    fn qualifier_aliases_are_normalised() {
        assert_eq!(compare_versions("1.0-a1", "1.0-alpha1"), Equal);
        assert_eq!(compare_versions("1.0-cr1", "1.0-rc1"), Equal);
        assert_eq!(compare_versions("1.0-ga", "1.0"), Equal);
        assert_eq!(compare_versions("1.0-final", "1.0"), Equal);
    }

    #[test]
    fn unknown_qualifiers_sort_after_known_ones() {
        assert_eq!(compare_versions("1.0-rc1", "1.0-jre"), Less);
        assert_eq!(compare_versions("1.0-android", "1.0-jre"), Less);
    }

    #[test]
    fn numbers_outrank_qualifiers_at_the_same_position() {
        assert_eq!(compare_versions("1.1", "1.1-beta"), Greater);
        assert_eq!(compare_versions("1.1", "1.1.1"), Less);
    }

    #[test]
    fn version_ordering_is_total_and_consistent() {
        let mut vs = vec![
            "1.0-SNAPSHOT",
            "1.0",
            "1.0-alpha1",
            "0.9",
            "1.0.1",
            "1.0-sp1",
            "2.0",
        ];
        vs.sort_by(|a, b| compare_versions(a, b));
        assert_eq!(
            vs,
            vec![
                "0.9",
                "1.0-alpha1",
                "1.0-SNAPSHOT",
                "1.0",
                "1.0-sp1",
                "1.0.1",
                "2.0",
            ]
        );
    }

    #[test]
    fn coordinates_order_by_group_then_artifact_then_version() {
        let mut cs = [
            Coord::new("b", "a", "1.0"),
            Coord::new("a", "z", "1.0"),
            Coord::new("a", "a", "2.0"),
            Coord::new("a", "a", "1.10"),
        ];
        cs.sort();
        let rendered: Vec<String> = cs.iter().map(|c| c.to_string()).collect();
        assert_eq!(rendered, vec!["a:a:1.10", "a:a:2.0", "a:z:1.0", "b:a:1.0"]);
    }

    #[test]
    fn ranges_are_recognised() {
        assert!(is_range("[1.0,2.0)"));
        assert!(is_range("(1.0,]"));
        assert!(!is_range("1.0"));
        assert!(!is_range("1.0-SNAPSHOT"));
    }

    #[test]
    fn only_compile_and_runtime_are_walked_transitively() {
        assert!(Scope::parse("compile").is_transitive());
        assert!(Scope::parse("runtime").is_transitive());
        assert!(!Scope::parse("provided").is_transitive());
        assert!(!Scope::parse("system").is_transitive());
        assert!(!Scope::parse("test").is_transitive());
        assert_eq!(
            Scope::parse(""),
            Scope::Compile,
            "empty scope means compile"
        );
    }
}
