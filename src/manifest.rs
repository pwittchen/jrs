//! `jrs.toml`: parsing, validation, defaults.
//!
//! The manifest is parsed by hand out of a `toml::Table` rather than through a
//! `serde` derive. Three things fall out of that which the derive would not give
//! us: declaration order is preserved (conflict mediation breaks ties on it,
//! SPEC §8.2), every diagnostic can name the offending key, and unknown keys can
//! be a warning instead of an error, so manifests stay forward-compatible
//! (SPEC §4.3).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result};

pub const MANIFEST_FILE: &str = "jrs.toml";
pub const LOCK_FILE: &str = "jrs.lock";

/// A declared dependency: a `group:artifact` key and an exact version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Dependency {
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

impl std::fmt::Display for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub url: String,
}

pub const CENTRAL_NAME: &str = "central";
pub const CENTRAL_URL: &str = "https://repo1.maven.org/maven2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaConfig {
    /// `--release`. `None` means "whatever the detected JDK is".
    pub source: Option<u32>,
    /// Only meaningful when it differs from `source`.
    pub target: Option<u32>,
    pub encoding: String,
    pub javac_args: Vec<String>,
}

impl Default for JavaConfig {
    fn default() -> Self {
        JavaConfig {
            source: None,
            target: None,
            encoding: "UTF-8".to_string(),
            javac_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Manifest {
    /// Absolute path to `jrs.toml`.
    pub path: PathBuf,
    /// Directory holding the manifest; every relative path is resolved against it.
    pub root: PathBuf,

    pub name: String,
    pub version: String,
    pub main_class: Option<String>,

    pub source_dir: PathBuf,
    pub test_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub test_resource_dir: PathBuf,
    pub target_dir: PathBuf,

    pub java: JavaConfig,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    /// User repositories in declaration order, with Central appended last.
    pub repositories: Vec<Repository>,

    /// Non-fatal complaints, surfaced by the CLI after the manifest loads.
    pub warnings: Vec<String>,
}

const PROJECT_KEYS: &[&str] = &[
    "name",
    "version",
    "main-class",
    "source-dir",
    "test-dir",
    "resource-dir",
    "target-dir",
];
const JAVA_KEYS: &[&str] = &["source", "target", "encoding", "javac-args"];
const TOP_KEYS: &[&str] = &[
    "project",
    "java",
    "dependencies",
    "dev-dependencies",
    "repositories",
];

impl Manifest {
    /// Read and parse the manifest at `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Manifest> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                JrsError::manifest(format!(
                    "no `{MANIFEST_FILE}` found at {}\n\nrun `jrs init` to create one, \
                     or `jrs migrate` to convert an existing Maven or Gradle build",
                    path.display()
                ))
            } else {
                JrsError::io(path, e)
            }
        })?;
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Manifest::parse(&text, path, &root)
    }

    /// Find the manifest for `dir`, walking up until one is found.
    pub fn discover(start: &Path) -> Result<PathBuf> {
        let start = if start.is_absolute() {
            start.to_path_buf()
        } else {
            // `.` joined onto the working directory would leave a trailing `/.`
            // in the error message; drop the no-op components instead.
            let mut absolute = std::env::current_dir().path(".")?;
            for component in start.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        absolute.pop();
                    }
                    other => absolute.push(other),
                }
            }
            absolute
        };
        let mut dir = start.as_path();
        loop {
            let candidate = dir.join(MANIFEST_FILE);
            if candidate.is_file() {
                return Ok(candidate);
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => {
                    return Err(JrsError::manifest(format!(
                        "no `{MANIFEST_FILE}` in {} or any parent directory\n\n\
                         run `jrs init` to create one, or `jrs migrate` to convert an \
                         existing Maven or Gradle build",
                        start.display()
                    )));
                }
            }
        }
    }

    pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Manifest> {
        let table: toml::Table = toml::from_str(text).map_err(|e| {
            let where_ = e
                .span()
                .map(|s| {
                    let (line, col) = locate(text, s.start);
                    format!("{}:{line}:{col}: ", path.display())
                })
                .unwrap_or_else(|| format!("{}: ", path.display()));
            JrsError::manifest(format!("{where_}{}", e.message()))
        })?;

        let mut warnings = Vec::new();
        warn_unknown(&table, TOP_KEYS, "", &mut warnings);

        let project = table
            .get("project")
            .ok_or_else(|| JrsError::manifest("missing required table `[project]`"))?
            .as_table()
            .ok_or_else(|| JrsError::manifest("`project` must be a table"))?;
        warn_unknown(project, PROJECT_KEYS, "project.", &mut warnings);

        let name = required_string(project, "name", "project")?;
        validate_name(&name)?;
        let version = required_string(project, "version", "project")?;
        if version.trim().is_empty() {
            return Err(JrsError::manifest("`project.version` must not be empty"));
        }
        let main_class = optional_string(project, "main-class", "project")?;
        if let Some(mc) = &main_class {
            validate_class_name(mc)?;
        }

        let source_dir = path_or(project, "source-dir", "src/main/java", "project")?;
        let test_dir = path_or(project, "test-dir", "src/test/java", "project")?;
        let resource_dir = path_or(project, "resource-dir", "src/main/resources", "project")?;
        let target_dir = path_or(project, "target-dir", "target", "project")?;
        // Not a manifest key: test resources simply sit beside the test sources.
        let test_resource_dir = match test_dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.join("resources"),
            _ => PathBuf::from("src/test/resources"),
        };

        let java = match table.get("java") {
            None => JavaConfig::default(),
            Some(v) => {
                let t = v
                    .as_table()
                    .ok_or_else(|| JrsError::manifest("`java` must be a table"))?;
                warn_unknown(t, JAVA_KEYS, "java.", &mut warnings);
                let source = optional_release(t, "source")?;
                // `target` only means anything when it differs from `source`
                // (SPEC §4.2); normalising here keeps the rest of the codebase
                // from having to compare the two.
                let target = optional_release(t, "target")?.filter(|t| Some(*t) != source);
                let encoding =
                    optional_string(t, "encoding", "java")?.unwrap_or_else(|| "UTF-8".into());
                let javac_args = match t.get("javac-args") {
                    None => Vec::new(),
                    Some(v) => v
                        .as_array()
                        .ok_or_else(|| {
                            JrsError::manifest("`java.javac-args` must be an array of strings")
                        })?
                        .iter()
                        .map(|a| {
                            a.as_str().map(str::to_string).ok_or_else(|| {
                                JrsError::manifest("`java.javac-args` must be an array of strings")
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                };
                JavaConfig {
                    source,
                    target,
                    encoding,
                    javac_args,
                }
            }
        };

        let dependencies = parse_dependencies(&table, "dependencies")?;
        let dev_dependencies = parse_dependencies(&table, "dev-dependencies")?;
        for dev in &dev_dependencies {
            if dependencies.iter().any(|d| d.key() == dev.key()) {
                warnings.push(format!(
                    "`{}` is declared in both [dependencies] and [dev-dependencies]; \
                     the main declaration wins",
                    dev.key()
                ));
            }
        }
        let repositories = parse_repositories(&table)?;

        Ok(Manifest {
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            name,
            version,
            main_class,
            source_dir,
            test_dir,
            resource_dir,
            test_resource_dir,
            target_dir,
            java,
            dependencies,
            dev_dependencies,
            repositories,
            warnings,
        })
    }

    // ---- resolved paths ---------------------------------------------------

    pub fn source_path(&self) -> PathBuf {
        self.root.join(&self.source_dir)
    }
    pub fn test_path(&self) -> PathBuf {
        self.root.join(&self.test_dir)
    }
    pub fn resource_path(&self) -> PathBuf {
        self.root.join(&self.resource_dir)
    }
    pub fn test_resource_path(&self) -> PathBuf {
        self.root.join(&self.test_resource_dir)
    }
    pub fn target_path(&self) -> PathBuf {
        self.root.join(&self.target_dir)
    }
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// `my-app-1.0.0.jar`
    pub fn jar_name(&self) -> String {
        format!("{}-{}.jar", self.name, self.version)
    }

    /// The main class, or an error naming the command that needs it.
    pub fn require_main_class(&self, command: &str) -> Result<&str> {
        self.main_class.as_deref().ok_or_else(|| {
            JrsError::manifest(format!(
                "`jrs {command}` needs a main class\n\n\
                 add it to {}:\n\n    [project]\n    main-class = \"com.example.Main\"",
                self.path.display()
            ))
        })
    }

    /// Render this manifest back to TOML, optionally with a comment header.
    ///
    /// Used by `jrs init` and `jrs migrate`; the output is deliberately
    /// hand-formatted, since a generated manifest is something a human reads.
    pub fn render(&self, header: Option<&str>) -> String {
        let mut s = String::new();
        if let Some(h) = header {
            for line in h.lines() {
                let _ = writeln!(s, "# {line}");
            }
            s.push('\n');
        }
        let _ = writeln!(s, "[project]");
        let _ = writeln!(s, "name = {}", quote(&self.name));
        let _ = writeln!(s, "version = {}", quote(&self.version));
        if let Some(mc) = &self.main_class {
            let _ = writeln!(s, "main-class = {}", quote(mc));
        }
        for (key, value, default) in [
            ("source-dir", &self.source_dir, "src/main/java"),
            ("test-dir", &self.test_dir, "src/test/java"),
            ("resource-dir", &self.resource_dir, "src/main/resources"),
            ("target-dir", &self.target_dir, "target"),
        ] {
            let value = to_slash(value);
            if value != default {
                let _ = writeln!(s, "{key} = {}", quote(&value));
            }
        }

        let java = &self.java;
        if java.source.is_some()
            || java.target.is_some()
            || java.encoding != "UTF-8"
            || !java.javac_args.is_empty()
        {
            let _ = writeln!(s, "\n[java]");
            if let Some(v) = java.source {
                let _ = writeln!(s, "source = {v}");
            }
            if let Some(v) = java.target
                && Some(v) != java.source
            {
                let _ = writeln!(s, "target = {v}");
            }
            if java.encoding != "UTF-8" {
                let _ = writeln!(s, "encoding = {}", quote(&java.encoding));
            }
            if !java.javac_args.is_empty() {
                let args: Vec<String> = java.javac_args.iter().map(|a| quote(a)).collect();
                let _ = writeln!(s, "javac-args = [{}]", args.join(", "));
            }
        }

        if !self.dependencies.is_empty() {
            let _ = writeln!(s, "\n[dependencies]");
            for d in &self.dependencies {
                let _ = writeln!(s, "{} = {}", quote(&d.key()), quote(&d.version));
            }
        }
        if !self.dev_dependencies.is_empty() {
            let _ = writeln!(s, "\n[dev-dependencies]");
            for d in &self.dev_dependencies {
                let _ = writeln!(s, "{} = {}", quote(&d.key()), quote(&d.version));
            }
        }
        let extra: Vec<&Repository> = self
            .repositories
            .iter()
            .filter(|r| r.url.trim_end_matches('/') != CENTRAL_URL)
            .collect();
        if !extra.is_empty() {
            let _ = writeln!(s, "\n[repositories]");
            for r in extra {
                let _ = writeln!(s, "{} = {}", quote(&r.name), quote(&r.url));
            }
        }
        s
    }
}

/// A manifest with nothing but the required fields, used by `init` and `migrate`.
pub fn blank(name: &str, version: &str, root: &Path) -> Manifest {
    Manifest {
        path: root.join(MANIFEST_FILE),
        root: root.to_path_buf(),
        name: name.to_string(),
        version: version.to_string(),
        main_class: None,
        source_dir: PathBuf::from("src/main/java"),
        test_dir: PathBuf::from("src/test/java"),
        resource_dir: PathBuf::from("src/main/resources"),
        test_resource_dir: PathBuf::from("src/test/resources"),
        target_dir: PathBuf::from("target"),
        java: JavaConfig::default(),
        dependencies: Vec::new(),
        dev_dependencies: Vec::new(),
        repositories: vec![Repository {
            name: CENTRAL_NAME.into(),
            url: CENTRAL_URL.into(),
        }],
        warnings: Vec::new(),
    }
}

// ---- parsing helpers -------------------------------------------------------

fn parse_dependencies(table: &toml::Table, section: &str) -> Result<Vec<Dependency>> {
    let Some(value) = table.get(section) else {
        return Ok(Vec::new());
    };
    let deps = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;

    let mut out = Vec::with_capacity(deps.len());
    for (key, value) in deps {
        let (group, artifact) = split_coordinate(key, section)?;
        let version = match value {
            toml::Value::String(v) => v.clone(),
            toml::Value::Table(t) => {
                let known = ["version"];
                for k in t.keys() {
                    if !known.contains(&k.as_str()) {
                        return Err(JrsError::manifest(format!(
                            "`{section}.\"{key}\"`: unknown key `{k}` \
                             (expected only `version`)"
                        )));
                    }
                }
                t.get("version")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JrsError::manifest(format!(
                            "`{section}.\"{key}\"` is missing a `version` string"
                        ))
                    })?
                    .to_string()
            }
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{section}.\"{key}\"` must be a version string or a table \
                     with a `version` key"
                )));
            }
        };
        if version.trim().is_empty() {
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"` has an empty version"
            )));
        }
        out.push(Dependency {
            group,
            artifact,
            version,
        });
    }
    Ok(out)
}

