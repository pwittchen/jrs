//! `jrs.toml`: parsing, validation, defaults.
//!
//! The manifest is parsed by hand out of a `toml::Table` rather than through a
//! `serde` derive. Three things fall out of that which the derive would not give
//! us: declaration order is preserved (conflict mediation breaks ties on it,
//! SPEC §8.2), every diagnostic can name the offending key, and unknown keys can
//! be a warning instead of an error, so manifests stay forward-compatible
//! (SPEC §4.3).
//!
//! The types live here, with what reads them; the work on them is split by
//! what it does: `parse` reads each table, `validate` holds the checks a value
//! must pass beyond its type, `dependencies` adds the implied runtime
//! libraries to the declared ones, `render` writes a manifest back out, and
//! `template` is the `${...}` strings tasks and attributes are written in.

use std::path::{Path, PathBuf};

use crate::compile::lang::Language;
use crate::error::{JrsError, Result};
use crate::resolve::coord::{Coord, Ga};

mod dependencies;
mod parse;
mod render;
mod template;
mod validate;

pub use parse::{jar_attribute, parse_toml, relocation};
pub use render::dependency_entry;
pub use template::{Placeholder, Segment, Template, free_port};
pub use validate::{RELEASES_URL, check_jrs_version, check_project_group, valid_group_pattern};

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

/// A dependency on another member of the workspace, by path
/// (`core = { path = "../core" }`): its classes go where its jar would, and
/// its own dependencies join the graph as a jar's transitive ones do
/// (`specs/WORKSPACES.md` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDependency {
    /// The manifest key: the member's `project.name`.
    pub name: String,
    /// The member's directory, relative to this manifest's and
    /// `/`-separated, as written.
    pub path: String,
    /// Declared in `[dev-dependencies]`: on the test classpath only.
    pub dev: bool,
    pub compile_only: bool,
    pub runtime_only: bool,
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
    /// Agents passed as `-javaagent:`, each taken from the resolved runtime
    /// classpath or pinned apart from it, in the order the manifest names them.
    pub java_agents: Vec<JavaAgent>,
    /// Added to the environment the program inherits, in declaration order.
    pub env: Vec<(String, Template)>,
    /// The program's working directory, relative to the project root. `None`
    /// keeps the directory jrs was started in.
    pub cwd: Option<Template>,
}

/// One `java-agents` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaAgent {
    /// `group:artifact`: the jar the project's own graph resolved, at the
    /// version `jrs.lock` pins for it, so the agent has to be a dependency.
    Graph(Ga),
    /// `group:artifact:version[:classifier]`: a jar resolved apart from the
    /// project's graph, as a tool is, and pinned in `jrs.lock` as a `[[tool]]`
    /// of its own. It is never on a classpath, which suits an agent the
    /// program does not call, such as the OpenTelemetry one.
    Pinned(Coord),
}

impl JavaAgent {
    /// `group:artifact`, without a version.
    #[must_use]
    pub fn ga(&self) -> Ga {
        match self {
            JavaAgent::Graph(ga) => ga.clone(),
            JavaAgent::Pinned(coord) => coord.ga(),
        }
    }

    /// The name of the `[[tool]]` block that pins a [`JavaAgent::Pinned`]
    /// agent of `section`: `run.java-agents.io.opentelemetry.javaagent:opentelemetry-javaagent`,
    /// which neither a compiler's nor a task's name can be.
    #[must_use]
    pub fn tool_name(&self, section: &str) -> String {
        format!("{section}.java-agents.{}", self.ga())
    }
}

impl std::fmt::Display for JavaAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JavaAgent::Graph(ga) => write!(f, "{ga}"),
            JavaAgent::Pinned(coord) => write!(f, "{coord}"),
        }
    }
}

/// `[test]`: how `jrs test` starts the test JVM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestConfig {
    pub jvm_args: Vec<String>,
    /// The `JaCoCo` release `jrs test --coverage` uses, for a JDK newer than
    /// jrs's default knows about.
    pub jacoco_version: Option<String>,
    /// Agents passed as `-javaagent:`, ahead of `JaCoCo`'s, taken from the
    /// resolved test classpath (dev-dependencies included) or pinned apart
    /// from it.
    pub java_agents: Vec<JavaAgent>,
    /// Added to the environment the test JVM inherits, in declaration order.
    pub env: Vec<(String, Template)>,
    /// `test.retries`: how many times a failed test is run again before it
    /// counts as failed. One that passes on a retry is reported as flaky.
    pub retries: u32,
    /// `test.forks`: how many test JVMs a run's classes are split among. `0`
    /// when the key is not set, which leaves it to jrs
    /// ([`crate::test::default_forks`]).
    pub forks: u32,
    /// `test.coverage-minimum`: project-wide totals `jrs test --coverage`
    /// must reach, in declaration order. Ignored without `--coverage`.
    pub coverage_minimum: Vec<CoverageMinimum>,
    /// `test.share-classes`: map the dependency jars' classes into the test
    /// JVM from a class-data-sharing archive, which puts the class
    /// directories on the launcher's own class loader (SPEC §10.2).
    pub share_classes: ShareClasses,
    /// `[test.suites.<name>]`, in declaration order: test sources of their
    /// own, run by `jrs test --suite <name>` and never by plain `jrs test`.
    pub suites: Vec<TestSuite>,
}

