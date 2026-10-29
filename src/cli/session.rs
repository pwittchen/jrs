//! The `Session` a command runs in, and its `build()` spine.
//!
//! A session holds one command's manifest, UI and clock. `build()` is what
//! `build`, `test`, `run` and `package` share: the dependencies (the
//! lockfile or a fresh resolution, the cache, the downloads), then the main
//! compile unit with its staleness check, then the resource copy.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::jdk::{install_jdk, is_java};
use super::{Cli, Command, effective_jobs, manifest_path};
use crate::build_cache::{self, BuildCache};
use crate::compile::worker::Worker;
use crate::compile::{self, CompileUnit, ForeignCompiler, Language, lang, share};
use crate::config::Config;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::lockfile::Lockfile;
use crate::manifest::{Builtin, Hook, JavaAgent, Manifest, Repository, TaskDef, TaskRef};
use crate::package::{self, JarManifest};
use crate::project::{self, Project, Sources, Unit};
use crate::resolve::cache::Cache;
use crate::resolve::coord::Coord;
use crate::resolve::repo::{Fetcher, Network};
use crate::resolve::{self, Classpath, Resolution, UiReporter};
use crate::timings::{self, Timings};
use crate::toolchain::{self, Toolchain};
use crate::ui::{self, Ui};
use crate::{obfuscate, runner, task};

/// One command's worth of state: the manifest, the user's configuration, the
/// UI, and the clock — and what has already happened, since a task and a
/// built-in command each run at most once per invocation, however many hooks
/// and `depends-on` lists reach them.
pub(super) struct Session<'a> {
    pub(super) manifest: Manifest,
    pub(super) ui: &'a Ui,
    pub(super) config: Config,
    pub(super) jobs: usize,
    pub(super) offline: bool,
    pub(super) started: Instant,
    pub(super) toolchain: OnceCell<Toolchain>,
    pub(super) resolution: OnceCell<Resolution>,
    /// Each language's compiler graph, resolved and pinned with the project's.
    pub(super) tools: OnceCell<Vec<Tool>>,
    /// Each task's own dependency graph (TASKS.md §8), by task name: resolved
    /// and pinned with the project's, and downloaded then, or when the task
    /// first runs.
    pub(super) task_tools: OnceCell<Vec<(String, Resolution)>>,
    /// `ProGuard`'s graph when `[obfuscate]` turns obfuscation on, resolved and
    /// pinned with the project's, and downloaded then or when `--obfuscate`
    /// runs. `Some(None)` means the project does not obfuscate.
    pub(super) obfuscator: OnceCell<Option<Resolution>>,
    /// Each java agent named with a version, by its `[[tool]]` name, as a
    /// graph of one: resolved and pinned with the project's, and downloaded
    /// then or when a JVM or an image first loads it.
    pub(super) agents: OnceCell<Vec<(String, Resolution)>>,
    /// The classpaths of the task graphs downloaded so far, by task name.
    pub(super) task_classpaths: RefCell<HashMap<String, Vec<PathBuf>>>,
    pub(super) built: OnceCell<Built>,
    /// Tasks already run (or found fresh) in this invocation.
    pub(super) ran: RefCell<HashSet<String>>,
    /// Built-ins a `depends-on` has already run.
    pub(super) done: RefCell<HashSet<Builtin>>,
    /// The jar, once `package` has written it.
    pub(super) jar: RefCell<Option<PathBuf>>,
    /// The wall time of each phase, for `--timings`.
    pub(super) timings: Timings,
    /// Whether the command asked for `--timings`.
    timings_wanted: bool,
    /// The build cache (SPEC §7.8), once the graph it keys jars by is
    /// known; `None` inside when it is turned off.
    pub(super) build_cache: OnceCell<Option<Arc<BuildCache>>>,
    /// What the command asked of the build cache.
    pub(super) cache_use: CacheUse,
    /// The `--watch` session's warm `javac`, which outlives each build's
    /// `Session` but not the command (SPEC §7.5).
    pub(super) worker: Option<Arc<Worker>>,
    /// The second wave of downloads, while it runs behind the main compile
    /// (SPEC §8.4).
    pending: RefCell<Option<Pending>>,
    /// What a workspace member's session is given (`specs/WORKSPACES.md`):
    /// its share of the workspace's graph, and the members it depends on.
    pub(super) member: Option<MemberContext>,
    /// For the session over a workspace's aggregate manifest: the members'
    /// directories, which make its `jrs.lock` a workspace's.
    pub(super) workspace_members: Vec<String>,
}

/// A workspace member's session's view of the rest of the workspace.
pub(super) struct MemberContext {
    /// Its share of the workspace's graph, and the tools' graphs it uses,
    /// in place of a resolution of its own.
    pub(super) preset: ResolvedWithTools,
    /// The members it depends on, nearest first, with their classpaths.
    pub(super) upstream: Vec<crate::workspace::Upstream>,
    /// The workspace's root: what the build cache keys paths against, so
    /// that a member's key names its upstream members' classes relatively.
    pub(super) root: PathBuf,
}

/// The jars the main compile does not need, downloading on a thread of
/// their own while it runs, and what the graph waits on until they are in.
struct Pending {
    thread: std::thread::JoinHandle<Result<Resolution>>,
    /// How many jars it downloads.
    count: usize,
    /// The graph with the first wave's jars in place.
    resolution: Resolution,
    /// Whether the graph was resolved afresh, so that `jrs.lock` is written
    /// once every jar's checksum is known.
    pub(super) fresh: bool,
}

/// What a command asks of the build cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheUse {
    /// Restore and store.
    On,
    /// `--no-build-cache`: neither.
    Off,
    /// `jrs build --verify-cache`: compile, and compare with what it holds.
    Verify,
}