fn split_coordinate(key: &str, section: &str) -> Result<(String, String)> {
    let mut parts = key.split(':');
    let group = parts.next().unwrap_or("");
    let artifact = parts.next().unwrap_or("");
    if group.is_empty() || artifact.is_empty() || parts.next().is_some() {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{key}\"`: dependency keys must be `group:artifact` \
             (the version belongs on the right-hand side)"
        )));
    }
    Ok((group.to_string(), artifact.to_string()))
}

fn parse_repositories(table: &toml::Table) -> Result<Vec<Repository>> {
    let mut repos = Vec::new();
    if let Some(value) = table.get("repositories") {
        let t = value
            .as_table()
            .ok_or_else(|| JrsError::manifest("`repositories` must be a table"))?;
        for (name, url) in t {
            let url = url.as_str().ok_or_else(|| {
                JrsError::manifest(format!("`repositories.{name}` must be a URL string"))
            })?;
            repos.push(Repository {
                name: name.clone(),
                url: url.trim_end_matches('/').to_string(),
            });
        }
    }
    // Maven Central is implicit and always last.
    if !repos.iter().any(|r| r.url == CENTRAL_URL) {
        repos.push(Repository {
            name: CENTRAL_NAME.into(),
            url: CENTRAL_URL.into(),
        });
    }
    Ok(repos)
}

