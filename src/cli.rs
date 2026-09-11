//! Command definitions and dispatch.
//!
//! Everything above this line is a library; this is where a build becomes a
//! sequence of phases with output attached. The rule the rest of the codebase
//! depends on holds here too: phase lines are emitted here, and the live scopes
//! only add motion, so `--progress never` produces the same transcript.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use rayon::prelude::*;

use crate::compile::lang;
use crate::compile::{self, CompileUnit, DocTool, DocUnit, ForeignCompiler, ForeignDoc, Language};
use crate::completions;
use crate::config::Config;
use crate::dist;
use crate::edit;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::image;
use crate::lockfile::Lockfile;
use crate::manifest::{
    self, Builtin, Dependency, Hook, LanguageConfig, MANIFEST_FILE, Manifest, Repository, TaskDef,
    TaskRef,
};
use crate::migrate;
use crate::model;
use crate::native_image::{self, NativeImage};
use crate::package::{self, JarManifest};
use crate::project::{self, Project, Snapshot, Sources, Unit};
use crate::resolve::cache::{Cache, Prune};
use crate::resolve::coord::{Coord, Ga};
use crate::resolve::metadata;
use crate::resolve::repo::{Fetcher, Network};
use crate::resolve::{self, Classpath, Resolution, UiReporter};
use crate::runner;
use crate::task;
use crate::test as junit;
use crate::test_report;
use crate::timings::{self, Timings};
use crate::toolchain::{self, Toolchain};
use crate::ui::{self, CharsetChoice, Style, TreeNode, Ui, UiOptions, When};

#[derive(Debug, Parser)]
#[command(
    name = "jrs",
    version,
    about = "A Java build system, in Rust",
    long_about = "A Java build system, in Rust.\n\n\
                  jrs builds, tests, runs and packages a single-module Java project \
                  from one jrs.toml file, resolving dependencies from Maven Central.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalFlags,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalFlags {
    /// Echo every subprocess command line and its exit status.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Errors only.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Fail rather than hit the network; use the cache and lockfile only.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Cap parallelism. Defaults to the number of available cores.
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub jobs: Option<usize>,

    /// Run against a manifest outside the current directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Live animated output.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub progress: WhenArg,

    /// Colour and styling.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub color: WhenArg,

    /// Glyph set for spinners, bars and trees.
    #[arg(long, global = true, value_name = "SET", default_value = "auto")]
    pub charset: CharsetArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WhenArg {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CharsetArg {
    Auto,
    Unicode,
    Ascii,
}

/// `jrs init --lang`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LangArg {
    Java,
    Kotlin,
    Scala,
    Groovy,
}

impl From<LangArg> for Language {
    fn from(l: LangArg) -> Language {
        match l {
            LangArg::Java => Language::Java,
            LangArg::Kotlin => Language::Kotlin,
            LangArg::Scala => Language::Scala,
            LangArg::Groovy => Language::Groovy,
        }
    }
}

impl From<WhenArg> for When {
    fn from(w: WhenArg) -> When {
        match w {
            WhenArg::Auto => When::Auto,
            WhenArg::Always => When::Always,
            WhenArg::Never => When::Never,
        }
    }
}

impl From<CharsetArg> for CharsetChoice {
    fn from(c: CharsetArg) -> CharsetChoice {
        match c {
            CharsetArg::Auto => CharsetChoice::Auto,
            CharsetArg::Unicode => CharsetChoice::Unicode,
            CharsetArg::Ascii => CharsetChoice::Ascii,
        }
    }
}

impl GlobalFlags {
    pub fn jobs(&self) -> usize {
        self.jobs.filter(|j| *j > 0).unwrap_or_else(default_jobs)
    }

    #[must_use]
    pub fn ui_options(&self) -> UiOptions {
        UiOptions {
            verbose: self.verbose,
            quiet: self.quiet,
            progress: self.progress.into(),
            color: self.color.into(),
            charset: self.charset.into(),
            jobs: self.jobs(),
        }
    }
}

/// `--jobs` beats the config file's `jobs`, which beats [`default_jobs`].
fn effective_jobs(flag: Option<usize>, config: Option<usize>) -> usize {
    flag.filter(|j| *j > 0)
        .or(config)
        .unwrap_or_else(default_jobs)
}

/// Cores, with a floor of 4: resolution is network-bound, and four in flight
/// beats one even on a single-core machine (SPEC §8.4).
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(4)
        .max(4)
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Resolve dependencies, compile main sources, copy resources.
    Build {
        /// Build again whenever a source, a resource or jrs.toml changes.
        #[arg(long)]
        watch: bool,
        /// Report the wall time of each phase after the summary.
        #[arg(long)]
        timings: bool,
    },

    /// Build, compile test sources, and run the test engine.
    Test(TestArgs),

    /// Build, then run the project's main class.
    Run {
        /// Arguments passed to the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
        /// Report the wall time of each phase before the program starts.
        #[arg(long)]
        timings: bool,
        /// Wait for a debugger before the program starts: JDWP on PORT, 5005
        /// unless given, on localhost unless given as HOST:PORT (`*:5005`).
        #[arg(
            long,
            value_name = "PORT",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "5005",
            value_parser = runner::DebugAddress::parse
        )]
        debug: Option<runner::DebugAddress>,
    },

    /// Build, then produce target/<name>-<version>.jar.
    Package(PackageArgs),

    /// Generate API documentation into target/doc with javadoc.
    Doc,

    /// Run a task from jrs.toml's [tasks], after whatever it depends on.
    Task(TaskArgs),

    /// Remove the target directory.
    Clean,

    /// Print the resolved dependency graph.
    Tree {
        /// Show this many levels of dependencies; 1 is the declared ones only.
        #[arg(long, value_name = "N")]
        depth: Option<usize>,
        /// Show why a dependency is in the graph: every path that leads to it.
        #[arg(long, value_name = "ARTIFACT")]
        why: Option<String>,
        /// Show a compiler's own graph instead: `kotlin-compiler`,
        /// `scala-compiler` or `groovy-compiler`.
        #[arg(long, value_name = "NAME", conflicts_with = "why")]
        tool: Option<String>,
    },

    /// Print the resolved classpath, for editors and ad-hoc `java` runs.
    Classpath {
        /// The test classpath instead: test classes, main classes, every jar.
        #[arg(long)]
        test: bool,
        /// The runtime classpath: without compile-only dependencies, and with
        /// runtime-only ones.
        #[arg(long, conflicts_with = "test")]
        runtime: bool,
    },

    /// Re-resolve dependencies and rewrite jrs.lock.
    Update,

    /// Re-hash the cached dependency jars against the checksums in jrs.lock.
    Verify,

    /// List declared dependencies that have newer releases.
    Outdated,

    /// Add dependencies to jrs.toml, at their newest release unless a version
    /// is given.
    Add {
        /// `group:artifact`, `group:artifact:version`, or
        /// `group:artifact:version:classifier`.
        #[arg(value_name = "COORDINATE", required = true)]
        coordinates: Vec<String>,
        /// Add them to [dev-dependencies].
        #[arg(long)]
        dev: bool,
        /// On the compile classpath only, not at runtime.
        #[arg(long, conflicts_with = "dev")]
        compile_only: bool,
        /// At runtime and in tests, but not on the classpath the main sources
        /// compile against.
        #[arg(long, conflicts_with_all = ["dev", "compile_only"])]
        runtime_only: bool,
    },

    /// Remove dependencies from jrs.toml.
    Remove {
        /// `group:artifact` (or `group:artifact:classifier`), as jrs.toml names it.
        #[arg(value_name = "KEY", required = true)]
        keys: Vec<String>,
        /// Only look in [dev-dependencies].
        #[arg(long)]
        dev: bool,
    },

    /// Inspect or prune the shared dependency cache.
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },

    /// Scaffold jrs.toml, a starter class and a starter test.
    Init {
        /// Project name. Defaults to the directory name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// A library: no main class, and a starter library class instead.
        #[arg(long)]
        lib: bool,
        /// The language of the starter code and its test.
        #[arg(long, value_name = "LANG", default_value = "java")]
        lang: LangArg,
        /// Where to scaffold. Defaults to the current directory.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },

    /// Generate jrs.toml from an existing pom.xml or Gradle build.
    Migrate {
        /// Force the source build system instead of detecting it.
        #[arg(long, value_name = "SYSTEM")]
        from: Option<String>,
        /// Print the manifest that would be written; touch nothing.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing jrs.toml.
        #[arg(long)]
        force: bool,
        /// Project root to migrate. Defaults to the current directory.
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },

    /// Print a shell completion script.
    Completions {
        #[arg(value_name = "SHELL", value_parser = ["bash", "zsh", "fish"])]
        shell: String,
    },

    /// Print the project model as JSON, for editors and tools.
    Metadata {
        /// Leave the dependencies out: nothing is resolved, and `classpaths`
        /// is null.
        #[arg(long)]
        no_deps: bool,
    },

    /// Download the dependencies into the cache without building.
    Fetch {
        /// Also download each dependency's -sources.jar, for editors.
        #[arg(long)]
        sources: bool,
    },
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    /// The task, as `[tasks.<name>]` names it.
    #[arg(value_name = "NAME", required_unless_present = "list")]
    pub name: Option<String>,
    /// List the tasks, their descriptions and the hooks that run them.
    #[arg(long, conflicts_with = "name")]
    pub list: bool,
    /// Run the task again whenever its inputs, a source or jrs.toml changes.
    #[arg(long, conflicts_with = "list")]
    pub watch: bool,
    /// Arguments appended to the task's own command line, after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub args: Vec<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "command-line switches, each independent of the others"
)]
#[derive(Debug, Default, Args)]
pub struct TestArgs {
    /// Only run classes matching this regular expression.
    #[arg(long, value_name = "PATTERN")]
    pub filter: Option<String>,
    /// Only run tests with this tag (a `JUnit` tag expression); repeatable.
    #[arg(long, value_name = "TAG")]
    pub include_tag: Vec<String>,
    /// Skip tests with this tag (a `JUnit` tag expression); repeatable.
    #[arg(long, value_name = "TAG")]
    pub exclude_tag: Vec<String>,
    /// Run one test method, as `com.example.FooTest#bar`; repeatable.
    #[arg(long, value_name = "CLASS#METHOD")]
    pub method: Vec<String>,
    /// Record coverage with `JaCoCo`; the report lands in target/coverage.
    #[arg(long)]
    pub coverage: bool,
    /// Test again whenever a source, a resource or jrs.toml changes.
    #[arg(long)]
    pub watch: bool,
    /// Report the wall time of each phase after the run.
    #[arg(long)]
    pub timings: bool,
    /// Wait for a debugger before the tests start: JDWP on PORT, 5005 unless
    /// given, on localhost unless given as HOST:PORT (`*:5005`).
    #[arg(
        long,
        value_name = "PORT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "5005",
        value_parser = runner::DebugAddress::parse
    )]
    pub debug: Option<runner::DebugAddress>,
    /// Run only the tests that failed last time, as its `JUnit` XML records them.
    #[arg(
        long,
        conflicts_with_all = ["filter", "include_tag", "exclude_tag", "method", "watch"]
    )]
    pub rerun_failed: bool,
    /// Stop at the first failing test.
    #[arg(long)]
    pub fail_fast: bool,
    /// Run failing tests again up to N times; overrides `[test] retries`.
    #[arg(long, value_name = "N")]
    pub retries: Option<u32>,
}

#[derive(Debug, Default, Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent command-line flag, as clap parses them"
)]
pub struct PackageArgs {
    /// Unpack every runtime dependency into the jar.
    #[arg(long, conflicts_with = "portable")]
    pub fat: bool,
    /// Copy the runtime dependencies into target/lib/ and point the jar's
    /// Class-Path at them, so the pair can be shipped together.
    #[arg(long)]
    pub portable: bool,
    /// Also build a trimmed runtime image with jlink, in target/image.
    #[arg(long)]
    pub jlink: bool,
    /// Also build a native package with jpackage, in target/jpackage. TYPE is
    /// jpackage's own: app-image, dmg, pkg, deb, rpm, exe or msi.
    #[arg(long, value_name = "TYPE", num_args = 0..=1)]
    pub jpackage: Option<Option<String>>,
    /// Report the wall time of each phase after the summary.
    #[arg(long)]
    pub timings: bool,
    /// Also write target/<name>-<version>-sources.jar from the main sources,
    /// generated ones included.
    #[arg(long)]
    pub sources: bool,
    /// Also run `jrs doc` and jar target/doc into
    /// target/<name>-<version>-javadoc.jar.
    #[arg(long)]
    pub javadoc: bool,
    /// Also write a distribution with launch scripts in target/dist, zipped
    /// into target/<name>-<version>.zip.
    #[arg(long)]
    pub dist: bool,
    /// Also build a native executable in target/native with `native-image`,
    /// which needs a `GraalVM` JDK.
    #[arg(long)]
    pub native_image: bool,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Print where the cache is.
    Path,
    /// Remove cached artifacts that no project's jrs.lock uses any more.
    Prune {
        /// Instead, remove whatever no build has used for this many days.
        #[arg(long, value_name = "DAYS")]
        unused_for: Option<u64>,
        /// Say what would be removed; remove nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

impl Command {
    /// Whether the command was given `--timings`, which `build`, `test`,
    /// `run` and `package` take.
    #[must_use]
    pub fn timings(&self) -> bool {
        match self {
            Command::Build { timings, .. } | Command::Run { timings, .. } => *timings,
            Command::Test(args) => args.timings,
            Command::Package(args) => args.timings,
            _ => false,
        }
    }
}

/// Parse arguments, run the command, and turn the result into an exit code.
#[must_use]
pub fn main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => exit::SUCCESS,
                _ => exit::USAGE,
            };
        }
    };

    let ui = Ui::new(cli.global.ui_options());
    match dispatch(&cli, &ui) {
        Ok(code) => code,
        Err(error) => {
            report(&ui, &error);
            error.exit_code()
        }
    }
}

/// Run one command line against a `Ui` the caller made, and return the exit
/// code. This is how the integration tests drive whole commands — hooks and
/// all — through a captured `Ui`, without spawning the binary.
pub fn run_with<I, T>(args: I, ui: &Ui) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            ui.error(e.to_string().trim_end());
            return exit::USAGE;
        }
    };
    match dispatch(&cli, ui) {
        Ok(code) => code,
        Err(error) => {
            report(ui, &error);
            error.exit_code()
        }
    }
}

/// The live region comes down before any diagnostic is printed.
fn report(ui: &Ui, error: &JrsError) {
    ui.suspend();
    ui.error(error.to_string());
}

fn dispatch(cli: &Cli, ui: &Ui) -> Result<i32> {
    // Commands that need no manifest, or that manage their own sessions.
    match &cli.command {
        Command::Init {
            name,
            lib,
            lang,
            path,
        } => {
            return init(ui, name.as_deref(), *lib, (*lang).into(), path.as_deref());
        }
        Command::Migrate {
            from,
            dry_run,
            force,
            path,
        } => {
            return migrate_command(ui, from.as_deref(), *dry_run, *force, path.as_deref());
        }
        Command::Completions { shell } => return completions_command(ui, shell),
        Command::Cache { action } => return cache_command(cli, ui, action),
        Command::Add {
            coordinates,
            dev,
            compile_only,
            runtime_only,
        } => {
            return add_command(cli, ui, coordinates, *dev, (*compile_only, *runtime_only));
        }
        Command::Remove { keys, dev } => return remove_command(cli, ui, keys, *dev),
        #[allow(
            clippy::redundant_closure_for_method_calls,
            reason = "`Session::build_command` does not satisfy watch's higher-ranked bound"
        )]
        Command::Build { watch: true, .. } => return watch(cli, ui, |s| s.build_command()),
        Command::Test(args) if args.watch => return watch(cli, ui, |s| s.test_command(args)),
        Command::Task(args) if args.watch => return watch(cli, ui, |s| s.task_command(args)),
        _ => {}
    }

    let session = Session::open(cli, ui)?;
    match &cli.command {
        Command::Build { .. } => session.build_command(),
        Command::Test(args) => session.test_command(args),
        Command::Run { args, debug, .. } => session.run_command(args, debug.as_ref()),
        Command::Package(args) => session.package_command(args),
        Command::Doc => session.doc_command(),
        Command::Task(args) => session.task_command(args),
        Command::Clean => session.clean_command(),
        Command::Tree { depth, why, tool } => {
            session.tree_command(*depth, why.as_deref(), tool.as_deref())
        }
        Command::Classpath { test, runtime } => session.classpath_command(*test, *runtime),
        Command::Update => session.update_command(),
        Command::Verify => session.verify_command(),
        Command::Outdated => session.outdated_command(),
        Command::Metadata { no_deps } => session.metadata_command(*no_deps),
        Command::Fetch { sources } => session.fetch_command(*sources),
        Command::Init { .. }
        | Command::Migrate { .. }
        | Command::Completions { .. }
        | Command::Cache { .. }
        | Command::Add { .. }
        | Command::Remove { .. } => unreachable!("handled above"),
    }
}

