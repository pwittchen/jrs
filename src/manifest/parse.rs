//! Reading `jrs.toml`: each table out of a `toml::Table`, by hand, in
//! declaration order, every unknown key a warning.

use std::path::{Path, PathBuf};

use super::render::quote;
use super::template::{Placeholder, Template};
use super::validate::{
    check_classifier, check_jrs_version, check_languages, check_one_classpath,
    check_path_templates, check_versionless, refuse_task_placeholders, release_parts,
    valid_group_pattern, validate_attribute_name, validate_class_name, validate_name,
    validate_package_name, validate_task_name,
};
use super::{
    Action, Builtin, CENTRAL_NAME, CENTRAL_URL, CoverageCounter, CoverageMinimum, Dependency,
    Exclusion, Hook, Hooks, JAR_ATTRIBUTES_OWNED, JavaAgent, JavaConfig, LanguageConfig,
    MANIFEST_FILE, Managed, Manifest, ModuleDependency, ObfuscateConfig, PackageConfig,
    RESERVED_TASK_NAMES, Relocation, Repository, ResourcesConfig, RunConfig, ShareClasses, TaskDef,
    TaskRef, TestConfig, TestSuite, check_project_group, is_module_path,
};
use crate::compile::lang::Language;
use crate::error::{IoResultExt, JrsError, Result};
use crate::resolve::coord::{Coord, Ga, is_range};

const OBFUSCATE_KEYS: &[&str] = &["version", "keep", "proguard-args"];

const PROJECT_KEYS: &[&str] = &[
    "name",
    "version",
    "group",
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
    "forks",
    "coverage-minimum",
    "share-classes",
    "suites",
];

const SUITE_KEYS: &[&str] = &[
    "test-dir",
    "test-resource-dir",
    "jvm-args",
    "env",
    "retries",
    "forks",
];

const PACKAGE_KEYS: &[&str] = &["add-modules", "manifest", "native-image-args", "relocate"];

const RESOURCES_KEYS: &[&str] = &["expand", "properties"];

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
    "cache",
];

const TOP_KEYS: &[&str] = &[
    "project",
    "java",
    "run",
    "test",
    "package",
    "resources",
    "obfuscate",
    "kotlin",
    "scala",
    "groovy",
    "dependencies",
    "dev-dependencies",
    "managed",
    "repositories",
    "tasks",
    "hooks",
    "workspace",
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
    pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Manifest> {
        Manifest::from_table(&parse_toml(text, path)?, path, root)
    }

    /// [`Manifest::parse`] for a table already read: a workspace member's,
    /// once the keys it inherits from the root are merged in
    /// (`specs/WORKSPACES.md` §3).
    ///
    /// # Errors
    ///
    /// As for [`Manifest::parse`].
    #[allow(
        clippy::similar_names,
        reason = "`test` is the `[test]` table, named like `run` and `package` beside it"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one short step per top-level table, in the order a reader of jrs.toml \
                  meets them; each table's own parsing is already a helper"
    )]
    pub fn from_table(table: &toml::Table, path: &Path, root: &Path) -> Result<Manifest> {
        let table = table.clone();
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
        let group = optional_string(project, "group", "project")?;
        if let Some(group) = &group {
            check_project_group(group, "project.group")?;
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
                forks: test_forks(t)?,
                coverage_minimum: coverage_minimum(t, &mut warnings)?,
                share_classes: share_classes(t)?,
                suites: parse_suites(t, &mut warnings)?,
            },
        };
        let package = match section(&table, "package", PACKAGE_KEYS, &mut warnings)? {
            None => PackageConfig::default(),
            Some(t) => PackageConfig {
                add_modules: string_array(t, "add-modules", "package")?,
                manifest: parse_jar_attributes(t)?,
                native_image_args: string_array(t, "native-image-args", "package")?,
                relocate: parse_relocations(t, &mut warnings)?,
            },
        };
        let resources = match section(&table, "resources", RESOURCES_KEYS, &mut warnings)? {
            None => ResourcesConfig::default(),
            Some(t) => parse_resources(t)?,
        };
        let obfuscate = match section(&table, "obfuscate", OBFUSCATE_KEYS, &mut warnings)? {
            None => None,
            Some(t) => {
                let version = required_string(t, "version", "obfuscate")?;
                if version.trim().is_empty() {
                    return Err(JrsError::manifest(
                        "`obfuscate.version` must name a ProGuard release".to_string(),
                    ));
                }
                Some(ObfuscateConfig {
                    version,
                    keep: string_array(t, "keep", "obfuscate")?,
                    proguard_args: string_array(t, "proguard-args", "obfuscate")?,
                })
            }
        };

        let languages = parse_languages(&table, &mut warnings)?;
        let (dependencies, mut modules) = parse_dependencies(&table, "dependencies")?;
        let (dev_dependencies, dev_modules) = parse_dependencies(&table, "dev-dependencies")?;
        if let Some(m) = dev_modules
            .iter()
            .find(|m| m.compile_only || m.runtime_only)
        {
            return Err(JrsError::manifest(format!(
                "`dev-dependencies.\"{}\"`: `compile-only` and `runtime-only` only mean \
                 something in [dependencies]",
                m.name
            )));
        }
        for dev in &dev_modules {
            if modules.iter().any(|m| m.name == dev.name) {
                warnings.push(format!(
                    "`{}` is declared in both [dependencies] and [dev-dependencies]; \
                     the main declaration wins",
                    dev.name
                ));
            }
        }
        modules.extend(
            dev_modules
                .into_iter()
                .filter(|dev| !modules.iter().any(|m| m.name == dev.name))
                .map(|mut m| {
                    m.dev = true;
                    m
                })
                .collect::<Vec<_>>(),
        );
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
            group,
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
            resources,
            obfuscate,
            languages,
            dependencies,
            dev_dependencies,
            modules,
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
}

