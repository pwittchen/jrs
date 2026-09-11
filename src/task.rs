//! User-defined tasks and lifecycle hooks (TASKS.md).
//!
//! Everything about tasks that neither starts a process nor touches a
//! terminal: the checks that need the whole manifest at once (references,
//! cycles, where a placeholder is available, where generated code may go), the
//! order tasks run in, placeholder expansion, the environment a task sees, and
//! the fingerprint that lets one be skipped. Like `resolve/`, it knows nothing
//! about `Ui`, so all of it is tested without a TTY; `cli.rs` decides when a
//! task runs and what is printed around it.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::compile::render_argfile;
use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::{Action, Builtin, Hook, Manifest, Placeholder, TaskDef, TaskRef, Template};
use crate::project;
use crate::toolchain::{Launch, Toolchain, find_program};

/// 1980-01-01, the timestamp jrs's own jar entries carry, as the
/// reproducible-builds `SOURCE_DATE_EPOCH` every task gets.
pub const SOURCE_DATE_EPOCH: &str = "315532800";

/// The environment variables a `shell` string can read the classpath from; a
/// string that mentions one needs the graph resolved, as a placeholder would.
const CLASSPATH_VARIABLES: &[&str] = &["JRS_CLASSPATH", "JRS_RUNTIME_CLASSPATH"];

// ---- whole-manifest checks -------------------------------------------------

/// Check what no single `[tasks.*]` table can decide alone. Called by
/// [`Manifest::parse`], so every problem here is found before anything runs.
/// Returns warnings.
///
/// # Errors
///
/// [`JrsError::Manifest`] when a `depends-on` or hook entry names no task, the
/// tasks form a cycle (through a built-in command's hooks too), a hook runs a
/// task that uses `{jar}` before there is one, or `source-outputs` /
/// `resource-outputs` point outside `project.target-dir`.
pub fn check(manifest: &Manifest) -> Result<Vec<String>> {
    for task in &manifest.tasks {
        for dep in &task.depends_on {
            if let TaskRef::Task(name) = dep
                && manifest.task(name).is_none()
            {
                return Err(JrsError::manifest(format!(
                    "`tasks.{}.depends-on` names `{name}`, which is not a task in [tasks]",
                    task.name
                )));
            }
        }
    }
    for (hook, names) in manifest.hooks.iter() {
        for name in names {
            if manifest.task(name).is_some() {
                continue;
            }
            let why = if crate::manifest::RESERVED_TASK_NAMES.contains(&name.as_str()) {
                "which is a jrs command; a hook runs tasks".to_string()
            } else {
                "which is not a task in [tasks]".to_string()
            };
            return Err(JrsError::manifest(format!(
                "`hooks.{hook}` names `{name}`, {why}"
            )));
        }
    }
    if let Some(cycle) = find_cycle(manifest) {
        return Err(JrsError::manifest(describe_cycle(&cycle)));
    }

    for hook in Hook::ALL.into_iter().filter(|h| *h != Hook::PostPackage) {
        for task in reached_from(manifest, hook) {
            if task.uses(Placeholder::Jar) && !depends_on(manifest, task, Builtin::Package) {
                return Err(JrsError::manifest(format!(
                    "`tasks.{}` uses `{{jar}}`, but the `{hook}` hook runs it before there \
                     is a jar\n\nonly the `post-package` hook, or a `depends-on` that \
                     includes `package`, comes after one",
                    task.name
                )));
            }
        }
    }

    let target = static_target(manifest);
    for task in &manifest.tasks {
        let generated = task
            .source_outputs
            .iter()
            .map(|t| ("source-outputs", t))
            .chain(
                task.resource_outputs
                    .iter()
                    .map(|t| ("resource-outputs", t)),
            );
        for (key, template) in generated {
            let path = static_path(manifest, template)?;
            let escapes = path.components().any(|c| c == Component::ParentDir);
            if escapes || path == target || !path.starts_with(&target) {
                return Err(JrsError::manifest(format!(
                    "`tasks.{}.{key}`: `{}` is not inside `project.target-dir` ({})\n\n\
                     generated code lives where `jrs clean` can remove it, and where \
                     `--watch` does not look",
                    task.name,
                    template.raw,
                    target.display()
                )));
            }
        }
    }

    let mut warnings = Vec::new();
    let generators: HashSet<&str> = reached_from(manifest, Hook::PreCompile)
        .into_iter()
        .chain(reached_from(manifest, Hook::PreTest))
        .map(|t| t.name.as_str())
        .collect();
    for task in &manifest.tasks {
        if generators.contains(task.name.as_str()) {
            continue;
        }
        for (key, list) in [
            ("source-outputs", &task.source_outputs),
            ("resource-outputs", &task.resource_outputs),
        ] {
            if !list.is_empty() {
                warnings.push(format!(
                    "`tasks.{}.{key}` is ignored: only a task the `pre-compile` or \
                     `pre-test` hook runs feeds a compilation",
                    task.name
                ));
            }
        }
    }
    Ok(warnings)
}

/// One step of a cycle: a node, and the hook that led to it from the step
/// before, when the step before is a built-in.
type Step = (TaskRef, Option<Hook>);

/// The graph jrs checks for cycles: a task points at what it depends on, and a
/// built-in command at every task its hooks run.
fn edges(manifest: &Manifest, node: &TaskRef) -> Vec<Step> {
    match node {
        TaskRef::Task(name) => manifest
            .task(name)
            .map(|t| t.depends_on.iter().map(|d| (d.clone(), None)).collect())
            .unwrap_or_default(),
        TaskRef::Builtin(b) => b
            .hooks()
            .iter()
            .flat_map(|h| {
                manifest
                    .hooks
                    .tasks(*h)
                    .iter()
                    .map(move |n| (TaskRef::Task(n.clone()), Some(*h)))
            })
            .collect(),
    }
}

/// The first cycle a depth-first walk in declaration order finds, starting and
/// ending on the same node.
fn find_cycle(manifest: &Manifest) -> Option<Vec<Step>> {
    #[derive(PartialEq)]
    enum State {
        Open,
        Done,
    }

    fn walk(
        manifest: &Manifest,
        step: Step,
        states: &mut HashMap<TaskRef, State>,
        path: &mut Vec<Step>,
    ) -> Option<Vec<Step>> {
        match states.get(&step.0) {
            Some(State::Done) => return None,
            Some(State::Open) => {
                let start = path.iter().position(|(n, _)| *n == step.0)?;
                let mut cycle = path[start..].to_vec();
                cycle.push(step);
                return Some(cycle);
            }
            None => {}
        }
        states.insert(step.0.clone(), State::Open);
        path.push(step.clone());
        for next in edges(manifest, &step.0) {
            if let Some(cycle) = walk(manifest, next, states, path) {
                return Some(cycle);
            }
        }
        path.pop();
        states.insert(step.0, State::Done);
        None
    }

    let mut states = HashMap::new();
    manifest
        .tasks
        .iter()
        .map(|t| TaskRef::Task(t.name.clone()))
        .find_map(|start| walk(manifest, (start, None), &mut states, &mut Vec::new()))
}

