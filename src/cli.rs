//! Command definitions and dispatch.
//!
//! Everything above this line is a library; this is where a build becomes a
//! sequence of phases with output attached. The rule the rest of the codebase
//! depends on holds here too: phase lines are emitted here, and the live scopes
//! only add motion, so `--progress never` produces the same transcript.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::compile::Language;
use crate::compile::share;
use crate::error::{JrsError, Result, exit};
use crate::manifest::{MANIFEST_FILE, Manifest};
use crate::runner;
use crate::selfupdate;
use crate::test as junit;
use crate::ui::{CharsetChoice, Ui, UiOptions, When};
use crate::workspace::Workspace;

mod add;
mod cache;
mod deps;
mod doc;
mod init;
mod install;
mod jdk;
mod members;
mod migration;
mod packaging;
mod run;
mod session;
mod setup;
mod tasks;
mod testing;
mod watch;

use add::{add_command, remove_command};
use cache::cache_command;
use init::init;
use jdk::jdk_command;
use migration::migrate_command;
use session::Session;
use setup::{completions_command, self_command, wrapper_command};
use watch::{run_watch, watch};

#[derive(Debug, Parser)]
#[command(
    name = "jrs",
    version,
    about = "A Java build system, in Rust",
    long_about = "A Java build system, in Rust.\n\n\
                  jrs builds, tests, runs and packages a Java project from one \
                  jrs.toml file, or a workspace of several modules from one per module, \
                  resolving dependencies from Maven Central.",
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

    /// In a workspace, act on this member (and the members it depends on)
    /// instead of every member; repeatable.
    #[arg(short = 'p', long = "module", global = true, value_name = "NAME")]
    pub modules: Vec<String>,

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
pub(super) fn effective_jobs(flag: Option<usize>, config: Option<usize>) -> usize {
    flag.filter(|j| *j > 0)
        .or(config)
        .unwrap_or_else(default_jobs)
}

/// Cores, with a floor of 4: resolution is network-bound, and four in flight
/// beats one even on a single-core machine (SPEC §8.4).
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
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
        /// Neither restore from nor store into the build cache.
        #[arg(long)]
        no_build_cache: bool,
        /// Compile every unit, and fail if the build cache holds other
        /// classes for the same inputs.
        #[arg(long, conflicts_with_all = ["no_build_cache", "watch"])]
        verify_cache: bool,
    },

    /// Build, compile test sources, and run the test engine.
    Test(TestArgs),

    /// Build, then run the project's main class.
    Run {
        /// Arguments passed to the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
        /// Stop the program, build again and restart it whenever a source, a
        /// resource or jrs.toml changes.
        #[arg(long)]
        watch: bool,
        /// Report the wall time of each phase before the program starts.
        #[arg(long)]
        timings: bool,
        /// Neither restore from nor store into the build cache.
        #[arg(long)]
        no_build_cache: bool,
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

    /// Build, then put the jar and a POM into a local Maven repository, where
    /// a project in another repository can depend on it.
    Install(InstallArgs),

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
        /// Show the graph of a task's own [tasks.<name>.dependencies] instead.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["why", "tool"])]
        task: Option<String>,
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

    /// List each dependency's licence, as its POM (or its parent's) declares
    /// it, and flag the dependencies that declare none.
    Licenses {
        /// Only what the program runs with and a package ships: no test or
        /// compile-only dependencies.
        #[arg(long)]
        runtime: bool,
    },

    /// Check the resolved dependencies against the OSV vulnerability
    /// database, and fail if any has a known advisory.
    Audit {
        /// Only what the program runs with and a package ships: no test or
        /// compile-only dependencies.
        #[arg(long)]
        runtime: bool,
        /// Accept an advisory, by its ID or an alias (`GHSA-…`, `CVE-…`).
        #[arg(long, value_name = "ID")]
        ignore: Vec<String>,
    },

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
        /// Add a `check` task running PMD's quickstart rules over the Java
        /// sources, and a `post-compile` hook that runs it on every build.
        #[arg(long)]
        check: bool,
        /// Add a `format` task that formats the sources — google-java-format,
        /// or ktfmt for Kotlin — and a `format-check` task, run on every
        /// build, that fails when one is not formatted.
        #[arg(long)]
        format: bool,
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

    /// Write jrsw and jrsw.cmd, which run one pinned jrs release, downloading
    /// and checking it first, so that every checkout builds with the same jrs.
    Wrapper {
        /// The release to pin, such as 0.11.1. Defaults to this jrs's version.
        #[arg(long, value_name = "VERSION", value_parser = parse_release)]
        version: Option<selfupdate::Version>,
    },

    /// List the JDKs jrs can see, or download one.
    Jdk {
        #[command(subcommand)]
        action: JdkCommand,
    },

    /// Check for a newer jrs release, or update jrs to it.
    #[command(name = "self")]
    SelfCmd {
        #[command(subcommand)]
        action: SelfCommand,
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
    /// Neither restore a cached task's outputs from the build cache nor
    /// store them there.
    #[arg(long)]
    pub no_build_cache: bool,
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
    /// Split the test classes among N test JVMs run at once; overrides
    /// `[test] forks`.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub forks: Option<u32>,
    /// Run only the I-th of N slices of the test classes, as `2/4`, so that
    /// N machines can share one suite. The slice follows from the class
    /// names alone, so it is the same on every machine.
    #[arg(
        long,
        value_name = "I/N",
        value_parser = junit::Shard::parse,
        conflicts_with_all = ["method", "rerun_failed"]
    )]
    pub shard: Option<junit::Shard>,
    /// Run the suite `[test.suites.<NAME>]` declares instead of the tests
    /// under `project.test-dir`.
    #[arg(long, value_name = "NAME")]
    pub suite: Option<String>,
    /// Run every test class, not only those a change since the last run
    /// reaches, and never take a passing run from the build cache.
    #[arg(long)]
    pub all: bool,
    /// Neither restore from nor store into the build cache.
    #[arg(long)]
    pub no_build_cache: bool,
}