/// `[kotlin]`, `[scala]` and `[groovy]`, each on its own: a version the
/// compiler can be pinned at, and the directories and flags.
fn parse_languages(table: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<LanguageConfig>> {
    let mut out = Vec::new();
    for language in Language::FOREIGN {
        let key = language.key();
        let args_key = language.args_key();
        let mut known = vec![
            "version",
            "source-dir",
            "test-dir",
            args_key.as_str(),
            "compiler-jvm-args",
        ];
        if language == Language::Kotlin {
            known.push("plugins");
        }
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
        let plugins = string_array(t, "plugins", key)?;
        for name in &plugins {
            if crate::compile::lang::kotlin_plugin(name).is_none() {
                let known: Vec<String> = crate::compile::lang::KOTLIN_PLUGINS
                    .iter()
                    .map(|p| format!("`{}`", p.name))
                    .collect();
                return Err(JrsError::manifest(format!(
                    "`{key}.plugins` names `{name}`, which is not a compiler plugin jrs knows \
                     (known: {})",
                    known.join(", ")
                )));
            }
        }
        out.push(LanguageConfig {
            language,
            source_dir: path_or(t, "source-dir", &format!("src/main/{key}"), key)?,
            test_dir: path_or(t, "test-dir", &format!("src/test/{key}"), key)?,
            compiler_args: string_array(t, &args_key, key)?,
            compiler_jvm_args: string_array(t, "compiler-jvm-args", key)?,
            plugins,
            version,
        });
    }
    Ok(out)
}

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
            cache: bool_key(t, "cache", &format!("`{section}.cache`"))?,
            dependencies,
        };
        check_path_templates(&task, &section)?;
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
            None if entry.contains(':') => {
                let (member, task) = entry.split_once(':').unwrap_or_default();
                if validate_name(member).is_err() || validate_task_name(task).is_err() {
                    return Err(JrsError::manifest(format!(
                        "`{section}.depends-on`: `{entry}` is not `<member>:<task>`, another \
                         workspace member's task"
                    )));
                }
                TaskRef::Member {
                    member: member.to_string(),
                    task: task.to_string(),
                }
            }
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
    if template.has_free_port() {
        return Err(JrsError::manifest(format!(
            "`{section}.cwd`: `{{free-port.<name>}}` is a port, not a directory; it belongs \
             in `env`"
        )));
    }
    if let Some(p) = template.placeholders().find(|p| p.is_classpath()) {
        return Err(JrsError::manifest(format!(
            "`{section}.cwd`: `{{{}}}` is a classpath, not a path; it cannot name a directory",
            p.name()
        )));
    }
    Ok(Some(template))
}