/// `1 test`, `2 tests`.
fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Where the manifest for this invocation is, per `--manifest-path` or by
/// walking up from the working directory.
fn manifest_path(cli: &Cli) -> Result<PathBuf> {
    match &cli.global.manifest_path {
        Some(p) if p.is_dir() => Ok(p.join(MANIFEST_FILE)),
        Some(p) => Ok(p.clone()),
        None => Manifest::discover(Path::new(".")),
    }
}

/// One command's worth of state: the manifest, the user's configuration, the
/// UI, and the clock — and what has already happened, since a task and a
/// built-in command each run at most once per invocation, however many hooks
/// and `depends-on` lists reach them.
struct Session<'a> {
    manifest: Manifest,
    ui: &'a Ui,
    config: Config,
    jobs: usize,
    offline: bool,
    started: Instant,
    toolchain: OnceCell<Toolchain>,
    resolution: OnceCell<Resolution>,
    /// Each language's compiler graph, resolved and pinned with the project's.
    tools: OnceCell<Vec<Tool>>,
    built: OnceCell<Built>,
    /// Tasks already run (or found fresh) in this invocation.
    ran: RefCell<HashSet<String>>,
    /// Built-ins a `depends-on` has already run.
    done: RefCell<HashSet<Builtin>>,
    /// The jar, once `package` has written it.
    jar: RefCell<Option<PathBuf>>,
    /// The wall time of each phase, for `--timings`.
    timings: Timings,
    /// Whether the command asked for `--timings`.
    timings_wanted: bool,
}

impl<'a> Session<'a> {
    fn open(cli: &Cli, ui: &'a Ui) -> Result<Session<'a>> {
        let manifest = Manifest::load(manifest_path(cli)?)?;
        for warning in &manifest.warnings {
            ui.warn(warning);
        }
        let config = Config::load()?;
        for warning in &config.warnings {
            ui.warn(warning);
        }
        Ok(Session {
            manifest,
            ui,
            jobs: effective_jobs(cli.global.jobs, config.jobs),
            config,
            offline: cli.global.offline,
            started: Instant::now(),
            toolchain: OnceCell::new(),
            resolution: OnceCell::new(),
            tools: OnceCell::new(),
            built: OnceCell::new(),
            ran: RefCell::new(HashSet::new()),
            done: RefCell::new(HashSet::new()),
            jar: RefCell::new(None),
            timings: Timings::new(),
            timings_wanted: cli.command.timings(),
        })
    }

    fn project(&self) -> Project<'_> {
        Project::new(&self.manifest)
    }

    fn elapsed(&self) -> String {
        ui::format_duration(self.started.elapsed())
    }

    /// `--timings`: the table on the terminal, and its copy in
    /// `target/.jrs/timings.txt`. Under `-q` the table is not printed but the
    /// file is still written, so a quiet CI run can keep the numbers.
    /// `command` names the command in the file.
    fn report_timings(&self, command: &str) -> Result<()> {
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
    fn record_steps(&self, unit: &str, steps: &[(&str, Duration)]) {
        for (compiler, took) in steps {
            self.timings
                .record(format!("compile {unit}: {compiler}"), *took);
        }
    }

    /// The JDK this project builds with: the one it pins, or the default.
    fn toolchain(&self) -> Result<Toolchain> {
        if let Some(toolchain) = self.toolchain.get() {
            return Ok(toolchain.clone());
        }
        let pin = toolchain::project_pin(self.manifest.java.jdk, &self.manifest.root);
        let toolchain = Toolchain::select(pin.as_ref(), &self.config.jdks)?;
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
    /// every task's inputs outside the target directory.
    fn watched_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.manifest.path.clone(),
            self.manifest.source_path(),
            self.manifest.resource_path(),
            self.manifest.test_path(),
            self.manifest.test_resource_path(),
        ];
        for config in &self.manifest.languages {
            paths.push(self.manifest.root.join(&config.source_dir));
            paths.push(self.manifest.root.join(&config.test_dir));
        }
        paths.extend(task::watched_inputs(&self.manifest));
        paths
    }

    // ---- commands ---------------------------------------------------------