impl<'a> Session<'a> {
    pub(super) fn open(cli: &Cli, ui: &'a Ui) -> Result<Session<'a>> {
        let manifest = Manifest::load(manifest_path(cli)?)?;
        for warning in &manifest.warnings {
            ui.warn(warning);
        }
        if let Some(module) = manifest.modules.first() {
            return Err(JrsError::manifest(format!(
                "{}: `{}` is a path dependency on another module, `{}`, which only a \
                 workspace can build\n\nadd a jrs.toml above both with:\n\n    \
                 [workspace]\n    members = [...]",
                manifest.path.display(),
                module.name,
                module.path
            )));
        }
        if let Some((task, reference)) = manifest.tasks.iter().find_map(|t| {
            t.depends_on
                .iter()
                .find(|d| matches!(d, TaskRef::Member { .. }))
                .map(|d| (t, d))
        }) {
            return Err(JrsError::manifest(format!(
                "{}: `tasks.{}.depends-on` names `{reference}`, another member's task, and \
                 the project is not in a workspace",
                manifest.path.display(),
                task.name
            )));
        }
        if !cli.global.modules.is_empty() {
            return Err(JrsError::usage(format!(
                "-p names a workspace member, and {} is not in a workspace",
                manifest.path.display()
            )));
        }
        let config = Config::load()?;
        for warning in &config.warnings {
            ui.warn(warning);
        }
        Ok(Session::new(cli, ui, manifest, config))
    }

    /// A session over `manifest`, with `config` already loaded and its
    /// warnings said.
    pub(super) fn new(cli: &Cli, ui: &'a Ui, manifest: Manifest, config: Config) -> Session<'a> {
        Session {
            manifest,
            ui,
            jobs: effective_jobs(cli.global.jobs, config.jobs),
            config,
            offline: cli.global.offline,
            started: Instant::now(),
            toolchain: OnceCell::new(),
            resolution: OnceCell::new(),
            tools: OnceCell::new(),
            task_tools: OnceCell::new(),
            obfuscator: OnceCell::new(),
            agents: OnceCell::new(),
            task_classpaths: RefCell::new(HashMap::new()),
            built: OnceCell::new(),
            ran: RefCell::new(HashSet::new()),
            done: RefCell::new(HashSet::new()),
            jar: RefCell::new(None),
            timings: Timings::new(),
            timings_wanted: cli.command.timings(),
            build_cache: OnceCell::new(),
            cache_use: match cli.command {
                Command::Build {
                    verify_cache: true, ..
                } => CacheUse::Verify,
                _ if cli.command.no_build_cache() => CacheUse::Off,
                _ => CacheUse::On,
            },
            worker: None,
            pending: RefCell::new(None),
            member: None,
            workspace_members: Vec::new(),
        }
    }

    /// Send every `javac` step of this build to `worker`.
    pub(super) fn with_worker(mut self, worker: Option<Arc<Worker>>) -> Session<'a> {
        self.worker = worker;
        self
    }

    pub(super) fn project(&self) -> Project<'_> {
        Project::new(&self.manifest)
    }

    /// The class directories of the workspace members this one depends on
    /// whose classpath `include` takes, nearest first: they go where their
    /// jars would, ahead of the jars (`specs/WORKSPACES.md` §4).
    pub(super) fn upstream_dirs(&self, include: impl Fn(Classpath) -> bool) -> Vec<PathBuf> {
        self.member
            .iter()
            .flat_map(|m| &m.upstream)
            .filter(|u| include(u.classpath))
            .map(|u| u.classes_dir.clone())
            .collect()
    }

    /// What the members whose classes `include` takes show a compiler:
    /// their API digests, by name, for a unit's fingerprint, so that a body
    /// change upstream leaves this member fresh (SPEC §7.2). `None` with no
    /// such member.
    pub(super) fn upstream_api(
        &self,
        include: impl Fn(Classpath) -> bool,
    ) -> Result<Option<String>> {
        let mut api = String::new();
        for upstream in self
            .member
            .iter()
            .flat_map(|m| &m.upstream)
            .filter(|u| include(u.classpath))
        {
            let _ = writeln!(
                api,
                "{} {}",
                upstream.name,
                compile::api_digest(&upstream.classes_dir)?
            );
        }
        Ok((!api.is_empty()).then_some(api))
    }

    /// A plain jar of each upstream member's classes that runs with this
    /// one, for packaging: written fresh beside its classes (in its
    /// `target/.jrs/module/`), named as its own jar is, so that a portable
    /// `lib/` or a fat jar takes it as it takes any dependency's.
    pub(super) fn module_jars(&self) -> Result<Vec<PathBuf>> {
        let mut jars = Vec::new();
        for upstream in self
            .member
            .iter()
            .flat_map(|m| &m.upstream)
            .filter(|u| u.classpath.runs())
        {
            let file = upstream.jar_path.file_name().map_or_else(
                || format!("{}-{}.jar", upstream.name, upstream.version),
                |n| n.to_string_lossy().into_owned(),
            );
            let jar = upstream
                .classes_dir
                .parent()
                .map_or_else(|| upstream.root.clone(), Path::to_path_buf)
                .join(".jrs")
                .join("module")
                .join(file);
            if let Some(dir) = jar.parent() {
                std::fs::create_dir_all(dir).path(dir)?;
            }
            package::write_thin_jar(
                &upstream.classes_dir,
                &jar,
                &JarManifest {
                    main_class: None,
                    class_path: Vec::new(),
                    attributes: Vec::new(),
                },
            )?;
            jars.push(jar);
        }
        Ok(jars)
    }

    /// A `Testing` line, which names the member in a workspace.
    pub(super) fn testing(&self, what: impl std::fmt::Display) {
        self.ui.phase("Testing", self.named(what));
    }

    /// `what`, named after this member when it is one of a workspace's: a
    /// phase line that does not say which module it is about otherwise.
    pub(super) fn named(&self, what: impl std::fmt::Display) -> String {
        if self.member.is_some() {
            format!("{}: {what}", self.manifest.name)
        } else {
            what.to_string()
        }
    }

    pub(super) fn elapsed(&self) -> String {
        ui::format_duration(self.started.elapsed())
    }

    /// `--timings`: the table on the terminal, and its copy in
    /// `target/.jrs/timings.txt`. Under `-q` the table is not printed but the
    /// file is still written, so a quiet CI run can keep the numbers.
    /// `command` names the command in the file.
    pub(super) fn report_timings(&self, command: &str) -> Result<()> {
        if !self.timings_wanted {
            return Ok(());
        }
        let rows = self.timings.rows();
        let total = self.started.elapsed();
        self.ui.timings(&rows, total);
        let work_dir = self.project().work_dir();
        std::fs::create_dir_all(&work_dir).path(&work_dir)?;
        let path = work_dir.join(timings::FILE);
        std::fs::write(&path, timings::render_file(command, &rows, total)).path(&path)?;
        self.ui.verbose(format!("wrote {}", path.display()));
        Ok(())
    }

