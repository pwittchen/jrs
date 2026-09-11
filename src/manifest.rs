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

use crate::compile::lang::{self, Language};
use crate::error::{IoResultExt, JrsError, Result};
use crate::resolve::coord::{Ga, is_range};

pub const MANIFEST_FILE: &str = "jrs.toml";
pub const LOCK_FILE: &str = "jrs.lock";

/// A declared dependency: a `group:artifact` key and an exact version, plus
/// what the long table form adds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    pub group: String,
    pub artifact: String,
    pub version: String,
    /// `natives-linux`, `tests`, ...: a file published beside the main jar.
    pub classifier: Option<String>,
    /// Transitive dependencies not to walk, as `group:artifact` (`*` allowed).
    pub exclusions: Vec<Exclusion>,
    /// On the compile and test classpaths, but not the runtime one (Maven's
    /// `provided`, Gradle's `compileOnly`): an API the runtime supplies, or an
    /// annotation-only library.
    pub compile_only: bool,
    /// On the runtime and test classpaths, but not the one the main sources
    /// compile against (Maven's `runtime`, Gradle's `runtimeOnly`): a JDBC
    /// driver, an SLF4J binding.
    pub runtime_only: bool,
    /// A jar in the project rather than in a repository: the `path` of the
    /// long form, relative to the project root and `/`-separated, as written.
    /// A local jar has no coordinate. Its manifest key is its name, held in
    /// `artifact`, and `group` and `version` are empty.
    pub path: Option<String>,
}

/// An exclusion from a dependency's transitive graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Exclusion {
    pub group: String,
    pub artifact: String,
}

impl std::fmt::Display for Exclusion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.group, self.artifact)
    }
}

impl Dependency {
    pub fn new(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Dependency {
        Dependency {
            group: group.into(),
            artifact: artifact.into(),
            version: version.into(),
            classifier: None,
            exclusions: Vec::new(),
            compile_only: false,
            runtime_only: false,
            path: None,
        }
    }

    /// A local jar called `name`, at `path` relative to the project root.
    pub fn local(name: impl Into<String>, path: impl Into<String>) -> Dependency {
        let mut dep = Dependency::new("", name, "");
        dep.path = Some(path.into());
        dep
    }

    /// Whether this is a jar in the project rather than a Maven coordinate.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.path.is_some()
    }

    /// `group:artifact`, or `group:artifact:classifier` — unique within a table.
    /// A local jar's key is its name.
    #[must_use]
    pub fn key(&self) -> String {
        if self.is_local() {
            return self.artifact.clone();
        }
        match &self.classifier {
            Some(c) => format!("{}:{}:{c}", self.group, self.artifact),
            None => format!("{}:{}", self.group, self.artifact),
        }
    }

    /// True when the short `"g:a" = "version"` form says everything.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.exclusions.is_empty() && !self.compile_only && !self.runtime_only && !self.is_local()
    }

    /// A coordinate that leaves its version to `[managed]` (SPEC §8.9).
    #[must_use]
    pub fn is_managed(&self) -> bool {
        !self.is_local() && self.version.is_empty()
    }
}

impl std::fmt::Display for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(path) = &self.path {
            return write!(f, "{} ({path})", self.artifact);
        }
        write!(f, "{}:{}", self.group, self.artifact)?;
        if !self.version.is_empty() {
            write!(f, ":{}", self.version)?;
        }
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        Ok(())
    }
}

/// One `[managed]` entry: a version for an artifact wherever it turns up in
/// the graph, or a BOM whose `<dependencyManagement>` versions do (SPEC §8.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    pub group: String,
    pub artifact: String,
    pub version: String,
    /// Import this POM's managed versions rather than manage the artifact.
    pub bom: bool,
}

impl Managed {
    #[must_use]
    pub fn new(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Managed {
        Managed {
            group: group.into(),
            artifact: artifact.into(),
            version: version.into(),
            bom: false,
        }
    }

    /// A BOM at `version`.
    #[must_use]
    pub fn bom(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Managed {
        Managed {
            bom: true,
            ..Managed::new(group, artifact, version)
        }
    }

    /// `group:artifact`, as the table's key.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub url: String,
    /// The groups this repository serves, from the long form's `groups`:
    /// `com.acme` for that group, `com.acme.*` for the groups under it. Empty
    /// for a repository asked for everything. A group one of these matches is
    /// looked up only in the repositories that claim it (`resolve::repo`).
    pub groups: Vec<String>,
}

impl Repository {
    /// A repository asked for every group.
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Repository {
        Repository {
            name: name.into(),
            url: url.into(),
            groups: Vec::new(),
        }
    }

    /// Whether this repository's `groups` name `group`. A repository without
    /// `groups` claims nothing: it is asked for what no other claims.
    #[must_use]
    pub fn claims(&self, group: &str) -> bool {
        self.groups
            .iter()
            .any(|pattern| group_matches(pattern, group))
    }
}

/// Whether `pattern` is something `groups` can hold: a group (`com.acme`), or
/// a group followed by `.*` for the groups under it. No other wildcard.
#[must_use]
pub fn valid_group_pattern(pattern: &str) -> bool {
    let base = pattern.strip_suffix(".*").unwrap_or(pattern);
    !base.is_empty()
        && !base.starts_with('.')
        && !base.ends_with('.')
        && !base.contains("..")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `com.acme` matches that group alone; `com.acme.*` matches the groups below
/// it (`com.acme.billing`), but not `com.acme` itself.
#[must_use]
pub fn group_matches(pattern: &str, group: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(prefix) => group
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1),
        None => pattern == group,
    }
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
    /// Appended verbatim to `jrs doc`'s `javadoc` invocation.
    pub javadoc_args: Vec<String>,
    /// The JDK feature version to build with, when the project pins one. A
    /// version, not a path: a path is a property of one machine, and the
    /// manifest is committed.
    pub jdk: Option<u32>,
}

impl Default for JavaConfig {
    fn default() -> Self {
        JavaConfig {
            source: None,
            target: None,
            encoding: "UTF-8".to_string(),
            javac_args: Vec::new(),
            javadoc_args: Vec::new(),
            jdk: None,
        }
    }
}

/// `[run]`: how `jrs run` starts the JVM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunConfig {
    /// Placed before `-cp`: `-Xmx512m`, `-Dkey=value`, `--enable-preview`.
    pub jvm_args: Vec<String>,
    /// Agents passed as `-javaagent:`, named by `group:artifact` and taken
    /// from the resolved runtime classpath, so each is the pinned version.
    pub java_agents: Vec<Ga>,
    /// Added to the environment the program inherits, in declaration order.
    pub env: Vec<(String, Template)>,
    /// The program's working directory, relative to the project root. `None`
    /// keeps the directory jrs was started in.
    pub cwd: Option<Template>,
}

/// `[test]`: how `jrs test` starts the test JVM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestConfig {
    pub jvm_args: Vec<String>,
    /// The `JaCoCo` release `jrs test --coverage` uses, for a JDK newer than
    /// jrs's default knows about.
    pub jacoco_version: Option<String>,
    /// Agents passed as `-javaagent:`, ahead of `JaCoCo`'s, taken from the
    /// resolved test classpath (dev-dependencies included).
    pub java_agents: Vec<Ga>,
    /// Added to the environment the test JVM inherits, in declaration order.
    pub env: Vec<(String, Template)>,
    /// `test.retries`: how many times a failed test is run again before it
    /// counts as failed. One that passes on a retry is reported as flaky.
    pub retries: u32,
    /// `test.coverage-minimum`: project-wide totals `jrs test --coverage`
    /// must reach, in declaration order. Ignored without `--coverage`.
    pub coverage_minimum: Vec<CoverageMinimum>,
}

/// A `JaCoCo` counter `test.coverage-minimum` can set a minimum for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageCounter {
    Instruction,
    Branch,
    Line,
    Complexity,
    Method,
    Class,
}

impl CoverageCounter {
    pub const ALL: [CoverageCounter; 6] = [
        CoverageCounter::Instruction,
        CoverageCounter::Branch,
        CoverageCounter::Line,
        CoverageCounter::Complexity,
        CoverageCounter::Method,
        CoverageCounter::Class,
    ];

    /// Its key in `test.coverage-minimum`: `line`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            CoverageCounter::Instruction => "instruction",
            CoverageCounter::Branch => "branch",
            CoverageCounter::Line => "line",
            CoverageCounter::Complexity => "complexity",
            CoverageCounter::Method => "method",
            CoverageCounter::Class => "class",
        }
    }

    /// The `type` of its `<counter>` in `jacoco.xml`: `LINE`.
    #[must_use]
    pub fn jacoco_type(self) -> &'static str {
        match self {
            CoverageCounter::Instruction => "INSTRUCTION",
            CoverageCounter::Branch => "BRANCH",
            CoverageCounter::Line => "LINE",
            CoverageCounter::Complexity => "COMPLEXITY",
            CoverageCounter::Method => "METHOD",
            CoverageCounter::Class => "CLASS",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<CoverageCounter> {
        CoverageCounter::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// One entry of `test.coverage-minimum`: the covered ratio a counter's total
/// must reach, kept in hundredths of a percent (`0.8` is `8000`) so that the
/// comparison with `JaCoCo`'s counts is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageMinimum {
    pub counter: CoverageCounter,
    pub basis_points: u32,
}

impl CoverageMinimum {
    /// The ratio as the manifest writes it: `0.8`, `0.755`, `1.0`.
    #[must_use]
    pub fn ratio(&self) -> String {
        let digits = format!(
            "{}.{:04}",
            self.basis_points / 10_000,
            self.basis_points % 10_000
        );
        let trimmed = digits.trim_end_matches('0');
        if trimmed.ends_with('.') {
            format!("{trimmed}0")
        } else {
            trimmed.to_string()
        }
    }
}

/// `[package]`: jar attributes, runtime images, native images.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageConfig {
    /// Modules added to a `--jlink` / `--jpackage` runtime beyond those `jdeps`
    /// finds — ones reached only by reflection or `ServiceLoader`, such as
    /// `jdk.crypto.ec` for TLS.
    pub add_modules: Vec<String>,
    /// `[package.manifest]`: extra `META-INF/MANIFEST.MF` attributes, in
    /// declaration order, so the jar is byte-identical from build to build.
    /// Values may use `{project.name}` and `{project.version}`.
    pub manifest: Vec<(String, Template)>,
    /// Passed through verbatim to `native-image` by `--native-image`.
    pub native_image_args: Vec<String>,
}

/// The `MANIFEST.MF` main attributes jrs writes itself, which
/// `[package.manifest]` may not set. `Name` is here too: it starts a
/// per-entry section, and jrs writes the main section only.
pub const JAR_ATTRIBUTES_OWNED: &[&str] = &[
    "Manifest-Version",
    "Created-By",
    "Main-Class",
    "Class-Path",
    "Name",
];

/// `[kotlin]`, `[scala]` or `[groovy]`: the table's presence turns the
/// language on, and its `version` pins the compiler (`JVM_LANGUAGES.md` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageConfig {
    pub language: Language,
    /// The compiler's version, and the implied runtime library's.
    pub version: String,
    /// An extra main source root: `src/main/<lang>` unless set.
    pub source_dir: PathBuf,
    /// An extra test source root: `src/test/<lang>` unless set.
    pub test_dir: PathBuf,
    /// `kotlinc-args`, `scalac-args` or `groovyc-args`, appended verbatim
    /// after jrs's own flags.
    pub compiler_args: Vec<String>,
    /// `java` flags for the JVM the compiler runs in: heap and stack size.
    pub compiler_jvm_args: Vec<String>,
}

/// A `{placeholder}` in a task's value, expanded by jrs before the process
/// starts (TASKS.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placeholder {
    Root,
    Target,
    ProjectName,
    ProjectVersion,
    Classes,
    TestClasses,
    Classpath,
    RuntimeClasspath,
    TestClasspath,
    ClasspathArgfile,
    Jar,
}

impl Placeholder {
    pub const ALL: [Placeholder; 11] = [
        Placeholder::Root,
        Placeholder::Target,
        Placeholder::ProjectName,
        Placeholder::ProjectVersion,
        Placeholder::Classes,
        Placeholder::TestClasses,
        Placeholder::Classpath,
        Placeholder::RuntimeClasspath,
        Placeholder::TestClasspath,
        Placeholder::ClasspathArgfile,
        Placeholder::Jar,
    ];

    /// The name between the braces.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Placeholder::Root => "root",
            Placeholder::Target => "target",
            Placeholder::ProjectName => "project.name",
            Placeholder::ProjectVersion => "project.version",
            Placeholder::Classes => "classes",
            Placeholder::TestClasses => "test-classes",
            Placeholder::Classpath => "classpath",
            Placeholder::RuntimeClasspath => "runtime-classpath",
            Placeholder::TestClasspath => "test-classpath",
            Placeholder::ClasspathArgfile => "classpath-argfile",
            Placeholder::Jar => "jar",
        }
    }

    /// The ones whose value needs the resolved dependency graph.
    #[must_use]
    pub fn is_classpath(self) -> bool {
        matches!(
            self,
            Placeholder::Classpath
                | Placeholder::RuntimeClasspath
                | Placeholder::TestClasspath
                | Placeholder::ClasspathArgfile
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Placeholder(Placeholder),
}

/// A string that may hold placeholders, parsed once when the manifest loads
/// so that a typo fails then rather than as a baffling tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// As written in `jrs.toml`.
    pub raw: String,
    pub segments: Vec<Segment>,
}

impl Template {
    /// Parse `raw`. `{{` and `}}` are literal braces.
    ///
    /// # Errors
    ///
    /// A message (without the key, which the caller knows) for an unknown
    /// placeholder, an unclosed `{` or a stray `}`.
    pub fn parse(raw: &str) -> std::result::Result<Template, String> {
        let mut segments = Vec::new();
        let mut text = String::new();
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(ch) => name.push(ch),
                            None => {
                                return Err(format!(
                                    "`{raw}` has an unclosed `{{`; write `{{{{` for a literal brace"
                                ));
                            }
                        }
                    }
                    let placeholder = Placeholder::ALL
                        .into_iter()
                        .find(|p| p.name() == name)
                        .ok_or_else(|| {
                            let known: Vec<String> = Placeholder::ALL
                                .iter()
                                .map(|p| format!("`{{{}}}`", p.name()))
                                .collect();
                            format!(
                                "unknown placeholder `{{{name}}}` (known: {})",
                                known.join(", ")
                            )
                        })?;
                    if !text.is_empty() {
                        segments.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    segments.push(Segment::Placeholder(placeholder));
                }
                '}' => {
                    return Err(format!(
                        "`{raw}` has a stray `}}`; write `}}}}` for a literal brace"
                    ));
                }
                _ => text.push(c),
            }
        }
        if !text.is_empty() {
            segments.push(Segment::Text(text));
        }
        Ok(Template {
            raw: raw.to_string(),
            segments,
        })
    }

    /// A template that is all text: braces in `text` are escaped, so nothing
    /// in it reads as a placeholder.
    #[must_use]
    pub fn literal(text: &str) -> Template {
        Template {
            raw: text.replace('{', "{{").replace('}', "}}"),
            segments: if text.is_empty() {
                Vec::new()
            } else {
                vec![Segment::Text(text.to_string())]
            },
        }
    }

    pub fn placeholders(&self) -> impl Iterator<Item = Placeholder> + '_ {
        self.segments.iter().filter_map(|s| match s {
            Segment::Placeholder(p) => Some(*p),
            Segment::Text(_) => None,
        })
    }

    /// Substitute every placeholder with what `value` says it is.
    ///
    /// # Errors
    ///
    /// Whatever `value` returns for a placeholder it cannot supply.
    pub fn expand(&self, mut value: impl FnMut(Placeholder) -> Result<String>) -> Result<String> {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(t) => out.push_str(t),
                Segment::Placeholder(p) => out.push_str(&value(*p)?),
            }
        }
        Ok(out)
    }
}

/// A built-in command a task may depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    Build,
    Test,
    Package,
    Doc,
}

impl Builtin {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Builtin::Build => "build",
            Builtin::Test => "test",
            Builtin::Package => "package",
            Builtin::Doc => "doc",
        }
    }

    fn parse(s: &str) -> Option<Builtin> {
        [
            Builtin::Build,
            Builtin::Test,
            Builtin::Package,
            Builtin::Doc,
        ]
        .into_iter()
        .find(|b| b.name() == s)
    }

    /// The hooks this command fires, in the order it reaches them. Commands
    /// include each other, so their hooks do too.
    #[must_use]
    pub fn hooks(self) -> &'static [Hook] {
        match self {
            Builtin::Build => &[Hook::PreCompile, Hook::PostCompile],
            Builtin::Test => &[
                Hook::PreCompile,
                Hook::PostCompile,
                Hook::PreTest,
                Hook::PostTest,
            ],
            Builtin::Package => &[Hook::PreCompile, Hook::PostCompile, Hook::PostPackage],
            Builtin::Doc => &[Hook::PreCompile],
        }
    }
}