fn required_string(t: &toml::Table, key: &str, section: &str) -> Result<String> {
    match t.get(key) {
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(JrsError::manifest(format!(
            "`{section}.{key}` must be a string"
        ))),
        None => Err(JrsError::manifest(format!(
            "missing required key `{section}.{key}`"
        ))),
    }
}

fn optional_string(t: &toml::Table, key: &str, section: &str) -> Result<Option<String>> {
    match t.get(key) {
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(JrsError::manifest(format!(
            "`{section}.{key}` must be a string"
        ))),
        None => Ok(None),
    }
}

fn path_or(t: &toml::Table, key: &str, default: &str, section: &str) -> Result<PathBuf> {
    let raw = optional_string(t, key, section)?.unwrap_or_else(|| default.to_string());
    let path = PathBuf::from(&raw);
    if path.is_absolute() || raw.contains("..") {
        return Err(JrsError::manifest(format!(
            "`{section}.{key}` must be a relative path inside the project (got `{raw}`)"
        )));
    }
    Ok(path)
}

/// `java.source` / `java.target` accept both `21` and `"21"`.
fn optional_release(t: &toml::Table, key: &str) -> Result<Option<u32>> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(Some(*n as u32)),
        Some(toml::Value::String(s)) => s
            .trim()
            .trim_start_matches("1.")
            .parse::<u32>()
            .map(Some)
            .map_err(|_| {
                JrsError::manifest(format!("`java.{key}`: `{s}` is not a Java release number"))
            }),
        Some(_) => Err(JrsError::manifest(format!(
            "`java.{key}` must be a release number, e.g. `21`"
        ))),
    }
}