    fn clean_command(&self) -> Result<i32> {
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

    fn build_command(&self) -> Result<i32> {
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

    fn run_command(&self, args: &[String], debug: Option<&runner::DebugAddress>) -> Result<i32> {
        let main_class = self.manifest.require_main_class("run")?.to_string();
        let built = self.build()?;
        self.check_main_class(&main_class)?;
        self.hook(Hook::PreRun)?;
        let toolchain = self.toolchain()?;

        let mut classpath = vec![self.project().classes_dir()];
        classpath.extend(built.resolution.runtime_classpath());
        let mut agents = runner::java_agents(
            "run",
            &self.manifest.run.java_agents,
            &built.resolution,
            true,
        )?;
        let environment = self.jvm_environment(
            "run",
            &self.manifest.run.env,
            self.manifest.run.cwd.as_ref(),
        )?;
        if environment.cwd.is_some() {
            // Relative paths would be read from the other directory.
            let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
            classpath = classpath.into_iter().map(absolute).collect();
            agents = agents.into_iter().map(absolute).collect();
        }
        let mut jvm_args = runner::jvm_prefix(debug, &agents);
        jvm_args.extend(self.manifest.run.jvm_args.iter().cloned());

        // The program's own run is not a build phase: the report covers what
        // led up to it, and comes before the program gets the terminal.
        self.report_timings("run")?;
        self.ui.phase("Running", &main_class);
        if let Some(debug) = debug {
            self.announce_debugger(debug);
        }
        let code = runner::run_main(
            &toolchain,
            &jvm_args,
            &classpath,
            &main_class,
            args,
            &environment,
            self.ui,
        )?;
        if code != 0 {
            self.ui
                .phase("Finished", format!("{main_class} exited with {code}"));
        }
        Ok(code)
    }

    /// A Kotlin `main` at file level compiles to `<File>Kt`, so a `main-class`
    /// naming the file's class instead is the likeliest slip in a Kotlin
    /// project. When there is no such class but there is its `Kt` one, say so
    /// rather than let `java` report a missing class.
    fn check_main_class(&self, main_class: &str) -> Result<()> {
        let classes = self.project().classes_dir();
        let kt = format!("{main_class}Kt");
        if compile::class_file(&classes, main_class).is_file()
            || !compile::class_file(&classes, &kt).is_file()
        {
            return Ok(());
        }
        Err(JrsError::manifest(format!(
            "there is no class `{main_class}` in {}, but there is `{kt}`: a Kotlin `main` \
             function at file level compiles to a class named after its file\n\n\
             set it in {}:\n\n    [project]\n    main-class = \"{kt}\"",
            classes.display(),
            self.manifest.path.display()
        )))
    }

    /// `<section>.env` and `<section>.cwd`, expanded for the JVM `jrs run` or
    /// `jrs test` starts.
    fn jvm_environment(
        &self,
        section: &str,
        env: &[(String, manifest::Template)],
        cwd: Option<&manifest::Template>,
    ) -> Result<toolchain::Environment> {
        let vars = if env.is_empty() {
            Vec::new()
        } else {
            task::jvm_env(&self.manifest, env, &self.classpaths()?)?
        };
        let cwd = match cwd {
            None => None,
            Some(template) => {
                let dir = task::jvm_cwd(&self.manifest, template)?;
                if !dir.is_dir() {
                    return Err(JrsError::manifest(format!(
                        "`{section}.cwd` is {}, which is not a directory",
                        dir.display()
                    )));
                }
                Some(dir)
            }
        };
        Ok(toolchain::Environment { cwd, vars })
    }

    /// Say where to attach before a JVM starts waiting for a debugger, since
    /// nothing else happens until one does.
    fn announce_debugger(&self, debug: &runner::DebugAddress) {
        self.ui.phase(
            "Debugging",
            format!(
                "attach a debugger to {}; the JVM waits until one does",
                debug.attach_to()
            ),
        );
    }

    /// `run.java-agents` as the image's launchers name them: `lib/<file>`,
    /// relative to the application directory, where the portable layout put
    /// each jar.
    fn image_agents(&self, lib_dir: Option<&Path>) -> Result<Vec<String>> {
        let wanted = &self.manifest.run.java_agents;
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let Some(lib_dir) = lib_dir else {
            return Err(JrsError::manifest(
                "`run.java-agents` cannot go into an image or a distribution of a fat \
                 jar: an agent loads from a jar of its own, and the fat jar has \
                 unpacked it\n\n\
                 leave out --fat, and the portable layout is used, with each agent in \
                 its lib/",
            ));
        };
        let resolution = self.resolved()?;
        let jars = runner::java_agents("run", wanted, &resolution, true)?;
        Ok(wanted
            .iter()
            .zip(jars)
            .map(|(ga, jar)| {
                let name = jar
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // `copy_libraries` prefixes a file name two groups share.
                let prefixed = format!("{}.{name}", ga.group);
                let file = if lib_dir.join(&prefixed).is_file() {
                    prefixed
                } else {
                    name
                };
                format!("lib/{file}")
            })
            .collect())
    }

    fn package_command(&self, args: &PackageArgs) -> Result<i32> {
        let mut rows = self.package(args)?;
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        rows.push(("time", self.elapsed()));
        self.ui.summary(&rows);
        self.report_timings("package")?;
        Ok(exit::SUCCESS)
    }

    /// Build, write the jar (and any image), then the `post-package` hook.
    /// Returns the summary rows.
    #[allow(
        clippy::too_many_lines,
        reason = "the checks every flag needs before the build, then one arm per jar kind; \
                  images and the other artifacts are already helpers"
    )]
    fn package(&self, args: &PackageArgs) -> Result<Vec<(&'static str, String)>> {
        let images = args.jlink || args.jpackage.is_some();
        if images {
            // An image starts its application with `java -jar`.
            let command = if args.jlink {
                "package --jlink"
            } else {
                "package --jpackage"
            };
            self.manifest.require_main_class(command)?;
        }
        if args.dist {
            self.manifest.require_main_class("package --dist")?;
        }
        if args.native_image {
            self.manifest.require_main_class("package --native-image")?;
            // Before anything is built: without GraalVM nothing can use it.
            native_image::find(&self.toolchain()?)?;
        }
        let attributes = self.manifest.jar_attributes()?;
        let built = self.build()?;
        let project = self.project();
        let output = project.jar_path();
        let runtime = built.resolution.runtime_classpath();
        // An image or a distribution needs a jar that runs anywhere: the fat
        // one when asked for, the portable layout otherwise.
        let portable = args.portable || ((images || args.dist) && !args.fat);
        let lib_dir = project.target_dir().join("lib");

        let packaging = Instant::now();
        let outcome = if args.fat {
            self.ui.phase(
                "Packaging",
                format!("{} (fat, {} dependencies)", output.display(), runtime.len()),
            );
            package::write_fat_jar(
                &project.classes_dir(),
                &runtime,
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path: Vec::new(),
                    attributes,
                },
            )?
        } else if portable {
            self.ui.phase(
                "Packaging",
                format!(
                    "{} (portable, {} dependencies in {})",
                    output.display(),
                    runtime.len(),
                    lib_dir.display()
                ),
            );
            let libraries: Vec<(String, PathBuf)> = runtime
                .iter()
                .map(|jar| {
                    let group = built
                        .resolution
                        .packages
                        .iter()
                        .find(|p| p.jar.as_ref() == Some(jar))
                        .map(|p| p.coord.group.clone())
                        .unwrap_or_default();
                    (group, jar.clone())
                })
                .collect();
            let class_path = package::copy_libraries(&libraries, &lib_dir, "lib")?;
            package::write_thin_jar(
                &project.classes_dir(),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path,
                    attributes,
                },
            )?
        } else {
            self.ui.phase("Packaging", output.display());
            // A thin jar points at the cached dependency jars, so `java -jar`
            // works without a classpath argument on this machine.
            let class_path = runtime
                .iter()
                .map(|p| package::class_path_entry(p))
                .collect();
            package::write_thin_jar(
                &project.classes_dir(),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path,
                    attributes,
                },
            )?
        };
        self.timings.since("packaging", packaging);
        for warning in &outcome.warnings {
            self.ui.warn(warning);
        }

        let mut rows = vec![
            ("build", format!("ok      {} classes", built.classes)),
            ("deps", deps_row(&built.resolution)),
            (
                "jar",
                format!(
                    "{}   {}",
                    self.manifest.jar_name(),
                    ui::format_bytes(outcome.bytes)
                ),
            ),
        ];
        if images {
            rows.extend(self.images(args, &output, portable.then_some(lib_dir.as_path()))?);
        }
        rows.extend(self.package_extras(
            args,
            &output,
            portable.then_some(lib_dir.as_path()),
            &built,
        )?);

        *self.jar.borrow_mut() = Some(output);
        self.hook(Hook::PostPackage)?;
        Ok(rows)
    }

    /// `--jlink` and `--jpackage`: runtime images of the packaged jar.
    fn images(
        &self,
        args: &PackageArgs,
        jar: &Path,
        lib_dir: Option<&Path>,
    ) -> Result<Vec<(&'static str, String)>> {
        let toolchain = self.toolchain()?;
        let project = self.project();
        let agents = self.image_agents(lib_dir)?;
        let app = image::App {
            name: &self.manifest.name,
            version: &self.manifest.version,
            main_class: self.manifest.main_class.as_deref(),
            jar,
            lib_dir,
            jvm_args: &self.manifest.run.jvm_args,
            java_agents: &agents,
        };

        self.ui.phase("Analysing", "module dependencies with jdeps");
        let started = Instant::now();
        let scope = self.ui.spinner("Analysing", "module dependencies");
        let modules = app.jars().and_then(|jars| {
            image::modules(
                &toolchain,
                &jars,
                toolchain.release(self.manifest.java.source)?,
                &self.manifest.package.add_modules,
                self.ui,
            )
        });
        scope.finish();
        self.timings.since("jdeps", started);
        let modules = modules?;
        self.ui.verbose(format!("modules: {}", modules.join(",")));

        let mut rows = Vec::new();
        if args.jlink {
            let output = project.target_dir().join("image");
            self.ui.phase(
                "Linking",
                format!("{} ({} modules)", output.display(), modules.len()),
            );
            let started = Instant::now();
            let scope = self.ui.spinner("Linking", "a runtime image");
            let linked = image::jlink(&toolchain, &app, &modules, &output, self.ui);
            scope.finish();
            self.timings.since("jlink", started);
            let linked = linked?;
            rows.push((
                "image",
                format!(
                    "{}   {}",
                    linked.path.display(),
                    ui::format_bytes(linked.bytes)
                ),
            ));
        }
        if let Some(kind) = &args.jpackage {
            let dest = project.target_dir().join("jpackage");
            self.ui.phase(
                "Bundling",
                format!(
                    "{} ({})",
                    dest.display(),
                    kind.as_deref().unwrap_or("the platform's default package")
                ),
            );
            let started = Instant::now();
            let scope = self.ui.spinner("Bundling", "with jpackage");
            let bundled = image::jpackage(
                &toolchain,
                &app,
                &modules,
                kind.as_deref(),
                &dest,
                &project.work_dir(),
                self.ui,
            );
            scope.finish();
            self.timings.since("jpackage", started);
            let bundled = bundled?;
            self.ui.phase("Bundled", bundled.display());
            rows.push(("package", bundled.display().to_string()));
        }
        Ok(rows)
    }

    /// `--sources`, `--javadoc`, `--dist` and `--native-image`: what
    /// `package` writes beside the jar, from the jar (and `lib/`) just
    /// written. Returns their summary rows.
    fn package_extras(
        &self,
        args: &PackageArgs,
        jar: &Path,
        lib_dir: Option<&Path>,
        built: &Built,
    ) -> Result<Vec<(&'static str, String)>> {
        let project = self.project();
        let target = project.target_dir();
        let base = format!("{}-{}", self.manifest.name, self.manifest.version);
        let row = |path: &Path, bytes: u64| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            format!("{name}   {}", ui::format_bytes(bytes))
        };
        let mut rows = Vec::new();

        if args.sources {
            // What the build compiled: every main root, and what the
            // `pre-compile` tasks generated.
            let generated = task::generated(&self.manifest, Hook::PreCompile)?;
            let sources = project.sources(Unit::Main, &generated.sources)?;
            let mut roots = project.roots(Unit::Main);
            roots.extend(generated.sources);
            let output = target.join(format!("{base}-sources.jar"));
            self.ui.phase(
                "Packaging",
                format!(
                    "{} ({})",
                    output.display(),
                    sources.describe("source files")
                ),
            );
            let outcome = package::write_sources_jar(&roots, &sources.files, &output)?;
            rows.push(("sources", row(&output, outcome.bytes)));
        }

        if args.javadoc {
            // Once per invocation, however a task's `depends-on` reached it.
            self.builtin(Builtin::Doc)?;
            let output = target.join(format!("{base}-javadoc.jar"));
            self.ui.phase("Packaging", output.display());
            let outcome = package::write_javadoc_jar(&target.join("doc"), &output)?;
            rows.push(("javadoc", row(&output, outcome.bytes)));
        }

        if args.dist {
            let agents = self.image_agents(lib_dir)?;
            let app = image::App {
                name: &self.manifest.name,
                version: &self.manifest.version,
                main_class: self.manifest.main_class.as_deref(),
                jar,
                lib_dir,
                jvm_args: &self.manifest.run.jvm_args,
                java_agents: &agents,
            };
            let zip = target.join(format!("{base}.zip"));
            self.ui.phase(
                "Archiving",
                format!(
                    "{} (launchers in {})",
                    zip.display(),
                    target.join("dist").join(&base).join("bin").display()
                ),
            );
            let outcome = dist::write_dist(&app, &target.join("dist"), &zip)?;
            rows.push(("dist", row(&outcome.zip, outcome.bytes)));
        }

        if args.native_image {
            let toolchain = self.toolchain()?;
            let tool = native_image::find(&toolchain)?;
            let main_class = self.manifest.require_main_class("package --native-image")?;
            let mut classpath = vec![project.classes_dir()];
            classpath.extend(built.resolution.runtime_classpath());
            let image = NativeImage {
                name: &self.manifest.name,
                main_class,
                classpath: &classpath,
                extra_args: &self.manifest.package.native_image_args,
            };
            let output_dir = target.join("native");
            self.ui.phase(
                "Building",
                format!("a native image of {main_class} in {}", output_dir.display()),
            );
            let scope = self.ui.spinner("Building", "a native image");
            let result =
                native_image::build(&tool, &image, &output_dir, &project.work_dir(), self.ui);
            scope.finish();
            let executable = result?;
            let bytes = std::fs::metadata(&executable).path(&executable)?.len();
            rows.push(("native", row(&executable, bytes)));
        }
        Ok(rows)
    }

    fn test_command(&self, args: &TestArgs) -> Result<i32> {
        let Some(outcome) = self.test(args)? else {
            self.report_timings("test")?;
            return Ok(exit::SUCCESS);
        };
        self.ui.phase(
            "Finished",
            format!("{} in {}", outcome.describe(), self.elapsed()),
        );
        // A failing run is timed too: how long the tests took is part of it.
        self.report_timings("test")?;
        if outcome.ok() && !outcome.coverage_shortfalls.is_empty() {
            let lines: Vec<String> = outcome
                .coverage_shortfalls
                .iter()
                .map(|s| format!("    {s}"))
                .collect();
            return Err(JrsError::test(format!(
                "coverage is below `test.coverage-minimum` in jrs.toml\n\n{}",
                lines.join("\n")
            )));
        }
        if outcome.ok() {
            Ok(exit::SUCCESS)
        } else {
            // The launcher already printed the failures verbatim; do not restate
            // them, only say that the run failed.
            Err(JrsError::test("tests failed"))
        }
    }

    /// Build, compile the tests, run them, then — only if they passed — the
    /// `post-test` hook. `None` when there are no tests to run.
    #[allow(
        clippy::too_many_lines,
        reason = "the test phases in the order they run, each already a helper or a \
                  library call; the extra lines are their --timings rows"
    )]
    fn test(&self, args: &TestArgs) -> Result<Option<junit::TestOutcome>> {
        // `--rerun-failed` reads the last run's reports before anything else:
        // this run is about to replace them.
        let rerun = if args.rerun_failed {
            match self.last_failures()? {
                Some(rerun) => Some(rerun),
                None => return Ok(None),
            }
        } else {
            None
        };
        let built = self.build()?;
        self.hook(Hook::PreTest)?;
        let project = self.project();
        let toolchain = self.toolchain()?;

        let generated = task::generated(&self.manifest, Hook::PreTest)?;
        let sources = project.sources(Unit::Test, &generated.sources)?;
        if sources.is_empty() {
            self.ui.phase(
                "Testing",
                format!(
                    "no tests found under {}",
                    display_roots(&project.roots(Unit::Test))
                ),
            );
            return Ok(None);
        }

        // Tests compile against the main classes plus the test classpath, and
        // Kotlin tests may use the main module's `internal` declarations.
        let mut classpath = vec![project.classes_dir()];
        classpath.extend(built.resolution.classpath(Classpath::Test));
        let mut unit = self.compile_unit(
            "test",
            &sources,
            project.test_classes_dir(),
            classpath.clone(),
            vec![project.classes_dir()],
        )?;
        let checking = Instant::now();
        // Compile avoidance: the tests see the main classes' API, not their
        // bytes, so a changed method body leaves them fresh (SPEC §7.2).
        unit.main_api = Some(compile::api_digest(&project.classes_dir())?);
        if compile::is_stale(&unit)? {
            let what = sources.describe("test sources");
            self.ui.phase("Compiling", &what);
            let scope = self.ui.spinner("Compiling", &what);
            let mut steps = Vec::new();
            let result = compile::compile_timed(&toolchain, &unit, self.ui, &mut steps);
            scope.finish();
            self.record_steps("test", &steps);
            result?;
        } else {
            self.timings.since("compile test (fresh)", checking);
        }
        let syncing = Instant::now();
        project::sync_resources(
            &self.manifest.test_resource_path(),
            &project.test_classes_dir(),
            &project.work_dir().join("resources-test.list"),
        )?;
        self.sync_generated(&generated.resources, &project.test_classes_dir(), "test")?;
        self.timings.since("resources test", syncing);

        // The launcher, and JaCoCo when coverage is on, are internal
        // dependencies: resolved by jrs, never on the user's own classpath.
        let launcher = junit::launcher_coordinate(&self.manifest, &built.resolution)?;
        let jacoco = self
            .manifest
            .test
            .jacoco_version
            .clone()
            .unwrap_or_else(|| junit::JACOCO_VERSION.to_string());
        let mut internal = vec![(launcher.clone(), "test launcher")];
        if args.coverage {
            internal.push((junit::jacoco_agent(&jacoco), "coverage agent"));
            internal.push((junit::jacoco_cli(&jacoco), "coverage report"));
        }
        let fetched = self.fetch_internal(&internal)?;

        let mut test_classpath = vec![project.test_classes_dir()];
        test_classpath.extend(junit::without_bundled_launcher(
            classpath,
            &built.resolution,
        ));
        test_classpath.push(fetched[0].clone());

        let mut jvm_args = self.manifest.test.jvm_args.clone();
        let exec = project.target_dir().join("jacoco.exec");
        if args.coverage {
            let _ = std::fs::remove_file(&exec);
            jvm_args.insert(0, junit::agent_argument(&fetched[1], &exec));
        }
        // The debugger and `test.java-agents` go ahead of JaCoCo's agent.
        jvm_args.splice(0..0, self.test_jvm_prefix(&built.resolution, args)?);

        let mut run = junit::TestRun {
            jvm_args,
            classpath: test_classpath,
            scan_dir: project.test_classes_dir(),
            filter: junit::class_name_filter(args.filter.as_deref(), sources.foreign().is_some()),
            include_tags: args.include_tag.clone(),
            exclude_tags: args.exclude_tag.clone(),
            methods: args.method.clone(),
            reports_dir: Some(project.target_dir().join("test-reports")),
            color: self.ui.color(),
            ascii: self.ui.glyphs().charset == ui::Charset::Ascii,
            launcher_version: launcher.version.clone(),
            work_dir: project.work_dir(),
            environment: self.jvm_environment("test", &self.manifest.test.env, None)?,
            unique_ids: Vec::new(),
            classes: Vec::new(),
            fail_fast: args.fail_fast,
        };
        self.announce_tests(&mut run, rerun.as_ref(), &sources);
        let started = Instant::now();
        let outcome = self
            .launch_tests(&toolchain, &run, args.debug.as_ref())
            .and_then(|mut outcome| {
                self.conclude_tests(&toolchain, &run, args, &mut outcome)?;
                Ok(outcome)
            });
        self.timings.since("test JVM", started);
        let mut outcome = outcome?;

        // Coverage is reported for a failing run too: which code the failing
        // tests reached is part of working out why.
        if args.coverage && exec.is_file() {
            let started = Instant::now();
            self.coverage_report(&project, &toolchain, exec, &fetched[2])?;
            self.timings.since("coverage report", started);
            outcome.coverage_shortfalls = self.coverage_shortfalls(&project)?;
        }

        if outcome.ok() && outcome.coverage_shortfalls.is_empty() {
            self.hook(Hook::PostTest)?;
        }
        Ok(Some(outcome))
    }

    /// What goes ahead of the test JVM's other arguments: the debugger's
    /// agent under `--debug`, then `test.java-agents` from the test classpath.
    fn test_jvm_prefix(&self, resolution: &Resolution, args: &TestArgs) -> Result<Vec<String>> {
        let agents =
            runner::java_agents("test", &self.manifest.test.java_agents, resolution, false)?;
        Ok(runner::jvm_prefix(args.debug.as_ref(), &agents))
    }

    /// Run the launcher under the live test counter, or, when the JVM waits
    /// for a debugger, without one: it has nothing to count yet, and a
    /// counter animating over it would say otherwise.
    fn launch_tests(
        &self,
        toolchain: &Toolchain,
        run: &junit::TestRun,
        debug: Option<&runner::DebugAddress>,
    ) -> Result<junit::TestOutcome> {
        if let Some(debug) = debug {
            self.announce_debugger(debug);
        }
        let scope = debug.is_none().then(|| self.ui.tests());
        let outcome = junit::run(toolchain, run, self.ui);
        if let Some(scope) = scope {
            scope.finish();
        }
        outcome
    }

    /// Point `run` at what `--rerun-failed` selected, and say what is about to
    /// run, and when `--fail-fast` cannot be honoured.
    fn announce_tests(
        &self,
        run: &mut junit::TestRun,
        rerun: Option<&(usize, test_report::Selection)>,
        sources: &Sources,
    ) {
        if let Some((_, selection)) = rerun {
            // The selectors name exactly what failed; no class-name pattern
            // may narrow them.
            run.filter = None;
            run.methods.clone_from(&selection.methods);
            run.unique_ids.clone_from(&selection.unique_ids);
            run.classes.clone_from(&selection.classes);
        }
        if run.fail_fast_mode() == junit::FailFast::Unsupported {
            self.ui.warn(format!(
                "the JUnit Platform {} console launcher cannot stop at the first failure: it has \
                 no --fail-fast (JUnit 6) and no test feed to follow (1.10), so every test runs",
                run.launcher_version
            ));
        }
        match rerun {
            Some((failed, _)) => self.ui.phase(
                "Testing",
                format!("{} that failed in the last run", counted(*failed, "test")),
            ),
            None => self.ui.phase("Testing", sources.describe("test sources")),
        }
    }

    /// After the launcher: say what stopping it cost, retry what failed, and
    /// write the page.
    fn conclude_tests(
        &self,
        toolchain: &Toolchain,
        run: &junit::TestRun,
        args: &TestArgs,
        outcome: &mut junit::TestOutcome,
    ) -> Result<()> {
        if outcome.stopped_early && run.fail_fast_mode() == junit::FailFast::Stop {
            self.ui.warn(format!(
                "the JUnit Platform {} console launcher has no --fail-fast (JUnit 6 added it), \
                 so jrs stopped it after the first failure: this run has no XML report",
                run.launcher_version
            ));
        }
        // A retry is a launcher of its own, and under `--debug` it would stop
        // and wait for a debugger again, unannounced: no retries there.
        let retries = if args.debug.is_some() {
            0
        } else {
            args.retries.unwrap_or(self.manifest.test.retries)
        };
        if !outcome.ok() && !outcome.stopped_early && retries > 0 {
            self.retry_failures(toolchain, run, retries, outcome)?;
        }
        self.report_tests(run, outcome)
    }

    /// What `--rerun-failed` runs: the tests still failing at the end of the
    /// last run, retries included, as its reports record them, and how many
    /// there are. `None`, having said so, when there are none.
    ///
    /// No reports at all is an error, not nothing to do: a run that ended
    /// before the launcher wrote any did not pass.
    fn last_failures(&self) -> Result<Option<(usize, test_report::Selection)>> {
        let reports = self.project().target_dir().join("test-reports");
        let Some(results) = test_report::load(&reports)? else {
            return Err(JrsError::usage(format!(
                "there is no test run to rerun: {} holds no JUnit XML\n\nrun `jrs test` first",
                reports.display()
            )));
        };
        let failed = results.failed();
        if failed.is_empty() {
            self.ui.phase(
                "Testing",
                "no failed tests to rerun: nothing failed in the last run",
            );
            return Ok(None);
        }
        Ok(Some((failed.len(), test_report::select(failed))))
    }

    /// `test.retries`: run what is still failing again, each attempt in a
    /// launcher of its own writing its reports into `retry-<n>/`, until it
    /// passes or the attempts run out. What passes on a retry is flaky, which
    /// `report_tests` counts; everything passing in the end makes the run a
    /// success, as it does with Gradle's test-retry plugin.
    fn retry_failures(
        &self,
        toolchain: &Toolchain,
        first: &junit::TestRun,
        retries: u32,
        outcome: &mut junit::TestOutcome,
    ) -> Result<()> {
        let Some(reports) = first.reports_dir.as_deref() else {
            return Ok(());
        };
        for attempt in 1..=retries {
            let Some(results) = test_report::load(reports)? else {
                break;
            };
            let failed = results.failed();
            let selection = test_report::select(failed.iter().copied());
            if selection.is_empty() {
                break;
            }
            self.ui.phase(
                "Retrying",
                format!(
                    "{} (attempt {} of {})",
                    counted(failed.len(), "failed test"),
                    attempt + 1,
                    retries + 1
                ),
            );
            let retry = junit::TestRun {
                jvm_args: junit::retry_jvm_args(&first.jvm_args),
                filter: None,
                include_tags: Vec::new(),
                exclude_tags: Vec::new(),
                methods: selection.methods,
                unique_ids: selection.unique_ids,
                classes: selection.classes,
                reports_dir: Some(test_report::retry_dir(reports, attempt)),
                fail_fast: false,
                ..first.clone()
            };
            let scope = self.ui.tests();
            let result = junit::run(toolchain, &retry, self.ui);
            scope.finish();
            if result?.ok() {
                outcome.exit_code = exit::SUCCESS;
                break;
            }
        }
        Ok(())
    }

    /// Read the run's reports back, retries folded in: name and count the
    /// flaky tests, and write `index.html` beside the XML. A run that left no
    /// XML — stopped before the launcher wrote it — gets no page.
    fn report_tests(&self, run: &junit::TestRun, outcome: &mut junit::TestOutcome) -> Result<()> {
        let Some(reports) = run.reports_dir.as_deref() else {
            return Ok(());
        };
        let Some(results) = test_report::load(reports)? else {
            return Ok(());
        };
        if results.retries > 0 {
            let flaky = results.flaky();
            for case in &flaky {
                self.ui.status(
                    "Flaky",
                    format!("{} (passed on attempt {})", case.label(), case.attempts),
                );
            }
            outcome.flaky = u64::try_from(flaky.len()).unwrap_or(u64::MAX);
            outcome.failed = if outcome.ok() {
                0
            } else {
                outcome.failed.saturating_sub(outcome.flaky)
            };
        }
        let index = reports.join(test_report::INDEX);
        self.ui.phase(
            "Reporting",
            format!("test results into {}", index.display()),
        );
        test_report::write_html(reports, &self.manifest.name, &results)?;
        Ok(())
    }

    /// The totals of the coverage report just written that fall short of
    /// `test.coverage-minimum`.
    fn coverage_shortfalls(&self, project: &Project<'_>) -> Result<Vec<junit::CoverageShortfall>> {
        let minimums = &self.manifest.test.coverage_minimum;
        if minimums.is_empty() {
            return Ok(Vec::new());
        }
        let xml = project.target_dir().join("coverage").join("jacoco.xml");
        let text = std::fs::read_to_string(&xml).path(&xml)?;
        Ok(junit::coverage_shortfalls(&text, minimums))
    }

    /// Fetch jrs's own test-time tools, announcing the ones not cached yet.
    /// The jars come back in the order of `internal`.
    fn fetch_internal(&self, internal: &[(Coord, &str)]) -> Result<Vec<PathBuf>> {
        let fetcher = self.fetcher()?;
        let missing: Vec<_> = internal
            .iter()
            .filter(|(c, _)| !fetcher.cache().contains(c, "jar"))
            .collect();
        for (coord, what) in &missing {
            self.ui
                .phase("Downloading", format!("{} ({what})", coord.artifact));
        }
        let started = Instant::now();
        let scope = self.ui.downloads(missing.len());
        let jars: Result<Vec<PathBuf>> = internal
            .iter()
            .map(|(coord, _)| fetcher.jar(coord).map(|(path, _)| path))
            .collect();
        scope.finish();
        self.timings.since("downloads (test tools)", started);
        jars
    }

    /// Render `exec` into `target/coverage`, with the report tool at `cli_jar`.
    fn coverage_report(
        &self,
        project: &Project<'_>,
        toolchain: &Toolchain,
        exec: PathBuf,
        cli_jar: &Path,
    ) -> Result<()> {
        let html = project.target_dir().join("coverage");
        if html.exists() {
            std::fs::remove_dir_all(&html).path(&html)?;
        }
        std::fs::create_dir_all(&html).path(&html)?;
        self.ui
            .phase("Reporting", format!("coverage into {}", html.display()));
        let report = junit::CoverageReport {
            exec,
            classes: project.classes_dir(),
            sources: project.roots(Unit::Main),
            xml: html.join("jacoco.xml"),
            html: html.clone(),
            name: self.manifest.name.clone(),
        };
        let coverage = junit::report_coverage(toolchain, &report, cli_jar, self.ui)?;
        self.ui.phase(
            "Coverage",
            format!(
                "{} ({})",
                coverage.describe(),
                html.join("index.html").display()
            ),
        );
        Ok(())
    }

    fn doc_command(&self) -> Result<i32> {
        let index = self.doc()?;
        self.ui.phase(
            "Finished",
            format!("{} in {}", index.display(), self.elapsed()),
        );
        Ok(exit::SUCCESS)
    }

    /// Document the main sources, generated ones included, so the
    /// `pre-compile` hook runs first. Returns the index page.
    ///
    /// Scala and Groovy units go to Scaladoc or Groovydoc (SPEC §7.4). The
    /// rest go to `javadoc`, and a Kotlin unit's Kotlin sources are left out,
    /// with a warning: the build runs first, since the Java sources may use
    /// the Kotlin classes and `javadoc` has to find them on the classpath.
    fn doc(&self) -> Result<PathBuf> {
        let toolchain = self.toolchain()?;
        let resolution = self.resolved()?;
        self.hook(Hook::PreCompile)?;
        let project = self.project();
        let generated = task::generated(&self.manifest, Hook::PreCompile)?;
        let all = project.sources(Unit::Main, &generated.sources)?;
        if let Some(language) = all.foreign()
            && let Some(config) = self.manifest.language(language)
            && let Some(tool) = language.doc_tool(&config.version)
        {
            let mut roots = project.roots(Unit::Main);
            roots.extend(generated.sources.iter().cloned());
            return self.foreign_doc(&toolchain, &resolution, config, tool, &all, roots);
        }

        let javadoc = toolchain.tool("javadoc")?;
        let mut classpath = resolution.classpath(Classpath::Compile);
        if let Some(language) = all.foreign() {
            self.ui.warn(format!(
                "jrs has no documentation tool for {language}: its {} source files were left \
                 out, and javadoc documents the Java ones",
                all.count(language)
            ));
            self.build()?;
            classpath.insert(0, project.classes_dir());
        }
        let sources: Vec<PathBuf> = all
            .files
            .iter()
            .filter(|p| Language::of(p) == Some(Language::Java))
            .cloned()
            .collect();
        if sources.is_empty() {
            return Err(JrsError::build(format!(
                "no .java files under {} to document",
                display_roots(&project.roots(Unit::Main))
            )));
        }
        let unit = DocUnit {
            sources: sources.clone(),
            output_dir: project.target_dir().join("doc"),
            classpath,
            release: toolchain.release(self.manifest.java.source)?,
            encoding: self.manifest.java.encoding.clone(),
            extra_args: self.manifest.java.javadoc_args.clone(),
            title: format!("{} {}", self.manifest.name, self.manifest.version),
            work_dir: project.work_dir(),
        };
        self.ui.phase(
            "Documenting",
            format!(
                "{} v{} ({} source files)",
                self.manifest.name,
                self.manifest.version,
                sources.len()
            ),
        );
        let scope = self
            .ui
            .spinner("Documenting", format!("{} source files", sources.len()));
        let result = compile::javadoc(&javadoc, &unit, self.ui);
        scope.finish();
        result?;
        Ok(unit.output_dir.join("index.html"))
    }

    /// Document a Scala or Groovy unit with its language's own tool, into
    /// `target/doc` as `javadoc` would. Scala 3's scaladoc reads the compiled
    /// classes' TASTy, so the build runs first and the Java sources, which
    /// have none, are left out; Scaladoc 2 and Groovydoc read every source.
    fn foreign_doc(
        &self,
        toolchain: &Toolchain,
        resolution: &Resolution,
        config: &LanguageConfig,
        tool: DocTool,
        all: &Sources,
        roots: Vec<PathBuf>,
    ) -> Result<PathBuf> {
        let language = config.language;
        let project = self.project();
        let reads_classes = ForeignDoc::reads_classes(language, &config.version);
        if reads_classes {
            self.build()?;
        }
        let tool_classpath = if tool.roots.is_empty() {
            self.tools()?
                .into_iter()
                .find(|t| t.language == language)
                .ok_or_else(|| {
                    JrsError::build(format!("the {language} compiler was not resolved"))
                })?
                .resolution
                .runtime_classpath()
        } else {
            self.doc_tool_classpath(language, &tool)?
        };

        let (sources, what) = if reads_classes {
            let java = all.count(Language::Java);
            if java > 0 {
                self.ui.warn(format!(
                    "scaladoc for Scala 3 reads TASTy, which Java classes have none of: {java} \
                     Java source files were left out"
                ));
            }
            let own: Vec<PathBuf> = all
                .files
                .iter()
                .filter(|p| Language::of(p) == Some(language))
                .cloned()
                .collect();
            let what = format!("{} {language} source files", own.len());
            (own, what)
        } else {
            (all.files.clone(), all.describe("source files"))
        };
        let unit = ForeignDoc {
            language,
            version: config.version.clone(),
            tool,
            tool_classpath,
            jvm_args: config.compiler_jvm_args.clone(),
            sources,
            roots,
            classes_dir: project.classes_dir(),
            classpath: resolution.classpath(Classpath::Compile),
            output_dir: project.target_dir().join("doc"),
            release: toolchain.release(self.manifest.java.source)?,
            encoding: self.manifest.java.encoding.clone(),
            name: self.manifest.name.clone(),
            project_version: self.manifest.version.clone(),
            work_dir: project.work_dir(),
        };
        self.ui.phase(
            "Documenting",
            format!("{} v{} ({what})", self.manifest.name, self.manifest.version),
        );
        let scope = self.ui.spinner("Documenting", &what);
        let result = compile::document(toolchain, &unit, self.ui);
        scope.finish();
        result?;
        Ok(unit.output_dir.join("index.html"))
    }

    /// A doc tool's own graph. It is resolved when `jrs doc` needs it and not
    /// pinned in `jrs.lock`: like the test launcher it is an internal tool at
    /// an exact version, and a build never needs it.
    fn doc_tool_classpath(&self, language: Language, tool: &DocTool) -> Result<Vec<PathBuf>> {
        let fetcher = self.fetcher()?;
        let started = Instant::now();
        let artifact = tool
            .roots
            .first()
            .map_or_else(String::new, |r| r.artifact.clone());
        let what = format!("{artifact} ({language} doc tool)");
        if !self.offline
            && tool
                .roots
                .first()
                .is_some_and(|root| !fetcher.cache().contains(root, "pom"))
        {
            self.ui.phase("Resolving", &what);
        }
        let mut resolution = resolve::resolve_tool(&tool.roots, &fetcher, self.jobs)?;
        resolve::locate_cached(&mut resolution, &fetcher);
        let missing = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            self.ui.phase("Downloading", &what);
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(&mut resolution, &fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(&mut resolution, &fetcher, self.jobs)?;
        }
        self.timings
            .since(format!("downloads ({language} doc tool)"), started);
        for warning in &resolution.warnings {
            self.ui.verbose(format!("{}: {warning}", tool.name));
        }
        Ok(resolution.runtime_classpath())
    }

    fn tree_command(
        &self,
        depth: Option<usize>,
        why: Option<&str>,
        tool: Option<&str>,
    ) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let trees = match (tool, why) {
            (Some(name), _) => vec![self.tool_tree(name, depth)?],
            (None, Some(target)) => self.why(&resolution, target)?,
            (None, None) => vec![self.tree(&resolution, depth)],
        };
        self.ui.suspend();
        for tree in &trees {
            self.ui.tree(tree);
        }
        Ok(exit::SUCCESS)
    }

    fn classpath_command(&self, test: bool, runtime: bool) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let project = self.project();
        let mut entries = Vec::new();
        if test {
            entries.push(project.test_classes_dir());
        }
        entries.push(project.classes_dir());
        entries.extend(if test {
            resolution.classpath(Classpath::Test)
        } else if runtime {
            resolution.runtime_classpath()
        } else {
            resolution.classpath(Classpath::Compile)
        });
        // The classpath is real output, for `java -cp "$(jrs classpath)"`.
        self.ui.suspend();
        self.ui.println_out(Toolchain::classpath(&entries));
        Ok(exit::SUCCESS)
    }

    fn update_command(&self) -> Result<i32> {
        let resolution = self.dependencies(true)?;
        self.ui.phase(
            "Updated",
            format!(
                "{} in {} packages",
                self.manifest.lock_path().display(),
                resolution.packages.len()
            ),
        );
        Ok(exit::SUCCESS)
    }

    /// What `jrs outdated` checks, as (row name, group, artifact, current
    /// version): the declared dependencies, then each language's compiler. A
    /// local jar has no repository to ask, so it is left out.
    fn outdated_checks(&self) -> Vec<(String, String, String, String)> {
        let mut checks: Vec<(String, String, String, String)> = self
            .manifest
            .dependencies
            .iter()
            .map(|d| (d.key(), d))
            .chain(
                self.manifest
                    .dev_dependencies
                    .iter()
                    .map(|d| (format!("{} (dev)", d.key()), d)),
            )
            .filter(|(_, d)| !d.is_local())
            .map(|(name, d)| (name, d.group.clone(), d.artifact.clone(), d.version.clone()))
            .collect();
        for config in &self.manifest.languages {
            if let Some(compiler) = config.language.compiler(&config.version) {
                checks.push((
                    format!("{}.version", config.language.key()),
                    compiler.coord.group,
                    compiler.coord.artifact,
                    config.version.clone(),
                ));
            }
        }
        checks
    }

    fn outdated_command(&self) -> Result<i32> {
        let declared = self.outdated_checks();
        if declared.is_empty() {
            self.ui.phase("Finished", "no dependencies are declared");
            return Ok(exit::SUCCESS);
        }
        let fetcher = self.fetcher()?;
        self.ui.phase(
            "Checking",
            format!("{} dependencies for newer releases", declared.len()),
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.jobs.max(1))
            .build()
            .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
        let scope = self
            .ui
            .spinner("Checking", format!("{} dependencies", declared.len()));
        let answers: Vec<Result<metadata::Metadata>> = pool.install(|| {
            declared
                .par_iter()
                .map(|(_, group, artifact, _)| fetcher.metadata(group, artifact))
                .collect()
        });
        scope.finish();

        let mut rows = Vec::new();
        for ((name, _, _, version), answer) in declared.iter().zip(answers) {
            match answer {
                Ok(m) => {
                    if let Some(newer) = metadata::newest(&m.versions, version) {
                        rows.push([name.clone(), version.clone(), newer]);
                    }
                }
                // One unreachable artifact should not hide the rest.
                Err(e) => self
                    .ui
                    .warn(e.to_string().lines().next().unwrap_or_default()),
            }
        }
        for warning in fetcher.take_warnings() {
            self.ui.warn(warning);
        }

        if rows.is_empty() {
            self.ui
                .phase("Finished", "every dependency is on its newest release");
            return Ok(exit::SUCCESS);
        }
        let header = [
            "dependency".to_string(),
            "current".to_string(),
            "newest".to_string(),
        ];
        let widths: Vec<usize> = (0..3)
            .map(|i| {
                rows.iter()
                    .chain(std::iter::once(&header))
                    .map(|r| r[i].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        self.ui.suspend();
        for row in std::iter::once(&header).chain(&rows) {
            self.ui.println_out(format!(
                "{:<w0$}  {:<w1$}  {}",
                row[0],
                row[1],
                row[2],
                w0 = widths[0],
                w1 = widths[1]
            ));
        }
        self.ui.phase(
            "Finished",
            format!(
                "{} of {} dependencies have newer releases",
                rows.len(),
                declared.len()
            ),
        );
        Ok(exit::SUCCESS)
    }

    fn verify_command(&self) -> Result<i32> {
        let lock_path = self.manifest.lock_path();
        let lock = Lockfile::load(&lock_path)?.ok_or_else(|| {
            JrsError::usage(format!(
                "{} does not exist\n\nrun `jrs build` or `jrs update` to resolve and pin \
                 the dependencies first",
                lock_path.display()
            ))
        })?;
        if !lock.matches(&self.manifest) {
            self.ui.warn(format!(
                "{} is out of date with {}; verifying what it records \
                 (run `jrs update` to re-resolve)",
                lock_path.display(),
                self.manifest.path.display()
            ));
        }

        // The compilers' pins are checked with the project's.
        let resolution = Resolution {
            packages: lock.all_packages().cloned().collect(),
            ..lock.to_resolution()
        };
        let count = resolution
            .packages
            .iter()
            .filter(|p| p.packaging != "pom")
            .count()
            + lock.local.len();
        self.ui
            .phase("Verifying", format!("{count} locked artifacts"));
        let scope = self
            .ui
            .spinner("Verifying", format!("{count} locked artifacts"));
        let checked = resolve::verify_cached(&resolution, &Cache::discover()?, self.jobs);
        scope.finish();

        let mut verified = 0;
        let mut not_cached = 0;
        let mut mismatches = Vec::new();
        for c in checked? {
            match c.integrity {
                resolve::Integrity::Verified => verified += 1,
                resolve::Integrity::NotCached => {
                    not_cached += 1;
                    self.ui.verbose(format!("{} is not cached", c.coord));
                }
                resolve::Integrity::Snapshot => {
                    self.ui
                        .verbose(format!("{} is a snapshot, which is never pinned", c.coord));
                }
                resolve::Integrity::Unpinned => self.ui.warn(format!(
                    "`{}` has no checksum in {}; it was not verified",
                    c.coord,
                    lock_path.display()
                )),
                resolve::Integrity::Mismatch { expected, actual } => mismatches.push(format!(
                    "  {}\n    locked {expected}\n    cached {actual}",
                    c.path.display()
                )),
            }
        }
        // Local jars are checked where the project keeps them.
        for local in &lock.local {
            let path = self.manifest.root.join(&local.path);
            match resolve::local_integrity(&path, local.checksum.as_deref())? {
                resolve::Integrity::Verified => verified += 1,
                resolve::Integrity::Mismatch { expected, actual } => mismatches.push(format!(
                    "  {} (local jar `{}`)\n    locked {expected}\n    found  {actual}",
                    local.path, local.name
                )),
                resolve::Integrity::NotCached => mismatches.push(format!(
                    "  {} (local jar `{}`)\n    missing",
                    local.path, local.name
                )),
                resolve::Integrity::Unpinned | resolve::Integrity::Snapshot => {
                    self.ui.warn(format!(
                        "the local jar `{}` has no checksum in {}; it was not verified",
                        local.name,
                        lock_path.display()
                    ));
                }
            }
        }

        if !mismatches.is_empty() {
            return Err(JrsError::build(format!(
                "{} cached artifacts do not match {}:\n\n{}\n\n\
                 delete them and build again to re-download them; if it is jrs.lock \
                 that changed, run `jrs update`",
                mismatches.len(),
                lock_path.display(),
                mismatches.join("\n")
            )));
        }
        let mut message = format!("{verified} artifacts against {}", lock_path.display());
        if not_cached > 0 {
            let _ = write!(
                message,
                " ({not_cached} not cached; checked when they are downloaded)"
            );
        }
        self.ui.phase("Verified", message);
        Ok(exit::SUCCESS)
    }

    /// `jrs metadata`: the project model as versioned JSON on stdout
    /// (`model.rs`). It resolves, unless `--no-deps`, and never compiles.
    fn metadata_command(&self, no_deps: bool) -> Result<i32> {
        let resolution = if no_deps {
            None
        } else {
            Some(self.dependencies(false)?)
        };
        // Without a JDK the layout and the classpaths are still worth having,
        // so a missing one leaves `jdk` null rather than failing the command.
        let toolchain = match self.toolchain() {
            Ok(toolchain) => Some(toolchain),
            Err(e) => {
                self.ui.warn(format!(
                    "{}; the model has no `jdk`",
                    e.to_string().lines().next().unwrap_or_default()
                ));
                None
            }
        };
        let cache = Cache::discover().ok();
        let document = model::metadata(&model::Inputs {
            manifest: &self.manifest,
            toolchain: toolchain.as_ref(),
            resolution: resolution.as_ref(),
            cache: cache.as_ref(),
        })?;
        // The model is real output, for `jrs metadata | jq`.
        self.ui.suspend();
        for line in document.render().lines() {
            self.ui.println_out(line);
        }
        Ok(exit::SUCCESS)
    }

    /// `jrs fetch`: resolve and download what a build and a test run need —
    /// the dependencies, the compilers, the test launcher — without building;
    /// with `--sources`, every dependency's `-sources.jar` as well.
    fn fetch_command(&self, sources: bool) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        // The launcher `jrs test` would run, when there is a JUnit to derive
        // it from, so that `jrs test --offline` works after a fetch.
        if let Ok(launcher) = junit::launcher_coordinate(&self.manifest, &resolution) {
            self.fetch_internal(&[(launcher, "test launcher")])?;
        }
        let jars = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_some())
            .count();
        let mut fetched = format!("{jars} dependencies");
        if sources {
            let (found, wanted) = self.fetch_sources(&resolution)?;
            let _ = write!(fetched, " and {found} of {wanted} sources jars");
        }
        self.ui.phase(
            "Finished",
            format!("fetched {fetched} in {}", self.elapsed()),
        );
        Ok(exit::SUCCESS)
    }

    /// Every dependency's `-sources.jar`, into the cache, under the usual
    /// `Downloading` line and bars. One that is not published is a warning,
    /// not an error: many libraries publish none, and no build needs one.
    /// Returns how many are cached now, out of how many were wanted.
    fn fetch_sources(&self, resolution: &Resolution) -> Result<(usize, usize)> {
        let fetcher = self.fetcher()?;
        let wanted = resolve::sources_coords(resolution);
        let missing = wanted
            .iter()
            .filter(|c| !fetcher.cache().contains(c, "jar"))
            .count();
        let results = if missing > 0 && !self.offline {
            self.ui
                .phase("Downloading", format!("{missing} sources jars"));
            let scope = self.ui.downloads(missing);
            let results = resolve::fetch_sources(&wanted, &fetcher, self.jobs);
            scope.finish();
            results?
        } else {
            resolve::fetch_sources(&wanted, &fetcher, self.jobs)?
        };

        let mut found = 0;
        let mut unpublished = Vec::new();
        let mut not_cached = Vec::new();
        for (coord, result) in results {
            let Err(e) = result else {
                found += 1;
                continue;
            };
            let artifact = coord.pom_coord().to_string();
            let reason = e.to_string();
            let reason = reason.lines().next().unwrap_or_default();
            if self.offline {
                not_cached.push(artifact);
            } else if reason.starts_with("could not find") {
                // What the fetcher says when no repository has the file.
                unpublished.push(artifact);
            } else {
                self.ui.warn(format!(
                    "could not download the sources jar of `{artifact}`: {reason}"
                ));
            }
        }
        for warning in fetcher.take_warnings() {
            self.ui.warn(warning);
        }
        if !unpublished.is_empty() {
            let (noun, verb) = if unpublished.len() == 1 {
                ("dependency", "publishes")
            } else {
                ("dependencies", "publish")
            };
            self.ui.warn(format!(
                "{} {noun} {verb} no sources jar: {}",
                unpublished.len(),
                unpublished.join(", ")
            ));
        }
        if !not_cached.is_empty() {
            self.ui.warn(format!(
                "{} sources jars are not in the cache, and --offline was given: {}",
                not_cached.len(),
                not_cached.join(", ")
            ));
        }
        Ok((found, wanted.len()))
    }

    // ---- the build pipeline -----------------------------------------------

    fn fetcher(&self) -> Result<Fetcher> {
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
    fn build(&self) -> Result<Built> {
        if let Some(built) = self.built.get() {
            return Ok(built.clone());
        }
        let toolchain = self.toolchain()?;
        let resolution = self.resolved()?;
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

        let unit = self.compile_unit(
            "main",
            &sources,
            project.classes_dir(),
            resolution.classpath(Classpath::Compile),
            Vec::new(),
        )?;

        let checking = Instant::now();
        let outcome = if compile::is_stale(&unit)? {
            self.ui.phase(
                "Compiling",
                format!(
                    "{} v{} ({})",
                    self.manifest.name,
                    self.manifest.version,
                    sources.describe("source files")
                ),
            );
            let scope = self
                .ui
                .spinner("Compiling", sources.describe("source files"));
            let mut steps = Vec::new();
            let result = compile::compile_timed(&toolchain, &unit, self.ui, &mut steps);
            scope.finish();
            self.record_steps("main", &steps);
            result?
        } else {
            self.ui.phase(
                "Fresh",
                format!("{} v{}", self.manifest.name, self.manifest.version),
            );
            self.timings.since("compile main (fresh)", checking);
            compile::Outcome::UpToDate
        };

        let syncing = Instant::now();
        let synced = project::sync_resources(
            &self.manifest.resource_path(),
            &project.classes_dir(),
            &project.work_dir().join("resources-main.list"),
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
            compile::Outcome::Compiled { classes } => classes,
            compile::Outcome::UpToDate => {
                project::find_by_extension(&project.classes_dir(), "class")?.len()
            }
        };

        let built = Built {
            resolution,
            classes,
        };
        Ok(self.built.get_or_init(|| built).clone())
    }

    /// Mirror generated resource directories into `to`, each with its own
    /// record of what it put there, as `src/main/resources` has (SPEC §7.3).
    fn sync_generated(&self, dirs: &[PathBuf], to: &Path, unit: &str) -> Result<()> {
        let work_dir = self.project().work_dir();
        for (i, dir) in dirs.iter().enumerate() {
            let record = work_dir.join(format!("resources-{unit}-generated-{i}.list"));
            let synced = project::sync_resources(dir, to, &record)?;
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
    fn resolved(&self) -> Result<Resolution> {
        if let Some(resolution) = self.resolution.get() {
            return Ok(resolution.clone());
        }
        let resolution = self.dependencies(false)?;
        Ok(self.resolution.get_or_init(|| resolution).clone())
    }

    /// The compilers' graphs, resolved with the project's.
    fn tools(&self) -> Result<Vec<Tool>> {
        self.resolved()?;
        Ok(self.tools.get().cloned().unwrap_or_default())
    }

    /// A compile unit for `sources`: `javac`'s settings, and the compiler of
    /// the unit's other language when it has sources in one.
    fn compile_unit(
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
                Some(ForeignCompiler {
                    language,
                    version: config.version.clone(),
                    classpath: tool.resolution.runtime_classpath(),
                    jvm_args: config.compiler_jvm_args.clone(),
                    extra_args: config.compiler_args.clone(),
                    module_name: if label == "test" {
                        format!("{name}_test")
                    } else {
                        name.clone()
                    },
                    friend_paths,
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
        })
    }

    // ---- tasks and hooks --------------------------------------------------

    fn task_command(&self, args: &TaskArgs) -> Result<i32> {
        let Some(name) = args.name.as_deref().filter(|_| !args.list) else {
            return Ok(self.task_list());
        };
        if self.manifest.task(name).is_none() {
            return Err(JrsError::usage(format!(
                "there is no task `{name}` in {}\n\n`jrs task --list` shows the tasks there are",
                self.manifest.path.display()
            )));
        }
        let plan = task::plan(&self.manifest, &[TaskRef::Task(name.to_string())]);
        // The named task comes last; what it depends on runs as a hook's
        // tasks do, and only the named one gets the terminal.
        for step in plan
            .iter()
            .filter(|s| **s != TaskRef::Task(name.to_string()))
        {
            self.step(step, None)?;
        }
        let code = match self.manifest.task(name) {
            Some(def) if self.ran.borrow_mut().insert(name.to_string()) => {
                self.run_task(def, None, Some(&args.args))?
            }
            _ => exit::SUCCESS,
        };
        if code == exit::SUCCESS {
            self.ui
                .phase("Finished", format!("task {name} in {}", self.elapsed()));
        } else {
            self.ui
                .phase("Finished", format!("task {name} exited with {code}"));
        }
        Ok(code)
    }

    /// `jrs task --list`, on stdout: it is the command's real output.
    fn task_list(&self) -> i32 {
        let lines = task::list(&self.manifest);
        if lines.is_empty() {
            self.ui.phase(
                "Finished",
                format!("{} declares no [tasks]", self.manifest.path.display()),
            );
            return exit::SUCCESS;
        }
        self.ui.suspend();
        for line in lines {
            self.ui.println_out(line);
        }
        exit::SUCCESS
    }

    /// Run the tasks a lifecycle point names, and what they depend on. Every
    /// time the point is reached, whether or not `javac` had anything to do:
    /// skipping work is a task's own business, through its inputs and outputs.
    fn hook(&self, hook: Hook) -> Result<()> {
        let roots: Vec<TaskRef> = self
            .manifest
            .hooks
            .tasks(hook)
            .iter()
            .map(|name| TaskRef::Task(name.clone()))
            .collect();
        for step in task::plan(&self.manifest, &roots) {
            self.step(&step, Some(hook))?;
        }
        Ok(())
    }

    /// One step of a plan: a built-in runs as its command would, hooks
    /// included; a task runs unless it already has. Either fails the command
    /// when it fails.
    fn step(&self, step: &TaskRef, hook: Option<Hook>) -> Result<()> {
        match step {
            TaskRef::Builtin(builtin) => self.builtin(*builtin),
            TaskRef::Task(name) => {
                if !self.ran.borrow_mut().insert(name.clone()) {
                    return Ok(());
                }
                let Some(def) = self.manifest.task(name) else {
                    return Ok(());
                };
                match self.run_task(def, hook, None)? {
                    exit::SUCCESS => Ok(()),
                    // Its output has been passed through already; this only
                    // says which task it was.
                    code => Err(JrsError::build(format!(
                        "task `{name}` failed (exit code {code})"
                    ))),
                }
            }
        }
    }

    /// A built-in named in a `depends-on`: the command's work and its hooks,
    /// without its `Finished` line and summary.
    fn builtin(&self, builtin: Builtin) -> Result<()> {
        if !self.done.borrow_mut().insert(builtin) {
            return Ok(());
        }
        match builtin {
            Builtin::Build => {
                self.build()?;
            }
            Builtin::Test => {
                if let Some(outcome) = self.test(&TestArgs::default())?
                    && !outcome.ok()
                {
                    return Err(JrsError::test(format!(
                        "tests failed ({})",
                        outcome.describe()
                    )));
                }
            }
            Builtin::Package => {
                self.package(&PackageArgs::default())?;
            }
            Builtin::Doc => {
                self.doc()?;
            }
        }
        Ok(())
    }

    /// Run one task and return its exit code. `named` carries the arguments
    /// of `jrs task <name> -- ...`: that task gets the terminal, like `jrs run`;
    /// any other task has its output streamed to stderr.
    fn run_task(&self, def: &TaskDef, hook: Option<Hook>, named: Option<&[String]>) -> Result<i32> {
        if def.action.is_none() {
            return Ok(exit::SUCCESS);
        }
        let toolchain = self.toolchain()?;
        let classpaths = if task::needs_classpath(def) || self.resolution.get().is_some() {
            Some(self.classpaths()?)
        } else {
            None
        };
        let jar = self.jar.borrow().clone();
        let path = std::env::var_os("PATH");
        let ctx = task::Context {
            manifest: &self.manifest,
            toolchain: &toolchain,
            hook,
            classpaths: classpaths.as_ref(),
            jar: jar.as_deref(),
            offline: self.offline,
            path: path.as_deref(),
        };
        let started = Instant::now();
        let prepared = task::prepare(def, &ctx, named.unwrap_or_default())?;
        if prepared.is_fresh() {
            self.ui.phase("Fresh", format!("{} (task)", def.name));
            self.timings
                .since(task_timing_label(&def.name, hook, true), started);
            return Ok(exit::SUCCESS);
        }

        match hook {
            Some(hook) => self.ui.phase("Task", format!("{} ({hook})", def.name)),
            None => self.ui.phase("Task", &def.name),
        }
        self.ui.verbose(prepared.launch.describe());
        self.ui.verbose(format!("in {}", prepared.cwd.display()));
        for (key, value) in prepared.env.iter().filter(|(k, _)| k != "PATH") {
            self.ui
                .verbose(format!("{key}={}", value.to_string_lossy()));
        }
        let process = toolchain::TaskProcess {
            name: &def.name,
            launch: &prepared.launch,
            cwd: &prepared.cwd,
            env: &prepared.env,
        };
        let code = if named.is_some() {
            toolchain::run_task_inherited(self.ui, &process)?
        } else {
            let scope = self.ui.spinner("Task", &def.name);
            let code = toolchain::run_task_streamed(self.ui, &process);
            scope.finish();
            code?
        };
        self.timings
            .since(task_timing_label(&def.name, hook, false), started);
        if code == exit::SUCCESS {
            prepared.record()?;
        } else {
            prepared.forget();
        }
        Ok(code)
    }

    /// The classpaths a task can name, as `jrs classpath` prints them — with
    /// absolute class directories, since a task may run somewhere else.
    fn classpaths(&self) -> Result<task::Classpaths> {
        let resolution = self.resolved()?;
        let project = self.project();
        let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
        let classes = absolute(project.classes_dir());
        let test_classes = absolute(project.test_classes_dir());
        let with = |dirs: Vec<PathBuf>, jars: Vec<PathBuf>| dirs.into_iter().chain(jars).collect();
        Ok(task::Classpaths {
            compile: with(
                vec![classes.clone()],
                resolution.classpath(Classpath::Compile),
            ),
            runtime: with(vec![classes.clone()], resolution.runtime_classpath()),
            test: with(
                vec![test_classes, classes],
                resolution.classpath(Classpath::Test),
            ),
        })
    }

    /// The resolved graph, from the lockfile when it still matches the
    /// manifest — and with it each language's compiler graph, which
    /// `jrs.lock` pins beside the project's (`JVM_LANGUAGES.md` §5.2).
    fn dependencies(&self, force_update: bool) -> Result<Resolution> {
        let manifest = &self.manifest;
        let declared = manifest.dependencies.len() + manifest.dev_dependencies.len();
        if declared == 0 && manifest.languages.is_empty() {
            let _ = self.tools.set(Vec::new());
            return Ok(Resolution::default());
        }

        // `jrs update` asks every cached snapshot for its newest build too.
        let resolving = Instant::now();
        let fetcher = self.fetcher()?.refreshing_snapshots(force_update);
        let lock_path = manifest.lock_path();
        let existing = Lockfile::load(&lock_path)?;

        let (mut resolution, mut tools, fresh) = match existing {
            Some(lock) if lock.matches(manifest) && !force_update => {
                self.ui.verbose(format!("reusing {}", lock_path.display()));
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
                (lock.to_resolution(), tools, false)
            }
            _ => {
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
                self.ui.phase("Resolving", &what);
                let scope = self.ui.spinner("Resolving", &what);
                let resolved = self.resolve_with_tools(&fetcher);
                scope.finish();
                let (resolution, tools) = resolved?;
                (resolution, tools, true)
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

        let downloading = Instant::now();
        resolve::locate_cached(&mut resolution, &fetcher);
        resolve::attach_local(&mut resolution, &manifest.root)?;
        let missing = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            self.ui.phase("Downloading", format!("{missing} artifacts"));
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(&mut resolution, &fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(&mut resolution, &fetcher, self.jobs)?;
        }
        self.timings.since("downloads", downloading);

        self.fetch_tools(&mut tools, &fetcher)?;

        if fresh {
            let mut lock = Lockfile::from_resolution(manifest, &resolution);
            for tool in &tools {
                lock = lock.with_tool(&tool.language.tool_name(), &tool.resolution);
            }
            lock.write(&lock_path)?;
            self.ui.verbose(format!("wrote {}", lock_path.display()));
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
        let _ = self.tools.set(tools);
        Ok(resolution)
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

    /// The project's graph, and each language's compiler as a graph apart
    /// from it.
    fn resolve_with_tools(&self, fetcher: &Fetcher) -> Result<(Resolution, Vec<Tool>)> {
        let resolution = resolve::resolve(&self.manifest, fetcher, self.jobs)?;
        let mut tools = Vec::new();
        for config in &self.manifest.languages {
            if let Some(compiler) = config.language.compiler(&config.version) {
                tools.push(Tool {
                    language: config.language,
                    resolution: resolve::resolve_tool(&[compiler.coord], fetcher, self.jobs)?,
                });
            }
        }
        Ok((resolution, tools))
    }

    /// `jrs tree --tool <name>`: a compiler's own graph.
    fn tool_tree(&self, name: &str, limit: Option<usize>) -> Result<TreeNode> {
        let tools = self.tools.get().cloned().unwrap_or_default();
        let Some(tool) = tools.iter().find(|t| t.language.tool_name() == name) else {
            let known: Vec<String> = tools
                .iter()
                .map(|t| format!("`{}`", t.language.tool_name()))
                .collect();
            let hint = if known.is_empty() {
                "this project has none: jrs resolves a compiler for each of [kotlin], [scala] \
                 and [groovy] it turns on"
                    .to_string()
            } else {
                format!("this project's are {}", known.join(", "))
            };
            return Err(JrsError::usage(format!(
                "there is no tool `{name}`\n\n{hint}"
            )));
        };
        let version = self
            .manifest
            .language(tool.language)
            .map_or("", |c| c.version.as_str());
        let mut root =
            TreeNode::styled(format!("{name} ({} {version})", tool.language), Style::Bold);
        if limit == Some(0) {
            return Ok(root);
        }
        let mut seen = Vec::new();
        for ga in &tool.resolution.roots {
            root.children
                .push(Self::tree_node(&tool.resolution, ga, &mut seen, 0, limit));
        }
        Ok(root)
    }

    /// The dependency graph as a drawable tree, `limit` levels deep.
    fn tree(&self, resolution: &Resolution, limit: Option<usize>) -> TreeNode {
        let mut root = TreeNode::styled(
            format!("{} v{}", self.manifest.name, self.manifest.version),
            Style::Bold,
        );
        if limit == Some(0) {
            return root;
        }
        let mut seen = Vec::new();
        let implied = self.manifest.implied_dependencies();
        for ga in resolution.roots.iter().chain(&resolution.test_roots) {
            let mut node = Self::tree_node(resolution, ga, &mut seen, 0, limit);
            if let Some((_, language)) = implied.iter().find(|(d, _)| {
                d.group == ga.group && d.artifact == ga.artifact && ga.classifier.is_none()
            }) {
                let _ = write!(node.label, " (implied by [{}])", language.key());
            }
            root.children.push(node);
        }
        // A local jar has no graph: one line, declared, with nothing below it.
        for local in &resolution.local {
            root.children
                .push(TreeNode::styled(local_label(local), Style::None));
        }
        root
    }

    fn tree_node(
        resolution: &Resolution,
        ga: &Ga,
        seen: &mut Vec<Ga>,
        depth: usize,
        limit: Option<usize>,
    ) -> TreeNode {
        let Some(package) = resolution.get(ga) else {
            return TreeNode::styled(format!("{ga} (unresolved)"), Style::Red);
        };
        let (label, style) = package_label(package);

        if seen.contains(ga) {
            return TreeNode::styled(format!("{label} (*)"), Style::Dim);
        }
        if depth > 32 {
            return TreeNode::styled(format!("{label} (...)"), Style::Dim);
        }
        seen.push(ga.clone());

        let mut node = TreeNode::styled(label, style);
        if limit.is_none_or(|l| depth + 1 < l) {
            for child in &package.dependencies {
                node.children
                    .push(Self::tree_node(resolution, child, seen, depth + 1, limit));
            }
        }
        node
    }

    /// `jrs tree --why`: for each package matching `target`, the inverted
    /// graph — the target at the top, the packages that pull it in below it,
    /// down to the manifest that declared them.
    fn why(&self, resolution: &Resolution, target: &str) -> Result<Vec<TreeNode>> {
        let wanted = Ga::parse(target);
        let matches: Vec<Ga> = resolution
            .packages
            .iter()
            .filter(|p| match &wanted {
                Some(ga) => {
                    p.coord.group == ga.group
                        && p.coord.artifact == ga.artifact
                        && (ga.classifier.is_none() || p.coord.classifier == ga.classifier)
                }
                None => p.coord.artifact == target,
            })
            .map(resolve::ResolvedPackage::ga)
            .collect();
        let local = resolution.local_jar(target).map(|l| self.local_why(l));
        if matches.is_empty() && local.is_none() {
            return Err(JrsError::usage(format!(
                "`{target}` is not in the resolved graph\n\n\
                 run `jrs tree` to see what is"
            )));
        }

        let mut dependents: HashMap<Ga, Vec<Ga>> = HashMap::new();
        for p in &resolution.packages {
            for child in &p.dependencies {
                dependents.entry(child.clone()).or_default().push(p.ga());
            }
        }
        for parents in dependents.values_mut() {
            parents.sort();
            parents.dedup();
        }
        Ok(matches
            .iter()
            .map(|ga| self.why_node(resolution, &dependents, ga, &mut vec![ga.clone()]))
            .chain(local)
            .collect())
    }

    /// `jrs tree --why <name>` for a local jar: nothing pulls it in but the
    /// manifest.
    fn local_why(&self, local: &resolve::LocalJar) -> TreeNode {
        let table = if local.classpath == Classpath::Test {
            "dev-dependencies"
        } else {
            "dependencies"
        };
        let mut node = TreeNode::styled(local_label(local), Style::None);
        node.children.push(TreeNode::styled(
            format!(
                "{} v{} [{table}]",
                self.manifest.name, self.manifest.version
            ),
            Style::Bold,
        ));
        node
    }

    fn why_node(
        &self,
        resolution: &Resolution,
        dependents: &HashMap<Ga, Vec<Ga>>,
        ga: &Ga,
        path: &mut Vec<Ga>,
    ) -> TreeNode {
        let mut node = match resolution.get(ga) {
            Some(package) => {
                let (label, style) = package_label(package);
                TreeNode::styled(label, style)
            }
            None => TreeNode::styled(format!("{ga} (unresolved)"), Style::Red),
        };
        let project = format!("{} v{}", self.manifest.name, self.manifest.version);
        if resolution.roots.contains(ga) {
            node.children.push(TreeNode::styled(
                format!("{project} [dependencies]"),
                Style::Bold,
            ));
        }
        if resolution.test_roots.contains(ga) {
            node.children.push(TreeNode::styled(
                format!("{project} [dev-dependencies]"),
                Style::Bold,
            ));
        }
        if path.len() > 32 {
            return node;
        }
        for parent in dependents.get(ga).into_iter().flatten() {
            if path.contains(parent) {
                node.children
                    .push(TreeNode::styled(format!("{parent} (cycle)"), Style::Dim));
                continue;
            }
            path.push(parent.clone());
            node.children
                .push(self.why_node(resolution, dependents, parent, path));
            path.pop();
        }
        node
    }
}

#[derive(Clone)]
struct Built {
    resolution: Resolution,
    classes: usize,
}

/// A language's compiler, as the graph jrs resolved for it.
#[derive(Clone)]
struct Tool {
    language: Language,
    resolution: Resolution,
}

/// A unit's source roots, for a message.
fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A task's row in the `--timings` report: `task gen (pre-compile)`, with
/// `fresh` when its up-to-date check let it be skipped.
fn task_timing_label(name: &str, hook: Option<Hook>, fresh: bool) -> String {
    match (hook, fresh) {
        (Some(hook), false) => format!("task {name} ({hook})"),
        (Some(hook), true) => format!("task {name} ({hook}, fresh)"),
        (None, false) => format!("task {name}"),
        (None, true) => format!("task {name} (fresh)"),
    }
}

/// `14      3 downloaded`, for the summary.
fn deps_row(resolution: &Resolution) -> String {
    format!(
        "{:<7} {} downloaded",
        resolution.packages.len() + resolution.local.len(),
        resolution.downloaded
    )
}

/// A package as `jrs tree` draws it. Mediated versions are coloured so the
/// nearest-wins decision is visible at a glance (SPEC §5.3.6).
fn package_label(package: &resolve::ResolvedPackage) -> (String, Style) {
    let mut label = package.coord.to_string();
    label.push_str(classpath_suffix(package.classpath));
    let style = if package.mediated {
        Style::Yellow
    } else {
        Style::None
    };
    (label, style)
}

/// What `jrs tree` adds after a package that is not on every classpath.
fn classpath_suffix(classpath: Classpath) -> &'static str {
    match classpath {
        Classpath::Test => " (test)",
        Classpath::Provided => " (compile-only)",
        Classpath::Runtime => " (runtime-only)",
        Classpath::Compile => "",
    }
}

/// A local jar as `jrs tree` draws it: its name, then the path it is at.
fn local_label(local: &resolve::LocalJar) -> String {
    format!(
        "{} = {}{}",
        local.name,
        local.path,
        classpath_suffix(local.classpath)
    )
}

// ---- watch -----------------------------------------------------------------

/// How often `--watch` looks for changes. The walk is the same sorted one a
/// build does, so a few times a second costs nothing noticeable.
const WATCH_INTERVAL: Duration = Duration::from_millis(300);

/// Run a command, then again every time a watched file changes, until
/// interrupted. A failure is reported and waited out rather than ending the
/// loop: the next save is usually the fix.
fn watch(cli: &Cli, ui: &Ui, run: impl Fn(&Session) -> Result<i32>) -> Result<i32> {
    loop {
        let watched = match Session::open(cli, ui) {
            Ok(session) => {
                if let Err(e) = run(&session) {
                    report(ui, &e);
                }
                session.watched_paths()
            }
            Err(e) => {
                // A manifest that does not parse is worth waiting on too.
                report(ui, &e);
                vec![manifest_path(cli)?]
            }
        };
        ui.phase("Watching", "for changes (ctrl-c to stop)");
        wait_for_change(ui, &watched);
    }
}

fn wait_for_change(ui: &Ui, paths: &[PathBuf]) {
    let before = Snapshot::take(paths);
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        let mut now = Snapshot::take(paths);
        if now == before {
            continue;
        }
        // Editors save in more than one step; let the tree settle first.
        loop {
            std::thread::sleep(WATCH_INTERVAL / 3);
            let again = Snapshot::take(paths);
            if again == now {
                break;
            }
            now = again;
        }
        if let Some(path) = before.first_difference(&now) {
            ui.phase("Changed", path.display());
        }
        return;
    }
}

// ---- add / remove ----------------------------------------------------------

/// `only` is `(--compile-only, --runtime-only)`, which clap keeps from both
/// being set.
fn add_command(
    cli: &Cli,
    ui: &Ui,
    coordinates: &[String],
    dev: bool,
    only: (bool, bool),
) -> Result<i32> {
    let (compile_only, runtime_only) = only;
    let session = Session::open(cli, ui)?;
    let section = if dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };
    let fetcher = session.fetcher()?;

    let mut additions = Vec::new();
    for coordinate in coordinates {
        let parts: Vec<&str> = coordinate.split(':').collect();
        let bad = || {
            JrsError::usage(format!(
                "`{coordinate}` is not `group:artifact`, `group:artifact:version` or \
                 `group:artifact:version:classifier`"
            ))
        };
        if parts.iter().any(|p| p.trim().is_empty()) {
            return Err(bad());
        }
        let (group, artifact, version, classifier) = match parts.as_slice() {
            [g, a] => (*g, *a, None, None),
            [g, a, v] => (*g, *a, Some(v.to_string()), None),
            [g, a, v, c] => (*g, *a, Some(v.to_string()), Some(c.to_string())),
            _ => return Err(bad()),
        };
        let version = if let Some(v) = version {
            v
        } else {
            ui.phase(
                "Looking up",
                format!("the newest release of {group}:{artifact}"),
            );
            let known = fetcher.metadata(group, artifact)?;
            metadata::newest_release(&known).ok_or_else(|| {
                JrsError::resolve(format!(
                    "`{group}:{artifact}` has no stable release to add\n\n\
                     name a version: `jrs add {group}:{artifact}:<version>`"
                ))
            })?
        };
        if let Some(scala) = session.manifest.language(Language::Scala)
            && let Some(suffix) = lang::cross_build_suffix(artifact)
            && format!("_{suffix}") != lang::scala_suffix(&scala.version)
        {
            ui.warn(format!(
                "`{group}:{artifact}` is built for Scala {suffix}, but [scala] is Scala {}; \
                 jrs adds it as written, and does not rewrite the suffix",
                scala.version
            ));
        }
        let mut dep = Dependency::new(group, artifact, version);
        dep.classifier = classifier;
        dep.compile_only = compile_only;
        dep.runtime_only = runtime_only;
        additions.push(dep);
    }

    edit_manifest(cli, ui, &session.manifest, |text| {
        let mut text = text.to_string();
        for dep in &additions {
            let (key, value) = manifest::dependency_entry(dep);
            let edited = edit::upsert(&text, section, &key, &value)?;
            match &edited {
                edit::Edited::Added(_) => ui.phase("Adding", format!("{dep} to [{section}]")),
                edit::Edited::Replaced { previous, .. } => {
                    ui.phase("Updating", format!("{key} from {previous} to {value}"));
                }
            }
            text = edited.text().to_string();
        }
        Ok(text)
    })
}

fn remove_command(cli: &Cli, ui: &Ui, keys: &[String], dev: bool) -> Result<i32> {
    let session = Session::open(cli, ui)?;
    let sections: &[&str] = if dev {
        &["dev-dependencies"]
    } else {
        &["dependencies", "dev-dependencies"]
    };
    let local = |key: &str| {
        let m = &session.manifest;
        m.dependencies
            .iter()
            .chain(&m.dev_dependencies)
            .any(|d| d.is_local() && d.key() == key)
    };
    for key in keys {
        if !local(key) && (Ga::parse(key).is_none() || key.split(':').count() > 3) {
            return Err(JrsError::usage(format!(
                "`{key}` is not a `group:artifact` key, as jrs.toml names dependencies"
            )));
        }
    }
    let path = session.manifest.path.clone();
    edit_manifest(cli, ui, &session.manifest, |text| {
        let mut text = text.to_string();
        for key in keys {
            let mut removed = false;
            for section in sections {
                if let Some(edited) = edit::remove(&text, section, key)? {
                    ui.phase("Removing", format!("{key} from [{section}]"));
                    text = edited;
                    removed = true;
                    break;
                }
            }
            if !removed {
                return Err(JrsError::usage(format!(
                    "`{key}` is not declared in {}",
                    path.display()
                )));
            }
        }
        Ok(text)
    })
}

/// Rewrite `jrs.toml` through `change`, then resolve against the result.
///
/// The edited text must parse back as a manifest before it is written, and if
/// the new graph does not resolve — a coordinate that does not exist, say —
/// the original file is put back, so a failed `jrs add` leaves nothing behind.
fn edit_manifest(
    cli: &Cli,
    ui: &Ui,
    current: &Manifest,
    change: impl FnOnce(&str) -> Result<String>,
) -> Result<i32> {
    let path = &current.path;
    let original = std::fs::read_to_string(path).path(path)?;
    let edited = change(&original)?;
    Manifest::parse(&edited, path, &current.root).map_err(|e| {
        JrsError::manifest(format!(
            "the edit would leave {} unreadable, so it was not written:\n\n{e}",
            path.display()
        ))
    })?;
    std::fs::write(path, &edited).path(path)?;

    let resolved = Session::open(cli, ui).and_then(|s| {
        let resolution = s.dependencies(false)?;
        Ok((s.manifest.lock_path(), resolution.packages.len()))
    });
    match resolved {
        Ok((lock, packages)) => {
            ui.phase(
                "Updated",
                format!("{} ({packages} packages)", path.display()),
            );
            if packages > 0 {
                ui.verbose(format!("resolved into {}", lock.display()));
            }
            Ok(exit::SUCCESS)
        }
        Err(e) => {
            std::fs::write(path, &original).path(path)?;
            ui.phase(
                "Restored",
                format!("{} to what it was before", path.display()),
            );
            Err(e)
        }
    }
}

// ---- cache -----------------------------------------------------------------

fn cache_command(cli: &Cli, ui: &Ui, action: &CacheCommand) -> Result<i32> {
    let cache = Cache::discover()?;
    match action {
        CacheCommand::Path => {
            ui.println_out(cache.root().display());
            Ok(exit::SUCCESS)
        }
        CacheCommand::Prune {
            unused_for,
            dry_run,
        } => {
            // The project jrs is run from is a user of the cache whether or
            // not it was built since projects started being recorded.
            if let Ok(path) = manifest_path(cli) {
                let lock = path.with_file_name(manifest::LOCK_FILE);
                if lock.is_file() {
                    cache.register_project(&lock)?;
                }
            }
            let rule = match unused_for {
                Some(days) => Prune::UnusedFor(Duration::from_secs(days * 24 * 60 * 60)),
                None => Prune::Unreferenced(referenced(ui, &cache)?),
            };
            ui.phase(
                if *dry_run { "Checking" } else { "Pruning" },
                cache.root().display(),
            );
            let pruned = cache.prune(&rule, *dry_run)?;
            if *dry_run {
                ui.suspend();
                for dir in &pruned.removed {
                    ui.println_out(dir);
                }
            }
            ui.phase(
                if *dry_run { "Would remove" } else { "Removed" },
                format!(
                    "{} artifacts ({}); kept {}",
                    pruned.removed.len(),
                    ui::format_bytes(pruned.bytes),
                    pruned.kept
                ),
            );
            Ok(exit::SUCCESS)
        }
    }
}

/// Every version directory a known project's `jrs.lock` names.
fn referenced(ui: &Ui, cache: &Cache) -> Result<HashSet<String>> {
    let projects = cache.forget_missing_projects()?;
    if projects.is_empty() {
        return Err(JrsError::usage(format!(
            "jrs has no record of a project using {}, so it cannot tell what is \
             still needed\n\nbuild your projects once so they are recorded, or prune \
             by age instead: `jrs cache prune --unused-for 30`",
            cache.root().display()
        )));
    }
    let mut keep = HashSet::new();
    for lock in &projects {
        // A lockfile that cannot be read could name anything: rather than guess,
        // keep everything.
        let parsed = Lockfile::load(lock).map_err(|e| {
            JrsError::usage(format!(
                "{} could not be read, so nothing was pruned: {e}",
                lock.display()
            ))
        })?;
        // The compilers it pins are its too.
        if let Some(lock) = parsed {
            for package in lock.all_packages() {
                keep.insert(package.coord.version_dir());
            }
        }
    }
    ui.verbose(format!(
        "{} projects use this cache: {}",
        projects.len(),
        projects
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(keep)
}

// ---- completions -----------------------------------------------------------

fn completions_command(ui: &Ui, shell: &str) -> Result<i32> {
    let shell = completions::Shell::parse(shell).ok_or_else(|| {
        JrsError::usage(format!(
            "`{shell}` is not a shell jrs writes completions for"
        ))
    })?;
    let script = completions::generate(shell, &Cli::command());
    for line in script.lines() {
        ui.println_out(line);
    }
    Ok(exit::SUCCESS)
}

// ---- init ------------------------------------------------------------------

/// The `JUnit` a new project starts with: the newest 5.x, which still compiles
/// for every `java.source` a project might lower itself to.
const STARTER_JUNIT: &str = "5.13.4";

const STARTER_MAIN: &str = r#"package com.example;

public class Main {
    public static void main(String[] args) {
        System.out.println(greeting());
    }

    static String greeting() {
        return "Hello from jrs";
    }
}
"#;

const STARTER_MAIN_TEST: &str = r#"package com.example;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class MainTest {
    @Test
    void greets() {
        assertEquals("Hello from jrs", Main.greeting());
    }
}
"#;

const STARTER_LIBRARY: &str = r#"package com.example;

public final class Library {
    private Library() {}

    public static String greeting(String name) {
        return "Hello, " + name;
    }
}
"#;

const STARTER_LIBRARY_TEST: &str = r#"package com.example;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class LibraryTest {
    @Test
    void greetsByName() {
        assertEquals("Hello, jrs", Library.greeting("jrs"));
    }
}
"#;

/// `MUnit`, the Scala starter's test framework: a `JUnit` 4 runner, so it runs
/// on the launcher's Vintage engine.
const STARTER_MUNIT: &str = "1.3.6";

/// Spock, the Groovy starter's test framework, in its build for the Groovy
/// line the starter pins.
const STARTER_SPOCK: &str = "2.4-groovy-5.0";

const STARTER_MAIN_KOTLIN: &str = r#"package com.example

fun main() {
    println(greeting())
}

fun greeting(): String = "Hello from jrs"
"#;

const STARTER_MAIN_TEST_KOTLIN: &str = r#"package com.example

import kotlin.test.Test
import kotlin.test.assertEquals

class MainTest {
    @Test
    fun greets() {
        assertEquals("Hello from jrs", greeting())
    }
}
"#;

const STARTER_LIBRARY_KOTLIN: &str = r#"package com.example

object Library {
    fun greeting(name: String): String = "Hello, $name"
}
"#;

const STARTER_LIBRARY_TEST_KOTLIN: &str = r#"package com.example

import kotlin.test.Test
import kotlin.test.assertEquals

class LibraryTest {
    @Test
    fun greetsByName() {
        assertEquals("Hello, jrs", Library.greeting("jrs"))
    }
}
"#;

const STARTER_MAIN_SCALA: &str = r#"package com.example

object Main:
  def main(args: Array[String]): Unit =
    println(greeting)

  def greeting: String = "Hello from jrs"
"#;

const STARTER_MAIN_SUITE_SCALA: &str = r#"package com.example

class MainSuite extends munit.FunSuite:
  test("greets") {
    assertEquals(Main.greeting, "Hello from jrs")
  }
"#;

const STARTER_LIBRARY_SCALA: &str = r#"package com.example

object Library:
  def greeting(name: String): String = s"Hello, $name"
"#;

const STARTER_LIBRARY_SUITE_SCALA: &str = r#"package com.example

class LibrarySuite extends munit.FunSuite:
  test("greets by name") {
    assertEquals(Library.greeting("jrs"), "Hello, jrs")
  }
"#;

const STARTER_MAIN_SPEC_GROOVY: &str = r#"package com.example

import spock.lang.Specification

class MainSpec extends Specification {
    def "greets"() {
        expect:
        Main.greeting() == "Hello from jrs"
    }
}
"#;

const STARTER_LIBRARY_SPEC_GROOVY: &str = r#"package com.example

import spock.lang.Specification

class LibrarySpec extends Specification {
    def "greets by name"() {
        expect:
        Library.greeting(name) == greeting

        where:
        name  | greeting
        "jrs" | "Hello, jrs"
        "you" | "Hello, you"
    }
}
"#;

/// What `jrs init` writes for a language: the main class, the test
/// dependencies, and two files — the starter and its test — relative to the
/// project root.
struct Starter {
    main_class: &'static str,
    dev_dependencies: Vec<Dependency>,
    files: [(&'static str, &'static str); 2],
}

fn starter(language: Language, lib: bool) -> Starter {
    let junit = || Dependency::new("org.junit.jupiter", "junit-jupiter", STARTER_JUNIT);
    let version = language.starter_version().unwrap_or_default();
    match (language, lib) {
        (Language::Java, false) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![junit()],
            files: [
                ("src/main/java/com/example/Main.java", STARTER_MAIN),
                ("src/test/java/com/example/MainTest.java", STARTER_MAIN_TEST),
            ],
        },
        (Language::Java, true) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![junit()],
            files: [
                ("src/main/java/com/example/Library.java", STARTER_LIBRARY),
                (
                    "src/test/java/com/example/LibraryTest.java",
                    STARTER_LIBRARY_TEST,
                ),
            ],
        },
        (Language::Kotlin, _) => Starter {
            // A `main` at file level compiles to a class named after the file.
            main_class: "com.example.MainKt",
            dev_dependencies: vec![
                junit(),
                Dependency::new("org.jetbrains.kotlin", "kotlin-test-junit5", version),
            ],
            files: if lib {
                [
                    (
                        "src/main/kotlin/com/example/Library.kt",
                        STARTER_LIBRARY_KOTLIN,
                    ),
                    (
                        "src/test/kotlin/com/example/LibraryTest.kt",
                        STARTER_LIBRARY_TEST_KOTLIN,
                    ),
                ]
            } else {
                [
                    ("src/main/kotlin/com/example/Main.kt", STARTER_MAIN_KOTLIN),
                    (
                        "src/test/kotlin/com/example/MainTest.kt",
                        STARTER_MAIN_TEST_KOTLIN,
                    ),
                ]
            },
        },
        (Language::Scala, _) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![Dependency::new("org.scalameta", "munit_3", STARTER_MUNIT)],
            files: if lib {
                [
                    (
                        "src/main/scala/com/example/Library.scala",
                        STARTER_LIBRARY_SCALA,
                    ),
                    (
                        "src/test/scala/com/example/LibrarySuite.scala",
                        STARTER_LIBRARY_SUITE_SCALA,
                    ),
                ]
            } else {
                [
                    ("src/main/scala/com/example/Main.scala", STARTER_MAIN_SCALA),
                    (
                        "src/test/scala/com/example/MainSuite.scala",
                        STARTER_MAIN_SUITE_SCALA,
                    ),
                ]
            },
        },
        // Groovy usually arrives for its tests: Java main code, Spock specs,
        // and Groovy declared as a test dependency so it stays out of the jar.
        (Language::Groovy, _) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![
                Dependency::new("org.apache.groovy", "groovy", version),
                Dependency::new("org.spockframework", "spock-core", STARTER_SPOCK),
            ],
            files: if lib {
                [
                    ("src/main/java/com/example/Library.java", STARTER_LIBRARY),
                    (
                        "src/test/groovy/com/example/LibrarySpec.groovy",
                        STARTER_LIBRARY_SPEC_GROOVY,
                    ),
                ]
            } else {
                [
                    ("src/main/java/com/example/Main.java", STARTER_MAIN),
                    (
                        "src/test/groovy/com/example/MainSpec.groovy",
                        STARTER_MAIN_SPEC_GROOVY,
                    ),
                ]
            },
        },
    }
}