fn describe_cycle(cycle: &[Step]) -> String {
    let path: Vec<String> = cycle.iter().map(|(n, _)| n.to_string()).collect();
    let mut message = format!(
        "the tasks depend on each other in a cycle: {}",
        path.join(" → ")
    );
    for pair in cycle.windows(2) {
        if let ((TaskRef::Builtin(b), _), (task, Some(hook))) = (&pair[0], &pair[1]) {
            let _ = write!(
                message,
                "\n\n`{}` runs the `{hook}` hook, which runs `{task}`: a task a hook runs \
                 cannot depend on a command that fires that hook",
                b.name()
            );
        }
    }
    message
}

// ---- ordering --------------------------------------------------------------

/// Everything `roots` needs, in the order it runs: dependencies before the
/// tasks that need them, and otherwise in the order `roots` and each
/// `depends-on` list name them. Each task and built-in appears once, however
/// many paths reach it. Built-ins are leaves here: the tasks their own hooks
/// run are the business of the command when it runs.
#[must_use]
pub fn plan(manifest: &Manifest, roots: &[TaskRef]) -> Vec<TaskRef> {
    fn visit(
        manifest: &Manifest,
        node: &TaskRef,
        seen: &mut HashSet<TaskRef>,
        out: &mut Vec<TaskRef>,
    ) {
        if !seen.insert(node.clone()) {
            return;
        }
        if let TaskRef::Task(name) = node
            && let Some(task) = manifest.task(name)
        {
            for dep in &task.depends_on {
                visit(manifest, dep, seen, out);
            }
        }
        out.push(node.clone());
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        visit(manifest, root, &mut seen, &mut out);
    }
    out
}

/// The tasks a hook runs, directly or through `depends-on`, in order.
#[must_use]
pub fn reached_from(manifest: &Manifest, hook: Hook) -> Vec<&TaskDef> {
    let roots: Vec<TaskRef> = manifest
        .hooks
        .tasks(hook)
        .iter()
        .map(|n| TaskRef::Task(n.clone()))
        .collect();
    plan(manifest, &roots)
        .iter()
        .filter_map(|step| match step {
            TaskRef::Task(name) => manifest.task(name),
            TaskRef::Builtin(_) => None,
        })
        .collect()
}

fn depends_on(manifest: &Manifest, task: &TaskDef, builtin: Builtin) -> bool {
    plan(manifest, &[TaskRef::Task(task.name.clone())]).contains(&TaskRef::Builtin(builtin))
}

// ---- generated code and watched inputs -------------------------------------

/// The directories a compile unit takes generated code from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Generated {
    pub sources: Vec<PathBuf>,
    pub resources: Vec<PathBuf>,
}

/// What the tasks `hook` runs generate: `pre-compile` feeds the main unit and
/// `pre-test` the test unit. A task both reach feeds the main one, where it
/// runs.
///
/// # Errors
///
/// None in practice: the paths were expanded once already, when the manifest
/// was checked.
pub fn generated(manifest: &Manifest, hook: Hook) -> Result<Generated> {
    let main: HashSet<&str> = if hook == Hook::PreCompile {
        HashSet::new()
    } else {
        reached_from(manifest, Hook::PreCompile)
            .into_iter()
            .map(|t| t.name.as_str())
            .collect()
    };
    let mut out = Generated::default();
    for task in reached_from(manifest, hook) {
        if main.contains(task.name.as_str()) {
            continue;
        }
        for t in &task.source_outputs {
            out.sources.push(static_path(manifest, t)?);
        }
        for t in &task.resource_outputs {
            out.resources.push(static_path(manifest, t)?);
        }
    }
    Ok(out)
}

/// Every task's `inputs`, for `--watch`. Nothing under `target-dir`: that is
/// where builds write, and watching it would loop.
#[must_use]
pub fn watched_inputs(manifest: &Manifest) -> Vec<PathBuf> {
    let target = static_target(manifest);
    manifest
        .tasks
        .iter()
        .flat_map(|t| &t.inputs)
        .filter_map(|t| static_path(manifest, t).ok())
        .filter(|p| !p.starts_with(&target))
        .collect()
}

/// `jrs task --list`: name, description, and in parentheses the hooks that run
/// it and `sh` for a `shell` task, which is not portable.
#[must_use]
pub fn list(manifest: &Manifest) -> Vec<String> {
    let rows: Vec<[String; 3]> = manifest
        .tasks
        .iter()
        .map(|task| {
            let mut notes: Vec<&str> = manifest
                .hooks
                .iter()
                .filter(|(_, names)| names.contains(&task.name))
                .map(|(hook, _)| hook.name())
                .collect();
            if matches!(task.action, Some(Action::Shell(_))) {
                notes.push("sh");
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("({})", notes.join(", "))
            };
            [
                task.name.clone(),
                task.description.clone().unwrap_or_default(),
                notes,
            ]
        })
        .collect();
    let width = |i: usize| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
    let (name_width, description_width) = (width(0), width(1));
    rows.iter()
        .map(|[name, description, notes]| {
            let mut line = format!("{name:<name_width$}");
            if description_width > 0 {
                let _ = write!(line, "   {description:<description_width$}");
            }
            if !notes.is_empty() {
                let _ = write!(line, "   {notes}");
            }
            line.trim_end().to_string()
        })
        .collect()
}

// ---- expansion -------------------------------------------------------------

/// The project root, absolute, as `{root}` gives it.
fn static_root(manifest: &Manifest) -> PathBuf {
    std::path::absolute(&manifest.root).unwrap_or_else(|_| manifest.root.clone())
}

fn static_target(manifest: &Manifest) -> PathBuf {
    static_root(manifest).join(&manifest.target_dir)
}

/// The value of a placeholder that depends on nothing but the manifest.
fn static_value(manifest: &Manifest, placeholder: Placeholder) -> Result<String> {
    let target = static_target(manifest);
    let path = match placeholder {
        Placeholder::Root => static_root(manifest),
        Placeholder::Target => target,
        Placeholder::ProjectName => return Ok(manifest.name.clone()),
        Placeholder::ProjectVersion => return Ok(manifest.version.clone()),
        Placeholder::Classes => target.join("classes"),
        Placeholder::TestClasses => target.join("test-classes"),
        Placeholder::Jar => target.join(manifest.jar_name()),
        Placeholder::Classpath
        | Placeholder::RuntimeClasspath
        | Placeholder::TestClasspath
        | Placeholder::ClasspathArgfile => {
            return Err(JrsError::build(format!(
                "`{{{}}}` has no value until dependencies are resolved",
                placeholder.name()
            )));
        }
    };
    Ok(path.display().to_string())
}

