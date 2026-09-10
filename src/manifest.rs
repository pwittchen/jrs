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
use crate::resolve::coord::is_range;

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
        }
    }

    /// `group:artifact`, or `group:artifact:classifier` — unique within a table.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.classifier {
            Some(c) => format!("{}:{}:{c}", self.group, self.artifact),
            None => format!("{}:{}", self.group, self.artifact),
        }
    }

    /// True when the short `"g:a" = "version"` form says everything.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.exclusions.is_empty() && !self.compile_only
    }
}

impl std::fmt::Display for Dependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        Ok(())
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
}

/// `[test]`: how `jrs test` starts the test JVM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestConfig {
    pub jvm_args: Vec<String>,
    /// The `JaCoCo` release `jrs test --coverage` uses, for a JDK newer than
    /// jrs's default knows about.
    pub jacoco_version: Option<String>,
}

/// `[package]`: runtime images.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageConfig {
    /// Modules added to a `--jlink` / `--jpackage` runtime beyond those `jdeps`
    /// finds — ones reached only by reflection or `ServiceLoader`, such as
    /// `jdk.crypto.ec` for TLS.
    pub add_modules: Vec<String>,
}

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
}

impl TaskDef {
    /// Every template the task holds, with the key it came from.
    pub fn templates(&self) -> impl Iterator<Item = (&'static str, &Template)> + '_ {
        let action: Vec<(&'static str, &Template)> = match &self.action {
            Some(Action::Run(argv)) => argv.iter().map(|t| ("run", t)).collect(),
            Some(Action::Script(t)) => vec![("script", t)],
            Some(Action::Shell(_)) | None => Vec::new(),
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
    /// User repositories in declaration order, with Central appended last.
    pub repositories: Vec<Repository>,
    /// `[tasks]`, in declaration order.
    pub tasks: Vec<TaskDef>,
    pub hooks: Hooks,

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
    "test-resource-dir",
    "target-dir",
];
const JAVA_KEYS: &[&str] = &[
    "source",
    "target",
    "encoding",
    "javac-args",
    "javadoc-args",
    "jdk",
];
const RUN_KEYS: &[&str] = &["jvm-args"];
const TEST_KEYS: &[&str] = &["jvm-args", "jacoco-version"];
const PACKAGE_KEYS: &[&str] = &["add-modules"];
const DEPENDENCY_KEYS: &[&str] = &["version", "classifier", "exclusions", "compile-only"];
const TASK_KEYS: &[&str] = &[
    "description",
    "run",
    "shell",
    "script",
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
            },
        };
        let test = match section(&table, "test", TEST_KEYS, &mut warnings)? {
            None => TestConfig::default(),
            Some(t) => TestConfig {
                jvm_args: string_array(t, "jvm-args", "test")?,
                jacoco_version: optional_string(t, "jacoco-version", "test")?,
            },
        };
        let package = match section(&table, "package", PACKAGE_KEYS, &mut warnings)? {
            None => PackageConfig::default(),
            Some(t) => PackageConfig {
                add_modules: string_array(t, "add-modules", "package")?,
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
            repositories,
            tasks,
            hooks,
            warnings,
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
        if !self.run.jvm_args.is_empty() {
            let _ = writeln!(s, "\n[run]");
            let _ = writeln!(s, "jvm-args = {}", quote_list(&self.run.jvm_args));
        }
        if self.test != TestConfig::default() {
            let _ = writeln!(s, "\n[test]");
            if !self.test.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&self.test.jvm_args));
            }
            if let Some(v) = &self.test.jacoco_version {
                let _ = writeln!(s, "jacoco-version = {}", quote(v));
            }
        }
        if !self.package.add_modules.is_empty() {
            let _ = writeln!(s, "\n[package]");
            let _ = writeln!(s, "add-modules = {}", quote_list(&self.package.add_modules));
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
            .filter(|r| r.url.trim_end_matches('/') != CENTRAL_URL)
            .collect();
        if !extra.is_empty() {
            let _ = writeln!(s, "\n[repositories]");
            for r in extra {
                let _ = writeln!(s, "{} = {}", quote(&r.name), quote(&r.url));
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
        let vars: Vec<String> = task
            .env
            .iter()
            .map(|(k, v)| format!("{} = {}", quote(k), quote(&v.raw)))
            .collect();
        let _ = writeln!(s, "env = {{ {} }}", vars.join(", "));
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
        repositories: vec![Repository {
            name: CENTRAL_NAME.into(),
            url: CENTRAL_URL.into(),
        }],
        tasks: Vec::new(),
        hooks: Hooks::default(),
        warnings: Vec::new(),
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
        if actions.len() > 1 {
            let keys: Vec<String> = actions.iter().map(|(k, _)| format!("`{k}`")).collect();
            return Err(JrsError::manifest(format!(
                "`{section}` has {}; a task runs exactly one of `run`, `shell` or `script`",
                keys.join(" and ")
            )));
        }
        let action = actions.pop().map(|(_, a)| a);
        let depends_on = parse_depends_on(t, &section)?;
        if action.is_none() && depends_on.is_empty() {
            return Err(JrsError::manifest(format!(
                "`{section}` does nothing: give it one of `run`, `shell` or `script`, \
                 or a `depends-on` list to run"
            )));
        }
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
                "`{section}.env.{key}`: names starting with `JRS_` are jrs's own, \
                 and are set for every task"
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
    let Some(value) = table.get(section) else {
        return Ok(Vec::new());
    };
    let deps = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;

    let mut out: Vec<Dependency> = Vec::with_capacity(deps.len());
    for (key, value) in deps {
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
                             `classifier`, `exclusions` or `compile-only`)"
                        )));
                    }
                }
                dep.version = t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JrsError::manifest(format!(
                            "`{section}.\"{key}\"` is missing a `version` string"
                        ))
                    })?
                    .to_string();
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
                dep.compile_only = match t.get("compile-only") {
                    None => false,
                    Some(toml::Value::Boolean(b)) => *b,
                    Some(_) => {
                        return Err(JrsError::manifest(format!(
                            "{} must be `true` or `false`",
                            name("compile-only")
                        )));
                    }
                };
            }
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{section}.\"{key}\"` must be a version string or a table \
                     with a `version` key"
                )));
            }
        }
        if dep.version.trim().is_empty() {
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"` has an empty version"
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
    if d.is_plain() && table_classifier.is_none() {
        return (key, quote(&d.version));
    }
    let mut fields = vec![format!("version = {}", quote(&d.version))];
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
}