fn init(
    ui: &Ui,
    name: Option<&str>,
    lib: bool,
    language: Language,
    path: Option<&Path>,
) -> Result<i32> {
    ui.banner();

    let root = path.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&root).path(&root)?;

    let name = match name {
        Some(n) => n.to_string(),
        None => root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_string()),
    };

    let manifest_path = root.join(MANIFEST_FILE);
    if manifest_path.exists() {
        return Err(JrsError::usage(format!(
            "{} already exists",
            manifest_path.display()
        )));
    }

    let starter = starter(language, lib);
    let mut manifest = manifest::blank(&name, "0.1.0", &root);
    if !lib {
        manifest.main_class = Some(starter.main_class.to_string());
    }
    if let Ok(toolchain) = Toolchain::discover() {
        manifest.java.source = Some(toolchain.version);
    }
    if let Some(version) = language.starter_version() {
        let key = language.key();
        manifest.languages.push(LanguageConfig {
            language,
            version: version.to_string(),
            source_dir: PathBuf::from(format!("src/main/{key}")),
            test_dir: PathBuf::from(format!("src/test/{key}")),
            compiler_args: Vec::new(),
            compiler_jvm_args: Vec::new(),
        });
    }
    manifest.dev_dependencies = starter.dev_dependencies;
    std::fs::write(&manifest_path, manifest.render(None)).path(&manifest_path)?;
    ui.phase("Created", manifest_path.display());

    for (relative, contents) in starter.files {
        let path = root.join(relative);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        std::fs::write(&path, contents).path(&path)?;
        ui.phase("Created", path.display());
    }

    ui.phase("Next", if lib { "jrs test" } else { "jrs run" });
    Ok(exit::SUCCESS)
}