/// `test.share-classes`: whether, and how, the test JVM maps the dependency
/// jars' classes from an archive (SPEC §10.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShareClasses {
    /// `false`, the default: the usual layout, no archive.
    #[default]
    Off,
    /// `true`: the AOT cache from JDK 25, where it keeps method profiles
    /// too, falling back to the archive for a classpath it cannot be
    /// assembled from; the dynamic archive on an older JDK.
    On,
    /// `"archive"`: a dynamic class-data-sharing archive, on every JDK jrs
    /// supports.
    Archive,
    /// `"aot"`: the AOT cache of JEP 483 on JDK 24 and later, which also
    /// keeps classes linked and, from JDK 25, method profiles; the dynamic
    /// archive on an older JDK.
    Aot,
}

/// One `[test.suites.<name>]`: Gradle's extra source set with its own `Test`
/// task (`integrationTest`, `e2e`). It compiles against the main classes, the
/// default test classes and the test classpath, as a Gradle source set whose
/// configurations extend `test`'s does, and runs in a JVM set up by its own
/// keys rather than `[test]`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSuite {
    pub name: String,
    /// The suite's sources, relative to the root. Required.
    pub test_dir: PathBuf,
    /// On the suite's classpath only; beside `test-dir` unless set.
    pub test_resource_dir: PathBuf,
    /// For the suite's JVM, instead of `test.jvm-args`.
    pub jvm_args: Vec<String>,
    /// Added to the environment the suite's JVM inherits.
    pub env: Vec<(String, Template)>,
    /// As `test.retries` and `test.forks`, for this suite; `0` when unset.
    pub retries: u32,
    pub forks: u32,
}

impl TestSuite {
    /// How many test JVMs to run: `forks`, or `None` when it is not set.
    #[must_use]
    pub fn forks(&self) -> Option<u32> {
        (self.forks > 0).then_some(self.forks)
    }
}

impl TestConfig {
    /// How many test JVMs to run: `test.forks`, or `None` when it is not
    /// set and jrs picks a number from the suite's size and the machine.
    #[must_use]
    pub fn forks(&self) -> Option<u32> {
        (self.forks > 0).then_some(self.forks)
    }

    /// The suite `[test.suites.<name>]` declares.
    #[must_use]
    pub fn suite(&self, name: &str) -> Option<&TestSuite> {
        self.suites.iter().find(|s| s.name == name)
    }
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
    /// `[package.relocate]`: the packages a fat jar moves, in declaration
    /// order (SPEC §9.9).
    pub relocate: Vec<Relocation>,
}

/// `[resources]`: resource files expanded as they are copied into
/// `target/classes` (SPEC §7.3), the answer to Gradle's
/// `processResources { expand(...) }` and Maven's `<filtering>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourcesConfig {
    /// Paths under `project.resource-dir`, `/`-separated, in which `${name}`
    /// is replaced. `*` matches within one path segment, `**` across them.
    pub expand: Vec<String>,
    /// The names a `${name}` may use besides `project.name` and
    /// `project.version`, in declaration order. Values may use
    /// `{project.name}` and `{project.version}`.
    pub properties: Vec<(String, Template)>,
}

/// One `[package.relocate]` entry: a package, and every package under it,
/// moved to `to` throughout the fat jar, references included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocation {
    pub from: String,
    pub to: String,
    /// Classes (`org.slf4j.Marker`, nested classes included) and packages
    /// (`org.slf4j.impl.*`, subpackages included) under `from` that stay put.
    pub exclude: Vec<String>,
}