/// One entry of a task's `depends-on`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskRef {
    Task(String),
    Builtin(Builtin),
}

impl std::fmt::Display for TaskRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskRef::Task(name) => f.write_str(name),
            TaskRef::Builtin(b) => f.write_str(b.name()),
        }
    }
}

/// A fixed point in a built-in command where `[hooks]` runs tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hook {
    PreCompile,
    PostCompile,
    PreTest,
    PostTest,
    PostPackage,
    PreRun,
}

impl Hook {
    pub const ALL: [Hook; 6] = [
        Hook::PreCompile,
        Hook::PostCompile,
        Hook::PreTest,
        Hook::PostTest,
        Hook::PostPackage,
        Hook::PreRun,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Hook::PreCompile => "pre-compile",
            Hook::PostCompile => "post-compile",
            Hook::PreTest => "pre-test",
            Hook::PostTest => "post-test",
            Hook::PostPackage => "post-package",
            Hook::PreRun => "pre-run",
        }
    }
}

impl std::fmt::Display for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What a task runs: at most one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// An argument vector, no shell involved.
    Run(Vec<Template>),
    /// One string for `sh -c` / `cmd /C`. No placeholders: the shell expands
    /// the `JRS_*` variables itself.
    Shell(String),
    /// A `.java` file for the JDK's single-file source launcher.
    Script(Template),
    /// A class run from the task's own `dependencies` (TASKS.md §8).
    Main(String),
}

/// `[tasks.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDef {
    pub name: String,
    pub description: Option<String>,
    /// `None` for a task that only aggregates its `depends-on`.
    pub action: Option<Action>,
    pub args: Vec<Template>,
    pub depends_on: Vec<TaskRef>,
    /// In declaration order.
    pub env: Vec<(String, Template)>,
    pub cwd: Option<Template>,
    pub inputs: Vec<Template>,
    pub outputs: Vec<Template>,
    pub source_outputs: Vec<Template>,
    pub resource_outputs: Vec<Template>,
    /// `[tasks.<name>.dependencies]`: Java tools from a repository, resolved
    /// as a graph of the task's own and pinned in `jrs.lock` (TASKS.md §8).
    pub dependencies: Vec<Dependency>,
}

impl TaskDef {
    /// The name of the `[[tool]]` block that pins this task's dependencies:
    /// `tasks.format`, which no compiler's name can be.
    #[must_use]
    pub fn tool_name(&self) -> String {
        format!("tasks.{}", self.name)
    }

    /// Every template the task holds, with the key it came from.
    pub fn templates(&self) -> impl Iterator<Item = (&'static str, &Template)> + '_ {
        let action: Vec<(&'static str, &Template)> = match &self.action {
            Some(Action::Run(argv)) => argv.iter().map(|t| ("run", t)).collect(),
            Some(Action::Script(t)) => vec![("script", t)],
            Some(Action::Shell(_) | Action::Main(_)) | None => Vec::new(),
        };
        action
            .into_iter()
            .chain(self.args.iter().map(|t| ("args", t)))
            .chain(self.env.iter().map(|(_, t)| ("env", t)))
            .chain(self.cwd.iter().map(|t| ("cwd", t)))
            .chain(self.path_templates())
    }

    /// The templates that name files or directories.
    pub fn path_templates(&self) -> impl Iterator<Item = (&'static str, &Template)> + '_ {
        self.inputs
            .iter()
            .map(|t| ("inputs", t))
            .chain(self.outputs.iter().map(|t| ("outputs", t)))
            .chain(self.source_outputs.iter().map(|t| ("source-outputs", t)))
            .chain(
                self.resource_outputs
                    .iter()
                    .map(|t| ("resource-outputs", t)),
            )
    }

    #[must_use]
    pub fn uses(&self, placeholder: Placeholder) -> bool {
        self.templates()
            .any(|(_, t)| t.placeholders().any(|p| p == placeholder))
    }
}

/// `[hooks]`: for each lifecycle point, the tasks it runs, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hooks(Vec<(Hook, Vec<String>)>);

impl Hooks {
    #[must_use]
    pub fn tasks(&self, hook: Hook) -> &[String] {
        self.0
            .iter()
            .find(|(h, _)| *h == hook)
            .map_or(&[], |(_, names)| names.as_slice())
    }

    pub fn iter(&self) -> impl Iterator<Item = (Hook, &[String])> + '_ {
        self.0.iter().map(|(h, names)| (*h, names.as_slice()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Append `task` to `hook`'s list, unless it is there already. The hooks
    /// stay in [`Hook::ALL`] order, as the parser leaves them.
    pub fn add(&mut self, hook: Hook, task: &str) {
        if let Some((_, names)) = self.0.iter_mut().find(|(h, _)| *h == hook) {
            if !names.iter().any(|n| n == task) {
                names.push(task.to_string());
            }
            return;
        }
        self.0.push((hook, vec![task.to_string()]));
        self.0
            .sort_by_key(|(h, _)| Hook::ALL.iter().position(|a| a == h));
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
    pub run: RunConfig,
    pub test: TestConfig,
    pub package: PackageConfig,
    /// The languages turned on besides Java, in `Language::FOREIGN` order.
    pub languages: Vec<LanguageConfig>,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    /// `[managed]`, in declaration order: pinned versions and BOMs.
    pub managed: Vec<Managed>,
    /// User repositories in declaration order, with Central appended last.
    pub repositories: Vec<Repository>,
    /// `[tasks]`, in declaration order.
    pub tasks: Vec<TaskDef>,
    pub hooks: Hooks,

    /// Non-fatal complaints, surfaced by the CLI after the manifest loads.
    pub warnings: Vec<String>,
    /// `project.jrs-version`: the oldest jrs that may build the project, as
    /// written. Checked when the manifest is parsed.
    pub jrs_version: Option<String>,
}

const PROJECT_KEYS: &[&str] = &[
    "name",
    "version",
    "main-class",
    "source-dir",
    "test-dir",
    "resource-dir",
    "test-resource-dir",
    "target-dir",
    "jrs-version",
];
const JAVA_KEYS: &[&str] = &[
    "source",
    "target",
    "encoding",
    "javac-args",
    "javadoc-args",
    "jdk",
];
const RUN_KEYS: &[&str] = &["jvm-args", "java-agents", "env", "cwd"];
const TEST_KEYS: &[&str] = &[
    "jvm-args",
    "jacoco-version",
    "java-agents",
    "env",
    "retries",
    "coverage-minimum",
];
const PACKAGE_KEYS: &[&str] = &["add-modules", "manifest", "native-image-args"];
const DEPENDENCY_KEYS: &[&str] = &[
    "version",
    "classifier",
    "exclusions",
    "compile-only",
    "runtime-only",
    "path",
];
/// What a local jar's table may hold: no version, classifier or exclusions,
/// since it has no coordinate and no graph.
const LOCAL_DEPENDENCY_KEYS: &[&str] = &["path", "compile-only", "runtime-only"];
const MANAGED_KEYS: &[&str] = &["version", "bom"];
const REPOSITORY_KEYS: &[&str] = &["url", "groups"];
const TASK_KEYS: &[&str] = &[
    "description",
    "run",
    "shell",
    "script",
    "main",
    "dependencies",
    "args",
    "depends-on",
    "env",
    "cwd",
    "inputs",
    "outputs",
    "source-outputs",
    "resource-outputs",
];
const TOP_KEYS: &[&str] = &[
    "project",
    "java",
    "run",
    "test",
    "package",
    "kotlin",
    "scala",
    "groovy",
    "dependencies",
    "dev-dependencies",
    "managed",
    "repositories",
    "tasks",
    "hooks",
];

/// Every `jrs` subcommand. A task may not take one of these names, so that
/// `depends-on` is never ambiguous and a task can never shadow a command
/// (`cli.rs` has a test that keeps this in step with the command tree).
pub const RESERVED_TASK_NAMES: &[&str] = &[
    "build",
    "test",
    "run",
    "package",
    "doc",
    "clean",
    "tree",
    "classpath",
    "update",
    "verify",
    "outdated",
    "add",
    "remove",
    "cache",
    "init",
    "migrate",
    "completions",
    "task",
    "help",
    "metadata",
    "fetch",
];

impl Manifest {
    /// Read and parse the manifest at `path`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when there is no file at `path`, or as for
    /// [`Manifest::parse`]; [`JrsError::Io`] when it exists but cannot be read.
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
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when neither `start` nor any parent holds one;
    /// [`JrsError::Io`] when `start` is relative and the working directory
    /// cannot be read.
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

    /// Parse manifest `text`. `path` is where it was read from and `root` the
    /// project directory its relative paths are resolved against.
    ///
    /// Unknown keys are not errors; they end up in `warnings`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when `text` is not TOML, `[project]` or one of
    /// its required keys is missing, or any value has the wrong type or an
    /// invalid form (a name, class name, path, release, dependency or
    /// repository).
    #[allow(
        clippy::similar_names,
        reason = "`test` is the `[test]` table, named like `run` and `package` beside it; \
                  `text` is the manifest source"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one short step per top-level table, in the order a reader of jrs.toml \
                  meets them; each table's own parsing is already a helper"
    )]
    pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Manifest> {
        let table: toml::Table = toml::from_str(text).map_err(|e| {
            let where_ = e.span().map_or_else(
                || format!("{}: ", path.display()),
                |s| {
                    let (line, col) = locate(text, s.start);
                    format!("{}:{line}:{col}: ", path.display())
                },
            );
            JrsError::manifest(format!("{where_}{}", e.message()))
        })?;

        let mut warnings = Vec::new();
        warn_unknown(&table, TOP_KEYS, "", &mut warnings);

        let project = table
            .get("project")
            .ok_or_else(|| JrsError::manifest("missing required table `[project]`"))?
            .as_table()
            .ok_or_else(|| JrsError::manifest("`project` must be a table"))?;
        // Before anything else is read: a manifest written for a newer jrs
        // may use keys and forms this one would misread or warn about, and
        // the one thing worth saying then is which jrs it needs.
        let jrs_version = parse_jrs_version(project, path)?;
        warn_unknown(project, PROJECT_KEYS, "project.", &mut warnings);

        let name = required_string(project, "name", "project")?;
        validate_name(&name)?;
        let version = required_string(project, "version", "project")?;
        if version.trim().is_empty() {
            return Err(JrsError::manifest("`project.version` must not be empty"));
        }
        let main_class = optional_string(project, "main-class", "project")?;
        if let Some(mc) = &main_class {
            validate_class_name(mc, "project.main-class")?;
        }

        let source_dir = path_or(project, "source-dir", "src/main/java", "project")?;
        let test_dir = path_or(project, "test-dir", "src/test/java", "project")?;
        let resource_dir = path_or(project, "resource-dir", "src/main/resources", "project")?;
        let target_dir = path_or(project, "target-dir", "target", "project")?;
        // By default test resources sit beside the test sources.
        let test_resource_dir = match project.get("test-resource-dir") {
            Some(_) => path_or(project, "test-resource-dir", "", "project")?,
            None => default_test_resource_dir(&test_dir),
        };

        let java = match section(&table, "java", JAVA_KEYS, &mut warnings)? {
            None => JavaConfig::default(),
            Some(t) => java_config(t)?,
        };

        let run = match section(&table, "run", RUN_KEYS, &mut warnings)? {
            None => RunConfig::default(),
            Some(t) => RunConfig {
                jvm_args: string_array(t, "jvm-args", "run")?,
                java_agents: parse_java_agents(t, "run")?,
                env: parse_jvm_env(t, "run")?,
                cwd: parse_jvm_cwd(t, "run")?,
            },
        };
        let test = match section(&table, "test", TEST_KEYS, &mut warnings)? {
            None => TestConfig::default(),
            Some(t) => TestConfig {
                jvm_args: string_array(t, "jvm-args", "test")?,
                jacoco_version: optional_string(t, "jacoco-version", "test")?,
                java_agents: parse_java_agents(t, "test")?,
                env: parse_jvm_env(t, "test")?,
                retries: optional_count(t, "retries", "test")?,
                coverage_minimum: coverage_minimum(t, &mut warnings)?,
            },
        };
        let package = match section(&table, "package", PACKAGE_KEYS, &mut warnings)? {
            None => PackageConfig::default(),
            Some(t) => PackageConfig {
                add_modules: string_array(t, "add-modules", "package")?,
                manifest: parse_jar_attributes(t)?,
                native_image_args: string_array(t, "native-image-args", "package")?,
            },
        };

        let languages = parse_languages(&table, &mut warnings)?;
        let dependencies = parse_dependencies(&table, "dependencies")?;
        let dev_dependencies = parse_dependencies(&table, "dev-dependencies")?;
        if let Some(d) = dev_dependencies.iter().find(|d| d.compile_only) {
            return Err(JrsError::manifest(format!(
                "`dev-dependencies.\"{}\"`: `compile-only` only means something in \
                 [dependencies]; a dev-dependency is never on the runtime classpath anyway",
                d.key()
            )));
        }
        if let Some(d) = dev_dependencies.iter().find(|d| d.runtime_only) {
            return Err(JrsError::manifest(format!(
                "`dev-dependencies.\"{}\"`: `runtime-only` only means something in \
                 [dependencies]; the test classpath is one classpath, that the tests \
                 compile and run against alike",
                d.key()
            )));
        }
        for dev in &dev_dependencies {
            if dependencies.iter().any(|d| d.key() == dev.key()) {
                warnings.push(format!(
                    "`{}` is declared in both [dependencies] and [dev-dependencies]; \
                     the main declaration wins",
                    dev.key()
                ));
            }
        }
        let managed = parse_managed(&table)?;
        check_versionless(&dependencies, &dev_dependencies, &managed)?;
        let repositories = parse_repositories(&table)?;
        let tasks = parse_tasks(&table, &mut warnings)?;
        let hooks = parse_hooks(&table, &mut warnings)?;

        let mut manifest = Manifest {
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
            run,
            test,
            package,
            languages,
            dependencies,
            dev_dependencies,
            managed,
            repositories,
            tasks,
            hooks,
            warnings,
            jrs_version,
        };
        let language_warnings = check_languages(&manifest)?;
        manifest.warnings.extend(language_warnings);
        // What needs the whole manifest at once: references between tasks,
        // cycles, and where a placeholder is available.
        let task_warnings = crate::task::check(&manifest)?;
        manifest.warnings.extend(task_warnings);
        Ok(manifest)
    }

    /// The task `[tasks.<name>]` declares.
    #[must_use]
    pub fn task(&self, name: &str) -> Option<&TaskDef> {
        self.tasks.iter().find(|t| t.name == name)
    }

    /// The table that turns `language` on, when the manifest has one.
    #[must_use]
    pub fn language(&self, language: Language) -> Option<&LanguageConfig> {
        self.languages.iter().find(|c| c.language == language)
    }

    /// The runtime libraries the languages imply, each with its language:
    /// the ones the manifest does not declare itself, in either table
    /// (`JVM_LANGUAGES.md` §4.3). They are never written into `jrs.toml`.
    #[must_use]
    pub fn implied_dependencies(&self) -> Vec<(Dependency, Language)> {
        let declared = |group: &str, artifact: &str| {
            self.dependencies
                .iter()
                .chain(&self.dev_dependencies)
                .any(|d| d.group == group && d.artifact == artifact && d.classifier.is_none())
        };
        let mut implied = Vec::new();
        for config in &self.languages {
            for (group, artifact) in config.language.runtime_libraries(&config.version) {
                if !declared(group, artifact) {
                    implied.push((
                        Dependency::new(group, artifact, &config.version),
                        config.language,
                    ));
                }
            }
        }
        implied
    }

    /// `[dependencies]` as resolution sees them: the declared ones, then the
    /// implied runtime libraries as if declared last. Each implied library is
    /// then a direct dependency, and beats a transitive copy under
    /// nearest-wins; a declared dependency still wins a tie at the same depth.
    #[must_use]
    pub fn effective_dependencies(&self) -> Vec<Dependency> {
        let mut dependencies = self.dependencies.clone();
        dependencies.extend(self.implied_dependencies().into_iter().map(|(d, _)| d));
        dependencies
    }

    // ---- resolved paths ---------------------------------------------------

    #[must_use]
    pub fn source_path(&self) -> PathBuf {
        self.root.join(&self.source_dir)
    }
    #[must_use]
    pub fn test_path(&self) -> PathBuf {
        self.root.join(&self.test_dir)
    }
    #[must_use]
    pub fn resource_path(&self) -> PathBuf {
        self.root.join(&self.resource_dir)
    }
    #[must_use]
    pub fn test_resource_path(&self) -> PathBuf {
        self.root.join(&self.test_resource_dir)
    }
    #[must_use]
    pub fn target_path(&self) -> PathBuf {
        self.root.join(&self.target_dir)
    }
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// `my-app-1.0.0.jar`
    #[must_use]
    pub fn jar_name(&self) -> String {
        format!("{}-{}.jar", self.name, self.version)
    }

    /// The main class, or an error naming the command that needs it.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when the manifest names no `main-class`.
    pub fn require_main_class(&self, command: &str) -> Result<&str> {
        self.main_class.as_deref().ok_or_else(|| {
            JrsError::manifest(format!(
                "`jrs {command}` needs a main class\n\n\
                 add it to {}:\n\n    [project]\n    main-class = \"com.example.Main\"",
                self.path.display()
            ))
        })
    }

    /// `[package.manifest]` with its placeholders expanded: the attributes a
    /// jar's `MANIFEST.MF` gets after jrs's own, in declaration order.
    ///
    /// # Errors
    ///
    /// None in practice: the parser admits only `{project.name}` and
    /// `{project.version}`, which are always known.
    pub fn jar_attributes(&self) -> Result<Vec<(String, String)>> {
        self.package
            .manifest
            .iter()
            .map(|(name, value)| {
                let expanded = value.expand(|p| match p {
                    Placeholder::ProjectName => Ok(self.name.clone()),
                    Placeholder::ProjectVersion => Ok(self.version.clone()),
                    other => Err(JrsError::manifest(format!(
                        "`package.manifest.{name}`: `{{{}}}` is not available here",
                        other.name()
                    ))),
                })?;
                Ok((name.clone(), expanded))
            })
            .collect()
    }

    /// Render this manifest back to TOML, optionally with a comment header.
    ///
    /// Used by `jrs init` and `jrs migrate`; the output is deliberately
    /// hand-formatted, since a generated manifest is something a human reads.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one short block per table, in the order jrs.toml lists them"
    )]
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
        let test_resources = to_slash(&self.test_resource_dir);
        if test_resources != to_slash(&default_test_resource_dir(&self.test_dir)) {
            let _ = writeln!(s, "test-resource-dir = {}", quote(&test_resources));
        }
        if let Some(v) = &self.jrs_version {
            let _ = writeln!(s, "jrs-version = {}", quote(v));
        }

        let java = &self.java;
        if java.source.is_some()
            || java.target.is_some()
            || java.encoding != "UTF-8"
            || !java.javac_args.is_empty()
            || !java.javadoc_args.is_empty()
            || java.jdk.is_some()
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
                let _ = writeln!(s, "javac-args = {}", quote_list(&java.javac_args));
            }
            if !java.javadoc_args.is_empty() {
                let _ = writeln!(s, "javadoc-args = {}", quote_list(&java.javadoc_args));
            }
            if let Some(v) = java.jdk {
                let _ = writeln!(s, "jdk = {v}");
            }
        }
        for config in &self.languages {
            let key = config.language.key();
            let _ = writeln!(s, "\n[{key}]");
            let _ = writeln!(s, "version = {}", quote(&config.version));
            for (name, value, default) in [
                ("source-dir", &config.source_dir, format!("src/main/{key}")),
                ("test-dir", &config.test_dir, format!("src/test/{key}")),
            ] {
                let value = to_slash(value);
                if value != default {
                    let _ = writeln!(s, "{name} = {}", quote(&value));
                }
            }
            if !config.compiler_args.is_empty() {
                let _ = writeln!(
                    s,
                    "{} = {}",
                    config.language.args_key(),
                    quote_list(&config.compiler_args)
                );
            }
            if !config.compiler_jvm_args.is_empty() {
                let _ = writeln!(
                    s,
                    "compiler-jvm-args = {}",
                    quote_list(&config.compiler_jvm_args)
                );
            }
        }
        if self.run != RunConfig::default() {
            let _ = writeln!(s, "\n[run]");
            if !self.run.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&self.run.jvm_args));
            }
            render_jvm_extras(&mut s, &self.run.java_agents, &self.run.env);
            if let Some(cwd) = &self.run.cwd {
                let _ = writeln!(s, "cwd = {}", quote(&cwd.raw));
            }
        }
        if self.test != TestConfig::default() {
            let _ = writeln!(s, "\n[test]");
            if !self.test.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&self.test.jvm_args));
            }
            if let Some(v) = &self.test.jacoco_version {
                let _ = writeln!(s, "jacoco-version = {}", quote(v));
            }
            render_jvm_extras(&mut s, &self.test.java_agents, &self.test.env);
            if self.test.retries > 0 {
                let _ = writeln!(s, "retries = {}", self.test.retries);
            }
            if !self.test.coverage_minimum.is_empty() {
                let entries: Vec<String> = self
                    .test
                    .coverage_minimum
                    .iter()
                    .map(|m| format!("{} = {}", m.counter.key(), m.ratio()))
                    .collect();
                let _ = writeln!(s, "coverage-minimum = {{ {} }}", entries.join(", "));
            }
        }
        if !self.package.add_modules.is_empty() || !self.package.native_image_args.is_empty() {
            let _ = writeln!(s, "\n[package]");
            if !self.package.add_modules.is_empty() {
                let _ = writeln!(s, "add-modules = {}", quote_list(&self.package.add_modules));
            }
            if !self.package.native_image_args.is_empty() {
                let _ = writeln!(
                    s,
                    "native-image-args = {}",
                    quote_list(&self.package.native_image_args)
                );
            }
        }
        if !self.package.manifest.is_empty() {
            // Attribute names are validated to bare-key characters on the way
            // in, so they need no quoting.
            let _ = writeln!(s, "\n[package.manifest]");
            for (name, value) in &self.package.manifest {
                let _ = writeln!(s, "{name} = {}", quote(&value.raw));
            }
        }

        if !self.managed.is_empty() {
            let _ = writeln!(s, "\n[managed]");
            for m in &self.managed {
                let value = if m.bom {
                    format!("{{ version = {}, bom = true }}", quote(&m.version))
                } else {
                    quote(&m.version)
                };
                let _ = writeln!(s, "{} = {value}", quote(&m.key()));
            }
        }
        if !self.dependencies.is_empty() {
            let _ = writeln!(s, "\n[dependencies]");
            for d in &self.dependencies {
                let _ = writeln!(s, "{}", render_dependency(d));
            }
        }
        if !self.dev_dependencies.is_empty() {
            let _ = writeln!(s, "\n[dev-dependencies]");
            for d in &self.dev_dependencies {
                let _ = writeln!(s, "{}", render_dependency(d));
            }
        }
        let extra: Vec<&Repository> = self
            .repositories
            .iter()
            .filter(|r| r.url.trim_end_matches('/') != CENTRAL_URL || !r.groups.is_empty())
            .collect();
        if !extra.is_empty() {
            let _ = writeln!(s, "\n[repositories]");
            for r in extra {
                if r.groups.is_empty() {
                    let _ = writeln!(s, "{} = {}", quote(&r.name), quote(&r.url));
                } else {
                    let _ = writeln!(
                        s,
                        "{} = {{ url = {}, groups = {} }}",
                        quote(&r.name),
                        quote(&r.url),
                        quote_list(&r.groups)
                    );
                }
            }
        }
        for task in &self.tasks {
            render_task(&mut s, task);
        }
        if !self.hooks.is_empty() {
            let _ = writeln!(s, "\n[hooks]");
            for (hook, names) in self.hooks.iter() {
                let _ = writeln!(s, "{hook} = {}", quote_list(names));
            }
        }
        s
    }
}