    /// A compile unit's steps, as `compile main: kotlinc` rows.
    /// Under `-v`, how many sources a unit compiled that only compiled some.
    pub(super) fn report_recompiled(&self, unit: &CompileUnit, outcome: &compile::Outcome) {
        if let compile::Outcome::Compiled { sources, .. } = *outcome
            && sources < unit.sources.len()
        {
            self.ui.verbose(format!(
                "{} unit: compiled {sources} of {} source files, the rest are unchanged",
                unit.label,
                unit.sources.len()
            ));
        }
    }

    pub(super) fn record_steps(&self, unit: &str, steps: &[(&str, Duration)]) {
        for (compiler, took) in steps {
            self.timings
                .record(format!("compile {unit}: {compiler}"), *took);
        }
    }

    /// Train `javac`'s AOT cache for `toolchain` on a thread of its own,
    /// when there is none and, unless `always`, no command before this one
    /// looked for it (`specs/BEATING_THE_DAEMON_3.md` §2); and the
    /// `javac` worker's the same way, unless this session holds workers of
    /// its own, whose first one records it
    /// (`specs/BEATING_THE_DAEMON_4.md` §2). From JDK 25 that worker
    /// maps `javac`'s cache instead, and only its program is built (§6).
    pub(super) fn train_javac(&self, toolchain: &Toolchain, always: bool) {
        let Ok(cache) = Cache::discover() else {
            return;
        };
        if share::train_javac(&cache.root().join("cds"), toolchain, always) {
            self.ui.verbose(format!(
                "training javac's class-data cache for JDK {} beside the build",
                toolchain.version
            ));
        }
        if self.worker.is_none()
            && Worker::train_beside(cache.root().join("worker"), toolchain, always)
        {
            let what = if toolchain.version >= share::JAVAC_AOT_SINCE {
                "building the javac worker's program"
            } else {
                "training the javac worker's class-data cache"
            };
            self.ui.verbose(format!(
                "{what} for JDK {} beside the build",
                toolchain.version
            ));
        }
    }

    /// The JDK this project would build with, when it is installed: one
    /// still to be downloaded is a build's to report.
    pub(super) fn installed_toolchain(&self) -> Option<Toolchain> {
        if let Some(toolchain) = self.toolchain.get() {
            return Some(toolchain.clone());
        }
        match toolchain::project_pin(self.manifest.java.jdk, &self.manifest.root) {
            None => Toolchain::discover().ok(),
            Some(pin) => Toolchain::select_installed(&pin, &self.config.jdks)
                .ok()
                .flatten(),
        }
    }

    /// The JDK this project builds with: the one it pins, or the default.
    pub(super) fn toolchain(&self) -> Result<Toolchain> {
        if let Some(toolchain) = self.toolchain.get() {
            return Ok(toolchain.clone());
        }
        let pin = toolchain::project_pin(self.manifest.java.jdk, &self.manifest.root);
        let toolchain = match &pin {
            None => Toolchain::discover()?,
            Some(pin) => match Toolchain::select_installed(pin, &self.config.jdks)? {
                Some(toolchain) => toolchain,
                None if !self.config.jdk.download => {
                    return Err(Toolchain::missing(pin, toolchain::Missing::Install));
                }
                None if self.offline => {
                    return Err(Toolchain::missing(pin, toolchain::Missing::Offline));
                }
                None => {
                    install_jdk(self.ui, &self.config, pin.version)?;
                    Toolchain::select_installed(pin, &self.config.jdks)?
                        .ok_or_else(|| Toolchain::missing(pin, toolchain::Missing::Install))?
                }
            },
        };
        if let Some(pin) = &pin {
            self.ui.verbose(format!(
                "using JDK {} at {}, as {} asks",
                toolchain.version,
                toolchain.javac.display(),
                pin.from
            ));
        }
        Ok(self.toolchain.get_or_init(|| toolchain).clone())
    }

    /// What `--watch` keeps an eye on: the manifest, the source trees, and
    /// every task's inputs outside the target directory. `tests` adds the
    /// test sources, which `jrs run --watch` has no reason to restart for.
    pub(super) fn watched_paths(&self, tests: bool) -> Vec<PathBuf> {
        let mut paths = vec![
            self.manifest.path.clone(),
            self.manifest.source_path(),
            self.manifest.resource_path(),
        ];
        for config in &self.manifest.languages {
            paths.push(self.manifest.root.join(&config.source_dir));
        }
        if tests {
            paths.push(self.manifest.test_path());
            paths.push(self.manifest.test_resource_path());
            for config in &self.manifest.languages {
                paths.push(self.manifest.root.join(&config.test_dir));
            }
            for suite in &self.manifest.test.suites {
                paths.push(self.manifest.root.join(&suite.test_dir));
                paths.push(self.manifest.root.join(&suite.test_resource_dir));
            }
        }
        paths.extend(task::watched_inputs(&self.manifest));
        paths
    }

    pub(super) fn clean_command(&self) -> Result<i32> {
        let project = self.project();
        let target = project.target_dir();
        if project.clean()? {
            self.ui.phase("Removed", target.display());
        } else {
            self.ui
                .phase("Clean", format!("{} does not exist", target.display()));
        }
        Ok(exit::SUCCESS)
    }

    pub(super) fn build_command(&self) -> Result<i32> {
        let built = self.build()?;
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        self.ui.summary(&[
            ("build", format!("ok      {} classes", built.classes)),
            ("deps", deps_row(&built.resolution)),
            ("time", self.elapsed()),
        ]);
        self.report_timings("build")?;
        Ok(exit::SUCCESS)
    }