/// `run.java-agents` or `test.java-agents`: a `group:artifact` names a
/// dependency, whose version is the one the graph resolved; a
/// `group:artifact:version[:classifier]` names a jar pinned apart from it.
fn parse_java_agents(t: &toml::Table, section: &str) -> Result<Vec<JavaAgent>> {
    let key = format!("{section}.java-agents");
    let mut agents: Vec<JavaAgent> = Vec::new();
    for raw in string_array(t, "java-agents", section)? {
        let well_formed = |s: &str| !s.is_empty() && !s.contains(char::is_whitespace);
        let parts: Vec<&str> = raw.split(':').collect();
        if !(2..=4).contains(&parts.len()) || !parts.iter().all(|p| well_formed(p)) {
            return Err(JrsError::manifest(format!(
                "`{key}`: `{raw}` is not a `group:artifact` coordinate\n\n\
                 name a dependency's agent without a version, as in \
                 `java-agents = [\"org.mockito:mockito-core\"]`, and the version is the one \
                 jrs.lock pins; name one the program does not depend on with its version, \
                 as in `java-agents = [\"io.opentelemetry.javaagent:opentelemetry-javaagent:2.10.0\"]`"
            )));
        }
        let agent = match parts.as_slice() {
            [g, a] => JavaAgent::Graph(Ga::new(*g, *a)),
            [g, a, v, rest @ ..] => {
                if is_range(v) {
                    return Err(JrsError::manifest(format!(
                        "`{key}`: `{raw}` has the range `{v}`; an agent is pinned at an exact \
                         version, like every dependency"
                    )));
                }
                JavaAgent::Pinned(
                    Coord::new(*g, *a, *v).with_classifier(rest.first().map(ToString::to_string)),
                )
            }
            _ => unreachable!("two to four parts"),
        };
        if agents.iter().any(|a| a.ga() == agent.ga()) {
            return Err(JrsError::manifest(format!(
                "`{key}` names `{}` twice",
                agent.ga()
            )));
        }
        agents.push(agent);
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

/// `src/test/resources` for `src/test/java`: the directory beside the tests.
pub(super) fn default_test_resource_dir(test_dir: &Path) -> PathBuf {
    match test_dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join("resources"),
        _ => PathBuf::from("src/test/resources"),
    }
}

fn parse_dependencies(
    table: &toml::Table,
    section: &str,
) -> Result<(Vec<Dependency>, Vec<ModuleDependency>)> {
    parse_dependency_table_with_modules(table.get(section), section)
}

/// A table of dependencies that may not name a workspace member: a task's
/// own. `section` is how messages name it.
fn parse_dependency_table(value: Option<&toml::Value>, section: &str) -> Result<Vec<Dependency>> {
    let (dependencies, modules) = parse_dependency_table_with_modules(value, section)?;
    if let Some(m) = modules.first() {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{}\"` names a directory, `{}`: only [dependencies] and \
             [dev-dependencies] may depend on another workspace member",
            m.name, m.path
        )));
    }
    Ok(dependencies)
}

/// A table of dependencies, with the workspace members it names apart;
/// `section` is how messages name it.
fn parse_dependency_table_with_modules(
    value: Option<&toml::Value>,
    section: &str,
) -> Result<(Vec<Dependency>, Vec<ModuleDependency>)> {
    let Some(value) = value else {
        return Ok((Vec::new(), Vec::new()));
    };
    let deps = value
        .as_table()
        .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;

    let mut out: Vec<Dependency> = Vec::with_capacity(deps.len());
    let mut modules: Vec<ModuleDependency> = Vec::new();
    for (key, value) in deps {
        if let toml::Value::Table(t) = value
            && let Some(toml::Value::String(path)) = t.get("path")
            && !path.trim().is_empty()
            && is_module_path(&path.replace('\\', "/"))
        {
            modules.push(parse_module(key, t, section)?);
            continue;
        }
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
        match value {
            toml::Value::String(v) => dep.version.clone_from(v),
            toml::Value::Table(t) => parse_dependency_keys(&mut dep, t, section, key)?,
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{section}.\"{key}\"` must be a version string or a table \
                     with a `version` key"
                )));
            }
        }
        check_classifier(&dep, section, key)?;
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
    Ok((out, modules))
}