/// A path-valued template, expanded, relative to the root when it is relative.
fn static_path(manifest: &Manifest, template: &Template) -> Result<PathBuf> {
    let expanded = template.expand(|p| static_value(manifest, p))?;
    Ok(static_root(manifest).join(expanded))
}

/// Whether a task needs the dependency graph resolved before it runs.
#[must_use]
pub fn needs_classpath(task: &TaskDef) -> bool {
    task.templates()
        .any(|(_, t)| t.placeholders().any(Placeholder::is_classpath))
        || matches!(&task.action, Some(Action::Shell(script))
            if CLASSPATH_VARIABLES.iter().any(|v| script.contains(v)))
}

/// The resolved classpaths, each exactly as `jrs classpath` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classpaths {
    /// `target/classes`, then the compile jars.
    pub compile: Vec<PathBuf>,
    /// `target/classes`, then the runtime jars.
    pub runtime: Vec<PathBuf>,
    /// `target/test-classes`, `target/classes`, then the test jars.
    pub test: Vec<PathBuf>,
}

/// What a task can see at the point it runs.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub manifest: &'a Manifest,
    pub toolchain: &'a Toolchain,
    /// The hook that triggered the task, if one did.
    pub hook: Option<Hook>,
    /// Present once dependencies have been resolved.
    pub classpaths: Option<&'a Classpaths>,
    /// Present once `package` has written the jar.
    pub jar: Option<&'a Path>,
    /// The jars of the task's own `dependencies`, once fetched (TASKS.md §8).
    pub tool_classpath: Option<&'a [PathBuf]>,
    pub offline: bool,
    /// The `PATH` jrs inherited, which the JDK's `bin` goes in front of.
    pub path: Option<&'a std::ffi::OsStr>,
}

/// A task, expanded and ready to start.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub name: String,
    pub launch: Launch,
    pub cwd: PathBuf,
    /// Additions to the inherited environment, in the order they apply.
    pub env: Vec<(String, OsString)>,
    pub outputs: Vec<PathBuf>,
    /// Only for a task that declares both `inputs` and `outputs`; without
    /// both, a task always runs.
    pub fingerprint: Option<String>,
    fingerprint_path: PathBuf,
}

impl Prepared {
    /// Whether the last successful run had the same fingerprint and left every
    /// output in place.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        let Some(fingerprint) = &self.fingerprint else {
            return false;
        };
        std::fs::read_to_string(&self.fingerprint_path).is_ok_and(|p| p == *fingerprint)
            && self.outputs.iter().all(|o| o.exists())
    }

    /// Remember a successful run.
    ///
    /// # Errors
    ///
    /// [`JrsError::Io`] if the fingerprint cannot be written.
    pub fn record(&self) -> Result<()> {
        let Some(fingerprint) = &self.fingerprint else {
            self.forget();
            return Ok(());
        };
        if let Some(dir) = self.fingerprint_path.parent() {
            std::fs::create_dir_all(dir).path(dir)?;
        }
        std::fs::write(&self.fingerprint_path, fingerprint).path(&self.fingerprint_path)
    }

    /// Forget any earlier run, so a failure is never taken for fresh.
    pub fn forget(&self) {
        let _ = std::fs::remove_file(&self.fingerprint_path);
    }
}

/// Where a task's own files go: `target/.jrs/tasks/`.
fn tasks_dir(manifest: &Manifest) -> PathBuf {
    static_target(manifest).join(".jrs").join("tasks")
}

/// Expand a task against `ctx`, find its program, and work out its
/// environment and fingerprint. `extra_args` (from `jrs task <name> -- ...`)
/// go after its own arguments. Writes `{classpath-argfile}` when it is used.
///
/// # Errors
///
/// [`JrsError::Build`] if the task has nothing to run, uses `{jar}` before
/// there is one, names a program that is not on `PATH` or a script that does
/// not exist; [`JrsError::Io`] if the argfile cannot be written or an input
/// directory cannot be walked.
pub fn prepare(task: &TaskDef, ctx: &Context<'_>, extra_args: &[String]) -> Result<Prepared> {
    let manifest = ctx.manifest;
    let root = static_root(manifest);
    let argfile = tasks_dir(manifest).join(format!("{}.cp.args", task.name));
    let mut wants_argfile = false;

    let join = |entries: &[PathBuf]| Toolchain::classpath(entries);
    let mut value = |p: Placeholder| -> Result<String> {
        let classpaths = || {
            ctx.classpaths.ok_or_else(|| {
                JrsError::build(format!(
                    "task `{}`: `{{{}}}` needs the dependencies resolved first",
                    task.name,
                    p.name()
                ))
            })
        };
        match p {
            Placeholder::Classpath => Ok(join(&classpaths()?.compile)),
            Placeholder::RuntimeClasspath => Ok(join(&classpaths()?.runtime)),
            Placeholder::TestClasspath => Ok(join(&classpaths()?.test)),
            Placeholder::ClasspathArgfile => {
                classpaths()?;
                wants_argfile = true;
                Ok(argfile.display().to_string())
            }
            Placeholder::Jar => ctx.jar.map(|j| j.display().to_string()).ok_or_else(|| {
                JrsError::build(format!(
                    "task `{}` uses `{{jar}}`, but nothing has been packaged yet\n\n\
                     add \"package\" to `tasks.{}.depends-on`",
                    task.name, task.name
                ))
            }),
            _ => static_value(manifest, p),
        }
    };
    // A `main` or `script` with dependencies of its own gets them as `-cp`,
    // from an argfile, as `{classpath-argfile}` gets the project's.
    let tool_argfile = tasks_dir(manifest).join(format!("{}.tool.args", task.name));
    let tool_classpath = if task.dependencies.is_empty() {
        None
    } else {
        Some(ctx.tool_classpath.ok_or_else(|| {
            JrsError::build(format!(
                "task `{}`: its dependencies have not been resolved",
                task.name
            ))
        })?)
    };
    if let Some(classpath) = tool_classpath {
        if let Some(dir) = tool_argfile.parent() {
            std::fs::create_dir_all(dir).path(dir)?;
        }
        let text = render_argfile(&["-cp".to_string(), join(classpath)], &[]);
        std::fs::write(&tool_argfile, text).path(&tool_argfile)?;
    }
    let search_path = task_path(ctx);
    let launch = launch(
        task,
        extra_args,
        ctx,
        tool_classpath.map(|_| tool_argfile.as_path()),
        &search_path,
        &mut value,
    )?;
    let cwd = match &task.cwd {
        Some(t) => root.join(t.expand(&mut value)?),
        None => root.clone(),
    };
    let mut own_env = Vec::with_capacity(task.env.len());
    for (key, t) in &task.env {
        own_env.push((key.clone(), t.expand(&mut value)?));
    }
    let resolve_paths =
        |list: Vec<String>| -> Vec<PathBuf> { list.into_iter().map(|p| root.join(p)).collect() };
    let inputs = resolve_paths(expand_list(&task.inputs, &mut value)?);
    let outputs = resolve_paths(expand_list(&task.outputs, &mut value)?);

    if wants_argfile && let Some(classpaths) = ctx.classpaths {
        if let Some(dir) = argfile.parent() {
            std::fs::create_dir_all(dir).path(dir)?;
        }
        let text = render_argfile(&["-cp".to_string(), join(&classpaths.compile)], &[]);
        std::fs::write(&argfile, text).path(&argfile)?;
    }

    let env = environment(task, ctx, &search_path, &own_env);
    let fingerprint = if task.inputs.is_empty() || task.outputs.is_empty() {
        None
    } else {
        Some(fingerprint(
            &launch,
            &cwd,
            &own_env,
            ctx,
            &inputs,
            needs_classpath(task),
            tool_classpath.unwrap_or_default(),
        )?)
    };
    Ok(Prepared {
        name: task.name.clone(),
        launch,
        cwd,
        env,
        outputs,
        fingerprint,
        fingerprint_path: tasks_dir(manifest).join(format!("{}.fingerprint", task.name)),
    })
}