/// One `[tasks.<name>]` table, its keys in the order TASKS.md §4.1 lists them.
fn render_task(s: &mut String, task: &TaskDef) {
    let _ = writeln!(s, "\n[tasks.{}]", task.name);
    if let Some(d) = &task.description {
        let _ = writeln!(s, "description = {}", quote(d));
    }
    match &task.action {
        Some(Action::Run(argv)) => {
            let _ = writeln!(s, "run = {}", quote_templates(argv));
        }
        Some(Action::Shell(script)) => {
            let _ = writeln!(s, "shell = {}", quote(script));
        }
        Some(Action::Script(file)) => {
            let _ = writeln!(s, "script = {}", quote(&file.raw));
        }
        Some(Action::Main(class)) => {
            let _ = writeln!(s, "main = {}", quote(class));
        }
        None => {}
    }
    if !task.args.is_empty() {
        let _ = writeln!(s, "args = {}", quote_templates(&task.args));
    }
    if !task.depends_on.is_empty() {
        let names: Vec<String> = task.depends_on.iter().map(ToString::to_string).collect();
        let _ = writeln!(s, "depends-on = {}", quote_list(&names));
    }
    if !task.env.is_empty() {
        let _ = writeln!(s, "env = {}", inline_env(&task.env));
    }
    if let Some(cwd) = &task.cwd {
        let _ = writeln!(s, "cwd = {}", quote(&cwd.raw));
    }
    for (key, list) in [
        ("inputs", &task.inputs),
        ("outputs", &task.outputs),
        ("source-outputs", &task.source_outputs),
        ("resource-outputs", &task.resource_outputs),
    ] {
        if !list.is_empty() {
            let _ = writeln!(s, "{key} = {}", quote_templates(list));
        }
    }
    if !task.dependencies.is_empty() {
        let _ = writeln!(s, "\n[tasks.{}.dependencies]", task.name);
        for d in &task.dependencies {
            let _ = writeln!(s, "{}", render_dependency(d));
        }
    }
}

/// An `env` table as one inline TOML table, in declaration order.
fn inline_env(env: &[(String, Template)]) -> String {
    let vars: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("{} = {}", quote(k), quote(&v.raw)))
        .collect();
    format!("{{ {} }}", vars.join(", "))
}

/// The `java-agents` and `env` lines `[run]` and `[test]` share.
fn render_jvm_extras(s: &mut String, agents: &[Ga], env: &[(String, Template)]) {
    if !agents.is_empty() {
        let names: Vec<String> = agents.iter().map(ToString::to_string).collect();
        let _ = writeln!(s, "java-agents = {}", quote_list(&names));
    }
    if !env.is_empty() {
        let _ = writeln!(s, "env = {}", inline_env(env));
    }
}

fn quote_templates(items: &[Template]) -> String {
    let raw: Vec<String> = items.iter().map(|t| t.raw.clone()).collect();
    quote_list(&raw)
}

/// A manifest with nothing but the required fields, used by `init` and `migrate`.
#[must_use]
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
        run: RunConfig::default(),
        test: TestConfig::default(),
        package: PackageConfig::default(),
        languages: Vec::new(),
        dependencies: Vec::new(),
        dev_dependencies: Vec::new(),
        managed: Vec::new(),
        repositories: vec![Repository::new(CENTRAL_NAME, CENTRAL_URL)],
        tasks: Vec::new(),
        hooks: Hooks::default(),
        warnings: Vec::new(),
        jrs_version: None,
    }
}

// ---- languages -------------------------------------------------------------

/// `[kotlin]`, `[scala]` and `[groovy]`, each on its own: a version the
/// compiler can be pinned at, and the directories and flags.
fn parse_languages(table: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<LanguageConfig>> {
    let mut out = Vec::new();
    for language in Language::FOREIGN {
        let key = language.key();
        let args_key = language.args_key();
        let known = [
            "version",
            "source-dir",
            "test-dir",
            args_key.as_str(),
            "compiler-jvm-args",
        ];
        let Some(t) = section(table, key, &known, warnings)? else {
            continue;
        };
        let version = match t.get("version") {
            Some(toml::Value::String(v)) if !v.trim().is_empty() => v.trim().to_string(),
            Some(toml::Value::String(_)) => {
                return Err(JrsError::manifest(format!(
                    "`{key}.version` must not be empty"
                )));
            }
            Some(_) => {
                return Err(JrsError::manifest(format!(
                    "`{key}.version` must be a version string"
                )));
            }
            None => {
                return Err(JrsError::manifest(format!(
                    "[{key}] needs `{key}.version`: the {language} compiler to build with, which \
                     is also the version of its runtime library\n\n    [{key}]\n    \
                     version = \"{}\"",
                    language.starter_version().unwrap_or_default()
                )));
            }
        };
        if is_range(&version) {
            return Err(JrsError::manifest(format!(
                "`{key}.version` is the range `{version}`; the compiler is pinned at an exact \
                 version, like every dependency"
            )));
        }
        language
            .check_version(&version)
            .map_err(|e| JrsError::manifest(format!("`{key}.version` is {version}: {e}")))?;
        out.push(LanguageConfig {
            language,
            source_dir: path_or(t, "source-dir", &format!("src/main/{key}"), key)?,
            test_dir: path_or(t, "test-dir", &format!("src/test/{key}"), key)?,
            compiler_args: string_array(t, &args_key, key)?,
            compiler_jvm_args: string_array(t, "compiler-jvm-args", key)?,
            version,
        });
    }
    Ok(out)
}

/// What the language tables mean for the rest of the manifest: the source
/// encoding kotlinc can read, the `javac-args` Groovy's joint `javac` can be
/// handed, and a declared runtime library at another version than its
/// compiler.
fn check_languages(manifest: &Manifest) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for config in &manifest.languages {
        let key = config.language.key();
        match config.language {
            Language::Kotlin => {
                let encoding = manifest
                    .java
                    .encoding
                    .to_ascii_uppercase()
                    .replace('_', "-");
                if encoding != "UTF-8" && encoding != "UTF8" {
                    warnings.push(format!(
                        "`java.encoding` is {}, but kotlinc reads sources as UTF-8 only; the \
                         Kotlin sources are read as UTF-8",
                        manifest.java.encoding
                    ));
                }
            }
            Language::Groovy => {
                lang::groovy_javac_args(&manifest.java.javac_args).map_err(|e| {
                    JrsError::manifest(format!(
                        "`java.javac-args`: {e}\n\n\
                         with [groovy] on, groovyc runs javac itself, and hands it these"
                    ))
                })?;
            }
            Language::Java | Language::Scala => {}
        }
        for (group, artifact) in config.language.runtime_libraries(&config.version) {
            if let Some(d) = manifest
                .dependencies
                .iter()
                .chain(&manifest.dev_dependencies)
                .find(|d| d.group == group && d.artifact == artifact && d.classifier.is_none())
                && d.version != config.version
            {
                warnings.push(format!(
                    "`{group}:{artifact}` is declared at {}, but `{key}.version` is {}; \
                     kotlinc and scalac reject a runtime library newer than themselves, so \
                     keep the two in step",
                    d.version, config.version
                ));
            }
        }
    }
    Ok(warnings)
}

// ---- tasks and hooks -------------------------------------------------------

