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

/// A `group:artifact` pair, plus a classifier when there is one — the identity
/// a dependency graph dedupes on.
///
/// The classifier is part of the identity because a classified artifact is a
/// different file: `lwjgl` and `lwjgl:natives-linux` are both needed at once, and
/// mediating one against the other would drop half of the library.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ga {
    pub group: String,
    pub artifact: String,
    pub classifier: Option<String>,
}

impl Ga {
    pub fn new(group: impl Into<String>, artifact: impl Into<String>) -> Ga {
        Ga {
            group: group.into(),
            artifact: artifact.into(),
            classifier: None,
        }
    }

    pub fn with_classifier(mut self, classifier: Option<String>) -> Ga {
        self.classifier = classifier;
        self
    }

    /// `group:artifact`, or `group:artifact:classifier`.
    pub fn parse(s: &str) -> Option<Ga> {
        let mut parts = s.splitn(3, ':');
        let group = parts.next().filter(|p| !p.is_empty())?;
        let artifact = parts.next().filter(|p| !p.is_empty())?;
        let classifier = parts.next().filter(|p| !p.is_empty()).map(str::to_string);
        Some(Ga::new(group, artifact).with_classifier(classifier))
    }

    /// Whether an exclusion pattern (`*` wildcards allowed) covers this artifact.
    /// Exclusions name a group and artifact only, so they match every classifier.
    pub fn excluded_by(&self, pattern: &Ga) -> bool {
        (pattern.group == "*" || pattern.group == self.group)
            && (pattern.artifact == "*" || pattern.artifact == self.artifact)
    }
}

impl fmt::Display for Ga {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group, self.artifact)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        Ok(())
    }
}

/// A full `group:artifact:version` triple, with an optional classifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Coord {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
}

pub const SNAPSHOT_SUFFIX: &str = "-SNAPSHOT";

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
            classifier: None,
        }
    }

    pub fn with_classifier(mut self, classifier: Option<String>) -> Coord {
        self.classifier = classifier;
        self
    }

    /// `group:artifact:version`, or Gradle's `group:artifact:version:classifier`.
    pub fn parse(s: &str) -> Result<Coord> {
        let parts: Vec<&str> = s.split(':').collect();
        if !(3..=4).contains(&parts.len()) || parts.iter().any(|p| p.is_empty()) {
            return Err(JrsError::resolve(format!(
                "`{s}` is not a `group:artifact:version` coordinate"
            )));
        }
        Ok(Coord::new(parts[0], parts[1], parts[2])
            .with_classifier(parts.get(3).map(|c| c.to_string())))
    }

    pub fn ga(&self) -> Ga {
        Ga::new(&self.group, &self.artifact).with_classifier(self.classifier.clone())
    }

    /// The coordinate whose POM describes this one: a classified artifact shares
    /// the POM of the artifact it sits beside.
    pub fn pom_coord(&self) -> Coord {
        Coord::new(&self.group, &self.artifact, &self.version)
    }

    /// `1.0-SNAPSHOT`: a version that may be republished under the same name.
    pub fn is_snapshot(&self) -> bool {
        self.version.ends_with(SNAPSHOT_SUFFIX)
    }

    /// `guava-33.0.0-jre.jar`, `lwjgl-3.3.3-natives-linux.jar`.
    ///
    /// A POM has no classifier: a classified artifact is an extra file published
    /// beside the main one, described by the same POM.
    pub fn file_name(&self, ext: &str) -> String {
        self.file_name_as(ext, &self.version)
    }

    /// Like [`Coord::file_name`], with the version in the file name replaced —
    /// a timestamped snapshot lives in the `-SNAPSHOT` directory under a name like
    /// `lib-1.0-20240101.120000-3.jar`.
    pub fn file_name_as(&self, ext: &str, file_version: &str) -> String {
        match &self.classifier {
            Some(c) if ext != "pom" && !ext.starts_with("pom.") => {
                format!("{}-{file_version}-{c}.{ext}", self.artifact)
            }
            _ => format!("{}-{file_version}.{ext}", self.artifact),
        }
    }

    /// `com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar` (SPEC §8.1).
    pub fn repo_path(&self, ext: &str) -> String {
        self.repo_path_as(ext, &self.version)
    }

    pub fn repo_path_as(&self, ext: &str, file_version: &str) -> String {
        format!(
            "{}/{}",
            self.version_dir(),
            self.file_name_as(ext, file_version)
        )
    }

    /// `com/google/guava/guava/33.0.0-jre`, where every file of a version lives.
    pub fn version_dir(&self) -> String {
        format!(
            "{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version
        )
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        Ok(())
    }
}

impl Ord for Coord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.group
            .cmp(&other.group)
            .then_with(|| self.artifact.cmp(&other.artifact))
            .then_with(|| self.classifier.cmp(&other.classifier))
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
        assert!(Coord::parse("g:a:1.0:natives:extra").is_err());
        assert!(Coord::parse("g::1.0").is_err());
    }

    #[test]
    fn a_classifier_names_a_file_beside_the_main_artifact() {
        let c = Coord::parse("org.lwjgl:lwjgl:3.3.3:natives-linux").unwrap();
        assert_eq!(c.classifier.as_deref(), Some("natives-linux"));
        assert_eq!(c.to_string(), "org.lwjgl:lwjgl:3.3.3:natives-linux");
        assert_eq!(
            c.repo_path("jar"),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
        );
        assert_eq!(
            c.repo_path("jar.sha1"),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar.sha1"
        );
        // ...but it is described by the unclassified POM.
        assert_eq!(c.repo_path("pom"), "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.pom");
        assert_eq!(c.pom_coord(), Coord::new("org.lwjgl", "lwjgl", "3.3.3"));
        assert_ne!(c.ga(), Ga::new("org.lwjgl", "lwjgl"));
        assert_eq!(c.ga().to_string(), "org.lwjgl:lwjgl:natives-linux");
    }

    #[test]
    fn group_artifact_pairs_parse_with_or_without_a_classifier() {
        assert_eq!(Ga::parse("g:a"), Some(Ga::new("g", "a")));
        assert_eq!(
            Ga::parse("g:a:tests"),
            Some(Ga::new("g", "a").with_classifier(Some("tests".into())))
        );
        assert_eq!(Ga::parse("g"), None);
        assert_eq!(Ga::parse(":a"), None);
    }

    #[test]
    fn exclusions_match_every_classifier_and_honour_wildcards() {
        let natives = Ga::new("g", "a").with_classifier(Some("natives".into()));
        assert!(natives.excluded_by(&Ga::new("g", "a")));
        assert!(natives.excluded_by(&Ga::new("*", "a")));
        assert!(natives.excluded_by(&Ga::new("g", "*")));
        assert!(!natives.excluded_by(&Ga::new("g", "b")));
    }

    #[test]
    fn timestamped_snapshots_keep_their_directory() {
        let c = Coord::new("g", "lib", "1.0-SNAPSHOT");
        assert!(c.is_snapshot());
        assert!(!Coord::new("g", "lib", "1.0").is_snapshot());
        assert_eq!(
            c.repo_path_as("jar", "1.0-20240101.120000-3"),
            "g/lib/1.0-SNAPSHOT/lib-1.0-20240101.120000-3.jar"
        );
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
