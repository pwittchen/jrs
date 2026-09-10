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
use crate::compile::{self, CompileUnit, DocUnit, ForeignCompiler, Language};
use crate::completions;
use crate::config::Config;
use crate::edit;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::image;
use crate::lockfile::Lockfile;
use crate::manifest::{
    self, Builtin, Dependency, Hook, LanguageConfig, MANIFEST_FILE, Manifest, Repository, TaskDef,
    TaskRef,
};
use crate::migrate;
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
    },

    /// Build, compile test sources, and run the test engine.
    Test(TestArgs),

    /// Build, then run the project's main class.
    Run {
        /// Arguments passed to the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
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
        /// The runtime classpath: without compile-only dependencies.
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
}

#[derive(Debug, Default, Args)]
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
        } => return add_command(cli, ui, coordinates, *dev, *compile_only),
        Command::Remove { keys, dev } => return remove_command(cli, ui, keys, *dev),
        #[allow(
            clippy::redundant_closure_for_method_calls,
            reason = "`Session::build_command` does not satisfy watch's higher-ranked bound"
        )]
        Command::Build { watch: true } => return watch(cli, ui, |s| s.build_command()),
        Command::Test(args) if args.watch => return watch(cli, ui, |s| s.test_command(args)),
        Command::Task(args) if args.watch => return watch(cli, ui, |s| s.task_command(args)),
        _ => {}
    }

    let session = Session::open(cli, ui)?;
    match &cli.command {
        Command::Build { .. } => session.build_command(),
        Command::Test(args) => session.test_command(args),
        Command::Run { args } => session.run_command(args),
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
        Command::Init { .. }
        | Command::Migrate { .. }
        | Command::Completions { .. }
        | Command::Cache { .. }
        | Command::Add { .. }
        | Command::Remove { .. } => unreachable!("handled above"),
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
        })
    }

    fn project(&self) -> Project<'_> {
        Project::new(&self.manifest)
    }

    fn elapsed(&self) -> String {
        ui::format_duration(self.started.elapsed())
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
        Ok(exit::SUCCESS)
    }

    fn run_command(&self, args: &[String]) -> Result<i32> {
        let main_class = self.manifest.require_main_class("run")?.to_string();
        let built = self.build()?;
        self.check_main_class(&main_class)?;
        self.hook(Hook::PreRun)?;
        let toolchain = self.toolchain()?;

        let mut classpath = vec![self.project().classes_dir()];
        classpath.extend(built.resolution.runtime_classpath());

        self.ui.phase("Running", &main_class);
        let code = runner::run_main(
            &toolchain,
            &self.manifest.run.jvm_args,
            &classpath,
            &main_class,
            args,
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

    fn package_command(&self, args: &PackageArgs) -> Result<i32> {
        let mut rows = self.package(args)?;
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        rows.push(("time", self.elapsed()));
        self.ui.summary(&rows);
        Ok(exit::SUCCESS)
    }

    /// Build, write the jar (and any image), then the `post-package` hook.
    /// Returns the summary rows.
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
        let built = self.build()?;
        let project = self.project();
        let output = project.jar_path();
        let runtime = built.resolution.runtime_classpath();
        // An image needs a jar that runs anywhere: the fat one when asked for,
        // the portable layout otherwise.
        let portable = args.portable || (images && !args.fat);
        let lib_dir = project.target_dir().join("lib");

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
                },
            )?
        };
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
        let app = image::App {
            name: &self.manifest.name,
            version: &self.manifest.version,
            main_class: self.manifest.main_class.as_deref(),
            jar,
            lib_dir,
            jvm_args: &self.manifest.run.jvm_args,
        };

        self.ui.phase("Analysing", "module dependencies with jdeps");
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
        let modules = modules?;
        self.ui.verbose(format!("modules: {}", modules.join(",")));

        let mut rows = Vec::new();
        if args.jlink {
            let output = project.target_dir().join("image");
            self.ui.phase(
                "Linking",
                format!("{} ({} modules)", output.display(), modules.len()),
            );
            let scope = self.ui.spinner("Linking", "a runtime image");
            let linked = image::jlink(&toolchain, &app, &modules, &output, self.ui);
            scope.finish();
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
            let bundled = bundled?;
            self.ui.phase("Bundled", bundled.display());
            rows.push(("package", bundled.display().to_string()));
        }
        Ok(rows)
    }

    fn test_command(&self, args: &TestArgs) -> Result<i32> {
        let Some(outcome) = self.test(args)? else {
            return Ok(exit::SUCCESS);
        };
        self.ui.phase(
            "Finished",
            format!("{} in {}", outcome.describe(), self.elapsed()),
        );
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
    fn test(&self, args: &TestArgs) -> Result<Option<junit::TestOutcome>> {
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
        let unit = self.compile_unit(
            "test",
            &sources,
            project.test_classes_dir(),
            classpath.clone(),
            vec![project.classes_dir()],
        )?;
        if compile::is_stale(&unit)? {
            let what = sources.describe("test sources");
            self.ui.phase("Compiling", &what);
            let scope = self.ui.spinner("Compiling", &what);
            let result = compile::compile(&toolchain, &unit, self.ui);
            scope.finish();
            result?;
        }
        project::sync_resources(
            &self.manifest.test_resource_path(),
            &project.test_classes_dir(),
            &project.work_dir().join("resources-test.list"),
        )?;
        self.sync_generated(&generated.resources, &project.test_classes_dir(), "test")?;

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

        let run = junit::TestRun {
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
        };

        self.ui.phase("Testing", sources.describe("test sources"));
        let scope = self.ui.tests();
        let outcome = junit::run(&toolchain, &run, self.ui);
        scope.finish();
        let outcome = outcome?;

        // Coverage is reported for a failing run too: which code the failing
        // tests reached is part of working out why.
        if args.coverage && exec.is_file() {
            self.coverage_report(&project, &toolchain, exec, &fetched[2])?;
        }

        if outcome.ok() {
            self.hook(Hook::PostTest)?;
        }
        Ok(Some(outcome))
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
        let scope = self.ui.downloads(missing.len());
        let jars: Result<Vec<PathBuf>> = internal
            .iter()
            .map(|(coord, _)| fetcher.jar(coord).map(|(path, _)| path))
            .collect();
        scope.finish();
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

    /// Document the main Java sources, generated ones included, so the
    /// `pre-compile` hook runs first. Returns the index page.
    ///
    /// Only Java is documented (`JVM_LANGUAGES.md` §9). In a project with
    /// another language the build runs first, since the Java sources may use
    /// its classes and `javadoc` has to find them on the classpath.
    fn doc(&self) -> Result<PathBuf> {
        let toolchain = self.toolchain()?;
        let javadoc = toolchain.tool("javadoc")?;
        let resolution = self.resolved()?;
        self.hook(Hook::PreCompile)?;
        let project = self.project();
        let generated = task::generated(&self.manifest, Hook::PreCompile)?;
        let all = project.sources(Unit::Main, &generated.sources)?;
        let mut classpath = resolution.classpath(Classpath::Compile);
        if let Some(language) = all.foreign() {
            self.ui.warn(format!(
                "jrs doc documents Java sources only; {} {language} source files were left out",
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
    /// version): the declared dependencies, then each language's compiler.
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
            .count();
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
                name: r.name.clone(),
                url: self.config.mirror_for(&r.name, &r.url).to_string(),
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
            let result = compile::compile(&toolchain, &unit, self.ui);
            scope.finish();
            result?
        } else {
            self.ui.phase(
                "Fresh",
                format!("{} v{}", self.manifest.name, self.manifest.version),
            );
            compile::Outcome::UpToDate
        };

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
        let prepared = task::prepare(def, &ctx, named.unwrap_or_default())?;
        if prepared.is_fresh() {
            self.ui.phase("Fresh", format!("{} (task)", def.name));
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

        resolve::locate_cached(&mut resolution, &fetcher);
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
        if matches.is_empty() {
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
            .collect())
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

/// `14      3 downloaded`, for the summary.
fn deps_row(resolution: &Resolution) -> String {
    format!(
        "{:<7} {} downloaded",
        resolution.packages.len(),
        resolution.downloaded
    )
}

/// A package as `jrs tree` draws it. Mediated versions are coloured so the
/// nearest-wins decision is visible at a glance (SPEC §5.3.6).
fn package_label(package: &resolve::ResolvedPackage) -> (String, Style) {
    let mut label = package.coord.to_string();
    match package.classpath {
        Classpath::Test => label.push_str(" (test)"),
        Classpath::Provided => label.push_str(" (compile-only)"),
        Classpath::Compile => {}
    }
    let style = if package.mediated {
        Style::Yellow
    } else {
        Style::None
    };
    (label, style)
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

fn add_command(
    cli: &Cli,
    ui: &Ui,
    coordinates: &[String],
    dev: bool,
    compile_only: bool,
) -> Result<i32> {
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
    for key in keys {
        if Ga::parse(key).is_none() || key.split(':').count() > 3 {
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
            Command::Run { args } => assert_eq!(args, vec!["--verbose", "input.txt"]),
            other => panic!("expected run, got {other:?}"),
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
}