/// `[tasks.*]`, structurally: types, names, actions, environment names. What
/// needs every task at once is `task::check`'s.
fn parse_tasks(table: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<TaskDef>> {
    let Some(value) = table.get("tasks") else {
        return Ok(Vec::new());
    };
    let tasks = value
        .as_table()
        .ok_or_else(|| JrsError::manifest("`tasks` must be a table of `[tasks.<name>]` tables"))?;
    let mut out = Vec::with_capacity(tasks.len());
    for (name, value) in tasks {
        let section = format!("tasks.{name}");
        validate_task_name(name)?;
        let t = value
            .as_table()
            .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;
        warn_unknown(t, TASK_KEYS, &format!("{section}."), warnings);

        let template = |key: &str, raw: &str| {
            Template::parse(raw).map_err(|e| JrsError::manifest(format!("`{section}.{key}`: {e}")))
        };
        let templates = |key: &str| -> Result<Vec<Template>> {
            string_array(t, key, &section)?
                .iter()
                .map(|raw| template(key, raw))
                .collect()
        };

        let mut actions = Vec::new();
        if t.contains_key("run") {
            let argv = templates("run")?;
            if argv.first().is_none_or(|p| p.raw.trim().is_empty()) {
                return Err(JrsError::manifest(format!(
                    "`{section}.run` must name a program: `run = [\"program\", \"arg\", ...]`"
                )));
            }
            actions.push(("run", Action::Run(argv)));
        }
        if let Some(script) = optional_string(t, "shell", &section)? {
            actions.push(("shell", Action::Shell(script)));
        }
        if let Some(file) = optional_string(t, "script", &section)? {
            actions.push(("script", Action::Script(template("script", &file)?)));
        }
        if let Some(class) = optional_string(t, "main", &section)? {
            validate_class_name(&class, &format!("{section}.main"))?;
            actions.push(("main", Action::Main(class)));
        }
        if actions.len() > 1 {
            let keys: Vec<String> = actions.iter().map(|(k, _)| format!("`{k}`")).collect();
            return Err(JrsError::manifest(format!(
                "`{section}` has {}; a task runs exactly one of `run`, `shell`, `script` \
                 or `main`",
                keys.join(" and ")
            )));
        }
        let action = actions.pop().map(|(_, a)| a);
        let depends_on = parse_depends_on(t, &section)?;
        if action.is_none() && depends_on.is_empty() {
            return Err(JrsError::manifest(format!(
                "`{section}` does nothing: give it one of `run`, `shell`, `script` or \
                 `main`, or a `depends-on` list to run"
            )));
        }
        let dependencies = parse_task_dependencies(t, &section, action.as_ref())?;
        let env = parse_task_env(t, &section)?;
        let cwd = optional_string(t, "cwd", &section)?
            .map(|raw| template("cwd", &raw))
            .transpose()?;

        let task = TaskDef {
            name: name.clone(),
            description: optional_string(t, "description", &section)?,
            action,
            args: templates("args")?,
            depends_on,
            env,
            cwd,
            inputs: templates("inputs")?,
            outputs: templates("outputs")?,
            source_outputs: templates("source-outputs")?,
            resource_outputs: templates("resource-outputs")?,
            dependencies,
        };
        for (key, t) in task
            .path_templates()
            .chain(task.cwd.iter().map(|t| ("cwd", t)))
        {
            if let Some(p) = t.placeholders().find(|p| p.is_classpath()) {
                return Err(JrsError::manifest(format!(
                    "`{section}.{key}`: `{{{}}}` is a classpath, not a path; it cannot \
                     name a file or directory",
                    p.name()
                )));
            }
        }
        out.push(task);
    }
    Ok(out)
}

/// `[tasks.<name>.dependencies]`: the `[dependencies]` value forms, as the
/// classpath of a `main` or `script` action. They are a tool's graph, never
/// the project's, so what only means something there is refused: a local jar,
/// `compile-only` and `runtime-only`, and a version left to `[managed]`.
fn parse_task_dependencies(
    t: &toml::Table,
    section: &str,
    action: Option<&Action>,
) -> Result<Vec<Dependency>> {
    let key = format!("{section}.dependencies");
    let dependencies = parse_dependency_table(t.get("dependencies"), &key)?;
    for d in &dependencies {
        let refused = if d.is_local() {
            Some("a local jar")
        } else if d.compile_only || d.runtime_only {
            Some("`compile-only` and `runtime-only`, which choose a classpath of the project's")
        } else if d.is_managed() {
            Some("a version from [managed], which versions the project's graph")
        } else {
            None
        };
        if let Some(what) = refused {
            return Err(JrsError::manifest(format!(
                "`{key}.\"{}\"`: a task's dependencies are a tool's graph of their own, \
                 so {what} does not go here",
                d.key()
            )));
        }
    }
    match action {
        Some(Action::Main(_)) if dependencies.is_empty() => Err(JrsError::manifest(format!(
            "`{section}.main` runs a class from the task's own dependencies, and it has none\n\n\
             add them:\n\n    [{section}.dependencies]\n    \"group:artifact\" = \"<version>\""
        ))),
        Some(Action::Run(_) | Action::Shell(_)) | None if !dependencies.is_empty() => {
            Err(JrsError::manifest(format!(
                "`{key}` is the classpath of a `main` or `script` action, and `{section}` has \
                 neither"
            )))
        }
        _ => Ok(dependencies),
    }
}

/// `depends-on`: task names, and the built-ins a task may depend on.
fn parse_depends_on(t: &toml::Table, section: &str) -> Result<Vec<TaskRef>> {
    let mut depends_on = Vec::new();
    for entry in string_array(t, "depends-on", section)? {
        let reference = match Builtin::parse(&entry) {
            Some(b) => TaskRef::Builtin(b),
            None if RESERVED_TASK_NAMES.contains(&entry.as_str()) => {
                return Err(JrsError::manifest(format!(
                    "`{section}.depends-on`: `{entry}` is a command a task cannot depend on; \
                     the built-ins a task can depend on are `build`, `test`, `package` and `doc`"
                )));
            }
            None => TaskRef::Task(entry),
        };
        if depends_on.contains(&reference) {
            return Err(JrsError::manifest(format!(
                "`{section}.depends-on` names `{reference}` twice"
            )));
        }
        depends_on.push(reference);
    }
    Ok(depends_on)
}

/// A task's `env` table, in declaration order. `JRS_*` names are jrs's.
fn parse_task_env(t: &toml::Table, section: &str) -> Result<Vec<(String, Template)>> {
    parse_env(t, section, ", and are set for every task")
}

/// `run.env` or `test.env`: the tasks' `env` table, without the placeholders
/// only a task has a value for.
fn parse_jvm_env(t: &toml::Table, section: &str) -> Result<Vec<(String, Template)>> {
    let env = parse_env(t, section, "")?;
    for (key, template) in &env {
        refuse_task_placeholders(template, &format!("{section}.env.{key}"))?;
    }
    Ok(env)
}

/// `run.cwd`: a directory, as a task's `cwd` is, relative to the root.
fn parse_jvm_cwd(t: &toml::Table, section: &str) -> Result<Option<Template>> {
    let Some(raw) = optional_string(t, "cwd", section)? else {
        return Ok(None);
    };
    let template =
        Template::parse(&raw).map_err(|e| JrsError::manifest(format!("`{section}.cwd`: {e}")))?;
    refuse_task_placeholders(&template, &format!("{section}.cwd"))?;
    if let Some(p) = template.placeholders().find(|p| p.is_classpath()) {
        return Err(JrsError::manifest(format!(
            "`{section}.cwd`: `{{{}}}` is a classpath, not a path; it cannot name a directory",
            p.name()
        )));
    }
    Ok(Some(template))
}

/// `{jar}` and `{classpath-argfile}` have a value in a task only: `jrs run`
/// and `jrs test` package nothing, and the argfile is a task's own.
fn refuse_task_placeholders(template: &Template, key: &str) -> Result<()> {
    for p in template.placeholders() {
        let why = match p {
            Placeholder::Jar => "`jrs run` and `jrs test` do not package, so there is no jar",
            Placeholder::ClasspathArgfile => {
                "the argfile is a task's own; use `{classpath}`, `{runtime-classpath}` or \
                 `{test-classpath}`"
            }
            _ => continue,
        };
        return Err(JrsError::manifest(format!(
            "`{key}`: `{{{}}}` has no value here: {why}",
            p.name()
        )));
    }
    Ok(())
}

/// `run.java-agents` or `test.java-agents`: `group:artifact` coordinates,
/// without a version, since the version is the one the graph resolved.
fn parse_java_agents(t: &toml::Table, section: &str) -> Result<Vec<Ga>> {
    let key = format!("{section}.java-agents");
    let mut agents: Vec<Ga> = Vec::new();
    for raw in string_array(t, "java-agents", section)? {
        let well_formed = |s: &str| !s.is_empty() && !s.contains(char::is_whitespace);
        let ga = match raw.split(':').collect::<Vec<_>>().as_slice() {
            [g, a] if well_formed(g) && well_formed(a) => Ga::new(*g, *a),
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{key}`: `{raw}` is not a `group:artifact` coordinate\n\n\
                     name the agent's dependency without a version, as in \
                     `java-agents = [\"org.mockito:mockito-core\"]`; the version is the \
                     one jrs.lock pins"
                )));
            }
        };
        if agents.contains(&ga) {
            return Err(JrsError::manifest(format!("`{key}` names `{raw}` twice")));
        }
        agents.push(ga);
    }
    Ok(agents)
}

/// An `env` table, in declaration order: names to templates. `JRS_*` names
/// are jrs's; `reserved` finishes the sentence that says so.
fn parse_env(t: &toml::Table, section: &str, reserved: &str) -> Result<Vec<(String, Template)>> {
    let Some(value) = t.get("env") else {
        return Ok(Vec::new());
    };
    let vars = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{section}.env` must be a table of strings")))?;
    let mut env = Vec::with_capacity(vars.len());
    for (key, value) in vars {
        let raw = value
            .as_str()
            .ok_or_else(|| JrsError::manifest(format!("`{section}.env.{key}` must be a string")))?;
        if key.is_empty() || key.contains(['=', '\0']) {
            return Err(JrsError::manifest(format!(
                "`{section}.env`: `{key}` is not an environment variable name"
            )));
        }
        if key.to_ascii_uppercase().starts_with("JRS_") {
            return Err(JrsError::manifest(format!(
                "`{section}.env.{key}`: names starting with `JRS_` are jrs's own{reserved}"
            )));
        }
        let template = Template::parse(raw)
            .map_err(|e| JrsError::manifest(format!("`{section}.env.{key}`: {e}")))?;
        env.push((key.clone(), template));
    }
    Ok(env)
}

fn validate_task_name(name: &str) -> Result<()> {
    let well_formed = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !well_formed {
        return Err(JrsError::manifest(format!(
            "`tasks.{name}`: a task name is lowercase letters, digits and `-`, \
             starting with a letter"
        )));
    }
    if RESERVED_TASK_NAMES.contains(&name) {
        return Err(JrsError::manifest(format!(
            "`tasks.{name}`: `{name}` is a jrs command, so it cannot be a task name"
        )));
    }
    Ok(())
}

fn parse_hooks(table: &toml::Table, warnings: &mut Vec<String>) -> Result<Hooks> {
    let Some(t) = section(table, "hooks", &[], &mut Vec::new())? else {
        return Ok(Hooks::default());
    };
    let mut hooks = Vec::new();
    for key in t.keys() {
        if !Hook::ALL.iter().any(|h| h.name() == key) {
            warnings.push(format!("unknown key `hooks.{key}` in jrs.toml (ignored)"));
        }
    }
    for hook in Hook::ALL {
        let names = string_array(t, hook.name(), "hooks")?;
        for (i, name) in names.iter().enumerate() {
            if names[..i].contains(name) {
                return Err(JrsError::manifest(format!(
                    "`hooks.{hook}` names `{name}` twice"
                )));
            }
        }
        if !names.is_empty() {
            hooks.push((hook, names));
        }
    }
    Ok(Hooks(hooks))
}

// ---- parsing helpers -------------------------------------------------------

/// `src/test/resources` for `src/test/java`: the directory beside the tests.
fn default_test_resource_dir(test_dir: &Path) -> PathBuf {
    match test_dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join("resources"),
        _ => PathBuf::from("src/test/resources"),
    }
}

fn parse_dependencies(table: &toml::Table, section: &str) -> Result<Vec<Dependency>> {
    parse_dependency_table(table.get(section), section)
}

/// A table of dependencies; `section` is how messages name it.
fn parse_dependency_table(value: Option<&toml::Value>, section: &str) -> Result<Vec<Dependency>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let deps = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;

    let mut out: Vec<Dependency> = Vec::with_capacity(deps.len());
    for (key, value) in deps {
        if let toml::Value::Table(t) = value
            && t.contains_key("path")
        {
            let dep = parse_local_jar(key, t, section)?;
            if out.iter().any(|d| d.key() == dep.key()) {
                return Err(JrsError::manifest(format!(
                    "`{section}` declares `{}` twice",
                    dep.key()
                )));
            }
            out.push(dep);
            continue;
        }
        let (group, artifact, key_classifier) = split_coordinate(key, section)?;
        let mut dep = Dependency::new(group, artifact, "");
        dep.classifier = key_classifier;
        let name = |k: &str| format!("`{section}.\"{key}\".{k}`");
        match value {
            toml::Value::String(v) => dep.version.clone_from(v),
            toml::Value::Table(t) => {
                for k in t.keys() {
                    if !DEPENDENCY_KEYS.contains(&k.as_str()) {
                        return Err(JrsError::manifest(format!(
                            "`{section}.\"{key}\"`: unknown key `{k}` (expected `version`, \
                             `classifier`, `exclusions`, `compile-only`, `runtime-only` \
                             or `path`)"
                        )));
                    }
                }
                // No `version` at all leaves it to `[managed]`, which is
                // checked once the whole manifest is read.
                dep.version = match t.get("version") {
                    None => String::new(),
                    Some(toml::Value::String(v)) if !v.trim().is_empty() => v.clone(),
                    Some(toml::Value::String(_)) => {
                        return Err(JrsError::manifest(format!(
                            "`{section}.\"{key}\"` has an empty version; leave `version` out \
                             to take it from [managed]"
                        )));
                    }
                    Some(_) => {
                        return Err(JrsError::manifest(format!(
                            "{} must be a version string",
                            name("version")
                        )));
                    }
                };
                if let Some(c) = optional_string(t, "classifier", &format!("{section}.\"{key}\""))?
                {
                    if dep.classifier.as_ref().is_some_and(|k| *k != c) {
                        return Err(JrsError::manifest(format!(
                            "{} says `{c}`, but the key names another classifier",
                            name("classifier")
                        )));
                    }
                    dep.classifier = Some(c).filter(|c| !c.is_empty());
                }
                for pattern in string_array(t, "exclusions", &format!("{section}.\"{key}\""))? {
                    let (g, a) = pattern
                        .split_once(':')
                        .filter(|(g, a)| !g.is_empty() && !a.is_empty() && !a.contains(':'))
                        .ok_or_else(|| {
                            JrsError::manifest(format!(
                                "{}: `{pattern}` is not a `group:artifact` pattern \
                                 (`*` may stand for either half)",
                                name("exclusions")
                            ))
                        })?;
                    dep.exclusions.push(Exclusion {
                        group: g.to_string(),
                        artifact: a.to_string(),
                    });
                }
                dep.compile_only = bool_key(t, "compile-only", &name("compile-only"))?;
                dep.runtime_only = bool_key(t, "runtime-only", &name("runtime-only"))?;
                check_one_classpath(&dep, section)?;
            }
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{section}.\"{key}\"` must be a version string or a table \
                     with a `version` key"
                )));
            }
        }
        if matches!(value, toml::Value::String(_)) && dep.version.trim().is_empty() {
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"` has an empty version; write `{{}}` to take it from \
                 [managed]"
            )));
        }
        if out.iter().any(|d| d.key() == dep.key()) {
            return Err(JrsError::manifest(format!(
                "`{section}` declares `{}` twice",
                dep.key()
            )));
        }
        out.push(dep);
    }
    Ok(out)
}

/// `[managed]`: `"group:artifact" = "version"` pins a version wherever the
/// artifact turns up in the graph; `{ version = "...", bom = true }` imports a
/// BOM's managed versions (SPEC §8.9). Order is kept: the first BOM to manage
/// an artifact wins, as the first import does in Maven.
fn parse_managed(table: &toml::Table) -> Result<Vec<Managed>> {
    let Some(value) = table.get("managed") else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_table()
        .ok_or_else(|| JrsError::manifest("`managed` must be a table"))?;
    let mut out: Vec<Managed> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let (group, artifact, classifier) = split_coordinate(key, "managed")?;
        if classifier.is_some() {
            return Err(JrsError::manifest(format!(
                "`managed.\"{key}\"`: a managed version applies to every classifier of \
                 `group:artifact`, so the key names no classifier"
            )));
        }
        let (version, bom) = match value {
            toml::Value::String(v) => (v.clone(), false),
            toml::Value::Table(t) => {
                for k in t.keys() {
                    if !MANAGED_KEYS.contains(&k.as_str()) {
                        return Err(JrsError::manifest(format!(
                            "`managed.\"{key}\"`: unknown key `{k}` (expected `version` or `bom`)"
                        )));
                    }
                }
                let version = optional_string(t, "version", &format!("managed.\"{key}\""))?
                    .ok_or_else(|| {
                        JrsError::manifest(format!(
                            "`managed.\"{key}\"` is missing a `version` string"
                        ))
                    })?;
                (
                    version,
                    bool_key(t, "bom", &format!("`managed.\"{key}\".bom`"))?,
                )
            }
            _ => {
                return Err(JrsError::manifest(format!(
                    "`managed.\"{key}\"` must be a version string, or a table with `version` \
                     and `bom = true`"
                )));
            }
        };
        let version = version.trim().to_string();
        if version.is_empty() {
            return Err(JrsError::manifest(format!(
                "`managed.\"{key}\"` has an empty version"
            )));
        }
        if is_range(&version) {
            return Err(JrsError::manifest(format!(
                "`managed.\"{key}\"` is the range `{version}`; a managed version is exact, \
                 like every other"
            )));
        }
        if out
            .iter()
            .any(|m| m.group == group && m.artifact == artifact)
        {
            return Err(JrsError::manifest(format!("`managed` names `{key}` twice")));
        }
        out.push(Managed {
            group,
            artifact,
            version,
            bom,
        });
    }
    Ok(out)
}