/// `core = { path = "../core" }`: a dependency on the workspace member in
/// that directory, whose `project.name` the key is.
fn parse_module(key: &str, t: &toml::Table, section: &str) -> Result<ModuleDependency> {
    let name = |k: &str| format!("`{section}.\"{key}\".{k}`");
    if validate_name(key).is_err() {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{key}\"` has a `path` to a directory, so it is a workspace member, \
             and its key is that member's `project.name`"
        )));
    }
    for k in t.keys() {
        if !LOCAL_DEPENDENCY_KEYS.contains(&k.as_str()) {
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"`: `{k}` does not go with a member's `path` — a member is \
                 built from its sources, at the version its own manifest gives (expected \
                 `path`, `compile-only` or `runtime-only`)"
            )));
        }
    }
    let raw = optional_string(t, "path", &format!("{section}.\"{key}\""))?.unwrap_or_default();
    let path = raw.replace('\\', "/");
    let trimmed = path.trim_end_matches('/').to_string();
    let as_path = Path::new(&trimmed);
    if trimmed.is_empty() || as_path.is_absolute() || as_path.has_root() || trimmed.starts_with('/')
    {
        return Err(JrsError::manifest(format!(
            "{} must name a member's directory relative to this manifest's, as \
             `../core` (got `{raw}`)",
            name("path")
        )));
    }
    let module = ModuleDependency {
        name: key.to_string(),
        path: trimmed,
        dev: false,
        compile_only: bool_key(t, "compile-only", &name("compile-only"))?,
        runtime_only: bool_key(t, "runtime-only", &name("runtime-only"))?,
    };
    if module.compile_only && module.runtime_only {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{key}\"` cannot be both `compile-only` and `runtime-only`"
        )));
    }
    Ok(module)
}