#[derive(Debug, Default, Clone, Args)]
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
    /// Obfuscate the packaged jar with `ProGuard`, which needs an `[obfuscate]`
    /// table in jrs.toml. Renames classes, methods and fields and strips debug
    /// information, leaving behaviour untouched.
    #[arg(long)]
    pub obfuscate: bool,
    /// Also write a `CycloneDX` SBOM of the runtime dependencies beside the
    /// jar, as target/<name>-<version>-cyclonedx.json.
    #[arg(long)]
    pub sbom: bool,
    /// Neither restore from nor store into the build cache.
    #[arg(long)]
    pub no_build_cache: bool,
}

#[derive(Debug, Default, Clone, Args)]
pub struct InstallArgs {
    /// The repository to install into, instead of ~/.m2/repository.
    #[arg(long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
    /// Also install the sources jar `jrs package --sources` writes.
    #[arg(long)]
    pub sources: bool,
    /// Also install the Javadoc jar `jrs package --javadoc` writes.
    #[arg(long)]
    pub javadoc: bool,
    /// Neither restore from nor store into the build cache.
    #[arg(long)]
    pub no_build_cache: bool,
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
        /// Also remove the JDKs `jrs jdk install` downloaded, which are
        /// otherwise kept.
        #[arg(long)]
        jdks: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum JdkCommand {
    /// Print every JDK jrs can see, where it is, and which one the project
    /// would build with.
    List,
    /// Download a JDK into the cache, where a project that pins its version
    /// finds it.
    Install {
        /// The Java feature version, such as 21.
        #[arg(value_name = "VERSION")]
        version: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum SelfCommand {
    /// Say whether this jrs is the latest release, and which release is.
    Check,
    /// Replace this jrs with the latest release, when it is newer.
    Update {
        /// Install this release instead, older ones included, such as
        /// `0.10.0`: what `project.jrs-version` or a team pins.
        #[arg(long, value_name = "VERSION", value_parser = parse_release)]
        to: Option<selfupdate::Version>,
    },
}

/// `--to`'s value: a release as its tag names it, `0.10.0` or `v0.10.0`.
fn parse_release(text: &str) -> std::result::Result<selfupdate::Version, String> {
    selfupdate::Version::parse(text)
        .filter(selfupdate::Version::is_release)
        .ok_or_else(|| format!("`{text}` is not a jrs release such as 0.10.0"))
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

    /// Whether the command was given `--no-build-cache`, which `build`,
    /// `test`, `run`, `package`, `install` and `task` take.
    #[must_use]
    pub fn no_build_cache(&self) -> bool {
        match self {
            Command::Build { no_build_cache, .. } | Command::Run { no_build_cache, .. } => {
                *no_build_cache
            }
            Command::Test(args) => args.no_build_cache,
            Command::Package(args) => args.no_build_cache,
            Command::Install(args) => args.no_build_cache,
            Command::Task(args) => args.no_build_cache,
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
    let code = match dispatch(&cli, &ui) {
        Ok(code) => code,
        Err(error) => {
            report(&ui, &error);
            error.exit_code()
        }
    };
    // The caches still being written are the command's, and so are the
    // workers still stopping: neither outlives it (SPEC §1.2).
    share::join_background();
    code
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
    let code = match dispatch(&cli, ui) {
        Ok(code) => code,
        Err(error) => {
            report(ui, &error);
            error.exit_code()
        }
    };
    share::join_background();
    code
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
            check,
            format,
            path,
        } => {
            return init(
                ui,
                name.as_deref(),
                *lib,
                (*lang).into(),
                *check,
                *format,
                path.as_deref(),
            );
        }
        Command::Migrate {
            from,
            dry_run,
            force,
            path,
        } => {
            return migrate_command(cli, ui, from.as_deref(), *dry_run, *force, path.as_deref());
        }
        Command::Completions { shell } => return completions_command(ui, shell),
        Command::Cache { action } => return cache_command(cli, ui, action),
        Command::SelfCmd { action } => return self_command(cli, ui, action),
        Command::Jdk { action } => return jdk_command(cli, ui, action),
        Command::Wrapper { version } => return wrapper_command(cli, ui, version.as_ref()),
        _ => {}
    }

    // A workspace's root, or a member of one: every member's manifest, one
    // lockfile, and the command run member by member (`specs/WORKSPACES.md`).
    if let Some((workspace, location)) = Workspace::find(&manifest_path(cli)?)? {
        return members::dispatch(cli, ui, workspace, location);
    }

    match &cli.command {
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
        Command::Run {
            watch: true,
            args,
            debug,
            ..
        } => return run_watch(cli, ui, args, debug.as_ref()),
        _ => {}
    }

    let session = Session::open(cli, ui)?;
    match &cli.command {
        Command::Build { .. } => session.build_command(),
        Command::Test(args) => session.test_command(args),
        Command::Run { args, debug, .. } => session.run_command(args, debug.as_ref()),
        Command::Package(args) => session.package_command(args),
        Command::Install(args) => session.install_command(args),
        Command::Doc => session.doc_command(),
        Command::Task(args) => session.task_command(args),
        Command::Clean => session.clean_command(),
        Command::Tree {
            depth,
            why,
            tool,
            task,
        } => session.tree_command(*depth, why.as_deref(), tool.as_deref(), task.as_deref()),
        Command::Classpath { test, runtime } => session.classpath_command(*test, *runtime),
        Command::Update => session.update_command(),
        Command::Verify => session.verify_command(),
        Command::Outdated => session.outdated_command(),
        Command::Licenses { runtime } => session.licenses_command(*runtime),
        Command::Audit { runtime, ignore } => session.audit_command(*runtime, ignore),
        Command::Metadata { no_deps } => session.metadata_command(*no_deps),
        Command::Fetch { sources } => session.fetch_command(*sources),
        Command::Init { .. }
        | Command::Migrate { .. }
        | Command::Completions { .. }
        | Command::Cache { .. }
        | Command::SelfCmd { .. }
        | Command::Jdk { .. }
        | Command::Wrapper { .. }
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{manifest, ui};
    use clap::CommandFactory;

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
            vec!["jrs", "init", "--check", "demo"],
            vec!["jrs", "tree", "--tool", "kotlin-compiler"],
            vec!["jrs", "tree", "--tool", "scala-compiler", "--depth", "2"],
            vec!["jrs", "migrate"],
            vec!["jrs", "completions", "zsh"],
            vec!["jrs", "task", "--list"],
            vec!["jrs", "task", "format"],
            vec!["jrs", "task", "format", "--watch"],
            vec!["jrs", "task", "format", "--", "--check", "src"],
            vec!["jrs", "build", "--no-build-cache"],
            vec!["jrs", "build", "--verify-cache"],
            vec!["jrs", "test", "--no-build-cache"],
            vec!["jrs", "run", "--no-build-cache"],
            vec!["jrs", "package", "--no-build-cache"],
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
            "--obfuscate",
        ])
        .command
        else {
            panic!("expected package");
        };
        assert!(p.portable && p.sources && p.javadoc && p.dist && p.native_image && p.obfuscate);
        let Command::Package(p) = parse(&["jrs", "package"]).command else {
            panic!("expected package");
        };
        assert!(!p.sources && !p.javadoc && !p.dist && !p.native_image && !p.obfuscate);
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
    fn run_takes_watch_before_the_program_arguments() {
        let cli = parse(&["jrs", "run", "--watch", "--", "--watch"]);
        match cli.command {
            Command::Run { args, watch, .. } => {
                assert!(watch);
                assert_eq!(args, vec!["--watch"], "after `--`, the program's");
            }
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
        match Cli::try_parse_from(["jrs", "test", "--forks", "3"])
            .unwrap()
            .command
        {
            Command::Test(args) => assert_eq!(args.forks, Some(3)),
            other => panic!("{other:?}"),
        }
        assert!(
            Cli::try_parse_from(["jrs", "test", "--forks", "0"]).is_err(),
            "at least one test JVM"
        );
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
}