fn warn_unknown(t: &toml::Table, known: &[&str], prefix: &str, warnings: &mut Vec<String>) {
    for key in t.keys() {
        if !known.contains(&key.as_str()) {
            warnings.push(format!("unknown key `{prefix}{key}` in jrs.toml (ignored)"));
        }
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(JrsError::manifest("`project.name` must not be empty"));
    }
    if name.contains(['/', '\\', ':', '\0']) || name == "." || name == ".." {
        return Err(JrsError::manifest(format!(
            "`project.name` must be a valid file name (got `{name}`); it is used \
             for the jar file"
        )));
    }
    Ok(())
}

fn validate_class_name(class: &str) -> Result<()> {
    let ok = !class.is_empty()
        && !class.starts_with('.')
        && !class.ends_with('.')
        && !class.contains("..")
        && class
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '$');
    if !ok {
        return Err(JrsError::manifest(format!(
            "`project.main-class` must be a fully-qualified class name (got `{class}`)"
        )));
    }
    Ok(())
}

/// Byte offset to 1-based line and column.
fn locate(text: &str, offset: usize) -> (usize, usize) {
    let head = &text[..offset.min(text.len())];
    let line = head.matches('\n').count() + 1;
    let col = head
        .rsplit('\n')
        .next()
        .map(|l| l.chars().count())
        .unwrap_or(0)
        + 1;
    (line, col)
}