/// The process a task's action starts: its command, its `args`, then
/// `extra_args`. `tool_argfile` holds `-cp` and the task's own dependencies,
/// when it has any.
fn launch(
    task: &TaskDef,
    extra_args: &[String],
    ctx: &Context<'_>,
    tool_argfile: Option<&Path>,
    search_path: &OsString,
    value: &mut impl FnMut(Placeholder) -> Result<String>,
) -> Result<Launch> {
    let classpath_arg = tool_argfile.map(|a| format!("@{}", a.display()));
    let root = static_root(ctx.manifest);
    let mut trailing = expand_list(&task.args, &mut *value)?;
    trailing.extend(extra_args.iter().cloned());
    Ok(match &task.action {
        Some(Action::Run(argv)) => {
            let mut command = expand_list(argv, &mut *value)?;
            let program = command.remove(0);
            command.extend(trailing);
            Launch::Exec {
                program: locate_program(&task.name, &program, &root, search_path)?,
                args: command,
            }
        }
        Some(Action::Script(file)) => {
            let file = root.join(file.expand(&mut *value)?);
            if !file.is_file() {
                return Err(JrsError::build(format!(
                    "task `{}`: the script {} does not exist",
                    task.name,
                    file.display()
                )));
            }
            let mut command: Vec<String> = classpath_arg.into_iter().collect();
            command.push(file.display().to_string());
            command.extend(trailing);
            Launch::Exec {
                program: ctx.toolchain.java.clone(),
                args: command,
            }
        }
        Some(Action::Main(class)) => {
            let Some(classpath_arg) = classpath_arg else {
                return Err(JrsError::build(format!(
                    "task `{}` runs `{class}`, but has no dependencies to find it in",
                    task.name
                )));
            };
            let mut command = vec![classpath_arg, class.clone()];
            command.extend(trailing);
            Launch::Exec {
                program: ctx.toolchain.java.clone(),
                args: command,
            }
        }
        Some(Action::Shell(script)) => Launch::Shell {
            script: script.clone(),
            args: trailing,
        },
        None => {
            return Err(JrsError::build(format!(
                "task `{}` has nothing to run of its own",
                task.name
            )));
        }
    })
}

fn expand_list(
    templates: &[Template],
    value: &mut impl FnMut(Placeholder) -> Result<String>,
) -> Result<Vec<String>> {
    templates.iter().map(|t| t.expand(&mut *value)).collect()
}

/// `PATH` as a task sees it: the JDK's `bin` first, so `java` is the
/// project's JDK.
fn task_path(ctx: &Context<'_>) -> OsString {
    let bin = ctx
        .toolchain
        .java
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let rest = ctx
        .path
        .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(rest)).unwrap_or_default()
}

/// A `run` program: taken relative to the root when it has a path separator,
/// looked up on the task's `PATH` otherwise.
fn locate_program(task: &str, program: &str, root: &Path, path: &OsString) -> Result<PathBuf> {
    if program.contains(['/', '\\']) {
        let located = root.join(program);
        if located.is_file() {
            return Ok(located);
        }
        return Err(JrsError::build(format!(
            "task `{task}`: {} does not exist",
            located.display()
        )));
    }
    find_program(program, path).ok_or_else(|| {
        let searched: Vec<String> = std::env::split_paths(path)
            .map(|p| format!("  {}", p.display()))
            .collect();
        JrsError::build(format!(
            "task `{task}`: `{program}` is not on PATH\n\nsearched:\n{}",
            searched.join("\n")
        ))
    })
}

/// The variables jrs adds for every task (TASKS.md §5.2), then the task's own
/// `env`, which wins.
fn environment(
    task: &TaskDef,
    ctx: &Context<'_>,
    path: &OsString,
    own: &[(String, String)],
) -> Vec<(String, OsString)> {
    let manifest = ctx.manifest;
    let toolchain = ctx.toolchain;
    let java_home = toolchain.home.clone().unwrap_or_else(|| {
        toolchain
            .java
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default()
    });
    let target = static_target(manifest);
    let mut env: Vec<(String, OsString)> = vec![
        ("JAVA_HOME".into(), java_home.into()),
        ("PATH".into(), path.clone()),
        ("JRS_TASK".into(), task.name.clone().into()),
    ];
    if let Some(hook) = ctx.hook {
        env.push(("JRS_HOOK".into(), hook.name().into()));
    }
    env.extend([
        ("JRS_ROOT".into(), static_root(manifest).into()),
        ("JRS_TARGET_DIR".into(), target.clone().into()),
        ("JRS_CLASSES_DIR".into(), target.join("classes").into()),
        ("JRS_PROJECT_NAME".into(), manifest.name.clone().into()),
        (
            "JRS_PROJECT_VERSION".into(),
            manifest.version.clone().into(),
        ),
    ]);
    if let Some(classpaths) = ctx.classpaths {
        env.push((
            "JRS_CLASSPATH".into(),
            Toolchain::classpath(&classpaths.compile).into(),
        ));
        env.push((
            "JRS_RUNTIME_CLASSPATH".into(),
            Toolchain::classpath(&classpaths.runtime).into(),
        ));
    }
    if let Some(jar) = ctx.jar {
        env.push(("JRS_JAR".into(), jar.into()));
    }
    if ctx.offline {
        env.push(("JRS_OFFLINE".into(), "1".into()));
    }
    env.push(("SOURCE_DATE_EPOCH".into(), SOURCE_DATE_EPOCH.into()));
    for (key, value) in own {
        // Windows environment names are case-insensitive.
        env.retain(|(k, _)| {
            if cfg!(windows) {
                !k.eq_ignore_ascii_case(key)
            } else {
                k != key
            }
        });
        env.push((key.clone(), value.into()));
    }
    env
}