/// `[obfuscate]`: the table's presence turns obfuscation on (SPEC §7.7,
/// ROADMAP). It is opt-in and set only from the manifest; `jrs package
/// --obfuscate` runs it, and the plain jar is unaffected. Its `version` pins
/// `ProGuard`, resolved as an isolated tool graph like a compiler's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObfuscateConfig {
    /// The `ProGuard` release, exact: an unpinned obfuscator is not reproducible,
    /// as an unpinned compiler is not.
    pub version: String,
    /// Fully-qualified class names whose names must survive, for reflection or
    /// a framework that looks them up by name. The entry point and every
    /// `ServiceLoader` provider are kept without being listed here.
    pub keep: Vec<String>,
    /// Passed through verbatim to `ProGuard`, after jrs's own configuration.
    pub proguard_args: Vec<String>,
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
    /// Kotlin compiler plugins, by name (`serialization`, `spring`, …),
    /// resolved into the compiler's own graph.
    pub plugins: Vec<String>,
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
    /// `"<member>:<task>"`: another workspace member's task, which runs in
    /// that member before this one's command (`specs/WORKSPACES.md` §6.3).
    Member {
        member: String,
        task: String,
    },
}

impl std::fmt::Display for TaskRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskRef::Task(name) => f.write_str(name),
            TaskRef::Builtin(b) => f.write_str(b.name()),
            TaskRef::Member { member, task } => write!(f, "{member}:{task}"),
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
    /// `cache = true`: the outputs are kept in the build cache under a key
    /// of the task's inputs, and restored instead of a run (SPEC §7.6).
    pub cache: bool,
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
    /// `project.group`: the Maven group `jrs install` writes the project
    /// under, which nothing else needs.
    pub group: Option<String>,
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
    pub resources: ResourcesConfig,
    /// `[obfuscate]`, when the project turns it on; `None` leaves the plain jar
    /// unaffected and pins no obfuscator.
    pub obfuscate: Option<ObfuscateConfig>,
    /// The languages turned on besides Java, in `Language::FOREIGN` order.
    pub languages: Vec<LanguageConfig>,
    pub dependencies: Vec<Dependency>,
    pub dev_dependencies: Vec<Dependency>,
    /// Path dependencies on other workspace members, `[dependencies]` first
    /// and then `[dev-dependencies]`, each in declaration order. Never part
    /// of `dependencies`: a module has no coordinate to resolve.
    pub modules: Vec<ModuleDependency>,
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
    "licenses",
    "audit",
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
    "self",
    "jdk",
    "wrapper",
    "install",
];

impl Manifest {
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

    /// `project.group`, for a command that cannot go on without one.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] naming the key when the project has none.
    pub fn require_group(&self, command: &str) -> Result<&str> {
        self.group.as_deref().ok_or_else(|| {
            JrsError::manifest(format!(
                "`jrs {command}` needs `project.group`, the Maven group to write `{}` under\n\n\
                 add it to {}:\n\n    [project]\n    group = \"com.example\"",
                self.name,
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

    /// `[resources.properties]` with their placeholders expanded, after the
    /// two every expansion knows: `project.name` and `project.version`.
    ///
    /// # Errors
    ///
    /// None in practice: parsing admits only those two placeholders.
    pub fn resource_properties(&self) -> Result<Vec<(String, String)>> {
        let mut out = vec![
            ("project.name".to_string(), self.name.clone()),
            ("project.version".to_string(), self.version.clone()),
        ];
        for (name, value) in &self.resources.properties {
            let expanded = value.expand(|p| match p {
                Placeholder::ProjectName => Ok(self.name.clone()),
                Placeholder::ProjectVersion => Ok(self.version.clone()),
                other => Err(JrsError::manifest(format!(
                    "`resources.properties.{name}`: `{{{}}}` is not available here",
                    other.name()
                ))),
            })?;
            out.push((name.clone(), expanded));
        }
        Ok(out)
    }
}

/// A manifest with nothing but the required fields, used by `init` and `migrate`.
#[must_use]
pub fn blank(name: &str, version: &str, root: &Path) -> Manifest {
    Manifest {
        path: root.join(MANIFEST_FILE),
        root: root.to_path_buf(),
        name: name.to_string(),
        version: version.to_string(),
        group: None,
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
        resources: ResourcesConfig::default(),
        obfuscate: None,
        languages: Vec::new(),
        dependencies: Vec::new(),
        dev_dependencies: Vec::new(),
        modules: Vec::new(),
        managed: Vec::new(),
        repositories: vec![Repository::new(CENTRAL_NAME, CENTRAL_URL)],
        tasks: Vec::new(),
        hooks: Hooks::default(),
        warnings: Vec::new(),
        jrs_version: None,
    }
}

/// Whether a `path` names a workspace member rather than a local jar: a
/// local jar's ends in `.jar`.
#[must_use]
pub fn is_module_path(path: &str) -> bool {
    !path
        .rsplit('/')
        .next()
        .is_some_and(|file| file.to_ascii_lowercase().ends_with(".jar"))
}

#[cfg(test)]
mod tests;