// ---- migrate ---------------------------------------------------------------

fn migrate_command(
    ui: &Ui,
    from: Option<&str>,
    dry_run: bool,
    force: bool,
    path: Option<&Path>,
) -> Result<i32> {
    ui.banner();

    let root = path.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let source = from.map(migrate::Source::parse).transpose()?;
    let migration = migrate::plan(&root, source)?;

    ui.phase(
        "Migrating",
        format!(
            "{} ({})",
            migration.source_file.display(),
            migration.source.as_str()
        ),
    );

    // The report and the manifest are real output: they belong on stdout so they
    // can be piped, diffed or redirected.
    ui.suspend();
    if let Some(preamble) = &migration.report.preamble {
        ui.println_out(preamble);
        ui.println_out("");
    }

    print_block(ui, "Migrated", &migration.report.migrated);
    print_block(ui, "Needs review", &migration.report.needs_review);
    print_block(ui, "Not migrated", &migration.report.not_migrated);

    if dry_run {
        ui.println_out("--- jrs.toml (dry run; nothing was written) ---");
        for line in migration.render_manifest().lines() {
            ui.println_out(line);
        }
        return Ok(exit::SUCCESS);
    }

    if let Some(written) = migrate::write(&migration, &root, force)? {
        ui.phase("Created", written.display());
    }
    ui.println_out("");
    ui.println_out("Next: `jrs build`, then `jrs tree` to compare the resolved graph");
    ui.println_out("against `mvn dependency:tree` / `gradle dependencies`.");
    Ok(exit::SUCCESS)
}