/// A dependency that leaves its version out takes it from `[managed]`. With a
/// BOM there, whether it covers the artifact is only known once the BOM is
/// read, at resolution; without one, the table has to name it.
fn check_versionless(
    dependencies: &[Dependency],
    dev_dependencies: &[Dependency],
    managed: &[Managed],
) -> Result<()> {
    if managed.iter().any(|m| m.bom) {
        return Ok(());
    }
    let tables = dependencies
        .iter()
        .map(|d| ("dependencies", d))
        .chain(dev_dependencies.iter().map(|d| ("dev-dependencies", d)));
    for (section, d) in tables {
        let covered = managed
            .iter()
            .any(|m| m.group == d.group && m.artifact == d.artifact);
        if d.is_managed() && !covered {
            let what = if managed.is_empty() {
                "there is no [managed] table"
            } else {
                "[managed] does not name it"
            };
            return Err(JrsError::manifest(format!(
                "`{section}.\"{}\"` has no version, and {what}\n\n\
                 give it a version, or manage it:\n\n    [managed]\n    \"{}:{}\" = \"<version>\"",
                d.key(),
                d.group,
                d.artifact
            )));
        }
    }
    Ok(())
}

/// A `true`/`false` key of a long form; `name` is how a message names it.
fn bool_key(t: &toml::Table, key: &str, name: &str) -> Result<bool> {
    match t.get(key) {
        None => Ok(false),
        Some(toml::Value::Boolean(b)) => Ok(*b),
        Some(_) => Err(JrsError::manifest(format!(
            "{name} must be `true` or `false`"
        ))),
    }
}

/// `compile-only` and `runtime-only` together would put the jar on both
/// classpaths, which is what a plain dependency already is.
fn check_one_classpath(dep: &Dependency, section: &str) -> Result<()> {
    if dep.compile_only && dep.runtime_only {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{}\"` is both `compile-only` and `runtime-only`, which is \
             what a plain dependency is; drop both",
            dep.key()
        )));
    }
    Ok(())
}

/// `name = { path = "libs/driver.jar" }`: a jar taken as it is, from inside
/// the project. It has no coordinate, so the key is a name of its own, and no
/// graph, so there is nothing to version, classify or exclude.
fn parse_local_jar(key: &str, t: &toml::Table, section: &str) -> Result<Dependency> {
    let name = |k: &str| format!("`{section}.\"{key}\".{k}`");
    let well_formed = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !well_formed {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{key}\"` has a `path`, so it is a local jar, and its key is a \
             name of its own: letters, digits, `.`, `-` and `_`, such as `oracle-driver`"
        )));
    }
    for k in t.keys() {
        if !LOCAL_DEPENDENCY_KEYS.contains(&k.as_str()) {
            let why = match k.as_str() {
                "version" | "classifier" | "exclusions" => {
                    " — a local jar is taken as it is: it has no coordinate and no \
                     transitive graph"
                }
                _ => "",
            };
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"`: `{k}` does not go with `path`{why} (expected \
                 `path`, `compile-only` or `runtime-only`)"
            )));
        }
    }
    let raw = optional_string(t, "path", &format!("{section}.\"{key}\""))?.unwrap_or_default();
    let path = raw.replace('\\', "/");
    let as_path = Path::new(&path);
    let escapes = as_path.is_absolute()
        || as_path.has_root()
        || path.starts_with('/')
        || path.split('/').any(|c| c == "..");
    if path.trim().is_empty() || escapes || path.ends_with('/') {
        return Err(JrsError::manifest(format!(
            "{} must name a jar file inside the project, relative to its root \
             (got `{raw}`)",
            name("path")
        )));
    }
    let mut dep = Dependency::local(key, path);
    dep.compile_only = bool_key(t, "compile-only", &name("compile-only"))?;
    dep.runtime_only = bool_key(t, "runtime-only", &name("runtime-only"))?;
    check_one_classpath(&dep, section)?;
    Ok(dep)
}

/// `group:artifact`, or `group:artifact:classifier`.
///
/// A third segment that starts with a digit is a version, not a classifier —
/// classifiers are words like `natives-linux` or `tests` — and gets the error
/// that says where the version goes.
fn split_coordinate(key: &str, section: &str) -> Result<(String, String, Option<String>)> {
    let parts: Vec<&str> = key.split(':').collect();
    let bad = || {
        JrsError::manifest(format!(
            "`{section}.\"{key}\"`: dependency keys must be `group:artifact` \
             (the version belongs on the right-hand side)"
        ))
    };
    match parts.as_slice() {
        [g, a] if !g.is_empty() && !a.is_empty() => Ok((g.to_string(), a.to_string(), None)),
        [g, a, c]
            if !g.is_empty()
                && !a.is_empty()
                && !c.is_empty()
                && !c.starts_with(|ch: char| ch.is_ascii_digit()) =>
        {
            Ok((g.to_string(), a.to_string(), Some(c.to_string())))
        }
        _ => Err(bad()),
    }
}

/// The `[java]` table, its unknown keys already warned about.
fn java_config(t: &toml::Table) -> Result<JavaConfig> {
    let source = optional_release(t, "source")?;
    // `target` only means anything when it differs from `source` (SPEC §4.2);
    // normalising here keeps the rest of the codebase from having to compare
    // the two.
    let target = optional_release(t, "target")?.filter(|t| Some(*t) != source);
    let encoding = optional_string(t, "encoding", "java")?.unwrap_or_else(|| "UTF-8".into());
    let javac_args = string_array(t, "javac-args", "java")?;
    let javadoc_args = string_array(t, "javadoc-args", "java")?;
    let jdk = optional_release(t, "jdk")?;
    Ok(JavaConfig {
        source,
        target,
        encoding,
        javac_args,
        javadoc_args,
        jdk,
    })
}