/// Everything that, if changed, means a task's outputs are out of date:
/// the expanded command, its working directory and own environment, the JDK,
/// each input file's size and modification time, and, when it reads a
/// classpath, each jar's too — a snapshot changes without changing its path,
/// as the compile fingerprint knows (SPEC §7.2). The task's own tool jars
/// always count: they are what runs.
fn fingerprint(
    launch: &Launch,
    cwd: &Path,
    own_env: &[(String, String)],
    ctx: &Context<'_>,
    inputs: &[PathBuf],
    reads_classpath: bool,
    tool_classpath: &[PathBuf],
) -> Result<String> {
    let mut s = String::new();
    match launch {
        Launch::Exec { program, args } => {
            let _ = writeln!(s, "exec {}\u{1}{}", program.display(), args.join("\u{1}"));
        }
        Launch::Shell { script, args } => {
            let _ = writeln!(s, "shell {script}\u{1}{}", args.join("\u{1}"));
        }
    }
    let _ = writeln!(s, "cwd {}", cwd.display());
    for (key, value) in own_env {
        let _ = writeln!(s, "env {key}={value}");
    }
    let _ = writeln!(s, "jdk {}", ctx.toolchain.version);
    if reads_classpath && let Some(classpaths) = ctx.classpaths {
        let mut jars: Vec<&PathBuf> = classpaths
            .compile
            .iter()
            .chain(&classpaths.runtime)
            .chain(&classpaths.test)
            .collect();
        jars.sort();
        jars.dedup();
        for jar in jars {
            if let Some((size, modified)) = stamp(jar) {
                let _ = writeln!(s, "jar {} {size} {modified}", jar.display());
            }
        }
    }
    for jar in tool_classpath {
        if let Some((size, modified)) = stamp(jar) {
            let _ = writeln!(s, "tool {} {size} {modified}", jar.display());
        }
    }
    for input in inputs {
        if input.is_dir() {
            for file in project::find_all(input)? {
                if let Some((size, modified)) = stamp(&file) {
                    let _ = writeln!(s, "input {} {size} {modified}", file.display());
                }
            }
        } else if let Some((size, modified)) = stamp(input) {
            let _ = writeln!(s, "input {} {size} {modified}", input.display());
        } else {
            let _ = writeln!(s, "input {} missing", input.display());
        }
    }
    Ok(s)
}

/// A file's size and modification time, from one `stat`.
fn stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    Some((meta.len(), modified))
}

// ---- the run and test JVMs -------------------------------------------------

/// A placeholder's value in `run.env`, `run.cwd` or `test.env`: what a task
/// would see, with the dependencies resolved. `{jar}` and
/// `{classpath-argfile}` were refused when the manifest was parsed.
fn jvm_value(manifest: &Manifest, classpaths: &Classpaths, p: Placeholder) -> Result<String> {
    match p {
        Placeholder::Classpath => Ok(Toolchain::classpath(&classpaths.compile)),
        Placeholder::RuntimeClasspath => Ok(Toolchain::classpath(&classpaths.runtime)),
        Placeholder::TestClasspath => Ok(Toolchain::classpath(&classpaths.test)),
        Placeholder::Jar | Placeholder::ClasspathArgfile => Err(JrsError::manifest(format!(
            "`{{{}}}` only has a value in a task",
            p.name()
        ))),
        _ => static_value(manifest, p),
    }
}

/// `run.env` or `test.env`, expanded: what the JVM adds to the environment it
/// inherits from jrs, in declaration order.
///
/// # Errors
///
/// [`JrsError::Manifest`] for a placeholder only a task has a value for,
/// which parsing already refuses.
pub fn jvm_env(
    manifest: &Manifest,
    env: &[(String, Template)],
    classpaths: &Classpaths,
) -> Result<Vec<(String, String)>> {
    env.iter()
        .map(|(key, t)| {
            Ok((
                key.clone(),
                t.expand(|p| jvm_value(manifest, classpaths, p))?,
            ))
        })
        .collect()
}