fn quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Manifest> {
        Manifest::parse(text, Path::new("/p/jrs.toml"), Path::new("/p"))
    }

    const FULL: &str = r#"
[project]
name = "my-app"
version = "1.0.0"
main-class = "com.example.Main"

[java]
source = 21
target = 21
encoding = "UTF-8"
javac-args = ["-Xlint:all", "-Werror"]

[dependencies]
"com.google.guava:guava" = "33.0.0-jre"
"org.apache.commons:commons-lang3" = { version = "3.14.0" }

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"

[repositories]
internal = "https://nexus.example.com/repository/maven-public/"
"#;

    #[test]
    fn parses_the_full_example_from_the_spec() {
        let m = parse(FULL).unwrap();
        assert_eq!(m.name, "my-app");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.main_class.as_deref(), Some("com.example.Main"));
        assert_eq!(m.java.source, Some(21));
        assert_eq!(m.java.javac_args, vec!["-Xlint:all", "-Werror"]);
        assert_eq!(m.jar_name(), "my-app-1.0.0.jar");
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn short_and_long_dependency_forms_agree() {
        let m = parse(FULL).unwrap();
        assert_eq!(
            m.dependencies,
            vec![
                Dependency {
                    group: "com.google.guava".into(),
                    artifact: "guava".into(),
                    version: "33.0.0-jre".into()
                },
                Dependency {
                    group: "org.apache.commons".into(),
                    artifact: "commons-lang3".into(),
                    version: "3.14.0".into()
                },
            ]
        );
        assert_eq!(m.dev_dependencies.len(), 1);
    }

    #[test]
    fn declaration_order_survives_parsing() {
        // Conflict mediation breaks ties on declaration order, so this is load
        // bearing, not cosmetic.
        let m = parse(
            r#"
[project]
name = "a"
version = "1"
[dependencies]
"z.z:zeta" = "1"
"a.a:alpha" = "2"
"m.m:mu" = "3"
"#,
        )
        .unwrap();
        let keys: Vec<String> = m.dependencies.iter().map(|d| d.key()).collect();
        assert_eq!(keys, vec!["z.z:zeta", "a.a:alpha", "m.m:mu"]);
    }

    #[test]
    fn central_is_implicit_and_always_last() {
        let m = parse(FULL).unwrap();
        assert_eq!(m.repositories.len(), 2);
        assert_eq!(m.repositories[0].name, "internal");
        assert_eq!(
            m.repositories[0].url,
            "https://nexus.example.com/repository/maven-public"
        );
        assert_eq!(m.repositories[1].url, CENTRAL_URL);

        let bare = parse("[project]\nname='a'\nversion='1'").unwrap();
        assert_eq!(
            bare.repositories,
            vec![Repository {
                name: CENTRAL_NAME.into(),
                url: CENTRAL_URL.into()
            }]
        );
    }

    #[test]
    fn defaults_follow_the_maven_like_layout() {
        let m = parse("[project]\nname='a'\nversion='1'").unwrap();
        assert_eq!(m.source_dir, PathBuf::from("src/main/java"));
        assert_eq!(m.test_dir, PathBuf::from("src/test/java"));
        assert_eq!(m.resource_dir, PathBuf::from("src/main/resources"));
        assert_eq!(m.test_resource_dir, PathBuf::from("src/test/resources"));
        assert_eq!(m.target_dir, PathBuf::from("target"));
        assert_eq!(m.java.encoding, "UTF-8");
        assert_eq!(m.java.source, None);
    }

    #[test]
    fn flat_layouts_can_override_the_source_root() {
        let m =
            parse("[project]\nname='a'\nversion='1'\nsource-dir='src'\ntest-dir='test'").unwrap();
        assert_eq!(m.source_path(), PathBuf::from("/p/src"));
        assert_eq!(m.test_path(), PathBuf::from("/p/test"));
    }

    #[test]
    fn unknown_keys_warn_rather_than_fail() {
        let m =
            parse("[project]\nname='a'\nversion='1'\nfuture-key='x'\n[wat]\nk=1\n[java]\nlevel=9")
                .unwrap();
        assert_eq!(m.warnings.len(), 3, "{:?}", m.warnings);
        assert!(m.warnings.iter().any(|w| w.contains("project.future-key")));
        assert!(m.warnings.iter().any(|w| w.contains("`wat`")));
        assert!(m.warnings.iter().any(|w| w.contains("java.level")));
    }

    #[test]
    fn syntax_errors_name_the_line_and_column() {
        let err = parse("[project]\nname = \nversion = '1'").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("jrs.toml:2:"), "{msg}");
    }

    #[test]
    fn missing_required_keys_name_the_key() {
        let err = parse("[project]\nversion='1'").unwrap_err();
        assert!(err.to_string().contains("`project.name`"));
        let err = parse("[java]\nsource=21").unwrap_err();
        assert!(err.to_string().contains("[project]"));
    }

    #[test]
    fn a_dependency_key_carrying_a_version_is_rejected() {
        let err = parse("[project]\nname='a'\nversion='1'\n[dependencies]\n\"g:a:1.0\" = \"1.0\"")
            .unwrap_err();
        assert!(err.to_string().contains("group:artifact"), "{err}");
    }

    #[test]
    fn escaping_the_project_root_is_rejected() {
        let err = parse("[project]\nname='a'\nversion='1'\ntarget-dir='../elsewhere'").unwrap_err();
        assert!(err.to_string().contains("relative path"), "{err}");
    }

    #[test]
    fn a_name_that_is_not_a_file_name_is_rejected() {
        let err = parse("[project]\nname='a/b'\nversion='1'").unwrap_err();
        assert!(err.to_string().contains("valid file name"), "{err}");
    }

    #[test]
    fn old_style_java_versions_are_accepted() {
        let m = parse("[project]\nname='a'\nversion='1'\n[java]\nsource='1.8'").unwrap();
        assert_eq!(m.java.source, Some(8));
    }

    #[test]
    fn missing_main_class_names_the_command_that_wanted_it() {
        let m = parse("[project]\nname='a'\nversion='1'").unwrap();
        let err = m.require_main_class("run").unwrap_err();
        assert!(err.to_string().contains("`jrs run`"), "{err}");
        assert!(err.to_string().contains("main-class"));
    }

    #[test]
    fn duplicate_declarations_warn() {
        let m = parse(
            "[project]\nname='a'\nversion='1'\n[dependencies]\n'g:a'='1'\n[dev-dependencies]\n'g:a'='2'",
        )
        .unwrap();
        assert!(
            m.warnings.iter().any(|w| w.contains("both")),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn rendering_round_trips() {
        let original = parse(FULL).unwrap();
        let text = original.render(Some("generated by jrs"));
        assert!(text.starts_with("# generated by jrs\n"));
        let again = Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
        assert_eq!(again.name, original.name);
        assert_eq!(again.version, original.version);
        assert_eq!(again.main_class, original.main_class);
        assert_eq!(again.java, original.java);
        assert_eq!(again.dependencies, original.dependencies);
        assert_eq!(again.dev_dependencies, original.dev_dependencies);
        assert_eq!(again.repositories, original.repositories);
    }

    #[test]
    fn rendering_omits_defaults() {
        let m = blank("app", "0.1.0", Path::new("/p"));
        let text = m.render(None);
        assert!(!text.contains("source-dir"));
        assert!(!text.contains("[java]"));
        assert!(!text.contains("[repositories]"));
        assert!(text.contains("name = \"app\""));
    }
}