    pub(super) fn fetcher(&self) -> Result<Fetcher> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        // A mirror changes where bytes come from, not what the project is: the
        // manifest keeps its own URLs, so `jrs.lock` does not depend on it.
        let repositories = self
            .manifest
            .repositories
            .iter()
            .map(|r| Repository {
                url: self.config.mirror_for(&r.name, &r.url).to_string(),
                ..r.clone()
            })
            .collect();
        let credentials = self
            .manifest
            .repositories
            .iter()
            .filter_map(|r| {
                let c = self.config.credentials_for(&r.name, &env)?;
                Some((r.name.clone(), c))
            })
            .collect();
        Fetcher::with_reporter(
            repositories,
            Cache::discover()?,
            self.offline,
            Box::new(UiReporter::new(self.ui.clone())),
        )
        .with_network(Network {
            credentials,
            proxy: self.config.proxy.clone(),
            ..Network::default()
        })
    }

    /// Resolve, download, run the `pre-compile` hook, compile, copy resources,
    /// run the `post-compile` hook. Once per invocation: a second call returns
    /// the first build.
    pub(super) fn build(&self) -> Result<Built> {
        if let Some(built) = self.built.get() {
            return Ok(built.clone());
        }
        let toolchain = self.toolchain()?;
        // A clean tree compiles whatever else holds: `javac`'s cache, when
        // this JDK has none, trains beside the resolution and the downloads.
        if !self.project().classes_dir().exists() {
            self.train_javac(&toolchain, false);
        }
        let resolution = self.compile_resolution()?;
        let project = self.project();

        // Code generators run before the source tree is globbed, so what they
        // write is compiled with the rest — and a project whose sources are
        // all generated still builds.
        self.hook(Hook::PreCompile)?;
        let generated = task::generated(&self.manifest, Hook::PreCompile)?;
        let sources = project.sources(Unit::Main, &generated.sources)?;
        if sources.is_empty() {
            return Err(JrsError::build(format!(
                "no source files under {}\n\n\
                 check `project.source-dir` in {}, or run `jrs init` to scaffold one",
                display_roots(&project.roots(Unit::Main)),
                self.manifest.path.display()
            )));
        }

        let unit = self.main_unit(&sources, &resolution)?;

        let checking = Instant::now();
        if self.cache_use == CacheUse::Verify {
            compile::forget(&unit);
        }
        let stale = compile::is_stale(&unit)?;
        let outcome = if stale && let Some(outcome) = compile::restore(&unit, self.ui)? {
            self.ui.phase(
                "Restored",
                format!(
                    "{} v{} (from the build cache)",
                    self.manifest.name, self.manifest.version
                ),
            );
            self.timings.since("compile main (restored)", checking);
            outcome
        } else if stale {
            self.ui.phase(
                "Compiling",
                format!(
                    "{} v{} ({})",
                    self.manifest.name,
                    self.manifest.version,
                    sources.describe("source files")
                ),
            );
            if unit.sources.iter().any(|s| is_java(s)) {
                self.train_javac(&toolchain, false);
            }
            let scope = self
                .ui
                .spinner("Compiling", sources.describe("source files"));
            let mut steps = Vec::new();
            let result = compile::compile_timed(&toolchain, &unit, self.ui, &mut steps);
            scope.finish();
            self.record_steps("main", &steps);
            let outcome = result?;
            self.report_recompiled(&unit, &outcome);
            outcome
        } else {
            self.ui.phase(
                "Fresh",
                format!("{} v{}", self.manifest.name, self.manifest.version),
            );
            self.timings.since("compile main (fresh)", checking);
            compile::Outcome::UpToDate
        };

        let syncing = Instant::now();
        let expansion = crate::expand::Expansion::from_manifest(&self.manifest)?;
        let synced = project::sync_resources(
            &self.manifest.resource_path(),
            &project.classes_dir(),
            &project.work_dir().join("resources-main.list"),
            expansion.as_ref(),
        )?;
        if synced.copied > 0 {
            self.ui
                .verbose(format!("copied {} resources", synced.copied));
        }
        if synced.removed > 0 {
            self.ui
                .verbose(format!("removed {} deleted resources", synced.removed));
        }
        self.sync_generated(&generated.resources, &project.classes_dir(), "main")?;
        self.timings.since("resources main", syncing);
        self.hook(Hook::PostCompile)?;

        let classes = match outcome {
            compile::Outcome::Compiled { classes, .. } | compile::Outcome::Restored { classes } => {
                classes
            }
            compile::Outcome::UpToDate => {
                project::find_by_extension(&project.classes_dir(), "class")?.len()
            }
        };

        // Whatever comes after the main compile may need any jar.
        let built = Built {
            resolution: self.resolved()?,
            classes,
        };
        Ok(self.built.get_or_init(|| built).clone())
    }

    /// The main sources' unit: the classes of the members this one depends
    /// on ahead of the jars, and their API in its fingerprint, so that a
    /// body change upstream leaves this member fresh (compile avoidance
    /// across members, `specs/WORKSPACES.md` §6.2).
    fn main_unit(&self, sources: &Sources, resolution: &Resolution) -> Result<CompileUnit> {
        let mut classpath = self.upstream_dirs(Classpath::compiles);
        classpath.extend(resolution.classpath(Classpath::Compile));
        let mut unit = self.compile_unit(
            "main",
            sources,
            self.project().classes_dir(),
            classpath,
            Vec::new(),
        )?;
        let digesting = Instant::now();
        unit.main_api = self.upstream_api(Classpath::compiles)?;
        if unit.main_api.is_some() {
            self.timings.since("compile main: api digest", digesting);
        }
        // An upstream API change compiles what uses the classes that changed
        // (`specs/BEATING_THE_DAEMON.md` §3).
        unit.tracked = self.upstream_dirs(Classpath::compiles);
        Ok(unit)
    }

    /// Mirror generated resource directories into `to`, each with its own
    /// record of what it put there, as `src/main/resources` has (SPEC §7.3).
    pub(super) fn sync_generated(&self, dirs: &[PathBuf], to: &Path, unit: &str) -> Result<()> {
        let work_dir = self.project().work_dir();
        for (i, dir) in dirs.iter().enumerate() {
            let record = work_dir.join(format!("resources-{unit}-generated-{i}.list"));
            let synced = project::sync_resources(dir, to, &record, None)?;
            if synced.copied + synced.removed > 0 {
                self.ui.verbose(format!(
                    "{}: copied {}, removed {}",
                    dir.display(),
                    synced.copied,
                    synced.removed
                ));
            }
        }
        Ok(())
    }

    /// The resolved graph, once per invocation: every phase and task that
    /// needs it shares one resolution.
    pub(super) fn resolved(&self) -> Result<Resolution> {
        if let Some(resolution) = self.resolution.get() {
            return Ok(resolution.clone());
        }
        let resolution = match self.join_dependencies()? {
            Some(resolution) => resolution,
            None => self.dependencies(false)?,
        };
        Ok(self.resolution.get_or_init(|| resolution).clone())
    }

    /// The graph the main compile needs: the whole of it once it is in,
    /// or else its compile classpath, with the rest downloading behind the
    /// compile until [`resolved`](Self::resolved) joins it.
    fn compile_resolution(&self) -> Result<Resolution> {
        if let Some(resolution) = self.current_resolution() {
            return Ok(resolution);
        }
        let resolution = self.gather(false, true)?;
        if self.pending.borrow().is_some() {
            return Ok(resolution);
        }
        Ok(self.resolution.get_or_init(|| resolution).clone())
    }

    /// The compilers' graphs, resolved with the project's: in place before
    /// any compile, whichever wave of downloads is still running.
    pub(super) fn tools(&self) -> Result<Vec<Tool>> {
        if self.tools.get().is_none() {
            self.resolved()?;
        }
        Ok(self.tools.get().cloned().unwrap_or_default())
    }

    /// A compile unit for `sources`: `javac`'s settings, and the compiler of
    /// the unit's other language when it has sources in one.
    pub(super) fn compile_unit(
        &self,
        label: &str,
        sources: &Sources,
        output_dir: PathBuf,
        classpath: Vec<PathBuf>,
        friend_paths: Vec<PathBuf>,
    ) -> Result<CompileUnit> {
        let toolchain = self.toolchain()?;
        let foreign = match sources.foreign() {
            None => None,
            Some(language) => {
                let config = self.manifest.language(language).ok_or_else(|| {
                    JrsError::manifest(format!("{language} is not turned on in jrs.toml"))
                })?;
                let tool = self
                    .tools()?
                    .into_iter()
                    .find(|t| t.language == language)
                    .ok_or_else(|| {
                        JrsError::build(format!("the {language} compiler was not resolved"))
                    })?;
                let name = &self.manifest.name;
                let plugin_args =
                    crate::compile::lang::plugin_flags(&config.plugins, &tool.resolution)
                        .map_err(JrsError::build)?;
                Some(ForeignCompiler {
                    language,
                    version: config.version.clone(),
                    classpath: tool.resolution.runtime_classpath(),
                    jvm_args: config.compiler_jvm_args.clone(),
                    extra_args: config.compiler_args.clone(),
                    plugin_args,
                    module_name: if label == "test" {
                        format!("{name}_test")
                    } else {
                        name.clone()
                    },
                    friend_paths,
                    source_root: Some(
                        std::path::absolute(&self.manifest.root)
                            .unwrap_or_else(|_| self.manifest.root.clone()),
                    ),
                    color: self.ui.color(),
                })
            }
        };
        Ok(CompileUnit {
            label: label.to_string(),
            sources: sources.files.clone(),
            output_dir,
            classpath,
            release: toolchain.release(self.manifest.java.source)?,
            target: self.manifest.java.target,
            encoding: self.manifest.java.encoding.clone(),
            extra_args: self.manifest.java.javac_args.clone(),
            work_dir: self.project().work_dir(),
            foreign,
            main_api: None,
            share_dir: Cache::discover().ok().map(|c| c.root().join("cds")),
            build_cache: self.build_cache(),
            worker: self.worker.clone(),
            tracked: Vec::new(),
        })
    }

    /// The build cache (SPEC §7.8), unless `--no-build-cache`,
    /// `JRS_BUILD_CACHE=off` or `[build-cache] enabled = false` turned it
    /// off, or there is no shared cache to keep it in. Its keys name the
    /// resolved jars by coordinate and pinned checksum, so it is set up once
    /// the graphs are.
    pub(super) fn build_cache(&self) -> Option<Arc<BuildCache>> {
        // While the second wave downloads, its jars are not known by their
        // identity yet: a cache for this compile alone, not memoised.
        if self.pending.borrow().is_some() {
            return self.new_build_cache();
        }
        self.build_cache
            .get_or_init(|| self.new_build_cache())
            .clone()
    }

    /// A build cache over the graph as it is now, or `None` when it is off.
    fn new_build_cache(&self) -> Option<Arc<BuildCache>> {
        if self.cache_use == CacheUse::Off || !self.config.build_cache.enabled {
            return None;
        }
        let toolchain = self.toolchain().ok()?;
        let cache = Cache::discover().ok()?;
        let mut jars = HashMap::new();
        let mut add = |resolution: &Resolution| {
            for p in &resolution.packages {
                if let (Some(jar), Some(sum)) = (&p.jar, &p.checksum) {
                    jars.insert(jar.clone(), format!("{} {sum}", p.coord));
                }
            }
            for l in &resolution.local {
                if let (Some(jar), Some(sum)) = (&l.jar, &l.checksum) {
                    jars.insert(jar.clone(), sum.clone());
                }
            }
        };
        if let Some(resolution) = self.current_resolution() {
            add(&resolution);
        }
        for tool in self.tools.get().into_iter().flatten() {
            add(&tool.resolution);
        }
        let config = &self.config.build_cache;
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let credentials = config
            .credentials
            .as_ref()
            .and_then(|name| self.config.credentials_for(name, &env));
        let remote = match build_cache::Remote::new(
            config,
            credentials.as_ref(),
            self.config.proxy.as_ref(),
        ) {
            Ok(remote) => remote,
            Err(e) => {
                self.ui
                    .verbose(format!("build cache: no remote cache: {e}"));
                None
            }
        };
        Some(Arc::new(
            BuildCache::new(
                cache.root().join("build"),
                self.member
                    .as_ref()
                    .map_or(&self.manifest.root, |m| &m.root),
                cache.root(),
                &toolchain,
            )
            .with_jars(jars)
            .with_remote(remote)
            .verifying(self.cache_use == CacheUse::Verify),
        ))
    }

    /// The resolved graph, from the lockfile when it still matches the
    /// manifest — and with it each language's compiler graph, which
    /// `jrs.lock` pins beside the project's (`JVM_LANGUAGES.md` §5.2).
    pub(super) fn dependencies(&self, force_update: bool) -> Result<Resolution> {
        self.gather(force_update, false)
    }

    /// The graph, as [`dependencies`](Self::dependencies) works it out. With
    /// `pipelined`, only the main compile's jars and its compiler are
    /// downloaded before this returns; the rest download on a thread behind
    /// it, and the graph it returns lacks them until
    /// [`resolved`](Self::resolved) joins that thread (SPEC §8.4).
    #[allow(
        clippy::too_many_lines,
        reason = "a workspace member's preset, then the lockfile or a fresh resolution, then \
                  the downloads, in the order they happen"
    )]
    fn gather(&self, force_update: bool, pipelined: bool) -> Result<Resolution> {
        let manifest = &self.manifest;
        // A member's graph is its share of the workspace's, resolved and
        // downloaded once for every member (`specs/WORKSPACES.md` §5).
        if let Some(member) = &self.member {
            let (resolution, tools, task_tools, obfuscator, agents) = member.preset.clone();
            let _ = self.tools.set(tools);
            let _ = self.task_tools.set(task_tools);
            let _ = self.obfuscator.set(obfuscator);
            let _ = self.agents.set(agents);
            let mut resolution = resolution;
            resolution
                .warnings
                .extend(lang::resolution_warnings(manifest, &resolution));
            for warning in &resolution.warnings {
                self.ui.warn(warning);
            }
            return Ok(resolution);
        }
        let declared = manifest.dependencies.len() + manifest.dev_dependencies.len();
        let tasks_with_tools: Vec<&TaskDef> = manifest
            .tasks
            .iter()
            .filter(|t| !t.dependencies.is_empty())
            .collect();
        let pinned_agents = manifest.pinned_agents();
        if declared == 0
            && manifest.languages.is_empty()
            && tasks_with_tools.is_empty()
            && manifest.obfuscate.is_none()
            && pinned_agents.is_empty()
        {
            let _ = self.tools.set(Vec::new());
            let _ = self.task_tools.set(Vec::new());
            let _ = self.obfuscator.set(None);
            let _ = self.agents.set(Vec::new());
            return Ok(Resolution::default());
        }

        // `jrs update` asks every cached snapshot for its newest build too.
        let resolving = Instant::now();
        let fetcher = self.fetcher()?.refreshing_snapshots(force_update);
        let lock_path = manifest.lock_path();
        let existing = Lockfile::load(&lock_path)?;

        let (mut resolution, mut tools, mut task_tools, mut obfuscator, mut agents, fresh) =
            match existing {
                Some(lock) if lock.matches(manifest) && !force_update => {
                    self.ui.verbose(format!("reusing {}", lock_path.display()));
                    let (resolution, tools, task_tools, obfuscator, agents) =
                        Self::from_lock(manifest, &lock, &tasks_with_tools, &pinned_agents);
                    (resolution, tools, task_tools, obfuscator, agents, false)
                }
                _ => {
                    let what = Self::resolving_what(manifest, declared, &tasks_with_tools);
                    self.ui.phase("Resolving", &what);
                    let scope = self.ui.spinner("Resolving", &what);
                    let resolved = self.resolve_with_tools(&fetcher);
                    scope.finish();
                    let (resolution, tools, task_tools, obfuscator, agents) = resolved?;
                    (resolution, tools, task_tools, obfuscator, agents, true)
                }
            };

        self.timings.since(
            if fresh {
                "resolution"
            } else {
                "resolution (jrs.lock)"
            },
            resolving,
        );

        let background = self.download(&mut resolution, &fetcher, pipelined)?;

        self.fetch_tools(&mut tools, &fetcher)?;
        // A fresh graph is downloaded now, so that jrs.lock pins its jars'
        // checksums; one read from jrs.lock waits until its task runs, until
        // `--obfuscate` reaches the obfuscator, or until a JVM loads the agent.
        if fresh {
            for (name, graph) in &mut task_tools {
                let classpath = self.fetch_task_tool(name, graph, &fetcher)?;
                self.task_classpaths
                    .borrow_mut()
                    .insert(name.clone(), classpath);
            }
            if let Some(graph) = &mut obfuscator {
                self.fetch_obfuscator(graph, &fetcher)?;
            }
            for (name, graph) in &mut agents {
                self.fetch_agent(name, graph, &fetcher)?;
            }
        }

        let _ = self.tools.set(tools);
        let _ = self.task_tools.set(task_tools);
        let _ = self.obfuscator.set(obfuscator);
        let _ = self.agents.set(agents);
        if fresh && background.is_none() {
            self.write_lock(&lock_path, &resolution)?;
        }
        // So that `jrs cache prune` knows this project still wants these
        // artifacts. Bookkeeping: a cache that cannot record it still builds.
        if let Err(e) = fetcher.cache().register_project(&lock_path) {
            self.ui
                .verbose(format!("could not record the project in the cache: {e}"));
        }
        resolution
            .warnings
            .extend(lang::resolution_warnings(manifest, &resolution));
        for warning in &resolution.warnings {
            self.ui.warn(warning);
        }
        if let Some((thread, count)) = background {
            *self.pending.borrow_mut() = Some(Pending {
                thread,
                count,
                resolution: resolution.clone(),
                fresh,
            });
        }
        Ok(resolution)
    }

    /// Wait for the second wave of downloads, if one is running: its jars
    /// join the graph, its warnings are said, and a fresh graph's `jrs.lock`
    /// is written now that every jar's checksum is known. An error it held
    /// is reported here, as it would have been before the compile.
    fn join_dependencies(&self) -> Result<Option<Resolution>> {
        let Some(pending) = self.pending.borrow_mut().take() else {
            return Ok(None);
        };
        let waiting = Instant::now();
        // Still running: the download bars take over with what is left.
        let scope = (!pending.thread.is_finished()).then(|| self.ui.downloads(pending.count));
        let joined = pending
            .thread
            .join()
            .unwrap_or_else(|_| Err(JrsError::resolve("the downloads' thread panicked")));
        if let Some(scope) = scope {
            scope.finish();
            self.timings.since("downloads (waited for)", waiting);
        }
        let background = joined?;
        let mut resolution = pending.resolution;
        for (package, fetched) in resolution.packages.iter_mut().zip(background.packages) {
            if !package.classpath.compiles() {
                package.jar = fetched.jar;
                package.checksum = fetched.checksum;
            }
        }
        resolution.downloaded += background.downloaded;
        for warning in &background.warnings {
            self.ui.warn(warning);
        }
        resolution.warnings.extend(background.warnings);
        if pending.fresh {
            self.write_lock(&self.manifest.lock_path(), &resolution)?;
        }
        Ok(Some(resolution))
    }

    /// The graph as the build has it so far: every jar in place, or, while
    /// the second wave downloads, the main compile's.
    fn current_resolution(&self) -> Option<Resolution> {
        self.resolution
            .get()
            .cloned()
            .or_else(|| self.pending.borrow().as_ref().map(|p| p.resolution.clone()))
    }

    /// What `dependencies` resolves, read from a `jrs.lock` that still
    /// matches the manifest: the project's graph and each tool's pinned one.
    fn from_lock(
        manifest: &Manifest,
        lock: &Lockfile,
        tasks_with_tools: &[&TaskDef],
        pinned_agents: &[(String, Coord)],
    ) -> ResolvedWithTools {
        let tools = manifest
            .languages
            .iter()
            .filter_map(|c| {
                Some(Tool {
                    language: c.language,
                    resolution: lock.tool(&c.language.tool_name())?,
                })
            })
            .collect();
        let task_tools = tasks_with_tools
            .iter()
            .filter_map(|t| Some((t.name.clone(), lock.tool(&t.tool_name())?)))
            .collect();
        let obfuscator = manifest
            .obfuscate
            .as_ref()
            .and_then(|_| lock.tool(obfuscate::TOOL_NAME));
        let agents = pinned_agents
            .iter()
            .filter_map(|(name, _)| Some((name.clone(), lock.tool(name)?)))
            .collect();
        (lock.to_resolution(), tools, task_tools, obfuscator, agents)
    }

    /// Point `resolution`'s packages at their jars: the cached ones and the
    /// local ones as they are, the rest downloaded — under a `Downloading`
    /// line when there is anything to fetch and jrs is online.
    ///
    /// With `pipelined`, that is two waves (SPEC §8.4): the main compile's
    /// jars now, and the rest — runtime-only and test jars — on a thread
    /// started once the first wave is in, so that it never takes bandwidth
    /// from what the compiler waits for. The thread, and how many jars it
    /// downloads, come back; [`join_dependencies`](Self::join_dependencies)
    /// waits for it. With nothing left to download there is no thread.
    pub(super) fn download(
        &self,
        resolution: &mut Resolution,
        fetcher: &Fetcher,
        pipelined: bool,
    ) -> Result<Option<(std::thread::JoinHandle<Result<Resolution>>, usize)>> {
        let downloading = Instant::now();
        resolve::locate_cached(resolution, fetcher);
        resolve::attach_local(resolution, &self.manifest.root)?;
        let missing = |p: &resolve::ResolvedPackage| p.jar.is_none() && p.packaging != "pom";
        let total = resolution.packages.iter().filter(|p| missing(p)).count();
        let later = if pipelined && !self.offline {
            resolution
                .packages
                .iter()
                .filter(|p| missing(p) && !p.classpath.compiles())
                .count()
        } else {
            0
        };
        let first_wave = |p: &resolve::ResolvedPackage| later == 0 || p.classpath.compiles();
        if total > 0 && !self.offline {
            self.ui.phase("Downloading", format!("{total} artifacts"));
            let scope = self.ui.downloads(total - later);
            let result = resolve::fetch_jars_where(resolution, fetcher, self.jobs, first_wave);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(resolution, fetcher, self.jobs)?;
        }
        self.timings.since("downloads", downloading);
        if later == 0 {
            return Ok(None);
        }
        let mut second = resolution.clone();
        let fetcher = self.fetcher()?;
        let jobs = self.jobs;
        let thread = std::thread::spawn(move || {
            // Only what this wave has to say comes back.
            second.warnings.clear();
            resolve::fetch_jars_where(&mut second, &fetcher, jobs, |p| !p.classpath.compiles())?;
            Ok(second)
        });
        Ok(Some((thread, later)))
    }

    /// What the `Resolving` line names: the declared dependencies, and the
    /// compilers and task tools resolved beside them.
    fn resolving_what(
        manifest: &Manifest,
        declared: usize,
        tasks_with_tools: &[&TaskDef],
    ) -> String {
        let mut what = format!("{declared} declared dependencies");
        if !manifest.languages.is_empty() {
            let names: Vec<&str> = manifest
                .languages
                .iter()
                .map(|c| c.language.name())
                .collect();
            let plural = if names.len() > 1 { "s" } else { "" };
            let _ = write!(what, " and the {} compiler{plural}", names.join(" and "));
        }
        if !tasks_with_tools.is_empty() {
            let names: Vec<&str> = tasks_with_tools.iter().map(|t| t.name.as_str()).collect();
            let plural = if names.len() > 1 { "s" } else { "" };
            let _ = write!(what, " and the tools of task{plural} {}", names.join(", "));
        }
        let agents = manifest.pinned_agents().len();
        if agents > 0 {
            let plural = if agents > 1 { "s" } else { "" };
            let _ = write!(what, " and {agents} pinned java agent{plural}");
        }
        what
    }

    /// Pin a freshly resolved graph in `jrs.lock`, with the compilers', the
    /// tasks', the obfuscator's and the pinned agents' graphs beside it.
    fn write_lock(&self, lock_path: &Path, resolution: &Resolution) -> Result<()> {
        let mut lock = Lockfile::from_resolution(&self.manifest, resolution);
        for tool in self.tools.get().into_iter().flatten() {
            lock = lock.with_tool(&tool.language.tool_name(), &tool.resolution);
        }
        for (name, graph) in self.task_tools.get().into_iter().flatten() {
            if let Some(def) = self.manifest.task(name) {
                lock = lock.with_tool(&def.tool_name(), graph);
            }
        }
        if let Some(graph) = self.obfuscator.get().and_then(Option::as_ref) {
            lock = lock.with_tool(obfuscate::TOOL_NAME, graph);
        }
        for (name, graph) in self.agents.get().into_iter().flatten() {
            lock = lock.with_tool(name, graph);
        }
        if !self.workspace_members.is_empty() {
            lock = lock.with_members(&self.workspace_members);
        }
        lock.write(lock_path)?;
        self.ui.verbose(format!("wrote {}", lock_path.display()));
        Ok(())
    }

    /// Download `ProGuard`'s graph, as a task tool's is, and return its classpath.
    fn fetch_obfuscator(&self, graph: &mut Resolution, fetcher: &Fetcher) -> Result<Vec<PathBuf>> {
        let started = Instant::now();
        resolve::locate_cached(graph, fetcher);
        let missing = graph
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            self.ui.phase(
                "Downloading",
                format!("{} (obfuscator)", obfuscate::ARTIFACT),
            );
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(graph, fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(graph, fetcher, self.jobs)?;
        }
        self.timings.since("downloads (obfuscator)", started);
        for warning in &graph.warnings {
            self.ui
                .verbose(format!("{}: {warning}", obfuscate::TOOL_NAME));
        }
        Ok(graph.runtime_classpath())
    }

    /// Download a pinned java agent's graph of one, as the obfuscator's is.
    fn fetch_agent(&self, name: &str, graph: &mut Resolution, fetcher: &Fetcher) -> Result<()> {
        let started = Instant::now();
        resolve::locate_cached(graph, fetcher);
        let missing = graph
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            let artifact = graph
                .roots
                .first()
                .map_or_else(String::new, |r| r.artifact.clone());
            self.ui
                .phase("Downloading", format!("{artifact} (java agent)"));
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(graph, fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(graph, fetcher, self.jobs)?;
        }
        self.timings.since("downloads (java agents)", started);
        for warning in &graph.warnings {
            self.ui.verbose(format!("{name}: {warning}"));
        }
        Ok(())
    }

    /// The jars `<section>.java-agents` names, in its order: from the
    /// project's graph, or pinned apart from it and downloaded now if this
    /// invocation has not yet (`runner::java_agents`).
    pub(super) fn java_agents(
        &self,
        section: &str,
        agents: &[JavaAgent],
        resolution: &Resolution,
        runtime: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut pinned = Vec::new();
        if agents.iter().any(|a| matches!(a, JavaAgent::Pinned(_))) {
            self.resolved()?;
            let fetcher = self.fetcher()?;
            for (name, graph) in self.agents.get().into_iter().flatten() {
                if agents.iter().any(|a| a.tool_name(section) == *name) {
                    let mut graph = graph.clone();
                    self.fetch_agent(name, &mut graph, &fetcher)?;
                    pinned.push((name.clone(), graph));
                }
            }
        }
        runner::java_agents(section, agents, resolution, &pinned, runtime)
    }

    /// `ProGuard`'s classpath, downloaded if this invocation has not yet, or an
    /// error naming the missing `[obfuscate]` table.
    pub(super) fn obfuscator_classpath(&self) -> Result<Vec<PathBuf>> {
        self.resolved()?;
        let mut graph = self.obfuscator.get().cloned().flatten().ok_or_else(|| {
            JrsError::build(
                "the obfuscator was not resolved: add an [obfuscate] table to jrs.toml".to_string(),
            )
        })?;
        self.fetch_obfuscator(&mut graph, &self.fetcher()?)
    }

    /// Download the compilers' jars, as the test launcher's are: one line
    /// naming each compiler, then the shared download bars.
    fn fetch_tools(&self, tools: &mut [Tool], fetcher: &Fetcher) -> Result<()> {
        for tool in tools {
            let started = Instant::now();
            resolve::locate_cached(&mut tool.resolution, fetcher);
            let missing = tool
                .resolution
                .packages
                .iter()
                .filter(|p| p.jar.is_none() && p.packaging != "pom")
                .count();
            if missing > 0 && !self.offline {
                let artifact = tool
                    .resolution
                    .roots
                    .first()
                    .map_or_else(String::new, |r| r.artifact.clone());
                self.ui.phase(
                    "Downloading",
                    format!("{artifact} ({} compiler)", tool.language),
                );
                let scope = self.ui.downloads(missing);
                let result = resolve::fetch_jars(&mut tool.resolution, fetcher, self.jobs);
                scope.finish();
                result?;
            } else {
                resolve::fetch_jars(&mut tool.resolution, fetcher, self.jobs)?;
            }
            self.timings
                .since(format!("downloads ({} compiler)", tool.language), started);
            // How a compiler's own graph mediated is the compiler's business.
            for warning in &tool.resolution.warnings {
                self.ui
                    .verbose(format!("{}: {warning}", tool.language.tool_name()));
            }
        }
        Ok(())
    }

    /// The project's graph, each language's compiler as a graph apart from
    /// it, and each task's own dependencies as a graph apart from both.
    fn resolve_with_tools(&self, fetcher: &Fetcher) -> Result<ResolvedWithTools> {
        // A workspace's aggregate walks every member's paths in full: one
        // member's exclusion must not take a jar off another's classpath.
        let resolution = if self.workspace_members.is_empty() {
            resolve::resolve(&self.manifest, fetcher, self.jobs)?
        } else {
            resolve::resolve_workspace(&self.manifest, fetcher, self.jobs)?
        };
        let mut tools = Vec::new();
        for config in &self.manifest.languages {
            let roots = crate::compile::lang::compiler_roots(config);
            if !roots.is_empty() {
                tools.push(Tool {
                    language: config.language,
                    resolution: resolve::resolve_tool(&roots, fetcher, self.jobs)?,
                });
            }
        }
        let mut task_tools = Vec::new();
        for def in self
            .manifest
            .tasks
            .iter()
            .filter(|t| !t.dependencies.is_empty())
        {
            let graph = resolve::resolve_tool_dependencies(&def.dependencies, fetcher, self.jobs)?;
            task_tools.push((def.name.clone(), graph));
        }
        let obfuscator = match &self.manifest.obfuscate {
            Some(config) => Some(resolve::resolve_tool(
                &[obfuscate::coord(&config.version)],
                fetcher,
                self.jobs,
            )?),
            None => None,
        };
        let mut agents = Vec::new();
        for (name, coord) in self.manifest.pinned_agents() {
            agents.push((name, resolve::resolve_agent(&coord, fetcher, self.jobs)?));
        }
        Ok((resolution, tools, task_tools, obfuscator, agents))
    }
}

#[derive(Clone)]
pub(super) struct Built {
    pub(super) resolution: Resolution,
    pub(super) classes: usize,
}

/// The project's graph, the compilers' and the tasks' tools', the
/// obfuscator's when it is on, and each pinned agent's — freshly resolved,
/// or read from `jrs.lock`.
pub(super) type ResolvedWithTools = (
    Resolution,
    Vec<Tool>,
    Vec<(String, Resolution)>,
    Option<Resolution>,
    Vec<(String, Resolution)>,
);

/// A language's compiler, as the graph jrs resolved for it.
#[derive(Clone)]
pub(super) struct Tool {
    pub(super) language: Language,
    pub(super) resolution: Resolution,
}

/// A unit's source roots, for a message.
pub(super) fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `14      3 downloaded`, for the summary.
pub(super) fn deps_row(resolution: &Resolution) -> String {
    format!(
        "{:<7} {} downloaded",
        resolution.packages.len() + resolution.local.len(),
        resolution.downloaded
    )
}