/// `run.cwd`, expanded and taken relative to the project root, as a task's
/// `cwd` is.
///
/// # Errors
///
/// [`JrsError::Build`] for a classpath placeholder, which parsing already
/// refuses in a path.
pub fn jvm_cwd(manifest: &Manifest, cwd: &Template) -> Result<PathBuf> {
    let expanded = cwd.expand(|p| static_value(manifest, p))?;
    Ok(static_root(manifest).join(expanded))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "[project]\nname='app'\nversion='1.0.0'\n";

    fn parse(text: &str) -> Result<Manifest> {
        let root = std::env::temp_dir().join("jrs-task-unit");
        Manifest::parse(&format!("{HEAD}{text}"), &root.join("jrs.toml"), &root)
    }

    fn names(steps: &[TaskRef]) -> Vec<String> {
        steps.iter().map(ToString::to_string).collect()
    }

    fn toolchain() -> Toolchain {
        Toolchain {
            javac: PathBuf::from("/jdk/bin/javac"),
            java: PathBuf::from("/jdk/bin/java"),
            jar: PathBuf::from("/jdk/bin/jar"),
            version: 21,
            home: Some(PathBuf::from("/jdk")),
        }
    }

    fn context<'a>(m: &'a Manifest, t: &'a Toolchain) -> Context<'a> {
        Context {
            manifest: m,
            toolchain: t,
            hook: None,
            classpaths: None,
            jar: None,
            tool_classpath: None,
            offline: false,
            path: None,
        }
    }

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let root = std::env::temp_dir().join(format!("jrs-task-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree(root)
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn manifest(&self, text: &str) -> Manifest {
            Manifest::parse(&format!("{HEAD}{text}"), &self.0.join("jrs.toml"), &self.0).unwrap()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dependencies_run_first_in_the_order_they_are_listed() {
        let m = parse(
            "[tasks.release]\ndepends-on = ['package', 'checksum']\n\
             [tasks.checksum]\nshell = 'x'\n",
        )
        .unwrap();
        let order = plan(&m, &[TaskRef::Task("release".into())]);
        assert_eq!(names(&order), ["package", "checksum", "release"]);
    }

    #[test]
    fn a_diamond_runs_each_task_once() {
        let m = parse(
            "[tasks.top]\ndepends-on = ['left', 'right']\n\
             [tasks.left]\ndepends-on = ['base']\nshell = 'l'\n\
             [tasks.right]\ndepends-on = ['base']\nshell = 'r'\n\
             [tasks.base]\nshell = 'b'\n",
        )
        .unwrap();
        let order = plan(&m, &[TaskRef::Task("top".into())]);
        assert_eq!(names(&order), ["base", "left", "right", "top"]);
    }

    #[test]
    fn a_cycle_names_its_path() {
        let err = parse(
            "[tasks.a]\ndepends-on = ['b']\nshell = 'a'\n\
             [tasks.b]\ndepends-on = ['a']\nshell = 'b'\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("a → b → a"), "{err}");

        let err = parse("[tasks.a]\ndepends-on = ['a']\nshell = 'a'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("a → a"), "{err}");
    }

    #[test]
    fn a_hook_task_that_depends_on_its_own_command_is_a_cycle() {
        let err = parse(
            "[tasks.gen]\ndepends-on = ['build']\nshell = 'g'\n\
             [hooks]\npre-compile = ['gen']\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("gen → build → gen"), "{err}");
        assert!(err.contains("`build` runs the `pre-compile` hook"), "{err}");

        // `package` fires the build hooks too.
        let err = parse(
            "[tasks.gen]\ndepends-on = ['package']\nshell = 'g'\n\
             [hooks]\npost-compile = ['gen']\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("gen → package → gen"), "{err}");

        // But a post-test task may package: `package` does not fire post-test.
        parse(
            "[tasks.ship]\ndepends-on = ['package']\nshell = 's'\n\
             [hooks]\npost-test = ['ship']\n",
        )
        .unwrap();
    }

    #[test]
    fn unknown_references_are_errors() {
        let err = parse("[tasks.a]\ndepends-on = ['nope']\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`tasks.a.depends-on` names `nope`"), "{err}");
        let err = parse("[hooks]\npre-compile = ['nope']\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`hooks.pre-compile` names `nope`"), "{err}");
        let err = parse("[hooks]\npost-test = ['package']\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("a hook runs tasks"), "{err}");
    }

    #[test]
    fn the_jar_is_only_available_after_package() {
        let err = parse("[tasks.sign]\nrun = ['sign', '{jar}']\n[hooks]\npre-compile = ['sign']\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("`tasks.sign` uses `{jar}`"), "{err}");
        assert!(err.contains("`pre-compile` hook"), "{err}");

        // Reached through a dependency counts too.
        let err = parse(
            "[tasks.sign]\nrun = ['sign', '{jar}']\n[tasks.all]\ndepends-on = ['sign']\n\
             [hooks]\npost-compile = ['all']\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`post-compile` hook"), "{err}");

        parse("[tasks.sign]\nrun = ['sign', '{jar}']\n[hooks]\npost-package = ['sign']\n").unwrap();
        parse(
            "[tasks.sign]\nrun = ['sign', '{jar}']\ndepends-on = ['package']\n\
             [hooks]\npost-test = ['sign']\n",
        )
        .unwrap();
    }

    #[test]
    fn generated_directories_must_live_under_the_target_directory() {
        for bad in ["src/generated", "{root}/gen", "{target}/../src", "{target}"] {
            let err = parse(&format!(
                "[tasks.gen]\nshell = 'g'\nsource-outputs = ['{bad}']\n[hooks]\npre-compile = ['gen']\n"
            ))
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("not inside `project.target-dir`"),
                "{bad}: {err}"
            );
        }
        let m = parse(
            "[tasks.gen]\nshell = 'g'\nsource-outputs = ['{target}/gen', 'target/more']\n\
             resource-outputs = ['target/res']\n[hooks]\npre-compile = ['gen']\n",
        )
        .unwrap();
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let g = generated(&m, Hook::PreCompile).unwrap();
        assert_eq!(g.sources.len(), 2);
        assert!(g.sources[1].ends_with("target/more"));
        assert_eq!(generated(&m, Hook::PreTest).unwrap(), Generated::default());
    }

    #[test]
    fn generated_directories_nothing_compiles_are_warned_about() {
        let m = parse("[tasks.gen]\nshell = 'g'\nsource-outputs = ['target/gen']\n").unwrap();
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("`tasks.gen.source-outputs` is ignored")),
            "{:?}",
            m.warnings
        );
    }

    #[test]
    fn a_pre_test_generator_feeds_the_test_unit_unless_pre_compile_runs_it_too() {
        let m = parse(
            "[tasks.fixtures]\nshell = 'f'\nsource-outputs = ['target/test-gen']\n\
             [tasks.both]\nshell = 'b'\nsource-outputs = ['target/both']\n\
             [hooks]\npre-compile = ['both']\npre-test = ['fixtures', 'both']\n",
        )
        .unwrap();
        let test = generated(&m, Hook::PreTest).unwrap();
        assert_eq!(test.sources.len(), 1);
        assert!(test.sources[0].ends_with("target/test-gen"));
        assert!(generated(&m, Hook::PreCompile).unwrap().sources[0].ends_with("target/both"));
    }

    #[test]
    fn placeholders_expand_and_braces_escape() {
        let tree = Tree::new("expand");
        let m = tree.manifest(
            "[tasks.t]\nrun = ['bin/tool', '{project.name}-{project.version}', '{{literal}}', '{classes}']\n\
             args = ['{target}']\ncwd = '{target}'\nenv = { OUT = '{root}/out' }\n",
        );
        tree.write("bin/tool", "");
        let t = toolchain();
        let p = prepare(&m.tasks[0], &context(&m, &t), &["extra".into()]).unwrap();
        let root = std::path::absolute(&tree.0).unwrap();
        let Launch::Exec { program, args } = &p.launch else {
            panic!("{:?}", p.launch)
        };
        assert_eq!(*program, root.join("bin/tool"));
        assert_eq!(args[0], "app-1.0.0");
        assert_eq!(args[1], "{literal}");
        assert_eq!(
            args[2],
            root.join("target").join("classes").display().to_string()
        );
        assert_eq!(args[3], root.join("target").display().to_string());
        assert_eq!(args[4], "extra", "`--` arguments come last");
        assert_eq!(p.cwd, root.join("target"));
        let out = p.env.iter().find(|(k, _)| k == "OUT").unwrap();
        assert_eq!(out.1, OsString::from(format!("{}/out", root.display())));
    }

    #[test]
    fn a_script_runs_on_the_projects_java() {
        let tree = Tree::new("script");
        tree.write("build/Gen.java", "");
        let m = tree.manifest("[tasks.gen]\nscript = 'build/Gen.java'\nargs = ['a']\n");
        let t = toolchain();
        let p = prepare(&m.tasks[0], &context(&m, &t), &[]).unwrap();
        let Launch::Exec { program, args } = &p.launch else {
            panic!()
        };
        assert_eq!(*program, PathBuf::from("/jdk/bin/java"));
        assert!(args[0].ends_with("Gen.java"));
        assert_eq!(args[1], "a");

        let m = tree.manifest("[tasks.gen]\nscript = 'build/Missing.java'\n");
        let err = prepare(&m.tasks[0], &context(&m, &t), &[]).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn the_environment_carries_what_jrs_knows_and_the_task_env_wins() {
        let tree = Tree::new("env");
        tree.write("Gen.java", "");
        let m = tree.manifest(
            "[tasks.gen]\nscript = 'Gen.java'\nenv = { SOURCE_DATE_EPOCH = '0', MODE = 'x' }\n",
        );
        let t = toolchain();
        let classpaths = Classpaths {
            compile: vec![PathBuf::from("/c.jar")],
            runtime: vec![PathBuf::from("/r.jar")],
            test: Vec::new(),
        };
        let jar = tree.0.join("target/app-1.0.0.jar");
        let ctx = Context {
            hook: Some(Hook::PostPackage),
            classpaths: Some(&classpaths),
            jar: Some(&jar),
            offline: true,
            ..context(&m, &t)
        };
        let p = prepare(&m.tasks[0], &ctx, &[]).unwrap();
        let get = |k: &str| {
            p.env
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("JAVA_HOME").as_deref(), Some("/jdk"));
        assert!(get("PATH").unwrap().starts_with("/jdk/bin"));
        assert_eq!(get("JRS_TASK").as_deref(), Some("gen"));
        assert_eq!(get("JRS_HOOK").as_deref(), Some("post-package"));
        assert_eq!(get("JRS_CLASSPATH").as_deref(), Some("/c.jar"));
        assert_eq!(get("JRS_RUNTIME_CLASSPATH").as_deref(), Some("/r.jar"));
        assert_eq!(get("JRS_JAR"), Some(jar.display().to_string()));
        assert_eq!(get("JRS_OFFLINE").as_deref(), Some("1"));
        assert_eq!(get("SOURCE_DATE_EPOCH").as_deref(), Some("0"));
        assert_eq!(get("MODE").as_deref(), Some("x"));
        assert_eq!(
            p.env
                .iter()
                .filter(|(k, _)| k == "SOURCE_DATE_EPOCH")
                .count(),
            1
        );

        let bare = prepare(&m.tasks[0], &context(&m, &t), &[]).unwrap();
        assert!(
            !bare
                .env
                .iter()
                .any(|(k, _)| k == "JRS_HOOK" || k == "JRS_JAR")
        );
    }

    #[test]
    fn the_jar_placeholder_needs_a_jar_at_run_time() {
        let tree = Tree::new("jar");
        let m = tree.manifest("[tasks.sign]\nshell = 's'\nargs = ['{jar}']\n");
        let t = toolchain();
        let err = prepare(&m.tasks[0], &context(&m, &t), &[]).unwrap_err();
        assert!(err.to_string().contains("add \"package\""), "{err}");
    }

    #[test]
    fn classpath_placeholders_and_variables_need_resolution() {
        let m = parse("[tasks.a]\nrun = ['java', '@{classpath-argfile}']\n").unwrap();
        assert!(needs_classpath(&m.tasks[0]));
        let m = parse("[tasks.a]\nshell = 'java -cp \"$JRS_CLASSPATH\" Main'\n").unwrap();
        assert!(needs_classpath(&m.tasks[0]));
        let m = parse("[tasks.a]\nshell = 'echo $JRS_JAR'\n").unwrap();
        assert!(!needs_classpath(&m.tasks[0]));
    }

    #[test]
    fn the_classpath_argfile_is_written_where_the_placeholder_points() {
        let tree = Tree::new("argfile");
        tree.write("Gen.java", "");
        let m =
            tree.manifest("[tasks.gen]\nscript = 'Gen.java'\nargs = ['@{classpath-argfile}']\n");
        let t = toolchain();
        let classpaths = Classpaths {
            compile: vec![PathBuf::from("/a b/c.jar")],
            ..Classpaths::default()
        };
        let ctx = Context {
            classpaths: Some(&classpaths),
            ..context(&m, &t)
        };
        let p = prepare(&m.tasks[0], &ctx, &[]).unwrap();
        let Launch::Exec { args, .. } = &p.launch else {
            panic!()
        };
        let path = args[1].strip_prefix('@').unwrap();
        assert!(path.ends_with("gen.cp.args"), "{path}");
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "-cp\n\"/a b/c.jar\"\n"
        );
    }

    #[test]
    fn a_fingerprint_follows_the_inputs_the_command_and_the_classpath() {
        let tree = Tree::new("fingerprint");
        tree.write("Gen.java", "");
        let input = tree.write("proto/a.proto", "one");
        let text =
            "[tasks.gen]\nscript = 'Gen.java'\ninputs = ['proto']\noutputs = ['target/gen']\n";
        let m = tree.manifest(text);
        let t = toolchain();
        let jar = tree.write("lib.jar", "jar");
        let classpaths = Classpaths {
            compile: vec![jar.clone()],
            ..Classpaths::default()
        };
        let fp = |m: &Manifest, extra: &[String], cp: Option<&Classpaths>| {
            let ctx = Context {
                classpaths: cp,
                ..context(m, &t)
            };
            prepare(&m.tasks[0], &ctx, extra).unwrap()
        };

        let first = fp(&m, &[], None);
        assert!(first.fingerprint.is_some());
        assert!(!first.is_fresh(), "never run");
        first.record().unwrap();
        assert!(!first.is_fresh(), "an output is missing");
        std::fs::create_dir_all(tree.0.join("target/gen")).unwrap();
        assert!(first.is_fresh());

        // Another input mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&input, "two").unwrap();
        assert!(!fp(&m, &[], None).is_fresh());
        fp(&m, &[], None).record().unwrap();
        assert!(fp(&m, &[], None).is_fresh());

        // Another argv.
        assert!(!fp(&m, &["x".into()], None).is_fresh());

        // Another env value.
        let with_env = tree.manifest(&format!("{text}env = {{ A = 'b' }}\n"));
        assert!(!fp(&with_env, &[], None).is_fresh());

        // A snapshot jar that changed size under the same path.
        let reads = tree.manifest(&format!("{text}args = ['{{classpath}}']\n"));
        fp(&reads, &[], Some(&classpaths)).record().unwrap();
        assert!(fp(&reads, &[], Some(&classpaths)).is_fresh());
        std::fs::write(&jar, "a bigger jar").unwrap();
        assert!(!fp(&reads, &[], Some(&classpaths)).is_fresh());

        // A failed run is forgotten.
        let again = fp(&reads, &[], Some(&classpaths));
        again.record().unwrap();
        again.forget();
        assert!(!again.is_fresh());
    }

    #[test]
    fn a_main_task_runs_its_class_from_its_own_dependencies() {
        let tree = Tree::new("main");
        let m = tree.manifest(
            "[tasks.fmt]\nmain = 'com.example.Fmt'\nargs = ['--replace']\n\
             [tasks.fmt.dependencies]\n'com.example:fmt' = '1.0'\n",
        );
        let t = toolchain();
        let mut ctx = context(&m, &t);
        let task = m.task("fmt").unwrap();
        let err = prepare(task, &ctx, &[]).unwrap_err().to_string();
        assert!(err.contains("have not been resolved"), "{err}");

        let jars = [
            PathBuf::from("/cache/fmt-1.0.jar"),
            PathBuf::from("/cache/dep-2.0.jar"),
        ];
        ctx.tool_classpath = Some(&jars);
        let prepared = prepare(task, &ctx, &["x".to_string()]).unwrap();
        let argfile = tree.0.join("target/.jrs/tasks/fmt.tool.args");
        let Launch::Exec { program, args } = &prepared.launch else {
            panic!("{:?}", prepared.launch);
        };
        assert_eq!(program, &t.java);
        assert_eq!(
            args,
            &[
                format!("@{}", argfile.display()),
                "com.example.Fmt".to_string(),
                "--replace".to_string(),
                "x".to_string()
            ]
        );
        let text = std::fs::read_to_string(&argfile).unwrap();
        assert!(text.starts_with("-cp"), "{text}");
        assert!(
            text.contains("fmt-1.0.jar") && text.contains("dep-2.0.jar"),
            "{text}"
        );
    }

    #[test]
    fn a_script_with_dependencies_gets_them_as_its_classpath() {
        let tree = Tree::new("script-deps");
        tree.write("Gen.java", "class Gen {}");
        let m = tree.manifest(
            "[tasks.gen]\nscript = 'Gen.java'\n[tasks.gen.dependencies]\n'g:poet' = '1.0'\n",
        );
        let t = toolchain();
        let jars = [PathBuf::from("/cache/poet-1.0.jar")];
        let mut ctx = context(&m, &t);
        ctx.tool_classpath = Some(&jars);
        let prepared = prepare(m.task("gen").unwrap(), &ctx, &[]).unwrap();
        let Launch::Exec { args, .. } = &prepared.launch else {
            panic!("{:?}", prepared.launch);
        };
        assert!(args[0].starts_with('@') && args[0].ends_with("gen.tool.args"));
        assert!(args[1].ends_with("Gen.java"), "{args:?}");
    }

    #[test]
    fn a_fingerprint_follows_the_tasks_own_tool_jars() {
        let tree = Tree::new("tool-fingerprint");
        tree.write("in.txt", "x");
        let jar = tree.write("tools/tool.jar", "a");
        let m = tree.manifest(
            "[tasks.t]\nmain = 'x.Y'\ninputs = ['in.txt']\noutputs = ['out']\n\
             [tasks.t.dependencies]\n'g:tool' = '1.0'\n",
        );
        let t = toolchain();
        let jars = [jar.clone()];
        let mut ctx = context(&m, &t);
        ctx.tool_classpath = Some(&jars);
        let task = m.task("t").unwrap();
        let before = prepare(task, &ctx, &[]).unwrap().fingerprint.unwrap();
        assert_eq!(
            before,
            prepare(task, &ctx, &[]).unwrap().fingerprint.unwrap()
        );
        std::fs::write(&jar, "a different tool").unwrap();
        assert_ne!(
            before,
            prepare(task, &ctx, &[]).unwrap().fingerprint.unwrap()
        );
    }

    #[test]
    fn a_task_without_inputs_and_outputs_always_runs() {
        let tree = Tree::new("always");
        tree.write("Gen.java", "");
        let m = tree.manifest("[tasks.gen]\nscript = 'Gen.java'\noutputs = ['Gen.java']\n");
        let t = toolchain();
        let p = prepare(&m.tasks[0], &context(&m, &t), &[]).unwrap();
        assert!(p.fingerprint.is_none());
        p.record().unwrap();
        assert!(!p.is_fresh());
    }

    #[test]
    fn a_program_not_on_path_says_where_jrs_looked() {
        let tree = Tree::new("missing-program");
        let m = tree.manifest("[tasks.t]\nrun = ['jrs-no-such-program']\n");
        let t = toolchain();
        let path = std::ffi::OsString::from("/nowhere");
        let ctx = Context {
            path: Some(&path),
            ..context(&m, &t)
        };
        let err = prepare(&m.tasks[0], &ctx, &[]).unwrap_err().to_string();
        assert!(
            err.contains("`jrs-no-such-program` is not on PATH"),
            "{err}"
        );
        assert!(err.contains("/jdk/bin"), "{err}");
        assert!(err.contains("/nowhere"), "{err}");
    }

    #[test]
    fn the_list_names_hooks_and_marks_shell_tasks() {
        let m = parse(
            "[tasks.build-info]\ndescription = 'Generate BuildInfo.java'\nshell = 'x'\n\
             [tasks.format]\nrun = ['fmt']\n\
             [tasks.release]\ndescription = 'Package, then checksum'\ndepends-on = ['package']\n\
             [hooks]\npre-compile = ['build-info']\n",
        )
        .unwrap();
        assert_eq!(
            list(&m),
            [
                "build-info   Generate BuildInfo.java   (pre-compile, sh)",
                "format",
                "release      Package, then checksum",
            ]
        );
    }

    #[test]
    fn watched_inputs_skip_the_target_directory() {
        let m = parse("[tasks.t]\nshell = 'x'\ninputs = ['src/main/proto', '{target}/classes']\n")
            .unwrap();
        let watched = watched_inputs(&m);
        assert_eq!(watched.len(), 1);
        assert!(watched[0].ends_with("src/main/proto"));
    }

    #[test]
    fn the_jvm_environment_expands_like_a_tasks() {
        let m = parse(
            "[run]\nenv = { OUT = '{target}/out', WHO = '{project.name}-{project.version}', \
             CP = '{runtime-classpath}', LIT = '{{x}}' }\ncwd = '{target}/work'\n\
             [test]\nenv = { CP = '{test-classpath}' }\n",
        )
        .unwrap();
        let classpaths = Classpaths {
            compile: vec![PathBuf::from("/c.jar")],
            runtime: vec![PathBuf::from("/classes"), PathBuf::from("/r.jar")],
            test: vec![PathBuf::from("/t.jar")],
        };
        let env = jvm_env(&m, &m.run.env, &classpaths).unwrap();
        let root = static_root(&m);
        assert_eq!(
            env,
            vec![
                (
                    "OUT".to_string(),
                    format!("{}/out", root.join("target").display())
                ),
                ("WHO".to_string(), "app-1.0.0".to_string()),
                ("CP".to_string(), Toolchain::classpath(&classpaths.runtime)),
                ("LIT".to_string(), "{x}".to_string()),
            ]
        );
        assert_eq!(
            jvm_env(&m, &m.test.env, &classpaths).unwrap()[0].1,
            "/t.jar"
        );
        assert_eq!(
            jvm_cwd(&m, m.run.cwd.as_ref().unwrap()).unwrap(),
            root.join("target").join("work")
        );
        // Relative to the root, as a task's `cwd` is.
        let m = parse("[run]\ncwd = 'work'\n").unwrap();
        assert_eq!(
            jvm_cwd(&m, m.run.cwd.as_ref().unwrap()).unwrap(),
            root.join("work")
        );
    }
}