fn print_block(ui: &Ui, title: &str, entries: &[String]) {
    ui.println_out(format!("{title}:"));
    if entries.is_empty() {
        ui.println_out("  (nothing)");
    } else {
        for entry in entries {
            ui.println_out(format!("  - {entry}"));
        }
    }
    ui.println_out("");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_command_in_the_spec_exists() {
        for args in [
            vec!["jrs", "build"],
            vec!["jrs", "build", "--watch"],
            vec!["jrs", "test"],
            vec!["jrs", "test", "--coverage", "--watch"],
            vec!["jrs", "run"],
            vec!["jrs", "package"],
            vec!["jrs", "package", "--fat"],
            vec!["jrs", "package", "--portable"],
            vec!["jrs", "package", "--jlink", "--jpackage"],
            vec!["jrs", "package", "--jpackage", "app-image"],
            vec!["jrs", "package", "--sources", "--javadoc"],
            vec!["jrs", "package", "--dist"],
            vec!["jrs", "package", "--fat", "--dist", "--native-image"],
            vec!["jrs", "package", "--dist", "--jlink", "--sources"],
            vec!["jrs", "doc"],
            vec!["jrs", "clean"],
            vec!["jrs", "tree"],
            vec!["jrs", "tree", "--depth", "1"],
            vec!["jrs", "tree", "--why", "guava"],
            vec!["jrs", "classpath"],
            vec!["jrs", "classpath", "--test"],
            vec!["jrs", "classpath", "--runtime"],
            vec!["jrs", "update"],
            vec!["jrs", "verify"],
            vec!["jrs", "outdated"],
            vec!["jrs", "add", "com.google.guava:guava"],
            vec![
                "jrs",
                "add",
                "--dev",
                "org.junit.jupiter:junit-jupiter:5.13.4",
            ],
            vec!["jrs", "remove", "com.google.guava:guava"],
            vec!["jrs", "cache", "path"],
            vec!["jrs", "cache", "prune", "--unused-for", "30", "--dry-run"],
            vec!["jrs", "init"],
            vec!["jrs", "init", "--lib"],
            vec!["jrs", "init", "--lang", "kotlin"],
            vec!["jrs", "init", "--lib", "--lang", "groovy"],
            vec!["jrs", "tree", "--tool", "kotlin-compiler"],
            vec!["jrs", "tree", "--tool", "scala-compiler", "--depth", "2"],
            vec!["jrs", "migrate"],
            vec!["jrs", "completions", "zsh"],
            vec!["jrs", "task", "--list"],
            vec!["jrs", "task", "format"],
            vec!["jrs", "task", "format", "--watch"],
            vec!["jrs", "task", "format", "--", "--check", "src"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?} did not parse");
        }
    }

    #[test]
    fn a_task_takes_arguments_after_a_double_dash() {
        match parse(&["jrs", "task", "format", "--", "--verbose", "-q"]).command {
            Command::Task(t) => {
                assert_eq!(t.name.as_deref(), Some("format"));
                assert_eq!(t.args, vec!["--verbose", "-q"]);
            }
            other => panic!("expected task, got {other:?}"),
        }
    }

    #[test]
    fn no_task_can_take_a_command_name() {
        // A task named like a command would make `depends-on` ambiguous and
        // could shadow a future built-in, so the manifest refuses every name in
        // this list — which must keep up with the command tree.
        for command in Cli::command().get_subcommands() {
            let name = command.get_name();
            assert!(
                manifest::RESERVED_TASK_NAMES.contains(&name),
                "`{name}` is a command but not a reserved task name"
            );
        }
    }

    #[test]
    fn contradictory_flags_are_refused() {
        for args in [
            vec!["jrs", "package", "--fat", "--portable"],
            vec!["jrs", "classpath", "--test", "--runtime"],
            vec!["jrs", "add", "--dev", "--compile-only", "g:a"],
            vec!["jrs", "add", "--dev", "--runtime-only", "g:a"],
            vec!["jrs", "add", "--compile-only", "--runtime-only", "g:a"],
            vec!["jrs", "add"],
            vec!["jrs", "completions", "powershell"],
            vec!["jrs", "task"],
            vec!["jrs", "task", "format", "--list"],
            vec!["jrs", "task", "--list", "--watch"],
            vec!["jrs", "init", "--lang", "clojure"],
            vec!["jrs", "tree", "--why", "guava", "--tool", "kotlin-compiler"],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?} parsed");
        }
    }

    #[test]
    fn init_scaffolds_each_language_as_a_project_jrs_can_build() {
        let ui = Ui::new(UiOptions {
            quiet: true,
            progress: When::Never,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            ..UiOptions::default()
        });
        for (lang, lib) in [
            (LangArg::Java, false),
            (LangArg::Kotlin, false),
            (LangArg::Kotlin, true),
            (LangArg::Scala, false),
            (LangArg::Scala, true),
            (LangArg::Groovy, false),
            (LangArg::Groovy, true),
        ] {
            let language = Language::from(lang);
            let root = std::env::temp_dir().join(format!(
                "jrs-init-{}-{lib}-{}",
                language.key(),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            init(&ui, Some("app"), lib, language, Some(&root)).unwrap();

            let manifest = Manifest::load(root.join(MANIFEST_FILE)).unwrap();
            assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
            let project = Project::new(&manifest);
            let main = project.main_sources().unwrap();
            let test = project.test_sources().unwrap();
            assert_eq!(main.len(), 1, "{language}");
            assert_eq!(test.len(), 1, "{language}");
            match language {
                Language::Java => {
                    assert!(manifest.languages.is_empty());
                    assert_eq!(main.foreign(), None);
                }
                Language::Groovy => {
                    // Java main code, Groovy specs, Groovy off the runtime.
                    assert_eq!(main.foreign(), None);
                    assert_eq!(test.foreign(), Some(Language::Groovy));
                    assert!(manifest.implied_dependencies().is_empty());
                }
                other => {
                    assert_eq!(main.foreign(), Some(other));
                    assert_eq!(test.foreign(), Some(other));
                    assert!(!manifest.implied_dependencies().is_empty());
                }
            }
            assert!(manifest.language(language).is_some() || language == Language::Java);
            if lib {
                assert_eq!(manifest.main_class, None);
            } else if language == Language::Kotlin {
                assert_eq!(manifest.main_class.as_deref(), Some("com.example.MainKt"));
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn jpackage_takes_an_optional_type() {
        let kind = |args: &[&str]| match parse(args).command {
            Command::Package(p) => p.jpackage,
            other => panic!("expected package, got {other:?}"),
        };
        assert_eq!(kind(&["jrs", "package"]), None);
        assert_eq!(kind(&["jrs", "package", "--jpackage"]), Some(None));
        assert_eq!(
            kind(&["jrs", "package", "--jpackage", "dmg"]),
            Some(Some("dmg".into()))
        );
    }

    #[test]
    fn the_packaging_extras_are_independent_flags() {
        let Command::Package(p) = parse(&[
            "jrs",
            "package",
            "--portable",
            "--sources",
            "--javadoc",
            "--dist",
            "--native-image",
        ])
        .command
        else {
            panic!("expected package");
        };
        assert!(p.portable && p.sources && p.javadoc && p.dist && p.native_image);
        let Command::Package(p) = parse(&["jrs", "package"]).command else {
            panic!("expected package");
        };
        assert!(!p.sources && !p.javadoc && !p.dist && !p.native_image);
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = parse(&["jrs", "build", "--verbose", "--offline", "--jobs", "2"]);
        assert!(cli.global.verbose);
        assert!(cli.global.offline);
        assert_eq!(cli.global.jobs(), 2);
    }

    #[test]
    fn verbose_and_quiet_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["jrs", "build", "-v", "-q"]).is_err());
    }

    #[test]
    fn jobs_defaults_to_at_least_four() {
        assert!(parse(&["jrs", "build"]).global.jobs() >= 4);
        assert_eq!(
            parse(&["jrs", "build", "--jobs", "0"]).global.jobs(),
            default_jobs(),
            "a zero job count falls back to the default rather than deadlocking"
        );
    }

    #[test]
    fn the_flag_beats_the_config_file_which_beats_the_default() {
        assert_eq!(effective_jobs(Some(2), Some(8)), 2);
        assert_eq!(effective_jobs(None, Some(8)), 8);
        assert_eq!(effective_jobs(Some(0), Some(8)), 8);
        assert_eq!(effective_jobs(None, None), default_jobs());
    }

    #[test]
    fn program_arguments_come_after_a_double_dash() {
        let cli = parse(&["jrs", "run", "--", "--verbose", "input.txt"]);
        match cli.command {
            Command::Run { args, .. } => assert_eq!(args, vec!["--verbose", "input.txt"]),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn debug_takes_an_optional_port_after_an_equals_sign() {
        let debug = |argv: &[&str]| match parse(argv).command {
            Command::Run { debug, .. } => debug,
            Command::Test(args) => args.debug,
            other => panic!("expected run or test, got {other:?}"),
        };
        assert_eq!(debug(&["jrs", "run"]), None);
        assert_eq!(
            debug(&["jrs", "run", "--debug"]),
            Some(runner::DebugAddress::default())
        );
        assert_eq!(debug(&["jrs", "test", "--debug=8000"]).unwrap().port, 8000);
        let any = debug(&["jrs", "test", "--debug=*:5006"]).unwrap();
        assert_eq!(any.host.as_deref(), Some("*"));
        // The program's arguments still come after `--`, untouched.
        match parse(&["jrs", "run", "--debug", "--", "--debug=1"]).command {
            Command::Run { args, debug, .. } => {
                assert_eq!(args, vec!["--debug=1"]);
                assert_eq!(debug.unwrap().port, runner::DEFAULT_DEBUG_PORT);
            }
            other => panic!("expected run, got {other:?}"),
        }
        // A port is validated before anything builds: a usage error.
        for bad in [
            &["jrs", "run", "--debug=0"][..],
            &["jrs", "test", "--debug=70000"],
            &["jrs", "test", "--debug=five"],
            &["jrs", "test", "--debug", "5005"],
        ] {
            assert!(Cli::try_parse_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_tri_state_flags_parse() {
        let cli = parse(&[
            "jrs",
            "build",
            "--progress",
            "never",
            "--color",
            "always",
            "--charset",
            "ascii",
        ]);
        let opts = cli.global.ui_options();
        assert_eq!(opts.progress, When::Never);
        assert_eq!(opts.color, When::Always);
        assert_eq!(opts.charset, CharsetChoice::Ascii);
    }

    #[test]
    fn an_unknown_flag_value_is_rejected() {
        assert!(Cli::try_parse_from(["jrs", "build", "--progress", "sometimes"]).is_err());
    }

    #[test]
    fn quiet_implies_the_quiet_ui_mode() {
        let opts = parse(&["jrs", "build", "-q"]).global.ui_options();
        assert!(opts.quiet);
        assert_eq!(Ui::new(opts).mode(), ui::Mode::Quiet);
    }

    #[test]
    fn test_takes_a_filter_tags_and_methods() {
        let cli = parse(&[
            "jrs",
            "test",
            "--filter",
            ".*ServiceTest",
            "--include-tag",
            "fast",
            "--exclude-tag",
            "slow",
            "--method",
            "com.example.FooTest#bar",
            "--method",
            "com.example.FooTest#baz",
        ]);
        match cli.command {
            Command::Test(args) => {
                assert_eq!(args.filter.as_deref(), Some(".*ServiceTest"));
                assert_eq!(args.include_tag, vec!["fast"]);
                assert_eq!(args.exclude_tag, vec!["slow"]);
                assert_eq!(args.method.len(), 2);
            }
            other => panic!("expected test, got {other:?}"),
        }
    }

    #[test]
    fn test_takes_rerun_fail_fast_and_retries() {
        let cli = parse(&[
            "jrs",
            "test",
            "--rerun-failed",
            "--fail-fast",
            "--retries",
            "3",
        ]);
        match cli.command {
            Command::Test(args) => {
                assert!(args.rerun_failed);
                assert!(args.fail_fast);
                assert_eq!(args.retries, Some(3));
            }
            other => panic!("expected test, got {other:?}"),
        }
        // A rerun selects exactly what failed: nothing may narrow or widen
        // it, and a watch would rerun the same list forever.
        for other in [
            ["--method", "a.BTest#c"],
            ["--filter", ".*"],
            ["--include-tag", "fast"],
            ["--exclude-tag", "slow"],
        ] {
            let mut argv = vec!["jrs", "test", "--rerun-failed"];
            argv.extend(other);
            assert!(Cli::try_parse_from(argv).is_err(), "{other:?}");
        }
        assert!(Cli::try_parse_from(["jrs", "test", "--rerun-failed", "--watch"]).is_err());
        assert!(Cli::try_parse_from(["jrs", "test", "--retries", "-1"]).is_err());
        assert!(Cli::try_parse_from(["jrs", "test", "--fail-fast", "--retries", "2"]).is_ok());
    }

    #[test]
    fn migrate_takes_the_flags_the_spec_lists() {
        let cli = parse(&[
            "jrs",
            "migrate",
            "--from",
            "gradle",
            "--dry-run",
            "--force",
            "--path",
            "/p",
        ]);
        match cli.command {
            Command::Migrate {
                from,
                dry_run,
                force,
                path,
            } => {
                assert_eq!(from.as_deref(), Some("gradle"));
                assert!(dry_run);
                assert!(force);
                assert_eq!(path, Some(PathBuf::from("/p")));
            }
            other => panic!("expected migrate, got {other:?}"),
        }
    }

    #[test]
    fn timings_is_a_flag_of_the_commands_that_build() {
        for args in [
            vec!["jrs", "build", "--timings"],
            vec!["jrs", "build", "--watch", "--timings"],
            vec!["jrs", "test", "--timings", "--coverage"],
            vec!["jrs", "run", "--timings", "--", "--timings"],
            vec!["jrs", "package", "--fat", "--timings"],
        ] {
            assert!(parse(&args).command.timings(), "{args:?}");
        }
        assert!(!parse(&["jrs", "build"]).command.timings());
        match parse(&["jrs", "run", "--timings", "--", "--timings"]).command {
            Command::Run { args, timings, .. } => {
                assert!(timings);
                assert_eq!(args, ["--timings"], "after `--` it is the program's");
            }
            other => panic!("expected run, got {other:?}"),
        }
        for args in [
            vec!["jrs", "tree", "--timings"],
            vec!["jrs", "classpath", "--timings"],
            vec!["jrs", "fetch", "--timings"],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?} parsed");
        }
    }

    #[test]
    fn metadata_and_fetch_take_their_flags() {
        match parse(&["jrs", "metadata", "--no-deps"]).command {
            Command::Metadata { no_deps } => assert!(no_deps),
            other => panic!("expected metadata, got {other:?}"),
        }
        match parse(&["jrs", "metadata"]).command {
            Command::Metadata { no_deps } => assert!(!no_deps),
            other => panic!("expected metadata, got {other:?}"),
        }
        match parse(&["jrs", "fetch", "--sources", "--offline"]).command {
            Command::Fetch { sources } => assert!(sources),
            other => panic!("expected fetch, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["jrs", "fetch", "guava"]).is_err());
    }

    #[test]
    fn a_task_row_names_its_hook_and_freshness() {
        assert_eq!(
            task_timing_label("gen", Some(Hook::PreCompile), false),
            "task gen (pre-compile)"
        );
        assert_eq!(
            task_timing_label("gen", Some(Hook::PreCompile), true),
            "task gen (pre-compile, fresh)"
        );
        assert_eq!(task_timing_label("fmt", None, false), "task fmt");
        assert_eq!(task_timing_label("fmt", None, true), "task fmt (fresh)");
    }
}