/// The keys of a dependency written as a table: its version, classifier,
/// exclusions and classpath.
fn parse_dependency_keys(
    dep: &mut Dependency,
    t: &toml::Table,
    section: &str,
    key: &str,
) -> Result<()> {
    let name = |k: &str| format!("`{section}.\"{key}\".{k}`");
    for k in t.keys() {
        if !DEPENDENCY_KEYS.contains(&k.as_str()) {
            return Err(JrsError::manifest(format!(
                "`{section}.\"{key}\"`: unknown key `{k}` (expected `version`, \
                 `classifier`, `exclusions`, `compile-only`, `runtime-only` or `path`)"
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
    if let Some(c) = optional_string(t, "classifier", &format!("{section}.\"{key}\""))? {
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
    check_one_classpath(dep, section)?;
    Ok(())
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
pub(super) fn section<'a>(
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

/// `[resources]`: which resource files are expanded, and the properties they
/// may name. A pattern is relative to `project.resource-dir`; one that
/// matches nothing is harmless, so a file can be added later.
fn parse_resources(t: &toml::Table) -> Result<ResourcesConfig> {
    let expand = string_array(t, "expand", "resources")?;
    for pattern in &expand {
        if pattern.is_empty()
            || pattern.starts_with('/')
            || pattern.contains('\\')
            || pattern
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(JrsError::manifest(format!(
                "`resources.expand`: `{pattern}` must be a relative, `/`-separated path \
                 under `project.resource-dir`, such as `application.yml` or `config/*.properties`"
            )));
        }
    }
    let mut properties: Vec<(String, Template)> = Vec::new();
    if let Some(value) = t.get("properties") {
        let table = value.as_table().ok_or_else(|| {
            JrsError::manifest(
                "`resources.properties` must be a table of names to strings, e.g.\n\n    \
                 [resources]\n    properties = { version = \"{project.version}\" }",
            )
        })?;
        for (name, value) in table {
            let key = format!("resources.properties.{name}");
            if !crate::expand::valid_property_name(name) {
                return Err(JrsError::manifest(format!(
                    "`{key}`: a property name is letters, digits, `.`, `-` and `_`"
                )));
            }
            if name == "project.name" || name == "project.version" {
                return Err(JrsError::manifest(format!(
                    "`{key}`: `{name}` is always set, from `[project]`"
                )));
            }
            let raw = value
                .as_str()
                .ok_or_else(|| JrsError::manifest(format!("`{key}` must be a string")))?;
            let template =
                Template::parse(raw).map_err(|e| JrsError::manifest(format!("`{key}`: {e}")))?;
            if template.has_free_port() {
                return Err(JrsError::manifest(format!(
                    "`{key}`: `{{free-port.<name>}}` is not known when resources are copied"
                )));
            }
            if let Some(p) = template
                .placeholders()
                .find(|p| !matches!(p, Placeholder::ProjectName | Placeholder::ProjectVersion))
            {
                return Err(JrsError::manifest(format!(
                    "`{key}`: `{{{}}}` is not known when resources are copied; a property \
                     can use `{{project.name}}` and `{{project.version}}`",
                    p.name()
                )));
            }
            properties.push((name.clone(), template));
        }
    }
    if expand.is_empty() && !properties.is_empty() {
        return Err(JrsError::manifest(
            "`resources.properties` is set, but `resources.expand` names no file to use them in",
        ));
    }
    Ok(ResourcesConfig { expand, properties })
}

/// `[package.relocate]`: package names to the names they move to, in
/// declaration order (SPEC §9.9). A value is the new name, or a table with
/// `to` and `exclude` — the classes (`org.slf4j.Marker`) and packages
/// (`org.slf4j.impl.*`) under the package that stay where they are.
fn parse_relocations(t: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<Relocation>> {
    let Some(value) = t.get("relocate") else {
        return Ok(Vec::new());
    };
    let table = value.as_table().ok_or_else(|| {
        JrsError::manifest(
            "`package.relocate` must be a table of packages to the names they move to, e.g.\n\n    \
             [package.relocate]\n    \"com.google.common\" = \"com.example.shaded.guava\"",
        )
    })?;
    let mut out = Vec::new();
    for (from, value) in table {
        let key = format!("package.relocate.\"{from}\"");
        let (to, exclude) = match value {
            toml::Value::String(to) => (to.clone(), Vec::new()),
            toml::Value::Table(long) => {
                warn_unknown(long, &["to", "exclude"], &format!("{key}."), warnings);
                let to = long
                    .get("to")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| {
                        JrsError::manifest(format!("`{key}` needs `to`, the package to move it to"))
                    })?;
                (to.to_string(), string_array(long, "exclude", &key)?)
            }
            _ => {
                return Err(JrsError::manifest(format!(
                    "`{key}` must be the package to move it to, or a table with `to` and \
                     `exclude`"
                )));
            }
        };
        out.push(relocation(from, &to, exclude)?);
    }
    Ok(out)
}

/// One `[package.relocate]` entry, checked as the parser checks it: `from`
/// and `to` are package names outside `java.*` and not the same, and each
/// exclusion is a class or `<package>.*` under `from`. `jrs migrate` uses it
/// to decide what a shading plugin's relocations can become.
///
/// # Errors
///
/// [`JrsError::Manifest`], naming `package.relocate."<from>"`, for any of those.
pub fn relocation(from: &str, to: &str, exclude: Vec<String>) -> Result<Relocation> {
    let key = format!("package.relocate.\"{from}\"");
    validate_package_name(from, &key)?;
    validate_package_name(to, &key)?;
    for name in [from, to] {
        if name == "java" || name.starts_with("java.") {
            return Err(JrsError::manifest(format!(
                "`{key}`: `java.*` belongs to the JDK; its classes are never in the jar, \
                 and no class can be defined there"
            )));
        }
    }
    if to == from {
        return Err(JrsError::manifest(format!(
            "`{key}` relocates the package to itself"
        )));
    }
    for excluded in &exclude {
        validate_package_name(excluded.strip_suffix(".*").unwrap_or(excluded), &key)?;
        if !excluded.starts_with(&format!("{from}.")) {
            return Err(JrsError::manifest(format!(
                "`{key}.exclude`: `{excluded}` is not in `{from}`, so there is nothing to \
                 exclude it from; name a class, or a package as `{from}.<name>.*`"
            )));
        }
    }
    Ok(Relocation {
        from: from.to_string(),
        to: to.to_string(),
        exclude,
    })
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
    if template.has_free_port() {
        return Err(JrsError::manifest(format!(
            "`{key}`: `{{free-port.<name>}}` is not known when a jar is written"
        )));
    }
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

/// `test.share-classes`: `true`, `false`, `"archive"` or `"aot"`.
pub(super) fn share_classes(t: &toml::Table) -> Result<ShareClasses> {
    match t.get("share-classes") {
        None | Some(toml::Value::Boolean(false)) => Ok(ShareClasses::Off),
        Some(toml::Value::Boolean(true)) => Ok(ShareClasses::On),
        Some(toml::Value::String(s)) if s == "archive" => Ok(ShareClasses::Archive),
        Some(toml::Value::String(s)) if s == "aot" => Ok(ShareClasses::Aot),
        Some(_) => Err(JrsError::manifest(
            "`test.share-classes` must be `true`, `false`, `\"archive\"` or `\"aot\"`".to_string(),
        )),
    }
}

/// `test.forks`: a whole number of test JVMs, 1 or more; `0` when absent.
fn test_forks(t: &toml::Table) -> Result<u32> {
    let invalid = || {
        JrsError::manifest("`test.forks` must be a whole number of test JVMs, 1 or more, e.g. `4`")
    };
    match t.get("forks") {
        None => Ok(0),
        Some(toml::Value::Integer(n)) => u32::try_from(*n)
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(invalid),
        Some(_) => Err(invalid()),
    }
}

/// `[test.suites.<name>]`, in declaration order. A suite's name is a task
/// name's shape, since it is typed on the command line.
fn parse_suites(t: &toml::Table, warnings: &mut Vec<String>) -> Result<Vec<TestSuite>> {
    let Some(value) = t.get("suites") else {
        return Ok(Vec::new());
    };
    let table = value.as_table().ok_or_else(|| {
        JrsError::manifest(
            "`test.suites` must hold one table per suite, e.g.\n\n    \
             [test.suites.e2e]\n    test-dir = \"src/e2e/java\"",
        )
    })?;
    let mut out = Vec::new();
    for (name, value) in table {
        let section = format!("test.suites.{name}");
        let well_formed = name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !well_formed {
            return Err(JrsError::manifest(format!(
                "`{section}`: a suite name is lowercase letters, digits and `-`, starting \
                 with a letter"
            )));
        }
        let t = value
            .as_table()
            .ok_or_else(|| JrsError::manifest(format!("`{section}` must be a table")))?;
        warn_unknown(t, SUITE_KEYS, &format!("{section}."), warnings);
        if t.get("test-dir").is_none() {
            return Err(JrsError::manifest(format!(
                "`{section}.test-dir` is required: the directory holding the suite's sources"
            )));
        }
        let test_dir = path_or(t, "test-dir", "", &section)?;
        let test_resource_dir = match t.get("test-resource-dir") {
            Some(_) => path_or(t, "test-resource-dir", "", &section)?,
            None => default_test_resource_dir(&test_dir),
        };
        let forks = match t.get("forks") {
            None => 0,
            Some(v) => v
                .as_integer()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n >= 1)
                .ok_or_else(|| {
                    JrsError::manifest(format!(
                        "`{section}.forks` must be a whole number of test JVMs, 1 or more"
                    ))
                })?,
        };
        out.push(TestSuite {
            name: name.clone(),
            test_dir,
            test_resource_dir,
            jvm_args: string_array(t, "jvm-args", &section)?,
            env: parse_jvm_env(t, &section)?,
            retries: optional_count(t, "retries", &section)?,
            forks,
        });
    }
    Ok(out)
}

/// `test.coverage-minimum = { line = 0.80, branch = 0.70 }`: a ratio from 0 to
/// 1 per `JaCoCo` counter. An unknown counter is a warning, like any unknown
/// key; a value that is not a ratio is an error naming its key.
pub(super) fn coverage_minimum(
    t: &toml::Table,
    warnings: &mut Vec<String>,
) -> Result<Vec<CoverageMinimum>> {
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

/// `text` as a TOML table, or the parser's message with the file, line and
/// column it points at.
///
/// # Errors
///
/// [`JrsError::Manifest`] when `text` is not TOML.
pub fn parse_toml(text: &str, path: &Path) -> Result<toml::Table> {
    toml::from_str(text).map_err(|e| {
        let where_ = e.span().map_or_else(
            || format!("{}: ", path.display()),
            |s| {
                let (line, col) = locate(text, s.start);
                format!("{}:{line}:{col}: ", path.display())
            },
        );
        JrsError::manifest(format!("{where_}{}", e.message()))
    })
}

/// Byte offset to 1-based line and column.
fn locate(text: &str, offset: usize) -> (usize, usize) {
    let head = &text[..offset.min(text.len())];
    let line = head.matches('\n').count() + 1;
    let col = head.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}