/// An optional sub-table, with its unknown keys turned into warnings.
fn section<'a>(
    table: &'a toml::Table,
    name: &str,
    known: &[&str],
    warnings: &mut Vec<String>,
) -> Result<Option<&'a toml::Table>> {
    let Some(value) = table.get(name) else {
        return Ok(None);
    };
    let t = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{name}` must be a table")))?;
    warn_unknown(t, known, &format!("{name}."), warnings);
    Ok(Some(t))
}

fn string_array(t: &toml::Table, key: &str, section: &str) -> Result<Vec<String>> {
    let bad = || JrsError::manifest(format!("`{section}.{key}` must be an array of strings"));
    match t.get(key) {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|a| a.as_str().map(str::to_string).ok_or_else(bad))
            .collect(),
    }
}

/// `[package.manifest]`: attribute names to string values, in declaration
/// order. Each name is checked against the jar specification and against the
/// attributes jrs writes itself, and each value may use `{project.name}` and
/// `{project.version}` — nothing else is known when a jar is written.
fn parse_jar_attributes(t: &toml::Table) -> Result<Vec<(String, Template)>> {
    let Some(value) = t.get("manifest") else {
        return Ok(Vec::new());
    };
    let table = value.as_table().ok_or_else(|| {
        JrsError::manifest(
            "`package.manifest` must be a table of attribute names to strings, e.g.\n\n    \
             [package.manifest]\n    Implementation-Title = \"My App\"",
        )
    })?;
    let mut out: Vec<(String, Template)> = Vec::new();
    for (name, value) in table {
        let key = format!("package.manifest.{name}");
        if let Some((earlier, _)) = out.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            return Err(JrsError::manifest(format!(
                "`{key}`: `{earlier}` is already set; attribute names are case-insensitive"
            )));
        }
        let raw = value
            .as_str()
            .ok_or_else(|| JrsError::manifest(format!("`{key}` must be a string")))?;
        out.push((name.clone(), jar_attribute(name, raw)?));
    }
    Ok(out)
}

/// One `[package.manifest]` attribute, checked as the parser checks it — a
/// name the jar specification allows, not one jrs writes itself, and a
/// one-line value whose only placeholders are `{project.name}` and
/// `{project.version}` — and its value parsed. `jrs migrate` uses it to decide
/// what a build's jar attributes can become.
///
/// # Errors
///
/// [`JrsError::Manifest`], naming `package.manifest.<name>`, for any of those.
pub fn jar_attribute(name: &str, raw: &str) -> Result<Template> {
    let key = format!("package.manifest.{name}");
    validate_attribute_name(name).map_err(|why| JrsError::manifest(format!("`{key}`: {why}")))?;
    if let Some(owned) = JAR_ATTRIBUTES_OWNED
        .iter()
        .find(|a| a.eq_ignore_ascii_case(name))
    {
        let hint = match *owned {
            "Main-Class" => "set `project.main-class` instead",
            "Class-Path" => {
                "it is written from the runtime classpath (see `jrs package --portable`)"
            }
            "Name" => "it starts a per-entry section, and jrs writes the main section only",
            _ => "jrs writes it itself",
        };
        return Err(JrsError::manifest(format!(
            "`{key}`: `{owned}` belongs to jrs; {hint}"
        )));
    }
    if raw.contains(['\n', '\r', '\0']) {
        return Err(JrsError::manifest(format!(
            "`{key}`: an attribute value must be one line, with no NUL"
        )));
    }
    let template = Template::parse(raw).map_err(|e| JrsError::manifest(format!("`{key}`: {e}")))?;
    if let Some(p) = template
        .placeholders()
        .find(|p| !matches!(p, Placeholder::ProjectName | Placeholder::ProjectVersion))
    {
        return Err(JrsError::manifest(format!(
            "`{key}`: `{{{}}}` is not known when a jar is written; an attribute can use \
             `{{project.name}}` and `{{project.version}}`",
            p.name()
        )));
    }
    Ok(template)
}

/// A main-section attribute name, per the jar specification: an ASCII letter
/// or digit, then letters, digits, `-` and `_`, 70 bytes at most.
fn validate_attribute_name(name: &str) -> std::result::Result<(), String> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(
            "an attribute name is an ASCII letter or digit followed by letters, digits, \
             `-` and `_`"
                .to_string(),
        );
    }
    if name.len() > 70 {
        return Err(format!(
            "an attribute name is at most 70 bytes (this one is {})",
            name.len()
        ));
    }
    Ok(())
}

/// One `[dependencies]` line: the short form when it says everything, the
/// inline table otherwise.
fn render_dependency(d: &Dependency) -> String {
    let (key, value) = dependency_entry(d);
    format!("{} = {value}", quote(&key))
}

/// A dependency as a table entry: its key, unquoted, and its value as TOML.
/// `jrs add` writes exactly this, so an added line reads like a generated one.
#[must_use]
pub fn dependency_entry(d: &Dependency) -> (String, String) {
    if let Some(path) = &d.path {
        let mut fields = vec![format!("path = {}", quote(path))];
        if d.compile_only {
            fields.push("compile-only = true".to_string());
        }
        if d.runtime_only {
            fields.push("runtime-only = true".to_string());
        }
        return (d.artifact.clone(), format!("{{ {} }}", fields.join(", ")));
    }
    // The classifier goes in the key when it can, so two classifiers of one
    // artifact stay two distinct keys.
    let key_classifier = d
        .classifier
        .as_ref()
        .filter(|c| !c.starts_with(|ch: char| ch.is_ascii_digit()));
    let key = match key_classifier {
        Some(c) => format!("{}:{}:{c}", d.group, d.artifact),
        None => format!("{}:{}", d.group, d.artifact),
    };
    let table_classifier = d.classifier.as_ref().filter(|_| key_classifier.is_none());
    if d.is_plain() && table_classifier.is_none() && !d.is_managed() {
        return (key, quote(&d.version));
    }
    // A managed dependency has no `version` field; `{}` alone says so.
    let mut fields = Vec::new();
    if !d.is_managed() {
        fields.push(format!("version = {}", quote(&d.version)));
    }
    if let Some(c) = table_classifier {
        fields.push(format!("classifier = {}", quote(c)));
    }
    if !d.exclusions.is_empty() {
        let patterns: Vec<String> = d.exclusions.iter().map(ToString::to_string).collect();
        fields.push(format!("exclusions = {}", quote_list(&patterns)));
    }
    if d.compile_only {
        fields.push("compile-only = true".to_string());
    }
    if d.runtime_only {
        fields.push("runtime-only = true".to_string());
    }
    if fields.is_empty() {
        return (key, "{}".to_string());
    }
    (key, format!("{{ {} }}", fields.join(", ")))
}

fn quote_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|a| quote(a)).collect();
    format!("[{}]", quoted.join(", "))
}

fn parse_repositories(table: &toml::Table) -> Result<Vec<Repository>> {
    let mut repos = Vec::new();
    if let Some(value) = table.get("repositories") {
        let t = value
            .as_table()
            .ok_or_else(|| JrsError::manifest("`repositories` must be a table"))?;
        for (name, value) in t {
            let (url, groups) = match value {
                toml::Value::Table(long) => repository_long_form(name, long)?,
                other => (
                    other.as_str().ok_or_else(|| {
                        JrsError::manifest(format!(
                            "`repositories.{name}` must be a URL string, or a table with \
                             `url` and `groups`"
                        ))
                    })?,
                    Vec::new(),
                ),
            };
            repos.push(Repository {
                name: name.clone(),
                url: url.trim_end_matches('/').to_string(),
                groups,
            });
        }
    }
    // Maven Central is implicit and always last.
    if !repos.iter().any(|r| r.url == CENTRAL_URL) {
        repos.push(Repository::new(CENTRAL_NAME, CENTRAL_URL));
    }
    Ok(repos)
}

/// `internal = { url = "...", groups = ["com.acme", "com.acme.*"] }`: a
/// repository confined to the groups it serves. The keys are checked, not
/// warned about: a misspelt `groups` would silently open the repository to
/// every group, which is the hole the key exists to close.
fn repository_long_form<'a>(name: &str, t: &'a toml::Table) -> Result<(&'a str, Vec<String>)> {
    let section = format!("repositories.{name}");
    for k in t.keys() {
        if !REPOSITORY_KEYS.contains(&k.as_str()) {
            return Err(JrsError::manifest(format!(
                "`{section}`: unknown key `{k}` (expected `url` or `groups`)"
            )));
        }
    }
    let url = match t.get("url") {
        Some(toml::Value::String(u)) if !u.trim().is_empty() => u.as_str(),
        _ => {
            return Err(JrsError::manifest(format!(
                "`{section}` needs a `url` string"
            )));
        }
    };
    let groups = string_array(t, "groups", &section)?;
    if t.contains_key("groups") && groups.is_empty() {
        return Err(JrsError::manifest(format!(
            "`{section}.groups` is empty, so the repository would serve nothing; leave \
             `groups` out to ask it for every group"
        )));
    }
    for pattern in &groups {
        if !valid_group_pattern(pattern) {
            return Err(JrsError::manifest(format!(
                "`{section}.groups`: `{pattern}` is not a group; write `com.acme` for \
                 that group, or `com.acme.*` for the groups under it"
            )));
        }
    }
    Ok((url, groups))
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
    // `has_root` as well as `is_absolute`: on Windows `/abs` is not absolute
    // (it has no drive), but it still escapes the project.
    if path.is_absolute() || path.has_root() || raw.contains("..") {
        return Err(JrsError::manifest(format!(
            "`{section}.{key}` must be a relative path inside the project (got `{raw}`)"
        )));
    }
    Ok(path)
}

/// `java.source` / `java.target` / `java.jdk` accept both `21` and `"21"`.
fn optional_release(t: &toml::Table, key: &str) -> Result<Option<u32>> {
    match t.get(key) {
        None => Ok(None),
        // Past `u32::MAX` a release used to wrap silently into a small one;
        // it gets the error an unparseable string gets instead.
        Some(toml::Value::Integer(n)) if *n > 0 => u32::try_from(*n).map(Some).map_err(|_| {
            JrsError::manifest(format!("`java.{key}`: `{n}` is not a Java release number"))
        }),
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

/// A whole number of times, `0` when absent: `test.retries`.
fn optional_count(t: &toml::Table, key: &str, section: &str) -> Result<u32> {
    match t.get(key) {
        None => Ok(0),
        Some(toml::Value::Integer(n)) => u32::try_from(*n).map_err(|_| {
            JrsError::manifest(format!(
                "`{section}.{key}` must be a whole number, 0 or more (got {n})"
            ))
        }),
        Some(_) => Err(JrsError::manifest(format!(
            "`{section}.{key}` must be a whole number, e.g. `2`"
        ))),
    }
}

/// `test.coverage-minimum = { line = 0.80, branch = 0.70 }`: a ratio from 0 to
/// 1 per `JaCoCo` counter. An unknown counter is a warning, like any unknown
/// key; a value that is not a ratio is an error naming its key.
fn coverage_minimum(t: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<CoverageMinimum>> {
    const COUNTERS: &[&str] = &[
        "instruction",
        "branch",
        "line",
        "complexity",
        "method",
        "class",
    ];
    let Some(value) = t.get("coverage-minimum") else {
        return Ok(Vec::new());
    };
    let table = value.as_table().ok_or_else(|| {
        JrsError::manifest(
            "`test.coverage-minimum` must be a table of ratios, e.g. \
             `{ line = 0.80, branch = 0.70 }`",
        )
    })?;
    warn_unknown(table, COUNTERS, "test.coverage-minimum.", warnings);
    let mut minimums = Vec::new();
    for (key, value) in table {
        let Some(counter) = CoverageCounter::from_key(key) else {
            continue;
        };
        let out_of_range = |shown: &str| {
            JrsError::manifest(format!(
                "`test.coverage-minimum.{key}` is {shown}, but a minimum is a ratio from 0 to 1 \
                 (write 0.8 for 80%)"
            ))
        };
        let basis_points = match value {
            toml::Value::Integer(0) => 0,
            toml::Value::Integer(1) => 10_000,
            toml::Value::Integer(n) => return Err(out_of_range(&n.to_string())),
            toml::Value::Float(ratio) if (0.0..=1.0).contains(ratio) => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the ratio is checked to be within 0..=1, so this is 0..=10000"
                )]
                let basis_points = (ratio * 10_000.0).round() as u32;
                basis_points
            }
            toml::Value::Float(ratio) => return Err(out_of_range(&ratio.to_string())),
            _ => {
                return Err(JrsError::manifest(format!(
                    "`test.coverage-minimum.{key}` must be a number from 0 to 1, e.g. `0.8`"
                )));
            }
        };
        minimums.push(CoverageMinimum {
            counter,
            basis_points,
        });
    }
    Ok(minimums)
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

/// `key` is how the message names the value: `project.main-class`.
fn validate_class_name(class: &str, key: &str) -> Result<()> {
    let ok = !class.is_empty()
        && !class.starts_with('.')
        && !class.ends_with('.')
        && !class.contains("..")
        && class
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '$');
    if !ok {
        return Err(JrsError::manifest(format!(
            "`{key}` must be a fully-qualified class name (got `{class}`)"
        )));
    }
    Ok(())
}

/// Where jrs releases are published, for a project that needs a newer one.
pub const RELEASES_URL: &str = "https://github.com/pwittchen/jrs/releases";

/// `project.jrs-version`: the oldest jrs the project builds with, like Cargo's
/// `rust-version`. A jrs older than that stops here, before it reads a key it
/// might not know.
fn parse_jrs_version(project: &toml::Table, path: &Path) -> Result<Option<String>> {
    let Some(value) = project.get("jrs-version") else {
        return Ok(None);
    };
    let bad = |got: String| {
        JrsError::manifest(format!(
            "`project.jrs-version` must be a jrs version as `MAJOR.MINOR` or \
             `MAJOR.MINOR.PATCH`, like \"0.9\" or \"0.9.1\" (got {got})"
        ))
    };
    let Some(required) = value.as_str() else {
        return Err(bad(format!("`{value}`")));
    };
    if release_parts(required).is_none() {
        return Err(bad(quote(required)));
    }
    check_jrs_version(required, env!("CARGO_PKG_VERSION"), path)?;
    Ok(Some(required.to_string()))
}

/// Fail unless `running` is at least `required`, naming both and where newer
/// releases are. `required` has been validated; a `running` version this
/// cannot read (it is jrs's own) passes.
///
/// # Errors
///
/// [`JrsError::Manifest`] when `running` is older than `required`.
pub fn check_jrs_version(required: &str, running: &str, path: &Path) -> Result<()> {
    // A pre-release or build suffix on the running version does not count:
    // `0.9.0-dev` is taken as 0.9.0.
    let numeric = running.split(['-', '+']).next().unwrap_or(running);
    let (Some(needed), Some(have)) = (release_parts(required), release_parts(numeric)) else {
        return Ok(());
    };
    if have >= needed {
        return Ok(());
    }
    Err(JrsError::manifest(format!(
        "{} needs jrs {required} or newer (`project.jrs-version`), but this is jrs \
         {running}\n\ninstall a newer release from {RELEASES_URL}",
        path.display()
    )))
}

/// `0.9` or `0.9.1` as `[0, 9, 0]` / `[0, 9, 1]`; anything else is `None`.
fn release_parts(version: &str) -> Option<[u64; 3]> {
    let parts: Vec<&str> = version.split('.').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut out = [0; 3];
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(out)
}

/// Byte offset to 1-based line and column.
fn locate(text: &str, offset: usize) -> (usize, usize) {
    let head = &text[..offset.min(text.len())];
    let line = head.matches('\n').count() + 1;
    let col = head.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
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
                Dependency::new("com.google.guava", "guava", "33.0.0-jre"),
                Dependency::new("org.apache.commons", "commons-lang3", "3.14.0"),
            ]
        );
        assert_eq!(m.dev_dependencies.len(), 1);
    }

    #[test]
    fn the_long_form_carries_classifiers_exclusions_and_compile_only() {
        let m = parse(
            r#"
[project]
name = "a"
version = "1"
[dependencies]
"org.lwjgl:lwjgl" = "3.3.3"
"org.lwjgl:lwjgl:natives-linux" = "3.3.3"
"io.netty:netty-transport-native-epoll" = { version = "4.1.100.Final", classifier = "linux-x86_64" }
"com.google.guava:guava" = { version = "33.0.0-jre", exclusions = ["com.google.code.findbugs:jsr305", "org.checkerframework:*"] }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.0.0", compile-only = true }
"#,
        )
        .unwrap();
        let d = &m.dependencies;
        assert_eq!(d[0].classifier, None);
        assert_eq!(d[1].classifier.as_deref(), Some("natives-linux"));
        assert_eq!(d[1].key(), "org.lwjgl:lwjgl:natives-linux");
        assert_eq!(d[2].classifier.as_deref(), Some("linux-x86_64"));
        assert_eq!(
            d[3].exclusions,
            vec![
                Exclusion {
                    group: "com.google.code.findbugs".into(),
                    artifact: "jsr305".into()
                },
                Exclusion {
                    group: "org.checkerframework".into(),
                    artifact: "*".into()
                },
            ]
        );
        assert!(d[4].compile_only);
        assert!(!d[3].compile_only);

        // Rendering round-trips, classifiers and all.
        let again = parse(&m.render(None)).unwrap();
        assert_eq!(again.dependencies, m.dependencies);
    }

    #[test]
    fn malformed_long_forms_name_the_key() {
        let base = "[project]\nname='a'\nversion='1'\n";
        let err = parse(&format!(
            "{base}[dependencies]\n'g:a' = {{ version = '1', exclusions = ['nope'] }}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("exclusions"), "{err}");
        let err = parse(&format!(
            "{base}[dependencies]\n'g:a' = {{ version = '1', compile-only = 'yes' }}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("compile-only"), "{err}");
        let err = parse(&format!(
            "{base}[dev-dependencies]\n'g:a' = {{ version = '1', compile-only = true }}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("[dependencies]"), "{err}");
        let err = parse(&format!(
            "{base}[dependencies]\n'g:a:x' = {{ version = '1', classifier = 'y' }}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("classifier"), "{err}");
        let err = parse(&format!(
            "{base}[dependencies]\n'g:a' = {{ version = '1', scope = 'provided' }}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("compile-only"), "{err}");
    }

    #[test]
    fn jvm_arguments_and_the_pinned_jdk_are_read() {
        let m = parse(
            "[project]\nname='a'\nversion='1'\n[java]\njdk = 21\n\
             [run]\njvm-args = ['-Xmx256m', '--enable-preview']\n\
             [test]\njvm-args = ['-Dmode=test']\njacoco-version = '0.8.15'\n\
             [package]\nadd-modules = ['jdk.crypto.ec']",
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(m.java.jdk, Some(21));
        assert_eq!(m.run.jvm_args, vec!["-Xmx256m", "--enable-preview"]);
        assert_eq!(m.test.jvm_args, vec!["-Dmode=test"]);
        assert_eq!(m.test.jacoco_version.as_deref(), Some("0.8.15"));
        assert_eq!(m.package.add_modules, vec!["jdk.crypto.ec"]);

        let again = parse(&m.render(None)).unwrap();
        assert_eq!(again.java, m.java);
        assert_eq!(again.run, m.run);
        assert_eq!(again.test, m.test);
        assert_eq!(again.package, m.package);

        let err =
            parse("[project]\nname='a'\nversion='1'\n[run]\njvm-args = '-Xmx1g'").unwrap_err();
        assert!(err.to_string().contains("run.jvm-args"), "{err}");
    }

    const DEMO: &str = "[project]\nname = \"demo\"\nversion = \"2.1.0\"\n\n";

    fn attributes(m: &Manifest) -> Vec<(String, String)> {
        m.jar_attributes().unwrap()
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn jar_attributes_keep_declaration_order_and_expand_placeholders() {
        let m = parse(&format!(
            "{DEMO}[package]\nnative-image-args = [\"--no-fallback\"]\n\n\
             [package.manifest]\nZ-Last-Alphabetically = \"first\"\n\
             Implementation-Title = \"{{project.name}}\"\n\
             Implementation-Version = \"{{project.version}}\"\n\
             Add-Opens = \"java.base/java.lang {{{{braces}}}}\"\n"
        ))
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(
            attributes(&m),
            pairs(&[
                ("Z-Last-Alphabetically", "first"),
                ("Implementation-Title", "demo"),
                ("Implementation-Version", "2.1.0"),
                ("Add-Opens", "java.base/java.lang {braces}"),
            ])
        );
        assert_eq!(m.package.native_image_args, vec!["--no-fallback"]);

        // Rendered back, it reads the same, placeholders and order included.
        let text = m.render(None);
        assert!(text.contains("\n[package.manifest]\nZ-Last-Alphabetically = \"first\"\n"));
        let again = parse(&text).unwrap();
        assert_eq!(again.package, m.package);
    }

    #[test]
    fn the_inline_table_form_reads_the_same() {
        let m = parse(&format!(
            "{DEMO}[package]\nmanifest = {{ B-Second = \"2\", A-First = \"1\" }}\n"
        ))
        .unwrap();
        assert_eq!(
            attributes(&m),
            pairs(&[("B-Second", "2"), ("A-First", "1")])
        );
    }

    #[test]
    fn attributes_jrs_writes_itself_are_refused() {
        for (name, hint) in [
            ("Main-Class", "project.main-class"),
            ("class-path", "--portable"),
            ("Created-By", "writes it itself"),
            ("MANIFEST-VERSION", "writes it itself"),
            ("Name", "per-entry section"),
        ] {
            let err = parse(&format!("{DEMO}[package.manifest]\n{name} = \"x\"\n")).unwrap_err();
            assert_eq!(err.exit_code(), 2);
            let text = err.to_string();
            assert!(
                text.contains(&format!("`package.manifest.{name}`")),
                "{text}"
            );
            assert!(text.contains("belongs to jrs"), "{text}");
            assert!(text.contains(hint), "{text}");
        }
    }

    #[test]
    fn attribute_names_follow_the_jar_specification() {
        let longest = "A".repeat(70);
        for ok in ["X", "X_1-a", "Premain-Class", "9-Lives", longest.as_str()] {
            let m = parse(&format!("{DEMO}[package.manifest]\n\"{ok}\" = \"v\"\n")).unwrap();
            assert_eq!(attributes(&m)[0].0, ok);
        }
        let too_long = "A".repeat(71);
        for bad in [
            "-Leading",
            "_Leading",
            "Has.Dot",
            "Has Space",
            "Ümlaut",
            too_long.as_str(),
        ] {
            let err = parse(&format!("{DEMO}[package.manifest]\n\"{bad}\" = \"v\"\n")).unwrap_err();
            assert!(err.to_string().contains("attribute name"), "{bad}: {err}");
        }
    }

    #[test]
    fn attribute_values_are_one_line_strings() {
        for (value, complaint) in [
            ("\"a\\nb\"", "one line"),
            ("1", "must be a string"),
            ("\"{classpath}\"", "not known when a jar is written"),
            ("\"{nope}\"", "unknown placeholder"),
            ("\"{unclosed\"", "unclosed"),
        ] {
            let err = parse(&format!("{DEMO}[package.manifest]\nX-Value = {value}\n")).unwrap_err();
            assert!(err.to_string().contains(complaint), "{value}: {err}");
        }
        let err = parse(&format!("{DEMO}[package]\nmanifest = [\"x\"]\n")).unwrap_err();
        assert!(err.to_string().contains("must be a table"), "{err}");
    }

    #[test]
    fn attribute_names_are_case_insensitive() {
        let err = parse(&format!(
            "{DEMO}[package.manifest]\nX-Flavour = \"a\"\nx-flavour = \"b\"\n"
        ))
        .unwrap_err();
        assert!(
            err.to_string().contains("`X-Flavour` is already set"),
            "{err}"
        );
    }

    #[test]
    fn runtime_only_and_local_jars_parse_and_render_back() {
        let m = parse(
            r#"
[project]
name = "a"
version = "1"
[dependencies]
"org.postgresql:postgresql" = { version = "42.7.3", runtime-only = true }
ojdbc = { path = "libs/ojdbc11.jar" }
"vendor-api" = { path = 'libs\vendor api.jar', compile-only = true }
[dev-dependencies]
fixtures = { path = "test-libs/fixtures.jar" }
"#,
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert!(m.dependencies[0].runtime_only);
        assert!(!m.dependencies[0].is_plain());
        let ojdbc = &m.dependencies[1];
        assert!(ojdbc.is_local());
        assert_eq!(ojdbc.key(), "ojdbc");
        assert_eq!(ojdbc.path.as_deref(), Some("libs/ojdbc11.jar"));
        assert_eq!(
            m.dependencies[2].path.as_deref(),
            Some("libs/vendor api.jar"),
            "a backslash becomes a slash"
        );
        assert!(m.dependencies[2].compile_only);
        assert!(m.dev_dependencies[0].is_local());
        assert!(m.effective_dependencies().iter().any(Dependency::is_local));

        let text = m.render(None);
        assert!(
            text.contains("\"ojdbc\" = { path = \"libs/ojdbc11.jar\" }"),
            "{text}"
        );
        assert!(
            text.contains("{ version = \"42.7.3\", runtime-only = true }"),
            "{text}"
        );
        let again = parse(&text).unwrap();
        assert_eq!(again.dependencies, m.dependencies);
        assert_eq!(again.dev_dependencies, m.dev_dependencies);
    }

    #[test]
    fn managed_versions_and_boms_parse_and_render_back() {
        let m = parse(
            r#"
[project]
name = "a"
version = "1"
[managed]
"org.springframework.boot:spring-boot-dependencies" = { version = "3.3.4", bom = true }
"com.fasterxml.jackson.core:jackson-databind" = "2.17.2"
[dependencies]
"org.springframework.boot:spring-boot-starter-web" = {}
"org.slf4j:slf4j-api" = { exclusions = ["x:y"] }
[dev-dependencies]
"org.springframework.boot:spring-boot-starter-test" = {}
"#,
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(m.managed.len(), 2);
        assert!(m.managed[0].bom && !m.managed[1].bom);
        assert_eq!(
            m.managed[1].key(),
            "com.fasterxml.jackson.core:jackson-databind"
        );
        let web = &m.dependencies[0];
        assert!(web.is_managed() && !m.dependencies[0].is_local());
        assert_eq!(
            web.to_string(),
            "org.springframework.boot:spring-boot-starter-web"
        );
        assert!(m.dev_dependencies[0].is_managed());

        let text = m.render(None);
        assert!(
            text.contains(
                "\n[managed]\n\
                 \"org.springframework.boot:spring-boot-dependencies\" = { version = \"3.3.4\", bom = true }\n\
                 \"com.fasterxml.jackson.core:jackson-databind\" = \"2.17.2\"\n\n[dependencies]\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("\"org.springframework.boot:spring-boot-starter-web\" = {}\n"),
            "{text}"
        );
        assert!(
            text.contains("\"org.slf4j:slf4j-api\" = { exclusions = [\"x:y\"] }\n"),
            "{text}"
        );
        let again = parse(&text).unwrap();
        assert_eq!(again.managed, m.managed);
        assert_eq!(again.dependencies, m.dependencies);
        assert_eq!(again.dev_dependencies, m.dev_dependencies);
    }

    #[test]
    fn a_task_runs_a_class_from_dependencies_of_its_own() {
        let m = parse(
            r#"
[project]
name = "a"
version = "1"
[tasks.format]
main = "com.google.googlejavaformat.java.Main"
args = ["--replace", "{root}/src/main/java"]
[tasks.format.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.22.0"
"org.example:extra" = { version = "2.0", exclusions = ["x:y"] }
[tasks.gen]
script = "build/Gen.java"
[tasks.gen.dependencies]
"com.squareup:javapoet" = "1.13.0"
"#,
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let format = m.task("format").unwrap();
        assert_eq!(
            format.action,
            Some(Action::Main("com.google.googlejavaformat.java.Main".into()))
        );
        assert_eq!(format.dependencies.len(), 2);
        assert_eq!(format.dependencies[1].exclusions.len(), 1);
        assert_eq!(format.tool_name(), "tasks.format");
        assert_eq!(m.task("gen").unwrap().dependencies.len(), 1);
        assert!(
            m.dependencies.is_empty(),
            "a task's dependencies are not the project's"
        );

        let text = m.render(None);
        assert!(
            text.contains("main = \"com.google.googlejavaformat.java.Main\"\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "\n[tasks.format.dependencies]\n\
                 \"com.google.googlejavaformat:google-java-format\" = \"1.22.0\"\n"
            ),
            "{text}"
        );
        assert_eq!(parse(&text).unwrap().tasks, m.tasks);
    }

    #[test]
    fn a_tasks_dependencies_go_only_where_they_mean_something() {
        let base = "[project]\nname='a'\nversion='1'\n";
        let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
        for (body, needle) in [
            ("[tasks.t]\nmain = 'x.Y'", "has none"),
            (
                "[tasks.t]\nshell = 'x'\n[tasks.t.dependencies]\n'g:a' = '1'",
                "`main` or `script` action",
            ),
            (
                "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\nd = { path = 'd.jar' }",
                "a local jar",
            ),
            (
                "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = { version = '1', compile-only = true }",
                "compile-only",
            ),
            (
                "[managed]\n'g:a' = '1'\n[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = {}",
                "[managed]",
            ),
            (
                "[tasks.t]\nmain = 'x.Y'\nscript = 'A.java'\n[tasks.t.dependencies]\n'g:a' = '1'",
                "exactly one of",
            ),
            (
                "[tasks.t]\nmain = 'not a class'\n[tasks.t.dependencies]\n'g:a' = '1'",
                "`tasks.t.main` must be a fully-qualified class name",
            ),
            (
                "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = '[1,2)'",
                "",
            ),
        ] {
            // A range is refused at resolution, as in [dependencies].
            if needle.is_empty() {
                assert!(parse(&format!("{base}{body}")).is_ok(), "{body}");
                continue;
            }
            let e = err(body);
            assert!(e.contains(needle), "{body}: {e}");
        }
    }

    #[test]
    fn a_versionless_dependency_needs_something_to_manage_it() {
        let base = "[project]\nname='a'\nversion='1'\n";
        let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
        for (body, needle) in [
            ("[dependencies]\n'g:a' = {}", "there is no [managed] table"),
            (
                "[managed]\n'g:b' = '1'\n[dev-dependencies]\n'g:a' = {}",
                "[managed] does not name it",
            ),
            ("[dependencies]\n'g:a' = ''", "write `{}`"),
            (
                "[dependencies]\n'g:a' = { version = '' }",
                "leave `version` out",
            ),
            ("[managed]\n'g:a' = '[1,2)'", "range"),
            ("[managed]\n'g:a:natives' = '1'", "every classifier"),
            (
                "[managed]\n'g:a' = { version = '1', scope = 'x' }",
                "unknown key `scope`",
            ),
            ("[managed]\n'g:a' = { bom = true }", "missing a `version`"),
            (
                "[managed]\n'g:a' = { version = '1', bom = 'yes' }",
                "`true` or `false`",
            ),
            ("[managed]\n'g:a' = ''", "empty version"),
        ] {
            let e = err(body);
            assert!(e.contains(needle), "{body}: {e}");
        }
        // With a BOM there, what it covers is only known at resolution.
        let m = parse(&format!(
            "{base}[managed]\n'g:bom' = {{ version = '1', bom = true }}\n\
             [dependencies]\n'g:a' = {{}}"
        ))
        .unwrap();
        assert!(m.dependencies[0].is_managed());
    }

    #[test]
    fn malformed_runtime_only_and_local_jars_name_the_problem() {
        let base = "[project]\nname='a'\nversion='1'\n";
        let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
        for (body, needle) in [
            (
                "[dependencies]\n'g:a' = { version = '1', compile-only = true, runtime-only = true }",
                "drop both",
            ),
            (
                "[dependencies]\n'g:a' = { version = '1', runtime-only = 'yes' }",
                "runtime-only",
            ),
            (
                "[dev-dependencies]\n'g:a' = { version = '1', runtime-only = true }",
                "[dependencies]",
            ),
            (
                "[dependencies]\n'g:a' = { path = 'libs/a.jar' }",
                "name of its own",
            ),
            (
                "[dependencies]\ndriver = { path = 'libs/a.jar', version = '1' }",
                "no coordinate",
            ),
            (
                "[dependencies]\ndriver = { path = '../a.jar' }",
                "inside the project",
            ),
            (
                "[dependencies]\ndriver = { path = '/opt/a.jar' }",
                "inside the project",
            ),
            (
                "[dependencies]\ndriver = { path = '' }",
                "inside the project",
            ),
            ("[dependencies]\ndriver = 'libs/a.jar'", "group:artifact"),
            (
                "[dev-dependencies]\ndriver = { path = 'a.jar', compile-only = true }",
                "[dependencies]",
            ),
        ] {
            let message = err(body);
            assert!(message.contains(needle), "{body}: {message}");
        }
    }

    #[test]
    fn a_repository_can_be_confined_to_groups() {
        let m = parse(
            "[project]\nname='a'\nversion='1'\n[repositories]\n\
             internal = { url = 'https://nexus.example.com/m2/', groups = ['com.acme', 'com.acme.*'] }\n\
             plain = 'https://x.example.com'",
        )
        .unwrap();
        let internal = &m.repositories[0];
        assert_eq!(internal.url, "https://nexus.example.com/m2");
        assert_eq!(internal.groups, ["com.acme", "com.acme.*"]);
        assert!(internal.claims("com.acme") && internal.claims("com.acme.billing"));
        assert!(!internal.claims("com.acmex"));
        assert!(m.repositories[1].groups.is_empty());
        assert!(!m.repositories[1].claims("com.acme"), "claims nothing");
        assert_eq!(m.repositories[2].url, CENTRAL_URL);
        let again = parse(&m.render(None)).unwrap();
        assert_eq!(again.repositories, m.repositories);

        let base = "[project]\nname='a'\nversion='1'\n[repositories]\n";
        for (body, needle) in [
            (
                "internal = { url = 'https://x', group = ['com.acme'] }",
                "unknown key `group`",
            ),
            ("internal = { groups = ['com.acme'] }", "needs a `url`"),
            ("internal = { url = 'https://x', groups = [] }", "empty"),
            (
                "internal = { url = 'https://x', groups = ['com.*.x'] }",
                "not a group",
            ),
            (
                "internal = { url = 'https://x', groups = ['*'] }",
                "not a group",
            ),
            ("internal = 3", "URL string"),
        ] {
            let message = parse(&format!("{base}{body}")).unwrap_err().to_string();
            assert!(message.contains(needle), "{body}: {message}");
        }
    }

    #[test]
    fn group_patterns_match_a_group_or_the_groups_below_it() {
        assert!(group_matches("com.acme", "com.acme"));
        assert!(!group_matches("com.acme", "com.acme.billing"));
        assert!(group_matches("com.acme.*", "com.acme.billing"));
        assert!(group_matches("com.acme.*", "com.acme.billing.api"));
        assert!(!group_matches("com.acme.*", "com.acme"));
        assert!(!group_matches("com.acme.*", "com.acmex.billing"));
        assert!(valid_group_pattern("io.github.some-one_2.*"));
        assert!(!valid_group_pattern("com..acme"));
        assert!(!valid_group_pattern(".*"));
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
            vec![Repository::new(CENTRAL_NAME, CENTRAL_URL)]
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
    fn the_test_resource_directory_is_configurable() {
        let derived = parse("[project]\nname='a'\nversion='1'\ntest-dir='test/java'").unwrap();
        assert_eq!(derived.test_resource_dir, PathBuf::from("test/resources"));

        let explicit =
            parse("[project]\nname='a'\nversion='1'\ntest-resource-dir='fixtures'").unwrap();
        assert_eq!(explicit.test_resource_path(), PathBuf::from("/p/fixtures"));
        assert!(explicit.warnings.is_empty(), "{:?}", explicit.warnings);
        let text = explicit.render(None);
        assert!(text.contains("test-resource-dir = \"fixtures\""), "{text}");
        assert!(!derived.render(None).contains("test-resource-dir"));

        let err = parse("[project]\nname='a'\nversion='1'\ntest-resource-dir='/abs'").unwrap_err();
        assert!(err.to_string().contains("relative path"), "{err}");
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
    fn retries_and_coverage_minimums_parse_and_render_back() {
        let m = parse(
            "[project]\nname='a'\nversion='1'\n\
             [test]\nretries = 2\ncoverage-minimum = { line = 0.80, branch = 0.755, class = 1 }",
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(m.test.retries, 2);
        assert_eq!(
            m.test.coverage_minimum,
            [
                CoverageMinimum {
                    counter: CoverageCounter::Line,
                    basis_points: 8000
                },
                CoverageMinimum {
                    counter: CoverageCounter::Branch,
                    basis_points: 7550
                },
                CoverageMinimum {
                    counter: CoverageCounter::Class,
                    basis_points: 10_000
                },
            ],
            "in declaration order"
        );
        let rendered = m.render(None);
        assert!(rendered.contains("retries = 2\n"), "{rendered}");
        assert!(
            rendered.contains("coverage-minimum = { line = 0.8, branch = 0.755, class = 1.0 }\n"),
            "{rendered}"
        );
        assert_eq!(parse(&rendered).unwrap().test, m.test);

        let plain = parse("[project]\nname='a'\nversion='1'\n[test]\n").unwrap();
        assert_eq!(plain.test.retries, 0);
        assert!(plain.test.coverage_minimum.is_empty());
    }

    #[test]
    fn coverage_minimums_are_ratios_checked_per_key() {
        let error = |body: &str| {
            parse(&format!("[project]\nname='a'\nversion='1'\n[test]\n{body}"))
                .unwrap_err()
                .to_string()
        };
        let err = error("coverage-minimum = { line = 80 }");
        assert!(err.contains("`test.coverage-minimum.line` is 80"), "{err}");
        assert!(err.contains("0.8 for 80%"), "{err}");
        let err = error("coverage-minimum = { branch = 1.5 }");
        assert!(
            err.contains("`test.coverage-minimum.branch` is 1.5"),
            "{err}"
        );
        let err = error("coverage-minimum = { branch = -0.1 }");
        assert!(err.contains("`test.coverage-minimum.branch`"), "{err}");
        let err = error("coverage-minimum = { line = '80%' }");
        assert!(err.contains("must be a number from 0 to 1"), "{err}");
        let err = error("coverage-minimum = 0.8");
        assert!(err.contains("must be a table of ratios"), "{err}");
        let err = error("retries = -1");
        assert!(
            err.contains("`test.retries` must be a whole number"),
            "{err}"
        );
        let err = error("retries = 'twice'");
        assert!(err.contains("`test.retries`"), "{err}");

        // A counter jrs does not know is a warning, as any unknown key is.
        let m = parse(
            "[project]\nname='a'\nversion='1'\n[test]\ncoverage-minimum = { lines = 0.8, line = 0.5 }",
        )
        .unwrap();
        assert_eq!(m.test.coverage_minimum.len(), 1);
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("`test.coverage-minimum.lines`")),
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

    /// A manifest with nothing but `[project]` and `extra`.
    fn with(extra: &str) -> Result<Manifest> {
        parse(&format!("[project]\nname='a'\nversion='1'\n{extra}"))
    }

    const TASKS: &str = r#"
[tasks.build-info]
description = "Generate BuildInfo.java"
script = "build/GenerateBuildInfo.java"
args = ["{target}/generated/sources", "{project.version}"]
inputs = ["build/GenerateBuildInfo.java", ".git/HEAD"]
outputs = ["{target}/generated/sources"]
source-outputs = ["{target}/generated/sources"]

[tasks.checksum]
shell = "shasum -a 256 \"$JRS_JAR\" > \"$JRS_JAR.sha256\""

[tasks.format]
run = ["google-java-format", "--replace"]
env = { MODE = "ci" }
cwd = "src"

[tasks.release]
depends-on = ["package", "checksum"]

[hooks]
pre-compile = ["build-info"]
post-package = ["checksum"]
"#;

    #[test]
    fn the_task_example_from_the_proposal_parses() {
        let m = with(TASKS).unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let names: Vec<&str> = m.tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["build-info", "checksum", "format", "release"]);
        assert!(matches!(m.tasks[0].action, Some(Action::Script(_))));
        assert!(matches!(m.tasks[1].action, Some(Action::Shell(_))));
        assert!(matches!(&m.tasks[2].action, Some(Action::Run(argv)) if argv.len() == 2));
        assert_eq!(m.tasks[2].env[0].0, "MODE");
        assert!(m.tasks[3].action.is_none(), "release only aggregates");
        assert_eq!(
            m.tasks[3].depends_on,
            [
                TaskRef::Builtin(Builtin::Package),
                TaskRef::Task("checksum".into())
            ]
        );
        assert_eq!(m.hooks.tasks(Hook::PreCompile), ["build-info"]);
        assert_eq!(m.hooks.tasks(Hook::PostPackage), ["checksum"]);
        assert!(m.hooks.tasks(Hook::PreRun).is_empty());
        assert_eq!(m.task("format").unwrap().name, "format");
    }

    #[test]
    fn a_task_runs_exactly_one_action_or_aggregates() {
        let err = with("[tasks.t]\nrun = ['x']\nshell = 'y'\n").unwrap_err();
        assert!(err.to_string().contains("`run` and `shell`"), "{err}");
        let err = with("[tasks.t]\ndescription = 'nothing'\n").unwrap_err();
        assert!(err.to_string().contains("`tasks.t` does nothing"), "{err}");
        let err = with("[tasks.t]\nrun = []\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("`tasks.t.run` must name a program"),
            "{err}"
        );
        let err = with("[tasks.t]\nrun = 'x y'\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("`tasks.t.run` must be an array of strings"),
            "{err}"
        );
        let err = with("[tasks.t]\nshell = ['x']\n").unwrap_err();
        assert!(err.to_string().contains("`tasks.t.shell`"), "{err}");
    }

    #[test]
    fn task_names_are_lowercase_and_never_a_command() {
        for name in ["Build", "1st", "under_score"] {
            let err = with(&format!("[tasks.{name}]\nshell = 'x'\n")).unwrap_err();
            assert!(
                err.to_string().contains("lowercase letters"),
                "{name}: {err}"
            );
        }
        for name in ["build", "test", "task", "clean", "help"] {
            let err = with(&format!("[tasks.{name}]\nshell = 'x'\n")).unwrap_err();
            assert!(
                err.to_string().contains("is a jrs command"),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn depends_on_takes_tasks_and_four_built_ins() {
        let m = with("[tasks.t]\ndepends-on = ['build', 'test', 'package', 'doc']\n").unwrap();
        assert_eq!(m.tasks[0].depends_on.len(), 4);
        let err = with("[tasks.t]\ndepends-on = ['run']\n").unwrap_err();
        assert!(err.to_string().contains("cannot depend on"), "{err}");
        let err = with("[tasks.t]\ndepends-on = ['build', 'build']\n").unwrap_err();
        assert!(err.to_string().contains("names `build` twice"), "{err}");
    }

    #[test]
    fn unknown_task_and_hook_keys_warn() {
        let m = with("[tasks.t]\nshell = 'x'\nretries = 3\n[hooks]\npre-deploy = ['t']\n").unwrap();
        assert_eq!(m.warnings.len(), 2, "{:?}", m.warnings);
        assert!(m.warnings.iter().any(|w| w.contains("`tasks.t.retries`")));
        assert!(m.warnings.iter().any(|w| w.contains("`hooks.pre-deploy`")));
    }

    #[test]
    fn jrs_environment_names_belong_to_jrs() {
        for key in ["JRS_TASK", "jrs_mine"] {
            let err = with(&format!(
                "[tasks.t]\nshell = 'x'\nenv = {{ {key} = 'v' }}\n"
            ))
            .unwrap_err();
            assert!(
                err.to_string().contains(&format!("`tasks.t.env.{key}`")),
                "{err}"
            );
        }
        let err = with("[tasks.t]\nshell = 'x'\nenv = { A = 1 }\n").unwrap_err();
        assert!(err.to_string().contains("must be a string"), "{err}");
    }

    #[test]
    fn placeholders_are_checked_when_the_manifest_loads() {
        let err = with("[tasks.t]\nshell = 'x'\nargs = ['{tagret}/out']\n").unwrap_err();
        assert!(
            err.to_string()
                .contains("`tasks.t.args`: unknown placeholder `{tagret}`"),
            "{err}"
        );
        let err = with("[tasks.t]\nrun = ['x', '{root']\n").unwrap_err();
        assert!(err.to_string().contains("unclosed"), "{err}");
        let err = with("[tasks.t]\nrun = ['x', 'a}b']\n").unwrap_err();
        assert!(err.to_string().contains("stray"), "{err}");
        let err = with("[tasks.t]\nshell = 'x'\ninputs = ['{classpath}']\n").unwrap_err();
        assert!(
            err.to_string().contains("is a classpath, not a path"),
            "{err}"
        );
        // A shell string is the shell's to expand: braces there are literal.
        with("[tasks.t]\nshell = 'echo ${HOME} {not-a-placeholder}'\n").unwrap();
    }

    #[test]
    fn a_template_splits_into_text_and_placeholders() {
        let t = Template::parse("{root}/a-{{b}}-{jar}").unwrap();
        assert_eq!(
            t.segments,
            [
                Segment::Placeholder(Placeholder::Root),
                Segment::Text("/a-{b}-".into()),
                Segment::Placeholder(Placeholder::Jar),
            ]
        );
        assert_eq!(t.raw, "{root}/a-{{b}}-{jar}");
        let plain = Template::parse("no braces").unwrap();
        assert_eq!(plain.segments, [Segment::Text("no braces".into())]);
    }

    #[test]
    fn rendering_keeps_tasks_and_hooks() {
        let text = r#"
[project]
name = "a"
version = "1"

[tasks.gen]
description = "Generate"
run = ["java", "@{classpath-argfile}", "Gen", "{{literal}}"]
env = { "MODE" = "fast", "OUT" = "{target}/gen" }
cwd = "tools"
inputs = ["src/gen"]
outputs = ["target/gen"]
source-outputs = ["target/gen"]

[tasks.check-all]
depends-on = ["test", "gen"]

[tasks.sh]
shell = "echo \"$JRS_JAR\""
depends-on = ["build"]

[hooks]
pre-compile = ["gen"]
post-package = ["sh"]
"#;
        let manifest = parse(text).unwrap();
        let again = parse(&manifest.render(None)).unwrap();
        assert_eq!(again.tasks, manifest.tasks);
        assert_eq!(again.hooks, manifest.hooks);
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
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

    #[test]
    fn each_language_table_parses_and_round_trips() {
        let m = with(
            "[kotlin]\nversion = '2.4.20'\nkotlinc-args = ['-Xjsr305=strict']\n\
             compiler-jvm-args = ['-Xmx2g']\n\
             [scala]\nversion = '3.9.0'\nsource-dir = 'src/main/sc'\nscalac-args = ['-deprecation']\n\
             [groovy]\nversion = '5.1.2'\ntest-dir = 'spec'\ngroovyc-args = ['--compile-static']\n",
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let keys: Vec<&str> = m.languages.iter().map(|c| c.language.key()).collect();
        assert_eq!(keys, ["kotlin", "scala", "groovy"]);
        let kotlin = m.language(Language::Kotlin).unwrap();
        assert_eq!(kotlin.version, "2.4.20");
        assert_eq!(kotlin.source_dir, PathBuf::from("src/main/kotlin"));
        assert_eq!(kotlin.test_dir, PathBuf::from("src/test/kotlin"));
        assert_eq!(kotlin.compiler_args, ["-Xjsr305=strict"]);
        assert_eq!(kotlin.compiler_jvm_args, ["-Xmx2g"]);
        let scala = m.language(Language::Scala).unwrap();
        assert_eq!(scala.source_dir, PathBuf::from("src/main/sc"));
        assert_eq!(scala.compiler_args, ["-deprecation"]);
        let groovy = m.language(Language::Groovy).unwrap();
        assert_eq!(groovy.test_dir, PathBuf::from("spec"));

        let text = m.render(None);
        assert!(text.contains("[kotlin]\nversion = \"2.4.20\"\n"), "{text}");
        assert!(
            !text.contains("src/main/kotlin"),
            "defaults stay out:\n{text}"
        );
        let again = parse(&text).unwrap();
        assert_eq!(again.languages, m.languages);
        assert!(!with("").unwrap().render(None).contains("[kotlin]"));
    }

    #[test]
    fn a_language_table_needs_a_version_it_can_pin() {
        let err = with("[kotlin]\n").unwrap_err().to_string();
        assert!(err.contains("`kotlin.version`"), "{err}");
        assert!(err.contains("version = \""), "{err}");
        let err = with("[scala]\nversion = 3\n").unwrap_err().to_string();
        assert!(
            err.contains("`scala.version` must be a version string"),
            "{err}"
        );
        let err = with("[groovy]\nversion = '[4.0,5.0)'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("range"), "{err}");
        let err = with("[kotlin]\nversion = '1.9.24'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Kotlin 2.0 or newer"), "{err}");
        let err = with("[scala]\nversion = '2.12.18'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("2.12 is not supported"), "{err}");
        let err = with("[groovy]\nversion = '3.0.22'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Groovy 4.0 or newer"), "{err}");
        let err = with("[kotlin]\nversion = '2.4.20'\nsource-dir = '../elsewhere'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`kotlin.source-dir`"), "{err}");
    }

    #[test]
    fn unknown_language_keys_warn() {
        let m = with("[kotlin]\nversion = '2.4.20'\nscalac-args = ['-x']\n").unwrap();
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        assert!(
            m.warnings[0].contains("`kotlin.scalac-args`"),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn the_runtime_library_is_implied_unless_declared() {
        let m = with("[kotlin]\nversion = '2.4.20'\n[dependencies]\n'g:a' = '1'\n").unwrap();
        let effective: Vec<String> = m
            .effective_dependencies()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            effective,
            ["g:a:1", "org.jetbrains.kotlin:kotlin-stdlib:2.4.20"],
            "the implied library comes last"
        );
        assert_eq!(m.implied_dependencies()[0].1, Language::Kotlin);
        assert!(
            !m.render(None).contains("kotlin-stdlib"),
            "an implied library is never written into jrs.toml"
        );

        // A declaration in either table replaces it, and a Java manifest
        // implies nothing.
        let dev = with(
            "[groovy]\nversion = '5.1.2'\n[dev-dependencies]\n'org.apache.groovy:groovy' = '5.1.2'\n",
        )
        .unwrap();
        assert!(dev.implied_dependencies().is_empty());
        assert!(dev.effective_dependencies().is_empty());
        let main =
            with("[kotlin]\nversion = '2.4.20'\n[dependencies]\n'org.jetbrains.kotlin:kotlin-stdlib' = '2.4.20'\n")
                .unwrap();
        assert!(main.implied_dependencies().is_empty());
        assert_eq!(main.effective_dependencies(), main.dependencies);
        assert!(with("").unwrap().implied_dependencies().is_empty());

        // Scala 3.8 split its library in two, and both are implied.
        let scala = with("[scala]\nversion = '3.9.0'\n").unwrap();
        let implied: Vec<String> = scala
            .implied_dependencies()
            .iter()
            .map(|(d, _)| d.to_string())
            .collect();
        assert_eq!(
            implied,
            [
                "org.scala-lang:scala3-library_3:3.9.0",
                "org.scala-lang:scala-library:3.9.0"
            ]
        );
    }

    #[test]
    fn a_runtime_library_declared_at_another_version_warns() {
        let m = with(
            "[kotlin]\nversion = '2.4.20'\n[dependencies]\n'org.jetbrains.kotlin:kotlin-stdlib' = '2.5.0'\n",
        )
        .unwrap();
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        assert!(
            m.warnings[0].contains("declared at 2.5.0"),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn kotlin_reads_utf8_only() {
        let m = with("[java]\nencoding = 'ISO-8859-1'\n[kotlin]\nversion = '2.4.20'\n").unwrap();
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("kotlinc reads sources as UTF-8")),
            "{:?}",
            m.warnings
        );
        let m = with("[java]\nencoding = 'utf8'\n[kotlin]\nversion = '2.4.20'\n").unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn javac_args_groovy_cannot_hand_on_are_a_manifest_error() {
        let err =
            with("[java]\njavac-args = ['-Xlint:all', 'oops']\n[groovy]\nversion = '5.1.2'\n")
                .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("`oops`"), "{err}");
        // Without [groovy], javac-args are javac's own business.
        with("[java]\njavac-args = ['-Xlint:all', 'oops']\n").unwrap();
    }

    // ---- project.jrs-version ----------------------------------------------

    #[test]
    fn a_jrs_version_this_jrs_satisfies_is_kept_and_rendered() {
        let m = parse("[project]\nname='a'\nversion='1'\njrs-version='0.1'").unwrap();
        assert_eq!(m.jrs_version.as_deref(), Some("0.1"));
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let text = m.render(None);
        assert!(text.contains("jrs-version = \"0.1\"\n"), "{text}");
        let again = Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
        assert_eq!(again.jrs_version.as_deref(), Some("0.1"));
        // `init` and `migrate` start from a blank manifest, which pins nothing.
        assert!(
            !blank("a", "1", Path::new("/p"))
                .render(None)
                .contains("jrs-version")
        );
    }

    #[test]
    fn a_newer_jrs_version_stops_before_any_other_key_is_read() {
        // A manifest for a future jrs: an unknown key, and a dependency form
        // this jrs cannot read. The version is the only thing it says.
        let err = parse(
            "[project]\nname='a'\nversion='1'\njrs-version='999.0'\nfuture-key=1\n\
             [dependencies]\n'g:a' = { path = 'libs/a.jar' }\n",
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("needs jrs 999.0 or newer"), "{msg}");
        assert!(
            msg.contains(&format!("this is jrs {}", env!("CARGO_PKG_VERSION"))),
            "{msg}"
        );
        assert!(msg.contains(RELEASES_URL), "{msg}");
        assert!(msg.contains("/p/jrs.toml"), "{msg}");
    }

    #[test]
    fn jrs_versions_compare_numerically() {
        let path = Path::new("jrs.toml");
        for (required, running) in [
            ("0.9", "0.9.0"),
            ("0.9.0", "0.9.0"),
            ("0.9", "0.10.0"),
            ("0.9.1", "0.9.2"),
            ("1.0", "1.0.0-dev"),
            ("0.4", "1.0.0"),
        ] {
            check_jrs_version(required, running, path)
                .unwrap_or_else(|e| panic!("{required} vs {running}: {e}"));
        }
        for (required, running) in [("0.10", "0.9.9"), ("0.9.1", "0.9.0"), ("1.0", "0.99.99")] {
            assert!(
                check_jrs_version(required, running, path).is_err(),
                "{required} vs {running}"
            );
        }
    }

    #[test]
    fn a_malformed_jrs_version_names_the_key() {
        for value in [
            "'1'",
            "'0.9.1.2'",
            "'0.x'",
            "'v0.9'",
            "'0.9-rc1'",
            "''",
            "0.9",
            "[1]",
        ] {
            let err = parse(&format!(
                "[project]\nname='a'\nversion='1'\njrs-version={value}"
            ))
            .unwrap_err();
            assert_eq!(err.exit_code(), 2, "{value}");
            assert!(
                err.to_string().contains("`project.jrs-version`"),
                "{value}: {err}"
            );
        }
    }

    const JVM_HEAD: &str = "[project]\nname='a'\nversion='1'\n";

    #[test]
    fn agents_environment_and_working_directory_are_read_and_rendered_back() {
        let m = parse(&format!(
            "{JVM_HEAD}[run]\njava-agents = ['io.opentelemetry:otel-agent']\n\
             env = {{ APP_MODE = 'dev', OUT = '{{target}}/out' }}\ncwd = '{{target}}/work'\n\
             [test]\njava-agents = ['org.mockito:mockito-core', 'net.bytebuddy:byte-buddy-agent']\n\
             env = {{ CP = '{{test-classpath}}' }}\n"
        ))
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(
            m.run.java_agents,
            vec![Ga::new("io.opentelemetry", "otel-agent")]
        );
        let names: Vec<&str> = m.run.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["APP_MODE", "OUT"], "declaration order is kept");
        assert_eq!(m.run.cwd.as_ref().unwrap().raw, "{target}/work");
        assert_eq!(m.test.java_agents.len(), 2);
        assert_eq!(
            m.test.java_agents[1].to_string(),
            "net.bytebuddy:byte-buddy-agent"
        );
        assert!(
            m.test.env[0]
                .1
                .placeholders()
                .any(|p| p == Placeholder::TestClasspath),
            "a classpath is a value an environment variable can hold"
        );

        let text = m.render(None);
        assert!(
            text.contains("java-agents = [\"io.opentelemetry:otel-agent\"]"),
            "{text}"
        );
        assert!(
            text.contains("env = { \"APP_MODE\" = \"dev\", \"OUT\" = \"{target}/out\" }"),
            "{text}"
        );
        let again = parse(&text).unwrap();
        assert_eq!(again.run, m.run);
        assert_eq!(again.test, m.test);

        // A [run] with only a working directory still renders.
        let cwd_only = parse(&format!("{JVM_HEAD}[run]\ncwd = 'work'\n")).unwrap();
        assert!(cwd_only.render(None).contains("[run]\ncwd = \"work\"\n"));
    }

    #[test]
    fn agents_are_named_by_group_and_artifact_once() {
        for bad in [
            "mockito-core",
            "org.mockito:mockito-core:5.14.2",
            "org.mockito:",
            " a:b",
        ] {
            let err = parse(&format!("{JVM_HEAD}[test]\njava-agents = ['{bad}']\n")).unwrap_err();
            assert_eq!(err.exit_code(), 2);
            let msg = err.to_string();
            assert!(msg.contains("`test.java-agents`"), "{bad}: {msg}");
            assert!(
                msg.contains("not a `group:artifact` coordinate"),
                "{bad}: {msg}"
            );
        }
        let err = parse(&format!("{JVM_HEAD}[run]\njava-agents = ['a:b', 'a:b']\n")).unwrap_err();
        assert!(err.to_string().contains("names `a:b` twice"), "{err}");
        let err = parse(&format!("{JVM_HEAD}[run]\njava-agents = 'a:b'\n")).unwrap_err();
        assert!(err.to_string().contains("run.java-agents"), "{err}");
    }

    #[test]
    fn the_jvm_environment_is_checked_like_a_tasks() {
        let cases = [
            (
                "[run]\nenv = { JRS_X = '1' }",
                "`run.env.JRS_X`",
                "jrs's own",
            ),
            (
                "[test]\nenv = { A = 1 }",
                "`test.env.A`",
                "must be a string",
            ),
            (
                "[test]\nenv = { A = '{nope}' }",
                "`test.env.A`",
                "unknown placeholder",
            ),
            ("[run]\nenv = { J = '{jar}' }", "`run.env.J`", "no jar"),
            (
                "[test]\nenv = { CP = '@{classpath-argfile}' }",
                "`test.env.CP`",
                "a task's own",
            ),
            ("[run]\ncwd = '{classpath}'", "`run.cwd`", "not a path"),
            ("[run]\ncwd = '{jar}'", "`run.cwd`", "no jar"),
            ("[run]\ncwd = 3", "`run.cwd`", "must be a string"),
        ];
        for (table, key, why) in cases {
            let err = parse(&format!("{JVM_HEAD}{table}\n")).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{table}");
            let msg = err.to_string();
            assert!(msg.contains(key) && msg.contains(why), "{table}: {msg}");
        }
        // `JRS_` stays reserved in a task, with the task's own explanation.
        let err = parse(&format!(
            "{JVM_HEAD}[tasks.t]\nshell = 'x'\nenv = {{ JRS_A = '1' }}\n"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("set for every task"), "{err}");
        // `test.cwd` is not a key: it warns, as every unknown key does.
        let m = parse(&format!("{JVM_HEAD}[test]\ncwd = 'x'\n")).unwrap();
        assert!(
            m.warnings.iter().any(|w| w.contains("test.cwd")),
            "{:?}",
            m.warnings
        );
    }
}
